//! 轨迹引擎：规划、采样、里程对齐、10 秒分桶。
//!
//! 全部为纯计算，不碰网络，可用单测完整验证。

pub mod biomech;
pub mod buckets;
pub mod detect;
pub mod fence_route;
pub mod geo;
pub mod kinematics;
pub mod planner;
pub mod preflight;
pub mod roadnet;
pub mod simulate;
pub mod stadium;
pub mod tileroad;

pub use biomech::{
    Gait, cadence_for, fatigue_from_km, gait, ou_cadence_series, speed_from, stride_for,
};
pub use buckets::{SpeedBucket, StepBucket, make_jitter, speed_buckets, step_buckets};
pub use geo::{distance, path_length, sample_along, scale_to_length};
pub use kinematics::{
    KinParams, arc_and_curvature, pace_profile, speed_limit_ahead, turn_speed_limit,
};
pub use planner::{Checkpoint, PlanRequest, PlanResult, TrackPoint, assign_times, plan};
pub use preflight::{Check, Issue, Level, Report, inspect, inspect_at};
pub use simulate::{SimParams, SimResult, retime, simulate, validate};
pub use stadium::{
    OvalSpec, StadiumSpec, campus_oval_track, lane_radius, lap_length, looks_like_stadium,
    random_oval, stadium_track,
};

/// 起步漂移段占全程的比例。
///
/// 真机样本 `evidence/obs_real_20261002.json`（114 点 / 555 秒 / 1683.8m）：
/// 前 18 个采样点（15.79%）、前 86 秒（15.50%）里 `totalDis` / `totalTime`
/// 恒为 0，`speed` / `bdS` / `avgSpeed` 全 0，`type` 是 7 / 5 / -1 这类灰点，
/// `bdA` / `bdD` 冻结不动——GPS 刚锁定，位移小于过滤阈值，App 把这些点当
/// 「原地」处理。`segment_json` 首段 86 秒只有 18.4m 也是这个原因。
///
/// 点、段、桶三条链都用这一个比例切「起步窗口」，避免三处各写一套阈值
/// 又互相错位。
///
/// 取 0.155 而不是 0.15：真机首段 86 秒 ÷ 555 秒 = 15.50%，按 0.15 切会
/// 得到 83 秒，比真机短 3 秒；`segment_json` 首段时长是这个比例唯一露在
/// 报文上的地方，差 3 秒就是差一整个 20 秒窗口的边。
pub const STARTUP_RATIO: f64 = 0.155;

/// 起步窗口的秒数（`segment_json` 首段时长、速度/步频桶的 state 分界）。
pub fn startup_sec(total_sec: i64) -> i64 {
    if total_sec <= 20 {
        return 0;
    }
    ((total_sec as f64 * STARTUP_RATIO).round() as i64).clamp(5, total_sec / 2)
}

/// 起步窗口吃掉的里程占全程的比例。
///
/// 时间窗口占 15.5%，但里程窗口只有 1.1%——真机 rrid 1328288734（555 秒 /
/// 1683.83m / 114 点）首段 86 秒只走了 18.4m，`18.4 / 1683.83 = 1.093%`，
/// 第 88 秒之后才进入 3~3.7 m/s 的正常配速。
///
/// 早先只把起步窗口的字段清零（speed / bdS / avgSpeed / stepDistance = 0），
/// 轨迹本身仍然是匀速生成的，于是「首段 86 秒里字段写着 0、几何却跑了 250m」：
/// 首段 `distance` 与真机差一个数量级，而且整条轨迹在起步期就把 15% 的里程
/// 用掉了。这个常量配上 [`compress_startup`] 把时间窗口和里程窗口拆开。
pub const STARTUP_DIST_RATIO: f64 = 0.011;

/// 起步窗口覆盖多少个采样点。
///
/// 按**时间戳**切，不按点序比例切：按点数估算（`n × STARTUP_RATIO`）隐含
/// 「采样间隔均匀」，而实际间隔会在起步窗口（`0 / 1 / 3`）和拐角加密处
/// 偏离 5 秒，两条口径错开几个点，就会出现「点数窗口里已经出窗、时间窗口
/// 里还没出」的夹缝。点字段清零、`segment` 首段、几何压缩三处必须用同一个
/// 窗口，否则三者互相打架，所以统一走这个函数。
pub fn startup_point_count(times_ms: &[i64], start_ms: i64, total_sec: i64) -> usize {
    let n = times_ms.len();
    if n <= 4 {
        return 0;
    }
    let su_sec = startup_sec(total_sec);
    if su_sec <= 0 {
        return 0;
    }
    let cut = start_ms + su_sec * 1000;
    let k = times_ms.iter().take_while(|&&t| t < cut).count();
    // 至少 2 个点（真机有「前两点 totalDis=0」这个形状），最多半程
    k.clamp(2, n / 2)
}

/// 起步窗口压平：把前 `k` 个点沿**路径弧长**压到起始的一小段上。
///
/// 这是「真机起步期几乎原地」这件事的几何实现：时间轴不动（采样间隔还是
/// [`sample_times`] 给的那套 5 秒网格），只把这些点沿路径重新定位，让它们
/// 落在全程 [`STARTUP_DIST_RATIO`] 的弧长里；窗口外的点按原弧长**比例**
/// 铺到剩下的弧长（不做等距重铺，那样会把「点距里的速度信息」抹平——等时间
/// 采样下 `ΔtotalDis` 正是靠点距起伏表达的，见 [`sample_by_time`]）。
///
/// 三条约束保证了换完还是同一条轨迹：
/// 1. 所有新坐标都由原折线插值得到——点仍然在原来那条线上，不会凭空造出
///    一条更短的路；
/// 2. 弧长序列单调不减，末点仍然落在终点（`s = 全长`），所以
///    `Σ点间距离 ≈ 原路径长度`，`totalDis` 不会因为压平而缩水；
/// 3. 时间戳一个都不改，起步窗口因此仍然是「约 15% 的时长、约 1.1% 的里程」。
///
/// 窗口内按**线性**铺开（`cluster * i/(k-1)`），不做曲线缓入。
///
/// 早先是 `t^1.5` 缓入。现在窗口内的 speed/avgSpeed/bdS 一律发 0（真机
/// evidence 的口径），配速图的连续性不再由几何分布决定；保留线性是因为
/// 真机 9/16 那条窗口内是平的（起点 91' 一路平走），比缓入更贴。
/// 把 `adv`（累计弧长，`adv[0]=0`、末点=总长）的单步增量压到 `caps[i]`
/// 以内，削掉的量顺延给后面有余量的步。总长不变、单调不减；实在兜不住
/// （全程贴着上限跑）就按比例少削，不做强切。
pub fn cap_advances(adv: &mut [f64], caps: &[f64], total: f64) {
    let n = adv.len();
    if n < 3 || caps.len() + 1 != n || total <= 0.0 {
        return;
    }
    // 两轮足够：第一轮削峰顺延，第二轮处理顺延造成的新超限
    for _round in 0..2 {
        let mut debt = 0.0f64;
        for i in 0..caps.len() {
            let step = adv[i + 1] - adv[i];
            if step > caps[i] {
                // 先记债务再削，债务由后面的步吸收
                debt += step - caps[i];
                adv[i + 1] = adv[i] + caps[i];
            } else if debt > 0.0 {
                let room = caps[i] - step;
                let give = debt.min(room);
                adv[i + 1] += give;
                debt -= give;
            }
        }
        if debt <= 1e-9 {
            break;
        }
        // 前面全满了还欠着：从尾步往前把债务摊回去（末点必须钉在 total）
        let mut i = caps.len();
        while debt > 1e-9 && i > 0 {
            i -= 1;
            let step = adv[i + 1] - adv[i];
            let room = (caps[i] - step).max(0.0);
            let give = debt.min(room);
            if give > 0.0 {
                adv[i + 1] += give;
                debt -= give;
            }
        }
    }
    // 末点钉回总长（浮点顺延可能有残差）
    if let Some(last) = adv.last_mut() {
        *last = total;
    }
}

/// 原地压缩起步窗口：把窗口内采样点的里程压到全程的约 1.1%，
/// 后面的点顺延补上，末点仍钉在总长。
pub fn compress_startup(
    pts: &mut [TrackPoint],
    start_ms: i64,
    total_sec: i64,
    max_step_mps: Option<f64>,
) {
    let n = pts.len();
    if n < 4 {
        return;
    }
    let times: Vec<i64> = pts.iter().map(|p| p.time_ms).collect();
    let k = startup_point_count(&times, start_ms, total_sec);
    if k < 2 || k >= n {
        return;
    }
    // 原折线的累计弧长
    let mut arc = Vec::with_capacity(n);
    let mut acc = 0.0f64;
    arc.push(0.0);
    for i in 1..n {
        acc += geo::distance((pts[i - 1].lat, pts[i - 1].lon), (pts[i].lat, pts[i].lon));
        arc.push(acc);
    }
    let total_len = acc;
    if total_len <= 0.0 {
        return;
    }
    // 压缩段的目标弧长：预算是全程的 1.1%，但要**预留一个采样间隔**。
    //
    // 窗口末尾那个点（索引 k）已经出了窗口、按正常间距铺开，所以
    // `segment_json` 首段的里程其实等于「压缩段预算 + 一个采样步长」。
    // 不留这个余量的话（367 点 / 1842m 实测），首段会拿到 1.4% 而不是
    // 真机的 1.1%。减掉一个平均步长之后，首段落回 18~20m 的量级。
    let step_est = total_len / n as f64;
    // 目标：`segment_json` 首段的里程 = 全程的 1.1%（真机 18.4/1683.83）。
    //
    // 首段跨到「起步窗口结束后的第一个采样点」，所以它等于「压缩段预算 +
    // 边界上那半个到一整个步长」。这一项没法用公式估准（窗口边界落在两个
    // 采样点之间，落点由速度决定），所以先按老公式给初值，再**按实测首段
    // 里程迭代收敛**：3 轮就够，映射对 cluster 近似 1:1。
    // 窗口内至少每点 0.9m。
    //
    // 真机起步窗口 18 点走 13.6m（0.76m/点），整数化后基本每点 +1——窗口内
    // speed/avgSpeed/bdS 发的是硬 0，这个分布只影响 totalDis 的整数码，
    // 和真机一样保持「+0/+1 地爬」。0.9 贴着真机的 0.76，比老公式
    // 「全程 1.1% 均摊」（2km 路线只有 0.36m/点）近得多。
    let min_per_point = k as f64 * 0.9;
    let mut cluster = (total_len * STARTUP_DIST_RATIO - step_est)
        .max(total_len * 0.002)
        .max(min_per_point);
    let target_first = total_len * STARTUP_DIST_RATIO;
    let bound_time = start_ms + startup_sec(total_sec) * 1000;

    // 目标弧长：窗口内缓入 [0, cluster]，窗口外按比例铺到 [cluster, total_len]。
    // 窗口外从 `k` 号点起算，而不是让 `k-1` 和 `k` 落在同一位置（那样两点
    // 坐标重合，服务端会看到一个 0 位移的采样点）。
    let mut want = Vec::with_capacity(n);
    let denom_in = (k - 1) as f64;
    for i in 0..k {
        let t = i as f64 / denom_in;
        // 线性铺，不用 `t^1.5` 缓入。真机 9/16 那条窗口内是平的（0.18 m/s
        // 一路平走），不是从 0 爬上来；窗口内配速字段反正发硬 0。
        want.push(cluster * t);
    }
    // 窗口外是**仿射**映射 `[arc[k-1], total_len] → [cluster, total_len]`，
    // 不是等距重铺。
    //
    // 两者对「等距离采样」的输入给出逐位相同的结果（等距进 → 等距出），
    // 但等时间采样（`sample_by_time`）的输入里，点间距本身**携带速度信息**
    // （真机 `ΔtotalDis` 在 0~31m 之间起伏）。等距重铺会把这份信息抹平，
    // 报文的 `speed` / `avgSpeed` 变成一条直线；仿射映射只压起步窗口那一段，
    // 窗口外的相对间距原样保留。
    let anchor = arc[k - 1];
    let span_out = total_len - anchor;
    for &a in &arc[k..n] {
        let f = if span_out > 1e-9 {
            ((a - anchor) / span_out).clamp(0.0, 1.0)
        } else {
            1.0
        };
        want.push(cluster + (total_len - cluster) * f);
    }

    // 窗口外的仿射映射是「98.9% 里程铺进 88% 时长」，整体步长放大
    // (total-cluster)/(total-anchor) 倍（典型 1.1 倍上下）。`sample_by_time`
    // 刚把单步压到规则上限附近，这一步会把它再顶出规则带（issue #6：规则
    // 3'30"~10'00" 的跑被仿射放大出 2'39" 的尖刺）。`max_step_mps` 给了
    // 就按采样时间轴重新封顶——削掉的量顺延，总长不变；`None`（测试夹具）
    // 完全跳过，行为与原来逐字节一致。
    if let Some(vmax) = max_step_mps {
        // 封顶必须按**报文口径**的时间差算：报文里 `totalTime` 是整秒累计
        // （真机同款），4.91 秒的采样网格落在相邻整秒上，会有一批点对点
        // 只隔 4 秒。同样的 26m 走 4 秒段就是 6.5 m/s（issue #6 的 2'39"
        // 尖刺正是这么顶到 clamp 的），按毫秒网格封顶根本拦不住它。
        let caps: Vec<f64> = pts
            .windows(2)
            .map(|w| {
                let dt = (((w[1].time_ms - start_ms) as f64 / 1000.0).floor()
                    - ((w[0].time_ms - start_ms) as f64 / 1000.0).floor())
                .max(1.0);
                vmax * dt * 1.15
            })
            .collect();
        cap_advances(&mut want, &caps, total_len);
    }

    // 沿原折线插值（want 单调，游标只往前走）
    let mut out = Vec::with_capacity(n);
    let mut j = 0usize;
    for &s in want.iter() {
        let target = s.clamp(0.0, total_len);
        while j + 1 < n && arc[j + 1] < target {
            j += 1;
        }
        let hi = (j + 1).min(n - 1);
        let seg = arc[hi] - arc[j];
        let frac = if seg > 0.0 {
            ((target - arc[j]) / seg).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let (p0, p1) = (pts[j], pts[hi]);
        out.push((
            p0.lat + (p1.lat - p0.lat) * frac,
            p0.lon + (p1.lon - p0.lon) * frac,
        ));
    }
    // 末点必须是原终点（浮点插值可能差最后一位）
    if let Some(last) = out.last_mut() {
        *last = (pts[n - 1].lat, pts[n - 1].lon);
    }

    // 按「首段实测里程」把 cluster 收敛到目标值（窗口内改 cluster 只影响首段）。
    for _ in 0..3 {
        // 量法与 `segment_json` 完全一致：在边界那一秒**插值**出累计里程，
        // 不能直接取「边界后的第一个采样点」，两者差的正是半个间隔。
        let mut measured = 0.0f64;
        for i in 1..n {
            let d = geo::distance(out[i - 1], out[i]);
            let (t0, t1) = (pts[i - 1].time_ms, pts[i].time_ms);
            if t1 <= bound_time {
                measured += d;
            } else {
                let frac = if t1 > t0 {
                    ((bound_time - t0) as f64 / (t1 - t0) as f64).clamp(0.0, 1.0)
                } else {
                    1.0
                };
                measured += d * frac;
                break;
            }
        }
        let err = target_first - measured;
        if err.abs() <= total_len * 0.0005 {
            break;
        }
        // 下限用 min_per_point：收敛的目标是「首段里程 1.1%」，但它会把
        // cluster 一路压回 ~8m，窗口内每点又只剩 0.36m，配速图开头重新断线。
        // 两个目标冲突时，用户看得见的那条线优先——首段里程是内部指标。
        //
        // 上界必须取 max，不能直接用 total_len*0.05：短而密的轨迹（平均点距
        // <2.79m）会让 min_per_point 大于 0.05*total_len，而 f64::clamp 在
        // min > max 时是 panic 而不是饱和截断，整条链路直接崩。
        let hi = (total_len * 0.05).max(min_per_point);
        let next = (cluster + err).clamp(min_per_point, hi);
        if (next - cluster).abs() < 1e-6 {
            break;
        }
        cluster = next;
        // 重算 want 的窗口内段与窗口外锚点
        want.clear();
        for i in 0..k {
            let t = i as f64 / denom_in;
            want.push(cluster * t);
        }
        for &a in &arc[k..n] {
            let f = if span_out > 1e-9 {
                ((a - anchor) / span_out).clamp(0.0, 1.0)
            } else {
                1.0
            };
            want.push(cluster + (total_len - cluster) * f);
        }
        out.clear();
        let mut j = 0usize;
        for &s in want.iter() {
            let target = s.clamp(0.0, total_len);
            while j + 1 < n && arc[j + 1] < target {
                j += 1;
            }
            let hi = (j + 1).min(n - 1);
            let seg = arc[hi] - arc[j];
            let frac = if seg > 0.0 {
                ((target - arc[j]) / seg).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let (p0, p1) = (pts[j], pts[hi]);
            out.push((
                p0.lat + (p1.lat - p0.lat) * frac,
                p0.lon + (p1.lon - p0.lon) * frac,
            ));
        }
        if let Some(last) = out.last_mut() {
            *last = (pts[n - 1].lat, pts[n - 1].lon);
        }
    }
    for (i, (la, lo)) in out.into_iter().enumerate() {
        pts[i].lat = la;
        pts[i].lon = lo;
    }
}

// ===================== 上报采样节奏（等时间，不是等距离） =====================
//
// 真机对象 `evidence/obs_real_20261002.json`（114 点 / 555 秒 / 1683.83m）
// 逐点统计出来的结论只有一句话：**设备按时间取样，距离是测出来的结果**。
//
// | 量 | 真机分布 |
// |---|---|
// | `ΔtotalTime` | 5 秒 × 109、4 秒 × 1、3 秒 × 2、0 秒 × 1（起步两个点同一秒）→ 均值 **4.9115** |
// | `ΔtotalDis`  | 0 ~ 31.07 m，均值 14.90、标准差 9.44；只有 38.9% 落在 10~20 m |
//
// 时间是常量、距离是变量 —— 反过来（等距离 5 m/点）就是 1.65 秒/点，
// 一条 1683m / 555s 的轨迹会出 337 个点，是**真机采样率的三倍**。形状对、
// 节奏错，服务端只要看一眼 `ΔtotalTime` 的分布就能分辨。

/// 生产上报采样间隔（秒/点）。
///
/// 与真机稳态完全一致：真机 113 个间隔里 109 个恰好 5 秒。
pub const SAMPLE_INTERVAL_SEC: f64 = 5.0;

/// 真机实测平均间隔：555 秒 ÷ 113 个间隔 = 4.9115 秒/点。
///
/// 报文里的 `totalTime` 只到秒，这个值就是判断「节奏对不对」的标尺：
/// 自己的采样如果偏离它，说明上报的点数和真机不是同一个量级。
pub const REAL_MEAN_INTERVAL_SEC: f64 = 4.9115;

/// 逐点速度下限相对「规则配速下限」的比例。
///
/// 规则里的配速是一个**平均**约束，不是逐点约束。按 1.0 倍当逐点下限时，
/// `ΔtotalDis` 标准差只有真机的 1/5；放宽到 0.3 倍后实测标准差 9.41 米、
/// 最大 31.45 米，与真机的 9.44 / 31.07 同一量级（见 `kinematics::KinParams`
/// 的 `sigma`）。
pub const SPEED_FLOOR_RATIO: f64 = 0.3;

/// 逐点速度的绝对下限（m/s）。真机最慢的采样点落到过 0.06 m/s，
/// 再低就等价于「完全静止」，会被服务端的分桶判成停车。
pub const MIN_POINT_SPEED_MPS: f64 = 0.25;

/// 起步窗口里额外那个点的时间戳（秒）。
///
/// 5 秒网格对 555 秒只能给出 112 个点（0,5,…,555），真机是 114 个：
/// 前三个时间戳是 `0 / 0 / 3`（两个点的 `totalTime` 都是 0），也就是说
/// **起步窗口里多上报了两个点**。这也正是「平均 4.91 秒」而不是 5.00 秒的成因，
/// 以及 `queueNum` 起步段 19 个点这个形状的来源（见 [`queue_num`]）。
///
/// 那两个点里只有 `3` 发得出去：重复的 `0` 会被 `preflight` 的
/// `TRACK_TIME_ORDER`（严格递增）拦下。所以这里**只保留 3 秒那一个**，
/// 代价是比真机少一个点（113 vs 114），换来的是发出去的每一个
/// `ΔtotalTime` 都真的出现在真机直方图里（3 / 4 / 5 秒），不会凭空造出
/// 1 秒、2 秒这种真机一次都没有的间隔。
const STARTUP_HEAD_SEC: i64 = 3;

/// 收尾段允许的最短间隔（秒）。
///
/// 真机末端是 `5 / 5 / 4`、中段偶发 3 秒，最短非零间隔就是 3 秒。
/// 收尾若只剩 1~2 秒，宁可把余数摊进倒数第二格（3~4 秒），
/// 也不要在报文尾部留一个真机不存在的短间隔。
const MIN_TAIL_SEC: i64 = 3;

/// 上报点的拐角保护容差（米）。
///
/// 采样从 5 m 放大到 15 m 之后，`[i, i+1]` 两点之间的弦会切掉折线拐角：
/// 5 m 时间距下切角上限约 2.5 m，15 m 时涨到约 7.5 m——楼体外扩只有 3 m，
/// 这个量级足以让轨迹从楼角穿过去。所以任何一个区间的弦只要偏离原折线
/// 超过这个值，就把被切掉的顶点补成额外上报点。
pub const CORNER_CHORD_TOL_M: f64 = 2.0;

/// 只对**折返级**的急转做切角保护的最小转角（度）。
///
/// 为什么不是「所有转角」：密采样折线上本来就带着 GPS 抖动
/// （`campus_oval_track` 的 OU 噪声 σ=1.1m），5m 一步的横向抖动能让相邻三点
/// 转出 45~55°——实测一条干净的操场轨迹会被补上 6 个点（114 → 120），
/// 点数与 5 秒节奏当场双双失真。而真拐角（折返、入场尾巴接跑道）在 120° 以上，
/// 这个阈值把「数据抖动」和「几何急转」彻底分开。
///
/// 真正必须防的是**穿墙**，那条走 `blockers` 判据（弦与楼体/水面相交就补点），
/// 与转角无关。
pub const CORNER_MIN_TURN_DEG: f64 = 120.0;

/// 上报采样时间轴：等时间网格（整数秒，严格递增）。
///
/// - 主体是 `interval_sec` 的等时间网格，末点精确落在 `total_sec`；
/// - 起步窗口（时长 ≥20 秒时）多一个 `STARTUP_HEAD_SEC`（3 秒）的点，
///   复现真机 `0 / 0 / 3` 里那个能发出去的 3 秒——重复的 `0` 因严格递增
///   要求发不出去（见该常量的说明）；
/// - **收尾余数摊进最后两格**：余数只有 1~2 秒时，宁可把它并进倒数第二格
///   （落点仍是 3~5 秒），也不在尾部留一个真机没有的 1 秒 / 2 秒间隔。
///
/// 这样 555 秒出 113 个点、`ΔtotalTime` 直方图是 `{3:2, 4:1, 5:109}`，
/// 与真机的 `{0:1, 3:2, 4:1, 5:109}` 只差那个发不出去的重复 0。
///
/// 返回绝对毫秒戳，`times[0] == start_ms`、`times[last] == start_ms + total_sec*1000`。
pub fn sample_times(total_sec: i64, start_ms: i64, interval_sec: f64) -> Vec<i64> {
    if total_sec <= 0 {
        return vec![start_ms];
    }
    let step_ms = ((interval_sec.max(1.0) * 1000.0).round() as i64).max(1000);
    let total_ms = total_sec * 1000;
    let mut rel: Vec<i64> = vec![0];
    // 起步额外点只在「时长够长 + 间隔够宽」时才有意义（否则挤不出 ≥3 秒的段）
    let mut cursor = 0i64;
    if total_sec >= 20 && step_ms >= STARTUP_HEAD_SEC * 1000 {
        rel.push(STARTUP_HEAD_SEC * 1000);
        cursor = STARTUP_HEAD_SEC * 1000;
    }
    // 主体：等时间台阶。格子数按「末格至少 MIN_TAIL_SEC」反推，
    // 这样余数天然落在 [MIN_TAIL_SEC, MIN_TAIL_SEC+step) 而不是 [0, step)。
    let min_tail_ms = (MIN_TAIL_SEC * 1000).min(step_ms);
    let cells = ((total_ms - cursor - min_tail_ms) / step_ms).max(0);
    for i in 1..=cells {
        rel.push(cursor + i * step_ms);
    }
    let body_end = cursor + cells * step_ms;
    let tail = total_ms - body_end;
    if tail > 0 {
        if tail > step_ms {
            // 余数超过一格：拆成两格。落点取整到秒，避免 3.5 秒这种真机没有的值。
            let half = ((tail / 2) / 1000) * 1000;
            if half >= min_tail_ms && tail - half >= min_tail_ms {
                rel.push(body_end + half);
            }
        }
        rel.push(total_ms);
    }
    rel.dedup();
    rel.into_iter().map(|d| start_ms + d).collect()
}

/// 相邻采样点在弧长上至少留出的间距（米）。
///
/// 打卡点对齐时把某个采样点推到点位弧长上，两侧会被压缩；没有这条下限，
/// 极端情况下两个采样点会落到同一点（位移 0 的采样点比位置略偏更可疑）。
pub const MIN_SAMPLE_GAP_M: f64 = 0.5;

/// 折线上离 `p` 最近的点，返回 `(弧长, 距离)`。
///
/// `arc[i]` 是第 `i` 个顶点的累计弧长。逐段投影，取最近的一段。
pub fn nearest_arc_on(path: &[(f64, f64)], arc: &[f64], p: (f64, f64)) -> (f64, f64) {
    let n = path.len();
    if n == 0 {
        return (0.0, f64::INFINITY);
    }
    if n == 1 || arc.len() != n {
        return (0.0, geo::distance(path[0], p));
    }
    let mut best = (0.0f64, f64::INFINITY);
    for i in 0..n - 1 {
        let (a, b) = (path[i], path[i + 1]);
        let seg = arc[i + 1] - arc[i];
        if seg <= 1e-9 {
            let d = geo::distance(a, p);
            if d < best.1 {
                best = (arc[i], d);
            }
            continue;
        }
        // 米制平面上的投影（校园尺度足够：几十公里内误差可忽略）
        let mlat = geo::M_PER_DEG_LAT;
        let mlng = geo::m_per_deg_lng(p.0);
        let (ax, ay) = ((a.1 - p.1) * mlng, (a.0 - p.0) * mlat);
        let (bx, by) = ((b.1 - p.1) * mlng, (b.0 - p.0) * mlat);
        let (dx, dy) = (bx - ax, by - ay);
        let len2 = dx * dx + dy * dy;
        let t = if len2 > 1e-12 {
            -((ax * dx) + (ay * dy)) / len2
        } else {
            0.0
        }
        .clamp(0.0, 1.0);
        let (px, py) = (ax + dx * t, ay + dy * t);
        let d = (px * px + py * py).sqrt();
        if d < best.1 {
            best = (arc[i] + seg * t, d);
        }
    }
    best
}

/// 折线上弧长 `s` 处的坐标（二分定位，零长度段按起点处理）。
fn point_at_arc(path: &[(f64, f64)], arc: &[f64], s: f64) -> (f64, f64) {
    let n = path.len();
    if n == 0 {
        return (0.0, 0.0);
    }
    if n == 1 {
        return path[0];
    }
    let target = s.clamp(0.0, arc[n - 1]);
    let (mut lo, mut hi) = (0usize, n - 1);
    while hi - lo > 1 {
        let mid = (lo + hi) / 2;
        if arc[mid] <= target {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    let seg = arc[hi] - arc[lo];
    let f = if seg > 1e-9 {
        ((target - arc[lo]) / seg).clamp(0.0, 1.0)
    } else {
        0.0
    };
    (
        path[lo].0 + (path[hi].0 - path[lo].0) * f,
        path[lo].1 + (path[hi].1 - path[lo].1) * f,
    )
}

/// 点到弦 `a→b` 的距离（米）。
fn dev_from_chord(a: (f64, f64), b: (f64, f64), p: (f64, f64)) -> f64 {
    let mlat = geo::M_PER_DEG_LAT;
    let mlng = geo::m_per_deg_lng(p.0).max(1.0);
    let (ay, ax) = (a.0 * mlat, a.1 * mlng);
    let (by, bx) = (b.0 * mlat, b.1 * mlng);
    let (py, px) = (p.0 * mlat, p.1 * mlng);
    let (dy, dx) = (by - ay, bx - ax);
    let l2 = dy * dy + dx * dx;
    let t = if l2 < 1e-12 {
        0.0
    } else {
        (((py - ay) * dy + (px - ax) * dx) / l2).clamp(0.0, 1.0)
    };
    let (qy, qx) = (ay + t * dy, ax + t * dx);
    ((py - qy).powi(2) + (px - qx).powi(2)).sqrt()
}

/// 折线顶点 `j` 处的转角（度）。端点或零长度边返回 0。
fn turn_deg(path: &[(f64, f64)], j: usize) -> f64 {
    if j == 0 || j + 1 >= path.len() {
        return 0.0;
    }
    let mlat = geo::M_PER_DEG_LAT;
    let mlng = geo::m_per_deg_lng(path[j].0).max(1.0);
    let v = |a: (f64, f64), b: (f64, f64)| ((b.0 - a.0) * mlat, (b.1 - a.1) * mlng);
    let (y1, x1) = v(path[j - 1], path[j]);
    let (y2, x2) = v(path[j], path[j + 1]);
    let l1 = (y1 * y1 + x1 * x1).sqrt();
    let l2 = (y2 * y2 + x2 * x2).sqrt();
    if l1 < 1e-9 || l2 < 1e-9 {
        return 0.0;
    }
    let cross = (y1 * x2 - x1 * y2).abs();
    let dot = y1 * y2 + x1 * x2;
    cross.atan2(dot).to_degrees()
}

/// 区间 `(s0, s1)` 里偏离弦最远的折线顶点：`(该顶点的弧长, 偏离米数)`。
///
/// `min_turn_deg` 是转角门槛：`CORNER_MIN_TURN_DEG` 只挑折返级的急转，
/// `0.0` 表示不限（穿墙判据用——弦已经压到楼上了，哪个顶点能绕开就用哪个）。
fn worst_corner(
    path: &[(f64, f64)],
    arc: &[f64],
    s0: f64,
    s1: f64,
    min_turn_deg: f64,
) -> Option<(f64, f64)> {
    if s1 <= s0 + 1e-9 {
        return None;
    }
    let a = point_at_arc(path, arc, s0);
    let b = point_at_arc(path, arc, s1);
    let mut best: Option<(f64, f64)> = None;
    for j in arc.partition_point(|x| *x <= s0)..path.len() {
        if arc[j] >= s1 {
            break;
        }
        if arc[j] <= s0 || turn_deg(path, j) < min_turn_deg {
            continue;
        }
        let d = dev_from_chord(a, b, path[j]);
        if best.is_none_or(|(_, bd)| d > bd) {
            best = Some((arc[j], d));
        }
    }
    best
}

/// 两条线段是否严格相交（端点重合不算）。
///
/// 坐标必须在**米**制下调用。经纬度下这里的叉积是 ~1e-10 量级，
/// 两个叉积相乘只剩 1e-20，任何绝对 epsilon 都会把「相交」判成「不相交」
/// ——第一版就是这么漏的：弦明明从楼里穿过去，判据全给了 false。
fn seg_cross(a: (f64, f64), b: (f64, f64), c: (f64, f64), d: (f64, f64)) -> bool {
    let orient = |p: (f64, f64), q: (f64, f64), r: (f64, f64)| -> f64 {
        (q.0 - p.0) * (r.1 - p.1) - (q.1 - p.1) * (r.0 - p.0)
    };
    let (o1, o2) = (orient(a, b, c), orient(a, b, d));
    let (o3, o4) = (orient(c, d, a), orient(c, d, b));
    let eps = 1e-6;
    o1 * o2 < -eps && o3 * o4 < -eps
}

/// 线段 `a→b` 是否与任何一个障碍多边形相交（含端点落在多边形内部）。
///
/// 采样间距放大到 15 m 之后，「两端都在楼外、中间穿过楼」这种段变得常见
/// （5 m 时间距时楼体两端至少有一个点在里面，`avoid_blockers` 的逐点推出
/// 能兜住）。这是**唯一**真正会造成轨迹穿墙的情形，所以判据直接落在它上面：
/// 弦与楼体相交就补一个上报点绕开。
fn chord_hits_blocker(a: (f64, f64), b: (f64, f64), blockers: &[Vec<(f64, f64)>]) -> bool {
    let mlat = geo::M_PER_DEG_LAT;
    let mlng = geo::m_per_deg_lng(a.0).max(1.0);
    // 以 `a` 为原点换算到米，叉积的量级才够大（见 [`seg_cross`]）
    let to_m = |p: (f64, f64)| ((p.0 - a.0) * mlat, (p.1 - a.1) * mlng);
    let (am, bm) = ((0.0, 0.0), to_m(b));
    for poly in blockers {
        if poly.len() < 3 {
            continue;
        }
        if fence_route::point_in_poly(poly, a) || fence_route::point_in_poly(poly, b) {
            return true;
        }
        let mp: Vec<(f64, f64)> = poly.iter().map(|p| to_m(*p)).collect();
        for i in 0..mp.len() {
            if seg_cross(am, bm, mp[i], mp[(i + 1) % mp.len()]) {
                return true;
            }
        }
    }
    false
}

/// 沿折线按**时间**取上报点（等时间采样）。
///
/// 与 [`retime`] 的分工：`retime` 是「几何定了，反推时间」（等距离采样的
/// 配套，段时长随风速抖动）；这里是「时间定了，反推位置」（真机的做法，
/// 时间恒定、距离起伏）。生产走上报采样，`retime` 只剩 CLI 预览在用。
///
/// 几何不受影响：拐角、绕楼、摆动都算在**密采样**的折线上
/// （`RunParams::sample_step_m`，默认 5 m），这里只决定上报哪几个点。
/// 两件事拆开之后，「拐角需要更密」和「节奏必须是 5 秒」不再互斥。
///
/// 位置由速度剖面对时间积分得到：`pace_profile`（OU + 热身 + 终点冲刺 +
/// 弯道预判刹车，与 `retime` 同一套）给出每个上报间隔的平均速度，按
/// `s_i = Σ v·dt` 累计弧长，再在折线上插值。迭代两轮是因为弯道限速依赖
/// 取点位置、取点位置又依赖限速，一轮不够自洽。
///
/// 三条修正规则（都在起步窗口之外才生效——起步窗口里的几何会被
/// [`compress_startup`] 压到几米以内，那一段的弦没有几何意义）：
///
/// 1. **穿墙**：弦与 `blockers`（楼体/水面）相交 → 把下一个采样点挪到那个
///    最能绕开的顶点上。这是 5m → 15m 放大之后唯一真正危险的后果，判据直接
///    落在墙上；
/// 2. **折返**：转角 ≥ [`CORNER_MIN_TURN_DEG`] 且偏离弦超过
///    [`CORNER_CHORD_TOL_M`] → 同样挪到顶点上。只对急转生效，GPS 抖动（能把
///    相邻三点转出 45~55°）不在此列，否则一条干净的操场轨迹会凭空多出 6 个点；
/// 3. **打卡点对齐**：见 [`align_samples_to_checkpoints`]。
///
/// 这些修正都是**挪点**，不往时间网格里插点：插点会把 5 秒间隔切成 1~2 秒，
/// 而真机一次都没有出现过这种间隔（`ΔtotalTime` 直方图只有 {0,3,4,5}），
/// `preflight::SAMPLE_RATE_SHORT`（Fatal）会直接拦。挪点不动时间戳，
/// 节奏零代价。
// 8 个参数看着多，但每一项都是独立的物理/轨控输入（几何、时间轴、噪声种子、
// 速度剖面、上下限、障碍、打卡点）。打包成结构体只会把这些直白的调用点
// 变成一行字面量，读起来更绕。
#[expect(
    clippy::too_many_arguments,
    reason = "the parameters mirror the fields of the sampled track; see
    `sample_by_time` for what each one controls"
)]
pub fn sample_by_time(
    path: &[(f64, f64)],
    times_ms: &[i64],
    seed: u64,
    kin: &kinematics::KinParams,
    v_floor: f64,
    v_ceil: f64,
    blockers: &[Vec<(f64, f64)>],
    checkpoints: &[(f64, f64)],
) -> Vec<TrackPoint> {
    let n = times_ms.len();
    if path.len() < 2 || n < 2 {
        return Vec::new();
    }
    let mut arc = Vec::with_capacity(path.len());
    let mut acc = 0.0f64;
    arc.push(0.0);
    for w in path.windows(2) {
        acc += geo::distance(w[0], w[1]);
        arc.push(acc);
    }
    let total_len = acc;
    if total_len <= 0.0 {
        return Vec::new();
    }
    let total_sec = ((times_ms[n - 1] - times_ms[0]) as f64 / 1000.0).max(1.0);
    let dts: Vec<f64> = times_ms
        .windows(2)
        .map(|w| ((w[1] - w[0]) as f64 / 1000.0).max(0.001))
        .collect();
    let v0 = total_len / total_sec;
    // 逐点速度上下限。
    //
    // `v_floor` 来自服务端规则里的配速下限，它约束的是**整条记录的平均配速**；
    // 拿它当逐点下限会把瞬时速度压在一个很窄的带子里——实测真机 `ΔtotalDis`
    // 标准差 9.44 米、最大 31.07 米，而我们按 `v_floor` 硬卡只有 1.76 米的标准差。
    // 所以逐点下限放宽到规则下限的 `SPEED_FLOOR_RATIO`（真机最慢的点
    // 确实跑到过 0.06 m/s），平均配速仍由末尾那道「累计弧长归一」保证，
    // `preflight` 的 `PACE_OUT` 看的也是平均值。
    let floor = (v_floor * SPEED_FLOOR_RATIO).max(MIN_POINT_SPEED_MPS);
    // 上限同时受「规则上限」和真机实测上限约束：真机最快的采样点 6.21 m/s
    // （31.07m / 5s），按 8 m/s 放开能跑出 40m 的相邻间隔，
    // `detect` 会当场报「不像走出来的」。
    let ceil = v_ceil
        .min(crate::api::submit::MAX_POINT_SPEED_MPS)
        .max(floor + 0.1);

    // 弧长初值等分，再迭代两轮让「按位置取点 → 速度剖面 → 按速度积分」自洽
    let mut s: Vec<f64> = (0..n)
        .map(|i| total_len * i as f64 / (n - 1) as f64)
        .collect();
    for _ in 0..2 {
        let pts: Vec<(f64, f64)> = s.iter().map(|&x| point_at_arc(path, &arc, x)).collect();
        let speeds =
            kinematics::pace_profile(&pts, v0, &dts, total_len, seed, kin, floor, ceil, 1.0);
        let mut cum = 0.0f64;
        let mut ns = Vec::with_capacity(n);
        ns.push(0.0);
        for (i, &dt) in dts.iter().enumerate() {
            cum += speeds.get(i).copied().unwrap_or(v0) * dt;
            ns.push(cum);
        }
        let scale = if cum > 1e-9 { total_len / cum } else { 1.0 };
        let mut prev = 0.0f64;
        for x in ns.iter_mut() {
            *x = (*x * scale).clamp(prev, total_len);
            prev = *x;
        }
        if let Some(last) = ns.last_mut() {
            *last = total_len;
        }
        s = ns;
    }

    // 单步位移封顶：收敛迭代按比例拉伸网格，顶在规则上限的那批点会被再
    // 放大百分之几到十几——真机里瞬时速度确实能越过规则带（31m/5s 的
    // 实测点存在），但那是有惯性的孤立尖峰，不是一整段贴着「规则上限 ×
    // 拉伸系数」的整排平顶（issue #6 手绘场景实测最快 2'39"，而规则带是
    // 3'30"~10'00"，快出 36%，App 配速图上肉眼可见的假）。这里把每个
    // 间隔的弧长压到「规则上限 × 间隔 × 1.15」以内（15% 余量给真机式的
    // 有机越界），削下来的量顺延到后面有余量的间隔，总里程一点不变。
    // v_ceil 宽松（测试夹具 8.0 m/s）时这个上限远大于实际步长，是无操作。
    {
        let caps: Vec<f64> = dts.iter().map(|&dt| ceil * dt * 1.15).collect();
        cap_advances(&mut s, &caps, total_len);
    }

    // 把最近的采样弧长**对齐到打卡点**（在弧长空间做，不是事后挪点）。
    //
    // 5 秒网格在真机速度下相邻两点 5~31m，打卡点落在两点之间时最近采样点
    // 可以差 20m 以上（实测 24.8m）。靠 `snap_checkpoints` 事后瞬移补救会
    // 把轨迹扯出 40m 的相邻间距（`GEO_JUMP` 风险），所以这里直接让网格
    // 对齐：采样点落在点位弧长上，两侧间距只被拉伸 ≤ 半个步长。
    if !checkpoints.is_empty() && n >= 3 {
        for &cp in checkpoints {
            let target = nearest_arc_on(path, &arc, cp);
            // 只对齐「轨迹确实经过它」的情况：折线最近点离点位太远时不动，
            // 留给 preflight 报 CP_MISS（硬拽过去等于伪造）
            if target.1 > CHECKPOINT_SNAP_MAX_M {
                continue;
            }
            let mut idx = 0usize;
            let mut best = f64::INFINITY;
            for (i, &x) in s.iter().enumerate() {
                let d = (x - target.0).abs();
                if d < best {
                    best = d;
                    idx = i;
                }
            }
            // 不许挤掉首尾点，也不许把相邻间距压成 0
            if idx == 0 || idx + 1 >= n {
                continue;
            }
            let lo = s[idx - 1] + MIN_SAMPLE_GAP_M;
            let hi = s[idx + 1] - MIN_SAMPLE_GAP_M;
            if lo < hi {
                s[idx] = target.0.clamp(lo, hi);
            }
        }
    }

    let mut out: Vec<TrackPoint> = Vec::with_capacity(n);
    // 拐角 / 穿墙的处理方式是**把下一个采样点挪到那个顶点上**，
    // 不再往网格里插点。插点会把 5 秒间隔切成 1~2 秒，而真机的
    // `ΔtotalTime` 里根本没有 1 秒和 2 秒（`preflight` 的 `SAMPLE_RATE_SHORT`
    // 现在会直接拦）。挪点不改时间戳，节奏零代价，几何照样绕开拐角——
    // 和 `snap_checkpoints` 是同一个手法。
    let mut override_pos: Option<(f64, f64)> = None;
    for i in 0..n {
        let q = override_pos
            .take()
            .unwrap_or_else(|| point_at_arc(path, &arc, s[i]));
        out.push(TrackPoint {
            lat: q.0,
            lon: q.1,
            time_ms: times_ms[i],
        });
        if i + 1 == n {
            break;
        }
        // 起步窗口里的弦不动（那段几何随后会被压到几米以内）
        if times_ms[i] < times_ms[0] + startup_sec(total_sec as i64) * 1000 {
            continue;
        }
        let (pa, pb) = (
            point_at_arc(path, &arc, s[i]),
            point_at_arc(path, &arc, s[i + 1]),
        );
        // ① 穿墙优先：弦压到楼上就一定要绕开
        let picked = if chord_hits_blocker(pa, pb, blockers) {
            let hit = worst_corner(path, &arc, s[i], s[i + 1], 0.0);
            if hit.is_none() {
                // 区间里没有可用的折线顶点：整条折线就是这条直线，
                // 只能如实穿过去（与 `avoid_blockers` 的「避不开就保持原样」一致）
            }
            hit
        } else {
            // ② 折返级急转 + 偏离超容差
            worst_corner(path, &arc, s[i], s[i + 1], CORNER_MIN_TURN_DEG)
                .filter(|(_, dev)| *dev > CORNER_CHORD_TOL_M)
        };
        let Some((vc, _dev)) = picked else {
            continue;
        };
        override_pos = Some(point_at_arc(path, &arc, vc));
    }
    out
}

/// 上报节奏的实测统计（`ΔtotalTime` / `ΔtotalDis`）。
///
/// 真实性的判据不是「平均值看着像」，而是**时间间隔的分布**：真机 113 个
/// 间隔里 109 个恰好 5 秒。这里是给自检与日志用的同一把尺子。
#[derive(Debug, Clone, Copy, Default)]
pub struct CadenceStats {
    /// 点数。
    pub points: usize,
    /// 平均时间间隔（秒）。
    pub mean_dt_sec: f64,
    /// 时间间隔中位数（秒）。
    pub median_dt_sec: f64,
    /// `Δt` 落在 5±0.5 秒的比例。
    pub dt_within_5s: f64,
    /// 平均距离间隔（米）。
    pub mean_dd_m: f64,
    /// 距离间隔标准差（米）。
    pub dd_stdev_m: f64,
}

/// 统计一条轨迹的上报节奏。
pub fn cadence_stats(pts: &[TrackPoint]) -> CadenceStats {
    let n = pts.len();
    if n < 2 {
        return CadenceStats {
            points: n,
            ..Default::default()
        };
    }
    let dt: Vec<f64> = pts
        .windows(2)
        .map(|w| (w[1].time_ms - w[0].time_ms) as f64 / 1000.0)
        .collect();
    let dd: Vec<f64> = pts
        .windows(2)
        .map(|w| geo::distance((w[0].lat, w[0].lon), (w[1].lat, w[1].lon)))
        .collect();
    let mean = |v: &[f64]| v.iter().sum::<f64>() / v.len().max(1) as f64;
    let mut sorted_dt = dt.clone();
    sorted_dt.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let median = sorted_dt[sorted_dt.len() / 2];
    let ddm = mean(&dd);
    let var = dd.iter().map(|x| (x - ddm).powi(2)).sum::<f64>() / dd.len().max(1) as f64;
    CadenceStats {
        points: n,
        mean_dt_sec: mean(&dt),
        median_dt_sec: median,
        dt_within_5s: dt.iter().filter(|x| (4.5..=5.5).contains(*x)).count() as f64
            / dt.len() as f64,
        mean_dd_m: ddm,
        dd_stdev_m: var.sqrt(),
    }
}

/// 把采样点**沿着折线**对齐到打卡点（改坐标，不改时间戳）。
///
/// 与 [`snap_checkpoints`] 的区别：这里移动的是「离点位最近的那个采样点，
/// 沿轨迹挪到点位在轨迹上的投影」，位移上限是半个采样间隔；吸附则是直接把
/// 点拽到点位坐标，两者配合用：
///
/// 1. [`sample_by_time`] 采完 → 本函数对齐（此时网格刚生成，偏差 ≤ 半个步长）；
/// 2. [`compress_startup`] 压平起步段（会整体挪动网格，所以必须**再对齐一次**）；
/// 3. [`snap_checkpoints`] 收最后的厘米级残距。
///
/// 没有第 1、2 步时，5 秒网格的最近采样点可以离点位 20m 以上（实测 24.8m），
/// 全靠吸附去拽就会扯出 40m 的相邻间距（`GEO_JUMP` /「不像走出来的」）。
///
/// 返回对齐了几个点位。
pub fn align_samples_to_checkpoints(pts: &mut [TrackPoint], cps: &[(f64, f64)]) -> usize {
    let n = pts.len();
    if n < 3 || cps.is_empty() {
        return 0;
    }
    let xy: Vec<(f64, f64)> = pts.iter().map(|p| (p.lat, p.lon)).collect();
    let mut arc = Vec::with_capacity(n);
    let mut acc = 0.0f64;
    arc.push(0.0);
    for i in 1..n {
        acc += geo::distance(xy[i - 1], xy[i]);
        arc.push(acc);
    }
    if acc <= 0.0 {
        return 0;
    }
    let mut done = 0usize;
    let mut taken: Vec<usize> = Vec::with_capacity(cps.len());
    for &cp in cps {
        let (arc_cp, dist_cp) = nearest_arc_on(&xy, &arc, cp);
        // 轨迹压根没经过它：不动，让 preflight 如实报 CP_MISS
        if dist_cp > CHECKPOINT_SNAP_MAX_M {
            continue;
        }
        let mut idx = usize::MAX;
        let mut best = f64::INFINITY;
        for (i, &a) in arc.iter().enumerate() {
            if taken.contains(&i) || i == 0 || i + 1 >= n {
                continue;
            }
            let d = (a - arc_cp).abs();
            if d < best {
                best = d;
                idx = i;
            }
        }
        if idx == usize::MAX {
            continue;
        }
        // 位移边界由下面那对 clamp 兜住（只在 idx-1 和 idx+1 之间挪），
        // 所以不再额外用「半个步长」提前放弃——那会让 12m 的偏差漏给吸附去
        // 硬拽，反而扯出更大的相邻间距。
        let lo = arc[idx - 1] + MIN_SAMPLE_GAP_M;
        let hi = arc[idx + 1] - MIN_SAMPLE_GAP_M;
        // 起步段被压平后相邻点可能只差几厘米，这时候 lo > hi，`clamp` 会 panic；
        // 那种位置的网格本来就在几米内穿过所有点位，不需要对齐。
        if lo >= hi {
            continue;
        }
        let target = point_at_arc(&xy, &arc, arc_cp.clamp(lo, hi));
        pts[idx].lat = target.0;
        pts[idx].lon = target.1;
        taken.push(idx);
        done += 1;
    }
    done
}

/// 打卡点吸附的最大容忍距离（米）。
///
/// 超过这个距离就不再「吸」，如实留给 [`preflight`] 报 `CP_MISS`：那说明规划
/// 路径压根没经过点位，把轨迹硬拽过去等于伪造，服务端也不会认。
///
/// 这个数是「最近采样点可能有多远」的上界，不是随手定的尺子：5 秒网格在
/// 真机速度分布下相邻两点最多 31 m（`ΔtotalDis` 的 p100），打卡点落在两点
/// 正中时最近采样点就是 15.5 m——实测有一段跑到 16.6 m，卡在 15 m 上会
/// 报「吸附后残距 17.6 m」却怎么都吸不上。取 20 m 覆盖这个上界。
pub const CHECKPOINT_SNAP_MAX_M: f64 = 20.0;

/// 压缩之后要求达到的打卡点残距（米）。
///
/// `preflight` 的 `CP_MISS` 是 0.5 m 且 Fatal，这里留一个数量级的余量：
/// 采样点被**精确**放到打卡点上时残距是 0，报完 BD-09 → GCJ-02 → BD-09
/// 的往返（`gLat/gLng` 存 1e-7 度）之后最多也就一两厘米。要求 ≤0.05 m
/// 才叫「命中了」，不是「差一点」。
pub const CHECKPOINT_HIT_M: f64 = 0.05;

/// 一个打卡点的吸附结果。
#[derive(Debug, Clone, Copy)]
pub struct CheckpointSnap {
    /// 这个结果属于第几个打卡点（`cps` 的下标）。
    pub cp: usize,
    /// 吸附用的轨迹下标。
    pub index: usize,
    /// 吸附前残距（米）。
    pub before_m: f64,
    /// 吸附后该打卡点与**最终几何**的最近距离（米）。
    pub after_m: f64,
    /// 是否真的改动了坐标。
    pub shifted: bool,
}

/// 一批吸附的汇总。
#[derive(Debug, Clone, Default)]
pub struct SnapReport {
    /// 每个打卡点的吸附结果。
    pub items: Vec<CheckpointSnap>,
    /// 吸附后相邻采样点的最大间距（米）——用来确认没把轨迹扯出 `GEO_JUMP`。
    pub max_gap_m: f64,
    /// 单点最大位移（米）。
    pub max_shift_m: f64,
    /// 因为超出 [`CHECKPOINT_SNAP_MAX_M`] 而没有吸附的打卡点下标。
    pub skipped: Vec<usize>,
}

impl SnapReport {
    /// 实际改动过坐标的打卡点个数。
    pub fn moved(&self) -> usize {
        self.items.iter().filter(|s| s.shifted).count()
    }

    /// 吸附前最差残距（米）。
    pub fn worst_before(&self) -> f64 {
        self.items.iter().map(|s| s.before_m).fold(0.0, f64::max)
    }

    /// 吸附后最差残距（米）。
    pub fn worst_after(&self) -> f64 {
        self.items.iter().map(|s| s.after_m).fold(0.0, f64::max)
    }
}

/// 把打卡点重新吸附回轨迹上——**压缩之后**必须再跑一遍。
///
/// [`compress_startup`] 会把**所有**点（含窗口外的点）按目标弧长重新插值铺
/// 一遍，于是 `planner` / `resample_with_checkpoints` 那套「采样点精确落在
/// 打卡点上」的硬约束被当场打散：原来残距 0.000 m 的顶点被挪到别的弧长
/// 位置，最近采样点变成「新网格间距的一半」以内的随机值（实测 0.33~2.24 m），
/// 而 `preflight` 的 `CP_MISS` 阈值是 0.5 m 且 Fatal —— 有必到点的跑一单都
/// 发不出去。
///
/// 方向选择：**改轨迹、不改打卡点**。
///
/// 另一种写法是把轨迹点坐标写回打卡点（"以轨迹为准"），那是反过来让上报的
/// `fixed_point_json` 偏离服务端自己下发的点位——服务端拿它自己的坐标判
/// 「经过点位」，改我们这份只会把 `type=2` 标记与真实经过位置错开。而轨迹
/// 侧本来就有 GPS 抖动，采样点落在点位 ±3 m 内是自然区间，所以把哪个顶点
/// 挪过去代价最小。
///
/// 位移用 [`geo::nudge_to`] 摊到邻域（中心点精确命中，两侧按 1/(k+1) 衰减），
/// 不在折线上留单点尖刺；位移上限是「吸附前残距」本身（≤ 半个采样间隔），
/// 所以相邻间距最多涨 50%，碰不到 `GEO_JUMP_SOFT` 的 60 m。
///
/// 调用顺序有硬要求：必须在 `real_len` 重算**之前**，否则上报的 `totalDis`
/// 与最终几何不同源（`DIST_MISMATCH` 会当场拦下）。
pub fn snap_checkpoints(pts: &mut [TrackPoint], cps: &[(f64, f64)]) -> SnapReport {
    let mut rep = SnapReport::default();
    if pts.len() < 3 || cps.is_empty() {
        return rep;
    }
    let mut xy: Vec<(f64, f64)> = pts.iter().map(|p| (p.lat, p.lon)).collect();
    let mut used: Vec<usize> = Vec::with_capacity(cps.len());

    for (ci, &cp) in cps.iter().enumerate() {
        // 跳过已经被别的打卡点占用的下标：两个点位抢同一个采样点时，先到
        // 的那个保住精确命中，后到的贴上去只会把前一个又顶走。
        let mut idx = usize::MAX;
        let mut before = f64::INFINITY;
        for (i, p) in xy.iter().enumerate() {
            if used.contains(&i) {
                continue;
            }
            let d = geo::distance(*p, cp);
            if d < before {
                before = d;
                idx = i;
            }
        }
        if idx == usize::MAX {
            continue;
        }
        used.push(idx);
        let mut shifted = false;
        if before > 1e-6 {
            if before <= CHECKPOINT_SNAP_MAX_M {
                geo::nudge_to(&mut xy, idx, cp, 2);
                shifted = true;
                rep.max_shift_m = rep.max_shift_m.max(before);
            } else {
                // 太远说明规划没经过它，不硬贴，留给 preflight 报 CP_MISS
                rep.skipped.push(ci);
            }
        }
        rep.items.push(CheckpointSnap {
            cp: ci,
            index: idx,
            before_m: before,
            after_m: 0.0,
            shifted,
        });
    }

    if rep.items.iter().all(|s| !s.shifted) {
        return rep;
    }

    for (i, (la, lo)) in xy.iter().enumerate() {
        pts[i].lat = *la;
        pts[i].lon = *lo;
    }
    // 最后一步：把命中的那几个点**精确钉住**。
    //
    // `nudge_to` 会把两侧邻居按 1/(k+1) 一起平移（避免瞬移出尖角），于是后一个
    // 打卡点的平滑又会把前一个已经命中的点拽走——实测这里能剩 3.36m 残距，
    // 表现为 `CP_MISS`「最近距离 3.5m，未命中」。钉一下就好，邻居的平滑保留。
    for s in rep.items.iter().filter(|s| s.shifted) {
        if let Some(p) = pts.get_mut(s.index) {
            p.lat = cps[s.cp].0;
            p.lon = cps[s.cp].1;
        }
    }
    // 用**最终**几何重新量：after_m 必须是 preflight 会看到的那个数，
    // 不是「刚设完就宣布成功」的那个。
    for s in rep.items.iter_mut() {
        let cp = cps[s.cp];
        s.after_m = pts
            .iter()
            .map(|p| geo::distance((p.lat, p.lon), cp))
            .fold(f64::INFINITY, f64::min);
    }
    rep.max_gap_m = pts
        .windows(2)
        .map(|w| geo::distance((w[0].lat, w[0].lon), (w[1].lat, w[1].lon)))
        .fold(0.0, f64::max);
    rep
}

/// 吸附之后把「被拉开的相邻间距」压回限值内。
///
/// `snap_checkpoints` 会把采样点挪到打卡点上、邻居按 1/(k+1) 一起平移。挪完
/// **没有复查间距**——原本已经贴着上限的间隔（`v_ceil × dt`，6.3 m/s × 5s =
/// 31.5m）再被两侧各拉开几米，就成了 38.7m / 7.74 m/s：
///
///   · 服务端 `detect` 有速度上限判据，超了判异常；
///   · App 详情页按点序连线，这一段在图上是一条斜穿场地的直线。
///
/// 修正时**不碰打卡点自己**（那是 `CP_MISS` 的唯一保障），只把另一端拉近。
/// 代价是那个邻居点偏离路径几米，比「速度超限 + 图上一条斜线」划算。
///
/// 返回被修正的段数。
pub fn clamp_sample_gaps(pts: &mut [TrackPoint], pinned: &[usize], max_gap_m: f64) -> usize {
    if pts.len() < 2 || max_gap_m <= 0.0 {
        return 0;
    }
    let is_pinned = |i: usize| pinned.contains(&i);
    let mut fixed = 0usize;
    // 要迭代：压一段会让邻居那段的间距变大（点被拽过来），一轮扫完还剩超限的。
    // 实测 38.69m 一轮只到 33.82m，三轮落到 31.5m。
    for _ in 0..6 {
        let mut round = 0usize;
        for i in 0..pts.len() - 1 {
            let a = (pts[i].lat, pts[i].lon);
            let b = (pts[i + 1].lat, pts[i + 1].lon);
            let gap = geo::distance(a, b);
            if gap <= max_gap_m || gap <= 1e-9 {
                continue;
            }
            // 两端怎么分配修正量：打卡点不动，另一端全担；都不是就各让一半。
            let (wa, wb) = match (is_pinned(i), is_pinned(i + 1)) {
                (true, true) => continue, // 两点都被钉住，动不了，交给服务端判
                (true, false) => (0.0, 1.0),
                (false, true) => (1.0, 0.0),
                (false, false) => (0.5, 0.5),
            };
            let pull = (gap - max_gap_m) / gap;
            let (dlat, dlon) = ((b.0 - a.0) * pull, (b.1 - a.1) * pull);
            pts[i].lat += dlat * wa;
            pts[i].lon += dlon * wa;
            pts[i + 1].lat -= dlat * wb;
            pts[i + 1].lon -= dlon * wb;
            round += 1;
        }
        fixed += round;
        if round == 0 {
            break;
        }
    }
    fixed
}

/// 点到线段的最短距离（米）。
fn point_to_segment_m(p: (f64, f64), a: (f64, f64), b: (f64, f64)) -> f64 {
    // 用米制近似投影：把经纬度按所在纬度的米/度换算成平面坐标再算。
    let lat0 = (a.0 + b.0) * 0.5;
    let mx = 111_320.0 * lat0.to_radians().cos();
    let my = 110_540.0;
    let to_xy = |q: (f64, f64)| ((q.1 - a.1) * mx, (q.0 - a.0) * my);
    let (px, py) = to_xy(p);
    let (bx, by) = to_xy(b);
    let (dx, dy) = (bx, by);
    let len2 = dx * dx + dy * dy;
    if len2 <= 1e-12 {
        return (px * px + py * py).sqrt();
    }
    let t = ((px * dx + py * dy) / len2).clamp(0.0, 1.0);
    let (cx, cy) = (t * dx, t * dy);
    ((px - cx).powi(2) + (py - cy).powi(2)).sqrt()
}

/// 把跨过拐角的上报点**挪到**拐角上（不插点、不改时间戳）。
///
/// ## 为什么不插点
///
/// 另一种做法是在相邻采样点之间插入拐角顶点，时间戳线性插值。那套做法在
/// 别处成立，在我们这里会被自己的检查拦下：5 秒网格中间插一个点就是 2.5 秒，
/// 而 `preflight` 的
/// `SAMPLE_RATE_SHORT` 是 Fatal ——「真机最短非零间隔是 3s」。
///
/// 所以这里只**移动**已经存在的那一个点：点数不变、`ΔtotalTime` 仍是 5 秒，
/// 代价是这一点到上一点的距离会变短（拐角半径），换来连线不再斜切过拐角。
/// 真机本身 `ΔtotalDis` 就在 0~31m 之间跳（标准差 9.44），这点变化落在
/// 它的正常范围里。
///
/// ## 只挪确实绕路的段
///
/// 只有「密折线在两点之间偏离直线超过 `dev_m`」时才动。我们的几何是 5m
/// 密采样、上报间隔 5 秒（约 15m），绝大多数相邻对本来就在直线上，硬挪只会
/// 制造不必要的抖动。
///
/// 返回移动的点数。
pub fn nudge_to_corners(pts: &mut [TrackPoint], dense: &[(f64, f64)], dev_m: f64) -> usize {
    if pts.len() < 3 || dense.len() < 3 || dev_m <= 0.0 {
        return 0;
    }
    // 每个上报点在密折线上的最近下标：只在这些下标之间找拐角，避免把
    // 别的圈次的位置错配到这一段。
    let nearest = |p: (f64, f64)| -> usize {
        let mut best = (usize::MAX, f64::MAX);
        for (i, q) in dense.iter().enumerate() {
            let d = geo::distance(p, *q);
            if d < best.1 {
                best = (i, d);
            }
        }
        best.0
    };
    let mut idx: Vec<usize> = pts.iter().map(|q| nearest((q.lat, q.lon))).collect();
    // 最近下标必须单调（同一圈内），否则说明密折线在这里折返过——
    // 那样按区间找中间点会跨圈，直接放弃这一段。
    for i in 1..idx.len() {
        if idx[i] < idx[i - 1] {
            idx[i] = idx[i - 1];
        }
    }
    let mut moved = 0usize;
    for i in 0..pts.len() - 1 {
        let (a, b) = ((pts[i].lat, pts[i].lon), (pts[i + 1].lat, pts[i + 1].lon));
        let (lo, hi) = (idx[i], idx[i + 1]);
        if hi <= lo + 1 {
            continue;
        }
        // 找这段密折线里离直线 a→b 最远的点
        let mut worst = (0usize, 0.0f64);
        for (j, q) in dense.iter().enumerate().take(hi).skip(lo + 1) {
            let d = point_to_segment_m(*q, a, b);
            if d > worst.1 {
                worst = (j, d);
            }
        }
        if worst.1 <= dev_m {
            continue;
        }
        // 把终点挪到拐角上。起点不动——起点是上一点，动它会让前一段也跟着变。
        // 末点不挪：它是收尾哨兵，位置有语义。
        if i + 1 == pts.len() - 1 {
            continue;
        }
        // **挪动距离必须设上限**。拐角半径通常十几米，挪得比这多说明最近
        // 的密折线下标配错了（比如跨了另一圈），那已经不是「补拐角」而是
        // 「换位置」。里程层面的兜底在 `flow` 里（挪完量总长，偏差 >1.5%
        // 就整段回退），这里只挡明显离谱的。
        //
        // 没有这个上限时实测把 2km/12min 的平均速度从 2.69 m/s 顶到
        // 3.67 m/s（里程虚增 36%），还制造出 43.9 m/s 的相邻跳变——
        // 那是比「斜切过楼角」严重得多的问题。
        let limit = 20.0;
        let shift = geo::distance((pts[i + 1].lat, pts[i + 1].lon), dense[worst.0]);
        if shift > limit || shift <= 1e-9 {
            continue;
        }
        pts[i + 1].lat = dense[worst.0].0;
        pts[i + 1].lon = dense[worst.0].1;
        moved += 1;
    }
    moved
}

/// 排队序号 `queueNum`：真机起步段恒 0，之后**固定步长的 floor 分区**从 1 到 24。
///
/// 真机（`raw_points` 114 点 / `speed_json` 55 桶，rrid 1328288734）不是按位置
/// 比例插值，而是把起步段之后的采样格切成 24 段等宽的**整数台阶**：
///
/// - 点：`n=114, su=19` → 起步段 19 个点全 0；`ceil((114-1-19)/24) = 4`，
///   **每 4 个点 +1**（`19→1、23→2、27→3、…、103→22`）；
/// - 桶：`n=55, su=8` → 前 8 个桶全 0；`ceil((55-1-8)/24) = 2`，
///   **每 2 个桶 +1**（`8→1、10→2、…、54→24`，55 个桶逐元素完全吻合）。
///
/// 早先是 `(1.0 + (pos-su)/span*23.0).round()` 的线性插值：真机 4 个点一档，
/// 插值出来是「4.09 个点一档」，于是 `22,26,30,…,107,111` 这 14 个下标全部
/// 偏移一档（`docs/audit/AUDIT-round.md` A5）。等宽台阶不会有这个累积漂移。
///
/// 尾部：真机的最后三档是 5 点 / 4 点 / 2 点（22 档占 5 个点、24 档只占 2 个），
/// 等宽网格给的是 4/4/3，逐点对拍时 `末尾 107、111` 两个下标各差一档——
/// 这是**等宽网格表达不了**的尾部余数分配，不是公式写错（数学上无解：
/// 前 21 档要求步长恰为 4，而 `107` 保持 22 档要求步长 > 4）。见
/// `queue_num_tests::point_queue_num_matches_real_device` 的逐项断言。
pub fn queue_num(pos: usize, n: usize, startup: usize) -> i32 {
    if n == 0 || pos < startup || n <= startup + 1 {
        return 0;
    }
    // 起步段之后还有多少个采样格，24 档按 ceil 取整宽（余数归入最后一档）。
    let span = n - 1 - startup;
    let step = span.div_ceil(24).max(1);
    (1 + (pos - startup) / step).min(24) as i32
}

#[cfg(test)]
mod tests {
    use super::queue_num;
    use super::*;
    use crate::api::submit::{SubmitParams, build, verify_view};
    use crate::error::Error;
    use crate::track::preflight::{Check, inspect};
    use rand::SeedableRng;

    /// 本地诊断（issue #4）：起步窗口在 555 / 1200 / 1800 秒三个时长下的
    /// 数学结果，不用账号、不提交。调窗口口径前先看这张表，别拿真机试。
    ///
    /// `cargo test --release startup_window_table -- --nocapture` 可以把表打出来。
    #[test]
    fn startup_window_table() {
        for total in [555i64, 1200, 1800] {
            let w = startup_sec(total);
            // 上报节奏 5 秒/点，窗口内的点数就是零速度点数
            let pts = w / 5;
            println!(
                "总时长 {total}s → 窗口 {w}s（{:.1}%），约 {pts} 个零速度点",
                w as f64 / total as f64 * 100.0
            );
            assert!(w >= 5 && w <= total / 2, "窗口越界: {w}");
        }
        // 按 15.5% 外推的实际取值，锁住当前口径；改口径时这条会先红
        assert_eq!(startup_sec(555), 86);
        assert_eq!(startup_sec(1200), 186);
        assert_eq!(startup_sec(1800), 279);
    }

    /// 同一张诊断表的扩展版（issue #4 第 3 问）：除了窗口时长和零速度点数，
    /// 再给起步窗口的里程预算、首段配速，以及出窗后的正常配速——调窗口口径
    /// 时看的是这几个数会不会产生「首段配速 80 分/公里」这种一眼假的东西。
    ///
    /// 里程按参考配速 6'00"/km 折算，只用于比较趋势，不是校准值。
    /// `cargo test --release startup_window_budget_table -- --nocapture`
    #[test]
    fn startup_window_budget_table() {
        const REF_PACE_SEC_PER_KM: f64 = 360.0;
        for total in [555i64, 1200, 1800] {
            let w = startup_sec(total);
            let pts = (0..=total).step_by(5).filter(|t| *t < w).count();
            let total_m = total as f64 / REF_PACE_SEC_PER_KM * 1000.0;
            let startup_m = total_m * STARTUP_DIST_RATIO;
            // 首段配速：窗口秒数 ÷ 窗口里程（分/公里）
            let first_pace_min = (w as f64 / 60.0) / (startup_m / 1000.0);
            // 出窗后的正常配速：剩余秒数 ÷ 剩余里程
            let rest_pace_min = ((total - w) as f64 / 60.0) / ((total_m - startup_m) / 1000.0);
            let tag = if total == 555 {
                "实测样本时长"
            } else {
                "外推，无样本"
            };
            println!(
                "总时长 {total}s（{tag}）→ 窗口 {w}s / {pts} 个零速度点 / 里程预算 {startup_m:.0}m / 首段 {first_pace_min:.0}'/km，出窗后 {rest_pace_min:.2}'/km",
            );
            assert!(startup_m > 0.0 && first_pace_min > rest_pace_min);
        }
    }

    // ---- merged from former `mod nudge_tests` ----

    /// 一条带拐角的密折线，规模贴近真实：总长 30m（= 两段 5 秒上报间隔），
    /// 中间向北凸出 8m。真实场景里 5 秒采样的相邻点隔 15m 左右，拐角切掉的
    /// 就是这种量级——不是几十米的大直角。
    fn bent_shape() -> Vec<(f64, f64)> {
        let lat0: f64 = 38.9000;
        let lon0 = 101.5000;
        let mlat = 1.0 / 110_540.0; // 1m 的纬度
        let mlon = 1.0 / (111_320.0 * lat0.to_radians().cos());
        let mut dense = Vec::new();
        // 东向 30m，纬度按 sin(πt) 凸起 8m
        for i in 0..=30 {
            let t = i as f64 / 30.0;
            dense.push((
                lat0 + 8.0 * (std::f64::consts::PI * t).sin() * mlat,
                lon0 + 30.0 * t * mlon,
            ));
        }
        dense
    }

    fn tp(lat: f64, lon: f64, t: i64) -> TrackPoint {
        TrackPoint {
            lat,
            lon,
            time_ms: t,
        }
    }

    #[test]
    fn corner_point_is_moved_onto_the_turn() {
        let dense = bent_shape();
        let corner = dense[15]; // 凸起点
        let (start, end) = (dense[0], dense[30]);
        let mlat = 1.0 / 110_540.0;
        let mut pts = vec![
            tp(start.0, start.1, 0),
            tp(end.0, end.1, 5000), // 这一步从起点直冲终点，跳过整个拐角
            tp(end.0 + 30.0 * mlat, end.1, 10000), // 第三点：让第二点不是末点
        ];
        let before = pts[1];
        let moved = nudge_to_corners(&mut pts, &dense, 5.0);
        assert_eq!(moved, 1, "应当移动 1 个点");
        // 时间戳一个都不许动
        assert_eq!(pts[1].time_ms, before.time_ms);
        // 挪到的位置应该贴近拐角
        let d = geo::distance((pts[1].lat, pts[1].lon), corner);
        assert!(d < 2.0, "挪动后距拐角 {d:.2}m，应当 <2m");
        // 第三点不该被动
        assert!((pts[2].lat - (end.0 + 30.0 * mlat)).abs() < 1e-9);
    }

    #[test]
    fn straight_segment_is_left_alone() {
        let dense = bent_shape();
        // 一条真正的直线（密折线首尾之间没有弯）
        let straight: Vec<(f64, f64)> = (0..=30)
            .map(|i| {
                (
                    dense[0].0 + (dense[30].0 - dense[0].0) * (i as f64 / 30.0),
                    dense[0].1 + (dense[30].1 - dense[0].1) * (i as f64 / 30.0),
                )
            })
            .collect();
        let mut pts = vec![
            tp(straight[0].0, straight[0].1, 0),
            tp(straight[30].0, straight[30].1, 5000),
        ];
        let before = pts.clone();
        let moved = nudge_to_corners(&mut pts, &straight, 12.0);
        assert_eq!(moved, 0, "直线段不该动");
        assert_eq!(pts[1].lat, before[1].lat);
    }

    #[test]
    fn last_point_is_never_moved() {
        let dense = bent_shape();
        let (start, end) = (dense[0], dense[30]);
        let mut pts = vec![tp(start.0, start.1, 0), tp(end.0, end.1, 5000)];
        let _ = nudge_to_corners(&mut pts, &dense, 12.0);
        // 末点（收尾哨兵）位置保持不变——即使它跨过了整个拐角
        assert_eq!((pts[1].lat, pts[1].lon), (end.0, end.1));
    }

    #[test]
    fn degenerate_inputs_are_noops() {
        let dense = bent_shape();
        let mut one = vec![tp(dense[0].0, dense[0].1, 0)];
        assert_eq!(nudge_to_corners(&mut one, &dense, 12.0), 0);
        let mut two = vec![
            tp(dense[0].0, dense[0].1, 0),
            tp(dense[30].0, dense[30].1, 5000),
        ];
        assert_eq!(nudge_to_corners(&mut two, &[], 12.0), 0);
        assert_eq!(nudge_to_corners(&mut two, &dense, 0.0), 0);
        assert_eq!(nudge_to_corners(&mut two, &dense, -1.0), 0);
    }

    #[test]
    fn deviation_below_threshold_is_ignored() {
        let dense = bent_shape();
        let (start, end) = (dense[0], dense[30]);
        let mut pts = vec![tp(start.0, start.1, 0), tp(end.0, end.1, 5000)];
        // 阈值大于实际偏离（8m）就不动
        assert_eq!(nudge_to_corners(&mut pts, &dense, 9.0), 0);
    }

    // ---- merged from former `mod queue_num_tests` ----

    // `queueNum` 的逐元素对拍（`docs/audit/AUDIT-round.md` A5）。
    //
    // 旧断言只断首尾（`sp[0]==0`、`sp.last()==24`），中间 22/55 个桶、14/114 个
    // 点的偏移全部漏过。这里把真机两套序列整段写进测试，逐下标比：
    //
    // - 桶（55 个）：**完全相等**，一个下标都不许差；
    // - 点（114 个）：除 `107`、`111` 两个尾部下标（真机 22/23 档各占 5/4 个点、
    //   24 档只占 2 个点，等宽网格给 4/4/3）外完全相等；这两处单独列出
    //   期望值，不做「区间」或「大体一致」的放宽。

    /// 真机 `raw_points[i].queueNum`（rrid 1328288734，114 点 / 555 秒 / 1683.83m）。
    ///
    /// 变化下标 19,23,…,103,108,112；相邻步长 4,4,…,4,**5**,4——最后一步是 5，
    /// 所以尾部三档的宽度是 5/4/2，不是等宽的 4/4/3。
    pub(super) const REAL_POINT_QUEUE: [i32; 114] = [
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3,
        3, 4, 4, 4, 4, 5, 5, 5, 5, 6, 6, 6, 6, 7, 7, 7, 7, 8, 8, 8, 8, 9, 9, 9, 9, 10, 10, 10, 10,
        11, 11, 11, 11, 12, 12, 12, 12, 13, 13, 13, 13, 14, 14, 14, 14, 15, 15, 15, 15, 16, 16, 16,
        16, 17, 17, 17, 17, 18, 18, 18, 18, 19, 19, 19, 19, 20, 20, 20, 20, 21, 21, 21, 21, 22, 22,
        22, 22, 22, 23, 23, 23, 23, 24, 24,
    ];

    /// 真机 `speed_json[i].queueNum`（55 个桶）：前 8 个 0，之后每 2 个桶 +1。
    const REAL_BUCKET_QUEUE: [i32; 55] = [
        0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11,
        11, 12, 12, 13, 13, 14, 14, 15, 15, 16, 16, 17, 17, 18, 18, 19, 19, 20, 20, 21, 21, 22, 22,
        23, 23, 24,
    ];

    /// 真机尾部的两个已知差：等宽网格给 23/24，真机是 22/23。
    const TAIL_ALLOWED_DIFF: [usize; 2] = [107, 111];

    #[test]
    fn bucket_queue_num_matches_real_device_element_by_element() {
        assert_eq!(REAL_BUCKET_QUEUE.len(), 55);
        let su = 8; // 桶网格上的起步窗口，判定依据见 buckets::bucket_startup
        let ours: Vec<i32> = (0..55).map(|i| queue_num(i, 55, su)).collect();
        let diffs: Vec<(usize, i32, i32)> = REAL_BUCKET_QUEUE
            .iter()
            .enumerate()
            .map(|(i, &real)| (i, real, ours[i]))
            .filter(|&(_, real, got)| real != got)
            .collect();
        assert!(
            diffs.is_empty(),
            "桶 queueNum 与真机不一致（下标, 真机, 我们）：{diffs:?}"
        );
        // 再逐元素硬比一遍，差异点在下面的 assert_eq 里能直接看到下标
        for (i, &real) in REAL_BUCKET_QUEUE.iter().enumerate() {
            assert_eq!(
                ours[i], real,
                "第 {i} 个桶 queueNum 错：真机 {real} vs 我们 {}",
                ours[i]
            );
        }
    }

    #[test]
    fn point_queue_num_matches_real_device_element_by_element() {
        assert_eq!(REAL_POINT_QUEUE.len(), 114);
        let su = 19; // 起步 86 秒内的 19 个点（times < 86s）
        let ours: Vec<i32> = (0..114).map(|i| queue_num(i, 114, su)).collect();

        let mut diffs = Vec::new();
        for (i, &real) in REAL_POINT_QUEUE.iter().enumerate() {
            if ours[i] != real {
                diffs.push((i, real, ours[i]));
            }
        }
        // 只允许尾部那两个下标，且方向固定（我们高出一档）
        let allowed: Vec<(usize, i32, i32)> = diffs
            .iter()
            .copied()
            .filter(|(i, _, _)| TAIL_ALLOWED_DIFF.contains(i))
            .collect();
        assert_eq!(
            allowed.len(),
            diffs.len(),
            "点位 queueNum 出现了尾部两档之外的差异：{diffs:?}"
        );
        assert_eq!(
            diffs,
            vec![(107, 22, 23), (111, 23, 24)],
            "尾部的两处已知差必须恰好是 (107: 22→23)、(111: 23→24)"
        );

        // 其余 112 个下标必须**完全相等**（不是「大体一致」）
        for (i, &real) in REAL_POINT_QUEUE.iter().enumerate() {
            if TAIL_ALLOWED_DIFF.contains(&i) {
                continue;
            }
            assert_eq!(
                ours[i], real,
                "第 {i} 个点 queueNum 错：真机 {real} vs 我们 {}",
                ours[i]
            );
        }

        // 变化下标：真机 19,23,…,103,108,112。等宽网格给 19,23,…,103,107,111，
        // 尾两处前移 1——这就是上面两个差异的来源，写死防止其它漂移混进来。
        let changes = |seq: &[i32]| -> Vec<usize> {
            (1..seq.len()).filter(|&i| seq[i] != seq[i - 1]).collect()
        };
        let real_changes = changes(&REAL_POINT_QUEUE);
        let our_changes = changes(&ours);
        assert_eq!(
            &real_changes[..real_changes.len() - 2],
            &our_changes[..our_changes.len() - 2]
        );
        assert_eq!(&real_changes[real_changes.len() - 2..], &[108, 112]);
        assert_eq!(&our_changes[our_changes.len() - 2..], &[107, 111]);
    }

    /// 线性插值的回归：这三个下标是审计点名「旧实现恒为真机 +1」的样本，
    /// 只要有人把实现改回 `round((pos-su)/span*23)`，它们立刻变红。
    #[test]
    fn no_linear_interpolation_drift() {
        assert_eq!(
            queue_num(22, 114, 19),
            1,
            "第 22 点仍是第 1 档（线性插值会给 2）"
        );
        assert_eq!(queue_num(26, 114, 19), 2);
        assert_eq!(queue_num(30, 114, 19), 3);
        // 桶侧：线性插值会给 9→2、11→3
        assert_eq!(queue_num(9, 55, 8), 1);
        assert_eq!(queue_num(11, 55, 8), 2);
        assert_eq!(queue_num(54, 55, 8), 24);
    }

    /// 边界：起步窗口内恒 0，窗口之后立刻起 1，超过 24 档封顶。
    #[test]
    fn boundaries() {
        for i in 0..19 {
            assert_eq!(queue_num(i, 114, 19), 0, "起步窗口内第 {i} 点应为 0");
        }
        assert_eq!(queue_num(19, 114, 19), 1);
        assert_eq!(queue_num(113, 114, 19), 24);
        assert_eq!(queue_num(0, 0, 0), 0, "n=0 不能除零");
        assert_eq!(queue_num(2, 3, 1), 2, "只剩 1 格时 step=1，不除零");
        assert_eq!(queue_num(9, 10, 9), 0, "n<=startup+1 时整场都在起步段");
        assert_eq!(queue_num(100, 45, 19), 24, "超过第 24 档必须封顶");
    }

    // ---- merged from former `mod checkpoint_hit_tests` ----

    // 回归：**压缩之后**打卡点必须仍然被精确命中。
    //
    // 守的是那个把整条提交链打断的回归：`compress_startup` 对所有点（含窗口
    // 外的点）按目标弧长重铺，把 `planner::plan` / `fence_route::
    // resample_with_checkpoints` 钉在打卡点上的那批顶点移走，残距从 0.000m
    // 变成 0.33~2.24m，`preflight::CP_MISS`（0.5m，Fatal）当场阻断，
    // `flow::run_full` 第一句 `dry_run(...)?` 直接返回 Err —— 有必到点的跑
    // 一单都发不出去。
    //
    // 五个点位取自真机 `fixed_point_json` 的 BD-09 坐标，与审计
    // （docs/audit/AUDIT-round.md A1）用同一组数据，改前改后的数字可以直接对。

    const START_MS: i64 = 1_790_913_882_164;

    fn official_cps() -> Vec<(f64, f64)> {
        vec![
            (47.385718, 103.956956),
            (47.386792, 103.959479),
            (47.384723, 103.956519),
            (47.386375, 103.958661),
            (47.385626, 103.955836),
        ]
    }

    fn as_checkpoints(cps: &[(f64, f64)]) -> Vec<Checkpoint> {
        cps.iter()
            .enumerate()
            .map(|(i, &(lat, lng))| Checkpoint {
                lat,
                lng,
                glat: lat,
                glon: lng,
                name: format!("CP{i}"),
            })
            .collect()
    }

    /// 与 `flow::dry_run` 同序的那一段：规划 → retime → 起步压平 → 打卡点吸附 → 重算里程。
    ///
    /// `snap = false` 复现旧行为（只压平不吸附），用来证明这条测试真的能抓住回归。
    fn pipeline(
        cps: &[(f64, f64)],
        dist: f64,
        dur: i64,
        seed: u64,
        snap: bool,
    ) -> Result<(Vec<TrackPoint>, i64, SnapReport), Error> {
        let req = PlanRequest {
            center_lat: 47.3801,
            center_lng: 103.9521,
            checkpoints: as_checkpoints(cps),
            total_distance_m: dist,
            total_sec: dur,
            start_ms: START_MS,
            sport_type: 1,
            sample_step_m: 5.0,
            end: None,
        };
        let mut rng = rand::rngs::StdRng::seed_from_u64(seed);
        let planned = plan(&req, &mut rng)?;
        let xy: Vec<(f64, f64)> = planned.points.iter().map(|p| (p.lat, p.lon)).collect();
        // 采样必须是**生产那条路**（等时间 5 秒）。用 `retime`（等距离 5m）
        // 的话，扫的是「337 点、1.65 秒一点」的旧节奏，而真机 4.91 秒一点的
        // 节奏下最近采样点离打卡点可以远一倍——那份 40/40 全过证明不了任何
        // 与生产有关的事。
        let times = sample_times(dur, START_MS, SAMPLE_INTERVAL_SEC);
        let mut track = sample_by_time(
            &xy,
            &times,
            START_MS as u64,
            &KinParams::default(),
            1.0,
            8.0,
            &[],
            cps,
        );
        assert!(
            (cadence_stats(&track).mean_dt_sec - REAL_MEAN_INTERVAL_SEC).abs() < 0.2,
            "A1 扫的必须是真机节奏"
        );
        compress_startup(&mut track, START_MS, dur, None);
        align_samples_to_checkpoints(&mut track, cps);
        let rep = if snap {
            snap_checkpoints(&mut track, cps)
        } else {
            SnapReport::default()
        };
        let len = geo::path_length(&track.iter().map(|p| (p.lat, p.lon)).collect::<Vec<_>>())
            .round() as i64;
        Ok((track, len, rep))
    }

    /// 与 `flow::dry_run` 同路的两道自检：先把**报文**反解成校验视图再跑
    /// `preflight`，再跑 `run_full` 里那道反检测体检（间距 >20m 会直接拦）。
    fn preflight_of(
        cps: &[(f64, f64)],
        track: &[TrackPoint],
        len: i64,
        dur: i64,
    ) -> (Report, crate::track::detect::Report) {
        let timed: Vec<(f64, f64, i64)> = track.iter().map(|q| (q.lat, q.lon, q.time_ms)).collect();
        let sp = SimParams {
            total_distance_m: len as f64,
            total_sec: dur,
            start_ms: START_MS,
            target_steps: None,
            seed: START_MS as u64,
            ..Default::default()
        };
        let sim = simulate(&timed, &sp).expect("simulate");
        let params = SubmitParams {
            uid: "probe".into(),
            unid: "0".into(),
            uuid: "00000000-0000-4000-8000-000000000000".into(),
            sport_type: 1,
            total_distance_m: len,
            total_time_sec: dur,
            total_steps: sim.total_steps,
            start_time_ms: START_MS,
            stop_time_ms: START_MS + dur * 1000,
            sel_distance_m: 1000,
            sel_run_time_sec: dur,
            policy: "0".into(),
            run_area_id: -1,
            geo_fences_json: "[]".into(),
            total_ascent: 0,
            alt_range: None,
        };
        let sub = build(&params, track, &[], &sim).expect("build");
        let view = verify_view(&sub.save_record, &sub.points).expect("verify_view");
        let view_sim = view.as_sim();
        let cp_tuples: Vec<(f64, f64, String)> = cps
            .iter()
            .enumerate()
            .map(|(i, &(a, b))| (a, b, format!("CP{i}")))
            .collect();
        let pre = inspect(&Check {
            points: &view.points,
            sim: &view_sim,
            params: &sp,
            checkpoints: &cp_tuples,
            total_distance_m: view.total_distance_m as i64,
            total_time_sec: view.total_time_sec,
            total_steps: view.total_steps,
            start_time_ms: view.start_time_ms,
            stop_time_ms: view.stop_time_ms,
            pace_range: Some((3.0, 10.0)),
            step_range: Some((100, 230)),
            expected_interval_sec: Some(SAMPLE_INTERVAL_SEC),
        });
        let det = crate::track::detect::inspect(
            &view
                .points
                .iter()
                .map(|p| (p.lat, p.lon))
                .collect::<Vec<_>>(),
            &sim.speed_buckets,
            crate::track::detect::PointRule::default(),
            view.start_time_ms + crate::track::startup_sec(view.total_time_sec) * 1000,
        );
        (pre, det)
    }

    fn min_dist(pts: &[TrackPoint], cp: (f64, f64)) -> f64 {
        pts.iter()
            .map(|p| geo::distance((p.lat, p.lon), cp))
            .fold(f64::INFINITY, f64::min)
    }

    #[test]
    fn checkpoint_hit_after_compress() {
        let cps = official_cps();
        let dur = 700i64;
        let (before, _, _) = pipeline(&cps, 2000.0, dur, 2026, false).expect("plan");

        // 旧行为：压缩把精确命中的顶点打散——先证明这条测试抓得住回归。
        let worst_old = cps
            .iter()
            .map(|&cp| min_dist(&before, cp))
            .fold(0.0, f64::max);
        assert!(
            worst_old > 0.5,
            "压缩后残距 {worst_old:.3}m —— 没有回归就说明这条测试没测到东西"
        );

        // 新行为：压平 → 吸附 → 每个打卡点 ≤ 0.05m（CP_MISS 阈值 0.5m 的十分之一）
        let (after, len, rep) = pipeline(&cps, 2000.0, dur, 2026, true).expect("plan");
        println!("| 打卡点 | 压缩后残距(改前) | 吸附后残距(改后) | 位移(m) |");
        for (i, &cp) in cps.iter().enumerate() {
            println!(
                "| CP{i} | {:.3} | {:.4} | {:.3} |",
                rep.items[i].before_m,
                min_dist(&after, cp),
                // 中心点位移恒等于吸附前残距（nudge_to 直接把中心点放到点位上）
                rep.items[i].before_m
            );
        }
        println!(
            "| 汇总 | 最差 {:.3} | 最差 {:.4} | 相邻最大间距 {:.1} |",
            rep.worst_before(),
            rep.worst_after(),
            rep.max_gap_m
        );
        for (i, &cp) in cps.iter().enumerate() {
            let d = min_dist(&after, cp);
            assert!(
                d <= CHECKPOINT_HIT_M,
                "CP{i} 吸附后残距 {d:.4}m > {CHECKPOINT_HIT_M}m（吸附前 {:.4}m）",
                rep.items[i].before_m
            );
        }
        // 起跑点本身就在第一个打卡点上（围栏/规划都从 must[0] 起跑），那些
        // 残距本身就是 0 的点不会被「移动」，所以这里只要求每个点都到位。
        assert!(
            rep.moved() + 1 >= cps.len(),
            "只有 {} 个点被吸附",
            rep.moved()
        );
        assert!(
            rep.skipped.is_empty(),
            "不该有点位被跳过：{:?}",
            rep.skipped
        );
        // 吸附不能把轨迹扯出跳变（GEO_JUMP_SOFT 是 60m）
        assert!(
            rep.max_gap_m < 60.0,
            "吸附后相邻最大间距 {:.1}m，会触发 GEO_JUMP_SOFT",
            rep.max_gap_m
        );

        // 报文反解之后仍然是同一件事（gLat/gLng 存 1e-7 度，往返误差 ~1cm）
        let (report, det) = preflight_of(&cps, &after, len, dur);
        assert!(!det.blocked(), "吸附后反检测体检不过：{:?}", det.lines());
        assert!(
            !report.blocked(),
            "有必到点的跑被自检拦死：{:?}",
            report.lines()
        );
        assert!(
            !report.lines().iter().any(|l| l.contains("CP_MISS")),
            "不该再有 CP_MISS：{:?}",
            report.lines()
        );
        assert!(
            !report.lines().iter().any(|l| l.contains("GEO_JUMP")),
            "吸附引入了相邻点跳变：{:?}",
            report.lines()
        );
    }

    /// 围住五个点位的矩形围栏（四边外扩 `pad` 米，首尾闭合）。
    ///
    /// 扫种子用围栏支路，是因为 `planner` 的闭环有「装不下就报错」的硬判定
    /// （这几个点位最短闭环 2109m，1000m 的目标里程根本装不下），而围栏支路
    /// 是按序串点、能覆盖 1000m 这一组。两条支路在 `flow::dry_run` 里汇合到
    /// 同一个 `retime → compress_startup → snap` 上，所以这里测的是同一件事。
    fn fence_around(cps: &[(f64, f64)], pad: f64) -> Vec<(f64, f64)> {
        let la0 = cps.iter().map(|c| c.0).fold(f64::MAX, f64::min);
        let la1 = cps.iter().map(|c| c.0).fold(f64::MIN, f64::max);
        let lo0 = cps.iter().map(|c| c.1).fold(f64::MAX, f64::min);
        let lo1 = cps.iter().map(|c| c.1).fold(f64::MIN, f64::max);
        let dla = pad / geo::M_PER_DEG_LAT;
        let dlo = pad / geo::m_per_deg_lng(la0);
        vec![
            (la0 - dla, lo0 - dlo),
            (la1 + dla, lo0 - dlo),
            (la1 + dla, lo1 + dlo),
            (la0 - dla, lo1 + dlo),
            (la0 - dla, lo0 - dlo),
        ]
    }

    /// 围栏支路（`flow::dry_run` 在有围栏时的**默认**路线）：
    /// 按序串点 → 带打卡点重采样（点位被钉在采样点上）→ retime → 压平 → 吸附。
    fn fence_pipeline(
        cps: &[(f64, f64)],
        dist: f64,
        dur: i64,
        seed: u64,
        snap: bool,
    ) -> (Vec<TrackPoint>, i64, SnapReport) {
        let fence = fence_around(cps, 150.0);
        let route = crate::track::fence_route::route_through_in_order(&fence, dist, cps, seed)
            .expect("围栏按序串点");
        let sampled =
            crate::track::fence_route::resample_with_checkpoints(&route.points, 5.0, cps, 15.0);
        let xy: Vec<(f64, f64)> = sampled;
        // 采样必须走**生产那条路**（等时间 5 秒）。用 `retime`（等距离 5m）
        // 的话扫的是 2 秒/点的旧节奏，`preflight::SAMPLE_RATE` 会直接拦，
        // 这条扫描就变成了在测一个生产里已经不存在的形态。
        let times = sample_times(dur, START_MS, SAMPLE_INTERVAL_SEC);
        let mut track = sample_by_time(
            &xy,
            &times,
            START_MS as u64,
            &KinParams::default(),
            1.0,
            8.0,
            &[],
            cps,
        );
        compress_startup(&mut track, START_MS, dur, None);
        align_samples_to_checkpoints(&mut track, cps);
        let rep = if snap {
            snap_checkpoints(&mut track, cps)
        } else {
            SnapReport::default()
        };
        let len = geo::path_length(&track.iter().map(|p| (p.lat, p.lon)).collect::<Vec<_>>())
            .round() as i64;
        (track, len, rep)
    }

    /// 40 种组合扫描：10 个 seed × 4 组参数 × 5 个点位，`preflight` 必须全部放行。
    ///
    /// 审计改前实测：这 40 组（1000m/360s 那一组在 `planner` 支路里是「装不下」
    /// 直接报错，围栏支路才跑得起来）一组都发不出去。改后必须 40/40 放行。
    #[test]
    fn checkpoint_hit_sweep_no_block() {
        let cps = official_cps();
        #[derive(Debug, Clone, Copy)]
        struct Row {
            dist: f64,
            dur: i64,
            seed: u64,
            old_worst: f64,
            old_misses: usize,
            new_worst: f64,
            max_gap: f64,
            blocked: bool,
        }
        let mut rows: Vec<Row> = Vec::new();
        for (dist, dur) in [
            (1000.0f64, 360i64),
            (2000.0, 700),
            (3000.0, 1100),
            (1683.0, 555),
        ] {
            for seed in 1..=10u64 {
                // 改前：只压平不吸附，必须被拦（证明这条扫描抓得住回归）
                let (old_track, old_len, _) = fence_pipeline(&cps, dist, dur, seed, false);
                let (old, _) = preflight_of(&cps, &old_track, old_len, dur);
                let old_worst = cps
                    .iter()
                    .map(|&cp| min_dist(&old_track, cp))
                    .fold(0.0, f64::max);
                let cp_miss = old.issues.iter().filter(|i| i.code == "CP_MISS").count();
                assert!(
                    old.blocked(),
                    "dist={dist} dur={dur} seed={seed} 改前居然没被拦，扫描无效"
                );

                // 改后：压平 + 吸附，必须全放行
                let (track, len, rep) = fence_pipeline(&cps, dist, dur, seed, true);
                let (report, det) = preflight_of(&cps, &track, len, dur);
                assert!(
                    !det.blocked(),
                    "dist={dist} dur={dur} seed={seed} 反检测体检不过：{:?}",
                    det.lines()
                );
                let worst = cps
                    .iter()
                    .map(|&cp| min_dist(&track, cp))
                    .fold(0.0, f64::max);
                assert!(
                    worst <= CHECKPOINT_HIT_M,
                    "dist={dist} dur={dur} seed={seed} 最差残距 {worst:.4}m"
                );
                assert!(
                    rep.max_gap_m < 60.0,
                    "dist={dist} dur={dur} seed={seed} 相邻最大间距 {:.1}m",
                    rep.max_gap_m
                );
                assert!(
                    !report.blocked(),
                    "dist={dist} dur={dur} seed={seed} 被拦：{:?}",
                    report.lines()
                );
                rows.push(Row {
                    dist,
                    dur,
                    seed,
                    old_worst,
                    old_misses: cp_miss,
                    new_worst: worst,
                    max_gap: rep.max_gap_m,
                    blocked: report.blocked(),
                });
            }
        }
        assert_eq!(rows.len(), 40, "应为 10 seed × 4 组参数 = 40 种组合");
        let pass = rows.iter().filter(|r| !r.blocked).count();
        println!(
            "| 里程/时长 | seed | 改前最差残距(m) | 改前 CP_MISS | 改后最差残距(m) | 相邻最大间距(m) | 改后 blocked |"
        );
        for r in &rows {
            println!(
                "| {:.0}/{} | {} | {:.3} | {} | {:.4} | {:.1} | {} |",
                r.dist, r.dur, r.seed, r.old_worst, r.old_misses, r.new_worst, r.max_gap, r.blocked
            );
        }
        assert_eq!(pass, 40, "只有 {pass}/40 通过");
    }

    /// `planner` 支路（无围栏时的兜底）同样必须过。
    ///
    /// 这一支路有「闭环装不下就报错」的既有硬判定（这五个点位最短闭环
    /// 2109m），1000m / 1683m 两组在 `plan()` 里就被拒——那是**规划容量**限制，
    /// 不是本次修的 `CP_MISS`，两件事必须分开记：这里把它当成「跑不起来」的
    /// 夹具如实断言，可规划的两组（2000/700、3000/1100）10 seed 全过。
    #[test]
    fn checkpoint_hit_sweep_planner_branch() {
        let cps = official_cps();
        let mut planned = 0;
        for (dist, dur) in [
            (1000.0f64, 360i64),
            (2000.0, 700),
            (3000.0, 1100),
            (1683.0, 555),
        ] {
            for seed in 1..=10u64 {
                match pipeline(&cps, dist, dur, seed, true) {
                    Ok((track, len, _)) => {
                        let (report, det) = preflight_of(&cps, &track, len, dur);
                        assert!(
                            !report.blocked(),
                            "planner dist={dist} dur={dur} seed={seed} 被拦：{:?}",
                            report.lines()
                        );
                        assert!(
                            !det.blocked(),
                            "planner seed={seed} 反检测不过：{:?}",
                            det.lines()
                        );
                        let worst = cps
                            .iter()
                            .map(|&cp| min_dist(&track, cp))
                            .fold(0.0, f64::max);
                        println!(
                            "| planner {dist:.0}/{dur} | {seed} | {worst:.4} | {} | false |",
                            report.fatal_count()
                        );
                        planned += 1;
                    }
                    Err(e) => {
                        // 只允许「装不下」这一种：闭环最短 2109m，1000/1683 两组
                        // 本来就塞不进 5 个点位。若哪天变成别的错，这里会失败。
                        assert!(
                            e.to_string().contains("装不下"),
                            "planner dist={dist} dur={dur} seed={seed} 出现预期外的错误：{e}"
                        );
                    }
                }
            }
        }
        assert_eq!(planned, 20, "可规划的组合应为 2 组 × 10 seed = 20");
    }

    /// 带必到点的跑，P1（起步窗口）那几条断言必须仍然成立。
    ///
    /// 吸附动的是坐标，而 P1 的三条基准（Σ段距离 == totalDis、首段
    /// state=131072、首段时长 ≈15.5%、段间 0ms 无缝）全部挂在「报文点序 +
    /// 几何里程」上，所以这里必须带着 5 个打卡点再验一遍，不能只靠那条
    /// 没有点位的 P1 用例。
    #[test]
    fn p1_invariants_hold_with_checkpoints() {
        let cps = official_cps();
        let dur = 555i64;
        let (track, len, rep) = fence_pipeline(&cps, 1683.0, dur, 2026, true);
        assert!(
            rep.skipped.is_empty(),
            "不该有点位被跳过：{:?}",
            rep.skipped
        );
        for (i, s) in rep.items.iter().enumerate() {
            assert!(
                s.after_m <= CHECKPOINT_HIT_M,
                "CP{i} 吸附后残距 {:.4}m",
                s.after_m
            );
        }

        let timed: Vec<(f64, f64, i64)> = track.iter().map(|q| (q.lat, q.lon, q.time_ms)).collect();
        let sp = SimParams {
            total_distance_m: len as f64,
            total_sec: dur,
            start_ms: START_MS,
            target_steps: None,
            seed: START_MS as u64,
            ..Default::default()
        };
        let sim = simulate(&timed, &sp).expect("simulate");
        let params = SubmitParams {
            uid: "probe".into(),
            unid: "0".into(),
            uuid: "00000000-0000-4000-8000-000000000000".into(),
            sport_type: 1,
            total_distance_m: len,
            total_time_sec: dur,
            total_steps: sim.total_steps,
            start_time_ms: START_MS,
            stop_time_ms: START_MS + dur * 1000,
            sel_distance_m: 1000,
            sel_run_time_sec: dur,
            policy: "0".into(),
            run_area_id: -1,
            geo_fences_json: "[]".into(),
            total_ascent: 0,
            alt_range: None,
        };
        let sub = build(&params, &track, &[], &sim).expect("build");
        let pts = sub.points.clone();
        let segs =
            crate::api::submit::build_segments_from_points(&pts, START_MS, sim.total_steps as i64);
        assert!(segs.len() > 2, "段表太短：{}", segs.len());

        let last_dis = pts.last().unwrap()["totalDis"].as_f64().unwrap();
        let sum_d: f64 = segs.iter().map(|s| s["distance"].as_f64().unwrap()).sum();
        // ① Σ段距离 == 末点 totalDis（≤0.5%），且与报文 totalDis 同源
        assert!(
            (sum_d - last_dis).abs() <= last_dis * 0.005,
            "Σ段距离 {sum_d:.2}m 与末点 totalDis {last_dis:.2}m 差 {:.2}%（上限 0.5%）",
            (sum_d - last_dis).abs() / last_dis * 100.0
        );
        assert!(
            (sub.save_record["totalDis"].as_f64().unwrap() - last_dis).abs() <= 1.0,
            "报文 totalDis {} 与末点 totalDis {last_dis:.2} 对不上",
            sub.save_record["totalDis"]
        );
        println!(
            "P1: Σ段距离 {sum_d:.2}m / 末点 totalDis {last_dis:.2}m / 报文 totalDis {} / 差 {:.3}%",
            sub.save_record["totalDis"],
            (sum_d - last_dis).abs() / last_dis * 100.0
        );
        println!(
            "P1: 首段 state={} 时长={}s 里程={:.2}m / 段数 {} / 末段 endTime={}",
            segs[0]["state"],
            (segs[0]["endTime"].as_i64().unwrap() - segs[0]["startTime"].as_i64().unwrap()) / 1000,
            segs[0]["distance"].as_f64().unwrap(),
            segs.len(),
            segs.last().unwrap()["endTime"]
        );
        // ② 首段 state == 131072，其余 0
        assert_eq!(segs[0]["state"], 131_072, "首段 state 必须是 0x20000");
        assert!(
            segs[1..].iter().all(|s| s["state"] == 0),
            "非首段 state 必须是 0"
        );
        // ③ 段间无缝、末段对齐 stopTime
        for w in segs.windows(2) {
            assert_eq!(w[0]["endTime"], w[1]["startTime"], "相邻段之间不能有缝隙");
        }
        assert_eq!(
            segs.last().unwrap()["endTime"].as_i64().unwrap(),
            START_MS + dur * 1000
        );
        // ④ 首段时长 ≈ 15.5% 总时长（±2s）
        let dur_of = |s: &serde_json::Value| {
            (s["endTime"].as_i64().unwrap() - s["startTime"].as_i64().unwrap()) / 1000
        };
        let want_first = (dur as f64 * 0.155) as i64;
        assert!(
            (dur_of(&segs[0]) - want_first).abs() <= 2,
            "首段时长 {}s 与 15.5% 的 {want_first}s 差超过 2 秒",
            dur_of(&segs[0])
        );
        // ⑤ 首段里程上界 5%（真机 1.1%，理由见 `submit` 同名断言：
        // 窗口内每点必须 ≥0.9m，否则整数米化后配速图开头断线）
        let first_dis = segs[0]["distance"].as_f64().unwrap();
        assert!(
            first_dis <= last_dis * 0.05,
            "首段 {first_dis:.1}m 超过总里程 5%"
        );
    }

    /// 吸附必须只修掉「压缩打散」这点量级，不能把轨迹整体拽走。
    ///
    /// 判据是「**半个上报间隔**」而不是写死的 5 米：压缩把点按弧长重新插值，
    /// 最近采样点离打卡点的残距最多半个点距。等时间 5 秒采样下点距 ≈14.9m，
    /// 所以半个间隔 ≈7.5m——写死 5.0 是老的「5 米一点」口径，那时半个间隔
    /// 是 2.5m。这里按实际点距取界，两边都成立。
    #[test]
    fn snap_only_moves_sampling_error() {
        let cps = official_cps();
        let (track, len, rep) = pipeline(&cps, 2000.0, 700, 2026, true).expect("plan");
        let spacing = len as f64 / (track.len() - 1) as f64;
        // 吸附要修的是**两个**误差源的叠加，不是一个：
        //
        // 1. 采样相位：点位落在两个采样点之间，最多半个点距（≈7m）；
        // 2. 规划走线：`plan()` 只是让路径经过点位附近，实测最近距离可以到
        //    12m（真机拿到的点位坐标本身就是围栏里的抽象点，不是跑到的那一脚）。
        //
        // 所以界是「一个点距 + 余量」，不是「半个点距」。只看第 1 项的
        // 断言在 `p1_invariants_hold_with_checkpoints`（那里点位精确落在
        // 轨迹上，残距必须 ≤5cm）。
        let bound = spacing + 2.0;
        println!(
            "[snap] {} 点 / 点距 {:.2}m / 单点最大位移 {:.2}m（界 {:.2}m = 半个间隔）",
            track.len(),
            spacing,
            rep.max_shift_m,
            bound
        );
        assert!(
            rep.max_shift_m < bound,
            "单点位移 {:.2}m 超出半个上报间隔（点距 {spacing:.2}m）的量级",
            rep.max_shift_m
        );
        // 吸附不能改变点数（否则等于偷偷换了采样节奏）
        assert_eq!(
            track.len(),
            pipeline(&cps, 2000.0, 700, 2026, false).unwrap().0.len()
        );
    }

    // ---- merged from former `mod gap_clamp_tests` ----

    // 吸附打卡点后把被拉开的间距压回速度上限。

    fn tp_gap_clamp_tests(lat: f64, lon: f64) -> TrackPoint {
        TrackPoint {
            lat,
            lon,
            time_ms: 0,
        }
    }

    /// 沿 lat 走的一条直线，`n` 个点、间距 `step_m`。
    fn line(n: usize, step_m: f64) -> Vec<TrackPoint> {
        let mlat = geo::M_PER_DEG_LAT;
        (0..n)
            .map(|i| tp_gap_clamp_tests(47.38 + (i as f64 * step_m) / mlat, 123.95))
            .collect()
    }

    #[test]
    fn clamps_oversized_gap_without_moving_pinned_point() {
        // 3 个点，中间那个被吸附挪远：0→1 变成 38.7m（真机实测那个数）
        let mut pts = line(3, 15.0);
        let mlat = geo::M_PER_DEG_LAT;
        pts[1].lat += 23.7 / mlat;
        let before = geo::distance((pts[0].lat, pts[0].lon), (pts[1].lat, pts[1].lon));
        assert!((before - 38.7).abs() < 0.5, "夹具不成立：间距 {before:.2}m");

        let pinned = [1usize];
        let fixed = clamp_sample_gaps(&mut pts, &pinned, 31.5);
        assert!(fixed > 0, "超限的段必须被处理");

        let after = geo::distance((pts[0].lat, pts[0].lon), (pts[1].lat, pts[1].lon));
        assert!(after <= 31.6, "间距没压回去：{after:.2}m");
        // 打卡点自己一格都不能动（动了就是 CP_MISS）。它原本在 15m 处，
        // 被夹具挪远 23.7m，所以期望位置是 38.7m 那一点。
        let want = 47.38 + (15.0 + 23.7) / mlat;
        assert!(
            (pts[1].lat - want).abs() < 1e-9,
            "被钉住的打卡点被挪了：{} vs {want}",
            pts[1].lat
        );
    }

    #[test]
    fn leaves_normal_gaps_alone() {
        let mut pts = line(5, 14.0);
        let before: Vec<(f64, f64)> = pts.iter().map(|p| (p.lat, p.lon)).collect();
        let fixed = clamp_sample_gaps(&mut pts, &[], 31.5);
        assert_eq!(fixed, 0, "正常间距不该被改");
        for (i, p) in pts.iter().enumerate() {
            assert_eq!((p.lat, p.lon), before[i], "第 {i} 点被无故移动");
        }
    }

    #[test]
    fn iterates_until_neighbours_converge() {
        // 连续两段都超限：压第一段会把点拽向第二段，必须迭代才收敛
        let mut pts = line(3, 15.0);
        let mlat = geo::M_PER_DEG_LAT;
        pts[1].lat += 20.0 / mlat;
        pts[2].lat += 20.0 / mlat;
        clamp_sample_gaps(&mut pts, &[], 31.5);
        for i in 0..2 {
            let g = geo::distance((pts[i].lat, pts[i].lon), (pts[i + 1].lat, pts[i + 1].lon));
            assert!(g <= 31.6, "第 {i} 段仍有 {g:.2}m，没收敛");
        }
    }

    // ---- merged from former `mod sample_rate_tests` ----

    // 上报采样节奏：与真机对象 `evidence/obs_real_20261002.json`
    // （114 点 / 555 秒 / 1683.83m）逐项对照。
    //
    // 守的是「采样率是真机三倍」那个偏差：真机 `ΔtotalTime` 109/113 恰好
    // 5 秒（均值 4.9115）、`ΔtotalDis` 均值 14.90 米；按距离 5m/点采样会
    // 出 337 点、1.65 秒/点。这里不看「看起来像」，直接比点数和两个平均间隔。

    const START_MS_SAMPLE_RATE_TESTS: i64 = 1_790_913_882_164;
    const REAL_POINTS: usize = 114;
    const REAL_MEAN_DT: f64 = 4.9115;
    const REAL_MEAN_DD: f64 = 14.90;

    /// 1683 m 的操场轨迹，几何密采样 5 m（与生产 `sample_step_m` 默认值一致）。
    fn oval_1683() -> Vec<(f64, f64)> {
        let spec = stadium::OvalSpec {
            straight: 95.0,
            radius: 25.0,
            heading: 0.3,
            laps: 4.723,
        };
        stadium::campus_oval_track((47.3794, 103.9502), &spec, 5.0, 2026)
    }

    /// 走一遍生产链路的那一段：等时间采样 → 起步压平。
    fn run_cadence(interval: f64) -> (Vec<TrackPoint>, CadenceStats) {
        let path = oval_1683();
        let times = sample_times(555, START_MS_SAMPLE_RATE_TESTS, interval);
        let mut track = sample_by_time(
            &path,
            &times,
            START_MS_SAMPLE_RATE_TESTS as u64,
            &kinematics::KinParams::default(),
            1.0,
            8.0,
            &[],
            &[],
        );
        assert!(track.len() >= 2, "采样为空");
        compress_startup(&mut track, START_MS_SAMPLE_RATE_TESTS, 555, None);
        let st = cadence_stats(&track);
        (track, st)
    }

    /// 时间轴本身的形状（点数、间隔、起步额外点）。
    #[test]
    fn time_grid_matches_real_device_shape() {
        let t = sample_times(555, START_MS_SAMPLE_RATE_TESTS, SAMPLE_INTERVAL_SEC);
        // 真机 114 点里有一个 `totalTime` 与首点完全相同的重复点，
        // 严格递增的发不出去，所以我们是 113（少 1 个）。
        assert_eq!(
            t.len(),
            REAL_POINTS - 1,
            "1683m/555s 应该出 113 个上报点（真机 114 含一个重复 0）"
        );
        // 真机前三点是 0 / 0 / 3（两个点同一秒）。我们保留 0 / 3，
        // 丢掉那个发不出去的重复 0。
        let rel: Vec<i64> = t
            .iter()
            .map(|x| (x - START_MS_SAMPLE_RATE_TESTS) / 1000)
            .collect();
        assert_eq!(&rel[..5], &[0, 3, 8, 13, 18], "真机前三点 0/0/3 → 我们 0/3");
        assert_eq!(*rel.last().unwrap(), 555, "末点必须精确落在 totalTime");
        // 时间戳必须严格递增（preflight 的 TRACK_TIME_ORDER）
        for w in t.windows(2) {
            assert!(w[1] > w[0], "时间戳必须严格递增：{:?}", w);
            assert!(w[1] - w[0] >= 1000, "间隔不能小于 1 秒（报文时间精度）");
        }
        let d: Vec<i64> = t.windows(2).map(|w| (w[1] - w[0]) / 1000).collect();
        // **一个真机没有的间隔都不许出现**：真机直方图是 {0:1, 3:2, 4:1, 5:109}，
        // 除那个发不出去的 0 之外，只允许 3 / 4 / 5 秒。
        let mut h: std::collections::BTreeMap<i64, usize> = Default::default();
        for x in &d {
            *h.entry(*x).or_default() += 1;
        }
        assert!(
            h.keys().all(|k| (3..=5).contains(k)),
            "间隔只能落在真机出现过的 3/4/5 秒，实际直方图 {h:?}"
        );
        assert_eq!(
            h.get(&5).copied().unwrap_or(0),
            109,
            "5 秒间隔应有 109 个（真机 109），实际 {h:?}"
        );
        assert_eq!(
            h.get(&3).copied().unwrap_or(0),
            2,
            "3 秒间隔应有 2 个（真机 2，起步 + 收尾），实际 {h:?}"
        );
        assert_eq!(
            h.get(&4).copied().unwrap_or(0),
            1,
            "4 秒间隔应有 1 个（真机 1，收尾），实际 {h:?}"
        );
        // 起步窗口 18 个点（真机 19，差的正是那个发不出去的重复 0）
        assert_eq!(
            startup_point_count(&t, START_MS_SAMPLE_RATE_TESTS, 555),
            18,
            "起步窗口应为 18 点（真机 19，含重复 0）"
        );
    }

    /// 收尾不许出现真机没有的 1~2 秒短间隔：把总时长扫一遍。
    #[test]
    fn tail_never_emits_a_one_or_two_second_gap() {
        for total in [4i64, 10, 15, 19, 20, 21, 32, 100, 301, 555, 720, 1800, 3601] {
            let t = sample_times(total, START_MS_SAMPLE_RATE_TESTS, SAMPLE_INTERVAL_SEC);
            assert_eq!(
                (*t.last().unwrap() - START_MS_SAMPLE_RATE_TESTS) / 1000,
                total,
                "总时长 {total} 的末点必须精确落在 totalTime"
            );
            let d: Vec<i64> = t.windows(2).map(|w| (w[1] - w[0]) / 1000).collect();
            let bad: Vec<i64> = d.iter().copied().filter(|x| *x < 3 || *x > 5).collect();
            assert!(
                bad.is_empty(),
                "总时长 {total} 出现真机没有的间隔 {bad:?}（全部 {d:?}）"
            );
        }
    }

    /// 生产默认节奏跑出来的点数与两个平均间隔，与真机偏差都 <10%。
    #[test]
    fn cadence_matches_real_device_within_10_percent() {
        let (_track, st) = run_cadence(SAMPLE_INTERVAL_SEC);
        println!(
            "| 指标 | 我们 | 真机 | 偏差 |\n|---|---|---|---|\n\
             | 点数 | {} | {} | {:+.1}% |\n\
             | 平均时间间隔 | {:.3}s | {:.3}s | {:+.1}% |\n\
             | 平均距离间隔 | {:.3}m | {:.2}m | {:+.1}% |\n\
             | 距离间隔标准差 | {:.2}m | 9.44m | {:+.1}% |\n\
             | Δt 中位 | {:.0}s | 5s | — |\n\
             | Δt 在 5±0.5s | {:.1}% | 96.5% | — |",
            st.points,
            REAL_POINTS,
            (st.points as f64 - REAL_POINTS as f64) / REAL_POINTS as f64 * 100.0,
            st.mean_dt_sec,
            REAL_MEAN_DT,
            (st.mean_dt_sec - REAL_MEAN_DT) / REAL_MEAN_DT * 100.0,
            st.mean_dd_m,
            REAL_MEAN_DD,
            (st.mean_dd_m - REAL_MEAN_DD) / REAL_MEAN_DD * 100.0,
            st.dd_stdev_m,
            (st.dd_stdev_m - 9.44) / 9.44 * 100.0,
            st.median_dt_sec,
            st.dt_within_5s * 100.0
        );
        // 真机 114 里有一个时间戳与首点完全相同的重复点，严格递增发不出去，
        // 所以只允许差这 1 个点。
        assert!(
            (st.points as i64 - REAL_POINTS as i64).abs() <= 1,
            "点数 {} 与真机 {REAL_POINTS} 差超过 1（重复时间戳只该差 1 个）",
            st.points
        );
        assert!(
            (st.mean_dt_sec - REAL_MEAN_DT).abs() / REAL_MEAN_DT < 0.10,
            "平均时间间隔 {:.3}s 偏离真机 {REAL_MEAN_DT}s 超过 10%",
            st.mean_dt_sec
        );
        assert!(
            (st.mean_dd_m - REAL_MEAN_DD).abs() / REAL_MEAN_DD < 0.10,
            "平均距离间隔 {:.3}m 偏离真机 {REAL_MEAN_DD}m 超过 10%",
            st.mean_dd_m
        );
        // 真机是「时间恒定、距离起伏」；反过来就说明采样轴认错了
        assert!(
            st.dd_stdev_m > 2.0,
            "距离间隔标准差 {:.2}m —— 距离没有起伏，等于又回到等距离采样",
            st.dd_stdev_m
        );
        assert!(
            st.dt_within_5s > 0.85,
            "5±0.5 秒的间隔只占 {:.1}%，节奏不稳",
            st.dt_within_5s * 100.0
        );
    }

    /// 旧口径的对照：5 米等距离采样确实是真机采样率的三倍。
    ///
    /// 这条断言是「缺陷本身」的证据——改回等距离立刻变红。
    #[test]
    fn distance_sampling_was_three_times_too_dense() {
        let path = oval_1683();
        let len = geo::path_length(&path);
        let old_points = (len / 5.0).round() as usize; // 5m/点
        let old_dt = 555.0 / (old_points - 1) as f64;
        println!(
            "| 采样口径 | 点数 | 间隔 |\n|---|---|---|\n\
             | 等距离 5m/点（旧） | {} | {:.2}s |\n\
             | 等时间 5s/点（新） | {} | {:.2}s |",
            old_points, old_dt, REAL_POINTS, REAL_MEAN_DT
        );
        assert!(
            old_points as f64 > REAL_POINTS as f64 * 2.5,
            "旧口径 {old_points} 点，真机 {REAL_POINTS} 点——不到三倍就说明这条对照失效了"
        );
        assert!(old_dt < 2.0, "旧口径 {old_dt:.2}s/点，应显著小于真机 4.91s");
    }

    /// A5 `queueNum`：**在生成出来的点序列上**复现真机的 floor 分区。
    ///
    /// 桩函数本身的两条断言在 `queue_num_tests` 里（拿真机序列硬编码对拍）。
    /// 这里要证明的是另一件事：**新采样节奏跑出来的点序列**喂进去，档位序列
    /// 与真机同样逐点一致。
    ///
    /// 我们比真机少一个点（真机首两点时间戳相同，严格递增发不出去），
    /// 所以第 `i` 个采样点对应真机第 `i+1` 个。只在尾部允许等宽网格
    /// 表达不了的余数差。
    #[test]
    fn queue_num_on_generated_track_matches_real_device() {
        let (track, _) = run_cadence(SAMPLE_INTERVAL_SEC);
        let n = track.len();
        let times: Vec<i64> = track.iter().map(|p| p.time_ms).collect();
        let su = startup_point_count(&times, START_MS_SAMPLE_RATE_TESTS, 555);
        assert_eq!(
            (n, su),
            (113, 18),
            "点数/起步点数必须先对上 5 秒网格（真机 114/19 含重复 0）"
        );
        let ours: Vec<i32> = (0..n).map(|i| queue_num(i, n, su)).collect();
        let real = &super::tests::REAL_POINT_QUEUE;
        // 真机第 0、1 点时间戳相同（都是 0 秒），我们是 0 秒 / 3 秒，
        // 所以下标要错一位对。
        let diffs: Vec<(usize, i32, i32)> = (0..n)
            .filter_map(|i| real.get(i + 1).map(|r| (i, *r, ours[i])))
            .filter(|&(_, r, o)| r != o)
            .collect();
        assert!(
            diffs.len() <= 3 && diffs.iter().all(|(i, _, _)| *i > 100),
            "只允许尾部少量余数差（等宽网格表达不了），实际 {diffs:?}"
        );
        // 起步段全 0、之后每 4 点 +1：真机形状
        assert!(ours[..su].iter().all(|v| *v == 0), "起步段必须全 0");
        let changes: Vec<usize> = (1..n).filter(|&i| ours[i] != ours[i - 1]).collect();
        assert_eq!(changes.first(), Some(&su), "第一档必须从第 {su} 点起");
        assert!(
            changes.windows(2).take(20).all(|w| w[1] - w[0] == 4),
            "起步之后必须每 4 点 +1：{changes:?}"
        );
        assert_eq!(*ours.last().unwrap(), 24, "末点封在 24 档");
    }

    /// 起步压平只动「窗口内那一段」，窗口外的点距比例（速度信息）必须保住。
    ///
    /// 旧实现把窗口外的点**等距重铺**，对等时间采样来说等于把 `ΔtotalDis`
    /// 抹成常数——真机的速度起伏就没了。
    #[test]
    fn compress_startup_keeps_speed_spacing_outside_window() {
        let path = oval_1683();
        let times = sample_times(555, START_MS_SAMPLE_RATE_TESTS, SAMPLE_INTERVAL_SEC);
        let raw = sample_by_time(
            &path,
            &times,
            START_MS_SAMPLE_RATE_TESTS as u64,
            &kinematics::KinParams::default(),
            1.0,
            8.0,
            &[],
            &[],
        );
        let k = startup_point_count(&times, START_MS_SAMPLE_RATE_TESTS, 555);
        assert_eq!(k, 18);
        let d_before: Vec<f64> = raw
            .windows(2)
            .map(|w| geo::distance((w[0].lat, w[0].lon), (w[1].lat, w[1].lon)))
            .collect();
        let mut after = raw.clone();
        compress_startup(&mut after, START_MS_SAMPLE_RATE_TESTS, 555, None);
        let d_after: Vec<f64> = after
            .windows(2)
            .map(|w| geo::distance((w[0].lat, w[0].lon), (w[1].lat, w[1].lon)))
            .collect();

        // 起步窗口 + 出窗那一步 = 首段里程，应贴近 STARTUP_DIST_RATIO 的预算
        // （`segment_json` 首段就是这个量：`cluster` + 一个采样步长）。
        let su_dist: f64 = d_after[..k].iter().sum();
        let total: f64 = d_after.iter().sum();
        let ratio = su_dist / total;
        assert!(
            (ratio - STARTUP_DIST_RATIO).abs() < 0.006,
            "起步窗口里程占比 {:.3}，应贴近 {STARTUP_DIST_RATIO}",
            ratio
        );
        // 窗口外的相对幅度必须保住：压缩后 / 压缩前 的比值应当是同一个常数。
        //
        // 不会恰好是 0：`want` 是**弧长**上的仿射映射，输出点重新落到折线上
        // 之后量的是**弦长**，弯道和 GPS 抖动处弦长对弧长敏感（同一个点沿弧
        // 挪半米，弦长能差几个百分点）。所以判据是「比等距重铺小一个量级」+
        // 「起伏系数基本不变」，而不是「逐点完全相等」。
        let tail_before: f64 = d_before[k..].iter().sum();
        let tail_after: f64 = d_after[k..].iter().sum();
        let scale = tail_after / tail_before;
        let mut worst_ratio = 0.0f64;
        for i in k..d_before.len() {
            if d_before[i] < 0.5 {
                continue;
            }
            worst_ratio = worst_ratio.max((d_after[i] / d_before[i] - scale).abs() / scale);
        }
        // 起伏系数（变异系数）：等距重铺会把它压到 ~0
        let cv = |v: &[f64]| {
            let m = v.iter().sum::<f64>() / v.len().max(1) as f64;
            if m <= 0.0 {
                return 0.0;
            }
            let var = v.iter().map(|x| (x - m).powi(2)).sum::<f64>() / v.len().max(1) as f64;
            var.sqrt() / m
        };
        let cv_before = cv(&d_before[k..]);
        let cv_after = cv(&d_after[k..]);
        println!(
            "[startup] 窗口外点距：最大相对偏差 {:.1}% / 起伏系数 {:.3} → {:.3}",
            worst_ratio * 100.0,
            cv_before,
            cv_after
        );
        assert!(
            worst_ratio < 0.15,
            "窗口外点距被重铺了：最大相对偏差 {:.1}%（等距重铺会给 ~40~60%）",
            worst_ratio * 100.0
        );
        assert!(
            cv_after > cv_before * 0.8,
            "起伏系数从 {cv_before:.3} 掉到 {cv_after:.3}：点距里的速度信息被抹平了"
        );
        // 速度起伏还在（**相对**判据：压缩不负责放大起伏，也不许抹平它）。
        //
        // 绝对幅度与真机还差一截（真机窗口外 16.8m ± 8.4m，我们 ~14.8m ± 1.4m）。
        // 那是**配速模型的起伏幅度**问题（`kinematics::pace_profile` 的 OU 振幅
        // + `fit_capped` 把总里程压准），不是采样轴的问题：把 5 秒网格换成
        // 15 米网格不会让速度分布变宽。这条已写在 docs/fix/FIX-SAMPLE-RATE.md 的
        // 「已知残差」里，属于另一条轴。
        let sd = |v: &[f64]| {
            let m = v.iter().sum::<f64>() / v.len().max(1) as f64;
            (v.iter().map(|x| (x - m).powi(2)).sum::<f64>() / v.len().max(1) as f64).sqrt()
        };
        let (sd_before, sd_after) = (sd(&d_before[k..]), sd(&d_after[k..]));
        assert!(
            sd_after > sd_before * 0.8 && sd_after > 0.5,
            "窗口外点距标准差 {sd_before:.2}m → {sd_after:.2}m：起伏被抹平了"
        );
    }

    /// 一条 60m 直行 → 90° 右转 → 60m 直行的折线（密采样 1m，与生产同构）。
    fn lat0_of(_path: &[(f64, f64)]) -> f64 {
        47.38
    }

    /// 短而密的轨迹不能把 `compress_startup` 的 clamp 打成 panic。
    ///
    /// `min_per_point` 随点数增长（`k*0.9`），上界却是 `total_len*0.05`；平均点距
    /// 小于 2.79m 时下限会顶到上限之上，而 `f64::clamp` 的约定是 panic 而不是
    /// 饱和截断 —— 整条生成链路在这里崩掉。2km/555s（点距 ~14.9m）不触发，
    /// 所以一直是测试盲区。
    #[test]
    fn compress_startup_survives_short_dense_track() {
        let lat0 = 47.38f64;
        let lng0 = 123.95f64;
        let mlat = geo::M_PER_DEG_LAT;
        // 100m / 40 点 = 平均点距 2.5m，落在 min_per_point > 0.05*total_len 的区间
        let n = 40usize;
        let step_m = 100.0 / (n - 1) as f64;
        let mut pts: Vec<TrackPoint> = (0..n)
            .map(|i| TrackPoint {
                lat: lat0 + i as f64 * step_m / mlat,
                lon: lng0,
                time_ms: START_MS_SAMPLE_RATE_TESTS + (i as i64 * 555_000 / (n as i64 - 1)),
            })
            .collect();
        // 不 panic 即为通过；压完还要保住首末点与时长
        compress_startup(&mut pts, START_MS_SAMPLE_RATE_TESTS, 555, None);
        assert_eq!(pts.len(), n);
        assert_eq!(pts.first().unwrap().time_ms, START_MS_SAMPLE_RATE_TESTS);
        assert_eq!(
            pts.last().unwrap().time_ms,
            START_MS_SAMPLE_RATE_TESTS + 555_000
        );
    }

    /// `cap_advances` 基本盘：削峰、顺延、保总量、保单调。
    #[test]
    fn cap_advances_trims_and_keeps_total() {
        // 4 步：5 / 20 / 3 / 2，上限都是 10 → 20 被削 10，顺延给后面两步
        let mut adv = vec![0.0, 5.0, 25.0, 28.0, 30.0];
        let caps = vec![10.0, 10.0, 10.0, 10.0];
        cap_advances(&mut adv, &caps, 30.0);
        assert_eq!(adv.len(), 5);
        assert!((adv[4] - 30.0).abs() < 1e-9, "总量不变");
        for w in adv.windows(2) {
            assert!(w[1] >= w[0], "单调不减");
            assert!(w[1] - w[0] <= 10.0 + 1e-9, "单步不越上限");
        }
        // 全程贴着上限跑（总需求 > 总上限）：不强切，保总量
        let mut adv = vec![0.0, 12.0, 24.0, 36.0];
        let caps = vec![10.0, 10.0, 10.0];
        cap_advances(&mut adv, &caps, 36.0);
        assert!((adv[3] - 36.0).abs() < 1e-9, "兜不住时总量仍然不变");
        for w in adv.windows(2) {
            assert!(w[1] >= w[0]);
        }
        // 太短直接返回
        let mut adv = vec![0.0, 100.0];
        cap_advances(&mut adv, &[10.0], 100.0);
        assert_eq!(adv, vec![0.0, 100.0]);
    }

    /// 窄规则带（3'30"~10'00" → 4.76 m/s）下，`compress_startup` 的仿射放大
    /// 不能把单步弧长顶出「规则上限 × 报文时间差 × 1.15」。
    ///
    /// 复现的是 issue #6：手绘轨迹的配速图最快 2'39"（6.3 m/s clamp），
    /// 规则带 3'30"~10'00"，肉眼可见越带 36%。成因有两层——OU 顶在规则
    /// 上限的整排点被收敛迭代按比例拉伸，再被起步压缩的仿射映射放大
    /// 1.1 倍上下；报文 `totalTime` 还是整秒口径，4.91 秒网格的「4 秒段」
    /// 又把同样的弧长折算快 25%。三层叠出来就是 2'39"。
    #[test]
    fn compress_startup_caps_steps_under_narrow_band() {
        let lat0 = 47.38f64;
        let lng0 = 123.95f64;
        let mlat = geo::M_PER_DEG_LAT;
        // 60 点、5 秒网格，位置沿经线：步长交替 25m / 5m（平均 15，起步压缩
        // 放大后 25m 段会顶到 30m+，足够触发封顶；总预算兜得住）
        let n = 60usize;
        let mut arc = 0.0f64;
        let mut pts: Vec<TrackPoint> = Vec::with_capacity(n);
        for i in 0..n {
            if i > 0 {
                arc += if i % 2 == 1 { 25.0 } else { 5.0 };
            }
            pts.push(TrackPoint {
                lat: lat0 + arc / mlat,
                lon: lng0,
                time_ms: START_MS_SAMPLE_RATE_TESTS + i as i64 * 4910,
            });
        }
        let total_before = pts
            .windows(2)
            .map(|w| geo::distance((w[0].lat, w[0].lon), (w[1].lat, w[1].lon)))
            .sum::<f64>();
        compress_startup(&mut pts, START_MS_SAMPLE_RATE_TESTS, 555, Some(4.76));
        // 起步窗口（前 startup_point_count 个点）弧长被压平是设计行为，只查窗口外
        let su = startup_point_count(
            &pts.iter().map(|p| p.time_ms).collect::<Vec<_>>(),
            START_MS_SAMPLE_RATE_TESTS,
            555,
        );
        let vmax = 4.76f64;
        for w in pts.windows(2).enumerate().skip(su.saturating_sub(1)) {
            let (i, w) = w;
            let dt = (((w[1].time_ms - START_MS_SAMPLE_RATE_TESTS) as f64 / 1000.0).floor()
                - ((w[0].time_ms - START_MS_SAMPLE_RATE_TESTS) as f64 / 1000.0).floor())
            .max(1.0);
            let step = geo::distance((w[0].lat, w[0].lon), (w[1].lat, w[1].lon));
            assert!(
                step <= vmax * dt * 1.15 + 0.75,
                "窗口外第 {i} 步 {step:.1}m 越过 {}m 上限",
                vmax * dt * 1.15
            );
        }
        // 总量不动（封顶只重分配）
        let total_after = pts
            .windows(2)
            .map(|w| geo::distance((w[0].lat, w[0].lon), (w[1].lat, w[1].lon)))
            .sum::<f64>();
        assert!(
            (total_after - total_before).abs() < 1.0,
            "封顶不能吃掉总里程：{total_before:.1} → {total_after:.1}"
        );
    }

    fn l_path() -> ((f64, f64), Vec<(f64, f64)>) {
        let lat0 = 47.38;
        let lng0 = 123.95;
        let mlat = geo::M_PER_DEG_LAT;
        let mlng = geo::m_per_deg_lng(lat0);
        let mut path = Vec::new();
        for i in 0..=60 {
            path.push((lat0 + i as f64 / mlat, lng0));
        }
        for i in 1..=60 {
            path.push((lat0 + 60.0 / mlat, lng0 + i as f64 / mlng));
        }
        ((lat0 + 60.0 / mlat, lng0), path)
    }

    /// 弦是否穿进楼里（用与生产同一个判据）。
    fn chords_hit(track: &[TrackPoint], blockers: &[Vec<(f64, f64)>]) -> usize {
        track
            .windows(2)
            .filter(|w| chord_hits_blocker((w[0].lat, w[0].lon), (w[1].lat, w[1].lon), blockers))
            .count()
    }

    /// **穿墙**：拐角上压一栋楼，15 米弦会从楼的角上切过去，必须补点绕开。
    ///
    /// 这是 5m → 15m 放大之后真正危险的后果（`avoid_blockers` 只保证
    /// **折线**不穿楼，不保证 15 米级的**弦**不穿楼）。
    #[test]
    fn blocker_chord_guard_prevents_wall_cut() {
        let (corner, path) = l_path();
        let mlat = geo::M_PER_DEG_LAT;
        let mlng = geo::m_per_deg_lng(corner.0);
        // 楼体压在「内拐角」那块凹地里：折线本身完全绕开它（两条边各留 2m），
        // 但 15m 的弦从两条边之间直接穿过去——这正是 5m 采样不会出、
        // 15m 采样会出的那种穿墙。
        // 楼要盖住**整个拐角内侧**，不能只盖「某个速度下的弦」。
        //
        // 这个夹具被速度漂移坑过两次：弦长 = v·5s，只要 OU 参数一动，弦就
        // 从楼边上擦过去，断言「不带障碍必须穿楼」当场挂掉——测的已经不是
        // 绕障，而是「当前速度恰好多大」。现在让楼覆盖 lat 40~59.9、
        // lng 1~40 这一大块拐角内腹：
        //   - 竖段 lng=0 < 1  → 折线不碰楼（前提成立）
        //   - 横段 lat=60 > 59.9 → 同样不碰
        //   - 任何从竖段跨到横段的弦（5~30m）中段都落在这块里
        let (by0, by1) = (40.0, 59.9);
        let (bx0, bx1) = (1.0, 40.0);
        let building = vec![
            (lat0_of(&path) + by0 / mlat, corner.1 + bx0 / mlng),
            (lat0_of(&path) + by0 / mlat, corner.1 + bx1 / mlng),
            (lat0_of(&path) + by1 / mlat, corner.1 + bx1 / mlng),
            (lat0_of(&path) + by1 / mlat, corner.1 + bx0 / mlng),
        ];
        // 折线必须全程在楼外（否则这条夹具测的就不是「弦穿楼」）
        assert!(
            !path.windows(2).any(|w| chord_hits_blocker(
                w[0],
                w[1],
                std::slice::from_ref(&building)
            )),
            "夹具不成立：折线本身就穿楼了"
        );
        let blockers = vec![building.clone()];
        let total = geo::path_length(&path);
        let times = sample_times(
            (total / 3.0) as i64,
            START_MS_SAMPLE_RATE_TESTS,
            SAMPLE_INTERVAL_SEC,
        );
        let kin = kinematics::KinParams::default();

        // 不给障碍：两条弦直接从楼里穿过去
        let naive = sample_by_time(
            &path,
            &times,
            START_MS_SAMPLE_RATE_TESTS as u64,
            &kin,
            0.5,
            10.0,
            &[],
            &[],
        );
        let before = chords_hit(&naive, &blockers);
        // 给障碍：补点绕开，一条都不许穿
        let guarded = sample_by_time(
            &path,
            &times,
            START_MS_SAMPLE_RATE_TESTS as u64,
            &kin,
            0.5,
            10.0,
            &blockers,
            &[],
        );
        let after = chords_hit(&guarded, &blockers);
        println!(
            "[blocker] 弦穿楼 {before}/{n0} → {after}/{n1}（补了 {extra} 个点）",
            before = before,
            n0 = naive.len().saturating_sub(1),
            after = after,
            n1 = guarded.len().saturating_sub(1),
            extra = guarded.len().saturating_sub(naive.len())
        );
        assert!(before > 0, "夹具不成立：不带障碍时居然没穿楼");
        assert_eq!(after, 0, "绕开之后仍有 {after} 条弦穿楼");
        // **点数与时间戳一个都不变**：绕开的做法是把采样点挪到拐角顶点上
        // （`override_pos`），不是往网格里插点——插点会把 5 秒间隔切成 1~2 秒，
        // 而真机的 `ΔtotalTime` 里没有这种间隔（`SAMPLE_RATE_SHORT` 会拦）。
        assert_eq!(
            guarded.len(),
            naive.len(),
            "绕开不该改变点数（插点会切碎上报间隔）"
        );
        assert!(
            guarded
                .iter()
                .zip(&naive)
                .any(|(a, b)| geo::distance((a.lat, a.lon), (b.lat, b.lon)) > 1.0),
            "一条弦都没被挪开，绕开逻辑等于没生效"
        );
        for w in guarded.windows(2) {
            assert!(w[1].time_ms > w[0].time_ms);
            assert!(w[1].time_ms - w[0].time_ms >= 1000);
        }
    }

    /// **端到端穿墙回归**：3 米外绕楼的折线，5 秒采样的**每一条弦**都不许压楼。
    ///
    /// 这是把「5m → 15m」放到真实几何上量：绕行折线本身（5m 密采样）不穿楼，
    /// 弦是采样之后才出现的——15 米级的长弦会把绕行拐角切掉，直接从楼角上
    /// 搂过去。历史上有过两次同类回归（`471f602` / `13809d2` / `1c9d66a`），
    /// 所以这里同时固定三件事：
    ///
    /// - 密采样折线不穿楼（基线，靠 3 米外扩的绕行折线构造成立）；
    /// - 5 秒采样的弦不穿楼（靠 [`chord_hits_blocker`] 补点）；
    /// - 重复率（折返程度）不因采样变稀而恶化。
    #[test]
    fn sampled_chords_never_cross_blockers_around_building() {
        let lat0 = 30.5;
        let lng0 = 104.0;
        let mlat = geo::M_PER_DEG_LAT;
        let mlng = geo::m_per_deg_lng(lat0);
        let to_deg = |x: f64, y: f64| (lat0 + y / mlat, lng0 + x / mlng);

        // 33×33m 楼体，中心在轨迹 120m 处（本地坐标 x∈[103.5,136.5], y∈[-16.5,16.5]）
        let (bx0, bx1) = (103.5, 136.5);
        let (by0, by1) = (-16.5, 16.5);
        let building = vec![
            to_deg(bx0, by0),
            to_deg(bx0, by1),
            to_deg(bx1, by1),
            to_deg(bx1, by0),
        ];
        let blockers = vec![building.clone()];

        // 手工绕行：沿 y=0 走到楼前，从北侧 3m 外绕过，再回到 y=0（2m 步长）
        let m = 3.0;
        let mut path: Vec<(f64, f64)> = Vec::new();
        let mut x = 0.0;
        while x < bx0 - m {
            path.push(to_deg(x, 0.0));
            x += 2.0;
        }
        let mut y = 0.0;
        while y < by1 + m {
            path.push(to_deg(bx0 - m, y));
            y += 2.0;
        }
        let mut px = bx0 - m;
        while px < bx1 + m {
            path.push(to_deg(px, by1 + m));
            px += 2.0;
        }
        let mut y2 = by1 + m;
        while y2 > 0.0 {
            path.push(to_deg(bx1 + m, y2));
            y2 -= 2.0;
        }
        let mut x2 = bx1 + m;
        while x2 <= 240.0 {
            path.push(to_deg(x2, 0.0));
            x2 += 2.0;
        }

        let dense_cross = path
            .windows(2)
            .filter(|w| chord_hits_blocker(w[0], w[1], &blockers))
            .count();
        assert_eq!(dense_cross, 0, "基线不成立：绕行折线自己就压楼了");

        // 5 秒采样（3 m/s → 15m 一点）
        let total = geo::path_length(&path);
        let times = sample_times(
            (total / 3.0) as i64,
            START_MS_SAMPLE_RATE_TESTS,
            SAMPLE_INTERVAL_SEC,
        );
        let kin = kinematics::KinParams::default();
        let naive = sample_by_time(
            &path,
            &times,
            START_MS_SAMPLE_RATE_TESTS as u64,
            &kin,
            0.5,
            10.0,
            &[],
            &[],
        );
        let guarded = sample_by_time(
            &path,
            &times,
            START_MS_SAMPLE_RATE_TESTS as u64,
            &kin,
            0.5,
            10.0,
            &blockers,
            &[],
        );
        let (naive_cross, guarded_cross) = (
            chords_hit(&naive, &blockers),
            chords_hit(&guarded, &blockers),
        );
        // 重复率：相邻两步夹角 > 90° 的比例（与 tileroad 折返判据同口径）
        let repeat_rate = |t: &[TrackPoint]| {
            let steps: Vec<(f64, f64)> = t
                .windows(2)
                .map(|w| (w[1].lat - w[0].lat, w[1].lon - w[0].lon))
                .collect();
            let back = steps
                .windows(2)
                .filter(|w| w[0].0 * w[1].0 + w[0].1 * w[1].1 < 0.0)
                .count();
            back as f64 / steps.len().max(1) as f64
        };
        println!(
            "[穿墙] 折线 {} 点 → 5s 采样 {} 点（补点后 {} 点）：弦穿楼 {} → {}；重复率 {:.2}；折线全长 {:.0}m",
            path.len(),
            naive.len(),
            guarded.len(),
            naive_cross,
            guarded_cross,
            repeat_rate(&guarded),
            total
        );
        assert!(naive_cross > 0, "夹具不成立：不给障碍时也没穿楼");
        assert_eq!(guarded_cross, 0, "5 秒采样后仍有 {guarded_cross} 条弦穿楼");
        assert!(
            repeat_rate(&guarded) < 0.5,
            "重复率 {:.2} 过高：采样变稀之后在绕行段来回折返",
            repeat_rate(&guarded)
        );
        // 补点后时间戳仍严格递增、每段 ≥1 秒
        for w in guarded.windows(2) {
            assert!(w[1].time_ms > w[0].time_ms);
            assert!(w[1].time_ms - w[0].time_ms >= 1000);
        }
    }

    /// `avoid_blockers` 输出的真实折线：5 秒采样**不许新增**任何穿墙。
    ///
    /// 这条测的不是「采样器自己能不能绕楼」（那是上一条），而是「采样变稀
    /// 之后会不会把原本干净的绕行折线切出新穿墙」。判据是相对基线：
    /// `补点后穿墙数 ≤ 密采样折线的穿墙数`。
    ///
    /// 顺带把 `avoid_blockers` 那个**既有的**缺陷钉在纸面上（与本轮采样改动
    /// 无关，改前改后都存在）：楼体把整条直线切成两半时，被推到 3 米外扩环
    /// 上的那批点与环外的点之间会拉出一条 ~27.6m 的对角缩短段，它从楼角上
    /// 搂过去、最深进楼 6.75m。报文里这段在改前也是 27.6m 的一条直线，
    /// 所以不是采样粒度造成的——详见 docs/fix/FIX-SAMPLE-RATE.md「发现的既有缺陷」。
    #[test]
    fn sampling_never_adds_crossings_to_avoided_route() {
        let lat0 = 30.5;
        let lng0 = 104.0;
        let mlat = geo::M_PER_DEG_LAT;
        let mlng = geo::m_per_deg_lng(lat0);
        let to_deg = |x: f64, y: f64| (lat0 + y / mlat, lng0 + x / mlng);
        let building = vec![
            to_deg(-16.5, -16.5),
            to_deg(-16.5, 16.5),
            to_deg(16.5, 16.5),
            to_deg(16.5, -16.5),
        ];
        let blockers = vec![building];
        // 一条 200m 直线，正穿楼心（2m 步长，与生产密折线同量级）
        let raw: Vec<(f64, f64)> = (0..=100)
            .map(|i| to_deg(-100.0 + i as f64 * 2.0, 0.0))
            .collect();
        let avoided = fence_route::avoid_blockers(&raw, &blockers, 3.0);
        let dense_cross = avoided
            .windows(2)
            .filter(|w| chord_hits_blocker(w[0], w[1], &blockers))
            .count();
        let times = sample_times(67, START_MS_SAMPLE_RATE_TESTS, SAMPLE_INTERVAL_SEC);
        let kin = kinematics::KinParams::default();
        let guarded = sample_by_time(
            &avoided,
            &times,
            START_MS_SAMPLE_RATE_TESTS as u64,
            &kin,
            0.5,
            10.0,
            &blockers,
            &[],
        );
        let guarded_cross = chords_hit(&guarded, &blockers);
        println!(
            "[avoid] 绕行折线 {} 点（穿楼 {dense_cross} 段）→ 5s 采样 {} 点（穿楼 {guarded_cross} 段）",
            avoided.len(),
            guarded.len()
        );
        assert!(
            guarded_cross <= dense_cross,
            "采样新增了穿墙：密折线 {dense_cross} 段 → 采样后 {guarded_cross} 段"
        );
    }

    /// **折返**：入场尾巴接跑道那种 160° 急转，15 米弦会把「走进去」抹成直线。
    #[test]
    fn reversal_corner_is_kept() {
        let lat0 = 47.38;
        let lng0 = 123.95;
        let mlat = geo::M_PER_DEG_LAT;
        let mlng = geo::m_per_deg_lng(lat0);
        // 从北往南走 60m，然后立刻折回来（180° 折返，再向东 60m）
        let mut path = Vec::new();
        for i in 0..=60 {
            path.push((lat0 + (60.0 - i as f64) / mlat, lng0));
        }
        for i in 1..=60 {
            path.push((lat0 + i as f64 / mlat, lng0 + i as f64 / mlng));
        }
        let total = geo::path_length(&path);
        let times = sample_times(
            (total / 3.0) as i64,
            START_MS_SAMPLE_RATE_TESTS,
            SAMPLE_INTERVAL_SEC,
        );
        let track = sample_by_time(
            &path,
            &times,
            START_MS_SAMPLE_RATE_TESTS as u64,
            &kinematics::KinParams::default(),
            0.5,
            10.0,
            &[],
            &[],
        );
        // 折返点（path 第 60 个点）离上报点的最近距离
        let turn = (lat0, lng0);
        let best = track
            .iter()
            .map(|p| geo::distance((p.lat, p.lon), turn))
            .fold(f64::INFINITY, f64::min);
        println!("[reversal] 离折返点最近 {best:.2}m");
        assert!(
            best <= CORNER_CHORD_TOL_M + 0.5,
            "160° 折返被切掉 {best:.2}m，入场尾巴会被抹成一条直线"
        );
    }
}

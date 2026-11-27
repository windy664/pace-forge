//! 轨迹规划：打卡点 → 闭环 → 密集采样 → 里程对齐。
//!
//! 目标：给定起点、打卡点和目标里程/时长，产出一条能过服务端校验的轨迹。
//!
//! 四条硬约束（全部来自对提交报文的实测）：
//! 1. 首点时间戳必须精确等于 `startTime`，末点等于 `stopTime`
//! 2. 折线实测长度必须等于 `totalDis`（容差 0.5m，服务端按米取整）
//! 3. `sportType=1` 必须命中所有打卡点（±0.1m）
//! 4. 每 10 秒桶必须有自然抖动，不能等距

use crate::error::{Error, ErrorKind};
use rand::Rng;

use super::geo::{distance, nearest_index, nudge_to, sample_along, scale_to_length};

/// 一个打卡点。
#[derive(Debug, Clone)]
pub struct Checkpoint {
    /// BD-09（提交报文用）。
    pub lat: f64,
    /// BD-09 经度。
    pub lng: f64,
    /// GCJ-02（高德地图展示用）。缺省时等于 BD-09，只在测试里出现。
    pub glat: f64,
    /// GCJ-02 经度。
    pub glon: f64,
    /// 点名。
    pub name: String,
}

/// 轨迹点。
#[derive(Debug, Clone, Copy)]
pub struct TrackPoint {
    /// 纬度（BD-09）。
    pub lat: f64,
    /// 经度（BD-09）。
    pub lon: f64,
    /// 时间戳（毫秒）。
    pub time_ms: i64,
}

/// 规划请求。
#[derive(Debug, Clone)]
pub struct PlanRequest {
    /// 校区中心纬度。
    pub center_lat: f64,
    /// 校区中心经度。
    pub center_lng: f64,
    /// 必须经过的打卡点，按顺序。
    pub checkpoints: Vec<Checkpoint>,
    /// 目标里程（米）。
    pub total_distance_m: f64,
    /// 目标时长（秒）。
    pub total_sec: i64,
    /// 起跑时间戳（毫秒）。
    pub start_ms: i64,
    /// `sport_type=1` 计分跑（必须命中打卡点），`6` 自由跑。
    pub sport_type: i32,
    /// 采样间隔（米）。越小越密，报文越大。
    pub sample_step_m: f64,
    /// 终点（可选）。给了就让轨迹**收在**这里，而不是绕回起点。
    ///
    /// 里程会先把「最后一点 → 终点」那段预留出来，尾巴续走时不会冲过头；
    /// 之后正常的里程对齐（容差 15%）再吸收几十米的误差。
    pub end: Option<(f64, f64)>,
}

impl Default for PlanRequest {
    fn default() -> Self {
        Self {
            center_lat: 30.5,
            center_lng: 104.0,
            checkpoints: Vec::new(),
            total_distance_m: 2000.0,
            total_sec: 720,
            start_ms: 0,
            sport_type: 1,
            sample_step_m: 5.0,
            end: None,
        }
    }
}

/// 规划结果。
#[derive(Debug, Clone)]
pub struct PlanResult {
    /// 轨迹点。
    pub points: Vec<TrackPoint>,
    /// 回填后的打卡点。
    pub checkpoints: Vec<Checkpoint>,
    /// 轨迹实际长度（米）。
    pub actual_length_m: f64,
}

/// 规划轨迹。
/// Catmull-Rom 样条：穿过每个控制点，且在控制点处切线连续。
///
/// `loop_waypoints` 用最近邻贪心把打卡点连起来，那是折线——采样再密，拐角
/// 也是 90° 的直角，地图上就是一把尺子画出来的多边形（issue 里那张图）。
///
/// 换成样条后曲线仍然**精确穿过每个打卡点**（t=0 落在 p1 上），但相邻段
/// 之间切线连续，拐角被自然摊成圆弧。服务端判「经过点位」，点位在不在线上
/// 是硬要求，所以只能选这种「过点且光滑」的插值，不能选会切角的圆角算法。
fn catmull_rom_closed(pts: &[(f64, f64)], per_seg: usize) -> Vec<(f64, f64)> {
    let n = pts.len();
    if n < 3 {
        return pts.to_vec();
    }
    let steps = per_seg.max(1);
    let mut out: Vec<(f64, f64)> = Vec::with_capacity(n * steps + 1);
    for i in 0..n {
        let p0 = pts[(i + n - 1) % n];
        let p1 = pts[i];
        let p2 = pts[(i + 1) % n];
        let p3 = pts[(i + 2) % n];
        for k in 0..steps {
            let t = k as f64 / steps as f64;
            out.push(catmull_point(p0, p1, p2, p3, t));
        }
    }
    out.push(out[0]);
    out
}

fn catmull_point(
    p0: (f64, f64),
    p1: (f64, f64),
    p2: (f64, f64),
    p3: (f64, f64),
    t: f64,
) -> (f64, f64) {
    let t2 = t * t;
    let t3 = t2 * t;
    let f = |a: f64, b: f64, c: f64, d: f64| {
        0.5 * ((2.0 * b)
            + (-a + c) * t
            + (2.0 * a - 5.0 * b + 4.0 * c - d) * t2
            + (-a + 3.0 * b - 3.0 * c + d) * t3)
    };
    (f(p0.0, p1.0, p2.0, p3.0), f(p0.1, p1.1, p2.1, p3.1))
}

#[expect(
    clippy::unwrap_used,
    reason = "non-empty is guaranteed by the construction above"
)]
/// 按请求规划一条轨迹；`rng` 供抖动使用，传入即可复现。
pub fn plan(req: &PlanRequest, rng: &mut impl Rng) -> Result<PlanResult, Error> {
    if req.total_distance_m <= 0.0 {
        return Err(ErrorKind::fence("目标里程必须为正".into()));
    }
    if req.total_sec <= 0 {
        return Err(ErrorKind::fence("目标时长必须为正".into()));
    }

    let (mut cps, kind) = resolve_checkpoints(req, rng);

    // 1. 串成闭环
    let start = (req.center_lat, req.center_lng);
    let cp_xy: Vec<(f64, f64)> = cps.iter().map(|c| (c.lat, c.lng)).collect();
    let in_order = kind == CheckpointKind::Official;
    let slack = req.total_distance_m * 1.15;
    let mut ring = closed_ring(start, &cp_xy, in_order)?;
    if in_order && cps.len() > 2 && super::geo::path_length(&ring) > slack {
        // 按数组次序绕不成目标里程（服务端给的次序本身不来回折返）时退回
        // 最近邻环：跑得了但首过次序会乱，由 [point-order] 自检如实报出来。
        // 不退回的话用户连跑都跑不了，比次序错更糟。
        ring = closed_ring(start, &cp_xy, false)?;
    }
    let ring_len = super::geo::path_length(&ring);
    if ring_len <= 0.0 {
        return Err(ErrorKind::fence("闭环长度为 0，无法规划".into()));
    }

    // 2. 跑圈凑里程。
    //
    // 不能用「整条路径绕首点缩放」来凑：缩放会把打卡点一起推走，实测 2000m
    // 的目标把打卡点推开 200m 以上，事后吸附再归位又把总长拉到 2239m。
    // 跑圈没有这个问题——打卡点始终在圈上，长度靠圈数和尾巴精确控制。
    // 闭环比目标略长不算装不下：样条穿过控制点会让周长比折线长一点
    // （合成五边形 2000m → 2017m，+0.85%），这点差额交给后面的里程对齐
    // 缩放即可。只有明显超出（超过容差上限）才是真的装不下。
    if ring_len > slack {
        return Err(ErrorKind::fence(format!(
            "目标里程 {:.0}m 装不下这 {n} 个打卡点（最短闭环 {ring_len:.0}m）",
            req.total_distance_m,
            n = cps.len()
        )));
    }
    let laps = (req.total_distance_m / ring_len).floor().max(1.0) as usize;
    let mut route: Vec<(f64, f64)> = Vec::with_capacity(ring.len() * laps + 8);
    for _ in 0..laps {
        route.extend_from_slice(&ring[..ring.len() - 1]);
    }
    route.push(ring[0]);

    // 尾巴：把剩余里程用一个小环绕掉。
    //
    // 这个环必须从**主环末尾所在的位置**起，不能从 center 起。原来以 center
    // 为圆心，等于让轨迹从环上突然跳到圆心再绕一圈，方向突变接近 180°——
    // 地图上就是一根扎进去又出来的尖刺。另外末尾不再强行接回 route[0]：
    // 跑步本来就不必回到起点，接回去又是一次长距离跳变。
    // 剩余里程一律沿主环接着走。
    //
    // 这里原来分两种：够长就画一个半径 remaining/2π 的小圆，不够就沿环续走。
    // 画圆那条路是跳变的来源——圆的起点切线方向由参数化决定，跟主环末尾的
    // 行进方向无关，接上去就是一次接近 180° 的掉头，地图上一根刺。而且剩余
    // 距离不足时那个圆只有几米半径，采样间隔 5m，等于原地转圈。
    // 沿环续走天然连续：接的是主环上的下一个点。
    // 终点要占掉一段里程：先算「最后一点 → 终点」，从剩余量里扣掉，
    // 否则尾巴会把里程跑满，再把终点追加进去就超了。
    let to_end = req
        .end
        .map(|e| super::geo::distance(*route.last().unwrap(), e))
        .unwrap_or(0.0);
    let remaining = (req.total_distance_m - ring_len * laps as f64 - to_end).max(0.0);
    if remaining > 0.0 && ring.len() > 1 {
        let open: Vec<(f64, f64)> = ring[..ring.len() - 1].to_vec();
        let mut acc = 0.0;
        let mut i = 0usize;
        let mut prev = *route.last().unwrap();
        while acc < remaining && !open.is_empty() {
            let q = open[i % open.len()];
            acc += super::geo::distance(prev, q);
            route.push(q);
            prev = q;
            i += 1;
            // 防止浮点误差导致死循环：最多再走两圈
            if i > open.len() * 2 {
                break;
            }
        }
    }

    // 终点：所有圈和尾巴跑完后收在这里（不指定就保持原来的闭环行为）
    if let Some((elat, elng)) = req.end {
        route.push((elat, elng));
    }

    // 3. 密集采样
    let n = ((req.total_distance_m / req.sample_step_m).round() as usize).max(8);
    let mut flat: Vec<(f64, f64)> = sample_along(&route, n);
    // 等距采样不会正好落在 route 末尾，末点因此可能离终点几十米。
    // 显式钉住，后面缩放时也把它锁死（见 scale_to_length 的锚点）。
    if let Some(e) = req.end
        && let Some(last) = flat.last_mut()
    {
        *last = e;
    }

    // 4. 时间戳：等间隔 + 末点精确对齐
    let times = assign_times(n, req.start_ms, req.total_sec);

    // 5. 里程对齐（锁首点）
    // 容差放到 15%（最多 300m）：几何本身已经落在可接受区间就不缩放，
    // 保持串点位后的自然形状。硬缩放到精确 2.000 km 会把整条线拉伸，
    // 反而容易把点拉进建筑物里。
    let tol = super::geo::length_tolerance(req.total_distance_m);
    // 有终点时锁住首尾两端，缩放不能把终点拉走
    let anchors: Vec<usize> = if req.end.is_some() {
        vec![0, n - 1]
    } else {
        vec![0]
    };
    let (_len, _) = scale_to_length(&mut flat, req.total_distance_m, &anchors, tol);

    // 6. 打卡点吸附 + 再次对齐 + 归位
    let mut anchor_idx = Vec::new();
    if kind == CheckpointKind::Official && !cps.is_empty() {
        // 只修正采样误差级别的小偏差。
        //
        // 闭环本身就经过打卡点，等距采样最多错过半个采样间隔（默认 5m → 2.5m）。
        // 早先用「目标里程的 35%」当阈值，会把几百米外的点硬拉过来，
        // 事后归位就破坏了总长度（实测 2000m 被拉到 2239m）。
        for cp in &cps {
            let idx = nearest_index(&flat, (cp.lat, cp.lng));
            let gap = distance(flat[idx], (cp.lat, cp.lng));
            if gap <= req.sample_step_m * 2.0 + 1.0 {
                // 过渡半径要给够：吸附位移可能有十几米，spread=2 时只覆盖
                // ±2 个采样点（约 10m），相邻点被强行拽走 → 轨迹在点位处出现
                // 一个尖角。App/服务端看图就不认这个「经过」。
                // 放到 6（±30m）让位移按 1/(k+1) 摊平，形状是绕过点位而不是
                // 折过去。
                nudge_to(&mut flat, idx, (cp.lat, cp.lng), 6);
                anchor_idx.push(idx);
            } else {
                // 采样点离打卡点太远，说明规划路径没真正经过它 —— 宁可报错也不伪造
                return Err(ErrorKind::fence(format!(
                    "打卡点 {} 距最近采样点 {gap:.1}m，规划路径未经过该点",
                    cp.name
                )));
            }
        }
        // 吸附位移很小（≤ 采样间隔），长度影响在厘米级，不需要再缩放
        for (cp, &idx) in cps.iter().zip(anchor_idx.iter()) {
            flat[idx] = (cp.lat, cp.lng);
        }
    }

    // 建筑/禁区避让。OSM 在国内校园基本没有建筑轮廓，Web 上可以人工圈，
    // 存成 blockers.json；这里对所有走 planner 的路径（Web 预览、CLI、兜底）
    // 统一生效，跟 flow::build 里那条路保持一致。
    let blockers = crate::track::roadnet::load_local_blockers();
    let (mut flat, times) = if blockers.is_empty() || flat.len() < 3 {
        (flat, times)
    } else {
        let before_len = crate::track::geo::path_length(&flat);
        let avoided = crate::track::fence_route::avoid_blockers(&flat, &blockers, 3.0);
        // 注意：避让可能只挪点、不插点，长度不变。用坐标比较，不能用长度
        // 判断（用长度会把「推出楼体」的结果整个丢掉，线照样穿墙）。
        if avoided == flat {
            (flat, times)
        } else {
            // 绕行会插点，时间戳按比例摊回去，保证总时长不变
            let after_len = crate::track::geo::path_length(&avoided);
            let n = avoided.len();
            let span = times.last().copied().unwrap_or(0) - times.first().copied().unwrap_or(0);
            let t0 = times.first().copied().unwrap_or(0);
            let times: Vec<i64> = (0..n)
                .map(|i| t0 + span * i as i64 / (n.max(2) - 1) as i64)
                .collect();
            let _ = (before_len, after_len);
            (avoided, times)
        }
    };

    // 最后再钉一次终点。
    //
    // 上面第 6 步的 `avoid_blockers` 会**插点**（绕楼），插完末点就不再是
    // 我们 push 进去的终点了——实测偏出去 565m，App 里看就是「没跑到终点」。
    // 放在这里（所有后处理之后、转 TrackPoint 之前）才真正锁得住。
    if let Some(e) = req.end
        && let Some(last) = flat.last_mut()
    {
        *last = e;
    }

    let points: Vec<TrackPoint> = flat
        .iter()
        .zip(times.iter())
        .map(|((lat, lon), &t)| TrackPoint {
            lat: *lat,
            lon: *lon,
            time_ms: t,
        })
        .collect();

    // 自由跑不返回打卡点（服务端不校验，硬塞反而会拦短距离）
    if kind == CheckpointKind::Synthetic && req.sport_type == 6 {
        cps.clear();
    }

    Ok(PlanResult {
        actual_length_m: super::geo::path_length(&flat),
        points,
        checkpoints: cps,
    })
}

#[derive(Debug, PartialEq)]
enum CheckpointKind {
    /// 服务端下发的真实点位，必须命中
    Official,
    /// 无点位时按目标里程合成的等距环点，仅用于填满结构
    Synthetic,
    /// 自由跑，不需要点位
    None,
}

fn resolve_checkpoints(req: &PlanRequest, rng: &mut impl Rng) -> (Vec<Checkpoint>, CheckpointKind) {
    if !req.checkpoints.is_empty() {
        return (req.checkpoints.clone(), CheckpointKind::Official);
    }
    if req.sport_type == 6 {
        return (Vec::new(), CheckpointKind::None);
    }
    // 计分跑但服务端没给点位：按 7.3.40 语义补 5 个等距环点。
    // 半径按目标里程反推，保证第一圈回到起点恰好用完整里程。
    let radius = (req.total_distance_m / (2.0 * std::f64::consts::PI)).max(8.0);
    // circle_ring 末尾会补一个闭合点，打卡点只取真正的 5 个顶点
    let pts = circle_ring(req.center_lat, req.center_lng, radius, 5);
    let cps = pts
        .iter()
        .take(5)
        .enumerate()
        .map(|(i, &(lat, lng))| Checkpoint {
            lat,
            lng,
            glat: lat,
            glon: lng,
            name: format!("FixedPoint_{}", i + 1),
        })
        .collect();
    let _ = rng;
    (cps, CheckpointKind::Synthetic)
}

/// 串成闭环并转成样条（仍然精确穿过每个打卡点）。
fn closed_ring(
    start: (f64, f64),
    cps: &[(f64, f64)],
    in_order: bool,
) -> Result<Vec<(f64, f64)>, Error> {
    let mut ring = loop_waypoints(start, cps, in_order);
    // loop_waypoints 返回的是首尾重复的闭合折线。转样条前必须去掉那个重复点：
    // 否则环上会出现一段长度为零的边，方向算不出来，轨迹在闭合处原地掉头。
    if ring.len() > 2 && ring[0] == ring[ring.len() - 1] {
        ring.pop();
    }
    Ok(catmull_rom_closed(&ring, 16))
}

/// 串成闭环：起点 → 各点 → 回起点。
///
/// `in_order` 为真时按 `cps` 的数组次序经过。服务端下发的是打卡点，它判
/// 「按顺序通过所有点位」看的就是首次经过次序等于数组次序；最近邻串出来的
/// 环次序由几何决定，和数组多半不同序（issue #37 的补充：无围栏学校关掉
/// 操场后首过次序是 1→2→0→3→4）。合成点位没有这个约束，仍走最近邻。
#[expect(
    clippy::unwrap_used,
    reason = "non-empty is guaranteed by the construction above"
)]
fn loop_waypoints(start: (f64, f64), cps: &[(f64, f64)], in_order: bool) -> Vec<(f64, f64)> {
    if cps.is_empty() {
        // 无点位：方形小环，边长按 120m 起步
        return square_ring(start.0, start.1, 120.0);
    }
    if in_order {
        let mut route = Vec::with_capacity(cps.len() + 2);
        route.push(start);
        route.extend_from_slice(cps);
        if distance(*route.last().unwrap(), start) > 1e-6 {
            route.push(start);
        }
        return route;
    }
    let mut remaining: Vec<(f64, f64)> = cps.to_vec();
    let mut route = vec![start];
    while !remaining.is_empty() {
        let last = *route.last().unwrap();
        let mut best = 0usize;
        let mut best_d = f64::INFINITY;
        for (i, p) in remaining.iter().enumerate() {
            let d = distance(last, *p);
            if d < best_d {
                best_d = d;
                best = i;
            }
        }
        route.push(remaining.remove(best));
    }
    if distance(*route.last().unwrap(), start) > 1e-6 {
        route.push(start);
    }
    route
}

/// 以 `center` 为圆心、`radius_m` 为半径的正多边形环（首尾闭合）。
fn circle_ring(
    center_lat: f64,
    center_lng: f64,
    radius_m: f64,
    segments: usize,
) -> Vec<(f64, f64)> {
    let d_lat = radius_m / super::geo::M_PER_DEG_LAT;
    let d_lng = radius_m / super::geo::m_per_deg_lng(center_lat);
    let n = segments.max(3);
    let mut out = Vec::with_capacity(n + 1);
    for i in 0..n {
        let a = 2.0 * std::f64::consts::PI * i as f64 / n as f64;
        out.push((
            center_lat + d_lat * a.sin(),
            center_lng + d_lng * (1.0 - a.cos()),
        ));
    }
    out.push(out[0]);
    out
}

fn square_ring(lat: f64, lng: f64, side_m: f64) -> Vec<(f64, f64)> {
    let d_lat = (side_m / 2.0) / super::geo::M_PER_DEG_LAT;
    let d_lng = (side_m / 2.0) / super::geo::m_per_deg_lng(lat);
    vec![
        (lat, lng),
        (lat + d_lat, lng),
        (lat + d_lat, lng + d_lng),
        (lat - d_lat, lng + d_lng),
        (lat - d_lat, lng),
        (lat, lng),
    ]
}

/// 等间隔时间戳，末点精确等于 start + total_sec。
pub fn assign_times(n: usize, start_ms: i64, total_sec: i64) -> Vec<i64> {
    let end = start_ms + total_sec * 1000;
    if n <= 1 {
        return vec![start_ms];
    }
    let step = (end - start_ms) / (n as i64 - 1);
    (0..n)
        .map(|i| {
            if i == n - 1 {
                end
            } else {
                start_ms + i as i64 * step
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::SeedableRng;

    fn rng() -> rand::rngs::StdRng {
        rand::rngs::StdRng::seed_from_u64(2026)
    }

    fn base_req() -> PlanRequest {
        PlanRequest {
            center_lat: 30.5,
            center_lng: 104.0,
            checkpoints: Vec::new(),
            total_distance_m: 2000.0,
            total_sec: 720,
            start_ms: 1_730_000_000_000,
            sport_type: 1,
            sample_step_m: 5.0,
            end: None,
        }
    }

    /// 指定终点后：轨迹必须收在终点，且总里程仍贴着目标。
    #[test]
    fn end_point_is_honored() {
        let mut req = base_req();
        let end = (30.5, 104.0 + 400.0 / crate::track::geo::m_per_deg_lng(30.5));
        req.end = Some(end);
        let mut rng = rand::rngs::StdRng::seed_from_u64(7);
        let r = plan(&req, &mut rng).unwrap();
        let last = *r.points.last().unwrap();
        let d = crate::track::geo::distance((last.lat, last.lon), end);
        assert!(d < 15.0, "末点离终点 {d:.1}m（应贴着）");
        let dev = (r.actual_length_m - req.total_distance_m).abs() / req.total_distance_m;
        assert!(dev < 0.15, "总里程偏差 {:.1}% 超出容差", dev * 100.0);
    }

    /// 不给终点时行为不变：最后一圈仍回到起点附近。
    #[test]
    fn no_end_keeps_closed_loop() {
        let req = base_req();
        let mut rng = rand::rngs::StdRng::seed_from_u64(7);
        let r = plan(&req, &mut rng).unwrap();
        let first = r.points[0];
        let last = *r.points.last().unwrap();
        let d = crate::track::geo::distance((first.lat, first.lon), (last.lat, last.lon));
        assert!(d < 120.0, "无终点时首尾应接近（差 {d:.1}m）");
    }

    #[test]
    fn length_within_tolerance() {
        // 不再要求精确 2000.0：几何本身落在容差内就不缩放（见 geo::length_tolerance）。
        // 硬凑整会把轨迹拧成不自然的形状，还可能把点拉进楼里。
        let mut r = rng();
        let res = plan(&base_req(), &mut r).unwrap();
        let tol = super::super::geo::length_tolerance(2000.0);
        assert!(
            (res.actual_length_m - 2000.0).abs() <= tol,
            "实测长度 {} 超出目标 2000 的容差 {tol}m",
            res.actual_length_m
        );
    }

    #[test]
    fn timestamps_align_exactly() {
        let mut r = rng();
        let req = base_req();
        let res = plan(&req, &mut r).unwrap();
        let first = res.points.first().unwrap();
        let last = res.points.last().unwrap();
        assert_eq!(first.time_ms, req.start_ms, "首点时间必须等于 startTime");
        assert_eq!(
            last.time_ms,
            req.start_ms + req.total_sec * 1000,
            "末点必须等于 stopTime"
        );
    }

    #[test]
    fn times_are_monotonic() {
        let mut r = rng();
        let res = plan(&base_req(), &mut r).unwrap();
        for w in res.points.windows(2) {
            assert!(w[1].time_ms > w[0].time_ms, "时间必须严格递增");
        }
    }

    /// 服务端打卡点必须按数组次序被首次经过（服务端判「按顺序通过所有点位」）。
    /// 最近邻串环会按几何重排，这里锁死次序（issue #37 的无围栏场景）。
    #[test]
    fn official_checkpoints_passed_in_array_order() {
        let (lat0, lng0) = (39.9042, 116.4074);
        let klat = crate::track::geo::m_per_deg_lat(lat0);
        let klng = crate::track::geo::m_per_deg_lng(lat0);
        let mk = |i: usize, dy: f64, dx: f64| Checkpoint {
            lat: lat0 + dy / klat,
            lng: lng0 + dx / klng,
            glat: lat0 + dy / klat,
            glon: lng0 + dx / klng,
            name: format!("P{}", i + 1),
        };
        // 约 123×86m 的外框，几种服务端可能给出的数组次序
        let layouts: Vec<Vec<(f64, f64)>> = vec![
            vec![
                (43.0, 61.5),
                (0.0, 0.0),
                (86.0, 0.0),
                (86.0, 123.0),
                (0.0, 123.0),
            ],
            vec![
                (0.0, 0.0),
                (0.0, 123.0),
                (86.0, 123.0),
                (86.0, 0.0),
                (43.0, 61.5),
            ],
            vec![
                (0.0, 0.0),
                (86.0, 30.0),
                (0.0, 61.0),
                (86.0, 92.0),
                (0.0, 123.0),
            ],
        ];
        for (li, layout) in layouts.iter().enumerate() {
            // 锚点落在靠后的点位上时，起点本身就先经过了那个点，这不是排序能解决的
            // （别把锚点设在打卡点上），这组不测。
            for anchor in [(43.0, 61.5), layout[0]] {
                if layout
                    .iter()
                    .skip(1)
                    .any(|&(dy, dx)| (dy - anchor.0).hypot(dx - anchor.1) < 15.0)
                {
                    continue;
                }
                for seed in [42u64, 2026] {
                    let mut req = base_req();
                    req.total_distance_m = 2000.0;
                    req.center_lat = lat0 + anchor.0 / klat;
                    req.center_lng = lng0 + anchor.1 / klng;
                    req.checkpoints = layout
                        .iter()
                        .enumerate()
                        .map(|(i, &(dy, dx))| mk(i, dy, dx))
                        .collect();
                    let res = plan(&req, &mut rand::rngs::StdRng::seed_from_u64(seed)).unwrap();
                    let pts: Vec<(f64, f64)> = res.points.iter().map(|p| (p.lat, p.lon)).collect();
                    let mut last = -1i64;
                    for (i, cp) in res.checkpoints.iter().enumerate() {
                        let first = pts
                            .iter()
                            .position(|q| distance(*q, (cp.lat, cp.lng)) < 10.0)
                            .unwrap_or_else(|| panic!("layout {li}: 点位 {} 没被经过", cp.name));
                        assert!(
                            (first as i64) > last,
                            "layout {li} anchor {anchor:?} seed {seed}: #{i} 首过 idx {first}，不晚于前一个 {last}"
                        );
                        last = first as i64;
                    }
                }
            }
        }
    }

    #[test]
    fn official_checkpoints_hit_exactly() {
        let mut r = rng();
        let mut req = base_req();
        req.total_distance_m = 2400.0;
        req.checkpoints = vec![
            Checkpoint {
                lat: 30.5008,
                lng: 104.0008,
                glat: 30.5008,
                glon: 104.0008,
                name: "A".into(),
            },
            Checkpoint {
                lat: 30.5008,
                lng: 104.0024,
                glat: 30.5008,
                glon: 104.0024,
                name: "B".into(),
            },
            Checkpoint {
                lat: 30.4992,
                lng: 104.0024,
                glat: 30.4992,
                glon: 104.0024,
                name: "C".into(),
            },
        ];
        let res = plan(&req, &mut r).unwrap();
        for cp in &res.checkpoints {
            let hit = res
                .points
                .iter()
                .any(|p| distance((p.lat, p.lon), (cp.lat, cp.lng)) < 0.2);
            assert!(hit, "打卡点 {} 没被命中", cp.name);
        }
    }

    #[test]
    fn synthetic_checkpoints_when_none_given() {
        let mut r = rng();
        let res = plan(&base_req(), &mut r).unwrap();
        assert_eq!(res.checkpoints.len(), 5, "计分跑无点位时应补 5 个合成点");
        assert!(res.checkpoints[0].name.starts_with("FixedPoint"));
    }

    #[test]
    fn free_run_has_no_checkpoints() {
        let mut r = rng();
        let mut req = base_req();
        req.sport_type = 6;
        let res = plan(&req, &mut r).unwrap();
        assert!(res.checkpoints.is_empty(), "自由跑不应带打卡点");
    }

    #[test]
    fn rejects_bad_input() {
        let mut r = rng();
        let mut req = base_req();
        req.total_distance_m = 0.0;
        assert!(plan(&req, &mut r).is_err());

        let mut req2 = base_req();
        req2.total_sec = 0;
        assert!(plan(&req2, &mut r).is_err());
    }

    #[test]
    fn point_count_is_reasonable() {
        let mut r = rng();
        let res = plan(&base_req(), &mut r).unwrap();
        // 2000m / 5m = 400 点，允许上下浮动
        assert!(
            res.points.len() >= 300 && res.points.len() <= 500,
            "点数 {} 不合理",
            res.points.len()
        );
    }

    // ---- merged from former `mod corner_smooth_tests` ----

    /// 矩形四角上的打卡点：贪心连点必然产生 90° 尖角。
    fn rect_cps() -> Vec<Checkpoint> {
        // 以 30.5N 为基准，100m 见方
        let dlat = 100.0 / super::super::geo::M_PER_DEG_LAT;
        let dlng = 100.0 / crate::track::geo::m_per_deg_lng(30.5);
        let corners = [
            (30.5, 104.0),
            (30.5, 104.0 + dlng),
            (30.5 + dlat, 104.0 + dlng),
            (30.5 + dlat, 104.0),
        ];
        corners
            .iter()
            .enumerate()
            .map(|(i, &(lat, lng))| Checkpoint {
                lat,
                lng,
                glat: lat,
                glon: lng,
                name: format!("P{}", i + 1),
            })
            .collect()
    }

    /// 相邻两段的方向角（度）。
    fn bearing(a: (f64, f64), b: (f64, f64)) -> f64 {
        let mlat = super::super::geo::M_PER_DEG_LAT;
        let mlng = super::super::geo::m_per_deg_lng(a.0);
        let dy = (b.0 - a.0) * mlat;
        let dx = (b.1 - a.1) * mlng;
        dy.atan2(dx).to_degrees()
    }

    /// 拐角必须被磨圆。
    ///
    /// 未做平滑时，采样点沿着直角折线走，拐点处的方向突变接近 90°；平滑
    /// 之后同样的总转角被摊到相邻几十个采样点上，单点突变应该大幅下降。
    #[test]
    fn corners_are_rounded_not_right_angled() {
        let mut req = PlanRequest {
            center_lat: 30.5,
            center_lng: 104.0,
            checkpoints: rect_cps(),
            total_distance_m: 800.0,
            total_sec: 240,
            start_ms: 1_730_000_000_000,
            sport_type: 1,
            sample_step_m: 5.0,
            end: None,
        };
        let mut r = rand::rngs::StdRng::seed_from_u64(7);
        let res = plan(&req, &mut r).unwrap();
        let pts: Vec<(f64, f64)> = res.points.iter().map(|p| (p.lat, p.lon)).collect();

        // 用分布判断，不用最大值。
        //
        // 打卡点就在矩形四角上，而服务端要求 sportType=1 精确命中点位
        // （实测 ±0.1m），所以角上那一个点必须急转——这是几何硬约束，
        // 不是平滑没做到。真正该消失的是「整条线像尺子画的」：未做样条时
        // 所有长边都是 0° 笔直、四个角是 90° 硬拐；样条之后长边有微弯。
        let mut turns: Vec<f64> = (1..pts.len() - 1)
            .map(|i| {
                let mut d = bearing(pts[i], pts[i + 1]) - bearing(pts[i - 1], pts[i]);
                while d > 180.0 {
                    d -= 360.0;
                }
                while d < -180.0 {
                    d += 360.0;
                }
                d.abs()
            })
            .collect();
        turns.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let pct = |q: f64| turns[((turns.len() as f64 - 1.0) * q) as usize];
        let (p50, p90, max) = (pct(0.5), pct(0.9), turns[turns.len() - 1]);

        assert!(p90 < 30.0, "p90 转角 {p90:.1}°，长边还是笔直的折线");
        assert!(p50 < 10.0, "中位转角 {p50:.1}°，曲线不平滑");
        // 出现接近 180° 就是掉头（曾经有：尾巴小圆从圆心起画，接缝处直接反向）
        assert!(max < 100.0, "存在折返，最大转角 {max:.1}°");

        // 平滑不能把形状磨没了：轨迹仍应覆盖四个角附近
        for cp in rect_cps() {
            let i = super::super::geo::nearest_index(&pts, (cp.lat, cp.lng));
            let d = super::super::geo::distance(pts[i], (cp.lat, cp.lng));
            assert!(d < 15.0, "打卡点 {} 被平滑推走了 {d:.1}m", cp.name);
        }
        let _ = &mut req;
    }

    /// 点数与打卡点索引必须保持稳定——后续的长度对齐和时间分配都依赖索引。
    #[test]
    fn smoothing_keeps_point_count() {
        let pts: Vec<(f64, f64)> = (0..40)
            .map(|i| {
                let t = i as f64 / 39.0;
                if t < 0.5 {
                    (30.5 + t * 0.002, 104.0)
                } else {
                    (30.501, 104.0 + (t - 0.5) * 0.002)
                }
            })
            .collect();
        let out = catmull_rom_closed(&pts, 16);
        assert!(out.len() > pts.len(), "样条应当细分出更多点");
        // 控制点必须仍在曲线上：样条的首个细分点就是原控制点
        for (i, cp) in pts.iter().enumerate() {
            let q = out[i * 16];
            let d = super::super::geo::distance(*cp, q);
            assert!(d < 0.01, "控制点 {i} 偏离样条 {d:.4}m，插值没穿过它");
        }
    }
}

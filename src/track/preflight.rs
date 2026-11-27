//! 提交前自检。
//!
//! 目标：**宁可本地失败，也不把注定被驳回的报文发出去**。
//!
//! 服务端驳回一次不只是失败——错误次数会被记录，异常次数多了会触发风控，
//! 后续正常提交也可能被拦。所以能在本地拦住的，绝不放到线上试。
//!
//! 分三级：
//! - `Fatal`：必然被驳回，直接阻断（里程对不上、时间戳错位、打卡点未命中）
//! - `Warn`：可能被驳回，需要人工确认（配速过快、步频异常）
//! - `Info`：仅提示（数据看起来不太自然，但合规）

use super::TrackPoint;
use super::biomech::gait;
use super::geo::distance;
use super::simulate::{SimParams, SimResult};

/// 问题等级。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    /// 仅提示
    Info,
    /// 可能被驳回
    Warn,
    /// 必然被驳回
    Fatal,
}

/// 一条检查结果。
#[derive(Debug, Clone)]
pub struct Issue {
    /// 严重程度。
    pub level: Level,
    /// 检查码。
    pub code: &'static str,
    /// 给用户看的说明。
    pub message: String,
}

impl Issue {
    fn fatal(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            level: Level::Fatal,
            code,
            message: message.into(),
        }
    }
    fn warn(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            level: Level::Warn,
            code,
            message: message.into(),
        }
    }
    fn info(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            level: Level::Info,
            code,
            message: message.into(),
        }
    }
}

/// 自检报告。
#[derive(Debug, Clone)]
pub struct Report {
    /// 全部检查结果。
    pub issues: Vec<Issue>,
}

impl Report {
    /// 有没有致命问题（有就不能提交）。
    pub fn blocked(&self) -> bool {
        self.issues.iter().any(|i| i.level == Level::Fatal)
    }

    /// 致命问题数（非零就不能提交）。
    pub fn fatal_count(&self) -> usize {
        self.issues
            .iter()
            .filter(|i| i.level == Level::Fatal)
            .count()
    }

    /// 警告数。
    pub fn warn_count(&self) -> usize {
        self.issues
            .iter()
            .filter(|i| i.level == Level::Warn)
            .count()
    }

    /// 一句话摘要。
    pub fn summary(&self) -> String {
        if self.issues.is_empty() {
            return "全部通过".into();
        }
        let f = self.fatal_count();
        let w = self.warn_count();
        let i = self.issues.len() - f - w;
        format!("{f} 致命 / {w} 警告 / {i} 提示")
    }

    /// 逐行文本，供日志或界面展示。
    pub fn lines(&self) -> Vec<String> {
        self.issues
            .iter()
            .map(|i| {
                let tag = match i.level {
                    Level::Fatal => "✗ 致命",
                    Level::Warn => "! 警告",
                    Level::Info => "· 提示",
                };
                format!("{tag} [{}] {}", i.code, i.message)
            })
            .collect()
    }
}

/// 自检输入。
pub struct Check<'a> {
    /// 轨迹点。
    pub points: &'a [TrackPoint],
    /// 合成的分桶与步数。
    pub sim: &'a SimResult,
    /// 合成参数。
    pub params: &'a SimParams,
    /// 必过的打卡点（纬度, 经度, 名称）。
    pub checkpoints: &'a [(f64, f64, String)],
    /// 报文里的总里程。
    pub total_distance_m: i64,
    /// 报文里的总时长。
    pub total_time_sec: i64,
    /// 报文里的总步数。
    pub total_steps: i32,
    /// 报文里的起止时间戳。
    pub start_time_ms: i64,
    /// 报文里的结束时间戳。
    pub stop_time_ms: i64,
    /// 服务端规则允许的配速区间（分/公里），`None` 表示不校验。
    pub pace_range: Option<(f64, f64)>,
    /// 服务端规则允许的步频区间（步/分钟），`None` 表示不校验。
    ///
    /// 原来这里硬编码 [100, 230]，而服务端实发 60~250。拿着一把更窄的
    /// 假尺子量自己，会误拦合法步频，也放过了真正越界的区间。
    pub step_range: Option<(i64, i64)>,
    /// 本次配置的上报间隔（秒），`None` 按真机的 [`SAMPLE_INTERVAL_SEC`] 算。
    ///
    /// 这是 `SAMPLE_RATE` 那条检查的**期望值**：采样器跑出来的中位间隔
    /// 必须贴着它。注意这里比的是「采样器有没有实现错」，不是「配置对不对」；
    /// 配置本身偏离真机只报警告（`SAMPLE_RATE_DEVICE`）。
    pub expected_interval_sec: Option<f64>,
}

/// 逐点配速越界比例的告警阈值。
///
/// 真机基线约 6%（GPS 抖动导致个别点瞬时配速离谱），所以阈值必须高于它，
/// 否则每一单都会命中，警告就没人看了。15% 意味着「大面积越界」才算问题。
pub const PACE_BODY_OUT_WARN: f64 = 0.15;

/// 10 秒桶的越界告警阈值。
///
/// **必须高于真机基线**：真机记录里约 25% 的桶会落在区间外（README「进区间
/// 比例 真机 25%」），因为起步窗口、停顿点、冲刺段的桶天然越界。定 10% 等于
/// 每一单都报警，警告就没人看了。40% 才是「明显不对劲」。
pub const BUCKET_OUT_WARN: f64 = 0.40;

/// 执行全部检查。
///
/// 不知道「服务器现在」时，`TIME_FUTURE` 这一项跳过——本机时钟自己跟自己比
/// 永远发现不了偏差，只有调用方把校时后的 `server_now` 传进来才查得出来。
pub fn inspect(c: &Check) -> Report {
    inspect_at(c, None)
}

/// 带服务器当前时刻的检查：多查一条「记录结束时间不得在服务器未来」。
///
/// 服务端硬判据是「开始时间 + 时长 ≤ 服务器当前时间」（`api/gmttime.rs` 模块
/// 文档有记载）。早先的检查只比对 stop 与 start、跨度与 totalTime，全是同一
/// 条时间轴内部的相对关系，`stopTime` 整条落到未来也查不出来。
pub fn inspect_at(c: &Check, now_ms: Option<i64>) -> Report {
    let mut issues = Vec::new();

    check_timeline(c, now_ms, &mut issues);

    if c.points.is_empty() {
        issues.push(Issue::fatal("TRACK_EMPTY", "轨迹为空"));
        return Report { issues };
    }

    check_point_times(c, &mut issues);

    check_distance_and_geo(c, &mut issues);

    check_checkpoints(c, &mut issues);

    check_buckets(c, &mut issues);

    check_sampling(c, &mut issues);

    check_physics(c, &mut issues);

    check_totals(c, &mut issues);

    check_startup(c, &mut issues);

    Report { issues }
}

/// 一、时间轴：起止缺失/顺序、时长与日期、末日判据。
fn check_timeline(c: &Check, now_ms: Option<i64>, issues: &mut Vec<Issue>) {
    if c.start_time_ms <= 0 || c.stop_time_ms <= 0 {
        issues.push(Issue::fatal("TIME_MISSING", "起止时间戳缺失"));
    } else if c.stop_time_ms <= c.start_time_ms {
        issues.push(Issue::fatal(
            "TIME_ORDER",
            format!(
                "stopTime({}) 不晚于 startTime({})",
                c.stop_time_ms, c.start_time_ms
            ),
        ));
    } else {
        let span = (c.stop_time_ms - c.start_time_ms) / 1000;
        if (span - c.total_time_sec).abs() > 2 {
            issues.push(Issue::fatal(
                "TIME_SPAN",
                format!("时间跨度 {span}s 与 totalTime {}s 不一致", c.total_time_sec),
            ));
        }
        // 与服务器当前时刻比：留 2 秒容差吸收网络往返与毫秒抖动。
        if let Some(now) = now_ms
            && c.stop_time_ms > now + 2_000
        {
            issues.push(Issue::fatal(
                "TIME_FUTURE",
                format!(
                    "stopTime 比服务器现在晚 {}s：记录结束时间落在未来，服务端会驳回",
                    (c.stop_time_ms - now) / 1000
                ),
            ));
        }
    }
}

/// 首尾时间戳必须落在报文声明的边界上，且整条严格递增。
fn check_point_times(c: &Check, issues: &mut Vec<Issue>) {
    let first_t = c.points.first().map(|p| p.time_ms).unwrap_or(0);
    let last_t = c.points.last().map(|p| p.time_ms).unwrap_or(0);
    if first_t != c.start_time_ms {
        issues.push(Issue::fatal(
            "TRACK_START",
            format!("首点时间 {first_t} ≠ startTime {}", c.start_time_ms),
        ));
    }
    if last_t != c.stop_time_ms {
        issues.push(Issue::fatal(
            "TRACK_STOP",
            format!("末点时间 {last_t} ≠ stopTime {}", c.stop_time_ms),
        ));
    }

    // 时间必须严格递增（重复时间戳会让速度无穷大）
    let mut dup = 0;
    for w in c.points.windows(2) {
        if w[1].time_ms <= w[0].time_ms {
            dup += 1;
        }
    }
    if dup > 0 {
        issues.push(Issue::fatal(
            "TRACK_TIME_ORDER",
            format!("{dup} 处时间戳未严格递增"),
        ));
    }
}

/// 二/三：里程与坐标（折线实测、越界、相邻点瞬移）。
fn check_distance_and_geo(c: &Check, issues: &mut Vec<Issue>) {
    let xy: Vec<(f64, f64)> = c.points.iter().map(|p| (p.lat, p.lon)).collect();
    let measured = super::geo::path_length(&xy);
    let declared = c.total_distance_m as f64;
    if (measured - declared).abs() > 2.0 {
        issues.push(Issue::fatal(
            "DIST_MISMATCH",
            format!("折线实测 {measured:.1}m 与 totalDis {declared:.0}m 不符"),
        ));
    }

    // ── 三、横坐标合法性 ──
    let bad_geo = c
        .points
        .iter()
        .filter(|p| {
            !(-90.0..=90.0).contains(&p.lat)
                || !(-180.0..=180.0).contains(&p.lon)
                || (p.lat == 0.0 && p.lon == 0.0)
        })
        .count();
    if bad_geo > 0 {
        issues.push(Issue::fatal(
            "GEO_INVALID",
            format!("{bad_geo} 个坐标越界或为 (0,0)"),
        ));
    }

    // 相邻点跳变（传送门式瞬移，会被判异常）
    let mut max_jump = 0.0f64;
    for w in c.points.windows(2) {
        let d = distance((w[0].lat, w[0].lon), (w[1].lat, w[1].lon));
        if d > max_jump {
            max_jump = d;
        }
    }
    if max_jump > 200.0 {
        issues.push(Issue::fatal(
            "GEO_JUMP",
            format!("相邻点最大跳变 {max_jump:.0}m，超过 200m 会被判定瞬移"),
        ));
    } else if max_jump > 60.0 {
        issues.push(Issue::warn(
            "GEO_JUMP_SOFT",
            format!("相邻点最大跳变 {max_jump:.0}m，偏大"),
        ));
    }
}

/// 四、打卡点：每个必到点必须有一个采样点落在 0.5m 内。
fn check_checkpoints(c: &Check, issues: &mut Vec<Issue>) {
    for (lat, lng, name) in c.checkpoints {
        let best = c
            .points
            .iter()
            .map(|p| distance((p.lat, p.lon), (*lat, *lng)))
            .fold(f64::INFINITY, f64::min);
        if best > 0.5 {
            issues.push(Issue::fatal(
                "CP_MISS",
                format!("打卡点 {name} 最近距离 {best:.1}m，未命中（要求 ≤0.5m）"),
            ));
        }
    }
}

/// 五、分桶：数量、步数合计、连续空桶、自相关。
fn check_buckets(c: &Check, issues: &mut Vec<Issue>) {
    let expect_buckets = (c.total_time_sec / 10).max(1) as usize;
    if c.sim.speed_buckets.len() != expect_buckets {
        issues.push(Issue::fatal(
            "BUCKET_COUNT",
            format!(
                "速度桶 {} 个，应为 {expect_buckets} 个",
                c.sim.speed_buckets.len()
            ),
        ));
    }
    let step_sum: i32 = c.sim.step_buckets.iter().map(|b| b.steps_num).sum();
    if step_sum != c.total_steps {
        issues.push(Issue::fatal(
            "BUCKET_STEPS",
            format!("步数分桶和 {step_sum} ≠ totalSteps {}", c.total_steps),
        ));
    }
    // 空桶：**连续多个** 10 秒完全没位移，服务端才会判定为静止。
    //
    // 早先的写法是 `filter(<0.5).count()`，单个空桶就报警——真机 114 点里
    // 本来就有十几处零位移（起步窗口 + 中途站着看手机），对着真机数据跑这个
    // 检查也会一路响，属于自己吓自己。何况中途停顿点是有意注入的：它把
    // App 配速图的纵轴撑开，作用恰恰是「像真机」。注释写着「连续多个」，
    // 代码就该按连续算。
    let mut run = 0usize;
    let mut worst = 0usize;
    for b in &c.sim.speed_buckets {
        if b.distance < 0.5 {
            run += 1;
            worst = worst.max(run);
        } else {
            run = 0;
        }
    }
    if worst >= 2 {
        issues.push(Issue::warn(
            "BUCKET_EMPTY",
            format!("连续 {worst} 个速度桶位移不足 0.5m，接近静止"),
        ));
    }

    // 分桶自相关过低的提示（白噪声特征）
    let counts: Vec<f64> = c
        .sim
        .step_buckets
        .iter()
        .map(|b| b.steps_num as f64)
        .collect();
    if counts.len() >= 8 {
        let mean = counts.iter().sum::<f64>() / counts.len() as f64;
        let den: f64 = counts.iter().map(|x| (x - mean).powi(2)).sum();
        if den > 1e-9 {
            let num: f64 = counts
                .windows(2)
                .map(|w| (w[0] - mean) * (w[1] - mean))
                .sum();
            let ac = num / den;
            if ac < 0.1 {
                issues.push(Issue::info(
                    "BUCKET_AC_LOW",
                    format!("步数一阶自相关 {ac:.3} 偏低，序列接近白噪声"),
                ));
            }
        }
    }
}

/// 五之二、上报采样节奏：中位间隔、过密插点、配置偏离真机。
fn check_sampling(c: &Check, issues: &mut Vec<Issue>) {
    //
    // 这条检查以前只写在文档和注释里（「硬判定在 preflight 的 SAMPLE_RATE」），
    // 代码里根本没有——于是把 `sample_interval_sec` 配错、或者采样器把拐角
    // 点插爆，本地全绿、上线才发现节奏不对。现在把它写成真的：
    //
    // - 采样器坏了（中位间隔偏离期望值 >30%，或出现真机没有的 1~2 秒间隔）→ Fatal；
    // - 配置本身偏离真机（间隔不在 4~6 秒）→ 只警告，不拦。
    //
    // `expected_interval_sec` 为 `None` 时整段跳过：调用方没交代配置，
    // 拿 5 秒去量一条刻意做密的合成轨迹只会误报。生产路径（`flow::dry_run`
    // 与 `web`）都显式传了 `Some(params.sample_interval_sec)`。
    if let Some(expect) = c.expected_interval_sec.filter(|_| c.points.len() >= 3) {
        let mut dts: Vec<f64> = c
            .points
            .windows(2)
            .map(|w| (w[1].time_ms - w[0].time_ms) as f64 / 1000.0)
            .collect();
        dts.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let median = dts.get(dts.len() / 2).copied().unwrap_or(0.0);
        if expect > 0.0 && (median - expect).abs() > expect * 0.3 {
            issues.push(Issue::fatal(
                "SAMPLE_RATE",
                format!("上报间隔中位数 {median:.2}s，与配置的 {expect:.2}s 相差超过 30%（采样器实现有偏差）"),
            ));
        }
        // 真机的 ΔtotalTime 只出现 0/3/4/5 秒，1~2 秒一次都没有。
        // 期望间隔本身 ≥4 秒时还冒出 <3 秒的间隔，就是采样轴出了问题
        // （典型来源：拐角补点按弧长切时切出 1 秒）。
        if expect >= 4.0
            && let Some(min) = dts.first().copied()
            && min < 3.0
        {
            issues.push(Issue::fatal(
                "SAMPLE_RATE_SHORT",
                format!("出现 {min:.1}s 的上报间隔，真机最短非零间隔是 3s（→ 采样轴插点过密）"),
            ));
        }
        if !(4.0..=6.0).contains(&expect) {
            issues.push(Issue::warn(
                "SAMPLE_RATE_DEVICE",
                format!("上报间隔配置为 {expect:.2}s，已偏离真机的 5 秒/点，服务端按 ΔtotalTime 分布即可分辨"),
            ));
        }
    }
}

/// 六、物理合理性：配速区间（均值/逐点/桶级）、步频、步态、步幅。
fn check_physics(c: &Check, issues: &mut Vec<Issue>) {
    let avg_v = c.total_distance_m as f64 / c.total_time_sec.max(1) as f64;
    // 配速：分/公里
    let pace = 1000.0 / avg_v / 60.0;
    if let Some((lo, hi)) = c.pace_range {
        // 注意：规则里的 speedTop/speedBottom 是分/公里，数值越大越慢
        let (fast, slow) = (lo.min(hi), lo.max(hi));
        if pace < fast || pace > slow {
            issues.push(Issue::fatal(
                "PACE_OUT",
                format!("配速 {pace:.2} 分/km 超出允许区间 [{fast}, {slow}]"),
            ));
        }
    }
    // 服务端可能不按「平均配速」判，而是按**逐点**或 **10 秒桶**判。7.4.20 客户端
    // 新增了 `isPaceOutOfRange` 与「*学校要求配速范围在」文案（见逆向报告），
    // 说明区间校验是逐点口径。这里对冲：只要主体段越界比例过高就提示，
    // 不拦提交（真机自己也有 ~6% 的点因为 GPS 抖动越界）。
    if let Some((lo, hi)) = c.pace_range {
        let (fast, slow) = (lo.min(hi), lo.max(hi));
        // 逐点：只算「在跑」的点（v ≥ 1 m/s），停下/漂移点必然越界，不能算
        let mut body = 0usize;
        let mut out_of_range = 0usize;
        for w in c.points.windows(2) {
            let dt = (w[1].time_ms - w[0].time_ms) as f64 / 1000.0;
            if dt <= 0.0 {
                continue;
            }
            let v = crate::track::geo::distance((w[0].lat, w[0].lon), (w[1].lat, w[1].lon)) / dt;
            if v < 1.0 {
                continue;
            }
            body += 1;
            let point_pace = 1000.0 / v / 60.0;
            if point_pace < fast || point_pace > slow {
                out_of_range += 1;
            }
        }
        // 阈值按**真机基线**定：真机自己的记录里约 6% 的点因 GPS 抖动越界
        // （README「进区间比例 真机 25%」那条同理）。定 5% 会让每一单都报警，
        // 等于把警告变成噪音；取 15% —— 正常数据不响，真异常（大面积越界）才响。
        if body >= 20 {
            let ratio = out_of_range as f64 / body as f64;
            if ratio > PACE_BODY_OUT_WARN {
                issues.push(Issue::warn(
                    "PACE_OUT_BODY",
                    format!(
                        "主体段 {:.1}% 的采样点配速超出 [{fast}, {slow}]（告警线 {:.0}%，真机基线约 6%）。\n    服务端若按逐点判会被当成参数异常，建议缩短时长或调整目标配速。",
                        ratio * 100.0,
                        PACE_BODY_OUT_WARN * 100.0
                    ),
                ));
            }
        }
        // 桶级：10 秒桶的平均配速
        let mut bad_buckets = 0usize;
        let mut total_buckets = 0usize;
        for b in &c.sim.speed_buckets {
            let dt = (b.end_time - b.begin_time) as f64 / 1000.0;
            if dt <= 0.0 || b.distance <= 0.0 {
                continue;
            }
            total_buckets += 1;
            let v = b.distance / dt;
            let bucket_pace = 1000.0 / v / 60.0;
            if bucket_pace < fast || bucket_pace > slow {
                bad_buckets += 1;
            }
        }
        if total_buckets >= 6 {
            let ratio = bad_buckets as f64 / total_buckets as f64;
            if ratio > BUCKET_OUT_WARN {
                issues.push(Issue::warn(
                    "BUCKET_PACE_OUT",
                    format!(
                        "{}/{total_buckets} 个 10 秒桶的平均配速超出 [{fast}, {slow}]（{:.0}%，告警线 {:.0}%）。桶级越界比点级更容易被服务端判为参数异常。",
                        bad_buckets,
                        ratio * 100.0,
                        BUCKET_OUT_WARN * 100.0
                    ),
                ));
            }
        }
    }

    if pace < 3.0 {
        issues.push(Issue::warn(
            "PACE_FAST",
            format!("配速 {pace:.2} 分/km 快得离谱（世界纪录约 2.83）"),
        ));
    }

    let avg_cad = c.total_steps as f64 * 60.0 / c.total_time_sec.max(1) as f64;
    // fallback 必须跟服务端实发口径一致（60~250）。这里原来是 (100, 230)——
    // 和上面 `step_range` 字段的注释直接打架：注释写着「服务端实发 60~250，
    // 拿更窄的假尺子量自己会误拦合法步频」，结果 `None` 时自己还在用窄尺子。
    // 策略里缺 stepTop/stepBottom 时，合法步频 60~99 或 231~250 会被本地
    // 自检拦死，等于自己把好单挡在门外。`policy.rs` 的兜底就是 (60, 250)。
    let (cad_lo, cad_hi) = c.step_range.unwrap_or((60, 250));
    if !((cad_lo as f64)..=(cad_hi as f64)).contains(&avg_cad) {
        issues.push(Issue::fatal(
            "CADENCE_OUT",
            format!("平均步频 {avg_cad:.0} 步/分 超出服务端区间 [{cad_lo}, {cad_hi}]"),
        ));
    }

    // 桶级步频：服务端若按 10 秒桶判步频区间，平均合规但个别桶越界同样会被挑。
    if let Some((lo, hi)) = c.step_range {
        let (cad_lo, cad_hi) = (lo.min(hi), lo.max(hi));
        let mut bad = 0usize;
        let mut total = 0usize;
        for b in &c.sim.step_buckets {
            let dt = (b.end_time - b.begin_time) as f64 / 1000.0;
            if dt <= 0.0 {
                continue;
            }
            total += 1;
            let cad = b.steps_num as f64 * 60.0 / dt;
            if cad < cad_lo as f64 || cad > cad_hi as f64 {
                bad += 1;
            }
        }
        if total >= 6 && (bad as f64) > BUCKET_OUT_WARN * total as f64 {
            issues.push(Issue::warn(
                "BUCKET_CADENCE_OUT",
                format!(
                    "{bad}/{total} 个 10 秒桶的步频超出 [{cad_lo}, {cad_hi}]（{:.0}%，告警线 {:.0}%）。",
                    bad as f64 / total as f64 * 100.0,
                    BUCKET_OUT_WARN * 100.0
                ),
            ));
        }
    }

    // 步态自洽：用报告的步数反跑步态，看是否与速度对得上
    let g = gait(avg_v, 0.0);
    let implied = c.total_steps as f64 * 60.0 / c.total_time_sec.max(1) as f64;
    if (implied - g.cadence).abs() > 25.0 {
        issues.push(Issue::warn(
            "GAIT_MISMATCH",
            format!(
                "步频 {implied:.0} 与该速度下的典型步频 {:.0} 差 {:.0} 步",
                g.cadence,
                (implied - g.cadence).abs()
            ),
        ));
    }

    // 步幅
    //
    // 上界从 1.8 收到 1.03。原来的 1.8 宽到没有意义：真人步幅根本到不了
    // 1.8 m，实测被服务端判「运动数据有问题」的记录步幅是 1.06~1.09，
    // 而同账号被判干净的记录是 0.91~1.01 —— 中间是空的。
    //
    // **只提示不拦**：这个区间是「某账号 11 条记录」的统计，不是协议规则，
    // 拦了等于拿统计当判据。真正的拦截交给提交方（脚本/自动化）决定。
    let stride = if implied > 0.0 {
        avg_v * 60.0 / implied
    } else {
        0.0
    };
    if !(0.4..=1.03).contains(&stride) {
        issues.push(Issue::warn(
            "STRIDE_OUT",
            format!("推算步幅 {stride:.2}m 超出常见范围"),
        ));
    }
}

/// 七、里程与时长的一致性。
fn check_totals(c: &Check, issues: &mut Vec<Issue>) {
    if c.total_time_sec <= 0 {
        issues.push(Issue::fatal("DURATION", "totalTime 必须为正"));
    }
    if c.total_distance_m <= 0 {
        issues.push(Issue::fatal("DISTANCE", "totalDis 必须为正"));
    }
    if (c.sim.actual_length_m - c.total_distance_m as f64).abs() > 2.0 {
        issues.push(Issue::fatal(
            "SIM_DIST",
            format!(
                "合成里程 {:.1}m 与报文 {}m 不符",
                c.sim.actual_length_m, c.total_distance_m
            ),
        ));
    }
}

/// 起步窗口按 15.5% 外推的提示（超过真机样本范围只提示）。
fn check_startup(c: &Check, issues: &mut Vec<Issue>) {
    //
    // 起步窗口按总时长的 15.5% 算，这个比例只来自一份 555 秒的真机样本
    // （窗口 86 秒）。更长的记录没有对照，外推出来的秒数、以及那段时间里
    // 该有多少个零速度点，都没有实测撑着。只提示、不拦：拦了等于拿推测
    // 当判据，反而会挡住本来能提交的记录。
    if c.total_time_sec > 0 {
        let window = super::startup_sec(c.total_time_sec);
        let sample_cap = super::startup_sec(555);
        if window > sample_cap {
            issues.push(Issue::info(
                "STARTUP_EXTRAPOLATED",
                format!(
                    "总时长 {}s 的起步窗口按 15.5% 外推为 {window}s，超过 86s 的部分没有真机样本",
                    c.total_time_sec
                ),
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::track::{Checkpoint, PlanRequest, plan, simulate};

    fn sample() -> (Vec<TrackPoint>, SimResult, SimParams) {
        let req = PlanRequest {
            center_lat: 30.5,
            center_lng: 104.0,
            checkpoints: vec![
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
            ],
            total_distance_m: 2000.0,
            total_sec: 720,
            start_ms: 1_730_000_000_000,
            sport_type: 1,
            sample_step_m: 5.0,
            end: None,
        };
        let mut rng = rand::rngs::StdRng::seed_from_u64(1);
        use rand::SeedableRng;
        let t = plan(&req, &mut rng).unwrap();
        let pts: Vec<(f64, f64, i64)> =
            t.points.iter().map(|p| (p.lat, p.lon, p.time_ms)).collect();
        let sp = SimParams {
            total_distance_m: t.actual_length_m,
            total_sec: 720,
            start_ms: 1_730_000_000_000,
            target_steps: Some(2300),
            seed: 1,
            ..Default::default()
        };
        let sim = simulate(&pts, &sp).unwrap();
        (t.points, sim, sp)
    }

    /// 策略里没给步频区间时，fallback 必须跟服务端实发口径一致（60~250）。
    ///
    /// 原来 fallback 是 `(100, 230)`，比服务端窄——`step_range` 字段的注释
    /// 自己就写着「服务端实发 60~250，拿更窄的假尺子量自己会误拦合法步频」，
    /// 结果 `None` 时还在用窄尺子。策略缺字段时，合法步频 70 会被本地自检
    /// 拦死，等于自己把好单挡在门外。
    #[test]
    fn missing_step_range_falls_back_to_server_bounds() {
        let (base, sim, sp) = sample();
        let start = base[0].time_ms;
        // 步频 70：在服务端 60~250 内，在旧的 (100,230) 外
        let sim70 = SimResult {
            avg_cadence: 70.0,
            ..sim.clone()
        };
        let c = Check {
            points: &base,
            sim: &sim70,
            params: &sp,
            checkpoints: &[],
            total_distance_m: 2000,
            total_time_sec: 720,
            total_steps: 2300,
            start_time_ms: start,
            stop_time_ms: start + 720_000,
            pace_range: None,
            step_range: None,
            expected_interval_sec: None,
        };
        let txt = inspect_at(&c, None).lines().join("\n");
        assert!(
            !txt.contains("CADENCE"),
            "步频 70 在服务端口径内，不该被拦：{txt}"
        );
    }

    /// 逐点 / 桶级配速越界要各出一条提示（服务端可能按点或桶判，不按平均）。
    ///
    /// 把配速区间收到一个和生成轨迹明显不交的窄带，两种口径都应该报警；
    /// 同时确认正常数据不会被误报（宁可漏报也不能让自检天天喊狼）。
    #[test]
    fn point_and_bucket_pace_ranges_are_checked() {
        let (base, sim, sp) = sample();
        let start = base[0].time_ms;
        let mk = |pace: Option<(f64, f64)>, step: Option<(i64, i64)>| Check {
            points: &base,
            sim: &sim,
            params: &sp,
            checkpoints: &[],
            total_distance_m: 2000,
            total_time_sec: 720,
            total_steps: 2300,
            start_time_ms: start,
            stop_time_ms: start + 720_000,
            pace_range: pace,
            step_range: step,
            expected_interval_sec: None,
        };
        // 正常：2km/12min ⇒ 6'00"/km，放在 [4'00", 9'00"] 内不该报
        let ok = inspect_at(&mk(Some((4.0, 9.0)), Some((60, 250))), None)
            .lines()
            .join("\n");
        assert!(
            !ok.contains("PACE_OUT_BODY"),
            "正常区间不该报逐点越界：{ok}"
        );
        assert!(
            !ok.contains("BUCKET_PACE_OUT"),
            "正常区间不该报桶级越界：{ok}"
        );

        // 窄带：整个主体段都在区间外，两种口径都要报
        let bad = inspect_at(&mk(Some((2.0, 3.0)), Some((60, 250))), None)
            .lines()
            .join("\n");
        assert!(bad.contains("PACE_OUT_BODY"), "逐点越界没报：{bad}");
        assert!(bad.contains("BUCKET_PACE_OUT"), "桶级越界没报：{bad}");
    }

    /// 「记录结束时间不得在服务器未来」必须由 `inspect_at` 真的拦下来。
    ///
    /// 早先自检只比 stop/start、跨度/totalTime，全是同一条时间轴内部的相对
    /// 关系，「现在跑」把整条时间窗放到未来也全绿。这条钉住新增的 TIME_FUTURE。
    #[test]
    fn stop_time_in_server_future_is_fatal() {
        let (base, sim, sp) = sample();
        let start = base[0].time_ms;
        let c = Check {
            points: &base,
            sim: &sim,
            params: &sp,
            checkpoints: &[],
            total_distance_m: sim.actual_length_m.round() as i64,
            total_time_sec: 720,
            total_steps: 2300,
            start_time_ms: start,
            stop_time_ms: start + 720_000,
            pace_range: None,
            step_range: None,
            expected_interval_sec: Some(crate::track::SAMPLE_INTERVAL_SEC),
        };
        // 服务器时间停在「开始时刻」上 → 结束时间晚了整整 720 秒
        let r = inspect_at(&c, Some(start));
        assert!(
            r.issues
                .iter()
                .any(|i| i.code == "TIME_FUTURE" && i.level == Level::Fatal),
            "结束时间在未来必须报 TIME_FUTURE，实际 {:?}",
            r.lines()
        );
        // 不知道服务器当前时间时跳过这一项，其余检查不受影响
        let r2 = inspect(&c);
        assert!(!r2.issues.iter().any(|i| i.code == "TIME_FUTURE"));
        // 结束时刻在过去（服务器现在 = stop + 10s）→ 不报
        let r3 = inspect_at(&c, Some(start + 720_000 + 10_000));
        assert!(!r3.issues.iter().any(|i| i.code == "TIME_FUTURE"));
    }

    /// `SAMPLE_RATE` 必须是**真的在拦**。
    ///
    /// 这条规则的注释从第一版就写着「硬判定在 preflight 的 SAMPLE_RATE」，
    /// 而代码里一直没有它——于是采样轴跑偏（真机 5 秒/点，跑成 2 秒/点）
    /// 本地全绿。这组测试把三种情形钉死。
    #[test]
    fn sample_rate_is_actually_enforced() {
        let (base, sim, sp) = sample();
        let start = base[0].time_ms;
        let retime = |gap_ms: i64| -> Vec<TrackPoint> {
            base.iter()
                .enumerate()
                .map(|(i, p)| TrackPoint {
                    lat: p.lat,
                    lon: p.lon,
                    time_ms: start + i as i64 * gap_ms,
                })
                .collect()
        };

        // ① 采样轴整体跑偏（压成 2 秒/点）：Fatal
        let pts = retime(2000);
        let c = Check {
            points: &pts,
            sim: &sim,
            params: &sp,
            checkpoints: &[],
            total_distance_m: sim.actual_length_m.round() as i64,
            total_time_sec: 720,
            total_steps: 2300,
            start_time_ms: start,
            stop_time_ms: start + 720_000,
            pace_range: None,
            step_range: None,
            expected_interval_sec: Some(crate::track::SAMPLE_INTERVAL_SEC),
        };
        let r = inspect(&c);
        assert!(
            r.issues
                .iter()
                .any(|i| i.code == "SAMPLE_RATE" && i.level == Level::Fatal),
            "2 秒/点的采样轴必须报 SAMPLE_RATE，实际 {:?}",
            r.lines()
        );

        // ② 主体 5 秒、中途夹一个 1 秒间隔（拐角补点切碎了网格）：
        //    真机一次都没出现过 <3 秒的间隔，必须 Fatal
        let mut pts = retime(5000);
        for p in pts.iter_mut().skip(41) {
            p.time_ms -= 4000;
        }
        let c = Check { points: &pts, ..c };
        let r = inspect(&c);
        assert!(
            r.issues
                .iter()
                .any(|i| i.code == "SAMPLE_RATE_SHORT" && i.level == Level::Fatal),
            "1 秒间隔必须报 SAMPLE_RATE_SHORT，实际 {:?}",
            r.lines()
        );

        // ③ 配置本身偏离真机（15 秒/点）：只警告，不拦
        let pts = retime(15_000);
        let c = Check {
            points: &pts,
            expected_interval_sec: Some(15.0),
            ..c
        };
        let r = inspect(&c);
        assert!(
            r.issues
                .iter()
                .any(|i| i.code == "SAMPLE_RATE_DEVICE" && i.level == Level::Warn),
            "15 秒/点应只报警告，实际 {:?}",
            r.lines()
        );
        assert!(
            !r.issues
                .iter()
                .any(|i| i.code.starts_with("SAMPLE_RATE") && i.level == Level::Fatal),
            "配置偏离不该阻断，实际 {:?}",
            r.lines()
        );
    }

    fn check<'a>(
        pts: &'a [TrackPoint],
        sim: &'a SimResult,
        sp: &'a SimParams,
        cps: &'a [(f64, f64, String)],
    ) -> Check<'a> {
        Check {
            points: pts,
            sim,
            params: sp,
            checkpoints: cps,
            // 必须和合成轨迹一致：SIM_DIST 检查的是「报文里程 vs 轨迹实测」，
            // 两者本来就该相等（flow 里报文直接用 real_len）。
            total_distance_m: sim.actual_length_m.round() as i64,
            total_time_sec: 720,
            total_steps: 2300,
            start_time_ms: 1_730_000_000_000,
            stop_time_ms: 1_730_000_720_000,
            pace_range: None,
            step_range: None,
            expected_interval_sec: None,
        }
    }

    #[test]
    fn healthy_track_passes() {
        let (pts, sim, sp) = sample();
        let cps = vec![
            (30.5008, 104.0008, "A".to_string()),
            (30.5008, 104.0024, "B".to_string()),
        ];
        let r = inspect(&check(&pts, &sim, &sp, &cps));
        assert!(!r.blocked(), "正常轨迹不该被阻断: {:?}", r.lines());
    }

    #[test]
    fn detects_dist_mismatch() {
        let (pts, sim, sp) = sample();
        let mut c = check(&pts, &sim, &sp, &[]);
        c.total_distance_m = 3000; // 谎报里程
        let r = inspect(&c);
        assert!(r.blocked());
        assert!(r.issues.iter().any(|i| i.code == "DIST_MISMATCH"));
    }

    #[test]
    fn detects_time_misalignment() {
        let (pts, sim, sp) = sample();
        let mut c = check(&pts, &sim, &sp, &[]);
        c.start_time_ms += 5000; // 首点对不上
        let r = inspect(&c);
        assert!(r.blocked());
        assert!(r.issues.iter().any(|i| i.code == "TRACK_START"));
    }

    #[test]
    fn detects_missed_checkpoint() {
        let (pts, sim, sp) = sample();
        let cps = vec![(31.0, 105.0, "远在天边".to_string())];
        let r = inspect(&check(&pts, &sim, &sp, &cps));
        assert!(r.blocked());
        assert!(r.issues.iter().any(|i| i.code == "CP_MISS"));
    }

    #[test]
    fn detects_pace_out_of_range() {
        let (pts, sim, sp) = sample();
        let mut c = check(&pts, &sim, &sp, &[]);
        c.pace_range = Some((9.0, 12.0)); // 只允许极慢
        let r = inspect(&c);
        assert!(r.blocked());
        assert!(r.issues.iter().any(|i| i.code == "PACE_OUT"));
    }

    #[test]
    fn detects_step_sum_mismatch() {
        let (pts, sim, sp) = sample();
        let mut c = check(&pts, &sim, &sp, &[]);
        c.total_steps = 9999;
        let r = inspect(&c);
        assert!(r.blocked());
        assert!(r.issues.iter().any(|i| i.code == "BUCKET_STEPS"));
    }

    #[test]
    fn long_run_gets_startup_extrapolation_note() {
        let (pts, sim, sp) = sample();
        let mut c = check(&pts, &sim, &sp, &[]);
        c.total_time_sec = 555;
        assert!(
            !inspect(&c)
                .issues
                .iter()
                .any(|i| i.code == "STARTUP_EXTRAPOLATED"),
            "555 秒正好是样本本身的时长，不该提示外推"
        );
        c.total_time_sec = 1800;
        let r = inspect(&c);
        let hit = r
            .issues
            .iter()
            .find(|i| i.code == "STARTUP_EXTRAPOLATED")
            .expect("1800 秒应提示起步窗口外推");
        // 只验这条本身是 Info；强行改时长会让时间轴那些检查同时报致命，
        // 那是测试构造的副作用，不代表这条会拦提交。
        assert_eq!(hit.level, Level::Info, "只提示，不阻断");
    }

    #[test]
    fn report_summary_readable() {
        let (pts, sim, sp) = sample();
        let mut c = check(&pts, &sim, &sp, &[]);
        c.total_distance_m = 9999;
        let r = inspect(&c);
        assert!(!r.summary().is_empty());
        assert!(!r.lines().is_empty());
        assert!(r.lines()[0].contains("致命"));
    }
}

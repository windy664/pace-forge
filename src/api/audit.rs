//! 账号体检（关 issue #14）：把跨记录指标算成一份可读报告，再给下一步建议。
//!
//! 和 `crosscheck` 的分工：`crosscheck` 只打数，`audit` 在同一批数上加**阈值
//! 判断**和**动作建议**。阈值不是协议规则，是「如果我是审核的人会先看哪几条」
//! 的经验值（见下方常量表），命中只提示、不拦提交。
//!
//! 三件套：
//! - `audit`   —— 指标 + 逐条发现（`AUDIT_*` 检查码，和 preflight 的 `PACE_*` 分开）
//! - `suggest` —— 基于发现给的具体动作（换时段/换线路/里程递进/隔天）
//! - `tracks`  —— 两两重合矩阵，顺带区分同向与反向（反向跑是真人行为）
//!
//! 数据来自 `pace-forge export-records`，不联网、不读账号。

use crate::api::crosscheck::{self as cc, RunRecord};
use crate::error::{Error, ErrorKind};

/// 连续天数到多少算「值得看一眼」。学校层面一般按周看，5 天已经能画出一根直线。
pub const STREAK_WARN_DAYS: usize = 5;
/// 起跑时刻落在同一个整点的占比。真人有作息，但不会一周里八成都同一分钟。
pub const HOUR_SHARE_WARN: f64 = 0.60;
/// 里程变异系数低于这个值，说明每条都一样长——真人会有一天 2km、一天 3km。
pub const DIST_CV_WARN: f64 = 0.05;
/// 配速变异系数低于这个值，同上。
pub const PACE_CV_WARN: f64 = 0.02;
/// 两条轨迹重合到这个程度，就是同一条线。
pub const OVERLAP_WARN: f64 = 0.90;
/// 反向判定：两条轨迹首尾互换后仍在这么多米内，算「同一条线反着跑」。
pub const REVERSE_TOL_M: f64 = 30.0;

/// 一次体检的全部指标。字段全是算出来的数，没有判断。
#[derive(Debug, Clone, Default)]
pub struct Metrics {
    /// 记录条数。
    pub count: usize,
    /// 首末记录跨了几天（含首末）
    pub span_days: usize,
    /// 最长连续打卡天数。
    pub streak_best: usize,
    /// 从最后一条往前数，当前连续了几天。
    pub streak_cur: usize,
    /// 起跑时刻最集中的整点（本地时区）
    pub hour: u32,
    /// 落在最集中那个整点的记录占比（0~1）。
    pub hour_share: f64,
    /// 平均里程（米）。
    pub dist_mean: f64,
    /// 里程变异系数（标准差 / 均值），越小越整齐。
    pub dist_cv: f64,
    /// 平均配速（秒/公里）。
    pub pace_mean: f64,
    /// 配速变异系数，越小越像机器生成。
    pub pace_cv: f64,
    /// 重合度最高的一对：`(下标 a, 下标 b, 重合度)`，0 基
    pub overlap: Option<(usize, usize, f64)>,
    /// 反向跑的一对：`(下标 a, 下标 b, 重合度)`，重合度高但方向相反
    pub reversed: Option<(usize, usize, f64)>,
    /// 全部两两结果，按重合度降序。`findings` 靠它逐对判断——
    /// 只留「最高那一对」会漏报：一对反向的挡住第一名，后面同向的真复制就永远
    /// 不报警（#14 指出的方向性错误）。
    pub pairs: Vec<(usize, usize, f64, bool)>,
    /// 起跑或结束贴着 0 点的记录数。
    pub near_midnight: usize,
    /// 记录所在时区偏移（秒，东八区为 28800）。
    pub tz: i64,
    /// 时区的人话说明。
    pub tz_desc: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// 一条体检结论的严重程度。
pub enum Level {
    /// 只是陈述，不是问题
    Info,
    /// 值得看一眼
    Warn,
}

#[derive(Debug, Clone)]
/// 一条体检结论。
pub struct Finding {
    /// 检查码，`AUDIT_` 前缀
    pub code: &'static str,
    /// 严重程度。
    pub level: Level,
    /// 给用户看的说明。
    pub text: String,
}

/// 所有 `AUDIT_*` 检查码，`config` 表尾引用它，免得和 `PACE_*` 环境变量混。
pub const AUDIT_CODES: [&str; 7] = [
    "AUDIT_STREAK",
    "AUDIT_HOUR",
    "AUDIT_DIST_FLAT",
    "AUDIT_PACE_FLAT",
    "AUDIT_OVERLAP",
    "AUDIT_REVERSE",
    "AUDIT_MIDNIGHT",
];

/// 首 / 中 / 尾三点互换后都贴近 → 同一条线反着跑。
///
/// **不能只比首尾**：记分跑绕的是闭合跑道，首点≈末点，于是任意两条同跑道
/// 记录（同向也好反向也好）首尾距离都接近 0，「反向」恒为真（#14 实测）。
///
/// 取样的索引也要避开中心对称位：`i` 与 `n-1-i` 在 `i=(n-1)/2` 时是同一个
/// 点，那样中点判据等于又比了一次首尾。用 `n/6, n/3, n/2` 三处，两两间
/// 隔得够远，同向时必然落在轨迹的不同位置。
fn is_reversed(a: &[(f64, f64)], b: &[(f64, f64)]) -> bool {
    let n = a.len().min(b.len());
    if n < 4 {
        return false;
    }
    [n / 6, n / 3, n / 2]
        .iter()
        .all(|&i| crate::track::geo::distance(a[i], b[n - 1 - i]) <= REVERSE_TOL_M)
}

/// 两两重合：返回 `(i, j, 重合度, 是否反向)`，按重合度降序。
pub fn overlap_pairs(records: &[RunRecord], radius_m: f64) -> Vec<(usize, usize, f64, bool)> {
    let idx: Vec<usize> = (0..records.len())
        .filter(|i| records[*i].track.len() >= 2)
        .collect();
    let mut out = Vec::new();
    for x in 0..idx.len() {
        for y in (x + 1)..idx.len() {
            let (i, j) = (idx[x], idx[y]);
            let (ta, tb) = (&records[i].track, &records[j].track);
            let ov = cc::track_overlap(ta, tb, radius_m);
            out.push((i, j, ov, is_reversed(ta, tb)));
        }
    }
    out.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));
    out
}

/// 算指标。纯函数，不读环境、不联网。
pub fn metrics(records: &[RunRecord]) -> Metrics {
    let tz = cc::tz_offset_sec();
    let mut m = Metrics {
        count: records.len(),
        tz,
        tz_desc: cc::tz_description(),
        ..Metrics::default()
    };
    if records.is_empty() {
        return m;
    }
    let first = records.iter().map(|r| r.start_ms).min().unwrap_or(0);
    let last = records.iter().map(|r| r.start_ms).max().unwrap_or(0);
    m.span_days = crate::number::i64_to_usize((last - first) / 86_400_000 + 1).unwrap_or(0);
    let (best, cur) = cc::streak_days(records, tz);
    m.streak_best = best;
    m.streak_cur = cur;
    let (hour, share) = cc::start_hour_concentration(records, tz);
    m.hour = hour;
    m.hour_share = share;

    let dists: Vec<f64> = records.iter().map(|r| r.distance_m).collect();
    let (dm, dcv) = cc::dispersion(&dists);
    m.dist_mean = dm;
    m.dist_cv = dcv;

    let paces: Vec<f64> = records
        .iter()
        .filter(|r| r.distance_m > 0.0 && r.duration_sec > 0)
        .map(|r| r.duration_sec as f64 / (r.distance_m / 1000.0))
        .collect();
    let (pm, pcv) = cc::dispersion(&paces);
    m.pace_mean = pm;
    m.pace_cv = pcv;

    let pairs = overlap_pairs(records, cc::OVERLAP_RADIUS_M);
    if let Some((i, j, ov, rev)) = pairs.first().copied() {
        m.overlap = Some((i, j, ov));
        if rev {
            m.reversed = Some((i, j, ov));
        }
    }
    m.pairs = pairs;
    m.near_midnight = cc::near_midnight_count(records, tz);
    m
}

/// 阈值判断。命中只出 `Warn`，不拦任何东西。
pub fn findings(m: &Metrics) -> Vec<Finding> {
    let mut out = Vec::new();
    if m.count == 0 {
        return out;
    }
    if m.streak_cur >= STREAK_WARN_DAYS {
        out.push(Finding {
            code: "AUDIT_STREAK",
            level: Level::Warn,
            text: format!(
                "当前连续 {} 天有记录（阈值 {STREAK_WARN_DAYS} 天）。连续性是跨记录最容易看出的特征，建议中间插一天不跑。",
                m.streak_cur
            ),
        });
    }
    if m.hour_share >= HOUR_SHARE_WARN {
        out.push(Finding {
            code: "AUDIT_HOUR",
            level: Level::Warn,
            text: format!(
                "起跑时刻 {:.0}% 落在 {:02}:00 这一小时（阈值 {:.0}%）。真人有作息但不会这么齐，换时段成本最低。",
                m.hour_share * 100.0,
                m.hour,
                HOUR_SHARE_WARN * 100.0
            ),
        });
    }
    if m.dist_cv < DIST_CV_WARN && m.count >= 3 {
        out.push(Finding {
            code: "AUDIT_DIST_FLAT",
            level: Level::Warn,
            text: format!(
                "里程离散只有 {:.1}%（阈值 {:.0}%），每条几乎一样长。真跑会有一天 2km、一天 3km。",
                m.dist_cv * 100.0,
                DIST_CV_WARN * 100.0
            ),
        });
    }
    if m.pace_cv > 0.0 && m.pace_cv < PACE_CV_WARN && m.count >= 3 {
        out.push(Finding {
            code: "AUDIT_PACE_FLAT",
            level: Level::Warn,
            text: format!(
                "配速离散只有 {:.1}%（阈值 {:.0}%），每次都跑成同一配速。",
                m.pace_cv * 100.0,
                PACE_CV_WARN * 100.0
            ),
        });
    }
    // 逐对看，不只看最高那一对：同向高重合才是复制特征，反向是真人行为。
    let mut cloned: Vec<(usize, usize, f64)> = m
        .pairs
        .iter()
        .filter(|(_, _, ov, rev)| *ov >= OVERLAP_WARN && !*rev)
        .map(|(i, j, ov, _)| (*i, *j, *ov))
        .collect();
    let total = cloned.len();
    cloned.truncate(5);
    for (i, j, ov) in cloned {
        out.push(Finding {
            code: "AUDIT_OVERLAP",
            level: Level::Warn,
            text: format!(
                "第 {} 条和第 {} 条轨迹重合 {:.0}% 且方向一致（阈值 {:.0}%）。同一条线同向跑两遍是最明显的复制特征。",
                i + 1,
                j + 1,
                ov * 100.0,
                OVERLAP_WARN * 100.0
            ),
        });
    }
    if total > 5 {
        out.push(Finding {
            code: "AUDIT_OVERLAP",
            level: Level::Warn,
            text: format!(
                "另有 {} 对同向高重合未逐条列出，用 `tracks` 看全表。",
                total - 5
            ),
        });
    }
    if let Some((i, j, ov)) = m.reversed {
        out.push(Finding {
            code: "AUDIT_REVERSE",
            level: Level::Info,
            text: format!(
                "第 {} 条和第 {} 条是同一条线反着跑（重合 {:.0}%）。这是真人行为，不用改；列出来是为了别把它当复制。",
                i + 1,
                j + 1,
                ov * 100.0
            ),
        });
    }
    if m.near_midnight > 0 {
        out.push(Finding {
            code: "AUDIT_MIDNIGHT",
            level: Level::Info,
            text: format!(
                "{} 条记录起跑时刻距本地零点不足 1 小时，日期归属受时区影响（当前 {}）。",
                m.near_midnight, m.tz_desc
            ),
        });
    }
    out
}

/// 解析 `tracks` 的子命令参数：`[records.json] [--top N]`。
///
/// 参数写错必须报错，不能当成「没写」——`--top abc` 静默回落会打出全部
/// 4950 对，和 v0.1.9 立的规矩相反（写错与没写后果差得远，见 #23）。
///
/// 放在 lib 里而不是 `main.rs`：CI 跑的是 `cargo test --lib`，写在 main 里
/// 的测试不会被执行。
pub fn parse_tracks_args(args: &[String]) -> Result<(Option<String>, Option<usize>), Error> {
    let mut file: Option<String> = None;
    let mut top: Option<usize> = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--top" => {
                let v = it
                    .next()
                    .ok_or_else(|| ErrorKind::client("--top 缺少次数，例如 --top 20".into()))?;
                let n: usize = v
                    .parse()
                    .map_err(|_| ErrorKind::client(format!("--top 不是整数：「{v}」")))?;
                if !(1..=5000).contains(&n) {
                    return Err(ErrorKind::client(format!(
                        "--top 需在 1..5000 之间，收到 {n}"
                    )));
                }
                top = Some(n);
            }
            s if s.starts_with("--") => return Err(ErrorKind::client(format!("未知参数：{s}"))),
            s => {
                if file.is_some() {
                    return Err(ErrorKind::client(format!(
                        "多余的参数：{s}（只接一个记录文件）"
                    )));
                }
                file = Some(s.to_string());
            }
        }
    }
    Ok((file, top))
}

/// 把发现翻成动作。每条都指向「下一条记录怎么跑」，不是抽象建议。
pub fn suggestions(m: &Metrics, found: &[Finding]) -> Vec<String> {
    let mut out = Vec::new();
    let has = |code: &str| found.iter().any(|f| f.code == code);

    if has("AUDIT_STREAK") {
        out.push(format!(
            "下一条隔一天再跑（今天是第 {} 天连着），让连续性断一次。",
            m.streak_cur
        ));
    }
    if has("AUDIT_HOUR") {
        let alt = (m.hour + 4) % 24;
        out.push(format!(
            "起跑时刻换到 {alt:02}:00 前后（现在集中在 {:02}:00），间隔 3 小时以上才算换时段。",
            m.hour
        ));
    }
    if has("AUDIT_OVERLAP") {
        out.push("下一条换方向或换一条线：反着跑、或者选另一侧的路，重合度自然掉下去。".into());
    }
    if has("AUDIT_DIST_FLAT") {
        let base = if m.dist_mean > 0.0 {
            m.dist_mean
        } else {
            2000.0
        };
        out.push(format!(
            "里程按 {:.0}m ± 15% 走（比如 {:.0}m / {:.0}m 交替），别每条一模一样。",
            base,
            (base * 1.15 / 100.0).round() * 100.0,
            (base * 0.85 / 100.0).round() * 100.0
        ));
    }
    if has("AUDIT_PACE_FLAT") {
        out.push("配速留出自然波动：慢跑日放到 6'30\"~7'00\"，别每次都 5'40\"。".into());
    }
    if out.is_empty() {
        out.push("这几条指标没到阈值，照现在的节奏跑就行；阈值只是经验值，不是协议规则。".into());
    }
    // 出口：这是三个风控维度里唯一能当场查的（时段/频率要靠自己记）。
    // 不联网，只读配置——`audit` 是离线命令，探测留给顶栏。
    match crate::api::egress::current_proxy() {
        Some(p) => out.push(format!(
            "提交走代理出口（{p}）——它的归属地必须和校区同城，不同城就别跑；确认一次，别每次都赌。"
        )),
        None => out.push(
            "提交走本机网络——开 `pace-forge web` 看顶栏那行出口 IP，不在校区同城就先换网络再跑。"
                .into(),
        ),
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 造一条记录。`day`/`hour` 是**本地**时间：减掉 +08:00 偏移，免得
    /// 测试按 UTC 摆数据、实现按本地切天，两边差出一天。
    fn rec(day: i64, hour: i64, dist: f64, dur: i64, track: Vec<(f64, f64)>) -> RunRecord {
        RunRecord {
            start_ms: (day * 86_400 + hour * 3_600 - 8 * 3_600) * 1000,
            distance_m: dist,
            duration_sec: dur,
            track,
        }
    }

    /// 沿纬线造直线，`offset_m` 用来错开两条线，`n` 控制长度。
    fn line(offset_m: f64, n: usize) -> Vec<(f64, f64)> {
        (0..n)
            .map(|i| (offset_m / 110_000.0, i as f64 * 30.0 / 110_000.0))
            .collect()
    }

    fn line_rev(offset_m: f64, n: usize) -> Vec<(f64, f64)> {
        let mut v = line(offset_m, n);
        v.reverse();
        v
    }

    #[test]
    fn flat_distance_and_hour_are_flagged() {
        // 同一天同一小时、里程全一样 → 三条都该命中
        let records: Vec<RunRecord> = (0..5)
            .map(|i| rec(i, 7, 2000.0, 700, line(0.0, 40)))
            .collect();
        let m = metrics(&records);
        let f = findings(&m);
        let codes: Vec<&str> = f.iter().map(|x| x.code).collect();
        assert!(
            codes.contains(&"AUDIT_STREAK"),
            "连续 5 天该命中: {codes:?}"
        );
        assert!(codes.contains(&"AUDIT_HOUR"), "起跑点全在 07 点: {codes:?}");
        assert!(codes.contains(&"AUDIT_DIST_FLAT"), "里程无差异: {codes:?}");
        assert!(codes.contains(&"AUDIT_OVERLAP"), "轨迹完全相同: {codes:?}");
    }

    #[test]
    fn normal_spread_produces_no_warn() {
        // 隔天跑、时段分散、里程和轨迹都不同 → 不该出 Warn
        let records = vec![
            rec(0, 7, 2000.0, 700, line(0.0, 40)),
            rec(2, 18, 3000.0, 1200, line(900.0, 40)),
            rec(4, 12, 2500.0, 880, line(1800.0, 40)),
        ];
        let m = metrics(&records);
        let warns: Vec<&str> = findings(&m)
            .iter()
            .filter(|f| f.level == Level::Warn)
            .map(|f| f.code)
            .collect();
        assert!(warns.is_empty(), "不该报警: {warns:?}");
        assert_eq!(m.streak_best, 1, "隔天跑最长连续应为 1 天");
    }

    #[test]
    fn reversed_run_is_info_not_warn() {
        let a = line(0.0, 40);
        let b = line_rev(0.0, 40);
        let records = vec![rec(0, 7, 2000.0, 700, a), rec(3, 17, 2600.0, 900, b)];
        let m = metrics(&records);
        let f = findings(&m);
        assert!(f.iter().any(|x| x.code == "AUDIT_REVERSE"), "{f:?}");
        assert!(
            !f.iter().any(|x| x.code == "AUDIT_OVERLAP"),
            "反向跑不该算成同向复制: {f:?}"
        );
        assert!(f.iter().all(|x| x.level == Level::Info), "反向跑只是 Info");
    }

    #[test]
    fn overlap_pairs_sorted_and_skip_short_tracks() {
        let records = vec![
            rec(0, 7, 2000.0, 700, line(0.0, 40)),
            rec(1, 7, 2000.0, 700, line(0.0, 40)),
            rec(2, 7, 2000.0, 700, line(3000.0, 40)),
            rec(3, 7, 2000.0, 700, vec![]), // 没轨迹，不参与
        ];
        let pairs = overlap_pairs(&records, cc::OVERLAP_RADIUS_M);
        assert_eq!(pairs.len(), 3, "4 条里 3 条带轨迹 → 3 对");
        assert_eq!((pairs[0].0, pairs[0].1), (0, 1));
        assert!(pairs[0].2 > 0.95);
        assert!(!pairs[0].3, "同向");
        assert!(pairs.windows(2).all(|w| w[0].2 >= w[1].2), "要按重合度降序");
    }

    /// 闭合圈：操场绕圈首点≈末点。同向两圈不能被判成「反向」，
    /// 否则 `AUDIT_OVERLAP` 被永久跳过（#14 实测到的方向性错误）。
    fn circle(offset_m: f64, n: usize, rev: bool) -> Vec<(f64, f64)> {
        let mut v: Vec<(f64, f64)> = (0..=n)
            .map(|i| {
                let t = i as f64 / n as f64 * std::f64::consts::TAU;
                (
                    offset_m / 110_000.0 + 0.0003 * t.cos(),
                    0.0003 * t.sin() * 1.4,
                )
            })
            .collect();
        if rev {
            v.reverse();
        }
        v
    }

    #[test]
    fn closed_loop_same_direction_is_not_reversed() {
        let records = vec![
            rec(0, 7, 2000.0, 700, circle(0.0, 60, false)),
            rec(3, 18, 2600.0, 900, circle(0.0, 60, false)),
        ];
        let m = metrics(&records);
        let f = findings(&m);
        assert!(
            !f.iter().any(|x| x.code == "AUDIT_REVERSE"),
            "同向的闭合圈不是反向: {f:?}"
        );
        assert!(
            f.iter().any(|x| x.code == "AUDIT_OVERLAP"),
            "闭合圈同向高重合必须报复制: {f:?}"
        );
    }

    #[test]
    fn closed_loop_reverse_direction_is_info_only() {
        let records = vec![
            rec(0, 7, 2000.0, 700, circle(0.0, 60, false)),
            rec(3, 18, 2600.0, 900, circle(0.0, 60, true)),
        ];
        let m = metrics(&records);
        let f = findings(&m);
        assert!(f.iter().any(|x| x.code == "AUDIT_REVERSE"), "{f:?}");
        assert!(
            !f.iter().any(|x| x.code == "AUDIT_OVERLAP"),
            "反向不算复制: {f:?}"
        );
    }

    #[test]
    fn suggestions_point_at_concrete_actions() {
        let records: Vec<RunRecord> = (0..5)
            .map(|i| rec(i, 7, 2000.0, 700, line(0.0, 40)))
            .collect();
        let m = metrics(&records);
        let s = suggestions(&m, &findings(&m));
        assert!(s.iter().any(|x| x.contains("隔一天")), "{s:?}");
        // 不能把 format! 写漏成字面量（#14 抓到的）
        assert!(
            !s.iter().any(|x| x.contains("{}")),
            "冒出了未填充的占位符: {s:?}"
        );
        assert!(s.iter().any(|x| x.contains("换方向")), "{s:?}");
        assert!(s.iter().any(|x| x.contains("里程按")), "{s:?}");
        // 出口那条无论配没配代理都要给（这是三个风控维度里唯一能当场查的）
        assert!(
            s.iter().any(|x| x.contains("出口")),
            "suggest 必须提示出口：{s:?}"
        );
        // 没有命中时也要给一句，不能空手回去
        let clean = suggestions(&metrics(&[]), &[]);
        assert!(!clean.is_empty());
    }

    #[test]
    fn tracks_args_accept_valid_forms() {
        let args = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(parse_tracks_args(&args(&[])).unwrap(), (None, None));
        assert_eq!(
            parse_tracks_args(&args(&["rec.json"])).unwrap(),
            (Some("rec.json".to_string()), None)
        );
        assert_eq!(
            parse_tracks_args(&args(&["rec.json", "--top", "20"])).unwrap(),
            (Some("rec.json".to_string()), Some(20))
        );
        // 顺序反过来也要认
        assert_eq!(
            parse_tracks_args(&args(&["--top", "3", "rec.json"])).unwrap(),
            (Some("rec.json".to_string()), Some(3))
        );
    }

    /// 写错要报错，不能静默变成「没写」。
    #[test]
    fn tracks_args_reject_bad_values() {
        let args = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(
            parse_tracks_args(&args(&["rec.json", "--top"])).is_err(),
            "缺值"
        );
        assert!(
            parse_tracks_args(&args(&["rec.json", "--top", "abc"])).is_err(),
            "非整数"
        );
        assert!(
            parse_tracks_args(&args(&["rec.json", "--top", "0"])).is_err(),
            "下界"
        );
        assert!(
            parse_tracks_args(&args(&["rec.json", "--top", "99999"])).is_err(),
            "上界"
        );
        assert!(
            parse_tracks_args(&args(&["--bogus", "rec.json"])).is_err(),
            "未知参数"
        );
        assert!(
            parse_tracks_args(&args(&["a.json", "b.json"])).is_err(),
            "多余文件"
        );
    }

    #[test]
    fn empty_input_is_safe() {
        let m = metrics(&[]);
        assert_eq!(m.count, 0);
        assert!(findings(&m).is_empty());
        assert!(!m.tz_desc.is_empty(), "时区来源始终要能打出来");
    }
}

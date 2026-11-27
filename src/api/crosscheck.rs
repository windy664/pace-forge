//! 跨记录自检：把最近几条记录放一起看，单条自检看不出的问题。
//!
//! 单条记录内部的自检（时间轴、点位命中、分桶、采样节奏）管的是「这条
//! 报文自己圆不圆」；服务端判异常时还会拿多条记录横向比：连续多少天、
//! 起跑时刻是否总在同一分钟、里程/配速离散度、两次轨迹是不是同一条线。
//! 这里只算数、不下结论——阈值得按各校规则和自己的实测定，程序没有立场。
//!
//! 入口是离线文件：`pace-forge crosscheck records.json`。文件可以是 JSON
//! 数组，也可以是 `{"records":[...]}`；每条记录的字段名按常见别名挑，
//! 轨迹可选（没有轨迹就算不了重合度）。

use crate::error::{Error, ErrorKind};
use serde_json::Value;

/// 本地时区偏移（秒），复用 OBS key 与时间戳那一套口径。
pub fn tz_offset_sec() -> i64 {
    crate::api::obs::tz_offset_sec()
}

/// 时区来源说明：设了、设了但解析失败、没设用默认，三种都说清楚。
///
/// 日期和「起跑时刻」的小时都由这个偏移推出来，输出不带来源的话，换个时区
/// 跑同一个文件会得到另一个「事实」，而人看不出是环境变了。
pub fn tz_description() -> String {
    let cfg = crate::config::global();
    let offset = cfg.network.tz_offset_sec;
    let sign = if offset >= 0 { '+' } else { '-' };
    let abs = offset.abs();
    let label = format!("UTC{sign}{:02}:{:02}", abs / 3600, (abs % 3600) / 60);
    // 非法值在配置加载时就报错了，这里只剩「来自哪一层」要交代
    match cfg.sources.get("network.tz_offset_sec") {
        Some(crate::config::Source::Env) => format!("{label}（PACE_TZ_OFFSET_SEC={offset}）"),
        Some(crate::config::Source::Toml) => {
            format!("{label}（config.toml network.tz_offset_sec={offset}）")
        }
        _ => format!("{label}（默认）"),
    }
}

/// 配速格式化：先整体四舍五入到秒，再拆分/秒。
///
/// 「先取模再四舍五入」会打出 `7'60"`（59.6 秒进位却没进位到分）。
pub fn format_pace(sec_per_km: f64) -> String {
    let total = sec_per_km.round() as i64;
    format!("{}'{:02}\"", total / 60, total % 60)
}

/// 起跑时刻距本地零点不足一小时的记录条数。跨零点时日期归属不稳定，
/// 输出里该提一句，别让「2026-09-21」看起来像客观事实。
pub fn near_midnight_count(records: &[RunRecord], tz_offset_sec: i64) -> usize {
    records
        .iter()
        .filter(|r| {
            let secs = (r.start_ms.div_euclid(1000) + tz_offset_sec).rem_euclid(86_400);
            !(3_600..=86_400 - 3_600).contains(&secs)
        })
        .count()
}

/// 本地日期 `(年, 月, 日)`，给命令行打印用。
pub fn local_date(start_ms: i64, tz_offset_sec: i64) -> (i64, u32, u32) {
    let secs = start_ms.div_euclid(1000) + tz_offset_sec;
    let days = local_day(start_ms, tz_offset_sec);
    let (y, m, d, _) = crate::api::obs::civil_from_days(days, secs.rem_euclid(86_400) / 3600);
    (y, m, d)
}

/// 首尾记录覆盖的本地日历天数，包含起止两天。
pub fn calendar_span(first_ms: i64, last_ms: i64, tz_offset_sec: i64) -> i64 {
    local_day(last_ms, tz_offset_sec) - local_day(first_ms, tz_offset_sec) + 1
}

/// 一条记录的最小视图。
#[derive(Debug, Clone)]
pub struct RunRecord {
    /// 起跑时间，毫秒戳
    pub start_ms: i64,
    /// 里程（米）。
    pub distance_m: f64,
    /// 用时（秒）。
    pub duration_sec: i64,
    /// 轨迹（BD-09 / WGS84 都行，只要同一批记录口径一致）
    pub track: Vec<(f64, f64)>,
}

/// 重合度判「经过」的半径：两条路线同一条线时，逐点偏差一般在 10~20m 内。
pub const OVERLAP_RADIUS_M: f64 = 30.0;

fn num(obj: &Value, keys: &[&str]) -> Option<f64> {
    for k in keys {
        let Some(v) = obj.get(*k) else { continue };
        let n = match v {
            Value::Number(n) => n.as_f64(),
            Value::String(s) => s.trim().parse::<f64>().ok(),
            _ => None,
        };
        if n.is_some() {
            return n;
        }
    }
    None
}

fn track_point(v: &Value) -> Option<(f64, f64)> {
    if let Some(pair) = v.as_array() {
        return Some((pair.first()?.as_f64()?, pair.get(1)?.as_f64()?));
    }
    let lat = num(v, &["lat", "gLat", "latitude"])?;
    let lon = num(v, &["lon", "gLng", "lng", "longitude"])?;
    Some((lat, lon))
}

fn parse_track(v: &Value) -> Vec<(f64, f64)> {
    let arr = match v {
        Value::Array(a) => a,
        Value::String(s) => match serde_json::from_str::<Value>(s) {
            Ok(Value::Array(a)) => return parse_track(&Value::Array(a)),
            _ => return Vec::new(),
        },
        _ => return Vec::new(),
    };
    arr.iter().filter_map(track_point).collect()
}

/// 从单条记录/详情里取轨迹：`allLocJson` 字符串、`points`、`track` 都认。
pub fn extract_track(v: &Value) -> Vec<(f64, f64)> {
    v.get("allLocJson")
        .or_else(|| v.get("points"))
        .or_else(|| v.get("track"))
        .map(parse_track)
        .unwrap_or_default()
}

fn record_rows(value: &Value) -> Option<&[Value]> {
    value
        .as_array()
        .or_else(|| {
            value
                .get("records")
                .or_else(|| value.get("list"))?
                .as_array()
        })
        .map(Vec::as_slice)
}

/// 从一段 JSON 里读记录。字段别名按服务端/前端两套都收。
pub fn parse_records(v: &Value) -> Vec<RunRecord> {
    let Some(arr) = record_rows(v) else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|r| {
            let mut start = num(r, &["startTime", "start_time", "startMs", "beginTime"])?;
            // 秒级戳很常见（服务端有的接口给秒），小于 1e11 一律按秒
            if start < 1e11 {
                start *= 1000.0;
            }
            let distance_m = num(r, &["totalDis", "distance", "totalDistance", "dis"])?;
            let duration_sec = num(r, &["totalTime", "duration", "useTime"])
                .and_then(crate::number::f64_to_i64)
                .unwrap_or(0);
            let track = r
                .get("points")
                .or_else(|| r.get("allLocJson"))
                .or_else(|| r.get("track"))
                .map(parse_track)
                .unwrap_or_default();
            Some(RunRecord {
                start_ms: crate::number::f64_to_i64(start).unwrap_or(0),
                distance_m,
                duration_sec,
                track,
            })
        })
        .collect()
}

/// 离线导入结果。行号对应原始文件中记录数组的位置。
pub struct LocalRecordImport {
    /// 能参与统计的记录。
    pub records: Vec<RunRecord>,
    /// 每条记录在原文件里的行号（1 基），与 `records` 一一对应。
    pub row_numbers: Vec<usize>,
    /// 被跳过或降级的记录及原因，带原始行号。
    pub issues: Vec<String>,
}

fn checked_local_track(value: &Value) -> Result<Vec<(f64, f64)>, Error> {
    let decoded;
    let value = if let Some(raw) = value.as_str() {
        decoded = serde_json::from_str::<Value>(raw)
            .map_err(|_| ErrorKind::client("轨迹字符串不是合法 JSON".into()))?;
        &decoded
    } else {
        value
    };
    let points = value
        .as_array()
        .ok_or_else(|| ErrorKind::client("轨迹应为坐标数组".into()))?;
    points
        .iter()
        .enumerate()
        .map(|(index, point)| match track_point(point) {
            Some((lat, lon))
                if (-90.0..=90.0).contains(&lat) && (-180.0..=180.0).contains(&lon) =>
            {
                Ok((lat, lon))
            }
            _ => Err(ErrorKind::client(format!(
                "轨迹第 {} 点的经纬度无效",
                index + 1
            ))),
        })
        .collect()
}

/// 检查导入的记录，记录无法参与统计的原始行号和原因。
pub fn parse_local_records(value: &Value) -> Result<LocalRecordImport, Error> {
    let rows = record_rows(value)
        .ok_or_else(|| ErrorKind::client("需要记录数组，或包含 records/list 数组的对象".into()))?;
    let mut result = LocalRecordImport {
        records: Vec::with_capacity(rows.len()),
        row_numbers: Vec::with_capacity(rows.len()),
        issues: Vec::new(),
    };
    for (index, row) in rows.iter().enumerate() {
        let row_number = index + 1;
        let mut issue = |detail: &str| {
            result.issues.push(format!("第 {row_number} 条：{detail}"));
        };
        let start_ms = num(row, &["startTime", "start_time", "startMs", "beginTime"])
            .map(|start| if start < 1e11 { start * 1000.0 } else { start })
            .filter(|start| (1.0..i64::MAX as f64).contains(start));
        let Some(start_ms) = start_ms else {
            issue("开始时间缺失或无效，跳过此条记录");
            continue;
        };
        let distance_m = num(row, &["totalDis", "distance", "totalDistance", "dis"])
            .filter(|distance| (0.0..i64::MAX as f64).contains(distance));
        let Some(distance_m) = distance_m else {
            issue("里程缺失或无效，跳过此条记录");
            continue;
        };
        let duration_sec = match num(row, &["totalTime", "duration", "useTime"]) {
            Some(duration) if (1.0..i64::MAX as f64).contains(&duration) => {
                crate::number::f64_to_i64(duration).unwrap_or(0)
            }
            _ => {
                issue("用时缺失或无效，此条不参加配速统计");
                0
            }
        };
        let track = row
            .get("points")
            .or_else(|| row.get("allLocJson"))
            .or_else(|| row.get("track"));
        let track = match track
            .filter(|track| !track.is_null())
            .map(checked_local_track)
        {
            Some(Ok(track)) => track,
            Some(Err(detail)) => {
                issue(&format!("{}，此条轨迹不参加统计", detail.message()));
                Vec::new()
            }
            None => Vec::new(),
        };
        result.records.push(RunRecord {
            start_ms: crate::number::f64_to_i64(start_ms).unwrap_or(0),
            distance_m,
            duration_sec,
            track,
        });
        result.row_numbers.push(row_number);
    }
    Ok(result)
}

/// 本地时区下的「天序号」（自 1970-01-01 起）。
fn local_day(start_ms: i64, tz_offset_sec: i64) -> i64 {
    (start_ms.div_euclid(1000) + tz_offset_sec).div_euclid(86_400)
}

/// 连续记录天数：`(最长一段, 截至最后一条的那一段)`
pub fn streak_days(records: &[RunRecord], tz_offset_sec: i64) -> (usize, usize) {
    if records.is_empty() {
        return (0, 0);
    }
    let mut days: Vec<i64> = records
        .iter()
        .map(|r| local_day(r.start_ms, tz_offset_sec))
        .collect();
    days.sort_unstable();
    days.dedup();
    let mut best = 1usize;
    let mut cur = 1usize;
    for w in days.windows(2) {
        if w[1] == w[0] + 1 {
            cur += 1;
            best = best.max(cur);
        } else {
            cur = 1;
        }
    }
    (best, cur)
}

/// 起跑时刻的集中度：`(最多的那个小时, 占比)`。小时按本地时区算。
pub fn start_hour_concentration(records: &[RunRecord], tz_offset_sec: i64) -> (u32, f64) {
    if records.is_empty() {
        return (0, 0.0);
    }
    let mut bins = [0usize; 24];
    for r in records {
        let hour = (r.start_ms.div_euclid(1000) + tz_offset_sec)
            .div_euclid(3600)
            .rem_euclid(24) as usize; // 0..24 by construction
        bins[hour] += 1;
    }
    let (idx, &max) = bins
        .iter()
        .enumerate()
        .max_by_key(|&(_, c)| c)
        .unwrap_or((0, &0));
    (
        u32::try_from(idx).unwrap_or(u32::MAX),
        max as f64 / records.len() as f64,
    )
}

/// 离散度：`(均值, 变异系数)`。变异系数 = 标准差 / 均值。
pub fn dispersion(values: &[f64]) -> (f64, f64) {
    if values.is_empty() {
        return (0.0, 0.0);
    }
    let mean = values.iter().sum::<f64>() / values.len() as f64;
    if mean.abs() < 1e-9 {
        return (mean, 0.0);
    }
    let var = values.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / values.len() as f64;
    (mean, var.sqrt() / mean)
}

/// 点到折线的最短距离（米）。用等距圆柱投影到本地平面，几十米的判据够用。
fn point_to_polyline_m(pt: (f64, f64), line: &[(f64, f64)]) -> f64 {
    if line.is_empty() {
        return f64::INFINITY;
    }
    if line.len() == 1 {
        return crate::track::geo::distance(pt, line[0]);
    }
    let mlat = crate::track::geo::m_per_deg_lat(pt.0);
    let mlng = crate::track::geo::m_per_deg_lng(pt.0);
    let to_xy = |p: (f64, f64)| ((p.1 - pt.1) * mlng, (p.0 - pt.0) * mlat);
    let mut best = f64::INFINITY;
    for w in line.windows(2) {
        let a = to_xy(w[0]);
        let b = to_xy(w[1]);
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let len2 = dx * dx + dy * dy;
        let t = if len2 <= 1e-12 {
            0.0
        } else {
            ((-a.0 * dx - a.1 * dy) / len2).clamp(0.0, 1.0)
        };
        let (px, py) = (a.0 + t * dx, a.1 + t * dy);
        best = best.min((px * px + py * py).sqrt());
    }
    best
}

/// 单向覆盖率：`a` 里有几成点落在 `b` 的 30 米缓冲内。
fn coverage(a: &[(f64, f64)], b: &[(f64, f64)], radius_m: f64) -> f64 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let hit = a
        .iter()
        .filter(|p| point_to_polyline_m(**p, b) <= radius_m)
        .count();
    hit as f64 / a.len() as f64
}

/// 两条轨迹的对称重合度：两个方向取小，避免「一条短轨迹整段落在长轨迹里」被算成 100%。
pub fn track_overlap(a: &[(f64, f64)], b: &[(f64, f64)], radius_m: f64) -> f64 {
    coverage(a, b, radius_m).min(coverage(b, a, radius_m))
}

/// 两两重合度里最高的那一对：`(记录下标 a, 下标 b, 重合度, 参与比较的对数)`
pub fn max_pair_overlap(
    records: &[RunRecord],
    radius_m: f64,
) -> Option<(usize, usize, f64, usize)> {
    let idx: Vec<usize> = (0..records.len())
        .filter(|i| records[*i].track.len() >= 2)
        .collect();
    if idx.len() < 2 {
        return None;
    }
    let mut best = (idx[0], idx[1], 0.0f64);
    let mut pairs = 0usize;
    for i in 0..idx.len() {
        for j in (i + 1)..idx.len() {
            pairs += 1;
            let ov = track_overlap(&records[idx[i]].track, &records[idx[j]].track, radius_m);
            if ov > best.2 {
                best = (idx[i], idx[j], ov);
            }
        }
    }
    Some((best.0, best.1, best.2, pairs))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 59.6 秒要进位成 `1'00"`，不是 `0'60"`。
    #[test]
    fn pace_rounds_whole_seconds_first() {
        assert_eq!(format_pace(479.6), "8'00\"");
        assert_eq!(format_pace(480.4), "8'00\"");
        assert_eq!(format_pace(383.0), "6'23\"");
    }

    /// 跨零点前后一小时都算「贴近零点」。
    #[test]
    fn near_midnight_window() {
        let r = |ms: i64| RunRecord {
            start_ms: ms,
            distance_m: 0.0,
            duration_sec: 0,
            track: vec![],
        };
        // 2026-09-21 23:40 +08:00 = 15:40Z
        let late = 1_790_005_200_000_i64;
        let recs = vec![r(late), r(late - 86_400_000), r(late + 3 * 3_600_000)];
        assert_eq!(near_midnight_count(&recs, 28_800), 2);
        assert_eq!(near_midnight_count(&recs[..1], 0), 0);
    }

    // ---- merged from former `mod tests` ----

    /// 配置映射等价性：tz_offset 从配置层来，描述行带来源标注。
    /// Guard 手工构造的 Config 不带来源 → 显示「默认」；注入来源分支在
    /// pace-forge-config 的加载测试里覆盖。
    #[test]
    fn tz_offset_comes_from_config() {
        assert_eq!(tz_offset_sec(), 8 * 3600, "默认东八区");
        assert!(tz_description().contains("默认"), "{}", tz_description());
        let _g = crate::config::Guard::replace(crate::config::Config {
            network: crate::config::Network {
                tz_offset_sec: 0,
                ..Default::default()
            },
            ..Default::default()
        });
        assert_eq!(tz_offset_sec(), 0);
        let d = tz_description();
        assert!(d.starts_with("UTC+00:00"), "{d}");
    }

    fn rec(day: i64, hour: i64, dist: f64, dur: i64) -> RunRecord {
        RunRecord {
            start_ms: ((day * 86_400 + hour * 3_600) * 1000) - 8 * 3_600 * 1000,
            distance_m: dist,
            duration_sec: dur,
            track: Vec::new(),
        }
    }

    /// 沿纬线造一条直线轨迹，`offset_m` 是两条线之间的纬度偏移。
    fn line(offset_m: f64, n: usize) -> Vec<(f64, f64)> {
        (0..n)
            .map(|i| (offset_m / 110_000.0, i as f64 * 30.0 / 110_000.0))
            .collect()
    }

    #[test]
    fn streak_counts_dedup_and_gap() {
        let tz = 8 * 3_600;
        // 第 0/1/2 天连续，跳一天，第 4/5 天连续
        let records = vec![
            rec(0, 7, 2000.0, 700),
            rec(1, 7, 2100.0, 720),
            rec(2, 7, 1900.0, 660),
            rec(4, 7, 2000.0, 700),
            rec(5, 7, 2000.0, 700),
        ];
        assert_eq!(streak_days(&records, tz), (3, 2), "最长 3 天、当前 2 天");
        // 同一天两条记录只算一天
        let same_day = vec![rec(3, 7, 2000.0, 700), rec(3, 8, 2000.0, 700)];
        assert_eq!(streak_days(&same_day, tz), (1, 1));
        assert_eq!(streak_days(&[], tz), (0, 0));
    }

    #[test]
    fn start_hour_concentration_reports_share() {
        let tz = 8 * 3_600;
        let records = vec![
            rec(0, 7, 2000.0, 700),
            rec(1, 7, 2000.0, 700),
            rec(2, 18, 2000.0, 700),
            rec(3, 7, 2000.0, 700),
        ];
        let (hour, share) = start_hour_concentration(&records, tz);
        assert_eq!(hour, 7);
        assert!((share - 0.75).abs() < 1e-9);
    }

    #[test]
    fn dispersion_is_relative_to_mean() {
        let (mean, cv) = dispersion(&[1900.0, 2000.0, 2100.0]);
        assert!((mean - 2000.0).abs() < 1e-9);
        // 标准差 81.65 / 2000 ≈ 4.08%
        assert!((cv - 0.0408).abs() < 0.001);
        assert_eq!(dispersion(&[]), (0.0, 0.0));
    }

    #[test]
    fn overlap_identical_is_full_and_shifted_is_zero() {
        let a = line(0.0, 40);
        assert!((track_overlap(&a, &a, OVERLAP_RADIUS_M) - 1.0).abs() < 1e-9);
        // 平移 200m，30m 缓冲内没有点
        let far = line(200.0, 40);
        assert!(track_overlap(&a, &far, OVERLAP_RADIUS_M) < 0.05);
        // 平移 10m 仍算同一条路线
        let near = line(10.0, 40);
        assert!(track_overlap(&a, &near, OVERLAP_RADIUS_M) > 0.95);
    }

    #[test]
    fn max_pair_overlap_finds_the_clone() {
        let mut r = Vec::new();
        for i in 0..3 {
            let mut t = line((i as f64) * 800.0, 40);
            if i == 1 {
                t = line(0.0, 40); // 2 号和 1 号是同一条线
            }
            r.push(RunRecord {
                start_ms: 0,
                distance_m: 1000.0,
                duration_sec: 300,
                track: t,
            });
        }
        let (a, b, ov, pairs) = max_pair_overlap(&r, OVERLAP_RADIUS_M).unwrap();
        assert_eq!((a, b), (0, 1), "应指向重合的那一对");
        assert!(ov > 0.95);
        assert_eq!(pairs, 3);
        // 只有一条带轨迹时算不了
        let one = vec![RunRecord {
            start_ms: 0,
            distance_m: 1.0,
            duration_sec: 1,
            track: line(0.0, 5),
        }];
        assert!(max_pair_overlap(&one, OVERLAP_RADIUS_M).is_none());
    }

    #[test]
    fn extract_track_reads_detail_shapes() {
        let v: Value = serde_json::from_str(
            r#"{"allLocJson":"[{\"lat\":47.38,\"lon\":103.95},{\"lat\":47.381,\"lon\":103.951}]"}"#,
        )
        .unwrap();
        assert_eq!(extract_track(&v).len(), 2);
        let p: Value =
            serde_json::from_str(r#"{"points":[[47.38,103.95],[47.381,103.951]]}"#).unwrap();
        assert_eq!(extract_track(&p).len(), 2);
        let none: Value = serde_json::from_str(r#"{"summary":"no track"}"#).unwrap();
        assert!(extract_track(&none).is_empty());
    }

    #[test]
    fn parse_accepts_seconds_and_track_strings() {
        let v: Value = serde_json::from_str(
            r#"[{"startTime":1790913882,"totalDis":1683.83,"totalTime":561,
                "points":"[{\"lat\":47.38,\"lon\":103.95}]"},
               {"beginTime":1790913882000,"distance":2000,"duration":700}]"#,
        )
        .unwrap();
        let r = parse_records(&v);
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].start_ms, 1_790_913_882_000, "秒级戳要乘 1000");
        assert!((r[0].distance_m - 1683.83).abs() < 1e-6);
        assert_eq!(r[0].track.len(), 1);
        assert_eq!(r[1].start_ms, 1_790_913_882_000);
        assert!(r[1].track.is_empty());
    }

    #[test]
    fn local_import_reports_original_row_numbers_and_invalid_coordinates() {
        let input = serde_json::json!({
            "records": [
                {"startTime": 1_790_913_882, "distance": 2000, "duration": 700},
                {"startTime": "NaN", "distance": 2000, "duration": 700},
                {"startTime": 1_790_913_882_000_i64, "distance": -2, "duration": 700},
                {
                    "startTime": 1_790_913_882_000_i64,
                    "distance": 2300,
                    "duration": "inf",
                    "points": [[31.2, 118.7], [91.0, 118.7]]
                },
                {"startTime": 1_790_913_882_000_i64, "distance": "inf", "duration": 700}
            ]
        });

        let imported = parse_local_records(&input).unwrap();
        assert_eq!(imported.row_numbers, [1, 4]);
        assert_eq!(imported.records[0].start_ms, 1_790_913_882_000);
        assert_eq!(imported.records[1].duration_sec, 0);
        assert!(imported.records[1].track.is_empty());
        assert_eq!(imported.issues.len(), 5);
        assert!(imported.issues[0].starts_with("第 2 条"));
        assert!(imported.issues[1].starts_with("第 3 条"));
        assert!(imported.issues[2].contains("第 4 条：用时"));
        assert!(
            imported.issues[3].contains("第 4 条") && imported.issues[3].contains("轨迹第 2 点")
        );
        assert!(imported.issues[4].starts_with("第 5 条"));
    }

    #[test]
    fn calendar_span_counts_local_midnight_instead_of_elapsed_days() {
        let tz = 8 * 3_600;
        let first = (16 * 3_600 - 60) * 1000;
        let last = (16 * 3_600 + 60) * 1000;
        assert_eq!(calendar_span(first, last, tz), 2);
        assert_eq!(calendar_span(first, last, 0), 1);
        assert_eq!(local_date(first, tz), (1970, 1, 1));
        assert_eq!(local_date(last, tz), (1970, 1, 2));
    }

    #[test]
    fn start_hour_buckets_include_times_before_the_local_unix_epoch() {
        let record = RunRecord {
            start_ms: 1,
            distance_m: 2000.0,
            duration_sec: 700,
            track: Vec::new(),
        };
        assert_eq!(start_hour_concentration(&[record], -8 * 3_600), (16, 1.0));
    }
}

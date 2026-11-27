//! 跑步策略：`POST /api/v70103/runModePolicy`。
//!
//! 提交成绩前必须先拉这个接口，它给出三样必需的东西：
//!
//! - `policy`：策略串，提交时原样回传，服务端用它校验"你是什么时候拉的规则"
//! - `minDistance`：本次有效里程的下限，`selDistance` 不能低于它
//! - `mustPoints`：必经打卡点（BD-09）
//!
//! `runMode` 区分项目：1=校园跑（有 11016 时段限制），2=户外跑，3=室外锻炼。

use serde_json::{Value, json};

use super::client::{Client, expect_ok};
use super::coord::gcj02_to_bd09;
use crate::crypto::consts::hosts;
use crate::error::{Error, ErrorKind};

/// 规则接口路径。
pub const POLICY_PATH: &str = "/api/v70103/runModePolicy";

/// 策略信息。
#[derive(Debug, Clone, Default)]
pub struct PolicyInfo {
    /// 服务端时间戳（毫秒），提交时要带上做时间对齐。
    pub timestamp: i64,
    /// 策略串。
    pub policy: i64,
    /// 有效里程下限（米）。
    pub min_distance: i64,
    /// 规则有效时长（秒）。
    pub valid_time: i64,
    /// 必经点（BD-09）。
    pub must_points: Vec<(f64, f64)>,
    /// 允许的配速区间（分/公里，小=快）。服务端 `speedTop`/`speedBottom`。
    ///
    /// 之前这两个值被 `let _ =` 直接丢掉，导致 retime 用硬编码 1.0~8.0 m/s
    /// （相当于 2:05~16:40 配速），preflight 的配速检查因为拿到 None 永远
    /// 不执行——自检报「全部通过」，发出去却是 2 分速，必驳回。
    pub pace_range: Option<(f64, f64)>,
    /// 步频上下限（步/分钟）。服务端 `stepTop`/`stepBottom`。
    ///
    /// 之前完全没解析，simulate 按自己的 biomech 包络生成步频，服务端
    /// 却按它下发的区间判。实测下发值是 60~250，比 preflight 里硬编码的
    /// [100,230] 宽得多——拿着一个更窄的假区间做自检，等于用错的尺子
    /// 量自己。
    pub step_range: Option<(i64, i64)>,
    /// 最短有效时长（秒）。服务端 `minRunTime`。
    pub min_run_time: i64,
    /// 每日有效里程上限（米）。服务端 `runRuleModel.dayGoal`；0 = 未下发。
    pub daily_cap_m: i64,
    /// 跑步冻结截止时间（毫秒戳）。`0` = 未冻结。
    ///
    /// 服务端在 `runRuleModel.freezeRunTime` 里下发；> 当前时间说明账号处于
    /// 「跑步冻结」期，App 里连开始按钮都是倒计时的。冻结期提交只会往违规
    /// 名单里再加一条。依据：Nyx4 探测报告 §10.2（`runModePolicy.freezeRunTime`）。
    pub freeze_run_time: i64,
}

/// 从 policy 响应里取字段，兼容 `data` 包一层和直接平铺两种情况。
fn get_field<'a>(v: &'a Value, key: &str) -> Option<&'a Value> {
    if let Some(inner) = v.get("data")
        && inner.get(key).is_some()
    {
        return inner.get(key);
    }
    v.get(key)
}

/// 解析 policy 响应。
pub fn parse_policy_response(biz: &Value) -> Result<PolicyInfo, Error> {
    let timestamp = get_field(biz, "timestamp")
        .and_then(|t| t.as_i64())
        .ok_or_else(|| ErrorKind::protocol("policy 响应缺 timestamp".into()))?;
    let policy = get_field(biz, "policy")
        .and_then(|t| t.as_i64())
        .unwrap_or(0);
    let rule = get_field(biz, "runRuleModel")
        .cloned()
        .unwrap_or(Value::Null);

    let min_distance = rule
        .get("minDistance")
        .and_then(|t| t.as_i64())
        .unwrap_or(1000);
    let valid_time = rule.get("validTime").and_then(|t| t.as_i64()).unwrap_or(0);
    // 配速区间：speedTop/speedBottom 的单位是「分/公里」。
    // 个别策略把它们放成秒或 m/s，按数量级判断：正常的分钟配速在 2~20 之间。
    let pace_range = parse_pace_range(&rule);

    Ok(PolicyInfo {
        timestamp,
        policy,
        min_distance,
        valid_time,
        must_points: extract_must_points(biz),
        pace_range,
        step_range: parse_steo_range(&rule),
        min_run_time: rule.get("minRunTime").and_then(|t| t.as_i64()).unwrap_or(0),
        daily_cap_m: parse_daily_cap(&rule),
        freeze_run_time: rule
            .get("freezeRunTime")
            .and_then(|t| t.as_i64())
            // 有的版本放在顶层
            .or_else(|| get_field(biz, "freezeRunTime").and_then(|t| t.as_i64()))
            .unwrap_or(0),
    })
}

/// 步频区间。
///
/// 两个值都可能是整数或浮点，也见过倒序下发的，统一成 (小, 大)。
/// 每日有效里程上限（米）。服务端 `runRuleModel.dayGoal`，读不到为 0。
///
/// 老代码在 flow 的配额检查里把 5000 写死，实测该校区 dayGoal=3000：
/// 超过 3km 的部分服务端根本不计有效里程，程序却放行，白跑还以为在攒进度。
fn parse_daily_cap(rule: &Value) -> i64 {
    for key in ["dayGoal", "maleDayUpper", "femaleDayUpper"] {
        if let Some(v) = rule.get(key).and_then(num_of)
            && v > 0.0
        {
            return crate::number::round_to_i64(v).unwrap_or(0);
        }
    }
    0
}

/// 每日上限默认值：服务端 `dayGoal` 取不到时退回改之前的旧常量。
pub const DEFAULT_DAILY_CAP_M: i64 = 5000;

/// 取每日上限（米），附带来源说明。
///
/// `runMode=1` 的 policy 在有效时段外会回 11016（实测凌晨就是），
/// 但 `dayGoal` 各模式一致，所以主模式失败时用 `runMode=2` 兜一次。
pub fn daily_cap_m(client: &Client, run_mode: i64) -> (i64, String) {
    let mut modes = vec![run_mode];
    if run_mode != 2 {
        modes.push(2);
    }
    for mode in modes {
        if let Ok(p) = fetch_policy_for(client, mode)
            && p.daily_cap_m > 0
        {
            return (p.daily_cap_m, format!("runMode={mode} dayGoal"));
        }
    }
    (
        DEFAULT_DAILY_CAP_M,
        "服务端 dayGoal 未取到，退回默认".to_string(),
    )
}

fn parse_steo_range(rule: &Value) -> Option<(i64, i64)> {
    let top = rule.get("stepTop").and_then(|v| v.as_f64())?;
    let bottom = rule.get("stepBottom").and_then(|v| v.as_f64())?;
    let (a, b) = (
        crate::number::round_to_i64(bottom)?,
        crate::number::round_to_i64(top)?,
    );
    if a <= 0 || b <= 0 {
        return None;
    }
    Some(if a <= b { (a, b) } else { (b, a) })
}

/// 从规则里取配速区间，统一换算成「分/公里」。
///
/// 单位有三种可能：分/公里（常见，2~20）、秒/公里（120~1200）、m/s（1~10）。
/// 按数值量级判定，避免把 6.0 分/km 当成 6 m/s 用。
fn parse_pace_range(rule: &Value) -> Option<(f64, f64)> {
    let top = rule.get("speedTop").and_then(num_of);
    let bottom = rule.get("speedBottom").and_then(num_of);
    let (a, b) = (top?, bottom?);
    if a <= 0.0 || b <= 0.0 {
        return None;
    }
    let to_min_per_km = |v: f64| -> f64 {
        if v < 25.0 {
            // 分/公里 或 m/s：4.0 分/km ≈ 4.17 m/s，两者数量级接近，
            // 但配速不会低于 2 分/km（=30 km/h），所以 <25 一律当分/公里
            v
        } else if v < 1500.0 {
            // 秒/公里
            v / 60.0
        } else {
            // m/s → 分/公里
            1000.0 / v / 60.0
        }
    };
    Some((to_min_per_km(a), to_min_per_km(b)))
}

fn num_of(v: &Value) -> Option<f64> {
    v.as_f64()
        .or_else(|| v.as_i64().map(|x| x as f64))
        .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
}

/// 从响应里防御式提取必经点。
///
/// 字段名不确定，逐个试。坐标优先 `lat/lon`（BD-09），缺失或全 0 时回退
/// `glat/glon`（GCJ-02）——两端点都实测出现过「只填 GCJ、BD 置 0」的情况。
pub fn extract_must_points(v: &Value) -> Vec<(f64, f64)> {
    let data = v.get("data").unwrap_or(v);
    for name in [
        "pointList",
        "runPointList",
        "mustPointList",
        "passPointList",
        "mustPoints",
        "passPoints",
        "runPoints",
        "points",
        "nodeList",
        "checkPointList",
    ] {
        let Some(arr) = data.get(name).and_then(|x| x.as_array()) else {
            continue;
        };
        let pts: Vec<(f64, f64)> = arr.iter().filter_map(point_xy).collect();
        if !pts.is_empty() {
            return pts;
        }
    }
    Vec::new()
}

/// 从响应里取一个点的 BD-09 坐标。
/// 取点位的 GCJ-02 坐标。
///
/// 服务端点位同时给了 BD-09（lat/lon）和 GCJ-02（glat/glon）。高德地图
/// 用的是 GCJ-02 网格，前端要按这套坐标摆放才和瓦片对齐，所以单独取一份。
pub fn point_gcj(p: &Value) -> Option<(f64, f64)> {
    let (ga, go) = (field_num(p, &["glat"]), field_num(p, &["glon"]));
    match (ga, go) {
        (Some(a), Some(o)) if !(a == 0.0 && o == 0.0) => Some((a, o)),
        _ => {
            // 没有 GCJ 就从 BD 反推，保证前端永远拿得到一套能用的坐标
            let bd = point_xy(p)?;
            Some(super::coord::bd09_to_gcj02(bd.0, bd.1))
        }
    }
}

/// 从点位 JSON 里取 `(lat, lon)`，字段名兼容 `latitude` 等写法。
pub fn point_xy(p: &Value) -> Option<(f64, f64)> {
    let lat = field_num(p, &["lat", "latitude"]);
    let lon = field_num(p, &["lon", "lng", "longitude"]);
    match (lat, lon) {
        (Some(a), Some(o)) if !(a == 0.0 && o == 0.0) => Some((a, o)),
        _ => {
            let (ga, go) = (field_num(p, &["glat"]), field_num(p, &["glon"]));
            match (ga, go) {
                (Some(a), Some(o)) if !(a == 0.0 && o == 0.0) => Some(gcj02_to_bd09(a, o)),
                _ => None,
            }
        }
    }
}

fn field_num(p: &Value, keys: &[&str]) -> Option<f64> {
    keys.iter().find_map(|k| p.get(*k).and_then(|v| v.as_f64()))
}

/// 拉策略。
///
/// body：`{"runMode":1,"ruleUpdateTime":0,"geoFenceUpdateTime":0,"selectUnid":<unid>,"operateType":0}`
pub fn fetch_policy_for(client: &Client, run_mode: i64) -> Result<PolicyInfo, Error> {
    let select_unid = client
        .credentials()
        .and_then(|c| c.unid.as_ref())
        .and_then(|u| u.parse::<i64>().ok())
        .unwrap_or(0);

    let body = json!({
        "runMode": run_mode,
        "ruleUpdateTime": 0,
        "geoFenceUpdateTime": 0,
        "selectUnid": select_unid,
        "operateType": 0,
    });

    let raw = client.request(
        "POST",
        &format!("{}{}", hosts::RUN, POLICY_PATH),
        Some(&body),
    )?;
    let v = expect_ok(&raw)?;
    // 策略响应里可能有点位判定相关的字段，我们只解析了四个。
    // 把原始 JSON 落盘，逐字段比对 runMode=1 与 runMode=2 的差异。
    dump_policy_in(dump_dir().as_deref(), run_mode, &v);
    parse_policy_response(&v)
}

/// 策略调试转储目录。
///
/// **默认不落盘**（返回 `None`）：只有显式配置 `run.dump_policy`
/// （env `PACE_DUMP_POLICY`）=<目录> 才写。策略响应里有考生身份相关字段，
/// 不该无声无息地堆进 `$HOME`；也正因为默认关闭，跑测试不可能往真实
/// `$HOME` 里多写一个文件。
pub fn dump_dir() -> Option<std::path::PathBuf> {
    // 原文直用（不 trim）：与旧 `var_os` 语义一致
    let cfg = crate::config::global();
    let dir = &cfg.run.dump_policy;
    (!dir.is_empty()).then(|| std::path::PathBuf::from(dir))
}

/// 策略转储文件路径：`<dir>/policy-runmode<runMode>.json`。
///
/// **纯路径计算，不碰磁盘。** 测试注入一次性临时目录用。
pub(crate) fn dump_policy_path_in(dir: &std::path::Path, run_mode: i64) -> std::path::PathBuf {
    dir.join(format!("policy-runmode{run_mode}.json"))
}

/// 落一份策略原文到 `dir`（`None` = 不落盘）。
///
/// 走 `session::write_file_private`：目录不存在时自己建（`PACE_DUMP_POLICY`
/// 指向一个还不存在的目录时，早先的 `fs::write` 会静默失败），权限 0600。
fn dump_policy_in(dir: Option<&std::path::Path>, run_mode: i64, v: &Value) {
    let Some(dir) = dir else {
        return;
    };
    let path = dump_policy_path_in(dir, run_mode);
    let body = serde_json::to_string_pretty(v).unwrap_or_default();
    let _ = super::session::write_file_private(&path, body.as_bytes());
}

/// 拿不到策略时的兜底值。
///
/// `runModePolicy` 会按**服务器当前时间**判时段，记分跑一旦不在窗口内
/// 就直接回 11016，连策略串都拿不到。但提交报文里只需要 `policy` 这个
/// 字符串（实测恒为 "0"）外加 `minDistance` / `validTime` 两项阈值，
/// `timestamp` 根本没进签名——所以拉不到策略不代表不能提交。
///
/// 这里给一份保守默认值：minDistance 用服务端最常见的 1500，validTime
/// 取户外跑实测的 06:00~21:30 窗口长度。`must_points` 留空，因为点位
/// 走的是独立的 `/api/points` 接口，不依赖策略。
pub fn fallback_policy() -> PolicyInfo {
    PolicyInfo {
        timestamp: crate::crypto::envelope::now_ms(),
        policy: 0,
        min_distance: 1500,
        valid_time: 55_800,
        must_points: Vec::new(),
        pace_range: None,
        // 兜底按实测下发值：记分跑 stepTop=250 / stepBottom=60
        step_range: Some((60, 250)),
        min_run_time: 0,
        daily_cap_m: 0,
        freeze_run_time: 0,
    }
}

/// 校级考勤配置里的有效时段（本地时区的 `HH:MM:SS` 起止）。
///
/// `GET /api/v70100/run/getHistoryConfig` 返回里 `randomValidTimes` 是数组
/// （字段名就叫 random，不是笔误）。实测某校 `06:30:00 – 22:00:59`。
/// 拿不到就返回 `None`，调用方按「不判断」处理——不能因为一个只读接口挂了
/// 就让人跑不了步。
pub fn valid_time_window(client: &Client) -> Option<(String, String)> {
    let raw = client
        .request(
            "GET",
            &format!("{}/api/v70100/run/getHistoryConfig", hosts::DISCOVERY),
            None,
        )
        .ok()?;
    let v = expect_ok(&raw).ok()?;
    let arr = v
        .get("data")
        .and_then(|d| d.get("randomValidTimes"))
        .or_else(|| v.get("randomValidTimes"))
        .and_then(|x| x.as_array())?;
    let first = arr.first()?;
    let start = first.get("startTime").and_then(|x| x.as_str())?.to_string();
    let end = first.get("endTime").and_then(|x| x.as_str())?.to_string();
    Some((start, end))
}

/// 当前本地时间是否落在 `HH:MM:SS` 区间内（闭区间，跨零点不支持）。
pub fn in_window(now_hms: &str, start: &str, end: &str) -> bool {
    now_hms >= start && now_hms <= end
}

/// 本月是否被抽中「信用分核验」。
///
/// 服务端每月随机抽检，命中时 App 会弹支付宝芝麻信用的核验页；不完成核验
/// 那条记录的成绩会判不达标。这里是只读查询（`taskType=1` 是「跑步」域），
/// 只用来提前提醒——自动化没法替真人做核验。
///
/// 依据：Nyx4 探测报告 §四.1（「每月随机核验」= 支付宝信用分，接口
/// `creditscore/v70390/popup`，返回 `{needPopup: bool}`）。
pub fn monthly_verify_needed(client: &Client) -> Result<bool, Error> {
    let raw = client.request(
        "GET",
        &format!("{}/api/creditscore/v70390/popup?taskType=1", hosts::RUN),
        None,
    )?;
    let v = expect_ok(&raw)?;
    Ok(v.get("data")
        .and_then(|d| d.get("needPopup"))
        .and_then(|x| x.as_bool())
        .unwrap_or(false))
}

/// 默认按校园跑拉策略。
pub fn fetch_policy(client: &Client) -> Result<PolicyInfo, Error> {
    fetch_policy_for(client, 1)
}

#[cfg(test)]
mod tests {
    use super::in_window;
    use super::*;

    #[test]
    fn window_is_closed_interval() {
        let (s, e) = ("06:30:00", "22:00:59");
        assert!(in_window("06:30:00", s, e), "起点算在窗口内");
        assert!(in_window("22:00:59", s, e), "终点算在窗口内");
        assert!(in_window("12:00:00", s, e));
        assert!(!in_window("06:29:59", s, e), "早于窗口不算");
        assert!(!in_window("22:01:00", s, e), "晚于窗口不算");
        assert!(!in_window("03:00:00", s, e));
    }

    // ---- merged from former `mod tests` ----

    #[test]
    fn parses_daily_cap_from_real_payload() {
        // 实测某校 runMode=2 的真实下发（dayGoal 是每日上限，不是目标）。
        let v = json!({
            "data": {
                "timestamp": 1_795_630_760_067i64,
                "policy": 0,
                "runRuleModel": {
                    "dayGoal": 3000.0,
                    "minDistance": 1500,
                    "speedTop": 2.0,
                    "speedBottom": 12.0,
                    "stepTop": 300,
                    "stepBottom": 40
                }
            }
        });
        let p = parse_policy_response(&v).unwrap();
        assert_eq!(p.daily_cap_m, 3000, "dayGoal 就是每日有效里程上限");

        // 没有 dayGoal 的老版本退回 male/female 的日上限
        let old = json!({ "data": { "timestamp": 1, "runRuleModel": { "maleDayUpper": 5000.0 } } });
        assert_eq!(parse_policy_response(&old).unwrap().daily_cap_m, 5000);
        // 全缺 = 未知（0），由调用方决定默认
        let none = json!({ "data": { "timestamp": 1, "runRuleModel": {} } });
        assert_eq!(parse_policy_response(&none).unwrap().daily_cap_m, 0);
    }

    #[test]
    fn parses_nested_and_flat_shapes() {
        // 嵌套在 data 里
        let nested = json!({
            "data": {
                "timestamp": 1_700_000_000_123i64,
                "policy": 1,
                "runRuleModel": { "minDistance": 2000, "validTime": 86400 }
            }
        });
        let p = parse_policy_response(&nested).unwrap();
        assert_eq!(p.timestamp, 1_700_000_000_123);
        assert_eq!(p.policy, 1);
        assert_eq!(p.min_distance, 2000);
        assert_eq!(p.valid_time, 86400);

        // 平铺
        let flat = json!({ "timestamp": 42, "policy": 3 });
        let p2 = parse_policy_response(&flat).unwrap();
        assert_eq!(p2.timestamp, 42);
        assert_eq!(p2.min_distance, 1000, "缺字段时用默认值");
    }

    #[test]
    fn extracts_must_points_from_various_keys() {
        let v = json!({
            "data": {
                "pointList": [
                    {"lat": 30.5, "lon": 104.0},
                    {"lat": 30.6, "lon": 104.1}
                ]
            }
        });
        let pts = extract_must_points(&v);
        assert_eq!(pts.len(), 2);
        assert!((pts[0].0 - 30.5).abs() < 1e-9);
    }

    #[test]
    fn falls_back_to_gcj_when_bd_is_zero() {
        let v = json!({
            "data": {
                "pointList": [
                    {"lat": 0, "lon": 0, "glat": 30.5, "glon": 104.0}
                ]
            }
        });
        let pts = extract_must_points(&v);
        assert_eq!(pts.len(), 1);
        // 转换后应该偏离原值（BD 相对 GCJ 有偏移）
        assert!((pts[0].0 - 30.5).abs() > 1e-6);
    }

    #[test]
    fn missing_timestamp_is_error() {
        assert!(parse_policy_response(&json!({"policy": 1})).is_err());
    }

    /// 护栏：策略转储路径的注入版**绝不能**落到真实 `$HOME`。
    ///
    /// 模式与 `session::tests::test_data_dir_is_isolated_from_real_home` 一致。
    /// 策略响应含考生身份字段，默认根本不落盘；这一条同时锁住「默认关闭」
    /// 这个前提——谁哪天给 `dump_dir()` 加了个 `$HOME/...` 兜底，测试立刻炸。
    #[test]
    fn policy_dump_path_is_isolated_from_real_home() {
        use crate::test_support::{TestDir, assert_isolated};

        let t = TestDir::new("policy-dump");
        let injected = dump_policy_path_in(&t.data(), 1);
        assert_isolated(injected.parent().unwrap(), "策略转储目录");
        assert_ne!(
            injected,
            dump_policy_path_in(&crate::test_support::real_data_dir(), 1),
            "注入路径不能等于真实 $HOME/.pace-forge 下的路径"
        );
        assert_eq!(
            injected.file_name().unwrap().to_string_lossy(),
            "policy-runmode1.json"
        );

        // 默认（没有任何配置来源）**不落盘**；并行的 Guard 测试只动别的键
        assert_eq!(
            dump_dir(),
            None,
            "默认不该有转储目录，否则测试会写真实 HOME"
        );
    }

    /// 转储只在显式给定目录时写，且目录不存在时自己建、权限 600。
    #[test]
    fn dump_policy_writes_only_with_injected_dir() {
        use crate::test_support::TestDir;

        let t = TestDir::new("policy-write");
        let dump = t.home().join("dumps");
        assert!(!dump.exists(), "转储目录还不存在，正好验证首次创建");

        let v = json!({"timestamp": 1, "policy": 0});
        dump_policy_in(Some(&dump), 2, &v);

        let path = dump_policy_path_in(&dump, 2);
        assert!(path.exists(), "应写出 {}", path.display());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "转储文件权限必须是 600");
        }

        // None = 完全不落盘（默认行为）
        let before = std::fs::read_dir(&dump).unwrap().count();
        dump_policy_in(None, 3, &v);
        assert_eq!(
            std::fs::read_dir(&dump).unwrap().count(),
            before,
            "无转储目录时不该写任何文件"
        );
    }

    // ---- merged from former `mod pace_range_tests` ----

    fn rule(top: Value, bottom: Value) -> Value {
        serde_json::json!({"speedTop": top, "speedBottom": bottom})
    }

    #[test]
    fn parses_minutes_per_km() {
        // 学生账号常见写法：3~10 分/公里
        let r = parse_pace_range(&rule(serde_json::json!(3.0), serde_json::json!(10.0)));
        assert_eq!(r, Some((3.0, 10.0)));
    }

    #[test]
    fn parses_seconds_per_km() {
        // 180 秒 = 3 分，600 秒 = 10 分
        let r = parse_pace_range(&rule(serde_json::json!(180), serde_json::json!(600)));
        assert_eq!(r, Some((3.0, 10.0)));
    }

    #[test]
    fn accepts_string_numbers() {
        let r = parse_pace_range(&rule(serde_json::json!("4.5"), serde_json::json!("9.0")));
        assert_eq!(r, Some((4.5, 9.0)));
    }

    #[test]
    fn missing_or_zero_is_none() {
        assert_eq!(parse_pace_range(&rule(Value::Null, Value::Null)), None);
        assert_eq!(
            parse_pace_range(&rule(serde_json::json!(0), serde_json::json!(10))),
            None
        );
        assert_eq!(parse_pace_range(&serde_json::json!({})), None);
    }

    /// 配速区间必须驱动 preflight 的 PACE_OUT 检查——之前它是 None，永远不跑。
    #[test]
    fn pace_out_of_range_is_fatal() {
        // 必须给真实轨迹点：points 为空时 inspect 会提前 return，
        // 后面的配速检查根本不跑（这也是它一直「通过」的原因之一）。
        let pts: Vec<crate::track::TrackPoint> = (0..200)
            .map(|i| crate::track::TrackPoint {
                lat: 31.2304 + i as f64 * 10.0 / 111_320.0,
                lon: 101.4737,
                time_ms: 1_790_000_000_000 + i as i64 * 4000,
            })
            .collect();
        // 2000m / 800s = 2.5 m/s = 6.67 分/km
        let mut c = crate::track::Check {
            points: &pts,
            sim: &crate::track::SimResult {
                speeds: vec![],
                cadences: vec![],
                strides: vec![],
                total_steps: 1600,
                avg_cadence: 160.0,
                speed_buckets: vec![],
                step_buckets: vec![],
                actual_length_m: 2000.0,
            },
            params: &crate::track::SimParams::default(),
            checkpoints: &[],
            total_distance_m: 2000,
            total_time_sec: 800,
            total_steps: 1600,
            step_range: None,
            start_time_ms: 1_790_000_000_000,
            stop_time_ms: 1_790_000_800_000,
            pace_range: Some((3.0, 5.0)),
            expected_interval_sec: None,
        };
        let r = crate::track::inspect(&c);
        let has = r.issues.iter().any(|i| i.code == "PACE_OUT");
        assert!(has, "6.67 分/km 超出 3~5 区间，必须报 PACE_OUT");

        c.pace_range = Some((6.0, 8.0));
        let r2 = crate::track::inspect(&c);
        assert!(
            !r2.issues.iter().any(|i| i.code == "PACE_OUT"),
            "区间放宽到 6~8 后不该再报"
        );
    }
}

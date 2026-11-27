//! 加载：定位文件 → TOML 层 → env 覆盖层 → 交叉校验 → 结构体。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::model::{Config, DEFAULT_APP_VERSION, FixedPoint, HwHeaders, PtScheme};
use crate::registry::{TABLE, find_entry};
use crate::value::{Source, Sources, Value};

/// 配置文件名；默认位置是 `<数据目录>/config.toml`。
pub const CONFIG_FILE_NAME: &str = "config.toml";

/// 加载配置。`explicit` 来自 `--config <路径>` 或 `PACE_CONFIG`。
///
/// 没有找到配置文件时返回全默认（这是正常情况，不是错误）；
/// env 层无论如何都生效。
pub fn load(explicit: Option<&str>) -> Result<Config, String> {
    load_from(explicit, &real_env)
}

fn real_env(name: &str) -> Option<String> {
    // 空串过滤在 load_from 的 env 通道里统一做；这里只做 UTF-8 检查
    std::env::var(name).ok()
}

pub(crate) fn load_from(
    explicit: Option<&str>,
    env: &dyn Fn(&str) -> Option<String>,
) -> Result<Config, String> {
    let path = match explicit.map(str::trim).filter(|p| !p.is_empty()) {
        Some(p) => {
            let pb = PathBuf::from(p);
            if !pb.is_file() {
                return Err(format!("--config 指定的文件不存在: {}", pb.display()));
            }
            Some(pb)
        }
        None => discover(env),
    };
    let mut errs: Vec<String> = Vec::new();
    let mut values: HashMap<&'static str, Value> = HashMap::new();
    let mut sources: Sources = HashMap::new();

    // ① TOML 层：文件存在才解析；整份文件全部解析、全部报错，
    //    不因为第一个坏键停下。没有文件不是错误——env 层照常生效。
    if let Some(path) = path {
        let raw = std::fs::read_to_string(&path)
            .map_err(|e| format!("读配置文件 {} 失败: {e}", path.display()))?;
        let doc: toml::Value =
            toml::from_str(&raw).map_err(|e| format!("{} 不是合法 TOML: {e}", path.display()))?;
        let mut flat: Vec<(String, String)> = Vec::new();
        flatten(&doc, String::new(), &path, &mut flat, &mut errs);
        for (key, raw) in flat {
            let Some(entry) = find_entry(&key) else {
                errs.push(format!(
                    "{}: 未知键 {key:?}。可用键见 config.example.toml",
                    path.display()
                ));
                continue;
            };
            match (entry.parse)(&raw) {
                Ok(v) => {
                    values.insert(entry.key, v);
                    sources.insert(entry.key, Source::Toml);
                }
                Err(msg) => errs.push(format!(
                    "{}: {} = {raw:?} 无效: {msg}",
                    path.display(),
                    entry.key
                )),
            }
        }
    }

    // ② env 层：主变量优先，未设才看旧变量
    for entry in TABLE {
        // 空串 = 未设（与旧 `filter(|v| !v.is_empty())` 一致）；过滤在 env
        // 通道里做，保证任何 env 来源都有这条语义
        let hit = env(entry.env)
            .filter(|raw| !raw.is_empty())
            .map(|raw| (entry.env, raw, entry.parse))
            .or_else(|| {
                entry.env_legacy.and_then(|name| {
                    env(name)
                        .filter(|raw| !raw.is_empty())
                        .map(|raw| (name, raw, entry.parse_legacy.unwrap_or(entry.parse)))
                })
            });
        let Some((name, raw, parse)) = hit else {
            continue;
        };
        match parse(&raw) {
            Ok(v) => {
                values.insert(entry.key, v);
                sources.insert(entry.key, Source::Env);
            }
            Err(msg) => errs.push(format!("环境变量 {name}={raw:?} 无效: {msg}")),
        }
    }

    // ③ 交叉校验：围栏锚点必须成对
    let has_lat = values.contains_key("run.center_lat");
    let has_lng = values.contains_key("run.center_lng");
    if has_lat != has_lng {
        errs.push(
            "run.center_lat / run.center_lng（PACE_CENTER_LAT / PACE_CENTER_LNG）必须成对出现：只设一个会被静默忽略，这类坑现在直接报错".into(),
        );
    }

    if !errs.is_empty() {
        return Err(errs.join("\n  - ").trim_start_matches("\n  - ").to_string());
    }

    let mut cfg = Config::from_values(&values);
    cfg.sources = sources;
    Ok(cfg)
}

/// `--config` / `PACE_CONFIG` 未指定时的搜索顺序：`./config.toml` →
/// `~/.pace-forge/config.toml`。都不存在 = 无配置文件，全默认。
fn discover(env: &dyn Fn(&str) -> Option<String>) -> Option<PathBuf> {
    let cwd = PathBuf::from(CONFIG_FILE_NAME);
    if cwd.is_file() {
        return Some(cwd);
    }
    let home = home_dir(env)?;
    let p = home.join(".pace-forge").join(CONFIG_FILE_NAME);
    p.is_file().then_some(p)
}

fn home_dir(env: &dyn Fn(&str) -> Option<String>) -> Option<PathBuf> {
    env("HOME")
        .or_else(|| env("USERPROFILE"))
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
}

/// 把 TOML 文档拍平成 `键路径 → 字符串`，让 TOML 值和 env 值走同一个解析器。
/// 数组、_datetime、嵌套表下的意外形态都算错误。
fn flatten(
    node: &toml::Value,
    prefix: String,
    path: &Path,
    out: &mut Vec<(String, String)>,
    errs: &mut Vec<String>,
) {
    match node {
        toml::Value::Table(t) => {
            for (k, v) in t {
                let key = if prefix.is_empty() {
                    k.clone()
                } else {
                    format!("{prefix}.{k}")
                };
                flatten(v, key, path, out, errs);
            }
        }
        toml::Value::String(s) => out.push((prefix, s.clone())),
        toml::Value::Integer(i) => out.push((prefix, i.to_string())),
        toml::Value::Float(f) => out.push((prefix, f.to_string())),
        toml::Value::Boolean(b) => out.push((prefix, b.to_string())),
        other => errs.push(format!(
            "{}: {prefix} 的值类型不支持（{}），字符串/数字/布尔都可以",
            path.display(),
            type_name(other)
        )),
    }
}

fn type_name(v: &toml::Value) -> &'static str {
    match v {
        toml::Value::String(_) => "字符串",
        toml::Value::Integer(_) => "整数",
        toml::Value::Float(_) => "浮点数",
        toml::Value::Boolean(_) => "布尔",
        toml::Value::Datetime(_) => "日期时间",
        toml::Value::Array(_) => "数组",
        toml::Value::Table(_) => "表",
    }
}

impl Config {
    /// 注册表值 → 结构体字段。键来自注册表，这里必须全覆盖；
    /// 漏一个键，该开关就永远到不了字段上——`registry_matches_config_keys`
    /// 测试保证键集合一致，本函数的 `_` 分支兜住理论上的不一致。
    // Every registry key must have an assignment below; a missing one is a programming error.
    #[expect(clippy::panic, reason = "registry keys are exhaustive here")]
    pub(crate) fn from_values(values: &HashMap<&'static str, Value>) -> Config {
        let mut cfg = Config::default();
        for (key, v) in values {
            match *key {
                "server.token" => cfg.server.token = v.as_str().to_string(),
                "server.data_dir" => cfg.server.data_dir = v.as_str().to_string(),
                "network.egress_proxy" => cfg.network.egress_proxy = v.as_str().to_string(),
                "network.extra_ca" => cfg.network.extra_ca = v.as_str().to_string(),
                "network.tz_offset_sec" => cfg.network.tz_offset_sec = v.as_int(),
                "device.app_version" => {
                    // 旧实现 trim 后空串回落默认，不发空版本号
                    cfg.device.app_version = if v.as_str().is_empty() {
                        DEFAULT_APP_VERSION.into()
                    } else {
                        v.as_str().to_string()
                    }
                }
                "device.model" => cfg.device.model = v.as_str().to_string(),
                "device.hw_headers" => {
                    cfg.device.hw_headers = if v.as_str() == "derived" {
                        HwHeaders::Derived
                    } else {
                        HwHeaders::Empty
                    }
                }
                "run.run_mode" => cfg.run.run_mode = v.as_int(),
                "run.step_m" => cfg.run.step_m = v.as_float(),
                "run.bulge_max" => cfg.run.bulge_max = v.as_float(),
                "run.pt_scheme" => {
                    cfg.run.pt_scheme = match v.as_str() {
                        "old" => PtScheme::Old,
                        "new" => PtScheme::New,
                        _ => PtScheme::Auto,
                    }
                }
                "run.fixed_point" => {
                    cfg.run.fixed_point = match v.as_str() {
                        "allnormal" => FixedPoint::AllNormal,
                        "server" => FixedPoint::FollowServer,
                        _ => FixedPoint::Auto,
                    }
                }
                "run.fixed_strict" => cfg.run.fixed_strict = v.as_bool(),
                "run.strict_policy" => cfg.run.strict_policy = v.as_bool(),
                "run.fence_full" => cfg.run.fence_full = v.as_bool(),
                "run.run_area_id" => cfg.run.run_area_id = Some(-1),
                "run.cad_intercept" => cfg.run.cad_intercept = Some(v.as_float()),
                "run.cad_slope" => cfg.run.cad_slope = Some(v.as_float()),
                "run.center_lat" => cfg.run.center_lat = Some(v.as_float()),
                "run.center_lng" => cfg.run.center_lng = Some(v.as_float()),
                "run.prefer_stadium" => cfg.run.prefer_stadium = v.as_bool(),
                "run.ignore_daily_limit" => cfg.run.ignore_daily_limit = v.as_bool(),
                "run.dump_policy" => cfg.run.dump_policy = v.as_str().to_string(),
                other => panic!("注册表键 {other} 在 Config::from_values 里没有落点"),
            }
        }
        cfg
    }
}

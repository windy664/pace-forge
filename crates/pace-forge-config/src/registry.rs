//! 开关注册表（唯一事实源）：键名、env 名、解析器都在这一张表里。
//!
//! TOML 解析和 env 解析用的是同一份定义，不存在「文档表」和「实际读取」
//! 两套东西对不上的问题。键的参考文档（默认值、影响说明、TOML 写法）
//! 只在仓库根的 `config.example.toml` 一处，代码里不重复一份。

use crate::parser::{
    RawParser, parse_bool, parse_bulge_max, parse_cad_coefficient, parse_center_lat,
    parse_center_lng, parse_fixed_point, parse_fixed_point_legacy, parse_hw_headers,
    parse_pt_scheme, parse_run_area, parse_run_mode, parse_step_m, parse_str_raw, parse_str_trim,
    parse_tz_offset,
};

/// 一个开关的完整定义：键名、env 名、解析器。
pub(crate) struct EntryDef {
    /// TOML 键路径，如 `run.bulge_max`。
    pub(crate) key: &'static str,
    /// 环境变量名（全部键都有）。
    pub(crate) env: &'static str,
    /// 旧环境变量名：仅当主变量未设时读，词汇表单独解析。
    pub(crate) env_legacy: Option<&'static str>,
    pub(crate) parse: RawParser,
    pub(crate) parse_legacy: Option<RawParser>,
}

macro_rules! entry {
    ($key:literal, $env:literal, $parse:ident) => {
        EntryDef {
            key: $key,
            env: $env,
            env_legacy: None,
            parse: $parse,
            parse_legacy: None,
        }
    };
}

/// 清单。新增开关 = 这里加一条 + [`Config`](crate::Config) 加字段 +
/// [`Config::key_values`](crate::Config::key_values) 加一行显示——三处少一处，
/// `registry_matches_config_keys` 测试直接红。口令类的打码在
/// `key_values` 的对应分支里做（原值不进显示）。
pub(crate) const TABLE: &[EntryDef] = &[
    entry!("server.token", "PACE_FORGE_TOKEN", parse_str_raw),
    entry!("server.data_dir", "PACE_FORGE_DATA_DIR", parse_str_raw),
    entry!("network.egress_proxy", "PACE_EGRESS_PROXY", parse_str_trim),
    entry!("network.extra_ca", "PACE_EXTRA_CA", parse_str_raw),
    entry!(
        "network.tz_offset_sec",
        "PACE_TZ_OFFSET_SEC",
        parse_tz_offset
    ),
    entry!("device.app_version", "PACE_APP_VERSION", parse_str_trim),
    entry!("device.model", "PACE_DEVICE_MODEL", parse_str_trim),
    entry!(
        "device.hw_headers",
        "PACE_DEVICE_HW_HEADERS",
        parse_hw_headers
    ),
    entry!("run.run_mode", "PACE_RUN_MODE", parse_run_mode),
    entry!("run.step_m", "PACE_STEP_M", parse_step_m),
    entry!("run.bulge_max", "PACE_BULGE_MAX", parse_bulge_max),
    entry!(
        "run.cad_intercept",
        "PACE_CAD_INTERCEPT",
        parse_cad_coefficient
    ),
    entry!("run.cad_slope", "PACE_CAD_SLOPE", parse_cad_coefficient),
    entry!("run.pt_scheme", "PACE_PT_SCHEME", parse_pt_scheme),
    EntryDef {
        key: "run.fixed_point",
        env: "PACE_FIXED_POINT",
        env_legacy: Some("PACE_FORCE_NORMAL_POINT"),
        parse: parse_fixed_point,
        parse_legacy: Some(parse_fixed_point_legacy),
    },
    entry!("run.fixed_strict", "PACE_FIXED_STRICT", parse_bool),
    entry!("run.strict_policy", "PACE_STRICT_POLICY", parse_bool),
    entry!("run.fence_full", "PACE_FENCE_FULL", parse_bool),
    entry!("run.run_area_id", "PACE_RUN_AREA_ID", parse_run_area),
    entry!("run.center_lat", "PACE_CENTER_LAT", parse_center_lat),
    entry!("run.center_lng", "PACE_CENTER_LNG", parse_center_lng),
    entry!("run.prefer_stadium", "PACE_PREFER_STADIUM", parse_bool),
    entry!(
        "run.ignore_daily_limit",
        "PACE_IGNORE_DAILY_LIMIT",
        parse_bool
    ),
    entry!("run.dump_policy", "PACE_DUMP_POLICY", parse_str_raw),
];

pub(crate) fn find_entry(key: &str) -> Option<&'static EntryDef> {
    TABLE.iter().find(|e| e.key == key)
}

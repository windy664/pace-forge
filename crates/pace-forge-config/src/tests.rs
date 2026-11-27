// Tests panic on purpose; the workspace lints deny that only in production code.
#![cfg_attr(test, expect(clippy::unwrap_used, reason = "assertions in tests"))]

//! 配置加载的等价性与边界测试矩阵。词汇表、优先级、来源标注、
//! 与旧 `PACE_*` 散装解析的逐字节对齐都锁在这里。

use std::collections::HashMap;

use crate::load::load_from;
use crate::registry::TABLE;
use crate::*;

/// 注册表与 Config 字段必须一一对应：新增开关少改任何一边都过不了这条。
#[test]
fn registry_matches_config_keys() {
    let mut table_keys: Vec<&str> = TABLE.iter().map(|e| e.key).collect();
    table_keys.sort_unstable();
    let mut cfg_keys: Vec<&str> = Config::default()
        .key_values()
        .iter()
        .map(|(k, _)| *k)
        .collect();
    cfg_keys.sort_unstable();
    assert_eq!(
        table_keys, cfg_keys,
        "注册表与 Config::key_values 的键集合不一致"
    );
}

#[test]
fn env_names_are_unique_and_paced_prefixed() {
    let mut names: Vec<&str> = TABLE.iter().map(|e| e.env).collect();
    names.sort_unstable();
    let n = names.len();
    names.dedup();
    assert_eq!(names.len(), n, "环境变量名重复");
    for e in TABLE {
        assert!(e.env.starts_with("PACE_"), "{} 不是 PACE_ 前缀", e.env);
        if let Some(legacy) = e.env_legacy {
            assert!(legacy.starts_with("PACE_"), "{legacy} 不是 PACE_ 前缀");
        }
    }
}

#[test]
fn no_file_means_all_defaults() {
    let cfg = load_from(None, &|_| None).unwrap();
    assert_eq!(cfg, Config::default());
    assert_eq!(cfg.sources.get("run.bulge_max"), None);
}

#[test]
fn toml_values_reach_fields_with_toml_source() {
    let dir = std::env::temp_dir().join(format!("pace-cfg-{}-{:x}", std::process::id(), micros()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("config.toml");
    std::fs::write(
        &path,
        "[network]\ntz_offset_sec = 0\n\n[run]\nbulge_max = 100\npt_scheme = \"old\"\n",
    )
    .unwrap();
    let cfg = load_from(Some(path.to_str().unwrap()), &|_| None).unwrap();
    assert_eq!(cfg.network.tz_offset_sec, 0);
    assert_eq!(cfg.run.bulge_max, 100.0);
    assert_eq!(cfg.run.pt_scheme, PtScheme::Old);
    assert_eq!(cfg.sources.get("run.bulge_max"), Some(&Source::Toml));
    assert_eq!(
        cfg.sources.get("run.bulge_max"),
        cfg.sources.get("run.pt_scheme")
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn env_overrides_toml() {
    let dir = std::env::temp_dir().join(format!("pace-cfg-{}-{:x}", std::process::id(), micros()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("config.toml");
    std::fs::write(&path, "[run]\nbulge_max = 100\n").unwrap();
    let cfg = load_from(Some(path.to_str().unwrap()), &|name| {
        (name == "PACE_BULGE_MAX").then(|| "200".to_string())
    })
    .unwrap();
    assert_eq!(cfg.run.bulge_max, 200.0);
    assert_eq!(cfg.sources.get("run.bulge_max"), Some(&Source::Env));
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn invalid_value_and_unknown_key_are_reported_together() {
    let dir = std::env::temp_dir().join(format!("pace-cfg-{}-{:x}", std::process::id(), micros()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("config.toml");
    std::fs::write(
        &path,
        "[run]\nbulge_max = 999\nunknown_key = 1\nstep_m = \"fast\"\n",
    )
    .unwrap();
    let err = load_from(Some(path.to_str().unwrap()), &|_| None).unwrap_err();
    assert!(err.contains("bulge_max"), "应报范围错误: {err}");
    assert!(err.contains("unknown_key"), "应报未知键: {err}");
    assert!(err.contains("step_m"), "应报类型错误: {err}");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn center_anchor_requires_both_halves() {
    let err = load_from(None, &|name| {
        (name == "PACE_CENTER_LAT").then(|| "30.0".to_string())
    })
    .unwrap_err();
    assert!(err.contains("成对"), "缺一半要报错: {err}");
    let cfg = load_from(None, &|name| match name {
        "PACE_CENTER_LAT" => Some("30.0".into()),
        "PACE_CENTER_LNG" => Some("104.0".into()),
        _ => None,
    })
    .unwrap();
    assert_eq!(cfg.run.center_lat, Some(30.0));
    assert_eq!(cfg.run.center_lng, Some(104.0));
}

#[test]
fn legacy_force_normal_point_maps_bool_vocab() {
    let cfg = load_from(None, &|name| {
        (name == "PACE_FORCE_NORMAL_POINT").then(|| "1".to_string())
    })
    .unwrap();
    assert_eq!(cfg.run.fixed_point, FixedPoint::AllNormal);
    let cfg = load_from(None, &|name| {
        (name == "PACE_FORCE_NORMAL_POINT").then(|| "0".to_string())
    })
    .unwrap();
    assert_eq!(cfg.run.fixed_point, FixedPoint::FollowServer);
}

#[test]
fn fixed_point_legacy_loses_to_primary() {
    let cfg = load_from(None, &|name| match name {
        "PACE_FIXED_POINT" => Some("server".into()),
        "PACE_FORCE_NORMAL_POINT" => Some("1".into()),
        _ => None,
    })
    .unwrap();
    assert_eq!(cfg.run.fixed_point, FixedPoint::FollowServer);
}

#[test]
fn cad_coefficients_are_optional_and_validated() {
    // 不设 = None（步态层用自己的内置默认 34.4 / 44.5，默认行为逐字节不变）。
    let cfg = load_from(None, &|_| None).unwrap();
    assert_eq!(cfg.run.cad_intercept, None);
    assert_eq!(cfg.run.cad_slope, None);

    // 设了就生效（env 覆盖层）。
    let cfg = load_from(None, &|name| match name {
        "PACE_CAD_INTERCEPT" => Some("45.3".to_string()),
        "PACE_CAD_SLOPE" => Some("44.5".to_string()),
        _ => None,
    })
    .unwrap();
    assert_eq!(cfg.run.cad_intercept, Some(45.3));
    assert_eq!(cfg.run.cad_slope, Some(44.5));

    // 非法值是错误，不是静默回落（步态系数配错会毁掉整条轨迹）。
    let err = load_from(None, &|name| {
        (name == "PACE_CAD_INTERCEPT").then(|| "abc".to_string())
    })
    .unwrap_err();
    assert!(err.contains("PACE_CAD_INTERCEPT"), "{err}");
}

#[test]
fn invalid_env_is_an_error_not_a_fallback() {
    let err = load_from(None, &|name| {
        (name == "PACE_BULGE_MAX").then(|| "6O".to_string())
    })
    .unwrap_err();
    assert!(err.contains("PACE_BULGE_MAX"), "{err}");
}

#[test]
fn empty_env_value_counts_as_unset() {
    // 空串 = 未设（与旧 `filter(|v| !v.is_empty())` 一致）
    let cfg = load_from(None, &|name| {
        (name == "PACE_EGRESS_PROXY").then(|| "".to_string())
    })
    .unwrap();
    assert_eq!(cfg.network.egress_proxy, "");
    assert_eq!(cfg.sources.get("network.egress_proxy"), None);
}

/// 与旧实现的字符串语义逐字节对齐：哪些键 trim、哪些保留原文、
/// 空串回落哪个默认，都是旧代码里各写各的，不能统一掉。
#[test]
fn string_semantics_match_legacy_exactly() {
    // token / data_dir / extra_ca / dump_policy：保留原文（旧实现不 trim，
    // 带空格的口令也是合法口令）
    let cfg = load_from(None, &|n| {
        (n == "PACE_FORGE_TOKEN").then(|| "  secret ".to_string())
    })
    .unwrap();
    assert_eq!(cfg.server.token, "  secret ");
    let cfg = load_from(None, &|n| {
        (n == "PACE_FORGE_DATA_DIR").then(|| " /tmp/x ".to_string())
    })
    .unwrap();
    assert_eq!(cfg.server.data_dir, " /tmp/x ");
    let cfg = load_from(None, &|n| {
        (n == "PACE_EXTRA_CA").then(|| " /ca/a.pem ".to_string())
    })
    .unwrap();
    assert_eq!(cfg.network.extra_ca, " /ca/a.pem ");
    // app_version / egress / model：trim（旧实现就是 trim 后用）
    let cfg = load_from(None, &|n| {
        (n == "PACE_APP_VERSION").then(|| " 7.4.21 ".to_string())
    })
    .unwrap();
    assert_eq!(cfg.device.app_version, "7.4.21");
    let cfg = load_from(None, &|n| {
        (n == "PACE_EGRESS_PROXY").then(|| "  http://127.0.0.1:7890  ".to_string())
    })
    .unwrap();
    assert_eq!(cfg.network.egress_proxy, "http://127.0.0.1:7890");
    // app_version 空串回落默认（旧 trim+filter → 默认，不发空版本号）
    let cfg = load_from(None, &|n| {
        (n == "PACE_APP_VERSION").then(|| " ".to_string())
    })
    .unwrap();
    assert_eq!(cfg.device.app_version, DEFAULT_APP_VERSION);
}

/// 数值边界与旧实现的 filter/parse 范围对齐；界外从「静默回落」改为报错。
#[test]
fn numeric_boundaries_match_legacy() {
    let ok =
        |name: &str, raw: &str| load_from(None, &|n| (n == name).then(|| raw.to_string())).unwrap();
    let err =
        |name: &str, raw: &str| load_from(None, &|n| (n == name).then(|| raw.to_string())).is_err();

    // tz ±20 小时（旧 filter 同范围：界内生效、界外回落默认 → 现在报错）
    assert_eq!(
        ok("PACE_TZ_OFFSET_SEC", "-72000").network.tz_offset_sec,
        -72_000
    );
    assert_eq!(
        ok("PACE_TZ_OFFSET_SEC", "72000").network.tz_offset_sec,
        72_000
    );
    assert_eq!(
        ok("PACE_TZ_OFFSET_SEC", "28800").network.tz_offset_sec,
        28_800
    );
    assert!(err("PACE_TZ_OFFSET_SEC", "-72001"));
    assert!(err("PACE_TZ_OFFSET_SEC", "72001"));
    assert!(err("PACE_TZ_OFFSET_SEC", "abc"));

    // bulge 0~300（旧 filter 同范围）
    assert_eq!(ok("PACE_BULGE_MAX", "0").run.bulge_max, 0.0);
    assert_eq!(ok("PACE_BULGE_MAX", "300").run.bulge_max, 300.0);
    assert_eq!(ok("PACE_BULGE_MAX", "60.5").run.bulge_max, 60.5);
    assert!(err("PACE_BULGE_MAX", "-0.1"));
    assert!(err("PACE_BULGE_MAX", "300.1"));
    assert!(err("PACE_BULGE_MAX", "inf"));
    assert!(err("PACE_BULGE_MAX", "6O"));

    // step_m：正数全放开（旧任意 f64，只挡明显打错的 0/负数/inf）
    assert_eq!(ok("PACE_STEP_M", "0.001").run.step_m, 0.001);
    assert_eq!(ok("PACE_STEP_M", "100000").run.step_m, 100_000.0);
    assert!(err("PACE_STEP_M", "0"));
    assert!(err("PACE_STEP_M", "-5"));
    assert!(err("PACE_STEP_M", "inf"));

    // run_mode：任意整数（服务端 0/2~6 实验值不设限）
    assert_eq!(ok("PACE_RUN_MODE", "0").run.run_mode, 0);
    assert_eq!(ok("PACE_RUN_MODE", "6").run.run_mode, 6);
    assert_eq!(ok("PACE_RUN_MODE", "-3").run.run_mode, -3);
    assert!(err("PACE_RUN_MODE", "abc"));
    assert!(err("PACE_RUN_MODE", "2.5"));
    // 旧实现对原文精确 parse：" 1" 回落默认，现在报错
    assert!(err("PACE_RUN_MODE", " 1"));

    // lat/lng 边界（必须成对设置，缺一半本身会报错）
    let pair = |lat: &str, lng: &str| {
        load_from(None, &|n| match n {
            "PACE_CENTER_LAT" => Some(lat.to_string()),
            "PACE_CENTER_LNG" => Some(lng.to_string()),
            _ => None,
        })
        .unwrap()
    };
    let pair_err = |lat: &str, lng: &str| {
        load_from(None, &|n| match n {
            "PACE_CENTER_LAT" => Some(lat.to_string()),
            "PACE_CENTER_LNG" => Some(lng.to_string()),
            _ => None,
        })
        .is_err()
    };
    assert_eq!(pair("-90", "104").run.center_lat, Some(-90.0));
    assert_eq!(pair("90", "104").run.center_lat, Some(90.0));
    assert_eq!(pair("30", "-180").run.center_lng, Some(-180.0));
    assert_eq!(pair("30", "180").run.center_lng, Some(180.0));
    assert!(pair_err("90.1", "104"));
    assert!(pair_err("nan", "104"));
    assert!(pair_err("30", "180.1"));
}

/// 布尔词汇表：统一后的 0/1、true/false、yes/no 全部可用（大小写不敏感）。
#[test]
fn bool_vocab_is_unified_and_case_insensitive() {
    let val = |name: &str, raw: &str| {
        load_from(None, &|n| (n == name).then(|| raw.to_string()))
            .unwrap()
            .run
            .ignore_daily_limit
    };
    for t in ["1", "true", "yes", "TRUE", "True", "YES"] {
        assert!(val("PACE_IGNORE_DAILY_LIMIT", t), "{t} 应为 true");
    }
    for f in ["0", "false", "no", "FALSE", "No"] {
        assert!(!val("PACE_IGNORE_DAILY_LIMIT", f), "{f} 应为 false");
    }
    assert!(
        load_from(None, &|n| {
            (n == "PACE_IGNORE_DAILY_LIMIT").then(|| "on".to_string())
        })
        .is_err()
    );
    // 语义修正对照：旧实现连 "0" 都会绕过日上限（存在性判断）
    assert!(!val("PACE_IGNORE_DAILY_LIMIT", "0"));
}

/// 枚举键词汇表与旧实现精确对齐（原文匹配，不 trim）。
#[test]
fn choice_and_legacy_vocab_exact() {
    let fp = |raw: &str| {
        load_from(None, &|n| {
            (n == "PACE_FIXED_POINT").then(|| raw.to_string())
        })
        .unwrap()
        .run
        .fixed_point
    };
    assert_eq!(fp("auto"), FixedPoint::Auto);
    assert_eq!(fp("all0"), FixedPoint::AllNormal);
    assert_eq!(fp("allnormal"), FixedPoint::AllNormal);
    assert_eq!(fp("server"), FixedPoint::FollowServer);
    assert_eq!(fp("follow"), FixedPoint::FollowServer);
    assert!(
        load_from(None, &|n| (n == "PACE_FIXED_POINT")
            .then(|| "1".to_string()))
        .is_err()
    );
    assert!(
        load_from(None, &|n| (n == "PACE_FIXED_POINT")
            .then(|| " all0 ".to_string()))
        .is_err()
    );

    // 旧变量词汇：0/1/false/no/true/yes
    let legacy = |raw: &str| {
        load_from(None, &|n| {
            (n == "PACE_FORCE_NORMAL_POINT").then(|| raw.to_string())
        })
        .unwrap()
        .run
        .fixed_point
    };
    assert_eq!(legacy("0"), FixedPoint::FollowServer);
    assert_eq!(legacy("false"), FixedPoint::FollowServer);
    assert_eq!(legacy("no"), FixedPoint::FollowServer);
    assert_eq!(legacy("1"), FixedPoint::AllNormal);
    assert_eq!(legacy("true"), FixedPoint::AllNormal);
    assert_eq!(legacy("yes"), FixedPoint::AllNormal);
    assert!(
        load_from(None, &|n| {
            (n == "PACE_FORCE_NORMAL_POINT").then(|| "2".to_string())
        })
        .is_err()
    );

    let ps = |raw: &str| {
        load_from(None, &|n| (n == "PACE_PT_SCHEME").then(|| raw.to_string()))
            .unwrap()
            .run
            .pt_scheme
    };
    assert_eq!(ps("old"), PtScheme::Old);
    assert_eq!(ps("marks"), PtScheme::Old);
    assert_eq!(ps("new"), PtScheme::New);
    assert_eq!(ps("auto"), PtScheme::Auto);
    // 旧实现对任何未知值（含拼写错误）静默走 auto；现在报错
    assert!(
        load_from(None, &|n| (n == "PACE_PT_SCHEME")
            .then(|| "odl".to_string()))
        .is_err()
    );

    let cfg = load_from(None, &|n| {
        (n == "PACE_RUN_AREA_ID").then(|| "minus1".to_string())
    })
    .unwrap();
    assert_eq!(cfg.run.run_area_id, Some(-1));
    assert!(
        load_from(None, &|n| (n == "PACE_RUN_AREA_ID")
            .then(|| "0".to_string()))
        .is_err()
    );

    let hw = |raw: &str| {
        load_from(None, &|n| {
            (n == "PACE_DEVICE_HW_HEADERS").then(|| raw.to_string())
        })
        .unwrap()
        .device
        .hw_headers
    };
    assert_eq!(hw("empty"), HwHeaders::Empty);
    assert_eq!(hw("0"), HwHeaders::Empty);
    assert_eq!(hw("derived"), HwHeaders::Derived);
    assert_eq!(hw("1"), HwHeaders::Derived);
    assert_eq!(hw("true"), HwHeaders::Derived);
    assert!(
        load_from(None, &|n| {
            (n == "PACE_DEVICE_HW_HEADERS").then(|| "yes".to_string())
        })
        .is_err()
    );
}

/// TOML 里的数组 / datetime / 嵌套意外形态要报错，不能静默丢键。
#[test]
fn toml_array_values_are_rejected() {
    let dir = std::env::temp_dir().join(format!("pace-cfg-{}-{:x}", std::process::id(), micros()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("config.toml");
    std::fs::write(&path, "[run]\npt_scheme = [\"old\"]\n").unwrap();
    let err = load_from(Some(path.to_str().unwrap()), &|_| None).unwrap_err();
    assert!(err.contains("pt_scheme"), "{err}");
    assert!(err.contains("数组"), "{err}");
    std::fs::remove_dir_all(dir).unwrap();
}

/// run_area_id 的合法值只有 -1/minus1；结构体字段必须是 Some(-1)。
#[test]
fn run_area_id_default_is_none() {
    let cfg = load_from(None, &|_| None).unwrap();
    assert_eq!(cfg.run.run_area_id, None);
}

#[test]
fn toml_scalar_types_are_accepted() {
    let dir = std::env::temp_dir().join(format!("pace-cfg-{}-{:x}", std::process::id(), micros()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("config.toml");
    // TOML 原生整数/布尔也要能落到浮点/布尔键上
    std::fs::write(&path, "[run]\nstep_m = 7\nfixed_strict = true\n").unwrap();
    let cfg = load_from(Some(path.to_str().unwrap()), &|_| None).unwrap();
    assert_eq!(cfg.run.step_m, 7.0);
    assert!(cfg.run.fixed_strict);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn guard_replaces_and_restores() {
    assert_eq!(global().run.bulge_max, 60.0);
    {
        let mut cfg = Config::default();
        cfg.run.bulge_max = 250.0;
        let _g = Guard::replace(cfg);
        assert_eq!(global().run.bulge_max, 250.0);
    }
    assert_eq!(global().run.bulge_max, 60.0);
}

#[test]
fn secret_tokens_are_masked_in_key_values() {
    let mut cfg = Config::default();
    cfg.server.token = "hunter2".into();
    let kv: HashMap<&str, String> = cfg.key_values().into_iter().collect();
    assert_eq!(kv["server.token"], "已设置（不显示值）");
    assert!(
        !kv.values().any(|v| v.contains("hunter2")),
        "口令原文不许出现在任何显示值里"
    );
}

#[test]
fn explicit_missing_path_is_an_error() {
    assert!(load_from(Some("/nonexistent/pace-cfg.toml"), &|_| None).is_err());
}

fn micros() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_micros())
        .unwrap_or(0)
}

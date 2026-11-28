//! 单键解析器。每个键一个入口，TOML 和 env 两个来源共用同一份。

use crate::value::Value;

/// 单键解析器：`原文 → 类型化值`。
pub(crate) type RawParser = fn(&str) -> Result<Value, String>;

/// 保留原文的字符串键：口令、目录类。旧实现里这几个值不 trim
/// （`PACE_FORGE_TOKEN` 带尾随空格也是合法口令），必须照搬。
pub(crate) fn parse_str_raw(raw: &str) -> Result<Value, String> {
    Ok(Value::Str(raw.to_string()))
}

/// 去空白字符串键：版本号、代理、机型。旧实现就是 `trim` 之后用。
pub(crate) fn parse_str_trim(raw: &str) -> Result<Value, String> {
    Ok(Value::Str(raw.trim().to_string()))
}

/// 布尔词汇表统一为 0/1、true/false、yes/no（大小写不敏感）。
///
/// 旧的散装解析各键口径不一（fence_full 只认 "1"、fixed_strict 还认
/// "TRUE"、prefer_stadium 认 yes），文档里写过的值全部行为不变；这次
/// 统一词汇表属于 issue #53 里声明的兼容性放宽，其余词汇外的值从「静默
/// 回落」改为「启动报错」。
pub(crate) fn parse_bool(raw: &str) -> Result<Value, String> {
    match raw.to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" => Ok(Value::Bool(true)),
        "0" | "false" | "no" => Ok(Value::Bool(false)),
        other => Err(format!("要 0/1、true/false 或 yes/no，收到 {other:?}")),
    }
}

fn int_in_range(min: i64, max: i64) -> impl Fn(&str) -> Result<Value, String> {
    move |raw: &str| {
        // 旧实现对原文精确 parse（" 60" 会回落默认），现在词汇外一律报错
        let v: i64 = raw
            .parse()
            .map_err(|_| format!("要 {min}~{max} 的整数，收到 {raw:?}"))?;
        if (min..=max).contains(&v) {
            Ok(Value::Int(v))
        } else {
            Err(format!("要 {min}~{max} 的整数，收到 {v}"))
        }
    }
}

/// 任意 i64（run_mode：服务端 0/2~6 都当自由跑，实验值不设上限）。
fn parse_int_any(raw: &str) -> Result<Value, String> {
    raw.parse::<i64>()
        .map(Value::Int)
        .map_err(|_| format!("要整数，收到 {raw:?}"))
}

fn float_in_range(min: f64, max: f64) -> impl Fn(&str) -> Result<Value, String> {
    move |raw: &str| {
        let v: f64 = raw
            .parse()
            .map_err(|_| format!("要 {min}~{max} 的数字，收到 {raw:?}"))?;
        if v.is_finite() && (min..=max).contains(&v) {
            Ok(Value::Float(v))
        } else {
            Err(format!("要 {min}~{max} 范围内的有限数字，收到 {raw:?}"))
        }
    }
}

fn choice(list: &'static [&'static str]) -> impl Fn(&str) -> Result<Value, String> {
    move |raw: &str| {
        if list.contains(&raw) {
            Ok(Value::Str(raw.to_string()))
        } else {
            Err(format!("要 {} 之一，收到 {raw:?}", list.join(" / ")))
        }
    }
}

/// 硬件头：`empty|derived`，env 老用法 `1/true`（=derived）、`0/false`（=empty）
/// 也认；大小写不敏感是这次统一的一部分。
pub(crate) fn parse_hw_headers(raw: &str) -> Result<Value, String> {
    match raw.to_ascii_lowercase().as_str() {
        "empty" | "0" | "false" => Ok(Value::Str("empty".into())),
        "derived" | "1" | "true" => Ok(Value::Str("derived".into())),
        other => Err(format!("要 empty 或 derived，收到 {other:?}")),
    }
}

/// 点位口径：`auto|old|marks|new`，marks 归一成 old。原文精确匹配。
pub(crate) fn parse_pt_scheme(raw: &str) -> Result<Value, String> {
    match raw {
        "auto" => Ok(Value::Str("auto".into())),
        "old" | "marks" => Ok(Value::Str("old".into())),
        "new" => Ok(Value::Str("new".into())),
        other => Err(format!("要 auto / old / new 之一，收到 {other:?}")),
    }
}

/// 固定点口径：`auto|all0|allnormal|server|follow`。原文精确匹配。
pub(crate) fn parse_fixed_point(raw: &str) -> Result<Value, String> {
    match raw {
        "auto" => Ok(Value::Str("auto".into())),
        "all0" | "allnormal" => Ok(Value::Str("allnormal".into())),
        "server" | "follow" => Ok(Value::Str("server".into())),
        other => Err(format!(
            "要 auto / all0 / allnormal / server / follow 之一，收到 {other:?}"
        )),
    }
}

/// 旧变量 `PACE_FORCE_NORMAL_POINT` 的布尔词汇：0→server，1→allnormal。
pub(crate) fn parse_fixed_point_legacy(raw: &str) -> Result<Value, String> {
    match raw {
        "0" | "false" | "no" => Ok(Value::Str("server".into())),
        "1" | "true" | "yes" => Ok(Value::Str("allnormal".into())),
        other => Err(format!("要 0 或 1（旧变量口径），收到 {other:?}")),
    }
}

// 带范围/带词汇表的键：注册表要的是函数指针，这里给每个键一个具名入口。
pub(crate) fn parse_tz_offset(raw: &str) -> Result<Value, String> {
    int_in_range(-72_000, 72_000)(raw)
}
pub(crate) fn parse_run_mode(raw: &str) -> Result<Value, String> {
    parse_int_any(raw)
}
pub(crate) fn parse_step_m(raw: &str) -> Result<Value, String> {
    // 旧实现接受任意 f64（连 0 和负数都收，然后生成坏轨迹）；正数全放开，
    // 只挡 0/负数/inf 这类明显是打错的值。
    float_in_range(0.001, 100_000.0)(raw)
}
pub(crate) fn parse_bulge_max(raw: &str) -> Result<Value, String> {
    float_in_range(0.0, 300.0)(raw)
}
/// 起步窗口的里程占比。
///
/// 0 是「起步段完全压成一点」（点全叠在一起，报文会出现 0 位移的采样点），
/// 0.155 是「时间占比 = 里程占比」= 完全不压缩。两者之间都是合法值。
pub(crate) fn parse_startup_dist_ratio(raw: &str) -> Result<Value, String> {
    float_in_range(0.001, 0.5)(raw)
}
pub(crate) fn parse_center_lat(raw: &str) -> Result<Value, String> {
    float_in_range(-90.0, 90.0)(raw)
}
pub(crate) fn parse_center_lng(raw: &str) -> Result<Value, String> {
    float_in_range(-180.0, 180.0)(raw)
}
/// 步频回归系数（`cadence = 截距 + 斜率×v`）。
///
/// 0 或负数会算出荒谬步频，inf/NaN 会直接毁掉整条轨迹 ⇒ 一律拒。
/// 合法区间放得很宽（这两个数是「从自己账号历史拟合出来」的量纲）。
pub(crate) fn parse_cad_coefficient(raw: &str) -> Result<Value, String> {
    float_in_range(0.001, 1000.0)(raw)
}

pub(crate) fn parse_run_area(raw: &str) -> Result<Value, String> {
    choice(&["-1", "minus1"])(raw)
}

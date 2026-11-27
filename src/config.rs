//! 全部 `PACE_*` 环境开关的清单、默认值与当前取值。
//!
//! 排障时最常见的一句是「我设了环境变量，怎么好像没生效」。这里把开关
//! 集中成一张表：`pace-forge config` 打印默认值、当前值、以及它影响哪条
//! 判据；Web 启动时再打一行「非默认项」，日志里一眼能看见对方开了什么。

/// 值的类型，用来判断「设了但解析不了」。
///
/// `config` 最初只把环境变量原文和默认值比大小，非法值也显示「← 非默认」。
/// 而运行时 `parse().ok().unwrap_or(默认)` 会静默回落——表里写着生效、实际
/// 跑的是默认值，比不显示更容易误导。
#[derive(Debug, Clone, Copy)]
pub enum Kind {
    Str,
    Int,
    Float,
    /// 只接受 0/1（`true`/`false` 也认）
    Bool,
    Enum(&'static [&'static str]),
}

impl Kind {
    fn accepts(self, raw: &str) -> bool {
        match self {
            Kind::Str => true,
            Kind::Int => raw.parse::<i64>().is_ok(),
            Kind::Float => raw.parse::<f64>().is_ok(),
            Kind::Bool => matches!(raw, "0" | "1" | "true" | "false"),
            Kind::Enum(list) => list.contains(&raw),
        }
    }
}

/// 一个开关的静态描述。
pub struct Entry {
    pub name: &'static str,
    pub default: &'static str,
    pub effect: &'static str,
    pub kind: Kind,
    /// 用于「是否非默认」比较的实际值；`None` 表示只要设置就算非默认
    /// （口令、目录、自动推导这类没有可比较的字符串默认值）。
    pub default_value: Option<&'static str>,
}

/// 清单。新增 `env::var("PACE_*")` 时必须同步加到这里，否则 `config`
/// 会漏项——这条约定写在 CONTRIBUTING 里。
const ENTRIES: &[Entry] = &[
    Entry {
        name: "PACE_FORGE_TOKEN",
        default: "未设置",
        effect: "控制台口令。不设只接受本机访问；设了才允许同网段访问，并把 X-Pace-Token 当门槛",
        default_value: None,
        kind: Kind::Str,
    },
    Entry {
        name: "PACE_STRICT_POLICY",
        default: "0",
        effect: "1 = 策略拉取失败直接报错，不走「policy=0」兜底",
        default_value: Some("0"),
        kind: Kind::Bool,
    },
    Entry {
        name: "PACE_FENCE_FULL",
        default: "0",
        effect: "1 = 围栏对象发完整结构（distance/id/name），默认只发 points",
        default_value: Some("0"),
        kind: Kind::Bool,
    },
    Entry {
        name: "PACE_DUMP_POLICY",
        default: "未设置",
        effect: "目录路径。策略响应落到该目录，排查用；不设则完全不落盘",
        default_value: None,
        kind: Kind::Str,
    },
    Entry {
        name: "PACE_TZ_OFFSET_SEC",
        default: "28800（东八区）",
        effect: "OBS 时间戳的时区偏移（秒），跨时区部署用",
        default_value: Some("28800"),
        kind: Kind::Int,
    },
    Entry {
        name: "PACE_RUN_AREA_ID",
        default: "真实围栏 id（自动）",
        effect: "-1 时提交报文发 runAreaId=-1，退回旧行为做对照实验",
        default_value: None,
        kind: Kind::Int,
    },
    Entry {
        name: "PACE_PT_SCHEME",
        default: "自动（按有无必到点分流）",
        effect: "old|new。强制点位 type 口径，做对照实验用",
        default_value: None,
        kind: Kind::Enum(&["old", "new"]),
    },
    Entry {
        name: "PACE_BULGE_MAX",
        default: "60",
        effect: "几何规划时弧的鼓出上限（米，0~300）。设 0 更贴直线",
        default_value: Some("60"),
        kind: Kind::Float,
    },
    Entry {
        name: "PACE_CAD_INTERCEPT",
        default: "34.4",
        effect: "步频回归截距：cadence = 截距 + 斜率×v。按**自己账号**的历史步频标定（默认那对常数是从别人的 4 条记录拟合的）",
        default_value: Some("34.4"),
        kind: Kind::Float,
    },
    Entry {
        name: "PACE_CAD_SLOPE",
        default: "44.5",
        effect: "步频回归斜率：cadence = 截距 + 斜率×v",
        default_value: Some("44.5"),
        kind: Kind::Float,
    },
    Entry {
        name: "PACE_DEVICE_HW_HEADERS",
        default: "empty（发空串）",
        effect: "derived = 五个硬件头（androidId/blMac/wifiMac/IMEI/CustomDeviceId）发派生假值；默认空串，与 7.4.21 硬编码一致。DeviceId 不受影响",
        default_value: Some("empty"),
        kind: Kind::Enum(&["derived", "empty"]),
    },
    Entry {
        name: "PACE_RUN_MODE",
        default: "2",
        effect: "CLI run 的 runMode：1=计分跑，2=户外跑",
        default_value: Some("2"),
        kind: Kind::Int,
    },
    Entry {
        name: "PACE_FIXED_STRICT",
        default: "0",
        effect: "1 = 必到点只认 10 米内，够不着就报缺失，不自动放开半径",
        default_value: Some("0"),
        kind: Kind::Bool,
    },
    Entry {
        name: "PACE_EGRESS_PROXY",
        default: "未设置（直连）",
        effect: "http://host:port。提交链路走这个出口，本机直连 / 代理 / 隧道都行；非法值只报一次并按直连走",
        default_value: None,
        kind: Kind::Str,
    },
    Entry {
        name: "PACE_APP_VERSION",
        default: "7.4.20",
        effect: "上报的 appVersion，默认取真机版本（7.4.20 / code 411）。改它等于自报用别的客户端版本",
        default_value: Some("7.4.20"),
        kind: Kind::Str,
    },
    Entry {
        name: "PACE_DEVICE_MODEL",
        default: "未设置（cpuModel 发 ABI）",
        effect: "机型 code 或平台代号。命中 data/devices.json 时 cpuModel 发 SoC 型号（SM8550），未收录仍发 ABI 并提示",
        default_value: None,
        kind: Kind::Str,
    },
    Entry {
        name: "PACE_FORGE_DATA_DIR",
        default: "未设置（$HOME/.pace-forge）",
        effect: "账号库、session、devices 的落盘根目录；设错就会「账号全没了」",
        default_value: None,
        kind: Kind::Str,
    },
    Entry {
        name: "PACE_STEP_M",
        default: "5.0",
        effect: "CLI 的几何采样间距（米），只管形状不管上报节奏",
        default_value: Some("5.0"),
        kind: Kind::Float,
    },
];

/// 当前取值；未设置或空串返回 `None`。
pub fn current(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

/// 打印整张表（`pace-forge config`）。
pub fn print_table() {
    println!("PACE_* 开关（默认值 → 当前值 → 影响）\n");
    for e in ENTRIES {
        let raw = current(e.name);
        let mut flag = String::new();
        let shown = match &raw {
            None => e.default.to_string(),
            Some(v) if e.name.contains("TOKEN") => {
                flag = "  ← 非默认".to_string();
                // 口令不打印原文：终端截图、issue 贴日志都是常态。
                "已设置（不显示值）".to_string()
            }
            Some(v) => {
                if e.kind.accepts(v) {
                    if e.default_value.is_none() || e.default_value != Some(v.as_str()) {
                        flag = "  ← 非默认".to_string();
                    }
                    v.clone()
                } else {
                    // 设了但解析不了：运行时多半静默回落默认值，别让它看起来生效
                    flag = format!("  ← 无效值（实际用 {}）", effective_default(e));
                    v.clone()
                }
            }
        };
        println!("{} = {}{}", e.name, shown, flag);
        println!("    默认 {}：{}", e.default, e.effect);
    }
    println!("\n以上为全部开关。清单外的 PACE_* 不会被读取——以这张表为准。");
    println!("（`PACE_OUT`、`PACE_FAST` 这类是 preflight 检查码，不是环境变量，别混。）");
}

/// 无效值回落到哪个值：有可比较默认值就用它，没有就说明「默认行为」。
fn effective_default(e: &Entry) -> String {
    match e.default_value {
        Some(v) => v.to_string(),
        None => format!("默认行为（{}）", e.default),
    }
}

/// Web / CLI 启动时打一行「非默认项」。全默认返回 `None`。
pub fn non_default_line() -> Option<String> {
    let mut items: Vec<String> = Vec::new();
    for e in ENTRIES {
        let Some(v) = current(e.name) else { continue };
        if e.name.contains("TOKEN") {
            items.push(format!("{}=（已设置）", e.name));
        } else if !e.kind.accepts(&v) {
            items.push(format!("{}={v}（无效，回落默认）", e.name));
        } else if e.default_value.is_none() || e.default_value != Some(v.as_str()) {
            items.push(format!("{}={v}", e.name));
        }
    }
    if items.is_empty() {
        None
    } else {
        Some(items.join(", "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 清单里不能有重名——重名会让启动那行「非默认项」打印两次。
    #[test]
    fn entries_are_unique() {
        let mut names: Vec<&str> = ENTRIES.iter().map(|e| e.name).collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), ENTRIES.len(), "开关名重复");
    }

    /// 每个名字都以 PACE_ 开头，避免混进 HOME / USER 这类系统变量。
    #[test]
    fn entries_are_pace_prefixed() {
        for e in ENTRIES {
            assert!(e.name.starts_with("PACE_"), "{} 不是 PACE_ 前缀", e.name);
        }
    }
}

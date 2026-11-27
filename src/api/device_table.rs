//! 机型 → SoC 表（关 issue #18）。
//!
//! 背景：`cpuModel` 原来一直发 `arm64-v8a`（ABI），真机这里是 `SM8550`
//! 这类芯片代号。表分两层：
//!
//! - `platforms`：芯片平台代号（`ro.board.platform`）→ SoC 型号；
//! - `models`：机型 code → 平台代号。`src` 只用来记来源、方便复核，**可以留空**；
//!   留空的条目照样参与查表（`devices` 里会标「未标出处」）。
//!
//! 查表流程：机型 code / 平台代号 → SoC。查不到就退回 ABI，并在日志里说
//! 一句「未收录」——不静默换成别的值。
//!
//! 表是编译进二进制的（`include_str!`），不依赖运行目录，也不需要网络。

use crate::error::{Error, ErrorKind};
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::OnceLock;

/// 编译进二进制的人工表（平台 + 带出处的机型 SoC）。
const RAW: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/data/devices.json"));

/// 编译进二进制的官方目录（model code → codename / 营销名 / 品牌）。
///
/// 它**只用来认设备，不用来推 SoC**：官方目录里没有芯片字段，硬从 codename
/// 猜平台就是编。作用有两个——校验手填的 model code 是否真实存在、
/// 在 `devices` 里把 `2211133C` 显示成「Xiaomi 13 (fuxi)」。
const CATALOG: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/data/device_catalog.json"
));

/// 查不到表时发出去的 `cpuModel` 值。与历史行为一致（ABI），换设备字段
/// 不影响已有账号的 `DeviceId`。
pub const ABI_FALLBACK: &str = "arm64-v8a";

#[derive(Debug, Clone)]
/// 一个 SoC 平台。
pub struct Platform {
    /// SoC 型号，真机 `cpuModel` 就是这一层（`SM8550`）
    pub soc: String,
    /// 商品名（`骁龙 8 Gen 2`），只用于打印
    pub name: String,
    /// 厂商（`Qualcomm`），只用于打印。
    pub vendor: String,
}

#[derive(Debug, Clone)]
/// 一款机型。
pub struct Model {
    /// 平台代号（`kalama`），再查 `Platform`
    pub platform: String,
    /// 品牌（`Xiaomi`），只用于打印。
    pub brand: String,
    /// 出处。空字符串视为「不可信」，`resolve` 不会用它
    pub src: String,
}

#[derive(Debug, Default)]
/// 内置机型表：机型 → 平台 → SoC。
pub struct Table {
    /// 平台代号 → 平台信息。
    pub platforms: BTreeMap<String, Platform>,
    /// 机型代号 → 机型信息。
    pub models: BTreeMap<String, Model>,
}

/// 官方目录里的一条：只描述「这台设备叫什么」，不含芯片。
#[derive(Debug, Clone)]
pub struct CatalogEntry {
    /// 机型代号（`venus`）。
    pub codename: String,
    /// 商品名。
    pub name: String,
    /// 品牌。
    pub brand: String,
}

/// 目录解析一次。
pub fn catalog() -> &'static BTreeMap<String, CatalogEntry> {
    static C: OnceLock<BTreeMap<String, CatalogEntry>> = OnceLock::new();
    C.get_or_init(|| {
        let mut out = BTreeMap::new();
        let Ok(v) = serde_json::from_str::<Value>(CATALOG) else {
            return out;
        };
        if let Some(map) = v.get("devices").and_then(|x| x.as_object()) {
            for (k, val) in map {
                out.insert(
                    k.clone(),
                    CatalogEntry {
                        codename: val
                            .get("device")
                            .and_then(|x| x.as_str())
                            .unwrap_or("")
                            .to_string(),
                        name: val
                            .get("name")
                            .and_then(|x| x.as_str())
                            .unwrap_or("")
                            .to_string(),
                        brand: val
                            .get("brand")
                            .and_then(|x| x.as_str())
                            .unwrap_or("")
                            .to_string(),
                    },
                );
            }
        }
        out
    })
}

/// 在官方目录里按关键词搜：model code 前缀优先，其次营销名/品牌包含。
///
/// 返回 `(model_code, 条目)`，最多 `limit` 条。给 Web 的机型选择框用。
pub fn search_catalog(q: &str, limit: usize) -> Vec<(String, CatalogEntry)> {
    let q = q.trim().to_lowercase();
    if q.is_empty() || limit == 0 {
        return Vec::new();
    }
    let mut exact = Vec::new();
    let mut fuzzy = Vec::new();
    for (code, e) in catalog() {
        let code_l = code.to_lowercase();
        if code_l.starts_with(&q) {
            exact.push((code.clone(), e.clone()));
        } else if e.name.to_lowercase().contains(&q) || e.brand.to_lowercase().contains(&q) {
            fuzzy.push((code.clone(), e.clone()));
        }
        if exact.len() >= limit && fuzzy.len() >= limit {
            break;
        }
    }
    exact.extend(fuzzy);
    exact.truncate(limit);
    exact
}

/// 品牌列表：`(品牌名原文, 机型数)`，按机型数降序、同名按字母序。
pub fn brands() -> Vec<(String, usize)> {
    // 按小写合并：官方目录里 `Vivo` / `vivo` 是两个品牌名，同属一家，列表
    // 里出现两次会让人以为选错了。显示名取第一次出现的那种写法。
    let mut m: BTreeMap<String, (String, usize)> = BTreeMap::new();
    for e in catalog().values() {
        let key = e.brand.to_lowercase();
        let slot = m.entry(key).or_insert_with(|| (e.brand.clone(), 0));
        slot.1 += 1;
    }
    let mut v: Vec<(String, usize)> = m.into_iter().map(|(_, (name, n))| (name, n)).collect();
    v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    v
}

/// 某个品牌下的机型：按营销名去重（国行/国际常是同一个名字多个 code），
/// 返回 `(model code, 条目)`。
pub fn models_of_brand(brand: &str, limit: usize) -> Vec<(String, CatalogEntry)> {
    let b = brand.trim().to_lowercase();
    let mut out: Vec<(String, CatalogEntry)> = Vec::new();
    for (code, e) in catalog() {
        if e.brand.to_lowercase() != b {
            continue;
        }
        if out.iter().any(|(_, x)| x.name == e.name) {
            continue;
        }
        out.push((code.clone(), e.clone()));
    }
    // 按机型名排序，稳定、可预期。试过「名字里数字越大越新」的排法，但官方
    // 目录里混着投影仪（`Mi Laser Projector 150"`）和型号数字（`Redmi 10 2022`），
    // 数字排序反而把不相干的顶到最前。定位靠搜索框，不靠排序猜。
    out.sort_by(|a, b| {
        a.1.name
            .to_lowercase()
            .cmp(&b.1.name.to_lowercase())
            .then_with(|| a.0.cmp(&b.0))
    });
    out.truncate(limit);
    out
}

/// 平台下拉用的列表：`(平台代号, SoC, 商品名, 厂商)`。
pub fn platforms() -> Vec<(String, String, String, String)> {
    table()
        .platforms
        .iter()
        .map(|(code, p)| {
            (
                code.clone(),
                p.soc.clone(),
                p.name.clone(),
                p.vendor.clone(),
            )
        })
        .collect()
}

/// 面板默认展示：常见品牌里按 code 排一批，给用户一个可点的地方。
///
/// 不做「热门」排序——没有可靠的机型销量数据，排出来就是编。按品牌 + code
/// 稳定列举，用户看到的是「官方目录里确实存在的机型」。
pub fn suggested(limit: usize) -> Vec<(String, CatalogEntry)> {
    const BRANDS: [&str; 4] = ["xiaomi", "redmi", "iqoo", "oneplus"];
    let mut out = Vec::new();
    for brand in BRANDS {
        let mut picked = 0;
        for (code, e) in catalog() {
            if e.brand.to_lowercase() != brand {
                continue;
            }
            // 同一机型常有多个 code（国行/国际），按名字去一次重
            if out
                .iter()
                .any(|(_, x): &(String, CatalogEntry)| x.name == e.name)
            {
                continue;
            }
            out.push((code.clone(), e.clone()));
            picked += 1;
            if picked >= 3 {
                break;
            }
            if out.len() >= limit {
                return out;
            }
        }
        if out.len() >= limit {
            break;
        }
    }
    out.truncate(limit);
    out
}

/// 按 model code 查官方目录。查不到就是「目录里没有这台」。
pub fn catalog_lookup(model: &str) -> Option<&'static CatalogEntry> {
    catalog().get(model.trim())
}

fn parse_table(raw: &str) -> Table {
    let mut t = Table::default();
    let Ok(v) = serde_json::from_str::<Value>(raw) else {
        return t;
    };
    if let Some(p) = v.get("platforms").and_then(|x| x.as_object()) {
        for (k, val) in p {
            let Some(soc) = val.get("soc").and_then(|x| x.as_str()) else {
                continue;
            };
            t.platforms.insert(
                k.clone(),
                Platform {
                    soc: soc.to_string(),
                    name: val
                        .get("name")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .to_string(),
                    vendor: val
                        .get("vendor")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .to_string(),
                },
            );
        }
    }
    if let Some(m) = v.get("models").and_then(|x| x.as_object()) {
        for (k, val) in m {
            let Some(platform) = val.get("platform").and_then(|x| x.as_str()) else {
                continue;
            };
            t.models.insert(
                k.clone(),
                Model {
                    platform: platform.to_string(),
                    brand: val
                        .get("brand")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .to_string(),
                    src: val
                        .get("src")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .to_string(),
                },
            );
        }
    }
    t
}

/// 用户级机型映射（运行时覆盖层）：`<数据目录>/device_models.json`。
///
/// 内置表是编译进二进制的，改不了；但「我这台机器是 2211133C、平台 kalama」
/// 这种映射属于本机事实，应该能就地记下来。格式与内置 `models` 同构：
/// `{"<model code>": {"platform": "<ro.board.platform>", "src": "..."}}`。
fn user_models_path() -> std::path::PathBuf {
    crate::api::session::data_dir().join("device_models.json")
}

fn load_user_models() -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let Ok(raw) = std::fs::read_to_string(user_models_path()) else {
        return out;
    };
    let Ok(v) = serde_json::from_str::<Value>(&raw) else {
        return out;
    };
    if let Some(m) = v.get("models").and_then(|x| x.as_object()) {
        for (k, val) in m {
            if let Some(p) = val.get("platform").and_then(|x| x.as_str()) {
                out.insert(k.clone(), p.to_string());
            }
        }
    }
    out
}

/// 用户层缓存的唯一实例。写入方和读取方必须拿到同一个——各自声明一个
/// 函数内 `static` 是两份独立缓存，写完了读侧还是旧值。
fn user_cache() -> &'static std::sync::Mutex<Option<BTreeMap<String, String>>> {
    static CACHE: OnceLock<std::sync::Mutex<Option<BTreeMap<String, String>>>> = OnceLock::new();
    CACHE.get_or_init(|| std::sync::Mutex::new(None))
}

/// 用户层映射（进程内缓存，写入后刷新）。
fn user_models() -> BTreeMap<String, String> {
    let lock = user_cache();
    let mut g = lock.lock().unwrap_or_else(|p| p.into_inner());
    if g.is_none() {
        *g = Some(load_user_models());
    }
    g.clone().unwrap_or_default()
}

/// 记一条用户级映射并刷缓存。`platform` 必须在内置 `platforms` 里。
pub fn set_user_model(model: &str, platform: &str) -> Result<(), Error> {
    let model = model.trim();
    let platform = platform.trim();
    if model.is_empty() || platform.is_empty() {
        return Err(ErrorKind::client("机型 code 和平台代号都要填".into()));
    }
    if soc_for_platform(platform).is_none() {
        return Err(ErrorKind::client(format!(
            "平台代号 `{platform}` 不在内置表里（可选：{}）",
            table()
                .platforms
                .keys()
                .cloned()
                .collect::<Vec<_>>()
                .join(" / ")
        )));
    }
    let path = user_models_path();
    let mut all: Value = std::fs::read_to_string(&path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_else(|| serde_json::json!({ "models": {} }));
    all["models"][model] = serde_json::json!({ "platform": platform, "src": "web" });
    let text = serde_json::to_string_pretty(&all)?;
    crate::api::session::write_file_private(&path, text.as_bytes())?;
    // 刷新缓存（与读取侧共用 user_cache()）
    {
        let lock = user_cache();
        let mut g = lock.lock().unwrap_or_else(|p| p.into_inner());
        *g = Some(load_user_models());
    }
    Ok(())
}

/// 全局表，解析一次。
pub fn table() -> &'static Table {
    static T: OnceLock<Table> = OnceLock::new();
    T.get_or_init(|| parse_table(RAW))
}

/// 机型 code → SoC 型号。
///
/// 查找顺序：用户层（`device_models.json`）→ 内置 `models` 层。
/// `src` 留空不影响结果：出处是给复核用的，不是准入门槛。
pub fn soc_for_model(model: &str) -> Option<String> {
    let model = model.trim();
    if let Some(platform) = user_models().get(model)
        && let Some(soc) = soc_for_platform(platform)
    {
        return Some(soc.to_string());
    }
    let t = table();
    let m = t.models.get(model)?;
    t.platforms.get(&m.platform).map(|p| p.soc.to_string())
}

/// 平台代号 → SoC 型号（表里没有就是没有）。
pub fn soc_for_platform(platform: &str) -> Option<&'static str> {
    table().platforms.get(platform).map(|p| p.soc.as_str())
}

/// 实际发出去的 `cpuModel`：
///
/// 1. 配置 `device.model`（env `PACE_DEVICE_MODEL`）有值且能查到 → SoC 型号；
/// 2. 查不到 → `arm64-v8a`，并在 stderr 说一句「未收录」（同一个值只报一次，
///    批量提交时不刷屏）。
pub fn cpu_model() -> String {
    let cfg = crate::config::global();
    let model = cfg.device.model.trim();
    if model.is_empty() {
        return ABI_FALLBACK.to_string();
    }
    // 先按机型 code 查 models，查不到再按平台代号直接查——`device.model = "kalama"`
    // 这种「我只知道芯片」的用法也要能用，否则表里那层平台数据白摆着。
    match soc_for_model(model).or_else(|| soc_for_platform(model).map(str::to_string)) {
        Some(soc) => soc,
        None => {
            warn_once(&format!(
                "PACE_DEVICE_MODEL={model} 未收录（models 层没有带出处的对应项），cpuModel 仍发 {ABI_FALLBACK}"
            ));
            ABI_FALLBACK.to_string()
        }
    }
}

fn warn_once(msg: &str) {
    use std::sync::Mutex;
    static SEEN: OnceLock<Mutex<Vec<String>>> = OnceLock::new();
    let seen = SEEN.get_or_init(|| Mutex::new(Vec::new()));
    let mut g = seen.lock().unwrap_or_else(|p| p.into_inner());
    if g.iter().any(|m| m == msg) {
        return;
    }
    g.push(msg.to_string());
    eprintln!("[device] {msg}");
}

/// `pace-forge devices` 的输出：表内容 + 覆盖率 + 补表方法。
pub fn report() -> String {
    let t = table();
    let mut out = String::new();
    out.push_str(&format!(
        "[表] 平台 {} 条，机型 {} 条（编译进二进制，离线可用）\n",
        t.platforms.len(),
        t.models.len()
    ));
    out.push_str("\n平台 → SoC：\n");
    for (code, p) in &t.platforms {
        out.push_str(&format!(
            "  {code:<11} {:<8} {}（{}）\n",
            p.soc, p.name, p.vendor
        ));
    }
    if t.models.is_empty() {
        out.push_str("\n机型表为空：\n");
        out.push_str("  只有平台层能直接用（如 PACE_DEVICE_MODEL 走平台代号），具体机型还没收。\n");
        out.push_str(
            "  收法：真机跑 `adb shell getprop ro.product.model` + `getprop ro.board.platform`，\n",
        );
        out.push_str("        把两行结果连同出处（截图/日期）写进 data/devices.json 的 models。\n");
    } else {
        out.push_str("\n机型 → 平台：\n");
        for (code, m) in &t.models {
            out.push_str(&format!(
                "  {code:<14} → {:<11} {}（出处：{}）\n",
                m.platform, m.brand, m.src
            ));
        }
    }
    out.push_str(&format!(
        "\n目录：官方 model code {} 条（Google Play 设备目录，中国品牌子集）\n",
        catalog().len()
    ));
    let cur = cpu_model();
    let model = crate::config::global().device.model.clone();
    let model = model.trim();
    // 已经按平台代号命中 SoC 时不必再说「目录未命中」——平台代号本来就不在
    // 官方 model code 目录里，那句话只会变成噪音。
    if cur == ABI_FALLBACK && !model.is_empty() {
        match catalog_lookup(model) {
            Some(e) => out.push_str(&format!(
                "  目录命中：{model} → {} {}（codename {}）\n",
                e.brand, e.name, e.codename
            )),
            None => out.push_str(&format!(
                "  目录未命中：{model}（不是已知官方 model code，可能是笔误或平台代号）\n"
            )),
        }
    }
    out.push_str(&format!("\n[当前] cpuModel 实际发：{cur}\n"));
    if cur == ABI_FALLBACK {
        out.push_str(
            "        （ABI 兜底。设 device.model = <机型或平台代号>（env PACE_DEVICE_MODEL）才会发 SoC 型号）\n",
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_parses_and_has_platforms() {
        let t = table();
        assert!(
            t.platforms.len() >= 10,
            "平台表应有内容: {}",
            t.platforms.len()
        );
        assert_eq!(soc_for_platform("kalama"), Some("SM8550"));
        assert_eq!(soc_for_platform("mt6989"), Some("MT6989"));
        assert_eq!(soc_for_platform("不存在的平台"), None);
    }

    #[test]
    fn model_without_source_still_resolves() {
        // 出处只用于复核，不是准入门槛：填了机型 code 就能查表
        let t = parse_table(
            r#"{"platforms":{"kalama":{"soc":"SM8550","name":"","vendor":""}},
                "models":{"X1":{"platform":"kalama","brand":"b","src":""}}}"#,
        );
        let m = t.models.get("X1").expect("条目要在");
        assert!(m.src.trim().is_empty(), "src 可以为空");
        let soc = t.platforms.get(&m.platform).map(|p| p.soc.as_str());
        assert_eq!(soc, Some("SM8550"), "没标出处也要能解析出 SoC");
        // 用户层覆盖：内置表里没有的能就地补
        // （测试里不写盘，只验查找顺序靠前的分支会在 set_user_model 里校验平台）
    }

    #[test]
    fn default_model_falls_back_to_abi() {
        // 无配置来源时行为与历史一致
        assert_eq!(cpu_model(), ABI_FALLBACK);
        assert_eq!(ABI_FALLBACK, "arm64-v8a");
    }

    #[test]
    fn platform_code_also_works_as_model() {
        // 只知道芯片时可以直接填平台代号
        let soc =
            soc_for_model("kalama").or_else(|| soc_for_platform("kalama").map(str::to_string));
        assert_eq!(soc.as_deref(), Some("SM8550"));
        assert_eq!(soc_for_platform("mt6989"), Some("MT6989"));
    }

    #[test]
    fn catalog_identifies_real_devices() {
        let c = catalog();
        assert!(c.len() > 4000, "官方目录应有数千条: {}", c.len());
        let e = catalog_lookup("2211133C").expect("Xiaomi 13 的 model code 必须在目录里");
        assert_eq!(e.codename, "fuxi");
        assert!(e.name.contains("Xiaomi 13"), "{}", e.name);
        let one = catalog_lookup("PJD110").expect("OnePlus 12");
        assert!(one.name.contains("OnePlus 12"), "{}", one.name);
        assert!(catalog_lookup("NOT_A_CODE").is_none());
    }

    /// 目录只用来认设备，不能反过来推 SoC。
    #[test]
    fn catalog_does_not_imply_soc() {
        // 目录里有这台机器，但 models 层（带出处的 SoC）还是空的
        assert!(catalog_lookup("2211133C").is_some());
        assert_eq!(
            soc_for_model("2211133C"),
            None,
            "目录里没有平台映射，仍给 ABI"
        );
        assert_eq!(soc_for_model("fuxi"), None, "目录不含 codename 映射");
    }

    #[test]
    fn search_finds_by_code_and_name() {
        // code 前缀
        let by_code = search_catalog("2211133", 5);
        assert!(
            by_code.iter().any(|(c, _)| c == "2211133C"),
            "按 code 搜不到: {:?}",
            by_code.iter().map(|(c, _)| c).collect::<Vec<_>>()
        );
        // 营销名
        let by_name = search_catalog("xiaomi 13", 10);
        assert!(
            by_name.iter().any(|(_, e)| e.name.contains("Xiaomi 13")),
            "按名字搜不到"
        );
        // 空查询不要返回全表
        assert!(search_catalog("", 10).is_empty());
        assert!(search_catalog("   ", 10).is_empty());
    }

    /// 写入用户层后，同一进程里立刻要能查到（缓存必须共用一份）。
    ///
    /// 会用真实数据目录，所以跑完把原文件恢复回去——测试不该在用户目录里
    /// 留一条假机型。
    #[test]
    fn user_model_takes_effect_immediately() {
        let path = user_models_path();
        let orig = std::fs::read_to_string(&path).ok();

        set_user_model("UNIT_TEST_MODEL", "kalama").expect("写用户层");
        assert_eq!(
            soc_for_model("UNIT_TEST_MODEL").as_deref(),
            Some("SM8550"),
            "写完立刻要生效"
        );
        // 平台代号不在内置表里要报错，不能默默写进去
        assert!(set_user_model("UNIT_TEST_MODEL", "不存在的平台").is_err());

        match orig {
            Some(t) => {
                let _ = crate::api::session::write_file_private(&path, t.as_bytes());
            }
            None => {
                let _ = std::fs::remove_file(&path);
            }
        }
        // 缓存也刷回磁盘现状
        {
            let lock = user_cache();
            let mut g = lock.lock().unwrap_or_else(|p| p.into_inner());
            *g = Some(load_user_models());
        }
    }

    #[test]
    fn brands_and_brand_models_are_grouped() {
        let bs = brands();
        assert!(bs.len() >= 6, "品牌数太少: {bs:?}");
        // 大小写合并后不该出现两个 Vivo
        let vivo: Vec<&String> = bs
            .iter()
            .map(|(n, _)| n)
            .filter(|n| n.eq_ignore_ascii_case("vivo"))
            .collect();
        assert_eq!(vivo.len(), 1, "品牌名大小写要合并: {bs:?}");
        let xiaomi = bs
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case("xiaomi"))
            .expect("要有小米");
        assert!(xiaomi.1 > 100, "小米机型数: {}", xiaomi.1);
        let list = models_of_brand("Xiaomi", 200);
        // 排序稳定：同一品牌的列表按名字不区分大小写升序
        let names: Vec<String> = list
            .iter()
            .take(8)
            .map(|(_, e)| e.name.to_lowercase())
            .collect();
        assert!(
            names.windows(2).all(|w| w[0] <= w[1]),
            "排序不对: {names:?}"
        );
        assert!(!list.is_empty());
        assert!(
            list.iter()
                .all(|(_, e)| e.brand.eq_ignore_ascii_case("xiaomi"))
        );
        // 名字去重
        let mut names: Vec<&str> = list.iter().map(|(_, e)| e.name.as_str()).collect();
        let n = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), n, "同一营销名只该出现一次");
        assert!(models_of_brand("不存在的品牌", 10).is_empty());
        assert_eq!(platforms().len(), table().platforms.len());
    }

    #[test]
    fn suggested_is_not_empty_and_deduped() {
        let list = suggested(8);
        assert!(!list.is_empty(), "面板默认要有点东西可点");
        let mut names: Vec<&str> = list.iter().map(|(_, e)| e.name.as_str()).collect();
        let n = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), n, "同一机型不该重复出现");
    }

    #[test]
    fn report_is_readable() {
        let r = report();
        assert!(r.contains("平台 → SoC"));
        assert!(r.contains("cpuModel"));
    }
}

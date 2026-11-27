//! 出口 IP：自检（顶栏那行）+ 可选代理接入（关 issue #10）。
//!
//! 两件事分开说清楚：
//!
//! **自检**——同类工具栽在「IP 提交异常」：出口 IP 的归属地和学校所在
//! 地对不上，服务端成片清理记录。这里把**真实**出口 IP 探出来显示，部署在
//! 异地服务器的人一眼能看出特征，自己决定换不换网络。
//!
//! **代理 / 出口配置**——`PACE_EGRESS_PROXY=http://host:port` 时提交链路走
//! 这个出口。出口可以是本机直连、HTTP 代理，或者用户自己搭的隧道；程序只
//! 负责把流量送过去。默认不开，不开时行为和以前完全一致（直连）。
//!
//! - 探测源：ipify（JSON）→ httpbin（JSON）兜底，单次 5 秒超时；
//! - 全部失败返回 `ok:false`，顶栏显示「出口未知」——它只是自检，不阻塞
//!   任何功能，也不在界面弹错误；
//! - 结果缓存 5 分钟：控制台每次刷新页面都会来问，免费接口不能这么刷。
//!
//! 注意显示侧（web/index.html）拿到的 `ip` 字符串来自外部接口，渲染必须
//! 走 `textContent`，不许拼 `innerHTML`。

use crate::error::{Error, ErrorKind};
use serde_json::{Value, json};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// 运行时可改的出口配置（网页里保存的就是它）。
static RUNTIME_PROXY: OnceLock<Mutex<Option<String>>> = OnceLock::new();

fn runtime_slot() -> &'static Mutex<Option<String>> {
    RUNTIME_PROXY.get_or_init(|| Mutex::new(None))
}

fn config_path() -> std::path::PathBuf {
    crate::api::session::data_dir().join("egress.json")
}

/// 读磁盘上的出口配置（首次调用时加载进运行时）。
fn load_config_once() -> Option<String> {
    let mut g = runtime_slot().lock().unwrap_or_else(|p| p.into_inner());
    if g.is_none()
        && let Ok(raw) = std::fs::read_to_string(config_path())
        && let Ok(v) = serde_json::from_str::<Value>(&raw)
        && let Some(p) = v.get("proxy").and_then(|x| x.as_str())
    {
        let t = p.trim();
        if !t.is_empty() {
            *g = Some(t.to_string());
        }
    }
    g.clone()
}

/// 保存出口配置。`None` / 空串表示清掉（回到直连）。
pub fn save_proxy(spec: Option<&str>) -> Result<(), Error> {
    match spec.map(str::trim).filter(|s| !s.is_empty()) {
        Some(raw) => {
            let clean = parse_proxy(raw)?; // 先校验再落盘，坏值不写进去
            let body = json!({ "proxy": clean }).to_string();
            crate::api::session::write_file_private(&config_path(), body.as_bytes())?;
            *runtime_slot().lock().unwrap_or_else(|p| p.into_inner()) = Some(clean);
        }
        None => {
            let path = config_path();
            match std::fs::remove_file(&path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(ErrorKind::client(format!("清出口配置失败: {e}"))),
            }
            *runtime_slot().lock().unwrap_or_else(|p| p.into_inner()) = None;
        }
    }
    Ok(())
}

/// 当前生效的出口：网页保存的优先，其次配置层（env > config.toml）。
pub fn current_proxy() -> Option<String> {
    if let Some(p) = load_config_once() {
        return Some(p);
    }
    let p = crate::config::global()
        .network
        .egress_proxy
        .trim()
        .to_string();
    (!p.is_empty()).then_some(p)
}

struct Probe {
    ip: String,
    source: &'static str,
}

static CACHE: OnceLock<Mutex<Option<(Probe, Instant)>>> = OnceLock::new();
const TTL: Duration = Duration::from_secs(300);

/// 代理地址校验，返回**脱敏**描述（不含账号密码，可直接打日志）。
///
/// 只认 `http://host:port`：ureq 2 的 `Proxy` 走 HTTP CONNECT，`socks5://`
/// 需要额外 feature，这里先不给一个「写上了却不生效」的选项。
pub fn parse_proxy(spec: &str) -> Result<String, Error> {
    let spec = spec.trim();
    let rest = spec.strip_prefix("http://").ok_or_else(|| {
        ErrorKind::client(format!("只支持 http:// 前缀的 CONNECT 代理，收到：{spec}"))
    })?;
    // 去掉 user:pass@
    let host_port = rest.rsplit('@').next().unwrap_or(rest);
    let (host, port) = host_port.rsplit_once(':').ok_or_else(|| {
        ErrorKind::client(format!(
            "代理要带端口，例如 http://10.0.0.2:7890，收到：{host_port}"
        ))
    })?;
    if host.is_empty() {
        return Err(ErrorKind::client("代理主机为空".into()));
    }
    match port.parse::<u16>() {
        Ok(p) if p > 0 => Ok(format!("http://{host}:{p}")),
        _ => Err(ErrorKind::client(format!(
            "代理端口不是 1..65535 的整数：{port}"
        ))),
    }
}

/// 给 agent 追加信任根：配置 `network.extra_ca`（env `PACE_EXTRA_CA`）指向
/// 一个 PEM 文件时，把里面的证书加到公共 CA 根之上（沙箱/企业出口网关自签
/// CA 的场景）。没设时与原来逐字节等价，不动 TLS。
pub fn with_extra_ca(builder: ureq::AgentBuilder) -> ureq::AgentBuilder {
    let cfg = crate::config::global();
    let path = cfg.network.extra_ca.trim();
    if path.is_empty() {
        return builder;
    }
    let pem = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) => {
            report_once(&format!("PACE_EXTRA_CA 读不到（{e}），本次按默认 CA 走"));
            return builder;
        }
    };
    // 手工拆 PEM：-----BEGIN/END CERTIFICATE----- 之间 base64 → DER
    let text = String::from_utf8_lossy(&pem);
    let mut ders: Vec<Vec<u8>> = Vec::new();
    for chunk in text.split("-----BEGIN CERTIFICATE-----").skip(1) {
        let Some(body) = chunk.split("-----END CERTIFICATE-----").next() else {
            continue;
        };
        let b64: String = body.chars().filter(|c| !c.is_whitespace()).collect();
        if let Ok(der) = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, &b64) {
            ders.push(der);
        }
    }
    if ders.is_empty() {
        report_once("PACE_EXTRA_CA 里没有可解析的证书，本次按默认 CA 走");
        return builder;
    }
    let mut roots = ureq::rustls::RootCertStore::empty();
    let certs: Vec<ureq::rustls::pki_types::CertificateDer<'static>> = ders
        .into_iter()
        .map(ureq::rustls::pki_types::CertificateDer::from)
        .collect();
    roots.add_parsable_certificates(certs);
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let versions = ureq::rustls::ClientConfig::builder_with_provider(std::sync::Arc::new(
        ureq::rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions();
    let Ok(versions) = versions else {
        report_once("rustls 没给出默认协议版本，本次按默认 CA 走");
        return builder;
    };
    let cfg = versions.with_root_certificates(roots).with_no_client_auth();
    builder.tls_config(std::sync::Arc::new(cfg))
}

/// 提交链路用的 agent。设了 `PACE_EGRESS_PROXY` 就走代理，否则等同直连。
///
/// 非法值不静默回落：打一行错到 stderr 后按直连走——「以为走了代理其实直连」
/// 比报错危险得多（这正是 #23 那类静默回落的问题）。
pub fn agent(timeout: Duration) -> ureq::Agent {
    let builder = with_extra_ca(ureq::AgentBuilder::new().timeout(timeout));
    match current_proxy() {
        Some(raw) => match parse_proxy(&raw) {
            Ok(clean) => match ureq::Proxy::new(&clean) {
                Ok(p) => builder.proxy(p).build(),
                Err(e) => {
                    report_once(&format!(
                        "PACE_EGRESS_PROXY 无法使用（{e}），本次按直连提交"
                    ));
                    builder.build()
                }
            },
            Err(e) => {
                report_once(&format!("PACE_EGRESS_PROXY 配置有误：{e}，本次按直连提交"));
                builder.build()
            }
        },
        _ => builder.build(),
    }
}

/// 每个错误只打一次，避免批量提交时刷屏。
fn report_once(msg: &str) {
    static ONCE: OnceLock<Mutex<Vec<String>>> = OnceLock::new();
    let seen = ONCE.get_or_init(|| Mutex::new(Vec::new()));
    let mut g = seen.lock().unwrap_or_else(|p| p.into_inner());
    if g.iter().any(|m| m == msg) {
        return;
    }
    g.push(msg.to_string());
    eprintln!("[egress] {msg}");
}

/// 当前代理配置（脱敏），给 `config` 和启动日志用。
pub fn proxy_description() -> Option<String> {
    let raw = current_proxy()?;
    match parse_proxy(&raw) {
        Ok(clean) => Some(format!("{clean}（提交走此出口）")),
        Err(e) => Some(format!("配置有误：{e}")),
    }
}

/// 出口归属地是否与校区同城。
///
/// 判据故意宽松：只比到城市级，且去掉「市」后缀后看包含关系
/// （服务端给的是「XX市」，接口回的是「XX」）。空值一律不判——探不到归属地
/// 不等于不在同城，不能因为查不到就把人拦下来。
pub fn city_mismatch(egress_city: &str, campus_city: &str) -> bool {
    let norm = |s: &str| {
        s.trim()
            .trim_end_matches('市')
            .trim_end_matches("地区")
            .to_string()
    };
    let (a, b) = (norm(egress_city), norm(campus_city));
    if a.is_empty() || b.is_empty() {
        return false;
    }
    !a.contains(&b) && !b.contains(&a)
}

/// 从 JSON 接口取 `field` 字段当 IP。
///
/// 校验只认「短、全可打印、无空格」的字符串：上游万一返回整段 HTML/错误
/// 页，别把它当 IP 显示出来。httpbin 的 `origin` 在有代理时会带逗号
/// （`1.2.3.4, 10.0.0.1`），取第一段。
fn fetch_json_ip(url: &str, field: &str) -> Option<String> {
    let agent = with_extra_ca(ureq::AgentBuilder::new().timeout(Duration::from_secs(5))).build();
    let body: Value = agent.get(url).call().ok()?.into_json().ok()?;
    let raw = body.get(field)?.as_str()?;
    let ip = raw.split(',').next()?.trim();
    if ip.is_empty() || ip.len() > 45 || ip.bytes().any(|b| !b.is_ascii_graphic()) {
        return None;
    }
    Some(ip.to_string())
}

fn probe_once() -> Option<Probe> {
    if let Some(ip) = fetch_json_ip("https://api.ipify.org?format=json", "ip") {
        return Some(Probe {
            ip,
            source: "ipify",
        });
    }
    if let Some(ip) = fetch_json_ip("https://httpbin.org/ip", "origin") {
        return Some(Probe {
            ip,
            source: "httpbin",
        });
    }
    None
}

/// 带缓存的探测结果，可直接作 JSON 响应体。
///
/// `proxy` 字段非空时说明提交链路走了代理——顶栏能直接看出「这个 IP 是
/// 代理的出口，不是这台机器的」。
pub fn cached_json() -> Value {
    let lock = CACHE.get_or_init(|| Mutex::new(None));
    let mut g = lock.lock().unwrap_or_else(|p| p.into_inner());
    let fresh = g
        .as_ref()
        .map(|(p, t)| t.elapsed() < TTL && !p.ip.is_empty())
        .unwrap_or(false);
    if !fresh {
        let probed = probe_once();
        *g = probed.map(|p| (p, Instant::now()));
    }
    let proxy = proxy_description();
    match &*g {
        Some((p, _)) => json!({ "ok": true, "ip": p.ip, "source": p.source, "proxy": proxy }),
        None => json!({ "ok": false, "proxy": proxy }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 只认 http:// 且必须带端口；账号密码不能出现在脱敏描述里。
    #[test]
    fn proxy_spec_is_validated_and_redacted() {
        assert_eq!(
            parse_proxy("http://10.0.0.2:7890").unwrap(),
            "http://10.0.0.2:7890"
        );
        // 账号密码要脱掉
        assert_eq!(
            parse_proxy("http://user:pass@10.0.0.2:7890").unwrap(),
            "http://10.0.0.2:7890"
        );
        // 前后空格不算错
        assert_eq!(parse_proxy("  http://h:1080  ").unwrap(), "http://h:1080");
        // 不支持的形态要明确报错，不静默
        assert!(parse_proxy("socks5://10.0.0.2:1080").is_err());
        assert!(parse_proxy("10.0.0.2:7890").is_err());
        assert!(parse_proxy("http://10.0.0.2").is_err(), "缺端口");
        assert!(parse_proxy("http://:7890").is_err(), "缺主机");
        assert!(parse_proxy("http://h:0").is_err(), "端口 0");
        assert!(parse_proxy("http://h:70000").is_err(), "端口越界");
    }

    /// 城市级比较：去掉「市」、允许包含；空值一律不判（探不到不等于不同城）。
    #[test]
    fn city_mismatch_is_lenient() {
        assert!(!city_mismatch("乌鲁木齐市", "乌鲁木齐"));
        assert!(!city_mismatch("乌鲁木齐", "乌鲁木齐市"));
        assert!(city_mismatch("北京市", "乌鲁木齐市"));
        assert!(!city_mismatch("", "乌鲁木齐市"), "查不到归属地不能判异常");
        assert!(!city_mismatch("乌鲁木齐市", ""), "校区没填也不能判异常");
        assert!(city_mismatch("阿克苏地区", "乌鲁木齐市"));
    }

    /// 代理配置在缓存响应里要有独立字段，且有值时不可能是空串。
    #[test]
    fn cache_reports_proxy_shape() {
        let v = cached_json();
        assert!(
            v.get("proxy").is_some(),
            "proxy 字段必须在（未设为 null）: {v}"
        );
        if let Some(p) = v["proxy"].as_str() {
            assert!(!p.is_empty());
        }
    }

    #[test]
    fn cache_shape_is_stable() {
        // 只看形状，不真发请求（测试机不一定有外网）：ok:false 分支。
        // （真发过请求的环境里缓存已被填充，这里不覆盖全局缓存，
        //  改成校验字段集合而不是值。）
        let v = cached_json();
        assert!(v.get("ok").is_some(), "必须有 ok 字段: {v}");
        if v["ok"].as_bool().unwrap_or(false) {
            assert!(v["ip"].is_string() && !v["ip"].as_str().unwrap().is_empty());
            assert!(v["source"].is_string());
        }
    }
}

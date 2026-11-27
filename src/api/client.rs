//! 协议客户端：设备指纹、请求头签名、收发与解密。
//!
//! 一次请求的完整形态：
//!
//! ```text
//! 设备头 + timeStamp + nonce + uid + token
//!   → tokenSign   = MD5(排序拼接 + 盐)
//!   → headerSign  = seal(上述全部头组成的 JSON)   ← 注意：是把 header 自身加密
//!   → body        = seal(业务 JSON)
//! ```
//!
//! `headerSign` 不是签名而是加密信封，这点最容易搞错。见 `crypto::envelope`。

use serde_json::Value;

use crate::crypto::envelope::{SessionKeys, seal};
use crate::crypto::{SignFields, decrypt, sign::token_sign};
use crate::error::{Error, ErrorKind};
use sha2::{Digest, Sha256};

/// 可选硬件标识头的开关。
///
/// **默认发空串**——7.4.21 的 `HttpUtil.getHeaderMap` 里这五个是硬编码的：
///
/// ```java
/// arrayMap2.put("IMEI", "");
/// arrayMap2.put("androidId", "");
/// arrayMap2.put("blMac", "");
/// arrayMap2.put("wifiMac", "");
/// arrayMap2.put("CustomDeviceId", "");
/// ```
///
/// 注意 `CustomDeviceId` 也在内：方法前面虽然 `Utils.OoooOo0(context, ...)`
/// 生成过一个值存进缓存，但最后 put 进头的仍是 `""`。
///
/// 要发派生假值做对照时设 `device.hw_headers = "derived"`（env
/// `PACE_DEVICE_HW_HEADERS=derived`）。
fn hw_id(value: &str) -> String {
    match crate::config::global().device.hw_headers {
        crate::config::HwHeaders::Derived => value.to_string(),
        crate::config::HwHeaders::Empty => String::new(),
    }
}

/// IMEI 的 Luhn 校验位。
///
/// 从右往左数第二位开始，每隔一位翻倍，超过 9 就减 9 再求和；校验位是
/// 让总和被 10 整除的那个数字。前 14 位传入，返回第 15 位。
fn luhn_check_digit(body: &[u8]) -> u8 {
    let mut sum: u32 = 0;
    for (i, d) in body.iter().rev().enumerate() {
        let mut v = *d as u32;
        if i % 2 == 0 {
            v *= 2;
            if v > 9 {
                v -= 9;
            }
        }
        sum += v;
    }
    ((10 - sum % 10) % 10) as u8
}

/// 设备指纹。首次运行随机生成后应持久化，否则容易被判为异常设备。
#[derive(Debug, Clone)]
pub struct DeviceProfile {
    /// App 版本号，要和服务端校验的那一套对得上。
    pub app_version: String,
    /// 物理分辨率，形如 `1080x2400`。
    pub physic_pixel: String,
    /// 逻辑分辨率。
    pub logic_pixel: String,
    /// 是否 root：`0` / `1`。
    pub is_root: String,
    /// 系统类型（`android`）。
    pub os_type: String,
    /// SoC 型号，对应机型表里的 `soc`。
    pub cpu_model: String,
    /// 设备名（`Pixel 6` 这类）。
    pub device_name: String,
    /// 设备唯一 id。
    pub device_id: String,
    /// 系统版本（`13`）。
    pub os_version: String,
    /// 请求头里的 UA。
    pub user_agent: String,
    /// 安卓安装 ID：真机是 16 位小写 hex。
    ///
    /// 真机这八项是从系统 API 直接读的，服务端把它们当设备指纹的一部分。
    /// 「缺失会被打上来源可疑标记」这条**没有证据**——既没有客户端逻辑、
    /// 也没有回执或对照实验支撑，只有同类工具的口头经验（issue #17 还给了
    /// 反例：官方客户端这些请求头常为空串）。按未验证假设处理，别当协议规则。
    pub android_id: String,
    /// 蓝牙 MAC（大写下划线分隔以外的常见形式）。
    pub bl_mac: String,
    /// Wi-Fi MAC。
    pub wifi_mac: String,
    /// 设备 IMEI。协议允许空串，但真机读得到。
    pub imei: String,
    /// App 安装时间（毫秒）。必须跨请求稳定，每次变等于自报改机。
    pub app_install_time: i64,
    /// App 上次更新时间（毫秒）。
    pub app_update_time: i64,
    /// 厂商自定义设备 ID。
    pub custom_device_id: String,
}

impl DeviceProfile {
    /// 按账号隔离设备标识的构造。
    ///
    /// `scope` 传账号 uid（或手机号）：同一个账号永远拿到同一个 device_id，
    /// 不同账号拿到不同的，避免被按设备聚合。
    pub fn for_account(scope: &str) -> Self {
        Self::for_account_in(&crate::api::session::home_dir(), scope)
    }

    /// 同上，但 HOME 基目录由调用方指定（测试注入隔离目录用）。
    pub(crate) fn for_account_in(base: &std::path::Path, scope: &str) -> Self {
        let device_id = persistent_device_id_in(base, scope);
        let mut d = Self {
            device_id: device_id.clone(),
            ..Self::base()
        };
        // 账号级机型：设了就发 SoC 型号，没设仍然是 ABI（与历史行为一致）。
        // device_id 不参与这次计算，所以换机型不会换设备。
        d.cpu_model = resolve_cpu_model(device_model_in(base, scope).as_deref());
        d.fill_derived_identity(&device_id);
        d
    }

    /// 不隔离的默认构造，但 HOME 基目录由调用方指定。
    pub(crate) fn default_in(base: &std::path::Path) -> Self {
        Self::for_account_in(base, "")
    }

    /// 由 device_id 派生其余设备标识。
    ///
    /// 全部是对 device_id 的确定性派生，所以同一账号每次跑都完全一致——
    /// 真机这些值从系统 API 读，一年都不会变；每次提交都换一茬等于自报
    /// 改机。安装时间用一个落在合理区间的固定值，也不用当前时间。
    fn fill_derived_identity(&mut self, seed: &str) {
        let h = |salt: &str| -> String {
            let mut hasher = Sha256::new();
            hasher.update(seed.as_bytes());
            hasher.update(salt.as_bytes());
            hasher
                .finalize()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect()
        };
        // 安卓 android_id 是 16 位小写 hex
        self.android_id = h("android_id")[..16].to_string();
        self.custom_device_id = h("custom")[..32].to_string();
        // MAC 用本地管理位（第二字节 bit1 置 1），不对应真实网卡
        let mac = |salt: &str, sep: &str| -> String {
            let x = h(salt);
            let bytes: Vec<String> = (0..6)
                .map(|i| {
                    format!(
                        "{:02x}",
                        u8::from_str_radix(&x[i * 2..i * 2 + 2], 16).unwrap_or(0)
                    )
                })
                .collect();
            let mut b = bytes.clone();
            if let Ok(v) = u8::from_str_radix(&b[0], 16) {
                b[0] = format!("{:02x}", v | 0x02);
            }
            b.join(sep)
        };
        self.wifi_mac = mac("wifi", ":");
        self.bl_mac = mac("bt", ":");
        // IMEI：15 位数字 = TAC 8 位 + 序列号 6 位 + Luhn 校验位 1 位。
        //
        // 原来取 15 个数字再加 "86" 前缀，`0<13` 只补不截，得到 17 位——
        // 任何长度或 Luhn 校验面前都是非法值（issue #16）。这里前 14 位
        // 固定以 "86" 开头，其余从摘要取，最后一位按 Luhn 算出来。
        let mut body: Vec<u8> = vec![8, 6];
        body.extend(
            h("imei")
                .chars()
                .filter_map(|c| c.to_digit(10))
                .take(12)
                .map(|d| d as u8),
        );
        while body.len() < 14 {
            body.push(0);
        }
        let check = luhn_check_digit(&body);
        self.imei = body
            .iter()
            .chain(std::iter::once(&check))
            .map(|d| d.to_string())
            .collect();
        // 安装/更新时间：从 device_id 派生一个落在 2023-2024 的固定时刻
        let base = 1_690_000_000_000i64; // 2023-07
        let off: i64 = u32::from_str_radix(&h("inst")[..8], 16).unwrap_or(0) as i64;
        self.app_install_time = base + off % (180 * 86_400_000);
        self.app_update_time = self.app_install_time + off % (7 * 86_400_000);
    }

    /// 不隔离的默认构造（单账号 / CLI 用）。
    fn base() -> Self {
        Self {
            device_id: String::new(),
            // 真机装的就是这个版本（7.4.20 / code 411，2026-11 从设备上取）。
            // 发旧版本号等于自报「用老客户端」，服务端的版本校验会直接看出来。
            // 需要对照实验时用配置 device.app_version（env PACE_APP_VERSION）覆盖。
            app_version: crate::config::global().device.app_version.clone(),
            physic_pixel: "1080x2400".into(),
            logic_pixel: "393x873".into(),
            is_root: "0".into(),
            os_type: "0".into(),
            cpu_model: crate::api::device_table::cpu_model(),
            device_name: "Android Device".into(),
            os_version: "13".into(),
            user_agent: "okhttp/4.12.0".into(),
            android_id: String::new(),
            bl_mac: String::new(),
            wifi_mac: String::new(),
            imei: String::new(),
            app_install_time: 0,
            app_update_time: 0,
            custom_device_id: String::new(),
        }
    }
}

impl Default for DeviceProfile {
    fn default() -> Self {
        Self::default_in(&crate::api::session::home_dir())
    }
}

/// UUID v4 格式字符串。
///
/// `nonce` 与登录 session 都要求这个格式（36 字符、带横线）。
/// 早先误用 32 位裸 hex，导致 `checkGeeUse` 返回 HTTP 200 + 空 body。
pub fn uuid_v4() -> String {
    use rand::Rng;
    let mut rng = rand::rng();
    let mut b = [0u8; 16];
    rng.fill(&mut b);
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        b[0],
        b[1],
        b[2],
        b[3],
        b[4],
        b[5],
        b[6],
        b[7],
        b[8],
        b[9],
        b[10],
        b[11],
        b[12],
        b[13],
        b[14],
        b[15]
    )
}

/// 设备标识文件路径：`<base>/.pace-forge-device-id[-<scope 摘要>]`。
///
/// 注意它**不在** `<base>/.pace-forge/` 目录里，而是与数据目录同级的隐藏
/// 文件——早期版本就是这么放的，改了会让所有老用户被判成新设备（10121）。
///
/// **纯路径计算，不碰磁盘。** 测试用它把路径注入到一次性临时目录。
/// 账号级机型配置文件：`<base>/.pace-forge-device-model-<hash>`，内容是
/// model code（或平台代号）一行纯文本，空表示不指定（cpuModel 发 ABI）。
///
/// 为什么按账号而不是环境变量：`PACE_*` 是**进程级**的，Web 多账号会共用
/// 同一个机型，等于所有账号同一台手机；而设备身份本来就该一账号一台。
pub(crate) fn device_model_path_in(base: &std::path::Path, scope: &str) -> std::path::PathBuf {
    let name = if scope.is_empty() {
        ".pace-forge-device-model".to_string()
    } else {
        format!(".pace-forge-device-model-{}", short_hash(scope))
    };
    base.join(name)
}

/// 读账号级机型。没有配置文件、或文件里是空串都返回 `None`。
pub(crate) fn device_model_in(base: &std::path::Path, scope: &str) -> Option<String> {
    let raw = std::fs::read_to_string(device_model_path_in(base, scope)).ok()?;
    let t = raw.trim().to_string();
    if t.is_empty() { None } else { Some(t) }
}

/// 写账号级机型。`None` 或空串表示清掉（回到 ABI）。
pub fn save_account_model(scope: &str, model: Option<&str>) -> Result<(), Error> {
    let base = crate::api::session::home_dir();
    save_account_model_in(&base, scope, model)
}

pub(crate) fn save_account_model_in(
    base: &std::path::Path,
    scope: &str,
    model: Option<&str>,
) -> Result<(), Error> {
    let path = device_model_path_in(base, scope);
    match model.map(str::trim).filter(|m| !m.is_empty()) {
        Some(m) => crate::api::session::write_file_private(&path, m.as_bytes()),
        None => match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(ErrorKind::client(format!(
                "清机型配置失败 {}: {e}",
                path.display()
            ))),
        },
    }
}

/// 当前账号的机型配置（给 Web 界面显示用）。
pub fn account_model(scope: &str) -> Option<String> {
    device_model_in(&crate::api::session::home_dir(), scope)
}

/// 机型 code / 平台代号 → 实际发出去的 cpuModel。查不到就是 ABI。
pub fn resolve_cpu_model(model: Option<&str>) -> String {
    model
        .map(str::trim)
        .filter(|m| !m.is_empty())
        .and_then(|m| {
            crate::api::device_table::soc_for_model(m)
                .or_else(|| crate::api::device_table::soc_for_platform(m).map(str::to_string))
        })
        .unwrap_or_else(|| crate::api::device_table::ABI_FALLBACK.to_string())
}

pub(crate) fn device_id_path_in(base: &std::path::Path, scope: &str) -> std::path::PathBuf {
    let name = if scope.is_empty() {
        ".pace-forge-device-id".to_string()
    } else {
        format!(".pace-forge-device-id-{}", short_hash(scope))
    };
    base.join(name)
}

/// 同上，但 HOME 基目录由调用方指定（测试注入隔离目录用）。
///
/// 写盘走 `session::write_file_private`：父目录不存在时自己建（`$HOME` 正常
/// 一定存在，但不把「环境一定对」当假设），权限收紧到 0600。
pub(crate) fn persistent_device_id_in(base: &std::path::Path, scope: &str) -> String {
    let path = device_id_path_in(base, scope);

    if let Ok(s) = std::fs::read_to_string(&path) {
        let t = s.trim().to_string();
        if t.len() == 32 && t.chars().all(|c| c.is_ascii_hexdigit()) {
            return t;
        }
    }

    let id = random_hex32();
    // 写盘失败不能阻断本次运行，但必须落一条告警：静默丢掉的话，下次启动
    // 读不到文件会另换随机 device_id，服务端判「新设备」撞 10121。
    if let Err(e) = crate::api::session::write_file_private(&path, id.as_bytes()) {
        eprintln!("[client] 设备标识写盘失败 {}: {e}", path.display());
    }
    id
}

/// 8 位十六进制摘要，用来给每个账号派生一个独立的设备文件。
fn short_hash(s: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(s.as_bytes());
    let d = h.finalize();
    format!("{:02x}{:02x}{:02x}{:02x}", d[0], d[1], d[2], d[3])
}

fn random_hex32() -> String {
    use rand::Rng;
    let mut rng = rand::rng();
    (0..32)
        .map(|_| format!("{:x}", rng.random_range(0..16u8)))
        .collect()
}

/// 鉴权凭据。
#[derive(Debug, Clone, Default)]
pub struct Credentials {
    /// 用户 id。
    pub uid: Option<String>,
    /// 登录 token。
    pub token: Option<String>,
    /// 会话标识。
    pub unid: Option<String>,
}

/// 协议客户端。
#[derive(Clone)]
pub struct Client {
    /// 设备指纹，随每个请求发出。
    pub device: DeviceProfile,
    /// 鉴权凭据，未登录时全空。
    pub creds: Credentials,
    /// 强制验签。
    ///
    /// 默认**关**：服务端的签名格式与 `@ApiCrypto` 注解的四个开关绑定，
    /// 部分接口（如 datapoint 埋点）返回明文、无 `s` 字段。强制验签会让这些
    /// 接口整体失败。已用 Python 实现实测确认：不验签时三个接口全部 error=10000。
    /// 需要完整性保证时再打开。
    pub require_signature: bool,
    /// HTTP 客户端（带代理、超时、证书配置）。
    pub agent: ureq::Agent,
}

impl Client {
    /// 按设备指纹建一个未登录的客户端。
    pub fn new(device: DeviceProfile) -> Self {
        Self {
            device,
            creds: Credentials::default(),
            require_signature: false,
            // 提交链路统一从这里建 agent：设了 PACE_EGRESS_PROXY 就走代理，
            // 没设与以前逐字节等价。见 api/egress.rs 顶部说明。
            agent: crate::api::egress::agent(std::time::Duration::from_secs(20)),
        }
    }

    /// 当前凭据的只读引用（未登录时为 `None`）。
    pub fn credentials(&self) -> Option<&Credentials> {
        if self.creds.uid.is_some() || self.creds.token.is_some() {
            Some(&self.creds)
        } else {
            None
        }
    }

    /// 带上凭据（builder 风格）。
    pub fn with_credentials(mut self, creds: Credentials) -> Self {
        self.creds = creds;
        self
    }

    /// 组装请求头，返回 (headers, headerSign 会话)。
    ///
    /// `headerSign` 的会话密钥必须保留——响应是用它解密的。
    fn build_headers(&self) -> Result<(Vec<(String, String)>, SessionKeys), Error> {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| ErrorKind::client(format!("系统时间异常: {e}")))?
            .as_millis() as u64;
        let nonce = uuid_v4();

        let mut headers: Vec<(String, String)> = vec![
            ("Accept".into(), "application/json".into()),
            ("Content-Type".into(), "application/json".into()),
            ("appVersion".into(), self.device.app_version.clone()),
            ("physicPixel".into(), self.device.physic_pixel.clone()),
            ("logicPixel".into(), self.device.logic_pixel.clone()),
            ("isRoot".into(), self.device.is_root.clone()),
            ("osType".into(), self.device.os_type.clone()),
            ("cpuModel".into(), self.device.cpu_model.clone()),
            ("deviceName".into(), self.device.device_name.clone()),
            ("DeviceId".into(), self.device.device_id.clone()),
            ("osVersion".into(), self.device.os_version.clone()),
            ("User-Agent".into(), self.device.user_agent.clone()),
            ("timeStamp".into(), now_ms.to_string()),
            ("nonce".into(), nonce),
            // 可选硬件标识：官方客户端有整段空串的会话（issue #17 的 59 条
            // 请求里五项全空），所以「发空」是一种真实状态；但空与派生各有
            // 风险，默认保持派生——同一台设备一半头有值一半空，比全空更像
            // 拼出来的。要拿对照数据时设 PACE_DEVICE_HW_HEADERS=empty，
            // 五个头（含 CustomDeviceId）一起清空，DeviceId 不动（它是设备锁）。
            ("androidId".into(), hw_id(&self.device.android_id)),
            ("blMac".into(), hw_id(&self.device.bl_mac)),
            ("wifiMac".into(), hw_id(&self.device.wifi_mac)),
            ("IMEI".into(), hw_id(&self.device.imei)),
            (
                "appInstallTime".into(),
                self.device.app_install_time.to_string(),
            ),
            (
                "appUpdateTime".into(),
                self.device.app_update_time.to_string(),
            ),
            // CustomDeviceId 也是客户端自造值，真机 HAR 里同样是空串
            // （issue #17：59/59）。跟着硬件头开关一起清。
            (
                "CustomDeviceId".into(),
                hw_id(&self.device.custom_device_id),
            ),
        ];
        if let Some(uid) = &self.creds.uid {
            headers.push(("uid".into(), uid.clone()));
        }
        if let Some(token) = &self.creds.token {
            headers.push(("token".into(), token.clone()));
        }

        // tokenSign
        let sign_fields: SignFields = vec![
            ("timeStamp".into(), Some(now_ms.to_string())),
            (
                "token".into(),
                Some(self.creds.token.clone().unwrap_or_default()),
            ),
            (
                "uid".into(),
                Some(self.creds.uid.clone().unwrap_or_default()),
            ),
        ];
        headers.push(("tokenSign".into(), token_sign(&sign_fields)));

        // headerSign：把上面所有头组成 JSON 后整体加密
        let header_json = headers_to_json(&headers);
        let env = seal(&header_json)?;
        headers.push(("headerSign".into(), env.wire));

        Ok((headers, env.session))
    }

    /// 发请求并解密响应。
    ///
    /// 返回业务 JSON 字符串（已解密）。HTTP 非 2xx 也会尝试解密，
    /// 因为服务端有时在错误响应里同样使用加密信封。
    pub fn request(&self, method: &str, url: &str, body: Option<&Value>) -> Result<String, Error> {
        self.request_with_headers(method, url, body, &[])
    }

    /// 带额外请求头的版本。登录需要 `Authorization: Basic ...`，
    /// 这类头不参与 `headerSign` 的计算（服务端只对设备头做信封）。
    ///
    /// 错误在此处一次性分类（#62）：厂商业务码走
    /// [`ErrorKind::classify_vendor`]，传输层失败归 `Network`，本地构造失败
    /// 归 `Client`，响应体损坏归 `Protocol`/`Json`。
    pub fn request_with_headers(
        &self,
        method: &str,
        url: &str,
        body: Option<&Value>,
        extra_headers: &[(&str, &str)],
    ) -> Result<String, Error> {
        let (headers, session) = self.build_headers()?;

        let mut req = match method.to_uppercase().as_str() {
            "GET" => self.agent.get(url),
            "POST" => self.agent.post(url),
            "PUT" => self.agent.put(url),
            m => return Err(ErrorKind::Client(format!("不支持的 method: {m}"))),
        };
        for (k, v) in &headers {
            req = req.set(k, v);
        }
        for (k, v) in extra_headers {
            req = req.set(k, v);
        }

        // POST body 同样要打信封（`encryptBody`）。响应仍用 headerSign 的会话密钥解密，
        // 所以 body 信封派生出的 key 直接丢弃——服务端不用它加密响应。
        let resp = match body {
            Some(b) => {
                let plain = serde_json::to_string(b).map_err(ErrorKind::Json)?;
                let body_env = seal(&plain)?;
                req.send_string(&body_env.wire)
            }
            None => req.call(),
        };

        let (status, text) = match resp {
            Ok(r) => (
                r.status(),
                r.into_string()
                    .map_err(|e| ErrorKind::Network(e.to_string()))?,
            ),
            Err(ureq::Error::Status(code, r)) => (code, r.into_string().unwrap_or_default()),
            Err(e) => return Err(ErrorKind::Network(e.to_string())),
        };

        if text.trim().is_empty() {
            return Err(ErrorKind::Protocol {
                code: None,
                msg: format!("HTTP {status} 响应为空"),
            });
        }
        // 服务端集群故障的**第一道拦截**。
        //
        // `expect_ok` 里已经认 `error=-10000` 并给出「跟你无关」的说明，但那
        // 条路要求响应能正常解密成业务 JSON。实际故障时后端把注册中心的异常
        // 原样透传，可能是 HTTP 5xx、可能没有加密信封、也可能信封结构不完整
        // 导致解密失败——三种都会在走到 `expect_ok` 之前就返回，用户看到的就
        // 只剩「解密失败」「HTTP 500」，然后又去换网络重装（issue #3 那位
        // 折腾了两天说的就是这个）。所以在这里按原始文本先认一次。
        if let Some(msg) = server_fault_hint(&text, status) {
            return Err(ErrorKind::Protocol {
                code: Some(crate::crypto::CODE_SERVER_FAULT),
                msg,
            });
        }
        // 网关级错误（如 Spring 的 404 页）是明文且无加密信封，直接透出原文
        if text.contains("\"status\":404") || text.contains("No message available") {
            return Err(ErrorKind::Protocol {
                code: None,
                msg: format!("HTTP {status} 网关错误: {}", truncate(&text, 200)),
            });
        }

        // 有些接口 decryptResponse=false，直接返回业务 JSON。探测：无 r 字段即明文。
        let looks_encrypted = text.contains("\"r\"") && text.contains("\"v\"");
        if !looks_encrypted {
            return Ok(text);
        }

        match decrypt::open(&text, &session, self.require_signature) {
            Ok(d) => Ok(d.body),
            Err(e) => Err(ErrorKind::Protocol {
                code: None,
                msg: format!(
                    "HTTP {status} 解密失败: {e}；原文 {} ",
                    truncate(&text, 120)
                ),
            }),
        }
    }
}

fn headers_to_json(headers: &[(String, String)]) -> String {
    let mut map = serde_json::Map::new();
    for (k, v) in headers {
        map.insert(k.clone(), Value::String(v.clone()));
    }
    Value::Object(map).to_string()
}

fn truncate(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// 原始响应文本里能认出的「服务端集群故障」吗？
///
/// 认两种特征：`error=-10000` 和注册中心那句 `Forbid consumer ... from
/// registry`。后者是前者的通常伴随物，单独出现（比如被包在纯文本里）也算。
/// 认出来就返回统一的说明，认不出返回 `None` 让调用方继续走原有逻辑。
pub fn server_fault_hint(text: &str, status: u16) -> Option<String> {
    let code_hit = text.contains("-10000");
    let reg_hit = text.contains("Forbid consumer") || text.contains("registry access list");
    if !code_hit && !reg_hit {
        return None;
    }
    Some(format!(
        "服务端集群故障（error=-10000，HTTP {status}），跟你这边无关，别折腾本地环境：\n\
         {}\n\n\
         这条错误是对方后端内部服务调用被注册中心白名单拦下后原样透传的，\n\
         登录、查记录、拉策略会一起失败。换网络、换账号、重装、重新编译都没有\n\
         用——等后端恢复即可。",
        truncate(text, 200)
    ))
}

/// 便捷方法：解析业务 JSON 并检查 `error` 字段。
///
/// 厂商业务码在此分类（#62）：`-10000` 集群故障保留长说明并归 `Protocol`，
/// 其余走 [`ErrorKind::classify_vendor`]。
pub fn expect_ok(body: &str) -> Result<Value, Error> {
    let v: Value = serde_json::from_str(body)?;
    let code = v.get("error").and_then(|x| x.as_i64());
    if code == Some(crate::crypto::CODE_OK) {
        return Ok(v);
    }
    let msg = v
        .get("message")
        .and_then(|x| x.as_str())
        .unwrap_or("未知错误");
    // 服务端内部故障要单独说清楚。它长得像「业务错误」，其实是后端服务被
    // 注册中心白名单拒绝后把异常原样透传上来，跟本地环境一点关系都没有。
    // 不点明的话，用户会去换网络、重装、换账号、挂代理，全白费
    // （issue #3 里那位折腾了整整两天）。
    if code == Some(crate::crypto::CODE_SERVER_FAULT) {
        return Err(ErrorKind::Protocol {
            code: Some(crate::crypto::CODE_SERVER_FAULT),
            msg: format!(
                "服务端集群故障（error=-10000），跟你这边无关，别折腾本地环境：{msg}\n\n\
                 这条错误是对方后端内部服务调用被拦（日志里通常带 \n\
                 `Forbid consumer x.x.x.x access service ... from registry ...:2181`），\n\
                 登录、查记录、拉策略会一起失败。\n\
                 换网络、换账号、重装、重新编译都没有用——等后端恢复即可。"
            ),
        });
    }
    // 有的接口把 -10000 包在别的字段里（`r` / `data` / 纯文本 message），
    // 走到这儿 code 认不出来，但原始文本认得出来，别再给「业务错误」了。
    if let Some(hint) = server_fault_hint(msg, 200) {
        return Err(ErrorKind::Protocol {
            code: Some(crate::crypto::CODE_SERVER_FAULT),
            msg: hint,
        });
    }
    Err(match code {
        Some(c) => ErrorKind::classify_vendor(c, msg),
        None => ErrorKind::Protocol {
            code: None,
            msg: format!("业务错误 message={msg}"),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestDir, assert_isolated};

    /// 硬件头开关：**默认空串**（7.4.21 硬编码 `""`），设 `derived` 才发假值。
    #[test]
    fn hw_headers_default_empty_derived_on_demand() {
        assert_eq!(
            hw_id("abc"),
            "",
            "默认必须是空串（与 7.4.21 的 getHeaderMap 一致）"
        );
        // 配置注入（Guard 互斥，多个用 Guard 的测试不会互相覆盖）
        let _g = crate::config::Guard::replace(crate::config::Config {
            device: crate::config::Device {
                hw_headers: crate::config::HwHeaders::Derived,
                ..Default::default()
            },
            ..Default::default()
        });
        assert_eq!(hw_id("abc"), "abc", "derived 时应发派生假值");
    }

    /// 回归（issue #16）：IMEI 必须 15 位并通过 Luhn。
    ///
    /// 原来取 15 个数字再加 "86" 前缀，`0<13` 只补不截，产出 17 位，
    /// 任何校验面前都是非法值。DeviceId 不受影响，仍是持久化那个。
    #[test]
    fn imei_is_15_digits_and_luhn_valid() {
        let td = TestDir::new("imei");
        let p = DeviceProfile::for_account_in(td.home(), "scope");
        assert_eq!(p.imei.len(), 15, "IMEI 位数不对: {}", p.imei);
        assert!(
            p.imei.chars().all(|c| c.is_ascii_digit()),
            "IMEI 含非数字: {}",
            p.imei
        );
        // 整串 Luhn：从右往左第二位起翻倍，总和须被 10 整除。
        let digits: Vec<u8> = p.imei.chars().map(|c| c as u8 - b'0').collect();
        let mut sum = 0u32;
        for (i, d) in digits.iter().rev().enumerate() {
            let mut v = *d as u32;
            if i % 2 == 1 {
                v *= 2;
                if v > 9 {
                    v -= 9;
                }
            }
            sum += v;
        }
        assert_eq!(sum % 10, 0, "Luhn 校验不通过: {}", p.imei);
        assert_eq!(
            luhn_check_digit(&[4, 9, 0, 1, 5, 4, 2, 0, 3, 2, 3, 7, 5, 1]),
            8
        );
    }

    /// 服务端集群故障要在**解密之前**就认出来。
    ///
    /// `expect_ok` 里那条 -10000 的说明要求响应能正常解密成业务 JSON。
    /// 实际故障时后端把注册中心的异常原样透传，可能是 HTTP 5xx、可能没有
    /// 加密信封、也可能信封不完整导致解密失败——三种都走不到 `expect_ok`，
    /// 用户只剩「解密失败 / HTTP 500」可看，然后又去换网络重装。
    #[test]
    fn server_fault_recognized_before_decrypt() {
        let reg = r#"Forbid consumer 10.0.50.83 access service com.iydsj.server.basicuser.service.UserBasicService from registry 10.0.30.34:2181. Please check registry access list (whitelist/blacklist)"#;
        // 带错误码
        let h = server_fault_hint(r#"{"error":-10000,"message":"x"}"#, 200);
        assert!(h.is_some(), "error=-10000 要在解密前被认出");
        let h = h.unwrap();
        assert!(h.contains("跟你这边无关"), "要排除本地问题: {h}");
        // 只有注册中心那句
        assert!(
            server_fault_hint(reg, 502).is_some(),
            "只有 Forbid consumer 那句也要认"
        );
        // HTTP 5xx + 那句话，状态码要带进提示
        let h = server_fault_hint(reg, 503).unwrap();
        assert!(h.contains("503"), "要带上真实状态码: {h}");
        // 正常响应不能被误判
        assert!(server_fault_hint(r#"{"error":10000,"message":"成功"}"#, 200).is_none());
        assert!(server_fault_hint("plain ok", 200).is_none());
        // 明文 10000（不带负号）不是故障
        assert!(server_fault_hint(r#"{"error":10000}"#, 200).is_none());
    }

    #[test]
    fn nonce_is_uuid_v4_format() {
        let u = uuid_v4();
        assert_eq!(
            u.len(),
            36,
            "nonce 必须是 36 字符 UUID，否则安全接口返回空 body"
        );
        assert_eq!(u.matches('-').count(), 4);
    }

    #[test]
    fn device_id_is_32_hex() {
        // 必须注入临时目录：DeviceProfile::default() 会读/写真实
        // `$HOME/.pace-forge-device-id`，测试不能碰。
        let t = TestDir::new("client-device-id");
        let d = DeviceProfile::default_in(t.home());
        assert_eq!(d.device_id.len(), 32);
        assert!(d.device_id.chars().all(|c| c.is_ascii_hexdigit()));
    }

    /// 设备标识必须落在注入目录里，且同一 scope 反复读得到同一个值。
    #[test]
    fn device_id_is_persisted_in_isolated_dir() {
        let t = TestDir::new("client-device-persist");
        let a = persistent_device_id_in(t.home(), "13800000000");
        let path = device_id_path_in(t.home(), "13800000000");
        assert!(path.exists(), "首次应落盘: {}", path.display());
        assert!(
            path.starts_with(t.home()),
            "设备标识必须写在注入基目录下，而不是 .pace-forge/ 里"
        );
        assert_eq!(
            path.parent().unwrap(),
            t.home(),
            "设备标识是数据目录的**同级文件**，不是目录内文件"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "设备标识文件权限必须是 600");
        }
        assert_eq!(
            persistent_device_id_in(t.home(), "13800000000"),
            a,
            "同一 scope 必须稳定"
        );
        assert_ne!(
            persistent_device_id_in(t.home(), "13900000000"),
            a,
            "不同 scope 必须是不同设备标识"
        );
    }

    /// 设备标识落盘失败时本次运行仍要拿到合法的 32 位 hex id，不能 panic。
    #[test]
    #[cfg(unix)]
    fn device_id_survives_persist_failure() {
        use std::os::unix::fs::PermissionsExt;
        let t = TestDir::new("client-device-ro");
        let ro = t.home().join("ro");
        std::fs::create_dir_all(&ro).unwrap();
        std::fs::set_permissions(&ro, PermissionsExt::from_mode(0o500)).unwrap();

        let id = persistent_device_id_in(&ro, "scope-x");
        assert_eq!(id.len(), 32);
        assert!(id.chars().all(|c| c.is_ascii_hexdigit()));

        // 还原权限，便于临时目录清理
        let _ = std::fs::set_permissions(&ro, PermissionsExt::from_mode(0o700));
    }

    #[test]
    fn headers_include_signature_and_envelope() {
        let t = TestDir::new("client-headers");
        let c = Client::new(DeviceProfile::for_account_in(t.home(), "u1"));
        let (headers, session) = c.build_headers().unwrap();
        let keys: Vec<&str> = headers.iter().map(|(k, _)| k.as_str()).collect();
        assert!(keys.contains(&"tokenSign"), "缺 tokenSign");
        assert!(keys.contains(&"headerSign"), "缺 headerSign");
        assert!(keys.contains(&"DeviceId"), "缺 DeviceId");
        // headerSign 必须是信封结构
        let h = headers
            .iter()
            .find(|(k, _)| k == "headerSign")
            .map(|(_, v)| v)
            .unwrap();
        let v: Value = serde_json::from_str(h).unwrap();
        assert_eq!(v["p"], "101");
        assert_eq!(session.response_key.len(), 16);
    }

    #[test]
    fn headers_carry_credentials_when_set() {
        let t = TestDir::new("client-creds");
        let c = Client::new(DeviceProfile::for_account_in(t.home(), "u1")).with_credentials(
            Credentials {
                uid: Some("123".into()),
                token: Some("tok".into()),
                unid: Some("88".into()),
            },
        );
        let (headers, _) = c.build_headers().unwrap();
        assert!(headers.iter().any(|(k, v)| k == "uid" && v == "123"));
        assert!(headers.iter().any(|(k, v)| k == "token" && v == "tok"));
    }

    #[test]
    fn expect_ok_accepts_10000() {
        assert!(expect_ok(r#"{"error":10000,"message":"成功","data":1}"#).is_ok());
        assert!(expect_ok(r#"{"error":10121,"message":"设备异常"}"#).is_err());
    }

    /// `-10000` 必须被判成**服务端故障**，而且提示里要明确说「跟你无关」。
    ///
    /// 真实的报文长这样（issue #3 里贴出来的原文）：错误码是负数，`message`
    /// 里塞着 dubbo 的 `Forbid consumer ... from registry ...:2181`。用户看到
    /// 这种信息的默认反应是重装、换号、挂代理，全都是白费。
    #[test]
    fn server_fault_is_explained_not_just_reported() {
        let body = r#"{"error":-10000,"message":"Forbid consumer 10.0.50.83 access service com.iydsj.server.basicuser.service.UserBasicService from registry 10.0.30.34:2181. Please check registry access list (whitelist/blacklist)"}"#;
        let e = expect_ok(body).unwrap_err();
        let text = e.to_string();
        assert!(text.contains("-10000"), "要带上原始错误码，方便搜：{text}");
        assert!(text.contains("服务端"), "要说明是服务端的问题：{text}");
        assert!(
            text.contains("无关") && text.contains("没有用"),
            "必须明确劝阻换网络/换账号/重装：{text}"
        );
        // 集群故障是厂商侧问题，归类为 Protocol 并带上厂商码
        assert!(
            matches!(
                e,
                ErrorKind::Protocol {
                    code: Some(-10000),
                    ..
                }
            ),
            "归类: {e:?}"
        );
        // 别的非零码仍然走分类：10121 归认证问题，不被集群故障吞掉
        let e2 = expect_ok(r#"{"error":10121,"message":"设备异常"}"#).unwrap_err();
        assert!(
            matches!(e2, ErrorKind::Auth(_)),
            "10121 应归类为认证问题：{e2}"
        );
    }

    /// 护栏：设备标识的注入路径**绝不能**是真实 `$HOME/.pace-forge-device-id*`。
    ///
    /// 模式与 `session::tests::test_data_dir_is_isolated_from_real_home` 一致。
    /// 注意设备标识是 `$HOME` 下的**同级文件**，不在 `.pace-forge/` 目录里，
    /// 所以这里单独断言它的基目录不能是真实 `$HOME`。
    #[test]
    fn device_id_path_is_isolated_from_real_home() {
        let t = TestDir::new("client-isolated");
        assert_isolated(&t.data(), "设备标识注入基目录");
        let injected = device_id_path_in(t.home(), "");
        let real = device_id_path_in(&crate::test_support::real_home(), "");
        assert_ne!(injected, real, "注入路径不能等于生产默认路径");
        assert_eq!(injected.file_name().unwrap(), ".pace-forge-device-id");
        assert_eq!(
            injected.parent().unwrap(),
            t.home(),
            "设备标识必须写在注入基目录下"
        );
    }
}

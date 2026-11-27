//! 类型化错误(#62 第一步):`ErrorKind` 分类 + 中文 `Display`。
//!
//! 动机(#62):163 处 `Result<_, String>` 让错误只能以中文叙述字符串传播,
//! Web 端全部落成同一张 400,CLI 无法按错误类分流退出码。
//!
//! 分阶段计划(issue #62 rollout;issue 草案落地时有一处扩展:`Client` 承载
//! 本地构造/环境失败——时间异常、密钥缺失、不支持的方法。这些与「网络」
//! 无关,折进 Network 会把「系统时间异常」误标成网络问题):
//! 1. ✅ 类型 + `client.rs` 分界分类 + 五个面向 Web/CLI 的读模块(本 PR)
//! 2. flow / submit / fence / points / policy / obs / gmttime(下一个 PR)
//! 3. CLI 退出码分流 + 移除各模块边界 `to_string()` 过渡层

/// 一个错误的类别。
///
/// 中文说明走 `Display`(保留旧文案措辞),类别供 Web 状态码映射和将来的
/// CLI 退出码分流使用。
#[derive(Debug, thiserror::Error)]
pub enum ErrorKind {
    /// 登录态/凭据问题(未登录、token 失效、异地登录挤下线)。
    #[error("登录态无效或凭据缺失：{0}")]
    Auth(String),
    /// 厂商拒绝:业务 error 码 + 原始 message。
    #[error("{}", protocol_display(code, msg))]
    Protocol {
        /// 服务端业务错误码；响应里没有就是 `None`。
        code: Option<i64>,
        /// 服务端给的原始说明。
        msg: String,
    },
    /// 连接/超时/DNS。
    #[error("网络错误：{0}")]
    Network(String),
    /// 触发风控/频率限制。
    #[error("触发频率限制：{0}")]
    RateLimit(String),
    /// 围栏/点位几何失败。
    #[error("围栏/点位几何失败：{0}")]
    Fence(String),
    /// 本地构造/环境失败(系统时间异常、密钥缺失、不支持的方法)。
    #[error("本地请求构造失败：{0}")]
    Client(String),
    /// 本地文件系统。
    #[error("本地文件读写失败：{0}")]
    Storage(#[from] std::io::Error),
    /// 坏响应体:厂商响应不是合法 JSON。
    #[error("业务响应不是合法 JSON：{0}")]
    Json(#[from] serde_json::Error),
}

/// 全库统一的错误类型；目前就是 [`ErrorKind`]，留个别名方便以后加 context。
pub type Error = ErrorKind;

fn protocol_display(code: &Option<i64>, m: &str) -> String {
    match code {
        Some(c) => format!("{m}(vendor code {c})"),
        None => m.to_string(),
    }
}

impl ErrorKind {
    /// Vendor response that is missing or malformed, with no code to attach.
    pub fn protocol(msg: String) -> Self {
        Self::Protocol { code: None, msg }
    }
    /// Local validation or environment failure.
    pub fn client(msg: String) -> Self {
        Self::Client(msg)
    }
    /// Transport failure: connect, timeout, DNS, non-2xx from object storage.
    pub fn network(msg: String) -> Self {
        Self::Network(msg)
    }
    /// Fence or checkpoint geometry that cannot be satisfied.
    pub fn fence(msg: String) -> Self {
        Self::Fence(msg)
    }
    /// 厂商业务码 → 类别。唯一分类点:新码出现时改这里。
    ///
    /// 已知码(依据 submit.rs 的说明表、issue 记录与真机实测):
    /// - `10121` 此账号已在其他设备登录(单设备登录限制,锁 1 小时)→ Auth
    /// - `10175` 手机号或密码错误(真机实测)→ Auth
    /// - `10603` 限流 → RateLimit
    /// - `11016` 不在有效时段 / `10003` 签名错误等 → Protocol
    pub fn classify_vendor(code: i64, msg: &str) -> ErrorKind {
        match code {
            10121 | 10175 => ErrorKind::Auth(msg.into()),
            10603 => ErrorKind::RateLimit(msg.into()),
            _ => ErrorKind::Protocol {
                code: Some(code),
                msg: msg.into(),
            },
        }
    }

    /// Web 控制台的 HTTP 状态码(#62):不再一律 400。
    ///
    /// Auth → 401;RateLimit → 429;Protocol/Network/Json → 502(上游拒绝/
    /// 不可达/响应损坏);Fence → 400(请求参数几何不成立);Client/Storage
    /// → 500(本地问题)。
    pub fn http_status(&self) -> u16 {
        match self {
            Self::Auth(_) => 401,
            Self::RateLimit(_) => 429,
            Self::Protocol { .. } | Self::Network(_) | Self::Json(_) => 502,
            Self::Fence(_) => 400,
            Self::Client(_) | Self::Storage(_) => 500,
        }
    }

    /// 不带类别前缀的原始文案，给已经自带上下文的提示用（例如「第 5 条：…」）。
    pub fn message(&self) -> String {
        match self {
            ErrorKind::Auth(m)
            | ErrorKind::Network(m)
            | ErrorKind::RateLimit(m)
            | ErrorKind::Fence(m)
            | ErrorKind::Client(m) => m.clone(),
            ErrorKind::Protocol { msg, .. } => msg.clone(),
            other => other.to_string(),
        }
    }

    /// Web 响应体里的 `kind` 字段取值。脚本按这个字段分流,不再解析中文。
    pub fn kind_name(&self) -> &'static str {
        match self {
            Self::Auth(_) => "auth",
            Self::Protocol { .. } => "protocol",
            Self::Network(_) => "network",
            Self::RateLimit(_) => "rate_limit",
            Self::Fence(_) => "fence",
            Self::Client(_) => "client",
            Self::Storage(_) => "storage",
            Self::Json(_) => "json",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    #[test]
    fn vendor_codes_classify_once() {
        assert!(matches!(
            ErrorKind::classify_vendor(10121, "设备异常"),
            ErrorKind::Auth(_)
        ));
        // 10175 真机实测:手机号或密码错误,归认证问题
        assert!(matches!(
            ErrorKind::classify_vendor(10175, "手机号或密码错误"),
            ErrorKind::Auth(_)
        ));
        assert!(matches!(
            ErrorKind::classify_vendor(10603, "请求过于频繁"),
            ErrorKind::RateLimit(_)
        ));
        assert!(matches!(
            ErrorKind::classify_vendor(11016, "不在有效时段"),
            ErrorKind::Protocol {
                code: Some(11016),
                ..
            }
        ));
        // 未知码默认 Protocol,不猜类别
        assert!(matches!(
            ErrorKind::classify_vendor(99999, "说明"),
            ErrorKind::Protocol {
                code: Some(99999),
                ..
            }
        ));
    }

    #[test]
    fn json_errors_convert_via_from() {
        let e: Error = serde_json::from_str::<Value>("{").unwrap_err().into();
        assert!(matches!(e, ErrorKind::Json(_)));
        assert_eq!(e.http_status(), 502);
    }

    #[test]
    fn http_status_mapping() {
        assert_eq!(ErrorKind::Auth("x".into()).http_status(), 401);
        assert_eq!(ErrorKind::RateLimit("x".into()).http_status(), 429);
        assert_eq!(
            ErrorKind::Protocol {
                code: None,
                msg: "x".into()
            }
            .http_status(),
            502
        );
        assert_eq!(ErrorKind::Network("x".into()).http_status(), 502);
        assert_eq!(ErrorKind::Fence("x".into()).http_status(), 400);
        assert_eq!(ErrorKind::Client("x".into()).http_status(), 500);
        assert_eq!(
            ErrorKind::Storage(std::io::Error::other("x")).http_status(),
            500
        );
    }

    #[test]
    fn kind_names_are_stable() {
        let names = [
            ErrorKind::Auth("x".into()).kind_name(),
            ErrorKind::Protocol {
                code: None,
                msg: "x".into(),
            }
            .kind_name(),
            ErrorKind::Network("x".into()).kind_name(),
            ErrorKind::RateLimit("x".into()).kind_name(),
            ErrorKind::Fence("x".into()).kind_name(),
            ErrorKind::Client("x".into()).kind_name(),
            ErrorKind::Storage(std::io::Error::other("x")).kind_name(),
            ErrorKind::Json(serde_json::from_str::<Value>("{").unwrap_err()).kind_name(),
        ];
        assert_eq!(
            names,
            [
                "auth",
                "protocol",
                "network",
                "rate_limit",
                "fence",
                "client",
                "storage",
                "json"
            ]
        );
    }

    #[test]
    fn display_keeps_chinese_messages() {
        let e = ErrorKind::Protocol {
            code: Some(11016),
            msg: "不在有效时段".into(),
        };
        assert_eq!(e.to_string(), "不在有效时段(vendor code 11016)");
        let e = ErrorKind::Protocol {
            code: None,
            msg: "HTTP 500 解密失败".into(),
        };
        assert_eq!(e.to_string(), "HTTP 500 解密失败");
        assert_eq!(
            ErrorKind::Auth("请先登录".into()).to_string(),
            "登录态无效或凭据缺失：请先登录"
        );
    }
}

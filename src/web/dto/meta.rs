//! 出口配置与设备机型的请求体。

use serde::Deserialize;
use serde_json::Value;

/// `PUT /api/egress`：`proxy` 为空串或缺失表示直连。
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct EgressForm {
    /// 出口代理地址（`host:port` 或带 scheme 的 URL）。
    #[serde(deserialize_with = "super::opt_string")]
    pub(crate) proxy: Option<String>,
}

impl EgressForm {
    /// 从请求体解析。
    pub(crate) fn parse(body: &Value) -> Self {
        serde_json::from_value(body.clone()).unwrap_or_default()
    }

    /// 代理地址；空白或无内容表示直连。
    pub(crate) fn proxy(&self) -> Option<&str> {
        self.proxy
            .as_deref()
            .map(str::trim)
            .filter(|p| !p.is_empty())
    }
}

/// `PUT /api/device`：机型代号，可选平台代号。
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct DeviceForm {
    /// 机型 code，空表示清掉当前账号的自定义机型。
    #[serde(deserialize_with = "super::opt_string")]
    pub(crate) model: Option<String>,
    /// 平台代号：内置表查不到 SoC 时用来记一条用户级映射。
    #[serde(deserialize_with = "super::opt_string")]
    pub(crate) platform: Option<String>,
}

impl DeviceForm {
    /// 从请求体解析。
    pub(crate) fn parse(body: &Value) -> Self {
        serde_json::from_value(body.clone()).unwrap_or_default()
    }

    /// 机型 code；空白表示清除。
    pub(crate) fn model(&self) -> Option<&str> {
        self.model
            .as_deref()
            .map(str::trim)
            .filter(|m| !m.is_empty())
    }

    /// 平台代号；空白表示不记映射。
    pub(crate) fn platform(&self) -> Option<&str> {
        self.platform
            .as_deref()
            .map(str::trim)
            .filter(|p| !p.is_empty())
    }
}

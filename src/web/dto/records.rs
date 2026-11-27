//! 轨迹回读与接口透传的请求体。

use serde::Deserialize;
use serde_json::{Value, json};

/// `POST /api/obs`：要取的对象 key，必须以 `run_data/` 或 `runData/` 开头。
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct ObsFetchForm {
    /// 对象存储 key。
    #[serde(deserialize_with = "super::opt_string")]
    pub(crate) key: Option<String>,
}

impl ObsFetchForm {
    /// 从请求体解析。
    pub(crate) fn parse(body: &Value) -> Self {
        serde_json::from_value(body.clone()).unwrap_or_default()
    }

    /// 合法的 key；格式不对就是 `None`，由调用方回中文原因。
    pub(crate) fn key(&self) -> Option<&str> {
        self.key
            .as_deref()
            .filter(|k| k.starts_with("run_data/") || k.starts_with("runData/"))
    }
}

/// `POST /api/raw`：透传任意业务接口。
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct RawForm {
    /// 目标路径，必须以 `/api/` 开头。
    #[serde(deserialize_with = "super::opt_string")]
    pub(crate) path: Option<String>,
    /// 透传的请求体；缺失按 `{}`。
    #[serde(deserialize_with = "super::opt_value")]
    pub(crate) body: Option<Value>,
    /// `run`（默认）或 `discovery`。
    #[serde(deserialize_with = "super::opt_string")]
    pub(crate) host: Option<String>,
}

impl RawForm {
    /// 从请求体解析。
    pub(crate) fn parse(body: &Value) -> Self {
        serde_json::from_value(body.clone()).unwrap_or_default()
    }

    /// 目标路径。
    pub(crate) fn path(&self) -> &str {
        self.path.as_deref().unwrap_or("")
    }

    /// 透传报文。
    pub(crate) fn inner(&self) -> Value {
        self.body.clone().unwrap_or_else(|| json!({}))
    }

    /// 目标主机；只有明确写 `discovery` 才走发现域名。
    pub(crate) fn is_discovery(&self) -> bool {
        self.host.as_deref() == Some("discovery")
    }
}

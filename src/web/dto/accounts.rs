//! `/api/accounts*` 的请求体。

use serde::Deserialize;
use serde_json::Value;

/// 新增账号：手机号 + 密码，两个都必须非空。
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct AddAccountForm {
    /// 登录手机号。
    #[serde(deserialize_with = "super::opt_string")]
    pub(crate) username: Option<String>,
    /// 明文密码，只在这一次登录里用，落盘前会加密。
    #[serde(deserialize_with = "super::opt_string")]
    pub(crate) password: Option<String>,
}

impl AddAccountForm {
    /// 从请求体解析。
    pub(crate) fn parse(body: &Value) -> Self {
        serde_json::from_value(body.clone()).unwrap_or_default()
    }

    /// 去掉首尾空白的手机号。
    pub(crate) fn username(&self) -> &str {
        self.username.as_deref().unwrap_or("").trim()
    }

    /// 原样密码（密码里的空白有意义，不能 trim）。
    pub(crate) fn password(&self) -> &str {
        self.password.as_deref().unwrap_or("")
    }
}

/// 批量刷：规划参数 + 批次控制（试运行、间隔、限定账号）。
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct BatchForm {
    /// 只刷这些账号 id；空数组或缺失表示全部就绪账号。
    #[serde(deserialize_with = "super::opt_string_list")]
    pub(crate) ids: Option<Vec<String>>,
    /// `true` 表示只预演不提交。
    #[serde(deserialize_with = "super::opt_bool")]
    pub(crate) dry: Option<bool>,
    /// 账号之间的间隔（毫秒），默认 8000。
    #[serde(deserialize_with = "super::opt_u64")]
    pub(crate) gap_ms: Option<u64>,
}

impl BatchForm {
    /// 从请求体解析。
    pub(crate) fn parse(body: &Value) -> Self {
        serde_json::from_value(body.clone()).unwrap_or_default()
    }

    /// 限定账号列表，空表示全部。
    pub(crate) fn ids(&self) -> Vec<String> {
        self.ids.clone().unwrap_or_default()
    }

    /// 是否只预演。
    pub(crate) fn dry(&self) -> bool {
        self.dry.unwrap_or(false)
    }

    /// 间隔毫秒数。
    pub(crate) fn gap_ms(&self) -> u64 {
        self.gap_ms.unwrap_or(8000)
    }
}

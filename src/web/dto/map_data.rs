//! 本地手绘禁区的请求体。
//!
//! 这块的校验报错是契约的一部分（「polys 必须是数组」「最多 200 块」），所以
//! 只把顶层字段集中，逐块的结构检查留在处理器里。

use serde::Deserialize;
use serde_json::Value;

/// `PUT /api/blockers`：`polys` 是 `[[[lat,lng], ...], ...]`。
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct BlockersForm {
    /// 禁区多边形列表，每块是一串坐标对。
    #[serde(deserialize_with = "super::opt_value")]
    pub(crate) polys: Option<Value>,
}

impl BlockersForm {
    /// 从请求体解析。
    pub(crate) fn parse(body: &Value) -> Self {
        serde_json::from_value(body.clone()).unwrap_or_default()
    }

    /// 顶层数组；`None` 表示字段缺失或不是数组。
    pub(crate) fn polys_array(&self) -> Option<&Vec<Value>> {
        self.polys.as_ref()?.as_array()
    }
}

//! 违规/作弊名单自查。
//!
//! 服务端有一条 `POST /api/v78/cheat/cheatlist`，返回里带 `self`（自己）和
//! `list`（本页通报）。这是**唯一**能让用户自己确认「有没有被标记」的入口
//! ——提交成功不代表没被标记，很多判定是事后跑批做的。
//!
//! 早先我们提交完就不管了：记录入账、App 能看见，于是以为没问题，直到某天
//! 判定下来才发现早就进了名单。这个模块把那次查询接出来。
//!
//! ## 解析上的两条纪律
//!
//! 1. **缺字段不等于 null**。`self` 整个缺失、`count` 是字符串、`self` 是
//!    非对象——这些都算「结果未知」，不能当成「没有违规」。把未知报成清白
//!    比不报更糟：用户会以为查过了没问题。
//! 2. **检查结果会过期**。名单是服务端跑批出来的，本地缓存超过
//!    [`FRESH_MS`] 就只是「上次查过」，不能当现在的状态。

use crate::error::Error;
use serde_json::Value;

/// 自查结果的有效期：5 分钟。
pub const FRESH_MS: i64 = 5 * 60 * 1000;

/// 违规名单查询接口路径。
pub const CHEATLIST_PATH: &str = "/api/v78/cheat/cheatlist";

/// 自查状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// 明确查到没有违规记录。
    NoRecord,
    /// 服务端已标记。
    Flagged,
    /// 结果未知：字段缺失或格式不对。**不等于清白**。
    Unknown,
    /// 检查结果过期。
    Stale,
}

impl Status {
    /// 给用户看的状态说明。
    pub fn label(self) -> &'static str {
        match self {
            Status::NoRecord => "暂未发现违规记录",
            Status::Flagged => "服务端已标记违规",
            Status::Unknown => "结果未知：个人状态字段缺失或格式异常",
            Status::Stale => "检查结果已过期，请重新检查",
        }
    }
}

/// 一次自查的快照。
#[derive(Debug, Clone, Default)]
pub struct CheatReport {
    /// 响应里的 `self` 对象（可能为 null / 缺失）。
    pub self_info: Value,
    /// `self` 字段是否存在。
    pub self_present: bool,
    /// 本页通报列表。
    pub list: Vec<Value>,
    /// 查询时刻（毫秒）。
    pub checked_at_ms: i64,
}

impl CheatReport {
    /// 从接口的 `data` 构造。`data` 不是对象就是格式异常——直接报错，
    /// 不要退回空报告（空报告会被读成「没有违规」）。
    pub fn from_data(data: &Value, checked_at_ms: i64) -> Result<Self, Error> {
        if !data.is_object() {
            return Err(Error::Protocol {
                code: None,
                msg: "违规接口数据格式异常，无法判断".into(),
            });
        }
        Ok(Self {
            self_info: data.get("self").cloned().unwrap_or(Value::Null),
            self_present: data.get("self").is_some(),
            list: data
                .get("list")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default(),
            checked_at_ms,
        })
    }

    /// 违规次数。`count` 可能是字符串，也可能是负数（脏数据）——后者算未知。
    pub fn count(&self) -> Option<i64> {
        let v = self.self_info.get("count")?;
        v.as_i64()
            .or_else(|| v.as_str()?.trim().parse().ok())
            .filter(|n| *n >= 0)
    }

    /// 在 `now` 时刻的状态。
    pub fn status_at(&self, now: i64) -> Status {
        // `now < checked_at` 也要算过期：时钟回拨（或缓存来自"未来"）时
        // `saturating_sub` 会得到 0，正好落在新鲜区间里，被当成刚查过。
        if self.checked_at_ms <= 0
            || now < self.checked_at_ms
            || now - self.checked_at_ms >= FRESH_MS
        {
            return Status::Stale;
        }
        if !self.self_present {
            return Status::Unknown;
        }
        match &self.self_info {
            // 服务端明确给了 null：没有记录
            Value::Null => Status::NoRecord,
            Value::Object(_) => match self.count() {
                Some(0) => Status::NoRecord,
                Some(_) => Status::Flagged,
                None => Status::Unknown,
            },
            _ => Status::Unknown,
        }
    }

    /// 依据当前时钟的状态。
    pub fn status(&self) -> Status {
        self.status_at(crate::crypto::envelope::now_ms())
    }

    /// 服务端给的原因（字段名在不同版本里不一样，逐个试）。
    pub fn reason(&self) -> String {
        text_field(&self.self_info, &["reason", "punishReason", "cause"])
            .unwrap_or_else(|| "服务端未提供原因".into())
    }

    /// 申述状态。
    pub fn appeal(&self) -> String {
        text_field(&self.self_info, &["appealStatus"]).unwrap_or_else(|| "未知".into())
    }

    /// 被判定记录的时间（原文或格式化后的时间）。
    pub fn occurred_at(&self) -> String {
        let Some(text) = text_field(&self.self_info, &["createTime", "time", "date"]) else {
            return "服务端未提供时间".into();
        };
        match text.parse::<i64>() {
            Ok(n) => {
                let ms = if (1_000_000_000..10_000_000_000).contains(&n) {
                    n * 1000
                } else {
                    n
                };
                if (1_000_000_000_000..10_000_000_000_000).contains(&ms) {
                    return crate::api::submit::fmt_run_time(ms);
                }
                text
            }
            Err(_) => text,
        }
    }

    /// 关联的运行记录 id。**不要**把通报 id 当成 rrid。
    pub fn record_id(&self) -> Option<i64> {
        let id = self.self_info.get("rrid")?;
        id.as_i64()
            .or_else(|| id.as_str()?.trim().parse().ok())
            .filter(|n| *n > 0)
    }

    /// 一行摘要，给日志用。
    ///
    /// 用**查询时刻**而不是当前时钟判状态：摘要描述的是「这次查询看到了
    /// 什么」，不是「现在是什么」。拿现在判会让刚查完的日志立刻显示过期。
    pub fn summary(&self) -> String {
        let at = if self.checked_at_ms > 0 {
            self.checked_at_ms
        } else {
            crate::crypto::envelope::now_ms()
        };
        let mut parts = vec![format!("状态：{}", self.status_at(at).label())];
        if let Some(n) = self.count() {
            parts.push(format!("违规 {n} 次"));
        }
        if self.status_at(at) == Status::Flagged {
            parts.push(format!("原因：{}", self.reason()));
            parts.push(format!("时间：{}", self.occurred_at()));
            parts.push(format!("申述：{}", self.appeal()));
            if let Some(rrid) = self.record_id() {
                parts.push(format!("关联记录 #{rrid}"));
            }
        }
        parts.push(format!("本页通报 {} 条", self.list.len()));
        parts.join("；")
    }
}

fn text_field(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| match value.get(key)? {
        Value::String(s) if !s.trim().is_empty() => Some(s.trim().to_string()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    })
}

/// 查询违规名单（第 `page` 页，每页 20 条）。
///
/// `unid` 取自登录态；没有登录态时传 0（服务端按 token 认人，unid 只是
/// 报文里的一个字段）。
pub fn query(
    client: &crate::api::Client,
    page: i64,
    log: &mut dyn FnMut(&str),
) -> Result<CheatReport, Error> {
    let unid: i64 = client
        .credentials()
        .and_then(|c| c.unid.as_ref())
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0);
    let body = serde_json::json!({"pageNum": page, "pageSize": 20, "unid": unid});
    let raw = client.request(
        "POST",
        &format!("{}{}", crate::crypto::hosts::DISCOVERY, CHEATLIST_PATH),
        Some(&body),
    )?;
    let v = crate::api::client::expect_ok(&raw)?;
    let data = v.get("data").cloned().unwrap_or(Value::Null);
    let report = CheatReport::from_data(&data, crate::crypto::envelope::now_ms())?;
    log(&format!("[cheat] {}", report.summary()));
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn report(checked: i64) -> CheatReport {
        CheatReport {
            self_info: Value::Null,
            self_present: true,
            list: Vec::new(),
            checked_at_ms: checked,
        }
    }

    #[test]
    fn clock_backwards_is_stale_not_fresh() {
        // 缓存时间戳在"未来"（时钟回拨过）→ 不能当成刚查过
        assert_eq!(report(10_000).status_at(5_000), Status::Stale);
    }

    #[test]
    fn just_now_is_fresh() {
        assert_eq!(report(10_000).status_at(10_000), Status::NoRecord);
        assert_eq!(
            report(10_000).status_at(10_000 + FRESH_MS - 1),
            Status::NoRecord
        );
        assert_eq!(report(10_000).status_at(10_000 + FRESH_MS), Status::Stale);
    }

    // ---- merged from former `mod tests` ----

    const T0: i64 = 1_700_000_000_000;

    #[test]
    fn explicit_null_self_means_no_record() {
        let r = CheatReport::from_data(&json!({"self": null, "list": []}), T0).unwrap();
        assert_eq!(r.status_at(T0), Status::NoRecord);
    }

    #[test]
    fn zero_count_means_no_record() {
        let r = CheatReport::from_data(&json!({"self": {"count": 0}, "list": []}), T0).unwrap();
        assert_eq!(r.status_at(T0), Status::NoRecord);
    }

    #[test]
    fn positive_count_means_flagged() {
        let r = CheatReport::from_data(
            &json!({"self": {"count": 2, "reason": "轨迹异常", "createTime": "2026-11-10 19:20:00"}}),
            T0,
        )
        .unwrap();
        assert_eq!(r.status_at(T0), Status::Flagged);
        assert_eq!(r.reason(), "轨迹异常");
        assert_eq!(r.count(), Some(2));
    }

    /// 服务端把 count 发成字符串的版本也认。
    #[test]
    fn count_as_string_is_parsed() {
        let r = CheatReport::from_data(&json!({"self": {"count": "3"}}), T0).unwrap();
        assert_eq!(r.count(), Some(3));
        assert_eq!(r.status_at(T0), Status::Flagged);
    }

    /// **缺字段不能读成清白**：这是本模块最容易写错的地方。
    #[test]
    fn missing_self_is_unknown_not_clean() {
        let r = CheatReport::from_data(&json!({"list": []}), T0).unwrap();
        assert!(!r.self_present);
        assert_eq!(r.status_at(T0), Status::Unknown);
        assert_ne!(r.status_at(T0), Status::NoRecord);
    }

    #[test]
    fn missing_count_is_unknown_not_clean() {
        let r = CheatReport::from_data(&json!({"self": {"appealStatus": "none"}}), T0).unwrap();
        assert_eq!(r.count(), None);
        assert_eq!(r.status_at(T0), Status::Unknown);
    }

    #[test]
    fn non_object_self_is_unknown() {
        let r = CheatReport::from_data(&json!({"self": "oops"}), T0).unwrap();
        assert_eq!(r.status_at(T0), Status::Unknown);
    }

    #[test]
    fn negative_count_is_unknown() {
        let r = CheatReport::from_data(&json!({"self": {"count": -1}}), T0).unwrap();
        assert_eq!(r.count(), None);
        assert_eq!(r.status_at(T0), Status::Unknown);
    }

    #[test]
    fn stale_after_fresh_window() {
        let r = CheatReport::from_data(&json!({"self": {"count": 1}}), T0).unwrap();
        assert_eq!(r.status_at(T0 + FRESH_MS - 1), Status::Flagged);
        assert_eq!(r.status_at(T0 + FRESH_MS), Status::Stale);
    }

    #[test]
    fn non_object_data_is_an_error() {
        assert!(CheatReport::from_data(&json!("bad"), T0).is_err());
        assert!(CheatReport::from_data(&json!([1, 2]), T0).is_err());
    }

    #[test]
    fn record_id_ignores_string_and_invalid() {
        let r = CheatReport::from_data(&json!({"self": {"rrid": "1790000000001"}}), T0).unwrap();
        assert_eq!(r.record_id(), Some(1_790_000_000_001));
        let r = CheatReport::from_data(&json!({"self": {"rrid": 0}}), T0).unwrap();
        assert_eq!(r.record_id(), None);
        let r = CheatReport::from_data(&json!({"self": {"id": 99}}), T0).unwrap();
        assert_eq!(r.record_id(), None, "通报 id 不能当 rrid");
    }

    #[test]
    fn summary_mentions_reason_when_flagged() {
        let r = CheatReport::from_data(
            &json!({"self": {"count": 1, "reason": "配速异常", "appealStatus": "未申述"}}),
            T0,
        )
        .unwrap();
        let s = r.summary();
        assert!(s.contains("配速异常"), "{s}");
        assert!(s.contains("未申述"), "{s}");
    }
}

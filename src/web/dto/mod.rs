//! 网页控制台的请求体契约：每个业务域一个子模块，字段名与前端发来的 JSON 一一对应。
//!
//! 为什么字段全用 `Option<T>` + 宽松反序列化：拆 `/api/*` 处理器之前，取值写法是
//! `body["x"].as_f64().unwrap_or(默认)` ——**类型不对就当没填**。这次只把「收哪些
//! 字段」挪到一处，不改这个口径，所以给了 `deserialize_with` 的函数都先落到
//! [`Value`] 再取，遇到字符串、`null`、缺失一律返回 `None`。线上的请求与响应
//! 逐字节不变，改变的是「字段清单有了单一出处」。
//!
//! 需要严格校验的字段（范围、枚举、非空）仍在处理器里判断，并给出中文原因；
//! DTO 只负责「有没有、是不是这个类型」。

use serde::Deserialize;
use serde_json::Value;

pub(crate) mod accounts;
pub(crate) mod map_data;
pub(crate) mod meta;
pub(crate) mod records;
pub(crate) mod run;

/// 读一个 [`Value`]，任何 JSON 都能落进来。
fn value<'de, D>(d: D) -> Result<Value, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Value::deserialize(d)
}

/// 数字字段：JSON 数字才算，其它类型当没填（与 `as_f64` 同义）。
pub(crate) fn opt_f64<'de, D>(d: D) -> Result<Option<f64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(value(d)?.as_f64())
}

/// 整数字段：同 [`opt_f64`]，非整数当没填（与 `as_i64` 同义）。
pub(crate) fn opt_i64<'de, D>(d: D) -> Result<Option<i64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(value(d)?.as_i64())
}

/// 非负整数字段：负数也当没填（与 `as_u64` 同义）。
pub(crate) fn opt_u64<'de, D>(d: D) -> Result<Option<u64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(value(d)?.as_u64())
}

/// 布尔字段：只有 JSON `true`/`false` 才算。
pub(crate) fn opt_bool<'de, D>(d: D) -> Result<Option<bool>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(value(d)?.as_bool())
}

/// 文本字段：只有 JSON 字符串才算。
pub(crate) fn opt_string<'de, D>(d: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(value(d)?.as_str().map(str::to_owned))
}

/// 字符串数组：非数组当没填，数组里的非字符串元素丢掉（与逐项 `as_str` 同义）。
pub(crate) fn opt_string_list<'de, D>(d: D) -> Result<Option<Vec<String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(value(d)?.as_array().map(|arr| {
        arr.iter()
            .filter_map(|x| x.as_str().map(str::to_owned))
            .collect()
    }))
}

/// 原始 JSON 值：`/api/raw` 的透传报文用。
pub(crate) fn opt_value<'de, D>(d: D) -> Result<Option<Value>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Some(value(d)?))
}

#[cfg(test)]
mod tests {
    use super::run::PlanForm;
    use serde_json::{Value, json};

    /// 拆分前的取值口径：`body["k"].as_*()`，类型不对就是 `None`。
    fn legacy_f64(v: &Value, k: &str) -> Option<f64> {
        v[k].as_f64()
    }
    fn legacy_i64(v: &Value, k: &str) -> Option<i64> {
        v[k].as_i64()
    }
    fn legacy_bool(v: &Value, k: &str) -> Option<bool> {
        v[k].as_bool()
    }
    fn legacy_str(v: &Value, k: &str) -> Option<String> {
        v[k].as_str().map(str::to_owned)
    }

    /// 正常请求：DTO 的每个字段都和旧写法一致。
    #[test]
    fn plan_form_matches_legacy_extraction() {
        let v = json!({
            "distance": 3.5, "duration": 20, "steps": 1200, "sportType": 1,
            "runMode": 2, "startAtMs": 1_700_000_000_000_i64, "lat": 47.3,
            "lng": 103.9, "coord": "wgs84", "mode": "stadium", "lane": 4,
            "endLat": 47.31, "endLng": 103.91, "altMin": 120, "altMax": 150,
            "preferRoads": false, "expectUid": "u1",
        });
        let f = PlanForm::parse(&v);
        assert_eq!(f.distance, legacy_f64(&v, "distance"));
        assert_eq!(f.duration, legacy_f64(&v, "duration"));
        assert_eq!(f.steps, legacy_i64(&v, "steps"));
        assert_eq!(f.sport_type, legacy_i64(&v, "sportType"));
        assert_eq!(f.run_mode, legacy_i64(&v, "runMode"));
        assert_eq!(f.start_at_ms, legacy_i64(&v, "startAtMs"));
        assert_eq!(f.lat, legacy_f64(&v, "lat"));
        assert_eq!(f.lng, legacy_f64(&v, "lng"));
        assert_eq!(f.coord, legacy_str(&v, "coord"));
        assert_eq!(f.mode, legacy_str(&v, "mode"));
        assert_eq!(f.lane, legacy_i64(&v, "lane"));
        assert_eq!(f.end_lat, legacy_f64(&v, "endLat"));
        assert_eq!(f.end_lng, legacy_f64(&v, "endLng"));
        assert_eq!(f.alt_min, legacy_f64(&v, "altMin"));
        assert_eq!(f.alt_max, legacy_f64(&v, "altMax"));
        assert_eq!(f.prefer_roads, legacy_bool(&v, "preferRoads"));
        assert_eq!(f.expect_uid, legacy_str(&v, "expectUid"));
    }

    /// 垃圾请求：字符串冒充数字、`null`、布尔冒充数字——旧写法一律当没填，
    /// DTO 必须同样当没填，不能让整次请求因为一个字段类型不对而失败。
    #[test]
    fn plan_form_is_lenient_about_wrong_types() {
        let v = json!({
            "distance": "3.5", "duration": null, "steps": true, "sportType": "1",
            "runMode": 2.5, "startAtMs": "now", "lat": [], "lng": {},
            "coord": 7, "mode": 3, "lane": "4", "endLat": null, "endLng": true,
            "altMin": "120", "altMax": null, "preferRoads": 1, "expectUid": 42,
        });
        let f = PlanForm::parse(&v);
        assert_eq!(f.distance, legacy_f64(&v, "distance"));
        assert_eq!(f.duration, legacy_f64(&v, "duration"));
        assert_eq!(f.steps, legacy_i64(&v, "steps"));
        assert_eq!(f.sport_type, legacy_i64(&v, "sportType"));
        assert_eq!(f.run_mode, legacy_i64(&v, "runMode"));
        assert_eq!(f.start_at_ms, legacy_i64(&v, "startAtMs"));
        assert_eq!(f.lat, legacy_f64(&v, "lat"));
        assert_eq!(f.lng, legacy_f64(&v, "lng"));
        assert_eq!(f.coord, legacy_str(&v, "coord"));
        assert_eq!(f.mode, legacy_str(&v, "mode"));
        assert_eq!(f.lane, legacy_i64(&v, "lane"));
        assert_eq!(f.end_lat, legacy_f64(&v, "endLat"));
        assert_eq!(f.end_lng, legacy_f64(&v, "endLng"));
        assert_eq!(f.alt_min, legacy_f64(&v, "altMin"));
        assert_eq!(f.alt_max, legacy_f64(&v, "altMax"));
        assert_eq!(f.prefer_roads, legacy_bool(&v, "preferRoads"));
        assert_eq!(f.expect_uid, legacy_str(&v, "expectUid"));
        // 全空时的默认值也要和旧代码一致
        assert_eq!(f.distance_km(), 2.0);
        assert_eq!(f.duration_min(), 12.0);
        assert_eq!(f.sport_type(), 1);
        assert_eq!(f.run_mode(), 1);
        assert_eq!(f.start_at_ms(), None);
        assert_eq!(f.coord_sys(), "gcj02");
        assert!(!f.prefer_stadium());
        assert!(f.prefer_roads());
        assert_eq!(f.lane(), 1);
        assert_eq!(f.expect_uid(), None);
    }

    /// 空对象：所有默认值与拆分前逐项相同。
    #[test]
    fn plan_form_defaults_match() {
        let f = PlanForm::parse(&json!({}));
        assert_eq!(f.distance_km(), 2.0);
        assert_eq!(f.duration_min(), 12.0);
        assert_eq!(f.steps(), None);
        assert_eq!(f.sport_type(), 1);
        assert_eq!(f.center(), None);
        assert_eq!(f.end_raw(), None);
        assert_eq!(f.alt_range(), None);
        assert_eq!(f.custom_path(), None);
        assert_eq!(f.lane(), 1);
    }

    /// `startAtMs` 负数、`steps` 0 都表示「没指定」。
    #[test]
    fn plan_form_treats_non_positive_as_absent() {
        let f = PlanForm::parse(&json!({"startAtMs": -5, "steps": 0}));
        assert_eq!(f.start_at_ms(), None);
        assert_eq!(f.steps(), None);
    }

    /// 终点两个都是 0 表示闭环，只有一个是 0 则仍是有效终点。
    #[test]
    fn plan_form_end_zero_means_no_end() {
        assert_eq!(
            PlanForm::parse(&json!({"endLat": 0, "endLng": 0})).end_raw(),
            None
        );
        assert_eq!(
            PlanForm::parse(&json!({"endLat": 0, "endLng": 103.9})).end_raw(),
            Some((0.0, 103.9))
        );
    }

    /// 手绘轨迹：脏点丢掉、不足 2 点当没传——和拆分前逐条一致。
    #[test]
    fn plan_form_custom_path_filters_junk() {
        let f = PlanForm::parse(&json!({
            "customPath": [[47.3, 103.9], ["x", 1], [47.4, 103.95], [91.0, 0.0]]
        }));
        assert_eq!(f.custom_path(), Some(vec![(47.3, 103.9), (47.4, 103.95)]));
        let f = PlanForm::parse(&json!({"customPath": [[47.3, 103.9]]}));
        assert_eq!(f.custom_path(), None);
        let f = PlanForm::parse(&json!({"customPath": "nope"}));
        assert_eq!(f.custom_path(), None);
    }

    /// 道次限定 1~8。
    #[test]
    fn plan_form_clamps_lane() {
        assert_eq!(PlanForm::parse(&json!({"lane": 99})).lane(), 8);
        assert_eq!(PlanForm::parse(&json!({"lane": 0})).lane(), 1);
        assert_eq!(PlanForm::parse(&json!({"lane": -3})).lane(), 1);
    }

    /// 登录/新增账号：手机号不去空白（旧代码原样比较），密码可以为空。
    #[test]
    fn add_account_form_keeps_raw_username() {
        use super::accounts::AddAccountForm;
        let f = AddAccountForm::parse(&json!({"username": " user_01 ", "password": ""}));
        assert_eq!(f.username.as_deref(), Some(" user_01 "));
        assert_eq!(f.password(), "");
        // 新增账号那条要求 trim 后非空，这正是 `username()` 的差别
        assert_eq!(f.username(), "user_01");
        let f = AddAccountForm::parse(&json!({"username": 1, "password": null}));
        assert_eq!(f.username(), "");
        assert_eq!(f.password(), "");
    }

    /// `/api/obs` 的 key 只认 `run_data/`、`runData/` 两个前缀。
    #[test]
    fn obs_form_key_prefix_is_checked() {
        use super::records::ObsFetchForm;
        let ok = ObsFetchForm::parse(&json!({"key": "run_data/acct/x"}));
        assert_eq!(ok.key(), Some("run_data/acct/x"));
        let ok2 = ObsFetchForm::parse(&json!({"key": "runData/x"}));
        assert_eq!(ok2.key(), Some("runData/x"));
        assert_eq!(
            ObsFetchForm::parse(&json!({"key": "etc/passwd"})).key(),
            None
        );
        assert_eq!(ObsFetchForm::parse(&json!({})).key(), None);
    }

    /// `/api/raw`：参数字段缺失时按 `{}` 透传，host 只有 `discovery` 才算发现域名。
    #[test]
    fn raw_form_defaults_and_host() {
        use super::records::RawForm;
        let f = RawForm::parse(&json!({"path": "/api/x"}));
        assert_eq!(f.path(), "/api/x");
        assert_eq!(f.inner(), json!({}));
        assert!(!f.is_discovery());
        let f = RawForm::parse(&json!({"host": "discovery", "body": {"a": 1}}));
        assert!(f.is_discovery());
        assert_eq!(f.inner(), json!({"a": 1}));
    }

    /// 出口配置：空白串等于直连，和旧代码的 `map(str::trim).filter` 一致。
    #[test]
    fn egress_form_blank_means_direct() {
        use super::meta::EgressForm;
        assert_eq!(EgressForm::parse(&json!({"proxy": "  "})).proxy(), None);
        assert_eq!(EgressForm::parse(&json!({"proxy": 8080})).proxy(), None);
        assert_eq!(
            EgressForm::parse(&json!({"proxy": " 1.2.3.4:9050 "})).proxy(),
            Some("1.2.3.4:9050")
        );
    }

    /// 批量刷：ids 里非字符串元素丢掉，`dry` 默认 false，间隔默认 8000ms。
    #[test]
    fn batch_form_defaults() {
        use super::accounts::BatchForm;
        let f = BatchForm::parse(&json!({"ids": ["a", 1, "b"], "gapMs": 0}));
        assert_eq!(f.ids(), vec!["a", "b"]);
        assert!(!f.dry());
        assert_eq!(f.gap_ms(), 0);
        let f = BatchForm::parse(&json!({}));
        assert!(f.ids().is_empty());
        assert_eq!(f.gap_ms(), 8000);
        assert!(!f.dry());
    }
}

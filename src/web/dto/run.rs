//! `/api/plan`、`/api/run`、`/api/accounts/batch` 共用的规划参数。
//!
//! 三个接口的字段是同一套（批量刷只是多几个控制字段），所以共用 [`PlanForm`]；
//! 默认值、坐标口径、海拔区间这些「业务解释」写成方法，处理器里不再各自
//! `unwrap_or`。

use serde::Deserialize;
use serde_json::Value;

/// 规划类请求的字段全集。缺失或类型不对的字段都是 `None`。
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct PlanForm {
    /// 目标距离（公里），默认 2.0。
    #[serde(deserialize_with = "super::opt_f64")]
    pub(crate) distance: Option<f64>,
    /// 目标时长（分钟），默认 12.0。
    #[serde(deserialize_with = "super::opt_f64")]
    pub(crate) duration: Option<f64>,
    /// 手填总步数；`0` 与缺失同义（交给配速推算）。
    #[serde(deserialize_with = "super::opt_i64")]
    pub(crate) steps: Option<i64>,
    /// 运动类型，默认 1（跑步）。
    #[serde(deserialize_with = "super::opt_i64")]
    pub(crate) sport_type: Option<i64>,
    /// 模式，默认 1。
    #[serde(deserialize_with = "super::opt_i64")]
    pub(crate) run_mode: Option<i64>,
    /// 起始时刻（毫秒时间戳）；非正数当没填。
    #[serde(deserialize_with = "super::opt_i64")]
    pub(crate) start_at_ms: Option<i64>,
    /// 中心点纬度。
    #[serde(deserialize_with = "super::opt_f64")]
    pub(crate) lat: Option<f64>,
    /// 中心点经度。
    #[serde(deserialize_with = "super::opt_f64")]
    pub(crate) lng: Option<f64>,
    /// 坐标口径：`gcj02`（默认）/`wgs84`/`bd09`。
    #[serde(deserialize_with = "super::opt_string")]
    pub(crate) coord: Option<String>,
    /// `stadium` 表示操场绕圈，其它值按自由路线。
    #[serde(deserialize_with = "super::opt_string")]
    pub(crate) mode: Option<String>,
    /// 跑道道次（1~8）。
    #[serde(deserialize_with = "super::opt_i64")]
    pub(crate) lane: Option<i64>,
    /// 终点纬度。
    #[serde(deserialize_with = "super::opt_f64")]
    pub(crate) end_lat: Option<f64>,
    /// 终点经度。
    #[serde(deserialize_with = "super::opt_f64")]
    pub(crate) end_lng: Option<f64>,
    /// 海拔区间下界（米）。
    #[serde(deserialize_with = "super::opt_f64")]
    pub(crate) alt_min: Option<f64>,
    /// 海拔区间上界（米）。
    #[serde(deserialize_with = "super::opt_f64")]
    pub(crate) alt_max: Option<f64>,
    /// 手绘轨迹：`[[lat,lng], ...]`，逐点过滤后不足 2 点当作没传。
    #[serde(deserialize_with = "custom_path_field")]
    pub(crate) custom_path: Option<Vec<(f64, f64)>>,
    /// 是否优先走道路，默认 `true`。
    #[serde(deserialize_with = "super::opt_bool")]
    pub(crate) prefer_roads: Option<bool>,
    /// 调用方要求核对当前登录账号；空串当没填。
    #[serde(deserialize_with = "super::opt_string")]
    pub(crate) expect_uid: Option<String>,
}

impl PlanForm {
    /// 从请求体解析。宽松反序列化不会失败，真失败（理论上不会）就当全空。
    pub(crate) fn parse(body: &Value) -> Self {
        serde_json::from_value(body.clone()).unwrap_or_default()
    }

    /// 目标距离（公里）。
    pub(crate) fn distance_km(&self) -> f64 {
        self.distance.unwrap_or(2.0)
    }

    /// 目标时长（分钟）。
    pub(crate) fn duration_min(&self) -> f64 {
        self.duration.unwrap_or(12.0)
    }

    /// 手填步数；`0` 或负数表示「没指定」。
    pub(crate) fn steps(&self) -> Option<i32> {
        self.steps
            .filter(|v| *v > 0)
            .and_then(crate::number::i64_to_i32)
    }

    /// 运动类型。
    pub(crate) fn sport_type(&self) -> i32 {
        self.sport_type
            .and_then(crate::number::i64_to_i32)
            .unwrap_or(1)
    }

    /// 模式编号。
    pub(crate) fn run_mode(&self) -> i64 {
        self.run_mode.unwrap_or(1)
    }

    /// 起始时刻；非正数当没填。
    pub(crate) fn start_at_ms(&self) -> Option<i64> {
        self.start_at_ms.filter(|v| *v > 0)
    }

    /// 坐标口径，默认 `gcj02`。
    pub(crate) fn coord_sys(&self) -> &str {
        self.coord.as_deref().unwrap_or("gcj02")
    }

    /// 请求里给的坐标；两个都缺任意一个就是 `None`。
    pub(crate) fn center(&self) -> Option<(f64, f64)> {
        Some((self.lat?, self.lng?))
    }

    /// 终点原始坐标；两个都是 0 表示没指定（保持闭环）。
    pub(crate) fn end_raw(&self) -> Option<(f64, f64)> {
        let (la, ln) = (self.end_lat?, self.end_lng?);
        (la != 0.0 || ln != 0.0).then_some((la, ln))
    }

    /// 海拔区间 `[lo, hi]`；上下界搞反、只填一个、超出地形范围都当没填。
    pub(crate) fn alt_range(&self) -> Option<(f64, f64)> {
        let (lo, hi) = (self.alt_min?, self.alt_max?);
        (hi > lo + 1.0 && lo > -500.0 && hi < 9000.0).then_some((lo, hi))
    }

    /// 手绘轨迹（BD-09 之前的口径由调用方转换）。
    pub(crate) fn custom_path(&self) -> Option<Vec<(f64, f64)>> {
        self.custom_path.clone()
    }

    /// 是否操场绕圈。
    pub(crate) fn prefer_stadium(&self) -> bool {
        self.mode.as_deref() == Some("stadium")
    }

    /// 是否优先走道路。
    pub(crate) fn prefer_roads(&self) -> bool {
        self.prefer_roads.unwrap_or(true)
    }

    /// 道次，限定在 1~8。
    pub(crate) fn lane(&self) -> u8 {
        crate::number::i64_to_u8(self.lane.unwrap_or(1).clamp(1, 8)).unwrap_or(1)
    }

    /// 要求核对的账号；空串当没要求。
    pub(crate) fn expect_uid(&self) -> Option<&str> {
        self.expect_uid.as_deref().filter(|v| !v.is_empty())
    }
}

/// `customPath` 的宽松解析：非数组当没填；数组里每项必须是 `[lat, lng]`，
/// 明显不是经纬度的点丢掉；剩下不足 2 点也当没传。
fn custom_path_field<'de, D>(d: D) -> Result<Option<Vec<(f64, f64)>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let v = Value::deserialize(d)?;
    let Some(arr) = v.as_array() else {
        return Ok(None);
    };
    let pts: Vec<(f64, f64)> = arr
        .iter()
        .filter_map(|item| {
            let pair = item.as_array()?;
            let lat = pair.first()?.as_f64()?;
            let lng = pair.get(1)?.as_f64()?;
            if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lng) {
                return None;
            }
            Some((lat, lng))
        })
        .collect();
    Ok((pts.len() >= 2).then_some(pts))
}

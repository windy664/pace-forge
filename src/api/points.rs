//! 校园点位：`POST /api/v560/get/1/distance/1`（sportType=4）。
//!
//! 这个接口和别的不一样，有两个特殊字段：
//!
//! - `runec`：把 `{uid}{经度6位}{纬度6位}{整秒时间戳}` 打成**额外一层信封**，
//!   外层顺序是 `k,p,d,h,t`（不是标准体的 `d,h,k,p,t`）
//! - `sign`：`MD5(http版URL + 盐)`，注意是 **http://** 不是 https://
//!
//! 还有限流：**5 分钟 3 次**（错误码 10603）。所以必须有缓存，不能每次跑都拉。

use serde_json::{Value, json};

use super::client::{Client, expect_ok};
use super::policy::point_xy;
use crate::crypto::consts::{SIGN_SALT, hosts};
use crate::crypto::envelope::{OuterOrder, now_ms, seal_with_order};
use crate::error::{Error, ErrorKind};

/// 点位接口路径（默认形态：`distance/1`）。
pub const POINTS_PATH: &str = "/api/v560/get/1/distance/1";

/// 按目标距离生成点位路径。
///
/// 路径末段是**目标距离（米）**，不是固定 `1`。有的学校只在带真实距离时
/// 才下发点位，写死 `distance/1` 会拿到空集——表现就是「这个学校的打卡点
/// 拉不起来」，而返回 200 不带错误码，很难往请求上想。
pub fn points_path(distance_m: i64) -> String {
    format!("/api/v560/get/1/distance/{}", distance_m.max(1))
}

/// 缓存有效期（毫秒）。服务端 5 分钟 3 次，取 5 分钟做保守值。
pub const POINTS_TTL_MS: i64 = 300_000;

/// 点位上下文。
#[derive(Debug, Clone, Default)]
pub struct PointsContext {
    /// 原始点位数组（服务端原样结构）。
    pub points: Vec<Value>,
    /// 解析后的 BD-09 坐标。
    pub coords: Vec<(f64, f64)>,
    /// 同一批点位的 GCJ-02 坐标，顺序与 `coords` 对齐。
    /// 高德地图按 GCJ-02 网格切瓦片，前端摆点位要用这一套。
    pub gcj_coords: Vec<(f64, f64)>,
}

/// 经纬度六位小数字符串。
fn six_digit(v: f64) -> String {
    format!("{v:.6}")
}

/// URL 签名：`MD5(http版URL + 盐)`。
///
/// 注意是把 `https://` 换成 `http://` 再算——早期用 https 算出来全是
/// `error=10003 签名错误`。
pub fn url_sign(url: &str) -> String {
    let http_url = url.replacen("https://", "http://", 1);
    let mut hasher = md5::Md5::new();
    use md5::Digest;
    hasher.update(format!("{http_url}{SIGN_SALT}").as_bytes());
    format!("{:x}", hasher.finalize())
}

/// 构造 `runec` 载荷。
///
/// 明文 = `{uid}{lon:.6}{lat:.6}{整秒ms}`
pub fn runec_plain(uid: &str, lon: f64, lat: f64, start_ms: i64) -> String {
    format!(
        "{uid}{}{}{}",
        six_digit(lon),
        six_digit(lat),
        (start_ms / 1000) * 1000
    )
}

/// 拉点位（不带缓存）。
pub fn fetch_points(
    client: &Client,
    lat: f64,
    lon: f64,
    run_area_id: Option<&str>,
) -> Result<PointsContext, Error> {
    fetch_points_at_distance(client, lat, lon, run_area_id, 1)
}

/// 带目标距离的点位拉取。
///
/// `distance_m` 会进 URL 末段；服务端按这个距离挑点位，所以距离给错
/// 等于让服务端按另一条路线出点。
pub fn fetch_points_at_distance(
    client: &Client,
    lat: f64,
    lon: f64,
    run_area_id: Option<&str>,
    distance_m: i64,
) -> Result<PointsContext, Error> {
    if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
        return Err(ErrorKind::client(format!("锚点越界: ({lat}, {lon})")));
    }
    if lat == 0.0 && lon == 0.0 {
        return Err(ErrorKind::client("锚点不能是 (0,0)".into()));
    }

    let uid = client
        .credentials()
        .and_then(|c| c.uid.as_ref())
        .cloned()
        .unwrap_or_else(|| "0".into());
    let unid = client
        .credentials()
        .and_then(|c| c.unid.as_ref())
        .cloned()
        .unwrap_or_else(|| "0".into());

    let url = format!("{}{}", hosts::RUN, points_path(distance_m));
    let start_ms = now_ms();
    let runec_plain = runec_plain(&uid, lon, lat, start_ms);
    let env = seal_with_order(&runec_plain, OuterOrder::Observed)?;

    let mut body = json!({
        "sportType": 4,
        "longitude": lon,
        "latitude": lat,
        "sign": url_sign(&url),
        "uuid": super::client::uuid_v4(),
        "selectedUnid": unid,
        "runec": env.wire,
    });
    if let Some(area) = run_area_id {
        body["runAreaId"] = json!(area);
    }

    let raw = client.request("POST", &url, Some(&body))?;
    let v = expect_ok(&raw)?;
    parse_points(&v)
}

/// 解析点位响应。
pub fn parse_points(v: &Value) -> Result<PointsContext, Error> {
    let payload = v.get("data").unwrap_or(v);
    let points = extract_points(payload)?;
    let coords = points.iter().filter_map(point_xy).collect();
    let gcj_coords = points.iter().filter_map(super::policy::point_gcj).collect();
    Ok(PointsContext {
        points,
        coords,
        gcj_coords,
    })
}

/// 嵌套下钻的最大层数。
///
/// 上游若回传 `data.result.data.result…` 的深层畸形链，逐层下钻会耗尽调用栈；
/// 超过这个深度就明确报错中止，不再继续递归。
const EXTRACT_MAX_DEPTH: usize = 5;

/// 从响应里找点位数组。
fn extract_points(payload: &Value) -> Result<Vec<Value>, Error> {
    extract_points_depth(payload, 0)
}

fn extract_points_depth(payload: &Value, depth: usize) -> Result<Vec<Value>, Error> {
    // 优先显式的打卡点字段名
    for name in [
        "pointsResModels",
        "pointList",
        "points",
        "runPoints",
        "checkPointList",
    ] {
        let Some(val) = payload.get(name) else {
            continue;
        };
        match val {
            Value::Array(a) => return Ok(a.clone()),
            // 有些版本把数组序列化成字符串
            Value::String(s) => {
                if let Ok(Value::Array(a)) = serde_json::from_str::<Value>(s) {
                    return Ok(a);
                }
            }
            _ => {}
        }
    }
    if depth >= EXTRACT_MAX_DEPTH {
        return Err(ErrorKind::protocol(
            "点位响应嵌套过深，已按畸形响应中止解析".into(),
        ));
    }
    // 再试一层嵌套
    for wrapper in ["result", "data"] {
        if let Some(inner) = payload.get(wrapper)
            && inner.get("pointsResModels").is_some()
        {
            return extract_points_depth(inner, depth + 1);
        }
    }
    Ok(Vec::new())
}

/// 点位中心（BD-09）。
pub fn center_bd(points: &[(f64, f64)]) -> Option<(f64, f64)> {
    if points.is_empty() {
        return None;
    }
    let n = points.len() as f64;
    let lat = points.iter().map(|p| p.0).sum::<f64>() / n;
    let lon = points.iter().map(|p| p.1).sum::<f64>() / n;
    Some((lat, lon))
}

/// 点位包围盒（BD-09）：(min_lat, min_lon, max_lat, max_lon)。
pub fn bbox(points: &[(f64, f64)]) -> Option<(f64, f64, f64, f64)> {
    if points.is_empty() {
        return None;
    }
    let mut b = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
    for &(lat, lon) in points {
        b.0 = b.0.min(lat);
        b.1 = b.1.min(lon);
        b.2 = b.2.max(lat);
        b.3 = b.3.max(lon);
    }
    Some(b)
}

/// 进程内点位缓存。
///
/// 服务端限流：同一个 runAreaId 5 分钟内最多取 3 次，超了返回 10603。
/// 规划一次要拉点位，重试一次就撞墙，所以按围栏 id 缓存 5 分钟。
///
/// **多槽而不是单槽。** 早先这里是个 `Option`——只留最后一次的结果。
/// 只要切一次围栏（或者改一次锚点/目标距离），另一份就被顶掉，切回去必然
/// 重拉上游；而上游对同一个位置每次可能给出不同的点位集合，用户看到的就是
/// 「一生成路线点位就变了」。缓存要按账号/学校/区域/位置各留一份，
/// 不能只留最后一份。
static CACHE: std::sync::Mutex<Option<Cache>> = std::sync::Mutex::new(None);

const CACHE_TTL_SECS: u64 = 300;
/// 槽位上限。key 里带围栏 id 和四位小数的锚点，正常一个账号十来个围栏
/// 就到头了；给 64 是防「手动点地图到处试」把表撑大，超了先清过期项。
const CACHE_MAX_ENTRIES: usize = 64;

struct Cache {
    map: std::collections::HashMap<String, (std::time::Instant, PointsContext)>,
}

impl Cache {
    fn new() -> Self {
        Self {
            map: std::collections::HashMap::new(),
        }
    }

    fn get(&self, k: &str) -> Option<PointsContext> {
        self.map
            .get(k)
            .filter(|(at, _)| at.elapsed().as_secs() < CACHE_TTL_SECS)
            .map(|(_, ctx)| ctx.clone())
    }

    fn put(&mut self, k: &str, ctx: &PointsContext) {
        if self.map.len() >= CACHE_MAX_ENTRIES && !self.map.contains_key(k) {
            // 先丢过期的；还有满的才动刀丢最早过期的那个，绝不整表清空
            self.map
                .retain(|_, (at, _)| at.elapsed().as_secs() < CACHE_TTL_SECS);
            if self.map.len() >= CACHE_MAX_ENTRIES
                && let Some(oldest) = self
                    .map
                    .iter()
                    .min_by_key(|(_, (at, _))| *at)
                    .map(|(k, _)| k.clone())
            {
                self.map.remove(&oldest);
            }
        }
        self.map
            .insert(k.to_string(), (std::time::Instant::now(), ctx.clone()));
    }
}

/// 带缓存的点位拉取。同一围栏 5 分钟内只打一次接口。
pub fn fetch_points_cached(
    client: &super::client::Client,
    lat: f64,
    lng: f64,
    run_area_id: Option<&str>,
) -> Result<PointsContext, Error> {
    fetch_points_cached_at(client, lat, lng, run_area_id, None)
}

/// 带「空点位重试」的缓存拉取。
///
/// 先按 `distance/1` 拉；拿到空集且给了 `fallback_distance_m` 时，再按真实
/// 目标距离拉一次。两次用不同的缓存 key（距离不同 = 不同请求），但仍然
/// 受 5 分钟 3 次限流约束——所以只在**空集**时才多打一发，正常学校不加请求。
pub fn fetch_points_cached_at(
    client: &super::client::Client,
    lat: f64,
    lng: f64,
    run_area_id: Option<&str>,
    fallback_distance_m: Option<i64>,
) -> Result<PointsContext, Error> {
    let key = |d: i64| format!("{}:{lat:.4},{lng:.4}:d{d}", run_area_id.unwrap_or("-"));
    let read = |k: &str| -> Option<PointsContext> {
        CACHE
            .lock()
            .ok()
            .and_then(|g| g.as_ref().and_then(|c| c.get(k)))
    };
    let write = |k: &str, ctx: &PointsContext| {
        if let Ok(mut g) = CACHE.lock() {
            g.get_or_insert_with(Cache::new).put(k, ctx);
        }
    };

    let k1 = key(1);
    if let Some(c) = read(&k1) {
        return Ok(c);
    }
    let ctx = fetch_points_at_distance(client, lat, lng, run_area_id, 1)?;
    // 只缓存非空结果。上游瞬时空响应（限流、灰度、后端抖动）也写进去的话，
    // 接下来 5 分钟全命中这个空缓存不再重试，用户只会看到「这个围栏没点位」。
    if !ctx.points.is_empty() {
        write(&k1, &ctx);
        return Ok(ctx);
    }
    let Some(d) = fallback_distance_m.filter(|d| *d > 1) else {
        return Ok(ctx);
    };
    let k2 = key(d);
    if let Some(c) = read(&k2) {
        return Ok(c);
    }
    let ctx2 = fetch_points_at_distance(client, lat, lng, run_area_id, d)?;
    if !ctx2.points.is_empty() {
        write(&k2, &ctx2);
    }
    Ok(ctx2)
}

/// 手动清缓存（换围栏时用，或者排查「点位怎么和上次不一样」）。
pub fn clear_cache() {
    if let Ok(mut g) = CACHE.lock()
        && let Some(c) = g.as_mut()
    {
        c.map.clear();
    }
}

/// 把点位转成 GCJ-02（OSM 路网对齐用）。
pub fn to_gcj(points: &[(f64, f64)]) -> Vec<(f64, f64)> {
    points
        .iter()
        .map(|&(lat, lon)| super::coord::bd09_to_gcj02(lat, lon))
        .collect()
}

/// 边长（米），用于判断点位分布范围是否合理。
pub fn span_m(points: &[(f64, f64)]) -> f64 {
    match bbox(points) {
        Some((min_lat, min_lon, max_lat, max_lon)) => {
            const LAT_M: f64 = 111_132.0;
            let dy = (max_lat - min_lat) * LAT_M;
            let dx = (max_lon - min_lon) * LAT_M * ((min_lat + max_lat) / 2.0).to_radians().cos();
            (dx * dx + dy * dy).sqrt()
        }
        None => 0.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 换围栏再换回来，缓存不能互相顶掉。
    ///
    /// 早先 CACHE 是单槽 `Option`，存一个 key 就把上一个挤掉：A 围栏规划完
    /// 去看 B，再切回 A 必然重拉上游，而上游对同一位置每次给的点位可能不同，
    /// 用户看到的就是「一生成路线点位就变了」。
    #[test]
    fn cache_keeps_multiple_fences() {
        let mut c = Cache::new();
        let mk = |name: &str| PointsContext {
            points: vec![serde_json::json!({"pointName": name})],
            coords: vec![(30.5, 104.0)],
            gcj_coords: vec![(30.5, 104.0)],
        };
        c.put("fenceA:d1", &mk("A"));
        c.put("fenceB:d1", &mk("B"));
        assert!(c.get("fenceA:d1").is_some(), "A 不该被 B 顶掉");
        assert!(c.get("fenceB:d1").is_some());
        // 命中值也要对得上，别串了槽
        let a = c.get("fenceA:d1").unwrap();
        assert_eq!(a.points[0]["pointName"], "A");
    }

    /// 槽位满了要淘汰，但不能把不在上限内的有效项整表清掉。
    #[test]
    fn cache_evicts_without_clearing_everything() {
        let mut c = Cache::new();
        let ctx = PointsContext {
            points: vec![serde_json::json!({"pointName": "x"})],
            coords: vec![(30.5, 104.0)],
            gcj_coords: vec![(30.5, 104.0)],
        };
        for i in 0..CACHE_MAX_ENTRIES {
            c.put(&format!("k{i}"), &ctx);
        }
        assert_eq!(c.map.len(), CACHE_MAX_ENTRIES);
        // 再加一个：先清过期的（这里没有），再丢最老的一个
        c.put("overflow", &ctx);
        assert!(c.map.len() <= CACHE_MAX_ENTRIES, "不能无限增长");
        assert!(c.get("overflow").is_some(), "新写入的要能读到");
    }

    #[test]
    fn url_sign_uses_http() {
        // 同一个 URL，http 与 https 版本必须给出不同结果
        let a = url_sign("https://run.gxapp.iydsj.com/api/x");
        let b = url_sign("http://run.gxapp.iydsj.com/api/x");
        assert_eq!(a, b, "实现内部会把 https 换成 http，两者应一致");
        assert_eq!(a.len(), 32);
    }

    #[test]
    fn runec_plain_format() {
        let s = runec_plain("12345", 104.0668001, 30.5728002, 1_730_000_123_456);
        // uid + lon6 + lat6 + 整秒ms
        assert!(s.starts_with("12345"));
        assert!(s.contains("104.066800"));
        assert!(s.contains("30.572800"));
        assert!(s.ends_with("1730000123000"), "时间戳应截断到整秒: {s}");
    }

    #[test]
    fn parses_points_from_various_shapes() {
        let v = json!({
            "data": {
                "pointsResModels": [
                    {"lat": 30.5, "lon": 104.0},
                    {"lat": 30.51, "lon": 104.01}
                ]
            }
        });
        let ctx = parse_points(&v).unwrap();
        assert_eq!(ctx.points.len(), 2);
        assert_eq!(ctx.coords.len(), 2);
        assert!((ctx.coords[0].0 - 30.5).abs() < 1e-9);
    }

    #[test]
    fn parses_string_encoded_array() {
        let inner = r#"[{"lat":30.5,"lon":104.0}]"#;
        let v = json!({ "data": { "pointsResModels": inner } });
        let ctx = parse_points(&v).unwrap();
        assert_eq!(ctx.points.len(), 1);
    }

    #[test]
    fn empty_response_is_not_error() {
        let v = json!({ "data": {} });
        let ctx = parse_points(&v).unwrap();
        assert!(ctx.points.is_empty());
    }

    #[test]
    fn deeply_nested_response_is_rejected_not_stack_overflown() {
        // 畸形响应：data 里再套 data，逐层下钻。深度受限后必须明确报错，
        // 而不是一路递归到栈溢出。
        let mut v = json!({"pointsResModels": {}, "data": json!({"pointsResModels": []})});
        for _ in 0..60 {
            v = json!({"pointsResModels": {}, "data": v});
        }
        let r = parse_points(&v);
        assert!(r.is_err(), "嵌套超过深度上限必须报错: {r:?}");
    }

    #[test]
    fn center_and_bbox() {
        let pts = vec![(30.5, 104.0), (30.6, 104.2)];
        let (lat, lon) = center_bd(&pts).unwrap();
        assert!((lat - 30.55).abs() < 1e-9);
        assert!((lon - 104.1).abs() < 1e-9);
        let (a, b, c, d) = bbox(&pts).unwrap();
        assert_eq!((a, b, c, d), (30.5, 104.0, 30.6, 104.2));
        assert!(span_m(&pts) > 19_000.0, "跨 0.1 度纬度应约 11km+");
    }

    #[test]
    fn gcj_conversion_changes_coords() {
        let pts = vec![(30.5, 104.0)];
        let g = to_gcj(&pts);
        assert!((g[0].0 - 30.5).abs() > 1e-6, "BD→GCJ 应有偏移");
    }

    #[test]
    fn path_uses_target_distance() {
        assert_eq!(points_path(1), "/api/v560/get/1/distance/1");
        assert_eq!(points_path(2000), "/api/v560/get/1/distance/2000");
        // 0/负数统一钳到 1，避免发出 distance/0
        assert_eq!(points_path(0), "/api/v560/get/1/distance/1");
        assert_eq!(points_path(-5), "/api/v560/get/1/distance/1");
        // 默认常量必须与 distance/1 一致，否则缓存 key 与真实请求会对不上
        assert_eq!(points_path(1), POINTS_PATH);
    }
}

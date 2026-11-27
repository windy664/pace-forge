//! 真实路网：从 OpenStreetMap 拉围栏内的道路，在图上路由。
//!
//! 之前是纯几何蛇形填充，完全不看地形——围栏里有湖、有河、有楼，轨迹照样
//! 直线穿过去。地图上一眼就看出不是人跑的，服务端也可能按轨迹异常判掉。
//!
//! 这里走数据源：Overpass API 拉 bbox 内的 `highway=*`（人行道、小路、校园
//! 道路、台阶都算），栅格化成图，然后沿图走。

use super::geo::{distance, m_per_deg_lat, m_per_deg_lng};
use crate::api::session::home_dir;
use crate::error::{Error, ErrorKind};
use serde_json::Value;

/// Overpass 端点。主站经常 502/504，准备了两个镜像轮询。
pub(crate) const OVERPASS_HOSTS: [&str; 3] = [
    "https://overpass-api.de/api/interpreter",
    "https://overpass.kumi.systems/api/interpreter",
    "https://overpass.private.coffee/api/interpreter",
];

/// 单个端点的超时。原来这里是 60~70 秒，再乘 3 个镜像、3 类查询（建筑、
/// 水体、路网），国内网络打不开 Overpass 时一次生成能卡近十分钟——用户看到
/// 的就是「点了生成，半天没反应」。校园 bbox 的查询正常几秒就回，6 秒足够。
const HOST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(6);

/// 竞速窗口：三个端点**同时**打，先回先用。
///
/// 之前是串行 for 循环，每个端点各等 `HOST_TIMEOUT`：全线不通时 3×10=30 秒
/// 才进冷却，用户报的「8~9 秒超时」就是单端点在排队。并发之后总等待等于
/// 最快的那个端点，不通时最坏也只有这一个窗口。
///
/// 窗口与单端点超时取同一个值：窗口结束后线程最多再活一个 `HOST_TIMEOUT`
/// 就自行退出，不会在后台堆线程。
const RACE_WINDOW: std::time::Duration = HOST_TIMEOUT;

/// 三个端点全线失败后的冷却时长：这段时间内不再打 Overpass，直接走瓦片/几何。
///
/// 没有它的话，同一次生成里建筑、水体、路网三查各失败一遍（30 秒），
/// 下一次生成又从零开始——网络不通的用户每次都要等一遍。
const DOWN_COOLDOWN_MS: u64 = 600_000;

static DOWN_AT_MS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn now_ms_u64() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// 冷却判断抽成纯函数，方便用固定时刻测窗口边界。
fn cooling_at(now_ms: u64, down_at_ms: u64) -> bool {
    down_at_ms != 0 && now_ms.saturating_sub(down_at_ms) < DOWN_COOLDOWN_MS
}

/// 打单个端点。
fn post_one(host: &str, body: &str) -> Result<String, Error> {
    match ureq::post(host)
        .set("User-Agent", "pace-forge/0.1 (route planning)")
        .set("Content-Type", "application/x-www-form-urlencoded")
        .timeout(HOST_TIMEOUT)
        .send_string(body)
    {
        Ok(r) => match r.into_string() {
            Ok(t) if t.contains("\"elements\"") => Ok(t),
            Ok(_) => Err(ErrorKind::network(format!("{host} 内容异常"))),
            Err(e) => Err(ErrorKind::network(format!("{host} 读取失败: {e}"))),
        },
        Err(e) => Err(ErrorKind::network(format!("{host} 请求失败: {e}"))),
    }
}

/// 从竞速通道里取第一个成功结果，同时收集各端点的失败原因。
///
/// 只依赖 channel，不碰网络：测试里喂假结果就能覆盖「先到即用」「全失败」
/// 「窗口内一个都没回」三种情形。
fn select_first_ok(
    rx: &std::sync::mpsc::Receiver<(usize, Result<String, Error>)>,
    window: std::time::Duration,
) -> (Option<String>, Vec<String>) {
    let deadline = std::time::Instant::now() + window;
    let mut errs = Vec::new();
    loop {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        if left.is_zero() {
            break;
        }
        match rx.recv_timeout(left) {
            Ok((_idx, Ok(text))) => return (Some(text), errs),
            Ok((_idx, Err(e))) => errs.push(e.to_string()),
            // 超时，或者所有发送端都已退出（小概率：端点比窗口先全挂）
            Err(_) => break,
        }
    }
    (None, errs)
}

/// 打 Overpass：并发竞速、窗口超时、全线失败后进入冷却。
fn overpass_post(body: &str, what: &str) -> Result<String, Error> {
    if cooling_at(
        now_ms_u64(),
        DOWN_AT_MS.load(std::sync::atomic::Ordering::Relaxed),
    ) {
        return Err(ErrorKind::fence(format!(
            "Overpass 刚失败过，{} 分钟内不再重试（{what}）",
            DOWN_COOLDOWN_MS / 60_000
        )));
    }
    let (tx, rx) = std::sync::mpsc::channel();
    for (idx, host) in OVERPASS_HOSTS.iter().enumerate() {
        let tx = tx.clone();
        let host = *host;
        let body = body.to_string();
        // 不 join：窗口一到就返回，慢端点自行超时退出（最长再等 HOST_TIMEOUT）。
        std::thread::spawn(move || {
            let _ = tx.send((idx, post_one(host, &body)));
        });
    }
    drop(tx);

    let (ok, errs) = select_first_ok(&rx, RACE_WINDOW);
    if let Some(text) = ok {
        return Ok(text);
    }
    DOWN_AT_MS.store(now_ms_u64(), std::sync::atomic::Ordering::Relaxed);
    let last = errs
        .last()
        .cloned()
        .unwrap_or_else(|| "窗口内没有端点返回".into());
    Err(ErrorKind::network(format!(
        "所有 Overpass 端点都失败（{what}，并发竞速 {}s，最后一次: {last}）",
        RACE_WINDOW.as_secs()
    )))
}

/// 打算走的路类型。excluding motorway/trunk：校园里没有，且人不能跑。
/// 包括 footway/path/pedestrian/steps/track/service/residential/unclassified/living_street/cycleway。
const WALKABLE: &str = "footway|path|pedestrian|steps|track|service|residential|unclassified|living_street|cycleway|tertiary|secondary";

/// 一段路（OSM way 的几何）。
#[derive(Debug, Clone)]
pub struct RoadSegment {
    /// 折线顶点（lat, lon）。
    pub points: Vec<(f64, f64)>,
    /// OSM 的道路等级（`residential` 等）。
    pub kind: String,
}

/// 拉来的路网。
#[derive(Debug, Clone, Default)]
pub struct RoadNet {
    /// 全部路段。
    pub segments: Vec<RoadSegment>,
    /// 数据来源描述，用于日志。
    pub source: String,
    /// 道路栅格（只有瓦片来源才有）。段间连边用它校验两点之间的直线
    /// 是不是真的贴着路面——不加这一步，两条路各自贴着楼的两侧、节点
    /// 相距 12m 内就被连上，A* 会直接从楼中间穿过去。
    pub mask: Option<crate::track::tileroad::Mask>,
}

impl RoadNet {
    /// 一段路都没拉到。
    pub fn is_empty(&self) -> bool {
        self.segments.is_empty()
    }

    /// 只保留落在围栏内的路段。
    ///
    /// OSM 里的市政道路（residential/tertiary）常常横穿围栏边界，直接走会
    /// 跑出学校。裁剪后只留围栏内部的，剩下的长度不够时调用方自己决定
    /// 退回几何规划。
    pub fn clip_to_fence(&self, fence: &[(f64, f64)]) -> RoadNet {
        use super::fence_route::contains;
        let mut segments = Vec::new();
        for seg in &self.segments {
            // 把连续的「在围栏内」的点切成独立段
            let mut cur: Vec<(f64, f64)> = Vec::new();
            for &p in &seg.points {
                if contains(fence, p.0, p.1) {
                    cur.push(p);
                } else if cur.len() >= 2 {
                    segments.push(RoadSegment {
                        points: std::mem::take(&mut cur),
                        kind: seg.kind.clone(),
                    });
                } else {
                    cur.clear();
                }
            }
            if cur.len() >= 2 {
                segments.push(RoadSegment {
                    points: cur,
                    kind: seg.kind.clone(),
                });
            }
        }
        RoadNet {
            source: format!("{} · 围栏内 {} 段", self.source, segments.len()),
            mask: self.mask.clone(),
            segments,
        }
    }

    /// 总长度（米）。
    pub fn total_length_m(&self) -> f64 {
        self.segments
            .iter()
            .map(|s| {
                s.points
                    .windows(2)
                    .map(|w| distance(w[0], w[1]))
                    .sum::<f64>()
            })
            .sum()
    }
}

/// 从 Overpass 拉 bbox 内的可跑路网。
///
/// 失败（网络、超时、限流）返回 Err，调用方退回几何规划——不能因为拿不到
/// 路网就完全不能跑。
pub fn fetch(bbox: (f64, f64, f64, f64)) -> Result<RoadNet, Error> {
    // 先看本地缓存。Overpass 主站经常 502/504，镜像也不稳，
    // 一次拉取要几十秒，缓存起来按 bbox 网格复用。
    if let Some(net) = cache_get(bbox) {
        return Ok(net);
    }
    let (min_lat, min_lon, max_lat, max_lon) = bbox;
    if !(min_lat < max_lat && min_lon < max_lon) {
        return Err(ErrorKind::fence("bbox 非法".into()));
    }
    let query = format!(
        "[out:json][timeout:40];\
         (way[\"highway\"~\"^({WALKABLE})$\"]({min_lat},{min_lon},{max_lat},{max_lon});\
          way[\"leisure\"=\"track\"]({min_lat},{min_lon},{max_lat},{max_lon}););\
         out geom;"
    );

    let body = format!("data={}", urlencode(&query));
    let text = overpass_post(&body, "路网")?;

    let v: Value = serde_json::from_str(&text)
        .map_err(|e| ErrorKind::fence(format!("路网 JSON 解析失败: {e}")))?;
    let elements = v
        .get("elements")
        .and_then(|e| e.as_array())
        .ok_or_else(|| ErrorKind::fence("路网响应缺少 elements".into()))?;

    let mut segments = Vec::new();
    for el in elements {
        let Some(geom) = el.get("geometry").and_then(|g| g.as_array()) else {
            continue;
        };
        let pts: Vec<(f64, f64)> = geom
            .iter()
            .filter_map(|p| {
                let lat = p.get("lat")?.as_f64()?;
                let lon = p.get("lon")?.as_f64()?;
                Some((lat, lon))
            })
            .collect();
        if pts.len() < 2 {
            continue;
        }
        let kind = el
            .get("tags")
            .and_then(|t| t.get("highway"))
            .and_then(|h| h.as_str())
            .or_else(|| {
                el.get("tags")
                    .and_then(|t| t.get("leisure"))
                    .and_then(|h| h.as_str())
            })
            .unwrap_or("unknown")
            .to_string();
        segments.push(RoadSegment { points: pts, kind });
    }

    if segments.is_empty() {
        return Err(ErrorKind::fence("该区域 OSM 里没有可用道路".into()));
    }

    let net = RoadNet {
        source: format!("OSM/Overpass · {} 段", segments.len()),
        mask: None,
        segments,
    };
    cache_put(bbox, &net);
    Ok(net)
}

/// 本地文件缓存路径，按 bbox 保留 3 位小数（约 100m 精度）。
///
/// 缓存目录走 `session::data_dir_in`，**纯路径计算不碰磁盘**；建目录的责任
/// 在写盘处（`session::write_file_private`）。生产入口是
/// [`cache_get`] / [`cache_put`]，它们用 `home_dir()` 作为 base 转调本函数。
fn cache_path_in(base: &std::path::Path, bbox: (f64, f64, f64, f64)) -> std::path::PathBuf {
    let key = format!("{:.3}_{:.3}_{:.3}_{:.3}", bbox.0, bbox.1, bbox.2, bbox.3);
    crate::api::session::data_dir_in(base).join(format!("roads-{key}.json"))
}

fn cache_get(bbox: (f64, f64, f64, f64)) -> Option<RoadNet> {
    cache_get_in(&home_dir(), bbox)
}

fn cache_get_in(base: &std::path::Path, bbox: (f64, f64, f64, f64)) -> Option<RoadNet> {
    let path = cache_path_in(base, bbox);
    let meta = std::fs::metadata(&path).ok()?;
    // 超过 7 天就重拉
    if let Ok(age) = meta
        .modified()
        .and_then(|t| t.elapsed().map_err(std::io::Error::other))
        && age.as_secs() > 7 * 86400
    {
        return None;
    }
    let text = std::fs::read_to_string(&path).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    let arr = v.get("segments")?.as_array()?;
    let mut segments = Vec::new();
    for s in arr {
        let pts: Vec<(f64, f64)> = s
            .get("points")?
            .as_array()?
            .iter()
            .filter_map(|p| {
                let a = p.as_array()?;
                Some((a.first()?.as_f64()?, a.get(1)?.as_f64()?))
            })
            .collect();
        if pts.len() >= 2 {
            segments.push(RoadSegment {
                points: pts,
                kind: s
                    .get("kind")
                    .and_then(|k| k.as_str())
                    .unwrap_or("unknown")
                    .into(),
            });
        }
    }
    if segments.is_empty() {
        return None;
    }
    Some(RoadNet {
        source: format!("本地缓存 · {} 段", segments.len()),
        mask: None,
        segments,
    })
}

fn cache_put(bbox: (f64, f64, f64, f64), net: &RoadNet) {
    cache_put_in(&home_dir(), bbox, net)
}

fn cache_put_in(base: &std::path::Path, bbox: (f64, f64, f64, f64), net: &RoadNet) {
    let path = cache_path_in(base, bbox);
    let segs: Vec<Value> = net
        .segments
        .iter()
        .map(|s| {
            serde_json::json!({
                "kind": s.kind,
                "points": s.points.iter().map(|p| vec![p.0, p.1]).collect::<Vec<_>>(),
            })
        })
        .collect();
    let body = serde_json::json!({ "segments": segs });
    // 目录不存在时由 write_file_private 建出来（首次运行）
    let _ = crate::api::session::write_file_private(&path, body.to_string().as_bytes());
}

/// 拉围栏内的水域（湖、河、池塘）。
///
/// 蛇形填充是纯几何的，不知道哪里有水，轨迹就直接从湖面上穿过去。
/// 拿到水域多边形后，扫描时在这些区域挖洞。
pub fn fetch_water(bbox: (f64, f64, f64, f64)) -> Result<Vec<Vec<(f64, f64)>>, Error> {
    if let Some(w) = water_cache_get(bbox) {
        return Ok(w);
    }
    let (min_lat, min_lon, max_lat, max_lon) = bbox;
    let query = format!(
        "[out:json][timeout:30];         (way[\"natural\"=\"water\"]({min_lat},{min_lon},{max_lat},{max_lon});          way[\"waterway\"~\"river|stream|canal\"]({min_lat},{min_lon},{max_lat},{max_lon}););         out geom;"
    );
    let body = format!("data={}", urlencode(&query));
    let text = overpass_post(&body, "水域")?;
    let v: Value = serde_json::from_str(&text)
        .map_err(|e| ErrorKind::fence(format!("水域 JSON 解析失败: {e}")))?;
    let mut polys = Vec::new();
    if let Some(els) = v.get("elements").and_then(|e| e.as_array()) {
        for el in els {
            let Some(geom) = el.get("geometry").and_then(|g| g.as_array()) else {
                continue;
            };
            let pts: Vec<(f64, f64)> = geom
                .iter()
                .filter_map(|p| Some((p.get("lat")?.as_f64()?, p.get("lon")?.as_f64()?)))
                .collect();
            if pts.len() >= 3 {
                polys.push(pts);
            }
        }
    }
    water_cache_put(bbox, &polys);
    Ok(polys)
}

/// 本地手绘禁区（Web 地图上圈出来的楼、施工区、封路）。
///
/// OSM 在国内校园几乎空白，Overpass 拉不到建筑轮廓，只能人工标一次。
/// 存一个文件，所有账号、所有之后的规划都用它。
pub fn load_local_blockers() -> Vec<Vec<(f64, f64)>> {
    load_local_blockers_in(&home_dir())
}

/// 同上，但 HOME 基目录由调用方指定（测试注入隔离目录用）。
fn load_local_blockers_in(base: &std::path::Path) -> Vec<Vec<(f64, f64)>> {
    let path = blockers_path_in(base);
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    let Ok(v) = serde_json::from_str::<Value>(&text) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    if let Some(arr) = v.get("polys").and_then(|p| p.as_array()) {
        for poly in arr {
            let Some(pts) = poly.as_array() else { continue };
            let pts: Vec<(f64, f64)> = pts
                .iter()
                .filter_map(|p| {
                    let a = p.as_array()?;
                    Some((a.first()?.as_f64()?, a.get(1)?.as_f64()?))
                })
                .collect();
            if pts.len() >= 3 {
                out.push(pts);
            }
        }
    }
    out
}

/// 保存本地禁区。
pub fn save_local_blockers(polys: &[Vec<(f64, f64)>]) -> Result<(), Error> {
    save_local_blockers_in(&home_dir(), polys)
}

/// 同上，但 HOME 基目录由调用方指定。
///
/// 首次运行（`~/.pace-forge/` 还不存在）时由 `write_file_private` 建目录。
fn save_local_blockers_in(base: &std::path::Path, polys: &[Vec<(f64, f64)>]) -> Result<(), Error> {
    let path = blockers_path_in(base);
    let body = serde_json::json!({
        "polys": polys
            .iter()
            .map(|p| p.iter().map(|q| vec![q.0, q.1]).collect::<Vec<_>>())
            .collect::<Vec<_>>()
    });
    crate::api::session::write_file_private(&path, body.to_string().as_bytes())
        .map_err(|e| ErrorKind::client(format!("写文件失败: {e}")))
}

fn blockers_path_in(base: &std::path::Path) -> std::path::PathBuf {
    crate::api::session::data_dir_in(base).join("blockers.json")
}

/// 拉围栏内的建筑物轮廓。
///
/// 路网数据不足时的兜底是「把打卡点按顺序用弧线串起来」——纯几何，不看楼。
/// 实测校区里 5 个打卡点分属不同楼群，串出来的弧直接从教学楼、宿舍楼中间
/// 穿过去，地图上一眼就假。拿到轮廓后由 fence_route::avoid_blockers 做避让。
pub fn fetch_buildings(bbox: (f64, f64, f64, f64)) -> Result<Vec<Vec<(f64, f64)>>, Error> {
    if let Some(b) = poly_cache_get(bbox, "buildings") {
        return Ok(b);
    }
    let (min_lat, min_lon, max_lat, max_lon) = bbox;
    let query = format!(
        "[out:json][timeout:30];(way[\"building\"]({min_lat},{min_lon},{max_lat},{max_lon}););out geom;"
    );
    let polys = fetch_polys(&query, "建筑")?;
    poly_cache_put(bbox, "buildings", &polys);
    Ok(polys)
}

/// Overpass 拉 polygon 的公共部分（水体、建筑共用）。
fn fetch_polys(query: &str, what: &str) -> Result<Vec<Vec<(f64, f64)>>, Error> {
    let body = format!("data={}", urlencode(query));
    let text = overpass_post(&body, what)?;
    let v: Value = serde_json::from_str(&text)
        .map_err(|e| ErrorKind::fence(format!("{what} JSON 解析失败: {e}")))?;
    let mut polys = Vec::new();
    if let Some(els) = v.get("elements").and_then(|e| e.as_array()) {
        for el in els {
            let Some(geom) = el.get("geometry").and_then(|g| g.as_array()) else {
                continue;
            };
            let pts: Vec<(f64, f64)> = geom
                .iter()
                .filter_map(|p| Some((p.get("lat")?.as_f64()?, p.get("lon")?.as_f64()?)))
                .collect();
            if pts.len() >= 3 {
                polys.push(pts);
            }
        }
    }
    Ok(polys)
}

fn poly_cache_path_in(
    base: &std::path::Path,
    bbox: (f64, f64, f64, f64),
    kind: &str,
) -> std::path::PathBuf {
    let key = format!("{:.3}_{:.3}_{:.3}_{:.3}", bbox.0, bbox.1, bbox.2, bbox.3);
    crate::api::session::data_dir_in(base).join(format!("{kind}-{key}.json"))
}

fn poly_cache_get(bbox: (f64, f64, f64, f64), kind: &str) -> Option<Vec<Vec<(f64, f64)>>> {
    poly_cache_get_in(&home_dir(), bbox, kind)
}

fn poly_cache_get_in(
    base: &std::path::Path,
    bbox: (f64, f64, f64, f64),
    kind: &str,
) -> Option<Vec<Vec<(f64, f64)>>> {
    let path = poly_cache_path_in(base, bbox, kind);
    let meta = std::fs::metadata(&path).ok()?;
    if let Ok(age) = meta
        .modified()
        .and_then(|t| t.elapsed().map_err(std::io::Error::other))
        && age.as_secs() > 30 * 86400
    {
        return None;
    }
    let text = std::fs::read_to_string(&path).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    let mut out = Vec::new();
    for poly in v.get("polys")?.as_array()? {
        let pts: Vec<(f64, f64)> = poly
            .as_array()?
            .iter()
            .filter_map(|p| {
                let a = p.as_array()?;
                Some((a.first()?.as_f64()?, a.get(1)?.as_f64()?))
            })
            .collect();
        if pts.len() >= 3 {
            out.push(pts);
        }
    }
    Some(out)
}

fn poly_cache_put(bbox: (f64, f64, f64, f64), kind: &str, polys: &[Vec<(f64, f64)>]) {
    poly_cache_put_in(&home_dir(), bbox, kind, polys)
}

fn poly_cache_put_in(
    base: &std::path::Path,
    bbox: (f64, f64, f64, f64),
    kind: &str,
    polys: &[Vec<(f64, f64)>],
) {
    // 空结果不写缓存：上游一次瞬时空响应不能把「这一带无障碍物」锁死 30 天，
    // 否则后续规划会一直缺楼、缺水。
    if polys.is_empty() {
        return;
    }
    let path = poly_cache_path_in(base, bbox, kind);
    let body = serde_json::json!({
        "polys": polys.iter().map(|p| p.iter().map(|q| vec![q.0, q.1]).collect::<Vec<_>>()).collect::<Vec<_>>()
    });
    let _ = crate::api::session::write_file_private(&path, body.to_string().as_bytes());
}

fn water_cache_path_in(base: &std::path::Path, bbox: (f64, f64, f64, f64)) -> std::path::PathBuf {
    let key = format!("{:.3}_{:.3}_{:.3}_{:.3}", bbox.0, bbox.1, bbox.2, bbox.3);
    crate::api::session::data_dir_in(base).join(format!("water-{key}.json"))
}

fn water_cache_get(bbox: (f64, f64, f64, f64)) -> Option<Vec<Vec<(f64, f64)>>> {
    water_cache_get_in(&home_dir(), bbox)
}

fn water_cache_get_in(
    base: &std::path::Path,
    bbox: (f64, f64, f64, f64),
) -> Option<Vec<Vec<(f64, f64)>>> {
    let path = water_cache_path_in(base, bbox);
    let meta = std::fs::metadata(&path).ok()?;
    if let Ok(age) = meta
        .modified()
        .and_then(|t| t.elapsed().map_err(std::io::Error::other))
        && age.as_secs() > 30 * 86400
    {
        return None;
    }
    let text = std::fs::read_to_string(&path).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    let mut out = Vec::new();
    for poly in v.get("polys")?.as_array()? {
        let pts: Vec<(f64, f64)> = poly
            .as_array()?
            .iter()
            .filter_map(|p| {
                let a = p.as_array()?;
                Some((a.first()?.as_f64()?, a.get(1)?.as_f64()?))
            })
            .collect();
        if pts.len() >= 3 {
            out.push(pts);
        }
    }
    Some(out)
}

fn water_cache_put(bbox: (f64, f64, f64, f64), polys: &[Vec<(f64, f64)>]) {
    water_cache_put_in(&home_dir(), bbox, polys)
}

fn water_cache_put_in(
    base: &std::path::Path,
    bbox: (f64, f64, f64, f64),
    polys: &[Vec<(f64, f64)>],
) {
    // 空结果不写缓存，理由同建筑轮廓：瞬时空集不能锁死 30 天。
    if polys.is_empty() {
        return;
    }
    let path = water_cache_path_in(base, bbox);
    let body = serde_json::json!({
        "polys": polys.iter().map(|p| p.iter().map(|q| vec![q.0, q.1]).collect::<Vec<_>>()).collect::<Vec<_>>()
    });
    let _ = crate::api::session::write_file_private(&path, body.to_string().as_bytes());
}

/// 最小 urlencode（只处理查询里会出现的字符）。
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// 一条走出来的路线。
#[derive(Debug, Clone)]
pub struct RoadRoute {
    /// 路线折线（lat, lon）。
    pub points: Vec<(f64, f64)>,
    /// 路线长度（米）。
    pub length_m: f64,
    /// 走过的路段数。
    pub legs: usize,
}

/// 在路网上找一条长度接近 `want_m` 的路线，且经过所有 `must` 点。
///
/// 策略是贪心最近邻串路：从最近的段出发，每次挑「离当前位置最近、且方向
/// 别立刻折返」的一段接上，走够里程就停。不是最优解，但保证全程在道路上，
/// 不会穿水穿楼。
pub fn route_on_roads(
    net: &RoadNet,
    start_hint: (f64, f64),
    must: &[(f64, f64)],
    want_m: f64,
    seed: u64,
) -> Result<RoadRoute, Error> {
    route_on_roads_with(net, start_hint, must, want_m, seed, 250.0)
}

/// 同上，但能指定「相邻两段之间允许多大的断开」。
///
/// OSM 的段是完整 way，端点未必严格相接，250m 的宽容度能多串一些路；
/// 而瓦片路网是像素骨架提出的碎片，只保留真正连通的段——宽容度给大了
/// 就会跨着空地跳，`route_on_roads` 输出过 119m 的瞬移。
#[expect(
    clippy::unwrap_used,
    reason = "non-empty is guaranteed by the guard above"
)]
pub fn route_on_roads_with(
    net: &RoadNet,
    start_hint: (f64, f64),
    must: &[(f64, f64)],
    want_m: f64,
    seed: u64,
    max_hop_m: f64,
) -> Result<RoadRoute, Error> {
    if net.is_empty() {
        return Err(ErrorKind::fence("路网为空".into()));
    }
    if want_m <= 0.0 {
        return Err(ErrorKind::fence("目标里程必须为正".into()));
    }

    // 所有段的索引
    let mut pool: Vec<usize> = (0..net.segments.len()).collect();
    // 用 seed 打散，避免每次都走同一条路
    let mut rng_state = seed | 1;
    let mut next_rand = || {
        rng_state ^= rng_state << 13;
        rng_state ^= rng_state >> 7;
        rng_state ^= rng_state << 17;
        rng_state
    };
    for i in (1..pool.len()).rev() {
        let j = (next_rand() as usize) % (i + 1);
        pool.swap(i, j);
    }

    // 起点不能直接用 start_hint——那是围栏顶点，可能离路网几百米，
    // 后面「跳过去太远」的判断会把它全程挡掉。先吸附到最近的路点。
    let mut cur = {
        let mut best = start_hint;
        let mut bd = f64::MAX;
        for seg in &net.segments {
            for &p in &seg.points {
                let d = distance(start_hint, p);
                if d < bd {
                    bd = d;
                    best = p;
                }
            }
        }
        best
    };
    let mut points: Vec<(f64, f64)> = vec![cur];
    let mut total = 0.0;
    let mut legs = 0usize;
    let mut used: Vec<bool> = vec![false; net.segments.len()];
    let mut pending: Vec<(f64, f64)> = must.to_vec();

    while total < want_m && legs < net.segments.len() * 3 {
        // 优先去还没走到的打卡点
        let target = if let Some(&p) = pending.first() {
            p
        } else {
            cur
        };

        // 挑一段：优先能接近 target 的，否则离 cur 最近的
        let mut best: Option<(usize, f64, bool)> = None; // (idx, score, reversed)
        for &idx in &pool {
            let seg = &net.segments[idx];
            let (a, b) = (seg.points[0], *seg.points.last().unwrap());
            let da = distance(cur, a);
            let db = distance(cur, b);
            let (near, far, rev) = if da <= db {
                (a, b, false)
            } else {
                (b, a, true)
            };

            // 这一段能不能帮我们靠近 target
            let helps = distance(far, target) < distance(near, target);
            let penalty = if used[idx] { 30.0 } else { 0.0 };
            let score = distance(cur, near) + penalty - if helps { 400.0 } else { 0.0 };

            if best.as_ref().map(|b| score < b.1).unwrap_or(true) {
                best = Some((idx, score, rev));
            }
        }

        let Some((idx, _, rev)) = best else { break };
        // 离得太远跳过去会撕出直线，跳过
        let seg = &net.segments[idx];
        let pts: Vec<(f64, f64)> = if rev {
            seg.points.iter().rev().copied().collect()
        } else {
            seg.points.clone()
        };
        if distance(cur, pts[0]) > max_hop_m {
            // 断开太远，标记这段已用，换下一段
            used[idx] = true;
            legs += 1;
            if legs > net.segments.len() * 2 {
                break;
            }
            continue;
        }

        // 接上这一段
        for &p in pts.iter().skip(1) {
            #[expect(
                clippy::unwrap_used,
                reason = "non-empty is guaranteed by the guard above"
            )]
            let d = distance(*points.last().unwrap(), p);
            if total + d > want_m {
                break;
            }
            points.push(p);
            total += d;
        }
        cur = *points.last().unwrap();
        used[idx] = true;
        legs += 1;

        // 打卡点：走过就标记完成
        pending.retain(|&m| !points.iter().any(|p| distance(*p, m) < 30.0));

        if total >= want_m {
            break;
        }
    }

    if points.len() < 2 {
        return Err(ErrorKind::fence("路网上没走出有效路线".into()));
    }

    // 文档承诺「经过所有 must 点」，实现却只在轨迹经过 30m 内时把点从
    // pending 里摘掉，循环结束从不检查是否摘干净 —— 漏打卡照样返回 Ok，
    // 下游 `resample_with_checkpoints` 会把漏掉的 must 当弦钉进路径兜底，
    // 那条弦线可能直接穿楼穿水。这里补上契约校验：还剩点就明确失败，
    // 由调用方退回几何规划（`flow::pick_road_route` 的 Err 分支）。
    if !pending.is_empty() {
        let worst = pending
            .iter()
            .map(|&m| {
                points
                    .iter()
                    .map(|&p| distance(p, m))
                    .fold(f64::INFINITY, f64::min)
            })
            .fold(0.0f64, f64::max);
        return Err(ErrorKind::fence(format!(
            "路网规划漏了 {} 个打卡点（最近点距 {:.0}m，要求 <30m）",
            pending.len(),
            worst
        )));
    }

    Ok(RoadRoute {
        points,
        length_m: total,
        legs,
    })
}

/// 把路线重采样成等间距点。
pub fn resample(path: &[(f64, f64)], step_m: f64) -> Vec<(f64, f64)> {
    if path.len() < 2 || step_m <= 0.0 {
        return path.to_vec();
    }
    let mut out = vec![path[0]];
    let mut carry = 0.0;
    for w in path.windows(2) {
        let d = distance(w[0], w[1]);
        if d < 1e-9 {
            continue;
        }
        let mut pos = step_m - carry;
        while pos <= d {
            let t = pos / d;
            out.push((
                w[0].0 + (w[1].0 - w[0].0) * t,
                w[0].1 + (w[1].1 - w[0].1) * t,
            ));
            pos += step_m;
        }
        carry = d - (pos - step_m);
    }
    out
}

/// bbox 从点集算出来，外扩一点。
pub fn bbox_of(points: &[(f64, f64)], pad_m: f64) -> (f64, f64, f64, f64) {
    if points.is_empty() {
        return (0.0, 0.0, 0.0, 0.0);
    }
    let lat_min = points.iter().map(|p| p.0).fold(f64::MAX, f64::min);
    let lat_max = points.iter().map(|p| p.0).fold(f64::MIN, f64::max);
    let lon_min = points.iter().map(|p| p.1).fold(f64::MAX, f64::min);
    let lon_max = points.iter().map(|p| p.1).fold(f64::MIN, f64::max);
    let mlat = m_per_deg_lat(lat_min).max(1.0);
    let mlng = m_per_deg_lng(lat_min).max(1.0);
    (
        lat_min - pad_m / mlat,
        lon_min - pad_m / mlng,
        lat_max + pad_m / mlat,
        lon_max + pad_m / mlng,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 冷却窗口：失败后 10 分钟内不再打 Overpass，过了窗口重新允许。
    #[test]
    fn overpass_cooldown_window() {
        let t = 1_790_000_000_000u64;
        assert!(!cooling_at(t, 0), "没失败过，正常请求");
        assert!(cooling_at(t, t - 1_000), "刚失败，冷却中");
        assert!(
            cooling_at(t, t - (DOWN_COOLDOWN_MS - 1)),
            "差 1ms 到窗口，仍冷却"
        );
        assert!(
            !cooling_at(t, t - DOWN_COOLDOWN_MS),
            "到点解冻，重新给 Overpass 机会"
        );
    }

    /// 竞速：一个端点先回成功，即使前面有失败也不等其余端点。
    #[test]
    fn race_returns_first_success_without_waiting() {
        let (tx, rx) = std::sync::mpsc::channel();
        tx.send((0, Err(ErrorKind::fence("主站 502".into()))))
            .unwrap();
        tx.send((1, Ok("{\"elements\":[]}".to_string()))).unwrap();
        // 故意不 drop(tx)：还有一个「慢端点」没回，竞速不能等它。
        let t0 = std::time::Instant::now();
        let (ok, errs) = select_first_ok(&rx, std::time::Duration::from_secs(5));
        drop(tx);
        assert!(ok.expect("应取到成功结果").contains("elements"));
        assert_eq!(errs.len(), 1, "失败原因要一并收集: {errs:?}");
        assert!(
            t0.elapsed() < std::time::Duration::from_secs(1),
            "不该等到窗口结束"
        );
    }

    /// 竞速：窗口内没有任何成功，返回空并保留失败原因。
    #[test]
    fn race_collects_errors_when_all_fail() {
        let (tx, rx) = std::sync::mpsc::channel();
        tx.send((0, Err(ErrorKind::fence("a 超时".into()))))
            .unwrap();
        tx.send((1, Err(ErrorKind::fence("b 502".into())))).unwrap();
        tx.send((2, Err(ErrorKind::network("c 内容异常".into()))))
            .unwrap();
        drop(tx);
        let (ok, errs) = select_first_ok(&rx, std::time::Duration::from_secs(1));
        assert!(ok.is_none());
        assert_eq!(errs.len(), 3);
        assert_eq!(errs.last().unwrap(), "网络错误：c 内容异常");
    }

    /// 竞速：窗口到期一个结果都没有，要按时退出，不能死等。
    #[test]
    fn race_times_out_at_window() {
        let (_tx, rx) = std::sync::mpsc::channel::<(usize, Result<String, Error>)>();
        let t0 = std::time::Instant::now();
        let (ok, errs) = select_first_ok(&rx, std::time::Duration::from_millis(120));
        assert!(ok.is_none() && errs.is_empty());
        assert!(t0.elapsed() >= std::time::Duration::from_millis(110));
        assert!(
            t0.elapsed() < std::time::Duration::from_secs(1),
            "窗口就是上限"
        );
    }

    fn l_shape_net() -> RoadNet {
        // 一个直角路口：东西向 + 南北向各一段
        let lat0 = 31.2304;
        let lon0 = 101.4737;
        let mut segs = vec![RoadSegment {
            points: (0..=20)
                .map(|i| (lat0, lon0 + i as f64 * 10.0 / m_per_deg_lng(lat0)))
                .collect(),
            kind: "footway".into(),
        }];
        segs.push(RoadSegment {
            points: (0..=20)
                .map(|i| {
                    (
                        lat0 + i as f64 * 10.0 / m_per_deg_lat(lat0),
                        lon0 + 200.0 / m_per_deg_lng(lat0),
                    )
                })
                .collect(),
            kind: "path".into(),
        });
        RoadNet {
            segments: segs,
            source: "test".into(),
            mask: None,
        }
    }

    /// 必经点在够不着的另一段路上时，必须返回 Err，不能给一条「漏打卡」的 Ok。
    ///
    /// 旧实现只在 30m 内把点从 pending 摘掉，循环结束不校验 pending 是否空：
    /// 两段相距 500m、must 在第二段上（max_hop 250m 跳不过去）时照样 Ok。
    #[test]
    fn must_point_unreachable_returns_err() {
        let lat0 = 31.2304;
        let lon0 = 101.4737;
        let mlng = m_per_deg_lng(lat0);
        let mlat = m_per_deg_lat(lat0);
        // 第一段：东西向 100m，起点区域
        let a: Vec<(f64, f64)> = (0..=10)
            .map(|i| (lat0, lon0 + i as f64 * 10.0 / mlng))
            .collect();
        // 第二段：正北 500m 之外，同样 100m 长，与第一段完全不连通
        let b: Vec<(f64, f64)> = (0..=10)
            .map(|i| (lat0 + 500.0 / mlat, lon0 + i as f64 * 10.0 / mlng))
            .collect();
        let net = RoadNet {
            segments: vec![
                RoadSegment {
                    points: a,
                    kind: "footway".into(),
                },
                RoadSegment {
                    points: b,
                    kind: "footway".into(),
                },
            ],
            source: "test".into(),
            mask: None,
        };
        // 必经点放在第二段中间，离第一段 ~500m
        let must = vec![(lat0 + 500.0 / mlat, lon0 + 50.0 / mlng)];
        let err = route_on_roads(&net, (lat0, lon0), &must, 120.0, 7)
            .expect_err("漏打卡必须报错，不能静默 Ok");
        assert!(
            err.to_string().contains("漏了") && err.to_string().contains("打卡点"),
            "错误信息要说清是漏点，实际: {err}"
        );
    }

    #[test]
    fn bbox_pads_outward() {
        let pts = vec![(31.23, 121.47), (31.24, 121.48)];
        let (a, b, c, d) = bbox_of(&pts, 50.0);
        assert!(a < 31.23 && b < 121.47 && c > 31.24 && d > 121.48);
    }

    #[test]
    fn route_stays_on_segments() {
        let net = l_shape_net();
        let r = route_on_roads(&net, (31.2304, 101.4737), &[], 300.0, 7).unwrap();
        assert!(r.points.len() > 5);
        // 每个点都必须落在某一段路上（端点之间线性插值也在路上）
        for p in &r.points {
            let on = net.segments.iter().any(|s| {
                s.points
                    .windows(2)
                    .any(|w| point_on_segment(*p, w[0], w[1], 3.0))
            });
            assert!(on, "点 {p:?} 不在任何道路上");
        }
    }

    #[test]
    fn route_length_is_bounded() {
        let net = l_shape_net();
        for want in [100.0, 200.0, 400.0] {
            let r = route_on_roads(&net, (31.2304, 101.4737), &[], want, 3).unwrap();
            assert!(
                r.length_m <= want * 1.1,
                "走了 {:.0}m 超过目标 {want}m",
                r.length_m
            );
        }
    }

    #[test]
    fn empty_net_is_rejected() {
        let net = RoadNet::default();
        assert!(route_on_roads(&net, (31.23, 121.47), &[], 100.0, 1).is_err());
    }

    /// 点到线段的距离在容差内。
    fn point_on_segment(p: (f64, f64), a: (f64, f64), b: (f64, f64), tol_m: f64) -> bool {
        let mlat = m_per_deg_lat(p.0).max(1.0);
        let mlng = m_per_deg_lng(p.0).max(1.0);
        let (py, px) = (0.0, 0.0);
        let (ay, ax) = ((a.0 - p.0) * mlat, (a.1 - p.1) * mlng);
        let (by, bx) = ((b.0 - p.0) * mlat, (b.1 - p.1) * mlng);
        let (vy, vx) = (by - ay, bx - ax);
        let len2 = vy * vy + vx * vx;
        let t = if len2 < 1e-12 {
            0.0
        } else {
            (-(ay * vy + ax * vx) / len2).clamp(0.0, 1.0)
        };
        let (cy, cx) = (ay + vy * t, ax + vx * t);
        ((cy - py).powi(2) + (cx - px).powi(2)).sqrt() < tol_m
    }

    // ------------------------------------------------------------- 缓存隔离

    use crate::test_support::{TestDir, assert_isolated};

    const BBOX: (f64, f64, f64, f64) = (31.230, 101.473, 31.240, 101.484);

    /// 路网缓存的读写必须全部落在**注入目录**里，并在首次写时自动建目录。
    #[test]
    fn roadnet_cache_roundtrip_in_isolated_dir() {
        let t = TestDir::new("roadnet-cache");
        let data = t.data();
        assert!(!data.exists(), "注入目录下不该预先存在 .pace-forge");

        // 首次运行：目录不存在也要能写进去
        cache_put_in(t.home(), BBOX, &l_shape_net());
        assert!(data.exists(), "首次写缓存应自动创建数据目录");

        let path = cache_path_in(t.home(), BBOX);
        assert!(path.exists(), "缓存文件应存在: {}", path.display());
        assert!(path.starts_with(t.home()), "缓存必须写在注入基目录下");

        let back = cache_get_in(t.home(), BBOX).expect("应能读回缓存");
        assert_eq!(back.segments.len(), 2);
        assert_eq!(back.segments[0].kind, "footway");
        // 命中缓存时 source 里有「本地缓存」字样
        assert!(back.source.contains("本地缓存"), "source={}", back.source);

        // 没写过的 bbox 读不到
        assert!(cache_get_in(t.home(), (10.0, 20.0, 10.1, 20.1)).is_none());
    }

    /// 手绘禁区（blockers.json）走同一套隔离与建目录逻辑。
    #[test]
    fn blockers_roundtrip_in_isolated_dir() {
        let t = TestDir::new("roadnet-blockers");
        assert!(!t.data().exists());

        let polys = vec![
            vec![(31.23, 121.47), (31.23, 121.48), (31.24, 121.48)],
            vec![(31.25, 121.49), (31.25, 121.50), (31.26, 121.50)],
        ];
        save_local_blockers_in(t.home(), &polys).unwrap();
        assert!(t.data().exists(), "首次写禁区应自动创建数据目录");
        assert!(blockers_path_in(t.home()).starts_with(t.home()));

        let back = load_local_blockers_in(t.home());
        assert_eq!(back.len(), 2);
        assert!((back[0][0].0 - 31.23).abs() < 1e-9);

        // 从未写过 → 空列表而不是 panic
        let t2 = TestDir::new("roadnet-blockers-empty");
        assert!(load_local_blockers_in(t2.home()).is_empty());
        assert!(!t2.data().exists(), "读路径不该创建数据目录");
    }

    /// 空的水域/建筑结果不能写缓存：上游一次瞬时空响应不该把「这一带没有
    /// 障碍物」锁死 30 天，否则后续规划持续缺楼缺水。
    #[test]
    fn empty_polys_are_not_cached() {
        let t = TestDir::new("roadnet-empty-cache");
        poly_cache_put_in(t.home(), BBOX, "buildings", &[]);
        assert!(
            poly_cache_get_in(t.home(), BBOX, "buildings").is_none(),
            "空建筑结果不该命中缓存"
        );
        water_cache_put_in(t.home(), BBOX, &[]);
        assert!(
            water_cache_get_in(t.home(), BBOX).is_none(),
            "空水域结果不该命中缓存"
        );
    }

    /// 水体 / 建筑轮廓缓存同样要能注入。
    #[test]
    fn poly_and_water_cache_roundtrip_in_isolated_dir() {
        let t = TestDir::new("roadnet-poly");
        let polys = vec![vec![(31.23, 121.47), (31.23, 121.48), (31.24, 121.48)]];

        poly_cache_put_in(t.home(), BBOX, "buildings", &polys);
        let got = poly_cache_get_in(t.home(), BBOX, "buildings").expect("建筑缓存应命中");
        assert_eq!(got.len(), 1);
        assert!(poly_cache_path_in(t.home(), BBOX, "buildings").starts_with(t.data()));

        water_cache_put_in(t.home(), BBOX, &polys);
        let got = water_cache_get_in(t.home(), BBOX).expect("水体缓存应命中");
        assert_eq!(got.len(), 1);
        assert!(water_cache_path_in(t.home(), BBOX).starts_with(t.data()));
    }

    /// 护栏：四类缓存路径的默认 base 都**绝不能**落到真实 `$HOME/.pace-forge`。
    ///
    /// 模式与 `session::tests::test_data_dir_is_isolated_from_real_home` 一致。
    /// 谁哪天把 `cache_put_in` / `save_local_blockers_in` 改回 `cache_put` /
    /// `save_local_blockers`，注入路径就会等于生产路径，这条立刻炸。
    #[test]
    fn roadnet_cache_paths_are_isolated_from_real_home() {
        let t = TestDir::new("roadnet-isolated");
        let home = t.home();
        let real = crate::test_support::real_home();

        for (label, injected, production) in [
            (
                "roads 缓存",
                cache_path_in(home, BBOX),
                cache_path_in(&real, BBOX),
            ),
            ("blockers", blockers_path_in(home), blockers_path_in(&real)),
            (
                "buildings 缓存",
                poly_cache_path_in(home, BBOX, "buildings"),
                poly_cache_path_in(&real, BBOX, "buildings"),
            ),
            (
                "water 缓存",
                water_cache_path_in(home, BBOX),
                water_cache_path_in(&real, BBOX),
            ),
        ] {
            assert_isolated(injected.parent().unwrap(), label);
            assert_ne!(injected, production, "{label}: 注入路径不能等于生产路径");
            assert!(
                !injected.starts_with(crate::test_support::real_data_dir()),
                "{label}: 不能落在真实 $HOME/.pace-forge 之下: {}",
                injected.display()
            );
        }
    }
}

//! 瓦片路网：拿高德瓦片截图当路网（OSM 没有数据时的第二级来源）。
//!
//! 国内校园在 OSM 上基本是空白（实测某校整个 bbox 只有 15 个 way、76 个
//! node，一栋楼都没有），Overpass 拉不到就只能退回几何规划，轨迹是连点
//! 折线，一眼假。但这类学校在地图 App 上是有路网的——本项目的服务端用的
//! 就是高德底图，同一套瓦片直接可见。
//!
//! 流程：瓦片 → 掩膜 → 形态学弥合 → 骨架化 → 折线追踪 → [`RoadNet`]。
//! 产出与 OSM 那条路同构，直接喂给 [`roadnet::route_on_roads`]，
//! 下游不用区分数据来自哪里。
//!
//! ## 为什么阈值是「几乎纯白」
//!
//! 高德路网图（`style=8`）是浅灰底 (#f0f0f0) 配纯白路面 (#ffffff)，两者
//! 只差 15 级。按「深底亮路」的常规思路取 `>=243` 会捞到 65% 的背景。
//! 实测 `min(R,G,B) >= 248 && max-min <= 6` 正好只命中道路：白色小路、
//! 车道虚线、楼间道路全都进来，背景/水面/绿地/建筑一个不漏地排除。
//!
//! 顺带一提，开运算在这一步不能用：路面线宽只有 2~3 px，半径 2 的圆盘
//! 结构元会把它整个吃掉（实测掩膜从 21 万 px 掉到 1.4 万 px）。
//!
//! ## 坐标系
//!
//! 高德瓦片是 GCJ-02 网格，反算出来天然是 GCJ-02。但 `RoadNet` 的消费方
//! （围栏、打卡点）用的是 BD-09，所以出口处统一转一次。

use std::collections::HashSet;
use std::path::PathBuf;

use crate::error::{Error, ErrorKind};
use crate::track::roadnet::{RoadNet, RoadSegment};

/// 瓦片服务。四个子域轮着用，避开单域限流。
const TILE_HOSTS: [&str; 4] = [
    "https://webrd01.is.autonavi.com/appmaptile?lang=zh_cn&size=1&scale=1&style=8",
    "https://webrd02.is.autonavi.com/appmaptile?lang=zh_cn&size=1&scale=1&style=8",
    "https://webrd03.is.autonavi.com/appmaptile?lang=zh_cn&size=1&scale=1&style=8",
    "https://webrd04.is.autonavi.com/appmaptile?lang=zh_cn&size=1&scale=1&style=8",
];

/// 缩放级别。z=17 在纬度 47° 附近约 1.19 m/px，够看清楼间小路；
/// 再高一档瓦片数量翻四倍，覆盖一个校区要上百张，拉取时间不可接受。
const ZOOM: u8 = 17;

/// 单张瓦片的端点超时。原来 15 秒 × 4 个域名，一个校区十几张瓦片，
/// 网络不顺时能卡十几分钟。高德瓦片正常几十毫秒就回，6 秒足够。
const TILE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(6);

/// 四个域名全失败后的冷却：这段时间内不再拉瓦片，直接走几何规划。
const TILE_DOWN_COOLDOWN_MS: u64 = 600_000;

static TILE_DOWN_AT_MS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn tile_now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn tile_cooling_at(now_ms: u64, down_at_ms: u64) -> bool {
    down_at_ms != 0 && now_ms.saturating_sub(down_at_ms) < TILE_DOWN_COOLDOWN_MS
}

/// 单次最多拉多少张瓦片。12×12 = 144 张约覆盖 3.6 km 见方，
/// 超过这个范围说明 bbox 不合理（或者调用方传错了坐标系）。
const MAX_TILES: usize = 200;

/// 像素 → 经纬度需要的瓦片原点。
#[derive(Debug, Clone, Copy)]
struct GeoRef {
    /// 拼接图左上角所在瓦片的整数索引。
    tx0: i32,
    ty0: i32,
}

/// 一张拼好的道路掩膜。
#[derive(Debug, Clone)]
pub struct Mask {
    w: usize,
    h: usize,
    bits: Vec<bool>,
    geo: GeoRef,
}

impl Mask {
    /// 像素中心 → GCJ-02。
    /// BD-09 坐标反查像素（用于「这条直线是不是贴着路面」）。
    fn bd_to_pixel(&self, lat: f64, lng: f64) -> Option<(f64, f64)> {
        let (glat, glng) = crate::api::coord::bd09_to_gcj02(lat, lng);
        // GCJ 经纬度 → 瓦片浮点索引 → 拼接图内像素（pixel_to_gcj 的逆）
        let n = (1u32 << ZOOM) as f64;
        let tx = (glng + 180.0) / 360.0 * n;
        let rad = glat.to_radians();
        let ty = (1.0 - (rad.tan() + 1.0 / rad.cos()).ln() / std::f64::consts::PI) / 2.0 * n;
        let x = (tx - self.geo.tx0 as f64) * 256.0 - 0.5;
        let y = (ty - self.geo.ty0 as f64) * 256.0 - 0.5;
        if !x.is_finite() || !y.is_finite() {
            return None;
        }
        Some((x, y))
    }

    /// 这个 BD-09 点周围 `r` 像素内有没有路面。
    fn near_road(&self, lat: f64, lng: f64, r: i32) -> bool {
        let Some((fx, fy)) = self.bd_to_pixel(lat, lng) else {
            return false;
        };
        let (cx, cy) = (fx.round() as i32, fy.round() as i32);
        for dy in -r..=r {
            for dx in -r..=r {
                let x = cx + dx;
                let y = cy + dy;
                if x < 0 || y < 0 || x as usize >= self.w || y as usize >= self.h {
                    continue;
                }
                if self.bits[y as usize * self.w + x as usize] {
                    return true;
                }
            }
        }
        false
    }

    /// 两点之间的直线是不是一路都贴着路面。
    ///
    /// 导致「穿墙」的正是段间连边：两条路各自贴着楼的两侧，节点相距 12m
    /// 内就被连起来，A* 于是从楼中间直插过去。这里沿直线采 9 个点，每个
    /// 点要求 4px 内有路面，允许 1/3 的采样点落空（路口/虚线会缺）。
    fn line_follows_road(&self, a: (f64, f64), b: (f64, f64)) -> bool {
        const N: i32 = 8;
        let mut bad = 0;
        for k in 0..=N {
            let t = k as f64 / N as f64;
            let p = (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t);
            if !self.near_road(p.0, p.1, 4) {
                bad += 1;
            }
        }
        bad * 3 <= N
    }

    fn pixel_to_gcj(&self, x: f64, y: f64) -> (f64, f64) {
        let n = (1u32 << ZOOM) as f64;
        let tx = self.geo.tx0 as f64 + (x + 0.5) / 256.0;
        let ty = self.geo.ty0 as f64 + (y + 0.5) / 256.0;
        let lng = tx / n * 360.0 - 180.0;
        let lat = (std::f64::consts::PI * (1.0 - 2.0 * ty / n))
            .sinh()
            .atan()
            .to_degrees();
        (lat, lng)
    }
}

// ------------------------------------------------------------------ 拉瓦片

/// 瓦片缓存目录：`<base>/.pace-forge/tiles`（纯路径计算，不碰磁盘）。
///
/// 测试把 `base` 注入成一次性临时目录，从而完全不碰真实
/// `$HOME/.pace-forge/tiles`。
fn cache_dir_in(base: &std::path::Path) -> PathBuf {
    crate::api::session::data_dir_in(base).join("tiles")
}

fn tile_path_in(base: &std::path::Path, x: i32, y: i32) -> PathBuf {
    cache_dir_in(base).join(format!("{ZOOM}_{x}_{y}.png"))
}

/// 拉一张瓦片，失败换子域重试。
///
/// 缓存**不设过期**：底图变化极慢，所以只看文件在不在、长度够不够；空文件
/// 或过短的文件会被删掉重拉；超过 512B 但解码失败的坏文件同样删除，避免
/// 坏字节永久留在缓存里毒化该格网。若要按天过期，需要在读到可用文件后再
/// 查 mtime——目前没有做。
fn fetch_tile(x: i32, y: i32) -> Result<image::RgbImage, Error> {
    fetch_tile_in(&crate::api::session::home_dir(), x, y)
}

fn fetch_tile_in(base: &std::path::Path, x: i32, y: i32) -> Result<image::RgbImage, Error> {
    let path = tile_path_in(base, x, y);
    let bytes = if let Ok(b) = std::fs::read(&path) {
        // 空文件或过短文件当没有
        if b.len() > 512 {
            b
        } else {
            std::fs::remove_file(&path).ok();
            Vec::new()
        }
    } else {
        Vec::new()
    };

    let bytes = if bytes.is_empty() {
        if tile_cooling_at(
            tile_now_ms(),
            TILE_DOWN_AT_MS.load(std::sync::atomic::Ordering::Relaxed),
        ) {
            return Err(ErrorKind::network("高德瓦片刚全失败过，冷却中".into()));
        }
        let mut last = String::new();
        let mut got: Option<Vec<u8>> = None;
        for host in TILE_HOSTS {
            let url = format!("{host}&x={x}&y={y}&z={ZOOM}");
            let resp = ureq::get(&url)
                .set("User-Agent", "Mozilla/5.0 (compatible; pace-forge/0.1)")
                .set("Referer", "https://www.amap.com/")
                .timeout(TILE_TIMEOUT)
                .call();
            match resp {
                Ok(r) => match read_all(r.into_reader()) {
                    // 空白格也是合法结果：高德在没有道路渲染的区域会回
                    // ~179B 的纯色 PNG。原来用「>512 字节」判失败，于是
                    // 西部/新校区整片判失败、还进 10 分钟冷却（#2 实测）。
                    // 改成「能解码成图就收」——图里没路，后面的识别自然得出空集。
                    Ok(b) if !b.is_empty() && image::load_from_memory(&b).is_ok() => {
                        got = Some(b);
                        break;
                    }
                    Ok(b) if b.is_empty() => last = format!("{url} 返回空响应"),
                    Ok(_) => last = format!("{url} 返回的不是可解码图片"),
                    Err(e) => last = format!("{url} 读取失败: {e}"),
                },
                Err(e) => last = format!("{url} 请求失败: {e}"),
            }
        }
        let b = match got {
            Some(b) => b,
            None => {
                // 四个域名全挂：记冷却，后面的瓦片直接快速失败，不再逐张等。
                TILE_DOWN_AT_MS.store(tile_now_ms(), std::sync::atomic::Ordering::Relaxed);
                return Err(ErrorKind::network(format!(
                    "瓦片 {x}/{y} 拉取失败（{last}）"
                )));
            }
        };
        // 目录不存在时由 write_file_private 建出来（首次运行）
        let _ = crate::api::session::write_file_private(&path, &b);
        b
    } else {
        bytes
    };

    match image::load_from_memory(&bytes) {
        Ok(img) => Ok(img.to_rgb8()),
        Err(e) => {
            // 解码失败说明缓存里是坏字节：删掉文件，下次请求重新拉，
            // 不能让坏文件一直留在缓存里把这一格网毒化。
            let _ = std::fs::remove_file(&path);
            Err(ErrorKind::network(format!("瓦片 {x}/{y} 解码失败: {e}")))
        }
    }
}

fn read_all(mut r: impl std::io::Read) -> std::io::Result<Vec<u8>> {
    let mut v = Vec::new();
    r.read_to_end(&mut v)?;
    Ok(v)
}

/// 经纬度 → 瓦片坐标（Web Mercator，浮点）。
fn lnglat_to_tile(lat: f64, lng: f64) -> (f64, f64) {
    let n = (1u32 << ZOOM) as f64;
    let x = (lng + 180.0) / 360.0 * n;
    let r = lat.to_radians();
    let y = (1.0 - (r.tan() + 1.0 / r.cos()).ln() / std::f64::consts::PI) / 2.0 * n;
    (x, y)
}

// ------------------------------------------------------------------ 掩膜

/// 这个像素是不是路面。
///
/// 高德路网图的路面是近乎纯白：三通道都 >= 248，且三通道差 <= 6（纯白无
/// 色偏）。虚线、白线、楼间小路都满足；浅灰底 (240) 差一点，正好卡掉。
#[inline]
fn is_road(px: &[u8]) -> bool {
    let mn = px[0].min(px[1]).min(px[2]);
    let mx = px[0].max(px[1]).max(px[2]);
    mn >= 248 && (mx - mn) <= 6
}

/// 沿一个轴做膨胀。
fn dilate_axis(src: &[bool], w: usize, h: usize, radius: usize, horiz: bool) -> Vec<bool> {
    let mut out = vec![false; w * h];
    for y in 0..h {
        for x in 0..w {
            let mut on = false;
            for d in -(radius as isize)..=(radius as isize) {
                let (nx, ny) = if horiz {
                    (x as isize + d, y as isize)
                } else {
                    (x as isize, y as isize + d)
                };
                if nx >= 0
                    && ny >= 0
                    && (nx as usize) < w
                    && (ny as usize) < h
                    && src[ny as usize * w + nx as usize]
                {
                    on = true;
                    break;
                }
            }
            out[y * w + x] = on;
        }
    }
    out
}

/// 沿一个轴做腐蚀。边界外一律按 false 处理。
fn erode_axis(src: &[bool], w: usize, h: usize, radius: usize, horiz: bool) -> Vec<bool> {
    let mut out = vec![false; w * h];
    for y in 0..h {
        for x in 0..w {
            let mut all = true;
            for d in -(radius as isize)..=(radius as isize) {
                let (nx, ny) = if horiz {
                    (x as isize + d, y as isize)
                } else {
                    (x as isize, y as isize + d)
                };
                // 越界按边界像素处理（replicate）。当成 false 的话，
                // 图像边缘一圈会被整条腐蚀掉——只有几行高的掩膜直接消失。
                let cx = nx.clamp(0, w as isize - 1) as usize;
                let cy = ny.clamp(0, h as isize - 1) as usize;
                if !src[cy * w + cx] {
                    all = false;
                    break;
                }
            }
            out[y * w + x] = all;
        }
    }
    out
}

/// 闭运算：先膨胀再腐蚀，把虚线缝和图层之间的断口接起来。
///
/// 用可分离的方形结构元（先横后纵），代价 O(w·h·r)，比真圆盘快一个量级，
/// 在这一步够用——形状误差后面由骨架化纠正。
///
/// 只做小半径（2 px）。路面线宽 2~3 px，半径再大就会把相邻不该连的路
/// 也粘到一起，凭空造出可以穿墙的路口。
fn close_gaps(m: &mut Mask, radius: usize) {
    if radius == 0 {
        return;
    }
    let (w, h) = (m.w, m.h);
    let d = dilate_axis(&m.bits, w, h, radius, true);
    let d = dilate_axis(&d, w, h, radius, false);
    let e = erode_axis(&d, w, h, radius, true);
    let e = erode_axis(&e, w, h, radius, false);
    m.bits = e;
}

/// Zhang-Suen 细化：把有宽度的路面削成 1 px 宽的中心线。
///
/// 不细化直接转图的话，同一条 6 px 宽的路会变成 3 条平行的边，寻路会
/// 在路的左右两侧反复横跳。
fn thin(m: &Mask) -> Vec<bool> {
    let (w, h) = (m.w, m.h);
    let mut bits = m.bits.clone();
    if w < 3 || h < 3 {
        return bits;
    }
    // 8 邻域顺时针：p2 p3 p4 p5 p6 p7 p8 p9
    let idx = |x: usize, y: usize| y * w + x;
    loop {
        let mut changed = false;
        for step in 0..2 {
            let mut del: Vec<usize> = Vec::new();
            for y in 1..h - 1 {
                for x in 1..w - 1 {
                    if !bits[idx(x, y)] {
                        continue;
                    }
                    let p2 = bits[idx(x, y - 1)];
                    let p3 = bits[idx(x + 1, y - 1)];
                    let p4 = bits[idx(x + 1, y)];
                    let p5 = bits[idx(x + 1, y + 1)];
                    let p6 = bits[idx(x, y + 1)];
                    let p7 = bits[idx(x - 1, y + 1)];
                    let p8 = bits[idx(x - 1, y)];
                    let p9 = bits[idx(x - 1, y - 1)];
                    let ring = [p2, p3, p4, p5, p6, p7, p8, p9];
                    let b = ring.iter().filter(|v| **v).count();
                    if !(2..=6).contains(&b) {
                        continue;
                    }
                    let mut a = 0;
                    for i in 0..8 {
                        if !ring[i] && ring[(i + 1) % 8] {
                            a += 1;
                        }
                    }
                    if a != 1 {
                        continue;
                    }
                    let c1 = if step == 0 {
                        !(p2 && p4 && p6)
                    } else {
                        !(p2 && p4 && p8)
                    };
                    let c2 = if step == 0 {
                        !(p4 && p6 && p8)
                    } else {
                        !(p2 && p6 && p8)
                    };
                    if c1 && c2 {
                        del.push(idx(x, y));
                    }
                }
            }
            if del.is_empty() {
                continue;
            }
            for i in del {
                bits[i] = false;
            }
            changed = true;
        }
        if !changed {
            break;
        }
    }
    bits
}

// ------------------------------------------------------------------ 折线追踪

const NEIGHBORS: [(isize, isize); 8] = [
    (-1, -1),
    (0, -1),
    (1, -1),
    (-1, 0),
    (1, 0),
    (-1, 1),
    (0, 1),
    (1, 1),
];

/// 从 1px 骨架里拆出折线段。
///
/// 分支点（度 >= 3）和端点（度 == 1）作为分界，沿普通点（度 == 2）走到底。
/// 剩下的纯环（一圈全是度 2）单独兜底成闭合段，否则绕圈的路会整条丢掉。
fn trace_lines(bits: &[bool], w: usize, h: usize) -> Vec<Vec<(usize, usize)>> {
    let deg = |x: usize, y: usize| -> usize {
        let mut d = 0;
        for (dx, dy) in NEIGHBORS {
            let nx = x as isize + dx;
            let ny = y as isize + dy;
            if nx >= 0
                && ny >= 0
                && (nx as usize) < w
                && (ny as usize) < h
                && bits[ny as usize * w + nx as usize]
            {
                d += 1;
            }
        }
        d
    };

    let mut lines: Vec<Vec<(usize, usize)>> = Vec::new();
    let mut used_edge: std::collections::HashSet<(usize, usize)> = std::collections::HashSet::new();
    let edge_key = |a: usize, b: usize| if a < b { (a, b) } else { (b, a) };

    let mut starts: Vec<(usize, usize)> = Vec::new();
    for y in 0..h {
        for x in 0..w {
            if !bits[y * w + x] {
                continue;
            }
            let d = deg(x, y);
            if d != 2 {
                starts.push((x, y));
            }
        }
    }

    for &(sx, sy) in &starts {
        for (dx, dy) in NEIGHBORS {
            let nx = sx as isize + dx;
            let ny = sy as isize + dy;
            if nx < 0 || ny < 0 || (nx as usize) >= w || (ny as usize) >= h {
                continue;
            }
            let (nx, ny) = (nx as usize, ny as usize);
            if !bits[ny * w + nx] {
                continue;
            }
            let k = edge_key(sy * w + sx, ny * w + nx);
            if used_edge.contains(&k) {
                continue;
            }
            // 沿这条边走出去
            let mut path = vec![(sx, sy), (nx, ny)];
            used_edge.insert(k);
            let mut prev = (sx, sy);
            let mut cur = (nx, ny);
            loop {
                if deg(cur.0, cur.1) != 2 {
                    break;
                }
                let mut nxt: Option<(usize, usize)> = None;
                for (ddx, ddy) in NEIGHBORS {
                    let ax = cur.0 as isize + ddx;
                    let ay = cur.1 as isize + ddy;
                    if ax < 0 || ay < 0 || (ax as usize) >= w || (ay as usize) >= h {
                        continue;
                    }
                    let cand = (ax as usize, ay as usize);
                    if cand == prev || !bits[cand.1 * w + cand.0] {
                        continue;
                    }
                    let kk = edge_key(cur.1 * w + cur.0, cand.1 * w + cand.0);
                    if used_edge.contains(&kk) {
                        continue;
                    }
                    nxt = Some(cand);
                    break;
                }
                match nxt {
                    Some(n) => {
                        used_edge.insert(edge_key(cur.1 * w + cur.0, n.1 * w + n.0));
                        prev = cur;
                        cur = n;
                        path.push(cur);
                    }
                    None => break,
                }
            }
            // 太短的段是细化留下的毛刺，保留下来会在轨迹里制造尖刺。
            // 阈值不能太大：8 px（约 11m）会把路口之间的小连接段一起删掉，
            // 图就直接碎成孤岛（实测游走只踩到 67 个节点）。
            if path.len() >= 5 {
                lines.push(path);
            }
        }
    }

    // 剩下没被访问的纯环
    let mut seen = vec![false; w * h];
    for line in &lines {
        for &(x, y) in line {
            seen[y * w + x] = true;
        }
    }
    for y in 1..h - 1 {
        for x in 1..w - 1 {
            let i = y * w + x;
            if !bits[i] || seen[i] {
                continue;
            }
            // 顺着环走一圈
            let mut ring = vec![(x, y)];
            seen[i] = true;
            let mut prev = (x, y);
            let mut cur = (x, y);
            loop {
                let mut nxt: Option<(usize, usize)> = None;
                for (dx, dy) in NEIGHBORS {
                    let nx = cur.0 as isize + dx;
                    let ny = cur.1 as isize + dy;
                    if nx < 0 || ny < 0 || (nx as usize) >= w || (ny as usize) >= h {
                        continue;
                    }
                    let c = (nx as usize, ny as usize);
                    if c == prev || !bits[c.1 * w + c.0] {
                        continue;
                    }
                    if c == ring[0] {
                        continue;
                    }
                    nxt = Some(c);
                    break;
                }
                match nxt {
                    Some(n) => {
                        if seen[n.1 * w + n.0] {
                            break;
                        }
                        seen[n.1 * w + n.0] = true;
                        ring.push(n);
                        prev = cur;
                        cur = n;
                    }
                    None => break,
                }
            }
            if ring.len() >= 8 {
                ring.push(ring[0]);
                lines.push(ring);
            }
        }
    }

    lines
}

// ------------------------------------------------------------------ 对外接口

/// 拉一片区域的瓦片路网，输出 BD-09 线段（与 OSM 那条路同构）。
///
/// `bbox` 是 `(min_lat, min_lon, max_lat, max_lon)`，**BD-09**——调用方
/// 传的是围栏范围，跟服务端数据同一套坐标系。
pub fn fetch_roadnet(bbox: (f64, f64, f64, f64)) -> Result<RoadNet, Error> {
    let (min_lat, min_lon, max_lat, max_lon) = bbox;
    if !(min_lat < max_lat && min_lon < max_lon) {
        return Err(ErrorKind::fence("bbox 非法".into()));
    }

    // BD-09 → GCJ-02：瓦片是高德的网格，必须用 GCJ-02 去查。
    let (g_min_lat, g_min_lon) = crate::api::coord::bd09_to_gcj02(min_lat, min_lon);
    let (g_max_lat, g_max_lon) = crate::api::coord::bd09_to_gcj02(max_lat, max_lon);

    let (x1, y1) = lnglat_to_tile(g_max_lat, g_min_lon); // 北西
    let (x2, y2) = lnglat_to_tile(g_min_lat, g_max_lon); // 南东
    let tx0 = x1.floor() as i32;
    let ty0 = y1.floor() as i32;
    let tx1 = x2.floor() as i32;
    let ty1 = y2.floor() as i32;

    let nx = (tx1 - tx0 + 1).max(1) as usize;
    let ny = (ty1 - ty0 + 1).max(1) as usize;
    if nx * ny > MAX_TILES {
        return Err(ErrorKind::fence(format!(
            "范围过大：{nx}×{ny} = {} 张瓦片，上限 {MAX_TILES}",
            nx * ny
        )));
    }

    let w = nx * 256;
    let h = ny * 256;
    let mut bits = vec![false; w * h];
    let mut failed = 0usize;

    for (i, tx) in (tx0..=tx1).enumerate() {
        for (j, ty) in (ty0..=ty1).enumerate() {
            match fetch_tile(tx, ty) {
                Ok(img) => {
                    let (ox, oy) = (i * 256, j * 256);
                    for py in 0..256usize {
                        for px in 0..256usize {
                            if is_road(&img.get_pixel(px as u32, py as u32).0) {
                                bits[(oy + py) * w + ox + px] = true;
                            }
                        }
                    }
                }
                Err(_) => failed += 1,
            }
        }
    }

    if failed == nx * ny {
        return Err(ErrorKind::network(format!("{nx}×{ny} 张瓦片全部拉取失败")));
    }
    let raw = bits.iter().filter(|v| **v).count();
    if raw < 200 {
        return Err(ErrorKind::fence(format!(
            "掩膜只有 {raw} px，这一带瓦片上没有道路"
        )));
    }

    let mut mask = Mask {
        w,
        h,
        bits,
        geo: GeoRef { tx0, ty0 },
    };
    close_gaps(&mut mask, 2);
    let skel = thin(&mask);
    let lines = trace_lines(&skel, w, h);

    // 像素折线 → 经纬度线段
    let mut segments: Vec<RoadSegment> = Vec::new();
    for line in lines {
        // 抽稀：每隔几步取一个点，再压掉共线的中间点
        let step = 4usize;
        let mut pts: Vec<(f64, f64)> = Vec::new();
        for (k, &(x, y)) in line.iter().enumerate() {
            if k % step != 0 && k != line.len() - 1 {
                continue;
            }
            let (glat, glng) = mask.pixel_to_gcj(x as f64, y as f64);
            // 出口转 BD-09，和围栏/打卡点统一
            let (blat, blng) = crate::api::coord::gcj02_to_bd09(glat, glng);
            pts.push((blat, blng));
        }
        if pts.len() >= 2 {
            segments.push(RoadSegment {
                points: pts,
                kind: "tile".into(),
            });
        }
    }

    if segments.is_empty() {
        return Err(ErrorKind::fence(
            "瓦片上找到了路面，但没串出可用线段".into(),
        ));
    }

    let before = segments.len();
    // 焊断口：追踪是按分支点切的，不合并的话下游会跨段跳
    let segments = merge_segments(segments, 12.0);
    let total: usize = segments.iter().map(|s| s.points.len()).sum();
    Ok(RoadNet {
        source: format!(
            "高德瓦片 z{ZOOM} · {nx}×{ny} 张 · {before} 段→合并后 {} 段 / {total} 点",
            segments.len()
        ),
        segments,
        mask: Some(mask),
    })
}

/// 把端点相邻的碎段接成整条路。
///
/// 骨架追踪是按分支点切的，一条 200m 的路会被切成七八段；`route_on_roads`
/// 靠「找最近的段」拼接，段一碎它就在断口处跳，实测输出过 89m 的瞬移。
/// 这里先按端点距离把碎段焊起来，让整条路重新变成长段。
///
/// 用网格哈希分桶（20m 一格），只比较同桶和相邻桶，避免 O(n²)——实测一个
/// 校区能提出 2000 多段，两两比较要 400 万次。
#[expect(
    clippy::unwrap_used,
    reason = "non-empty is guaranteed by the guard above"
)]
fn merge_segments(mut segs: Vec<RoadSegment>, tol_m: f64) -> Vec<RoadSegment> {
    if segs.len() < 2 {
        return segs;
    }
    let cell = 20.0 / 111_320.0; // 20m 对应的度数（纬向，粗略够用）
    let key = |p: (f64, f64)| -> (i32, i32) {
        ((p.0 / cell).floor() as i32, (p.1 / cell).floor() as i32)
    };

    for _ in 0..40 {
        // 端点分桶
        let mut grid: std::collections::HashMap<(i32, i32), Vec<(usize, bool)>> =
            std::collections::HashMap::new();
        let alive: Vec<bool> = vec![true; segs.len()];
        for (i, sg) in segs.iter().enumerate() {
            if !alive[i] || sg.points.len() < 2 {
                continue;
            }
            let h = sg.points[0];
            #[expect(
                clippy::unwrap_used,
                reason = "non-empty is guaranteed by the guard above"
            )]
            let t = *sg.points.last().unwrap();
            grid.entry(key(h)).or_default().push((i, false));
            grid.entry(key(t)).or_default().push((i, true));
        }

        let mut merged_any = false;
        let mut dead = vec![false; segs.len()];
        let mut appended: Vec<RoadSegment> = Vec::new();

        for i in 0..segs.len() {
            if dead[i] || !alive[i] || segs[i].points.len() < 2 {
                continue;
            }
            // 找 i 的尾端点附近有没有别的段端点
            #[expect(
                clippy::unwrap_used,
                reason = "non-empty is guaranteed by the guard above"
            )]
            let tail = *segs[i].points.last().unwrap();
            let (kx, ky) = key(tail);
            let mut found: Option<(usize, bool)> = None;
            'scan: for dx in -1..=1 {
                for dy in -1..=1 {
                    if let Some(cands) = grid.get(&(kx + dx, ky + dy)) {
                        for &(j, is_tail) in cands {
                            if j == i || dead[j] || !alive[j] || segs[j].points.len() < 2 {
                                continue;
                            }
                            let p = if is_tail {
                                *segs[j].points.last().unwrap()
                            } else {
                                segs[j].points[0]
                            };
                            if crate::track::geo::distance(tail, p) <= tol_m {
                                found = Some((j, is_tail));
                                break 'scan;
                            }
                        }
                    }
                }
            }

            let Some((j, is_tail)) = found else { continue };

            // 拼接：i 在前，j 接在后面（j 的尾端点相连则反转）
            let mut pts = segs[i].points.clone();
            let mut other = segs[j].points.clone();
            if is_tail {
                other.reverse();
            }
            // 去掉重合的那个点，避免零长度段
            if pts.len() >= 2 && other.len() >= 2 {
                pts.extend_from_slice(&other[1..]);
            }
            appended.push(RoadSegment {
                points: pts,
                kind: segs[i].kind.clone(),
            });
            dead[i] = true;
            dead[j] = true;
            merged_any = true;
        }

        if !merged_any {
            break;
        }
        let mut next: Vec<RoadSegment> = Vec::with_capacity(segs.len());
        for (i, sg) in segs.into_iter().enumerate() {
            if !dead[i] {
                next.push(sg);
            }
        }
        next.extend(appended);
        segs = next;
    }
    segs
}

// ------------------------------------------------------------------ 图游走

/// 沿路网把打卡点按顺序连起来（A* 最短路），输出全程贴路的轨迹。
///
/// 为什么不用贪心游走：游走走到死胡同要 `pop` 回退，回退之后往往又从同一条
/// 边走出去，路径里出现 A→B→A 的重复。那就是轨迹里那些 180° 的尖刺，也会
/// 让里程虚高（声明的 2001m 空间上只展开 1338m）。
///
/// A* 最短路天然不会来回走：每一步都在靠近目标。点与点之间的连接都是真实
/// 道路，串起来就是一条能跑的路。
///
/// 里程不足 `want_m` 时不硬凑——`totalDis` 报的是实测值，服务端按实测校验，
/// 差一点没关系。
pub fn route_through(
    g: &RoadGraph,
    must: &[(f64, f64)],
    want_m: f64,
) -> Result<Vec<(f64, f64)>, Error> {
    route_through_ex(g, must, want_m, false)
}

/// 同上，但可以**禁止末尾折返**。
///
/// `fix_end = true` 时，里程不够也不再「原路往回走一段」——因为用户指定了
/// 终点，那段折返会把末端带回几百米外，终点就白设了。代价是实际里程可能
/// 短于目标（少掉折返那部分），由调用方决定接不接受。
/// 未轮到的打卡点周围多少米算「不许擦过」。
pub const ZONE_BAN_RADIUS_M: f64 = 12.0;

/// 按顺序经过 `must` 各点走出约 `want_m` 米；`fix_end` 为真时不做末尾折返。
pub fn route_through_ex(
    g: &RoadGraph,
    must: &[(f64, f64)],
    want_m: f64,
    fix_end: bool,
) -> Result<Vec<(f64, f64)>, Error> {
    if must.len() < 2 {
        return Err(ErrorKind::fence("至少要两个点".into()));
    }
    let mut path: Vec<usize> = Vec::new();
    // 已经走过的边。闭环那一段拿它做惩罚，尽量别再原路走一遍。
    let mut used: HashSet<(usize, usize)> = HashSet::new();

    // 每个打卡点周围 25m 的节点。走到后面某一段时，这些点如果还没轮到，
    // 就不许从旁边擦过去——擦过去服务端就记成「提前经过」，按顺序判定
    // 直接失败（实测 #2 在 idx=89 被踩到、#1 还在 idx=148）。
    //
    // 半径从 25m 收到 `ZONE_BAN_RADIUS_M`（12m）：校园点位大多挂在同一条主路
    // 上，25m 半径会把主路整段判成禁区，硬禁之后路网不连通，只能退回软罚，
    // 于是照样擦过去（实测 5 个点位首过 0,2,3,4,1）。12m 刚好覆盖服务端认
    // 「经过」的量级，又不会把路切断。
    let zones: Vec<HashSet<usize>> = must
        .iter()
        .map(|p| {
            g.nodes
                .iter()
                .enumerate()
                .filter(|(_, n)| crate::track::geo::distance(**n, *p) < ZONE_BAN_RADIUS_M)
                .map(|(i, _)| i)
                .collect()
        })
        .collect();

    // 只连 must[0..n]，不闭环。
    //
    // 闭环回到起点是「来回跑」的根：5 个点跨度 500m、目标 2000m，回程
    // 1400m 无论怎么绕都要用主干道，实测重复率 47%，闭环比开放路径差
    // 一大截。真实跑步里跑完停在别处本来也常见。
    // `fix_end` 时终点不参与「凑里程」：先把 must[..n-1] 连起来、绕够里程，
    // 最后再单独 A* 走一段到终点。否则凑里程的 while 循环会把终点变成中途点，
    // 收尾仍然要拉几百米直线（实测 583m）。
    // fix_end 时主循环只连到倒数第二个必到点：终点必须留到最后一段，
    // 否则「到达终点后还要继续凑里程」又会把它变成中途点。
    let connect_end = if fix_end && must.len() >= 2 {
        must.len() - 2
    } else {
        must.len() - 1
    };
    for i in 0..connect_end {
        let a = g.nearest(must[i]);
        let b = g.nearest(must[i + 1]);

        // 这一段的禁区：所有还没轮到的点
        let mut banned: HashSet<usize> = HashSet::new();
        for z in &zones[i + 2..] {
            banned.extend(z.iter().copied());
        }
        banned.remove(&a);
        banned.remove(&b);

        let plain = astar(g, a, b);
        // 硬禁（跳过禁区节点）优先；走不通再软罚（×5），再不行才无约束。
        // 硬禁走不通说明这一段只有一条穿过后面点位的路，交由下游 `[point-order]` 自检报出。
        let clean = if banned.is_empty() {
            None
        } else {
            astar_penalized(g, a, b, &HashSet::new(), 1.0, &banned, true)
                .or_else(|| astar_penalized(g, a, b, &HashSet::new(), 1.0, &banned, false))
        };

        let seg = clean.or(plain).ok_or_else(|| {
            ErrorKind::fence(format!("点 {} 到点 {} 之间路网不连通", i + 1, i + 2))
        })?;

        for w in seg.windows(2) {
            used.insert(if w[0] < w[1] {
                (w[0], w[1])
            } else {
                (w[1], w[0])
            });
        }

        if path.is_empty() {
            path.extend(seg);
        } else {
            path.extend(seg.into_iter().skip(1));
        }
    }
    if path.len() < 3 {
        return Err(ErrorKind::fence("连出来的路径太短".into()));
    }

    // 5 个点走完后，从最后一个点继续往前延伸，把剩余里程走满。
    //
    // 原来是「闭环 + laps 重复整圈」补里程，问题有两个：闭环回起点必然
    // 沿去程倒回去（重复率高），laps 再把整圈重复一遍（实测 61~67% 的
    // 采样点落在重复路段上）。改成从终点向远处延伸，重复率直接归零。
    // 5 个点走完后，从最后一个点继续往前延伸，把剩余里程走满。
    //
    // 原来是「闭环 + laps 重复整圈」补里程：闭环回起点必然沿去程倒回去，
    // laps 再把整圈重复一遍，实测 61~67% 的采样点落在重复路段上。
    //
    // 改成多段延伸——路网半径有限（实测从终点出发最远只够 1055m，而需要
    // 1546m），一段走不满就接着走下一段，每段都避开已经走过的边。
    let all_zones: HashSet<usize> = zones.iter().flatten().copied().collect();
    let mut total: f64 = path_len(g, &path);
    let mut hops = 0;
    let end_zone: HashSet<usize> = if fix_end {
        zones.last().cloned().unwrap_or_default()
    } else {
        HashSet::new()
    };
    while total < want_m - 150.0 && hops < 5 {
        hops += 1;
        #[expect(
            clippy::unwrap_used,
            reason = "non-empty is guaranteed by the guard above"
        )]
        let last = *path.last().unwrap();
        let de = dijkstra_all(g, last);
        let need = want_m - total;
        let mut pick: Option<(f64, usize)> = None;
        for (m, &d) in de.iter().enumerate() {
            // 延伸段不许经过任何一个打卡点，否则那批点会被二次标记
            if all_zones.contains(&m) || m == last {
                continue;
            }
            // fix_end：凑里程时不能钻进终点周围，否则最后一段又从终点
            // 折回来，收尾还是要拉直线
            if end_zone.contains(&m) {
                continue;
            }
            if !d.is_finite() || d < 120.0 {
                continue;
            }
            // 别选远超剩余量的点：排除终点区域后候选变少，选太远的一下
            // 就把里程冲过头（实测目标 2000 跑出 2663）。
            if d > need {
                continue;
            }
            // 选最接近剩余里程的。这里不设偏差上限——路网就这么大，
            // 挑不到正好合适的就让总长略超/略短，交给最后的折返补。
            let closer = pick
                .map(|(bd, _)| (d - need).abs() < (bd - need).abs())
                .unwrap_or(true);
            if closer {
                pick = Some((d, m));
            }
        }
        let Some((_, m)) = pick else { break };
        let Some(tail) = astar_penalized(g, last, m, &used, 25.0, &HashSet::new(), false) else {
            break;
        };
        for w in tail.windows(2) {
            used.insert(if w[0] < w[1] {
                (w[0], w[1])
            } else {
                (w[1], w[0])
            });
        }
        path.extend(tail.into_iter().skip(1));
        total = path_len(g, &path);
    }

    // fix_end：最后单独走一段到终点（前面所有凑里程都在它之前完成）。
    if fix_end && must.len() >= 2 {
        #[expect(
            clippy::unwrap_used,
            reason = "non-empty is guaranteed by the guard above"
        )]
        let a = *path.last().unwrap();
        #[expect(
            clippy::unwrap_used,
            reason = "non-empty is guaranteed by the guard above"
        )]
        let b = g.nearest(*must.last().unwrap());
        if a != b {
            if let Some(seg) = astar(g, a, b) {
                path.extend(seg.into_iter().skip(1));
            } else {
                return Err(ErrorKind::fence("最后一段到终点的路网不连通".into()));
            }
        }
    }

    // 还差一截就从终点原路折返。折返只重复缺口的一半，重复率大约是
    // 缺口的 1/4，比按圈重复整圈好得多。
    if total < want_m && !fix_end {
        let need = want_m - total;
        let mut acc = 0.0;
        let mut cut = path.len().saturating_sub(1);
        for i in (0..path.len().saturating_sub(1)).rev() {
            let d = crate::track::geo::distance(g.nodes[path[i]], g.nodes[path[i + 1]]);
            if acc + d > need / 2.0 {
                break;
            }
            acc += d;
            cut = i;
        }
        let back: Vec<usize> = path[cut..].iter().rev().copied().collect();
        path.extend(back.into_iter().skip(1));
    }

    Ok(path.into_iter().map(|i| g.nodes[i]).collect())
}

/// 从 s 出发到全图各点的最短距离（不可达为 INF）。闭环选绕行顶点用。
fn dijkstra_all(g: &RoadGraph, s: usize) -> Vec<f64> {
    use std::cmp::Reverse;
    use std::collections::BinaryHeap;
    let n = g.nodes.len();
    let mut best = vec![f64::INFINITY; n];
    let mut heap: BinaryHeap<Reverse<(i64, usize)>> = BinaryHeap::new();
    best[s] = 0.0;
    heap.push(Reverse((0, s)));
    while let Some(Reverse((du, u))) = heap.pop() {
        let d = du as f64 / 100.0;
        if d > best[u] + 1e-9 {
            continue;
        }
        for &(v, w, _) in &g.adj[u] {
            let nd = best[u] + w;
            if nd + 1e-9 < best[v] {
                best[v] = nd;
                heap.push(Reverse(((nd * 100.0) as i64, v)));
            }
        }
    }
    best
}

/// 一段路径的总长（米）。
fn path_len(g: &RoadGraph, p: &[usize]) -> f64 {
    p.windows(2)
        .map(|w| crate::track::geo::distance(g.nodes[w[0]], g.nodes[w[1]]))
        .sum()
}

/// 图上 A*。启发式用直线距离，权重是实际边长。
fn astar(g: &RoadGraph, s: usize, t: usize) -> Option<Vec<usize>> {
    astar_penalized(g, s, t, &HashSet::new(), 1.0, &HashSet::new(), false)
}

/// 带边惩罚的 A*。`penal` 里的边代价乘 `factor`。
///
/// 惩罚只放大代价、不禁用，所以启发式仍然是可采纳的下界，A* 的最优性
/// 不受影响——只是最优解变成「把惩罚算进去之后最短的那条」。
fn astar_penalized(
    g: &RoadGraph,
    s: usize,
    t: usize,
    penal: &HashSet<(usize, usize)>,
    factor: f64,
    banned: &HashSet<usize>,
    hard: bool,
) -> Option<Vec<usize>> {
    use std::cmp::Reverse;
    use std::collections::BinaryHeap;

    let n = g.nodes.len();
    let mut best = vec![f64::INFINITY; n];
    let mut came = vec![usize::MAX; n];
    let mut heap: BinaryHeap<Reverse<(i64, usize)>> = BinaryHeap::new();
    let h = |i: usize| crate::track::geo::distance(g.nodes[i], g.nodes[t]);
    best[s] = 0.0;
    heap.push(Reverse(((h(s) * 100.0) as i64, s)));

    while let Some(Reverse((_, u))) = heap.pop() {
        if u == t {
            let mut out = vec![t];
            let mut c = t;
            while came[c] != usize::MAX {
                c = came[c];
                out.push(c);
            }
            out.reverse();
            return Some(out);
        }
        for &(v, w, _) in &g.adj[u] {
            let key = if u < v { (u, v) } else { (v, u) };
            let mut w = if penal.contains(&key) { w * factor } else { w };
            // 禁区是软约束：绕得开就绕，绕不开也得有解。
            // 最早的写法是直接 continue 跳过禁区节点，结果 #0→#1 这一段在
            // 路网上无解（禁区把唯一通路切断了），退回无约束最短路之后
            // 照样从 #3 旁边擦过去，顺序判定还是错。
            if banned.contains(&v) && v != t {
                if hard {
                    continue;
                }
                w *= 5.0;
            }
            let ng = best[u] + w;
            if ng + 1e-9 < best[v] {
                best[v] = ng;
                came[v] = u;
                heap.push(Reverse((((ng + h(v)) * 100.0) as i64, v)));
            }
        }
    }
    None
}

/// 把沿路路径磨圆 + 轻微摆动，别像尺子画的。
///
/// 瓦片路网的骨架是**直线段**：一条街就是一串严格共线的点，拐弯是直角。
/// 直接拿去当跑步轨迹有两个毛病——拐角太硬，沿路又太直，一看就是程序
/// 生成的。
///
/// 分两步：
///   1. 保端点的 1-2-1 加权平均，把直角摊成圆弧
///   2. `add_wander` 按弧长加小幅横向摆动（真人跑步本来就有左右晃）
///
/// 幅度都压在路宽以内（合计 6~7m），偏移太多就会跑到路外面去。
/// 打卡点由下游的 `resample_with_checkpoints` 再钉一次。
pub fn soften(points: &[(f64, f64)], passes: usize, amp_m: f64, seed: u64) -> Vec<(f64, f64)> {
    if points.len() < 4 {
        return points.to_vec();
    }
    let n = points.len();
    let mut cur = points.to_vec();
    for _ in 0..passes {
        let mut next = cur.clone();
        // 首尾各留两个点不动：起点/终点要精确，另外平滑边界会因为
        // 缺少邻居而向外偏
        for i in 2..n - 2 {
            next[i] = (
                (cur[i - 1].0 + 2.0 * cur[i].0 + cur[i + 1].0) / 4.0,
                (cur[i - 1].1 + 2.0 * cur[i].1 + cur[i + 1].1) / 4.0,
            );
        }
        cur = next;
    }
    if amp_m > 0.0 {
        cur = crate::track::fence_route::add_wander(&cur, amp_m, 45.0, seed);
    }
    cur
}

/// 把线段集合焊成一张可走的图。
///
/// `route_on_roads` 那套「每段只用一次、用完跳到最近的段」的模型是为 OSM
/// 的长 way 设计的；瓦片提出的是几百条几十米的碎段，按那个模型走，走到断口
/// 就停（实测目标 2000m 只走出 261m）。
///
/// 这里改成节点图：所有点按 1m 网格去重，段内相邻点连边，段间端点距离
/// 小于 `hop_m` 就连边。之后在图上走，断口由连边补上，不需要「跳」。
pub struct RoadGraph {
    /// 去重后的节点坐标。
    pub nodes: Vec<(f64, f64)>,
    /// 邻接表：(邻居节点, 边长米, 边编号)
    pub adj: Vec<Vec<(usize, f64, usize)>>,
    /// 每条边是否走过（规划时标记）。
    pub edge_used: Vec<bool>,
}

/// 1m 网格去重用的键。
fn node_key(p: (f64, f64)) -> (i64, i64) {
    let k = 111_320.0;
    (
        (p.0 * k / 1.0).round() as i64,
        (p.1 * k * 0.7 / 1.0).round() as i64,
    )
}

/// 把路网的线段焊成图：端点相距不超过 `hop_m` 就连边。
pub fn build_graph(net: &RoadNet, hop_m: f64) -> RoadGraph {
    use std::collections::HashMap;
    let mut nodes: Vec<(f64, f64)> = Vec::new();
    let mut index: HashMap<(i64, i64), usize> = HashMap::new();
    let mut adj: Vec<Vec<(usize, f64, usize)>> = Vec::new();
    let mut edge_used: Vec<bool> = Vec::new();
    let mut edges: HashMap<(usize, usize), usize> = HashMap::new();

    let mut intern = |p: (f64, f64),
                      nodes: &mut Vec<(f64, f64)>,
                      adj: &mut Vec<Vec<(usize, f64, usize)>>|
     -> usize {
        let k = node_key(p);
        if let Some(&i) = index.get(&k) {
            return i;
        }
        let i = nodes.len();
        nodes.push(p);
        adj.push(Vec::new());
        index.insert(k, i);
        i
    };

    let add_edge = |a: usize,
                    b: usize,
                    nodes: &[(f64, f64)],
                    adj: &mut Vec<Vec<(usize, f64, usize)>>,
                    edge_used: &mut Vec<bool>,
                    edges: &mut HashMap<(usize, usize), usize>| {
        if a == b {
            return;
        }
        let k = if a < b { (a, b) } else { (b, a) };
        if edges.contains_key(&k) {
            return;
        }
        let w = crate::track::geo::distance(nodes[a], nodes[b]);
        let e = edge_used.len();
        edge_used.push(false);
        edges.insert(k, e);
        adj[a].push((b, w, e));
        adj[b].push((a, w, e));
    };

    // 段内连边
    for seg in &net.segments {
        let mut prev: Option<usize> = None;
        for &p in &seg.points {
            let cur = intern(p, &mut nodes, &mut adj);
            if let Some(pv) = prev {
                add_edge(pv, cur, &nodes, &mut adj, &mut edge_used, &mut edges);
            }
            prev = Some(cur);
        }
    }

    // 段间连边。
    //
    // 早先只连「每段的首尾点」，结果图是个森林：边数几乎等于节点数，说明
    // 没有任何环，也没有把环道接回主干道——因为两条路的连接处往往落在段的
    // 中间而不是端点。图上走两步就无路可走（实测 429 步只踩过 155 个节点）。
    //
    // 现在按 12m 邻域把不同段的节点连起来。距离卡得比 hop_m 严，是为了
    // 不把两条平行的路凭空接上——平行路间距一般 15m 以上，12m 只会命中
    // 真正相交的路口。
    {
        let grid_m = hop_m.clamp(6.0, 20.0);
        let cell = grid_m / 111_320.0;
        let key = |p: (f64, f64)| -> (i32, i32) {
            (
                (p.0 / cell).floor() as i32,
                (p.1 / cell * 0.7).floor() as i32,
            )
        };
        let mut owner: Vec<usize> = vec![0; nodes.len()];
        for (si, seg) in net.segments.iter().enumerate() {
            for &p in &seg.points {
                if let Some(&i) = index.get(&node_key(p)) {
                    owner[i] = si;
                }
            }
        }
        let mut buckets: std::collections::HashMap<(i32, i32), Vec<usize>> =
            std::collections::HashMap::new();
        for (i, p) in nodes.iter().enumerate() {
            buckets.entry(key(*p)).or_default().push(i);
        }
        let mut links: Vec<(usize, usize)> = Vec::new();
        for (i, p) in nodes.iter().enumerate() {
            let (kx, ky) = key(*p);
            for dx in -1..=1 {
                for dy in -1..=1 {
                    if let Some(cands) = buckets.get(&(kx + dx, ky + dy)) {
                        for &j in cands {
                            if j <= i || owner[i] == owner[j] {
                                continue;
                            }
                            if crate::track::geo::distance(*p, nodes[j]) <= grid_m {
                                // 距离够了还得这条路真的能走：两条路各自贴着
                                // 楼的两侧、节点相距 12m 内也会被连上，A* 就
                                // 从楼中间直插过去（用户截图里那一刀）。
                                if let Some(m) = &net.mask
                                    && !m.line_follows_road(*p, nodes[j])
                                {
                                    continue;
                                }
                                links.push((i, j));
                            }
                        }
                    }
                }
            }
        }
        links.sort_unstable();
        links.dedup();
        for (a, b) in links {
            add_edge(a, b, &nodes, &mut adj, &mut edge_used, &mut edges);
        }
    }

    RoadGraph {
        nodes,
        adj,
        edge_used,
    }
}

impl RoadGraph {
    fn nearest(&self, p: (f64, f64)) -> usize {
        let mut best = 0usize;
        let mut bd = f64::MAX;
        for (i, n) in self.nodes.iter().enumerate() {
            let d = crate::track::geo::distance(*n, p);
            if d < bd {
                bd = d;
                best = i;
            }
        }
        best
    }
}

/// 在图上走够 `want_m`，优先把还没到过的 `must` 点串进来。
///
/// 每一段边只走一次；走到死胡同时回溯一步另找出口，实在没路才复用走过的
/// 边（总比原地停下好）。这样产出的轨迹全程贴着真实道路，不会跨空地跳。
pub fn walk_graph(
    g: &mut RoadGraph,
    start_hint: (f64, f64),
    must: &[(f64, f64)],
    want_m: f64,
    seed: u64,
) -> Result<Vec<(f64, f64)>, Error> {
    if g.nodes.is_empty() {
        return Err(ErrorKind::fence("图为空".into()));
    }
    let mut rng = seed | 1;
    let mut next_rand = || {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        rng
    };

    let start = g.nearest(start_hint);
    let mut path: Vec<usize> = vec![start];
    let mut total = 0.0f64;
    let mut cur = start;
    // 刚走过的边。禁止立刻原路返回——否则贪心会在两个节点之间来回
    // 抖，2000m 的路径在空间上只展开几百米。
    let mut came_edge: Option<usize> = None;
    let mut pending: Vec<(f64, f64)> = must.to_vec();

    // 打卡点走过就划掉
    let mark_passed = |cur_pt: (f64, f64), pending: &mut Vec<(f64, f64)>| {
        pending.retain(|m| crate::track::geo::distance(cur_pt, *m) > 15.0);
    };
    mark_passed(g.nodes[cur], &mut pending);

    let mut stall = 0usize;
    while total < want_m && stall < g.nodes.len() * 4 {
        // 目标：优先最近的未完成打卡点，全完成就随便走
        let goal = pending
            .iter()
            .min_by(|a, b| {
                let da = crate::track::geo::distance(g.nodes[cur], **a);
                let db = crate::track::geo::distance(g.nodes[cur], **b);
                da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
            })
            .copied();

        // 分两轮挑：第一轮只走「还有别的出口」的节点，第二轮才允许进死胡同。
        //
        // 骨架图的枝杈末端是度 1 的点，走进去必然要原路退出来——这一进一出
        // 就是轨迹里那个 180° 的掉头。先走主干和环，末梢留到最后。
        let mut chosen: Option<(usize, f64, usize)> = None;
        for pass in 0..2 {
            let mut best: Option<(usize, f64, f64)> = None; // (邻居, 边长, 评分)
            for &(nb, w, e) in &g.adj[cur] {
                if pass == 0 && g.adj[nb].len() <= 1 {
                    continue;
                }
                // 马上原路返回：除非没有别的出口，否则不走
                if Some(e) == came_edge && g.adj[cur].len() > 1 {
                    continue;
                }
                // 走过的边不禁止，只加惩罚。路网本来就是网状，回头路是常态，
                // 禁掉等于把图砍成一条线（实测目标 2000m 只走出 891m）。
                let penalty = if g.edge_used[e] { 400.0 } else { 0.0 };
                let base = match goal {
                    Some(t) => {
                        let before = crate::track::geo::distance(g.nodes[cur], t);
                        let after = crate::track::geo::distance(g.nodes[nb], t);
                        if after < before {
                            -500.0 + w
                        } else {
                            w + 200.0
                        }
                    }
                    None => w,
                };
                let jitter = (next_rand() % 1000) as f64 / 1000.0;
                let score = base + penalty + jitter * 0.5;
                if best.map(|b| score < b.2).unwrap_or(true) {
                    best = Some((nb, w, score));
                }
            }
            if let Some((nb, w, _)) = best {
                // 找回对应的边号
                let e = g.adj[cur]
                    .iter()
                    .find(|(n, _, _)| *n == nb)
                    .map(|(_, _, e)| *e)
                    .unwrap_or(0);
                chosen = Some((nb, w, e));
                break;
            }
        }

        match chosen {
            Some((nb, w, e)) => {
                g.edge_used[e] = true;
                came_edge = Some(e);
                cur = nb;
                path.push(cur);
                total += w;
                mark_passed(g.nodes[cur], &mut pending);
            }
            None => {
                // 走到尽头：原路退回去，这一段**照样计入里程**。
                //
                // 真人跑到死胡同就是折返跑回来，轨迹上确实有这段路。早先
                // 是 pop 掉再扣里程，等于假装没走过——路径里不留痕迹，长度
                // 又对不上。
                if path.len() >= 2 {
                    let prev = path[path.len() - 2];
                    for &(nb, _, e) in &g.adj[cur] {
                        if nb == prev {
                            g.edge_used[e] = false;
                        }
                    }
                    cur = prev;
                    path.push(cur);
                    total += crate::track::geo::distance(g.nodes[prev], g.nodes[cur]);
                    came_edge = None;
                }
                stall += 1;
            }
        }
    }

    if path.len() < 3 {
        return Err(ErrorKind::fence("图上游走失败，没走出路径".into()));
    }
    Ok(path.into_iter().map(|i| g.nodes[i]).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestDir, assert_isolated};

    /// 瓦片冷却窗口：四个域名全挂后 10 分钟内直接快速失败。

    #[test]
    fn tile_cooldown_window() {
        let t = 1_790_000_000_000u64;
        assert!(!tile_cooling_at(t, 0));
        assert!(tile_cooling_at(t, t - 1_000));
        assert!(tile_cooling_at(t, t - (TILE_DOWN_COOLDOWN_MS - 1)));
        assert!(!tile_cooling_at(t, t - TILE_DOWN_COOLDOWN_MS));
    }

    /// 护栏：瓦片缓存目录的注入版**绝不能**落到真实 `$HOME/.pace-forge/tiles`。
    ///
    /// 模式与 `session::tests::test_data_dir_is_isolated_from_real_home` 一致。
    /// 谁哪天把 `tile_path_in` 改回 `cache_dir()`，这条立刻炸。
    #[test]
    fn tile_cache_dir_is_isolated_from_real_home() {
        let t = TestDir::new("tiles-isolated");
        let injected = cache_dir_in(t.home());
        assert_isolated(&injected, "瓦片缓存目录");
        assert_ne!(
            injected,
            cache_dir_in(&crate::test_support::real_home()),
            "注入路径不能等于生产路径"
        );
        assert_eq!(injected.file_name().unwrap(), "tiles");
        assert_eq!(
            injected.parent().unwrap().file_name().unwrap(),
            ".pace-forge"
        );
    }

    /// 瓦片文件名布局不能变（`<base>/.pace-forge/tiles/<z>_<x>_<y>.png`），
    /// 否则老机器上的缓存全部失效、整片区域要重新拉一遍瓦片。
    #[test]
    fn tile_path_layout_is_stable_and_injected() {
        let t = TestDir::new("tiles-path");
        let p = tile_path_in(t.home(), 123, 456);
        assert!(p.starts_with(t.data()), "瓦片必须落在注入的数据目录下");
        assert_eq!(
            p.file_name().unwrap().to_string_lossy(),
            format!("{ZOOM}_123_456.png")
        );
        assert!(!t.data().exists(), "只算路径不该创建目录");
    }

    /// 空白瓦片是合法结果：高德在没有道路渲染的区域回 ~179B 的纯色 PNG。
    /// 「长度小 = 失败」的判据会把整片区域判失败并进冷却（#2 实测），
    /// 所以判据必须是「能不能解码」。
    #[test]
    fn tiny_blank_png_is_a_valid_tile() {
        // 1×1 白色 PNG（比阈值 512B 小得多）
        const BLANK: &[u8] = &[
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48,
            0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00,
            0x00, 0x90, 0x77, 0x53, 0xDE, 0x00, 0x00, 0x00, 0x0C, 0x49, 0x44, 0x41, 0x54, 0x08,
            0xD7, 0x63, 0xF8, 0xCF, 0xC0, 0x00, 0x00, 0x03, 0x01, 0x01, 0x00, 0x18, 0xDD, 0x8D,
            0xB0, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
        ];
        assert!(BLANK.len() < 512, "样例本身要小于旧阈值");
        assert!(
            image::load_from_memory(BLANK).is_ok(),
            "空白图必须能被解码，否则新判据会把正常响应判成失败"
        );
        // 反过来：同样小的垃圾字节不能当成功
        assert!(image::load_from_memory(&[0u8; 179]).is_err());
    }

    /// 缓存里那份超过 512B 但解码失败的坏瓦片，必须删掉而不是留下毒化缓存：
    /// 旧实现解码报错后原样保留文件，下次请求还会读到同一份坏字节。
    #[test]
    fn corrupt_cached_tile_is_deleted() {
        let t = TestDir::new("tile-corrupt");
        let path = tile_path_in(t.home(), 7, 9);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, vec![0xab_u8; 1024]).unwrap();
        assert!(path.exists());

        let err = fetch_tile_in(t.home(), 7, 9).expect_err("坏瓦片必须解码失败");
        assert!(err.to_string().contains("解码失败"), "错误信息: {err}");
        assert!(!path.exists(), "解码失败的坏文件应被删除，不能留在缓存里");
    }

    #[test]
    fn mercator_roundtrip() {
        // 已知点：天安门 GCJ-02
        let (lat, lng) = (39.9087, 116.3975);
        let (tx, ty) = lnglat_to_tile(lat, lng);
        let n = (1u32 << ZOOM) as f64;
        let back_lng = tx / n * 360.0 - 180.0;
        let back_lat = (std::f64::consts::PI * (1.0 - 2.0 * ty / n))
            .sinh()
            .atan()
            .to_degrees();
        assert!((back_lat - lat).abs() < 1e-9, "纬度回环误差过大");
        assert!((back_lng - lng).abs() < 1e-9, "经度回环误差过大");
    }

    #[test]
    fn road_pixel_detection() {
        // 纯白 = 路
        assert!(is_road(&[255, 255, 255]));
        assert!(is_road(&[250, 250, 250]));
        // 高德浅灰底 (240) 不是路——这一条是整条链能不能用的关键
        assert!(!is_road(&[240, 240, 240]));
        assert!(!is_road(&[239, 239, 239]));
        // 有色偏的不是路（绿地、水面、黄色主干道）
        assert!(!is_road(&[252, 250, 240]));
    }

    #[test]
    fn thinning_keeps_skeleton_thin() {
        // 3px 宽的横条，细化后应只剩 1px 高
        let (w, h) = (40usize, 10usize);
        let mut bits = vec![false; w * h];
        for y in 4..7 {
            for x in 5..35 {
                bits[y * w + x] = true;
            }
        }
        let m = Mask {
            w,
            h,
            bits,
            geo: GeoRef { tx0: 0, ty0: 0 },
        };
        let sk = thin(&m);
        for x in 8..32 {
            let col: Vec<bool> = (0..h).map(|y| sk[y * w + x]).collect();
            let n = col.iter().filter(|v| **v).count();
            assert!(n <= 1, "第 {x} 列还有 {n} 个像素，没细化干净");
        }
    }

    #[test]
    fn close_gaps_bridges_small_holes() {
        // 一条被 2px 缺口断开的线，闭运算后应连上
        let (w, h) = (30usize, 3usize);
        let mut bits = vec![false; w * h];
        for x in 2..12 {
            bits[w + x] = true;
        }
        for x in 15..28 {
            bits[w + x] = true;
        }
        let mut m = Mask {
            w,
            h,
            bits,
            geo: GeoRef { tx0: 0, ty0: 0 },
        };
        close_gaps(&mut m, 2);
        let covered = (12..15).any(|x| m.at(x, 1));
        assert!(covered, "缺口没被闭运算接上");
    }

    // ---- merged from former `mod live_tests` ----

    /// 闭环段不能原路折返，也不能提前穿过后面的打卡点。
    /// 用固定坐标离线跑，不依赖打卡点接口（那个 5 分钟限 3 次）。
    ///   cargo test --lib closing_leg -- --ignored --nocapture
    #[test]
    #[ignore]
    fn live_closing_leg_not_backtracking() {
        let must = vec![
            (47.385626, 103.955836),
            (47.385718, 103.956956),
            (47.386375, 103.958661),
            (47.386792, 103.959479),
            (47.387745, 103.959490),
        ];
        let net = fetch_roadnet((47.379, 103.950, 47.390, 103.964)).expect("瓦片路网");
        let g = build_graph(&net, 25.0);
        let path = route_through(&g, &must, 2000.0).expect("规划");
        // route_through 返回的就是坐标序列
        let pts: Vec<(f64, f64)> = path;
        let len: f64 = pts
            .windows(2)
            .map(|w| crate::track::geo::distance(w[0], w[1]))
            .sum();

        // 穿墙检查：轨迹上每个点周围 6px 内必须能找到路面。
        let m = net.mask.as_ref().expect("瓦片来源应该有掩膜");
        let off = pts.iter().filter(|q| !m.near_road(q.0, q.1, 6)).count();
        println!("[closing] 不在路面上的点 {off}/{}", pts.len());
        assert!(
            off as f64 / (pts.len() as f64) < 0.05,
            "轨迹有 {off} 个点不在路面上，可能穿墙"
        );

        let mut idx = Vec::new();
        for p in &must {
            idx.push(
                pts.iter()
                    .position(|q| crate::track::geo::distance(*q, *p) < 12.0)
                    .unwrap_or(usize::MAX),
            );
        }
        println!("[closing] 里程 {len:.0}m / {} 点", pts.len());
        println!("[closing] 首次经过 {idx:?}");
        assert!(idx.windows(2).all(|w| w[0] < w[1]), "点位顺序错乱：{idx:?}");

        let s = 10.0 / 111320.0;
        let mut hit = std::collections::HashMap::new();
        for p in &pts {
            let so = 10.0 / (111320.0 * p.0.to_radians().cos());
            let k = ((p.0 / s).round() as i64, (p.1 / so).round() as i64);
            *hit.entry(k).or_insert(0) += 1;
        }
        let vis: i64 = hit.values().sum();
        let rep: i64 = hit.values().map(|v| (v - 1).max(0)).sum();
        println!(
            "[closing] 格子 {} / 重复 {:.1}%",
            hit.len(),
            rep as f64 / vis as f64 * 100.0
        );
        // 50% 是这套坐标系下的现实上限：5 个点跨度 500m、路网从终点算最远
        // 1055m，跑 2000m 必然有一段折返。改造前这里是 65%。
        // 加了「段间连边必须贴着路面」的校验后路网收窄（覆盖格子 232→203），
        // 重复率从 37.8% 回到 45.1%——这是拿一点点重复换不穿墙，划算。
        assert!(rep as f64 / (vis as f64) < 0.50, "重复率过高");
    }

    /// 真实拉取 + 图游走。跑法：
    ///   cargo test --lib tileroad::live -- --ignored --nocapture
    #[test]
    #[ignore]
    fn live_campus_roadnet() {
        // 某校园围栏范围（合成）（BD-09）
        let bbox = (47.3796, 103.9514, 47.3888, 103.9631);
        let t0 = std::time::Instant::now();
        let net = fetch_roadnet(bbox).expect("拉取失败");
        println!("{}  ({:.1}s)", net.source, t0.elapsed().as_secs_f64());
        assert!(net.segments.len() > 20, "线段太少，路网没提出来");

        // 服务端真实下发的 5 个打卡点（BD-09）
        let cps = [
            (47.385626, 103.955836),
            (47.385718, 103.956956),
            (47.386375, 103.958661),
            (47.386792, 103.959479),
            (47.387745, 103.959490),
        ];

        // 每个点都得离路网够近，否则图上根本走不到
        for (i, cp) in cps.iter().enumerate() {
            let mut best = f64::INFINITY;
            for sg in &net.segments {
                for p in &sg.points {
                    let d = crate::track::geo::distance(*cp, *p);
                    if d < best {
                        best = d;
                    }
                }
            }
            println!("  打卡点 #{} 距路网 {:.1} m", i + 1, best);
            assert!(best < 40.0, "打卡点 #{} 离路网 {best:.0}m，走不到", i + 1);
        }

        let mut g = build_graph(&net, 25.0);
        println!("图：{} 节点 / {} 边", g.nodes.len(), g.edge_used.len());
        let path = walk_graph(&mut g, cps[0], &cps, 2000.0, 20261112).expect("图游走失败");
        let len: f64 = path
            .windows(2)
            .map(|w| crate::track::geo::distance(w[0], w[1]))
            .sum();
        println!("游走 {} 点 / {:.0} m", path.len(), len);

        let mut hit = 0;
        for (i, cp) in cps.iter().enumerate() {
            let d = path
                .iter()
                .map(|p| crate::track::geo::distance(*p, *cp))
                .fold(f64::INFINITY, f64::min);
            let ok = d < 15.0;
            println!(
                "  打卡点 #{} 最近 {:.1} m {}",
                i + 1,
                d,
                if ok { "✓" } else { "✗" }
            );
            if ok {
                hit += 1;
            }
        }
        assert_eq!(hit, 5, "只跑到 {hit}/5 个打卡点");
    }

    /// 磨圆前后的转角分布对比，并导出图像供人工核对。
    #[test]
    #[ignore]
    fn live_soften_shape() {
        let bbox = (47.3796, 103.9514, 47.3888, 103.9631);
        let net = fetch_roadnet(bbox).expect("拉取失败");
        let cps = [
            (47.385626, 103.955836),
            (47.385718, 103.956956),
            (47.386375, 103.958661),
            (47.386792, 103.959479),
            (47.387745, 103.959490),
        ];
        let g = build_graph(&net, 25.0);
        let raw = route_through(&g, &cps, 2000.0).expect("连点失败");

        let stat = |name: &str, pts: &[(f64, f64)]| {
            let mut t: Vec<f64> = Vec::new();
            for i in 1..pts.len() - 1 {
                let b = |a: (f64, f64), c: (f64, f64)| {
                    let ml = 111320.0;
                    let mlng = ml * a.0.to_radians().cos();
                    ((c.0 - a.0) * ml).atan2((c.1 - a.1) * mlng).to_degrees()
                };
                let mut d = b(pts[i], pts[i + 1]) - b(pts[i - 1], pts[i]);
                while d > 180.0 {
                    d -= 360.0;
                }
                while d < -180.0 {
                    d += 360.0;
                }
                t.push(d.abs());
            }
            t.sort_by(|a, b| a.partial_cmp(b).unwrap());
            let pct = |q: f64| t[((t.len() as f64 - 1.0) * q) as usize];
            let len: f64 = pts
                .windows(2)
                .map(|w| crate::track::geo::distance(w[0], w[1]))
                .sum();
            println!(
                "{name}: {} 点 / {:.0}m | 中位 {:.1}° p90 {:.1}° p99 {:.1}° 最大 {:.1}°",
                pts.len(),
                len,
                pct(0.5),
                pct(0.9),
                pct(0.99),
                t[t.len() - 1]
            );
        };
        stat("原始骨架 ", &raw);
        let soft = soften(&raw, 3, 1.2, 20261112);
        stat("磨圆+摆动", &soft);

        // 打卡点必须还在附近
        for (i, cp) in cps.iter().enumerate() {
            let d = soft
                .iter()
                .map(|p| crate::track::geo::distance(*p, *cp))
                .fold(f64::INFINITY, f64::min);
            println!("  打卡点 #{} 最近 {:.1} m", i + 1, d);
        }

        // 导出坐标给外部画图
        let dump = |pts: &[(f64, f64)]| -> String {
            pts.iter()
                .map(|p| format!("{:.7},{:.7}", p.0, p.1))
                .collect::<Vec<_>>()
                .join(";")
        };
        let _ = std::fs::write("/tmp/shape_raw.txt", dump(&raw));
        let _ = std::fs::write("/tmp/shape_soft.txt", dump(&soft));
        println!("已导出 /tmp/shape_raw.txt 与 /tmp/shape_soft.txt");
    }

    impl Mask {
        #[inline]
        fn at(&self, x: usize, y: usize) -> bool {
            self.bits[y * self.w + x]
        }
    }
}

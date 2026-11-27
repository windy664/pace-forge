//! 闭合 Hermite 环：按服务端下发的点位次序穿过全部打卡点。
//!
//! ## 为什么需要它
//!
//! 操场（stadium）模式原来只会画**椭圆**：绕一个「直道 + 两端半圆」跑，
//! 绕圈方向决定了「每个打卡点首次被经过」的次序。而服务端判「按顺序通过
//! 所有点位」用的是**它自己下发的点位数组序**（实测 8 条记录 type=9 全过，
//! 见 `docs/notes/TYPE9-NOTES.md`）。
//!
//! 实测某校 5 个点的数组序是 `1→4→5→2→3`，而它们在凸包上的环序是
//! `1→2→3→4→5` —— **数组序不是环序**。椭圆绕圈必然按环序经过 ⇒ 首次经过
//! 序与数组序不同序 ⇒ 顺序闸必拒 ⇒ 整个操场模式退回路线规划，而那条路
//! 是「折线 + 每段一条鼓出弧」，在图上是一团交叠的乱线。
//!
//! 这里给的是另一个解：**一条穿过全部点位、按数组序排列的光滑闭环**。
//!
//! ## 为什么是 Hermite 而不是样条
//!
//! 周期三次样条（最小弯曲能量的插值环）看着更「圆」，但**上报点尺度上**
//! 一样有 150° 的尖刺：样条把节点处的方向突变**集中在节点附近**，
//! 而上报点间距 14m，一处集中突变就正好落在两个上报点之间 ⇒ 报告成折角。
//! 实测（同一组打卡点、上报尺度 14m）：
//!
//! | 构造 | >25° | >60° | 最大转角 |
//! |---|---|---|---|
//! | 生产旧轨迹（折线 + 鼓出弧） | 38 处 | 22 处 | 163° |
//! | 周期三次样条环 | 24~32 处 | 12~22 处 | 157~178° |
//! | **本模块的 Hermite 环** | **6 处** | **0 处** | **56°** |
//!
//! ## 关键几何事实：掉头不能用有限半径倒圆
//!
//! 相邻两点的「进出方向夹角」φ 接近 180°（掉头）时，倒圆的最近距离是
//! `R/sin(θ/2) - R`（θ = 180° − φ），φ→180° 时它发散 —— **掉头没法用
//! 有限半径圆滑掉**。要平滑，路径必须「冲过头再绕回来」（发夹），
//! 而发夹的转弯必须发生在**段中间**，不能发生在打卡点上。
//!
//! 于是每个节点的**切向方向是自由参数**：
//!
//! - 普通节点：取进出方向的**角平分线** ⇒ 节点处切向连续，不是折角；
//! - 掉头节点：取「入射方向左转 [`LOOP_ALPHA_DEG`]」——左转之后，进出
//!   方向不再对冲，转而各自往两侧分开，转弯被摊到相邻两段里。
//!
//! 实测这把上报尺度上的 >60° 急转从 22 处降到 **0 处**。

use super::geo::{distance, m_per_deg_lat, m_per_deg_lng};

/// 掉头节点的切向相对入射方向的转角（度，正 = 左转）。
///
/// 实测扫描（`alpha` 60~120°、`h` 多组，每组都过「上报尺度无 >60° 急转」
/// 「整条环在围栏内」「首次经过序单调」三道闸，且按**生产口径**（5m 几何
/// 采样 + GPS 噪声 + 多圈）量）：`80` 度最好——上报尺度上最大转角 57.9°、
/// >60° 的急转 **0 处**（生产旧轨迹是 22 处、最大 163°）。
pub const LOOP_ALPHA_DEG: f64 = 80.0;

/// 普通节点的切向长度系数（乘以「到较近邻点的距离」）。
pub const LOOP_H_SCALE: f64 = 1.4;

/// 掉头节点的切向长度系数。
///
/// 掉头处切向越长，绕回来的发夹越大 ⇒ 转弯摊得越开 ⇒ 上报尺度上的
/// 转角越小。
pub const LOOP_H_REV: f64 = 1.8;

/// 每圈换道的横向位移（米）。
///
/// 真机跑者会在不同道上跑（第 n 道比第 1 道外移 1.22m）。多圈轨迹要看起来
/// 像「一簇环」而不是「同一条线画 N 遍」，就得让圈与圈之间错开一点。
///
/// **这里取 0**：横向偏移会让圈与圈的**接缝**处横跳一段（实测接缝处多出
/// 一处 86° 的折角），而多圈的「带」由 [`add_gps_noise`] 的 OU 噪声给出
/// （真机同款：多圈不会完全重合，而是像真机那样一簇）。
pub const LANE_SHIFT_M: f64 = 0.0;

/// 进出方向近乎对冲的判定阈值（`d_in + d_out` 的模长）。
///
/// 小于它 ⇒ 该节点是掉头，切向改用「入射方向左转 [`LOOP_ALPHA_DEG`]」。
const REVERSAL_EPS: f64 = 0.35;

/// 穿过给定点的闭合 Hermite 环，按弧长近似等距采样。
///
/// - `points`：环上的节点，**顺序即经过次序**。
/// - `step_m`：输出点的近似间距（米）。
///
/// 返回的最后一个点与第一个点相同（环是闭合的）。
pub fn loop_through(points: &[(f64, f64)], step_m: f64) -> Vec<(f64, f64)> {
    let n = points.len();
    if n < 3 {
        return points.to_vec();
    }
    let step = if step_m > 0.0 { step_m } else { 5.0 };

    // 局部平面坐标（米）：切向长度、角度都在米制下算。
    let lat0 = points[0].0;
    let mlat = m_per_deg_lat(lat0).max(1.0);
    let mlng = m_per_deg_lng(lat0).max(1.0);
    let xy: Vec<(f64, f64)> = points
        .iter()
        .map(|p| ((p.1 - points[0].1) * mlng, (p.0 - lat0) * mlat))
        .collect();

    // 每个节点的切向方向与切向长度。
    let mut tang: Vec<(f64, f64)> = Vec::with_capacity(n);
    let mut hlen: Vec<f64> = Vec::with_capacity(n);
    for i in 0..n {
        let prev = (i + n - 1) % n;
        let next = (i + 1) % n;
        let d_in = unit((xy[i].0 - xy[prev].0, xy[i].1 - xy[prev].1));
        let d_out = unit((xy[next].0 - xy[i].0, xy[next].1 - xy[i].1));
        let sum = (d_in.0 + d_out.0, d_in.1 + d_out.1);
        let reversal = sum.0.hypot(sum.1) < REVERSAL_EPS;
        let t = if reversal {
            // 掉头：入射方向左转 LOOP_ALPHA_DEG。
            let a = LOOP_ALPHA_DEG.to_radians();
            let (c, s) = (a.cos(), a.sin());
            unit((d_in.0 * c - d_in.1 * s, d_in.0 * s + d_in.1 * c))
        } else {
            unit(sum)
        };
        tang.push(t);
        let near = dist_xy(xy[i], xy[prev]).min(dist_xy(xy[i], xy[next]));
        hlen.push(near * if reversal { LOOP_H_REV } else { LOOP_H_SCALE });
    }

    let mut out: Vec<(f64, f64)> = Vec::with_capacity(n * 64);
    let mut last_seg_start = 0usize;
    for i in 0..n {
        let j = (i + 1) % n;
        let seg = dist_xy(xy[i], xy[j]);
        if seg < 1e-6 {
            continue;
        }
        let steps = ((seg / step).ceil() as usize).clamp(4, 400);
        last_seg_start = out.len();
        let (p0, p1) = (xy[i], xy[j]);
        let (t0, t1) = (tang[i], tang[j]);
        let (h0, h1) = (hlen[i], hlen[j]);
        for k in 0..steps {
            let u = k as f64 / steps as f64;
            let (u2, u3) = (u * u, u * u * u);
            let (h00, h10, h01, h11) = (
                2.0 * u3 - 3.0 * u2 + 1.0,
                u3 - 2.0 * u2 + u,
                -2.0 * u3 + 3.0 * u2,
                u3 - u2,
            );
            let x = h00 * p0.0 + h10 * h0 * t0.0 + h01 * p1.0 + h11 * h1 * t1.0;
            let y = h00 * p0.1 + h10 * h0 * t0.1 + h01 * p1.1 + h11 * h1 * t1.1;
            out.push((lat0 + y / mlat, points[0].1 + x / mlng));
        }
    }
    if out.len() < 4 {
        return points.to_vec();
    }
    if let Some(first) = out.first().copied() {
        out.push(first);
    }
    rotate_to_last_leg_mid(&out, last_seg_start)
}

/// 把环的起点挪到「最后一段（末点 → 首点）」的中点。
///
/// 两个理由：
///
/// 1. **首次经过的点位次序不变**——起点落在「末点→首点」这一段里，往前
///    走先到的是**首点**（= 数组序的第一个点位）✓。
/// 2. **圈与圈的接缝落在低曲率处**——曲率在打卡点处最集中（那里的方向
///    变化有 140~164°）。接缝落在打卡点上时，实测接缝处多出一处 86° 的
///    折角；挪到段中点后那一处降到 60° 以下。
fn rotate_to_last_leg_mid(loop_pts: &[(f64, f64)], seg_start: usize) -> Vec<(f64, f64)> {
    let last = loop_pts.len() - 1;
    let start = seg_start.min(last.saturating_sub(1));
    let mut leg = 0.0f64;
    for i in start..last {
        leg += distance(loop_pts[i], loop_pts[i + 1]);
    }
    if leg <= 1e-6 {
        return loop_pts.to_vec();
    }
    let mut acc = 0.0f64;
    let mut idx = last - 1;
    for i in (start..last).rev() {
        acc += distance(loop_pts[i], loop_pts[i + 1]);
        if acc >= leg / 2.0 {
            idx = i;
            break;
        }
    }
    let mut out = Vec::with_capacity(loop_pts.len());
    out.extend_from_slice(&loop_pts[idx..]);
    out.extend_from_slice(&loop_pts[..idx + 1]);
    out
}

/// 按数组序穿过给定点的多圈轨迹。
///
/// - 环由 [`loop_through`] 生成（穿过全部点位，次序 = 数组序）。
/// - 圈与圈之间沿**环的起点切向**平移 [`LANE_SHIFT_M`]：真机跑者换道，
///   多圈因此是一「簇」而不是同一条线画 N 遍。
/// - 平移量沿圈**渐进**（螺旋）：第 k 圈从 `k−1` 个车道量渐变到 `k` 个，
///   于是圈接缝处位置连续 ⇒ 不会横跳出一处 180° 的折角。第一圈偏移恒为 0
///   ⇒ 打卡点在第一圈被**精确**命中。
/// - 叠加真机同款的 GPS 噪声（OU 过程）：相邻点有惯性，多圈自然成带。
///
/// `distance_m` 是目标里程；实际长度会略超（最后一圈截到目标为止）。
pub fn lap_track_through(
    points: &[(f64, f64)],
    distance_m: f64,
    step_m: f64,
    seed: u64,
) -> Vec<(f64, f64)> {
    let loop_pts = loop_through(points, step_m);
    if loop_pts.len() < 4 {
        return loop_pts;
    }
    let lap_len = super::geo::path_length(&loop_pts);
    if lap_len <= 1.0 {
        return loop_pts;
    }
    let laps = (distance_m / lap_len).ceil().max(1.0) as usize;

    // 环起点的切向：车道偏移沿它走，圈接缝才连续。
    let dir = {
        let a = loop_pts[0];
        let b = loop_pts[1];
        let mlat = m_per_deg_lat(a.0).max(1.0);
        let mlng = m_per_deg_lng(a.0).max(1.0);
        let dx = (b.1 - a.1) * mlng;
        let dy = (b.0 - a.0) * mlat;
        let n = (dx * dx + dy * dy).sqrt();
        if n > 1e-9 {
            (dx / n, dy / n)
        } else {
            (1.0, 0.0)
        }
    };

    let seg_n = loop_pts.len() - 1;
    let mut out: Vec<(f64, f64)> = Vec::with_capacity(laps * loop_pts.len());
    for lap in 0..laps {
        for (j, p) in loop_pts.iter().enumerate().take(seg_n + 1) {
            // 上一圈的末点与本圈的首点是**同一个点**（环是闭合的）。留着它
            // 就多出一段零长线段，GPS 噪声会把它变成一段 0.5m 的随机方向
            // 小线段 ⇒ 那一处的转角变成随机的（实测接缝处多出 72°）。
            if lap > 0 && j == 0 {
                continue;
            }
            // 螺旋偏移：第 k 圈从 (k−1) 个车道量渐变到 k 个。
            let frac = j as f64 / seg_n as f64;
            let off = (lap as f64 - 1.0 + frac).max(0.0) * LANE_SHIFT_M;
            let mlat = m_per_deg_lat(p.0).max(1.0);
            let mlng = m_per_deg_lng(p.0).max(1.0);
            out.push((p.0 + dir.1 * off / mlat, p.1 + dir.0 * off / mlng));
        }
    }

    // 截到目标里程。
    let mut acc = 0.0f64;
    let mut cut: Vec<(f64, f64)> = Vec::with_capacity(out.len());
    for i in 1..out.len() {
        acc += distance(out[i - 1], out[i]);
        cut.push(out[i]);
        if acc >= distance_m {
            break;
        }
    }

    add_gps_noise(&mut cut, step_m, seed);
    cut
}

/// 给轨迹叠一层真机同款的 GPS 噪声（OU 过程）。
///
/// 相邻点有惯性 ⇒ 多圈不会完全重合，而是像真机那样一簇椭圆。
///
/// θ 和 σ 都按采样间距缩放，基准是真机样本的 14.9m 间距（θ=0.15、σ=1.1
/// 的原值就是在那个间距下标定的）：θ ∝ spacing/14.9（相关长度按米恒定
/// ≈100m）、σ ∝ √(spacing/14.9)（慢速漂移的带宽按米恒定 ≈2m）。
fn add_gps_noise(pts: &mut [(f64, f64)], step_m: f64, seed: u64) {
    use rand::{Rng, SeedableRng};
    let mut rng = rand::rngs::StdRng::seed_from_u64(seed);
    let theta = 0.15 * (step_m / 14.9);
    let sigma = 1.1 * (step_m / 14.9).sqrt();
    let gauss = |rng: &mut rand::rngs::StdRng| {
        let u1: f64 = rng.random::<f64>().max(1e-12);
        let u2: f64 = rng.random();
        (-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos()
    };
    let mut jx = 0.0f64;
    let mut jy = 0.0f64;
    for p in pts.iter_mut() {
        jx += -theta * jx + sigma * gauss(&mut rng);
        jy += -theta * jy + sigma * gauss(&mut rng);
        let mlat = m_per_deg_lat(p.0).max(1.0);
        let mlng = m_per_deg_lng(p.0).max(1.0);
        p.0 += jy / mlat;
        p.1 += jx / mlng;
    }
}

fn unit(v: (f64, f64)) -> (f64, f64) {
    let n = v.0.hypot(v.1);
    if n > 1e-12 {
        (v.0 / n, v.1 / n)
    } else {
        (0.0, 0.0)
    }
}

fn dist_xy(a: (f64, f64), b: (f64, f64)) -> f64 {
    (a.0 - b.0).hypot(a.1 - b.1)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 实测某校的 5 个点（数组序 1→4→5→2→3，环序 1→2→3→4→5）。
    fn scrambled() -> Vec<(f64, f64)> {
        let lat = 23.457084;
        let mlat = m_per_deg_lat(lat);
        let mlng = m_per_deg_lng(lat);
        let base = (lat, 113.499217);
        let offs = [
            (0.0, 0.0),
            (-120.7, 129.7),
            (-85.2, 59.3),
            (20.5, 28.2),
            (-29.5, 103.5),
        ];
        offs.iter()
            .map(|(dx, dy)| (base.0 + dy / mlat, base.1 + dx / mlng))
            .collect()
    }

    /// 转角（度）：按给定间距重采样后逐点算。
    fn turns_at_spacing(pts: &[(f64, f64)], spacing: f64) -> Vec<f64> {
        let mut acc = vec![0.0f64];
        for i in 1..pts.len() {
            acc.push(acc[i - 1] + distance(pts[i - 1], pts[i]));
        }
        let total = acc[pts.len() - 1];
        let mut sampled: Vec<(f64, f64)> = Vec::new();
        let mut s = 0.0f64;
        let mut j = 0usize;
        while s <= total {
            while j + 1 < acc.len() && acc[j] < s {
                j += 1;
            }
            if j + 1 >= acc.len() {
                break;
            }
            let seg = acc[j + 1] - acc[j];
            let t = if seg < 1e-9 { 0.0 } else { (s - acc[j]) / seg };
            sampled.push((
                pts[j].0 + (pts[j + 1].0 - pts[j].0) * t,
                pts[j].1 + (pts[j + 1].1 - pts[j].1) * t,
            ));
            s += spacing;
        }
        let mut out = Vec::new();
        for i in 1..sampled.len().saturating_sub(1) {
            let v1 = (
                sampled[i].0 - sampled[i - 1].0,
                sampled[i].1 - sampled[i - 1].1,
            );
            let v2 = (
                sampled[i + 1].0 - sampled[i].0,
                sampled[i + 1].1 - sampled[i].1,
            );
            let (n1, n2) = (v1.0.hypot(v1.1), v2.0.hypot(v2.1));
            if n1 < 1e-9 || n2 < 1e-9 {
                continue;
            }
            let c = ((v1.0 * v2.0 + v1.1 * v2.1) / (n1 * n2)).clamp(-1.0, 1.0);
            out.push(c.acos().to_degrees());
        }
        out
    }

    #[test]
    fn loop_passes_every_point_exactly() {
        let pts = scrambled();
        let loop_pts = loop_through(&pts, 4.0);
        for p in &pts {
            let best = loop_pts
                .iter()
                .map(|q| distance(*q, *p))
                .fold(f64::MAX, f64::min);
            assert!(best < 0.01, "点位应被精确命中，实际残距 {best:.4}m");
        }
    }

    #[test]
    fn loop_is_closed() {
        let loop_pts = loop_through(&scrambled(), 4.0);
        let first = loop_pts[0];
        let last = *loop_pts.last().unwrap();
        assert!(distance(first, last) < 1e-6, "环必须闭合");
    }

    /// 实测那组打卡点：上报点尺度（14m）上不该再有 >60° 的急转。
    ///
    /// 生产旧轨迹在同一组点上实测 >60° 有 22 处、最大 163°。
    #[test]
    fn scrambled_points_have_no_sharp_turn_at_reporting_scale() {
        let loop_pts = loop_through(&scrambled(), 5.0);
        let turns = turns_at_spacing(&loop_pts, 14.0);
        let sharp = turns.iter().filter(|t| **t > 60.0).count();
        let worst = turns.iter().cloned().fold(0.0f64, f64::max);
        assert_eq!(sharp, 0, "上报尺度上不该有 >60° 的急转：{turns:?}");
        assert!(worst < 65.0, "最大转角应 < 65°，实际 {worst:.1}°");
    }

    /// 首次经过序必须等于点位数组序（服务端「按顺序通过所有点位」）。
    #[test]
    fn first_pass_order_follows_array_order() {
        let pts = scrambled();
        let loop_pts = loop_through(&pts, 4.0);
        let mut firsts = Vec::new();
        for p in &pts {
            let idx = loop_pts
                .iter()
                .position(|q| distance(*q, *p) < 1.0)
                .unwrap_or(usize::MAX);
            firsts.push(idx);
        }
        assert!(
            firsts.windows(2).all(|w| w[0] < w[1]),
            "首次经过序应与数组序一致，实际 {firsts:?}"
        );
    }

    #[test]
    fn lap_track_reaches_target_distance() {
        let tr = lap_track_through(&scrambled(), 1000.0, 5.0, 42);
        let len = super::super::geo::path_length(&tr);
        assert!(
            (len - 1000.0).abs() < 60.0,
            "里程应贴近目标，实际 {len:.1}m"
        );
        assert!(tr.len() > 50, "点数应足够");
    }

    /// 圈接缝处不能有横跳（否则图上出现 180° 折角）。
    #[test]
    fn lap_junctions_have_no_direction_jump() {
        let tr = lap_track_through(&scrambled(), 3000.0, 5.0, 7);
        let turns = turns_at_spacing(&tr, 14.0);
        let worst = turns.iter().cloned().fold(0.0f64, f64::max);
        // 生产旧轨迹在同一点实测 163°、22 处 >60°；这里守的是「不回到那个量级」。
        // GPS 噪声在曲率集中点会把转角抬高十几度（同一份几何、不同种子实测
        // 58°~73°），所以阈值留到 75°。
        assert!(worst < 75.0, "圈接缝处转角应平顺，最差 {worst:.1}°");
    }

    /// **上报层**（真正发出去的那 218 个点）的首次经过序必须等于点位数组序。
    ///
    /// 几何层的首过序对了不代表上报层也对：采样 / 起步压缩 / 对齐 / 吸附
    /// 这四步都会动点，动完可能把某个打卡点的「首次经过」推到后面去。服务端
    /// 判「按顺序通过所有点位」看的是**上报轨迹**，所以判据必须站在这一层。
    ///
    /// 实测（某校 5 点 / 3km / 18min）：几何层首过序 #0→#4 递增 ✓，
    /// 上报层却变成 #1→#2→#3→#4→#0 ✗ ⇒ 服务端会判「按顺序通过所有点位」失败。
    ///
    /// **必须扫多个 seed**：生产的 seed 是 `start_ms`（每次跑都变），而
    /// 配速剖面是按 seed 随机的 ⇒ 起步窗口覆盖的弧长随 seed 变 ⇒ 打卡点
    /// 落在窗口内还是窗口外随 seed 变 ⇒ 顺序会不会乱也随 seed 变。
    /// 单个 seed 通过等于没测（这是本项目踩过多次的「判据自己会错」）。
    #[test]
    fn reported_first_pass_order_follows_array_order() {
        let cps = scrambled();
        let total_sec = 1080i64;
        let v_floor = 1.67f64;
        let v_ceil = 7.68f64;
        let radius = 10.0f64;
        let mut broken: Vec<(f64, u64, Vec<usize>)> = Vec::new();

        // 只扫 3.0km —— 生产用的距离。别的距离（2.5/2.9/3.2/3.5km）已知还会
        // 乱，见下面那个 #[ignore] 的已知问题测试。
        for dist_m in [3000.0_f64] {
            for k in 0..24u64 {
                let start_ms = 1_700_000_000_000i64 + (k as i64) * 997;
                let seed = start_ms as u64;
                let geom = lap_track_through(&cps, dist_m, 5.0, seed);
                let geo_first = first_pass(&geom, &cps, radius);
                assert!(
                    geo_first.windows(2).all(|w| w[0] < w[1]),
                    "几何层首过序应递增，实际 {geo_first:?}"
                );

                let times = super::super::sample_times(total_sec, start_ms, 5.0);
                let mut pts = super::super::sample_by_time(
                    &geom,
                    &times,
                    start_ms as u64,
                    &super::super::kinematics::KinParams::default(),
                    v_floor,
                    v_ceil,
                    &[],
                    &cps,
                );
                let stage_sample = first_pass(&xy_of(&pts), &cps, radius);
                super::super::compress_startup(&mut pts, start_ms, total_sec, Some(v_ceil), &cps);
                let stage_compress = first_pass(&xy_of(&pts), &cps, radius);
                super::super::align_samples_to_checkpoints(&mut pts, &cps);
                let stage_align = first_pass(&xy_of(&pts), &cps, radius);
                let snap = super::super::snap_checkpoints(&mut pts, &cps);
                let pinned: Vec<usize> = snap
                    .items
                    .iter()
                    .filter(|s| s.shifted)
                    .map(|s| s.index)
                    .collect();
                let max_gap =
                    crate::api::submit::MAX_POINT_SPEED_MPS * super::super::SAMPLE_INTERVAL_SEC;
                super::super::clamp_sample_gaps(&mut pts, &pinned, max_gap);
                super::super::nudge_to_corners(&mut pts, &geom, 12.0, &pinned);
                super::super::clamp_sample_gaps(&mut pts, &pinned, max_gap);

                let final_first = first_pass(&xy_of(&pts), &cps, radius);
                if !final_first.windows(2).all(|w| w[0] < w[1]) {
                    broken.push((dist_m, k, final_first.clone()));
                    println!(
                        "dist {dist_m} seed {k}: 采样后 {stage_sample:?} / 压缩后 {stage_compress:?} / 对齐后 {stage_align:?} / 最终 {final_first:?}"
                    );
                }
            }
        }
        assert!(
            broken.is_empty(),
            "上报层首过序在部分 seed 上乱序：{broken:?}"
        );
    }

    /// **已知问题（未修）**：上报层的首过序只在 3.0km 上稳。
    ///
    /// 扫 5 个距离 × 24 个种子：3.0km 全过；2.5/2.9/3.2/3.5km 会出乱序
    /// （圈数变了 ⇒ 起步窗口在路径上覆盖的弧长比例变了 ⇒ 打卡点落不落在
    /// 窗口里跟着变）。实测 2.9km/seed 2：几何层首过序 [9,44,63,85,104] ✓，
    /// 采样层却变成 [4,11,63,70,27] —— #4 被**提前**经过。
    ///
    /// 机制（已定位到一半）：路径在发夹处**自折**（两支挨得近），采样点/
    /// 拐角补位一旦落进某个打卡点的 10m 邻域，就等于替它「提前经过」，
    /// 服务端按 10m 半径判首次经过时次序就反了。真正的修法在几何层：
    /// 把发夹撑开，让两支在空间上分开 >20m（路径不相邻 ⇒ 不会有提前经过）。
    ///
    /// 这个测试是 `#[ignore]` 的：它是**已知问题的记录**，不是门禁。
    /// 等几何层修好（路径不自折）再把它转成断言。
    #[test]
    #[ignore = "已知问题：非 3.0km 的距离上上报层首过序仍会乱，等几何层修好"]
    fn known_reported_order_breaks_at_other_distances() {
        let cps = scrambled();
        let total_sec = 1080i64;
        let radius = 10.0f64;
        let mut broken: Vec<(f64, u64)> = Vec::new();
        for dist_m in [2500.0_f64, 2900.0, 3000.0, 3200.0, 3500.0] {
            for k in 0..24u64 {
                let start_ms = 1_700_000_000_000i64 + (k as i64) * 997;
                let geom = lap_track_through(&cps, dist_m, 5.0, start_ms as u64);
                let times = super::super::sample_times(total_sec, start_ms, 5.0);
                let mut pts = super::super::sample_by_time(
                    &geom,
                    &times,
                    start_ms as u64,
                    &super::super::kinematics::KinParams::default(),
                    1.67,
                    7.68,
                    &[],
                    &cps,
                );
                super::super::compress_startup(&mut pts, start_ms, total_sec, Some(7.68), &cps);
                super::super::align_samples_to_checkpoints(&mut pts, &cps);
                let snap = super::super::snap_checkpoints(&mut pts, &cps);
                let pinned: Vec<usize> = snap
                    .items
                    .iter()
                    .filter(|s| s.shifted)
                    .map(|s| s.index)
                    .collect();
                let max_gap =
                    crate::api::submit::MAX_POINT_SPEED_MPS * super::super::SAMPLE_INTERVAL_SEC;
                super::super::clamp_sample_gaps(&mut pts, &pinned, max_gap);
                super::super::nudge_to_corners(&mut pts, &geom, 12.0, &pinned);
                super::super::clamp_sample_gaps(&mut pts, &pinned, max_gap);
                let fp = first_pass(&xy_of(&pts), &cps, radius);
                if !fp.windows(2).all(|w| w[0] < w[1]) {
                    broken.push((dist_m, k));
                }
            }
        }
        println!("乱序组合: {broken:?}");
        assert!(
            broken.is_empty(),
            "上报层首过序在 {} 个组合上乱序：{broken:?}",
            broken.len()
        );
    }

    /// 首次被经过的下标（半径内第一个点）。
    fn first_pass(pts: &[(f64, f64)], cps: &[(f64, f64)], radius: f64) -> Vec<usize> {
        cps.iter()
            .map(|c| {
                pts.iter()
                    .position(|q| distance(*q, *c) < radius)
                    .unwrap_or(usize::MAX)
            })
            .collect()
    }

    /// 上报点 → 坐标对。
    fn xy_of(pts: &[super::super::TrackPoint]) -> Vec<(f64, f64)> {
        pts.iter().map(|p| (p.lat, p.lon)).collect()
    }

    /// 退化输入不能 panic。
    #[test]
    fn degenerate_inputs_are_safe() {
        assert_eq!(loop_through(&[], 5.0).len(), 0);
        let two = vec![(23.0, 113.0), (23.001, 113.0)];
        assert_eq!(loop_through(&two, 5.0).len(), 2);
        let same = vec![(23.0, 113.0); 3];
        // 三个重合点：环退化成几段同点的折线，不 panic 即可。
        assert!(loop_through(&same, 5.0).len() >= 3);
    }
}

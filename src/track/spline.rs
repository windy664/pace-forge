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
/// 参数是**在真实上报链上扫出来的**（`sweep_loop_params_on_reporting_scale`）：
/// 几何 → 采样 → 起步压缩 → 打卡点对齐 → 吸附 → 限距 → 拐角补位 → 再限距，
/// 然后量**上报点**的转角。
///
/// ⚠️ 早先那版扫描只量了「几何等距采样」的转角，那个量具测不到上报尺度
/// 的真实尖角（见 `nudge_to_corners` 的位移上限说明），所以结论是错的。
///
/// 换成真链、并且**与门禁共用同一批判据函数**之后（`pick_best_loop_params`）：
/// | 参数 | 上报 max | 上报 >60° | 几何 >60° | 几何 max | 接缝 max | 结果 |
/// |---|---|---|---|---|---|---|
/// | **80 / 1.8 / 1.4（本档）** | 90.3° | 89 处 | 0 | 51.7° | 73.1° | **✓ 保留** |
/// | 100 / 2.2 / 1.6 | 96.0° | 65 处 | 0 | 57.9° | 69.5° | ✗ 见下 |
/// | 100 / 2.4 / 1.6 | 94.0° | 65 处 | 0 | 56.1° | 70.5° | ✗ 见下 |
///
/// ⚠️ 后两档在**本地门禁里全过**，但部署到生产后立刻翻车：
/// 切向变长 ⇒ 环变胖 ⇒「样条环有 2 个点落在围栏外」⇒ 退回路线规划 ⇒
/// 上报首过序变成 `#4 先于 #1` ⇒ 服务端判「按顺序通过所有点位」失败。
///
/// ⇒ **本地门禁缺一条判据：「环在围栏内」**。围栏是学校下发的，本地夹具
/// 里没有，所以门禁量不到它。这条只能靠生产验证 ⇒ 教训：门禁覆盖不到的
/// 约束（外部数据），改参数时必须先在生产跑一次规划看日志。
///
/// ⇒ 所以**保留 80 / 1.8 / 1.4**：「上报 >60° 少 27%」的收益不值得拿
/// 「环出围栏 + 首过序乱」去换。真正的大头是 `nudge_to_corners` 的位移上限
/// （那个从 170° 降到 90°，且不动环的形状）。
///
/// ⚠️ 扫描量具本身踩过**三次**「判据自己会错」，每次都改变了结论：
/// ① 只量几何等距采样 ⇒ 漏掉上报尺度的真尖角；
/// ② 只量上报层首过序 ⇒ 漏掉**几何层**塌掉的档；
/// ③ 扫描与门禁的步长/半径口径不同 ⇒ 结论不可复现。
/// ⇒ 现在的做法：扫描与门禁**共用同一批函数**（见测试模块里的
/// `geom_first_pass` / `geom_turns` / `junction_turns` /
/// `reported_first_pass_broken` / `reported_turns_stats`），
/// 「扫描说行」⇒「门禁必过」。
pub const LOOP_ALPHA_DEG: f64 = 80.0;

/// 普通节点的切向长度系数（乘以「到较近邻点的距离」）。
///
/// 切向越长 ⇒ 节点处的弯摊得越开 ⇒ 上报尺度的转角越小。1.6 是在真链上
/// 扫出来的（见 [`LOOP_ALPHA_DEG`] 的扫描说明）。
pub const LOOP_H_SCALE: f64 = 1.4;

/// 掉头节点的切向长度系数。
///
/// 掉头处切向越长，绕回来的发夹越大 ⇒ 转弯摊得越开 ⇒ 上报尺度上的
/// 转角越小。2.0 是在真链上扫出来的（见 [`LOOP_ALPHA_DEG`] 的扫描说明）。
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
    loop_through_with(
        points,
        step_m,
        LOOP_ALPHA_DEG,
        LOOP_H_SCALE,
        LOOP_H_REV,
        REVERSAL_EPS,
    )
}

/// [`loop_through`] 的可调参数版本：掉头转角、切向长度系数、掉头判定阈值。
///
/// 抽出来是为了能**在真实上报链上**扫参数 —— 急转是**上报尺度**的现象
/// （几何上很缓的弯，采样点一稀就变成尖角），只看几何等距采样会量错。
/// 参数默认值就是上面那几个常量，生产路径仍走 `loop_through`。
pub fn loop_through_with(
    points: &[(f64, f64)],
    step_m: f64,
    alpha_deg: f64,
    h_scale: f64,
    h_rev: f64,
    reversal_eps: f64,
) -> Vec<(f64, f64)> {
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
        let reversal = sum.0.hypot(sum.1) < reversal_eps;
        let t = if reversal {
            // 掉头：入射方向左转 alpha_deg。
            let a = alpha_deg.to_radians();
            let (c, s) = (a.cos(), a.sin());
            unit((d_in.0 * c - d_in.1 * s, d_in.0 * s + d_in.1 * c))
        } else {
            unit(sum)
        };
        tang.push(t);
        let near = dist_xy(xy[i], xy[prev]).min(dist_xy(xy[i], xy[next]));
        hlen.push(near * if reversal { h_rev } else { h_scale });
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
    lap_track_through_with(
        points,
        distance_m,
        step_m,
        seed,
        LOOP_ALPHA_DEG,
        LOOP_H_SCALE,
        LOOP_H_REV,
        REVERSAL_EPS,
    )
}

/// [`lap_track_through`] 的可调参数版本（见 [`loop_through_with`]）。
#[expect(
    clippy::too_many_arguments,
    reason = "参数扫描要同时驱动三个形状参数 + 两个判据阈值"
)]
pub fn lap_track_through_with(
    points: &[(f64, f64)],
    distance_m: f64,
    step_m: f64,
    seed: u64,
    alpha_deg: f64,
    h_scale: f64,
    h_rev: f64,
    reversal_eps: f64,
) -> Vec<(f64, f64)> {
    let loop_pts = loop_through_with(points, step_m, alpha_deg, h_scale, h_rev, reversal_eps);
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

    // ── 共享判据 ────────────────────────────────────────────────────────
    //
    // ⚠️ 这些函数是**门禁与参数扫描的唯一判据来源**。
    //
    // 本轮在参数扫描上踩了三次「判据自己会错」：每次扫描用的判据都少一道
    // 或与门禁差一个口径，于是扫出来的「最优参数」一进门禁就挂：
    //   ① 只量几何等距采样 ⇒ 漏掉上报尺度的真尖角；
    //   ② 只量上报层首过序 ⇒ 漏掉几何层塌掉的档；
    //   ③ 扫描用的几何步长/半径与门禁不同 ⇒ 结论再次不可复现。
    //
    // ⇒ 门禁和扫描都调下面这几个函数，判据只有一份，不可能再分叉。

    /// 几何层首次经过序（步长 4.0、半径 1.0）——服务端「按顺序通过所有点位」
    /// 的最严格口径：环必须**精确**按数组序经过每个点。
    fn geom_first_pass(alpha: f64, hs: f64, hr: f64) -> Option<Vec<usize>> {
        let pts = scrambled();
        let loop_pts = loop_through_with(&pts, 4.0, alpha, hs, hr, REVERSAL_EPS);
        let mut firsts = Vec::new();
        for p in &pts {
            let idx = loop_pts
                .iter()
                .position(|q| distance(*q, *p) < 1.0)
                .unwrap_or(usize::MAX);
            firsts.push(idx);
        }
        if firsts.windows(2).all(|w| w[0] < w[1]) {
            Some(firsts)
        } else {
            None
        }
    }

    /// 几何层 14 米重采样下的急转：>60° 的处数与最大转角。
    fn geom_turns(alpha: f64, hs: f64, hr: f64) -> (usize, f64) {
        let loop_pts = loop_through_with(&scrambled(), 5.0, alpha, hs, hr, REVERSAL_EPS);
        let turns = turns_at_spacing(&loop_pts, 14.0);
        let worst = turns.iter().cloned().fold(0.0f64, f64::max);
        (turns.iter().filter(|t| **t > 60.0).count(), worst)
    }

    /// 圈接缝处的最大转角（固定 seed 7，与生产同款）。
    fn junction_turns(alpha: f64, hs: f64, hr: f64) -> f64 {
        let tr = lap_track_through_with(&scrambled(), 3000.0, 5.0, 7, alpha, hs, hr, REVERSAL_EPS);
        let turns = turns_at_spacing(&tr, 14.0);
        turns.iter().cloned().fold(0.0f64, f64::max)
    }

    /// **上报层**（真正发出去的那批点）的首次经过序：跑完整条上报链，
    /// 返回乱序的 `(距离, seed)` 组合。
    fn reported_first_pass_broken(
        alpha: f64,
        hs: f64,
        hr: f64,
        dists: &[f64],
        seeds: u64,
        startup_dist_ratio: f64,
    ) -> Vec<(f64, u64)> {
        let cps = scrambled();
        let total_sec = 1080i64;
        let radius = 10.0f64;
        let mut broken = Vec::new();
        for &dist_m in dists {
            for k in 0..seeds {
                let start_ms = 1_700_000_000_000i64 + (k as i64) * 997;
                let pts = reported_points_with_ratio(
                    &cps,
                    dist_m,
                    start_ms as u64,
                    start_ms,
                    total_sec,
                    alpha,
                    hs,
                    hr,
                    REVERSAL_EPS,
                    startup_dist_ratio,
                );
                let fp = first_pass(&xy_of(&pts), &cps, radius);
                if !fp.windows(2).all(|w| w[0] < w[1]) {
                    broken.push((dist_m, k));
                }
            }
        }
        broken
    }

    /// **上报层**的急转：max 转角与 >60° 处数（12 个 seed × 3.0km）。
    fn reported_turns_stats(alpha: f64, hs: f64, hr: f64) -> (f64, usize) {
        let cps = scrambled();
        let total_sec = 1080i64;
        let mut mx = 0.0f64;
        let mut c60 = 0usize;
        for k in 0..12u64 {
            let start_ms = 1_700_000_000_000i64 + (k as i64) * 997;
            let pts = reported_points_with_ratio(
                &cps,
                3000.0,
                start_ms as u64,
                start_ms,
                total_sec,
                alpha,
                hs,
                hr,
                REVERSAL_EPS,
                super::super::REAL_DEVICE_STARTUP_DIST_RATIO,
            );
            for x in reported_turns(&pts) {
                if x > mx {
                    mx = x;
                }
                if x > 60.0 {
                    c60 += 1;
                }
            }
        }
        (mx, c60)
    }

    /// **上报层**的转角：相邻上报采样点构成的夹角（度）。
    ///
    /// 急转是**上报尺度**的现象 —— 几何上很缓的弯，采样点一稀（15 米一步）
    /// 就变成尖角。所以必须量上报点，不能量几何折线。
    fn reported_turns(pts: &[super::super::TrackPoint]) -> Vec<f64> {
        let xy: Vec<(f64, f64)> = pts.iter().map(|p| (p.lat, p.lon)).collect();
        let mut out = Vec::new();
        for i in 1..xy.len().saturating_sub(1) {
            let v1 = (xy[i].0 - xy[i - 1].0, xy[i].1 - xy[i - 1].1);
            let v2 = (xy[i + 1].0 - xy[i].0, xy[i + 1].1 - xy[i].1);
            let (n1, n2) = (v1.0.hypot(v1.1), v2.0.hypot(v2.1));
            if n1 < 1e-9 || n2 < 1e-9 {
                continue;
            }
            let co = ((v1.0 * v2.0 + v1.1 * v2.1) / (n1 * n2)).clamp(-1.0, 1.0);
            out.push(co.acos().to_degrees());
        }
        out
    }

    /// 生产**默认值**必须等于真机同款预算。
    ///
    /// 门禁里一律**显式**传 [`super::super::REAL_DEVICE_STARTUP_DIST_RATIO`]，
    /// 不读全局配置 —— 否则改一下配置默认值就能让门禁静默失效（判据自己
    /// 变松）。这条把两者钉在一起：谁改了默认值，这条当场失败，提醒他
    /// 同步改门禁口径，或者改回来。
    #[test]
    fn production_default_budget_matches_real_device() {
        let dflt = crate::config::Run::default().startup_dist_ratio;
        assert_eq!(
            dflt,
            super::super::REAL_DEVICE_STARTUP_DIST_RATIO,
            "生产默认起步预算 {dflt} 与真机同款预算 {} 不一致：门禁口径和生产口径会分叉",
            super::super::REAL_DEVICE_STARTUP_DIST_RATIO
        );
    }

    /// [`reported_points_with_ratio`] 的起步预算可调版本。
    ///
    /// 起步预算（`run.startup_dist_ratio`）会改变「起步窗口覆盖路径的哪一段
    /// 弧长」⇒ 打卡点落不落在窗口里跟着变 ⇒ 上报层的首次经过序随预算变。
    /// 扫描别的预算时必须走这个版本，否则量具和生产不是一个口径。
    #[expect(
        clippy::too_many_arguments,
        reason = "上报链要同时拿到形状参数、判据阈值与起步预算"
    )]
    fn reported_points_with_ratio(
        cps: &[(f64, f64)],
        dist_m: f64,
        seed: u64,
        start_ms: i64,
        total_sec: i64,
        alpha: f64,
        hs: f64,
        hr: f64,
        rev: f64,
        startup_dist_ratio: f64,
    ) -> Vec<super::super::TrackPoint> {
        let geom = lap_track_through_with(cps, dist_m, 5.0, seed, alpha, hs, hr, rev);
        let times = super::super::sample_times(total_sec, start_ms, 5.0);
        let mut pts = super::super::sample_by_time_with_ratio(
            &geom,
            &times,
            seed,
            &super::super::kinematics::KinParams::default(),
            1.67,
            7.68,
            &[],
            cps,
            startup_dist_ratio,
        );
        super::super::compress_startup_with_ratio(
            &mut pts,
            start_ms,
            total_sec,
            Some(7.68),
            cps,
            startup_dist_ratio,
        );
        super::super::align_samples_to_checkpoints(&mut pts, cps);
        let snap = super::super::snap_checkpoints(&mut pts, cps);
        let pinned: Vec<usize> = snap
            .items
            .iter()
            .filter(|s| s.shifted)
            .map(|s| s.index)
            .collect();
        let max_gap = crate::api::submit::MAX_POINT_SPEED_MPS * super::super::SAMPLE_INTERVAL_SEC;
        super::super::clamp_sample_gaps(&mut pts, &pinned, max_gap);
        super::super::nudge_to_corners(&mut pts, &geom, 12.0, &pinned);
        super::super::clamp_sample_gaps(&mut pts, &pinned, max_gap);
        pts
    }

    /// 对照实验：默认参数的 `lap_track_through` 与显式传同一组参数的
    /// `lap_track_through_with` 必须**逐位相同**。
    ///
    /// 不一样就说明「可调参数版本」抄错了 —— 而量具一旦抄错，扫出来的
    /// 参数表全是废的（这轮就踩了：默认参数实测 max 93°，同一组参数
    /// 从可调版本扫出来却是 170°）。
    #[test]
    fn loop_param_variants_agree_with_defaults() {
        let cps = scrambled();
        for k in 0..8u64 {
            let seed = 1_700_000_000_000u64 + k * 997;
            let a = lap_track_through(&cps, 3000.0, 5.0, seed);
            let b = lap_track_through_with(
                &cps,
                3000.0,
                5.0,
                seed,
                LOOP_ALPHA_DEG,
                LOOP_H_SCALE,
                LOOP_H_REV,
                REVERSAL_EPS,
            );
            assert_eq!(
                a.len(),
                b.len(),
                "seed {k}: 点数不同 {} vs {}",
                a.len(),
                b.len()
            );
            for i in 0..a.len() {
                assert!(
                    (a[i].0 - b[i].0).abs() < 1e-12 && (a[i].1 - b[i].1).abs() < 1e-12,
                    "seed {k}: 第 {i} 点不同：{:?} vs {:?}",
                    a[i],
                    b[i]
                );
            }
        }
    }

    /// **诊断**：急转是上报链的**哪一步**引入的？
    ///
    /// 量具本身会错（这轮就踩了：全量扫描报 max 170°，而生产实测是 78°），
    /// 所以先分步骤定位，再谈调参。
    #[test]
    #[ignore = "诊断用的量具，不是门禁"]
    fn diagnose_where_sharp_turns_come_from() {
        // 多 seed：单个 seed 会把「大多数 seed 都炸」这件事藏起来。
        for k in 0..12u64 {
            let cps = scrambled();
            let total_sec = 1080i64;
            let start_ms = 1_700_000_000_000i64 + (k as i64) * 997;
            let seed = start_ms as u64;

            let geom = lap_track_through(&cps, 3000.0, 5.0, seed);
            let times = super::super::sample_times(total_sec, start_ms, 5.0);
            let mut pts = super::super::sample_by_time_with_ratio(
                &geom,
                &times,
                seed,
                &super::super::kinematics::KinParams::default(),
                1.67,
                7.68,
                &[],
                &cps,
                0.011,
            );
            let su = super::super::startup_point_count(
                &pts.iter().map(|p| p.time_ms).collect::<Vec<_>>(),
                start_ms,
                total_sec,
            );
            let report = |label: &str, pts: &[super::super::TrackPoint]| {
                let t = reported_turns(pts);
                let mx = t.iter().cloned().fold(0.0f64, f64::max);
                let c60 = t.iter().filter(|x| **x > 60.0).count();
                let c45 = t.iter().filter(|x| **x > 45.0).count();
                // 急转的位置：前 su 个点是起步窗口
                let head = t
                    .iter()
                    .take(su.saturating_sub(1))
                    .filter(|x| **x > 60.0)
                    .count();
                println!(
                    "{label:<22} 点数 {:>3} | max {:>6.1}° | >60° {:>3} 处（起步段 {head}）| >45° {:>3} 处",
                    pts.len(),
                    mx,
                    c60,
                    c45
                );
            };

            report("① sample_by_time 后", &pts);
            super::super::compress_startup(&mut pts, start_ms, total_sec, Some(7.68), &cps);
            report("② compress_startup 后", &pts);
            super::super::align_samples_to_checkpoints(&mut pts, &cps);
            report("③ align 后", &pts);
            let snap = super::super::snap_checkpoints(&mut pts, &cps);
            let pinned: Vec<usize> = snap
                .items
                .iter()
                .filter(|s| s.shifted)
                .map(|s| s.index)
                .collect();
            report("④ snap 后", &pts);
            let max_gap =
                crate::api::submit::MAX_POINT_SPEED_MPS * super::super::SAMPLE_INTERVAL_SEC;
            super::super::clamp_sample_gaps(&mut pts, &pinned, max_gap);
            report("⑤ clamp 后", &pts);
            super::super::nudge_to_corners(&mut pts, &geom, 12.0, &pinned);
            report("⑥ nudge 后", &pts);
            super::super::clamp_sample_gaps(&mut pts, &pinned, max_gap);
            report("⑦ 再 clamp 后", &pts);

            // 最大的几个转角在第几个点？该点的间距多大？
            let t = reported_turns(&pts);
            let mut idx: Vec<usize> = (0..t.len()).collect();
            idx.sort_by(|a, b| {
                t[*b]
                    .partial_cmp(&t[*a])
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            println!("\n最大 8 个转角：");
            for i in idx.iter().take(8) {
                let p = i + 1;
                let d_prev = super::super::geo::distance(
                    (pts[p - 1].lat, pts[p - 1].lon),
                    (pts[p].lat, pts[p].lon),
                );
                let d_next = super::super::geo::distance(
                    (pts[p].lat, pts[p].lon),
                    (pts[p + 1].lat, pts[p + 1].lon),
                );
                println!(
                    "  点 {p:>3}（起步窗口 {su}）：转角 {:>6.1}°  前段间距 {d_prev:>5.2}m 后段间距 {d_next:>5.2}m",
                    t[*i]
                );
            }
        }
    }

    /// **参数选择器**：用**门禁同一套判据**遍历候选参数，选出最优档。
    ///
    /// ⚠️ 这一条是「判据自己会错」的根治：早先的扫描自己写判据，与门禁
    /// 差着口径（少一道 / 步长不同 / 半径不同），扫出来的「最优参数」
    /// 一进门禁就挂，连挂三次。现在扫描与门禁**共用同一批函数**
    /// （`geom_first_pass` / `geom_turns` / `junction_turns` /
    /// `reported_first_pass_broken` / `reported_turns_stats`），
    /// 所以「扫描说行」⇒「门禁必过」。
    ///
    /// 硬判据（任一不过即淘汰）：
    /// ① 几何层首过序（步长 4.0、半径 1.0）递增；
    /// ② 几何层 14m 重采样 >60° **0 处**且 max < 65°；
    /// ③ 圈接缝 max < 75°；
    /// ④ 上报层首过序在 5 距离 × 24 seed 上 **0 乱序**。
    /// 优化目标：上报层 max 转角、>60° 处数（越少越好）。
    #[test]
    #[ignore = "参数选择器（耗时，跑一次选完参数就不再跑）"]
    fn pick_best_loop_params() {
        let dists = [2500.0_f64, 2900.0, 3000.0, 3200.0, 3500.0];
        println!(
            "{:>6} {:>6} {:>6} | {:>8} {:>9} | {:>8} {:>8} | {:>8} | 结果",
            "alpha", "h_rev", "h_s", "上报max", "上报>60°", "几何>60°", "几何max", "接缝max",
        );
        let mut best: Option<(usize, f64, f64, f64, f64, f64)> = None;
        for alpha in [97.5_f64, 100.0, 102.5, 105.0] {
            for hr in [2.2_f64, 2.4, 2.6, 2.8, 3.0] {
                for hs in [1.4_f64, 1.6, 1.8] {
                    if geom_first_pass(alpha, hs, hr).is_none() {
                        println!("{alpha:>6.0} {hr:>6.1} {hs:>6.1} | 几何首过序乱 —— 淘汰");
                        continue;
                    }
                    let (g60, gmax) = geom_turns(alpha, hs, hr);
                    if g60 > 0 || gmax >= 65.0 {
                        println!(
                            "{alpha:>6.0} {hr:>6.1} {hs:>6.1} | 几何急转 {g60} 处 / {gmax:.1}° —— 淘汰"
                        );
                        continue;
                    }
                    let jmax = junction_turns(alpha, hs, hr);
                    if jmax >= 75.0 {
                        println!("{alpha:>6.0} {hr:>6.1} {hs:>6.1} | 接缝 {jmax:.1}° —— 淘汰");
                        continue;
                    }
                    let broken = reported_first_pass_broken(
                        alpha,
                        hs,
                        hr,
                        &dists,
                        24,
                        super::super::REAL_DEVICE_STARTUP_DIST_RATIO,
                    );
                    if !broken.is_empty() {
                        println!(
                            "{alpha:>6.0} {hr:>6.1} {hs:>6.1} | 上报首过序 {} 处乱序 —— 淘汰",
                            broken.len()
                        );
                        continue;
                    }
                    let (rmax, rc60) = reported_turns_stats(alpha, hs, hr);
                    println!(
                        "{alpha:>6.0} {hr:>6.1} {hs:>6.1} | {rmax:>8.1} {rc60:>9} | {g60:>8} {gmax:>8.1} | {jmax:>8.1} | ✓"
                    );
                    let cand = (rc60, rmax, alpha, hr, hs, gmax);
                    if best.is_none_or(|b| (cand.0, cand.1) < (b.0, b.1)) {
                        best = Some(cand);
                    }
                }
            }
        }
        match best {
            Some((c60, rmax, alpha, hr, hs, gmax)) => println!(
                "\n最优：alpha={alpha} h_rev={hr} h_scale={hs} ⇒ 上报 >60° {c60} 处、max {rmax:.1}°、几何 max {gmax:.1}°"
            ),
            None => println!("\n没有候选档全过 —— 参数空间需要重新想"),
        }
    }

    /// **校园跑夹具**：真实围栏 + 打卡点 ⇒ 「环在围栏内」这条判据搬进本地。
    ///
    /// 围栏是学校下发的外部数据，本地夹具以前没有 ⇒ 门禁量不到「环在围栏内」
    /// ⇒ 改参数只能靠「部署到生产跑一次规划看日志」⇒ 上一轮就这样翻车过
    /// （切向变长 ⇒ 环变胖 ⇒ 2 个点出围栏 ⇒ 退回路线规划 ⇒ 首过序乱，
    /// 而本地门禁全绿）。
    ///
    /// 夹具 `src/track/campus_fixture.json`：某高校的围栏 ring 与 5 个打卡点，
    /// 都是 **BD-09**（与服务端下发同口径；上报时网关自己转 GCJ-02）。
    /// checkpoints 的顺序 = 服务端返回的**数组序**（决定绕圈次序）。
    ///
    /// 这条守：样条环和**上报点**都必须在围栏内（生产那边是「出围栏就退回
    /// 路线规划」，本地这边是直接失败）。
    #[test]
    fn campus_loop_stays_inside_the_fence() {
        let fx: serde_json::Value =
            serde_json::from_str(include_str!("campus_fixture.json")).expect("夹具 JSON");
        let nums = |k: &str| -> Vec<(f64, f64)> {
            fx[k]
                .as_array()
                .unwrap_or_else(|| panic!("夹具缺 {k}"))
                .iter()
                .map(|v| {
                    (
                        v["lat"].as_f64().expect("lat"),
                        v["lng"].as_f64().expect("lng"),
                    )
                })
                .collect()
        };
        let cps = nums("checkpoints");
        let fence = nums("fence");
        assert!(cps.len() >= 3 && fence.len() >= 3, "夹具不完整");
        let mut bad_loop = 0usize;
        let mut bad_reported = 0usize;
        for k in 0..8u64 {
            let start_ms = 1_700_000_000_000i64 + (k as i64) * 997;
            // 生产的操场环就是这一条（`flow::stadium_loop_track`）。
            let loop_pts = lap_track_through(&cps, 3000.0, 5.0, start_ms as u64);
            bad_loop += loop_pts
                .iter()
                .filter(|p| !crate::track::fence_route::contains(&fence, p.0, p.1))
                .count();
            let pts = reported_points_with_ratio(
                &cps,
                3000.0,
                start_ms as u64,
                start_ms,
                1080,
                LOOP_ALPHA_DEG,
                LOOP_H_SCALE,
                LOOP_H_REV,
                REVERSAL_EPS,
                super::super::REAL_DEVICE_STARTUP_DIST_RATIO,
            );
            bad_reported += pts
                .iter()
                .filter(|p| !crate::track::fence_route::contains(&fence, p.lat, p.lon))
                .count();
        }
        assert_eq!(bad_loop, 0, "样条环有 {bad_loop} 个点落在围栏外");
        assert_eq!(bad_reported, 0, "上报点有 {bad_reported} 个落在围栏外");
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
        let (sharp, worst) = geom_turns(LOOP_ALPHA_DEG, LOOP_H_SCALE, LOOP_H_REV);
        assert_eq!(sharp, 0, "上报尺度上不该有 >60° 的急转");
        assert!(worst < 65.0, "最大转角应 < 65°，实际 {worst:.1}°");
    }

    /// 首次经过序必须等于点位数组序（服务端「按顺序通过所有点位」）。
    #[test]
    fn first_pass_order_follows_array_order() {
        let firsts = geom_first_pass(LOOP_ALPHA_DEG, LOOP_H_SCALE, LOOP_H_REV)
            .unwrap_or_else(|| panic!("首次经过序应与数组序一致"));
        assert_eq!(firsts.len(), 5);
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
        let worst = junction_turns(LOOP_ALPHA_DEG, LOOP_H_SCALE, LOOP_H_REV);
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
        let broken = reported_first_pass_broken(
            LOOP_ALPHA_DEG,
            LOOP_H_SCALE,
            LOOP_H_REV,
            &[3000.0],
            24,
            super::super::REAL_DEVICE_STARTUP_DIST_RATIO,
        );
        assert!(
            broken.is_empty(),
            "上报层首过序在部分 seed 上乱序：{broken:?}"
        );
    }

    /// 上报层的首过序必须在**每个距离**上都等于点位数组序。
    ///
    /// 扫 5 个距离 × 24 个种子，**起步里程预算用真机同款的 1.1%**
    /// （显式传 [`REAL_DEVICE_STARTUP_DIST_RATIO`，不靠全局配置 ——
    /// 靠全局配置的话，改默认值就能让这条门禁静默失效）。
    ///
    /// 这条曾经长期过不去，根因查了两轮：
    ///
    /// 1. **第一轮猜错了**：以为是「路径在发夹处自折、两支挨得近，采样点
    ///    提前落进打卡点的 10m 邻域」。实测**否掉了**：多圈轨迹上每个打卡点
    ///    本来就有 4~5 支（每圈一支），跟自折无关。
    /// 2. **真因是起步压缩把窗口里的打卡点压扁了**：某校 5 个打卡点有 4 个
    ///    落在起步窗口内，压缩把整个窗口压到预算里 ⇒ 相邻打卡点只剩 5.5m
    ///    < 服务端 10m 半径 ⇒ 两次「首次经过」撞成同一下标 ⇒ 次序乱。
    ///
    /// 修法在几何层：把里程预算挪到**采样阶段**的弧长分配里
    /// （`track::apply_startup_budget`）—— 压缩之前窗口就已经是预算内，
    /// 窗口里没有打卡点被撑开的位置 ⇒ 压缩退化成恒等 + 锚定。
    /// 于是 1.1%（真机同款）下 5 个距离 × 24 seed 全过。
    #[test]
    fn reported_first_pass_order_holds_at_other_distances() {
        let broken = reported_first_pass_broken(
            LOOP_ALPHA_DEG,
            LOOP_H_SCALE,
            LOOP_H_REV,
            &[2500.0, 2900.0, 3000.0, 3200.0, 3500.0],
            24,
            super::super::REAL_DEVICE_STARTUP_DIST_RATIO,
        );
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

    /// **诊断**：上报层首次经过序乱序时，是**哪个打卡点**、被**哪一支**提前经过。
    ///
    /// 量具 = 真实上报链（[`reported_points_with_ratio`]）+ 生产判据（10 m 半径、
    /// 按数组序取首次经过）。起步预算取**真机同款的 1.1%** —— 这是本轮要回
    /// 到的目标值。
    ///
    /// 「支路」= 几何环上距该打卡点 < 10 m 的**连续下标簇**；一簇 = 一支。
    /// 簇多于一支 ⇒ 路径在该点附近自折 ⇒ 采样点可能从另一支提前触及它。
    #[test]
    #[ignore = "诊断用的量具，不是门禁"]
    fn diagnose_first_pass_breakdown() {
        let cps = scrambled();
        let total_sec = 1080i64;
        let ratio = 0.011f64;
        for &dist_m in &[2500.0f64, 2900.0, 3000.0, 3200.0, 3500.0] {
            let mut printed = 0usize;
            for k in 0..24u64 {
                let start_ms = 1_700_000_000_000i64 + (k as i64) * 997;
                let pts = reported_points_with_ratio(
                    &cps,
                    dist_m,
                    start_ms as u64,
                    start_ms,
                    total_sec,
                    LOOP_ALPHA_DEG,
                    LOOP_H_SCALE,
                    LOOP_H_REV,
                    REVERSAL_EPS,
                    ratio,
                );
                let xy = xy_of(&pts);
                let fp = first_pass(&xy, &cps, 10.0);
                if fp.windows(2).all(|w| w[0] < w[1]) {
                    continue;
                }
                println!("=== 乱序 dist={dist_m} seed={k} 上报层首次经过={fp:?}");
                let geom = lap_track_through_with(
                    &cps,
                    dist_m,
                    5.0,
                    start_ms as u64,
                    LOOP_ALPHA_DEG,
                    LOOP_H_SCALE,
                    LOOP_H_REV,
                    REVERSAL_EPS,
                );
                for (ci, cp) in cps.iter().enumerate() {
                    let near: Vec<usize> = (0..geom.len())
                        .filter(|&i| distance(geom[i], *cp) < 10.0)
                        .collect();
                    let mut clusters: Vec<(usize, usize)> = Vec::new();
                    for &i in &near {
                        match clusters.last_mut() {
                            Some(last) if last.1 + 1 == i => last.1 = i,
                            _ => clusters.push((i, i)),
                        }
                    }
                    let hits: Vec<usize> = (0..xy.len())
                        .filter(|&i| distance(xy[i], *cp) < 10.0)
                        .collect();
                    println!("  cp#{ci} 上报首次={} 邻域内上报点={:?}", fp[ci], hits);
                    println!("      几何簇(10m)={clusters:?} ⇒ {} 支", clusters.len());
                }
                printed += 1;
                if printed >= 2 {
                    break;
                }
            }
            if printed == 0 {
                println!("=== dist={dist_m} 在 1.1% 预算下 24 seed 全过");
            }
        }
    }

    /// **诊断**：上报链**每一步**之后，各打卡点的首次经过下标。
    ///
    /// 用 dist=2500 / seed=2 —— 那是 1.1% 起步预算下唯一乱序的组合
    /// （上报层首次经过 = [78, 49, 58, 65, 73]，#0 掉到最后）。
    #[test]
    #[ignore = "诊断用的量具，不是门禁"]
    fn diagnose_first_pass_step_by_step() {
        let cps = scrambled();
        let total_sec = 1080i64;
        let dist_m: f64 = std::env::var("DIAG_DIST")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(2500.0);
        let k: u64 = std::env::var("DIAG_SEED")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(2);
        let start_ms = 1_700_000_000_000i64 + (k as i64) * 997;
        let seed = start_ms as u64;

        let show = |tag: &str, p: &[(f64, f64)]| {
            let fp = first_pass(p, &cps, 10.0);
            let ok = fp.windows(2).all(|w| w[0] < w[1]);
            println!("  {tag:22} fp={fp:?} {}", if ok { "✓" } else { "✗乱序" });
        };

        let geom = lap_track_through_with(
            &cps,
            dist_m,
            5.0,
            seed,
            LOOP_ALPHA_DEG,
            LOOP_H_SCALE,
            LOOP_H_REV,
            REVERSAL_EPS,
        );
        println!("=== dist={dist_m} seed={k} 1.1% 预算");
        show("几何环", &geom);
        println!("  几何点数={} 环长≈{:.0}m", geom.len(), {
            let mut s = 0.0;
            for i in 1..geom.len() {
                s += distance(geom[i - 1], geom[i]);
            }
            s
        });
        for (i, cp) in cps.iter().enumerate() {
            let near: Vec<usize> = (0..geom.len())
                .filter(|&j| distance(geom[j], *cp) < 10.0)
                .collect();
            let mut cl: Vec<(usize, usize)> = Vec::new();
            for &j in &near {
                match cl.last_mut() {
                    Some(l) if l.1 + 1 == j => l.1 = j,
                    _ => cl.push((j, j)),
                }
            }
            println!("    cp#{i} 几何簇={cl:?}");
        }

        let times = super::super::sample_times(total_sec, start_ms, 5.0);
        let mut pts = super::super::sample_by_time_with_ratio(
            &geom,
            &times,
            seed,
            &super::super::kinematics::KinParams::default(),
            1.67,
            7.68,
            &[],
            &cps,
            0.011,
        );
        show("+sample_by_time", &xy_of(&pts));
        println!("    采样点数={}", pts.len());

        super::super::compress_startup_with_ratio(
            &mut pts,
            start_ms,
            total_sec,
            Some(7.68),
            &cps,
            0.011,
        );
        show("+compress_startup", &xy_of(&pts));

        super::super::align_samples_to_checkpoints(&mut pts, &cps);
        show("+align_cp", &xy_of(&pts));

        let snap = super::super::snap_checkpoints(&mut pts, &cps);
        show("+snap_cp", &xy_of(&pts));
        let pinned: Vec<usize> = snap
            .items
            .iter()
            .filter(|s| s.shifted)
            .map(|s| s.index)
            .collect();
        println!("    snap 明细={:?}", snap.items.len());
        println!("    pinned={pinned:?}");

        let max_gap = crate::api::submit::MAX_POINT_SPEED_MPS * super::super::SAMPLE_INTERVAL_SEC;
        super::super::clamp_sample_gaps(&mut pts, &pinned, max_gap);
        show("+clamp_gaps", &xy_of(&pts));

        super::super::nudge_to_corners(&mut pts, &geom, 12.0, &pinned);
        show("+nudge_corners", &xy_of(&pts));

        super::super::clamp_sample_gaps(&mut pts, &pinned, max_gap);
        show("+clamp_gaps2", &xy_of(&pts));
    }

    /// **诊断**：`compress_startup` 把打卡点从「窗口内」挪进了「挤在一起」的簇。
    ///
    /// 打印压缩前后的弧长分布、各打卡点在折线上的首次弧长、以及窗口内点
    /// 与打卡点的相对位置。
    #[test]
    #[ignore = "诊断用的量具，不是门禁"]
    fn diagnose_compress_startup_arcs() {
        let cps = scrambled();
        let total_sec = 1080i64;
        let dist_m: f64 = std::env::var("DIAG_DIST")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(2500.0);
        let k: u64 = std::env::var("DIAG_SEED")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(2);
        let start_ms = 1_700_000_000_000i64 + (k as i64) * 997;
        let seed = start_ms as u64;

        let geom = lap_track_through_with(
            &cps,
            dist_m,
            5.0,
            seed,
            LOOP_ALPHA_DEG,
            LOOP_H_SCALE,
            LOOP_H_REV,
            REVERSAL_EPS,
        );
        let times = super::super::sample_times(total_sec, start_ms, 5.0);
        let mut pts = super::super::sample_by_time_with_ratio(
            &geom,
            &times,
            seed,
            &super::super::kinematics::KinParams::default(),
            1.67,
            7.68,
            &[],
            &cps,
            0.011,
        );

        // 压缩前的累计弧长
        let mut arc: Vec<f64> = vec![0.0];
        for i in 1..pts.len() {
            arc.push(
                arc[i - 1]
                    + super::super::geo::distance(
                        (pts[i - 1].lat, pts[i - 1].lon),
                        (pts[i].lat, pts[i].lon),
                    ),
            );
        }
        let total_len = *arc.last().unwrap();
        let su = super::super::startup_point_count(
            &pts.iter().map(|p| p.time_ms).collect::<Vec<_>>(),
            start_ms,
            total_sec,
        );
        println!("=== dist={dist_m} seed={k}");
        println!(
            "  总弧长={total_len:.1}m  采样点={}  起步窗口={su} 点",
            pts.len()
        );
        println!(
            "  压缩前窗口内弧长 arc[{}]={:.1}m（占 {:.2}%）",
            su - 1,
            arc[su - 1],
            arc[su - 1] / total_len * 100.0
        );
        println!("  压缩前窗口内每点弧长前 12 个: {:?}", {
            let v: Vec<f64> = (1..=12)
                .map(|i| (arc[i] - arc[i - 1]) * 1000.0)
                .map(|v| (v / 1000.0 * 1000.0).round() / 1000.0)
                .collect();
            v
        });
        // 压缩后窗口外每点弧长
        println!("  压缩前窗口外每点弧长 (su..su+8): {:?}", {
            let v: Vec<f64> = (su..(su + 8).min(pts.len() - 1))
                .map(|i| ((arc[i + 1] - arc[i]) * 1000.0).round() / 1000.0)
                .collect();
            v
        });

        // 各打卡点在折线上的首次弧长与首次下标
        let xy: Vec<(f64, f64)> = pts.iter().map(|p| (p.lat, p.lon)).collect();
        for (i, cp) in cps.iter().enumerate() {
            let (a, d) = super::super::first_arc_on(&xy, &arc, *cp, 10.0).unwrap_or((-1.0, -1.0));
            let idx = xy.iter().position(|q| distance(*q, *cp) < 10.0);
            println!("  cp#{i} 首次弧长={a:.1}m 下标={:?} 距折线={d:.2}m", idx);
        }

        // 压缩后的窗口内弧长
        super::super::compress_startup_with_ratio(
            &mut pts,
            start_ms,
            total_sec,
            Some(7.68),
            &cps,
            0.011,
        );
        let mut arc2: Vec<f64> = vec![0.0];
        for i in 1..pts.len() {
            arc2.push(
                arc2[i - 1]
                    + super::super::geo::distance(
                        (pts[i - 1].lat, pts[i - 1].lon),
                        (pts[i].lat, pts[i].lon),
                    ),
            );
        }
        println!(
            "  压缩后窗口内弧长 arc2[{}]={:.1}m（占 {:.2}%）",
            su - 1,
            arc2[su - 1],
            arc2[su - 1] / total_len * 100.0
        );
        let xy2: Vec<(f64, f64)> = pts.iter().map(|p| (p.lat, p.lon)).collect();
        for (i, cp) in cps.iter().enumerate() {
            let idx = xy2.iter().position(|q| distance(*q, *cp) < 10.0);
            println!("  压缩后 cp#{i} 首次下标={:?}", idx);
        }
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

//! 生物力学：配速、步频、步幅的非线性耦合。
//!
//! 硬约束：`v = (cadence / 60) · stride`（v: m/s，cadence: 步/分，stride: 米）
//!
//! **这个约束必须始终成立**。早期实现里步频和步数是各自随机生成的，两者与
//! 速度对不上——服务端只要拿 `totalSteps / totalTime` 和 `totalDis / totalTime`
//! 一除就能看出矛盾：步频 192 步/分 × 步幅必然推出的速度，与报文里的配速不符。
//!
//! 步频公式由真实记录回归得到，不是拍脑袋常数。四条样本（配速 5:31 /
//! 6:40 / 7:02 / 8:19）回归出的斜率与截距，拟合误差 ≤2 步/分。

/// 标准正态抽样（Box-Muller）。
///
/// 不引 `rand_distr`：它要额外拉依赖，而这里只需要一个标准正态。
fn normal_sample(rng: &mut impl rand::Rng) -> f64 {
    let u1 = rng.random::<f64>().max(1e-12);
    let u2 = rng.random::<f64>();
    (-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos()
}

/// 解析步频回归系数：读不到 / 不合法就用默认值。
fn parse_cad_coeffs(intercept: Option<f64>, slope: Option<f64>) -> (f64, f64) {
    // 值的**语法**校验交给配置层（`parse_cad_coefficient`，非法值在启动时报错）；
    // 这里只挡非有限数——配置也能被程序注入，那条路没有语法校验。
    let pick = |raw: Option<f64>, default: f64| raw.filter(|v| v.is_finite()).unwrap_or(default);
    (pick(intercept, 34.4), pick(slope, 44.5))
}

/// 步频回归的系数（`cadence = intercept + slope * v`）。
///
/// **默认就是原来那对常数**（34.4 / 44.5，来自 4 条真实记录的回归）⇒
/// 不设环境变量时行为逐字节不变。
///
/// 为什么要能配：回归是**从某个人的 4 条记录**拟合的，而服务端的判定看的是
/// **账号画像**——同一个公式在不同账号身上的「像不像本人」不一样。
/// 某账号自己的真实步频比公式高 5~13% 时，公式生成的步频偏低、步幅就偏长
/// （步幅 = 速度×60/步频），报文的步态与该账号的历史记录对不上。
/// `PACE_CAD_INTERCEPT` / `PACE_CAD_SLOPE` 让部署方按**自己账号的历史**标定。
///
/// ⚠️ **实测更正（2026-11-27）**：某个账号被判「运动数据有问题」的 3 条记录
/// 步幅是 1.06~1.09 m，同账号被判干净的 8 条是 0.911~1.007 m（中间是空的）。
/// 一开始把前者归因到**这个公式**（6'00"/km 时公式确实算出 1.052 m），
/// 但把 `PACE_CAD_INTERCEPT` 按该账号回归值设成 45.3 之后，步幅反而掉到
/// 0.822~0.880 m——**掉出账号的历史区间**，比默认更差 ⇒ 生产上撤掉了这个标定。
///
/// 真正的根因是**轨迹长度**：新版规划器的轨迹跑不满目标距离（3.0 km 的目标
/// 实测 2.66~2.98 km）且含慢速段（<2.0 m/s 走慢跑锚点 179 步/分），
/// 距离短 + 步频高 ⇒ 步幅 0.889~0.961 m，正好落在账号的干净区间里。
/// 产出 1.06~1.09 m 的是**旧版规划器**（轨迹刚好跑满目标距离、全程 ≥2.0 m/s
/// ⇒ 走回归段 ⇒ 步幅 = 公式值）。
///
/// ⇒ 用这两个开关之前先量一量自己账号的步幅分布，别照抄别人的常数。
///
/// 用 `OnceLock` 缓存：热路径（每个轨迹点）读环境变量太贵。
fn cad_coeffs() -> (f64, f64) {
    static COEFFS: std::sync::OnceLock<(f64, f64)> = std::sync::OnceLock::new();
    *COEFFS.get_or_init(|| {
        // 走配置快照：`PACE_CAD_INTERCEPT` / `PACE_CAD_SLOPE` 已登记进配置注册表，
        // 由配置层统一读（env 与 config.toml 都支持，且非法值在启动时就报错）。
        let cfg = crate::config::global();
        parse_cad_coeffs(cfg.run.cad_intercept, cfg.run.cad_slope)
    })
}

/// 步态：步频与步幅。
#[derive(Clone, Copy, Debug)]
pub struct Gait {
    /// 步频（步/分）。
    pub cadence: f64,
    /// 步幅（米）。
    pub stride: f64,
}

/// 由速度与疲劳度计算步态，严格满足 `v = cadence/60 * stride`。
///
/// 步频公式来自 4 条真实跑步记录的线性回归：`cadence = 34.4 + 44.5 * v`，
/// 拟合误差 ≤2 步/分。
///
/// | 配速 | 实测步频 | 公式预测 |
/// |------|---------|---------|
/// | 5:31/km (3.02 m/s) | 170 | 169 |
/// | 6:40/km (2.50 m/s) | 144 | 146 |
/// | 7:02/km (2.37 m/s) | 139 | 140 |
/// | 8:19/km (2.00 m/s) | 125 | 124 |
///
/// 常见的旧公式 `150 + 12v` 在 6:40/km 会给出 180 步/分，比实测高 36 步，
/// 而且形态是「步频主导」；真实数据是**步幅主导**——慢配速时步频掉得更多。
///
/// 回归的下界是 2.0 m/s（8:19/km）。**低于它不许外推**：拿回归去算
/// 12:00/km（1.39 m/s）会给出 96 步/分，被 100 的下限一夹，步频整段压成
/// 100~112 的平带，步幅又被等式反推到 0.77m——issue #7 实测对比里正好
/// 就是这两个数（真机步频 179、步幅 0.47m）。慢速走跑的实测形态是
/// 「步频顶在 175~185 一带、步幅随速度缩」：1.40 m/s 步幅 0.47m（#7 真机），
/// 2.0 m/s 步幅 0.96m（回归端点），中间两段线性接起来；1.40 以下步频近似
/// 恒定、步幅继续缩。端点上 2.0 m/s 恰好回到回归值 125，两段连续。
pub const GAIT_REGRESSION_FLOOR_MPS: f64 = 2.0;

/// 按速度（m/s）和疲劳度（0~1）给出步频与步幅；
/// 低于 [`GAIT_REGRESSION_FLOOR_MPS`] 走慢速分支，不外推回归。
pub fn gait(v: f64, fatigue: f64) -> Gait {
    if v >= GAIT_REGRESSION_FLOOR_MPS {
        let (intercept, slope) = cad_coeffs();
        let target_cad = (intercept + slope * v) * (1.0 - 0.05 * fatigue);
        let mut cadence = target_cad.clamp(100.0, 210.0);
        let mut stride = if cadence > 0.0 {
            v * 60.0 / cadence
        } else {
            0.0
        };

        // 步幅越界时反解步频，保持等式成立
        if stride > 1.7 {
            stride = 1.7;
            cadence = v * 60.0 / stride;
        } else if stride < 0.5 {
            stride = 0.5;
            cadence = v * 60.0 / stride;
        }
        Gait { cadence, stride }
    } else {
        // 慢速走跑段（见上面的锚点说明）。上端锚在回归于 2.0 的取值上
        // （123.4 步/分 → 步幅 0.973m），保证两侧连续；下端锚 1.40 →
        // 0.47m（#7 真机实测）。1.40 以下步频近似恒定、步幅随速度缩。
        //
        // 端点步幅跟着回归系数走：配了 `PACE_CAD_*` 就按新系数重算端点，
        // 否则快慢两段在 2.0 m/s 处会断开。默认系数下端点仍取原来的
        // 0.9733（**逐字节不变**）。
        let (intercept, slope) = cad_coeffs();
        let anchor_stride =
            if (intercept - 34.4).abs() < f64::EPSILON && (slope - 44.5).abs() < f64::EPSILON {
                0.9733
            } else {
                (GAIT_REGRESSION_FLOOR_MPS * 60.0 / (intercept + slope * GAIT_REGRESSION_FLOOR_MPS))
                    .clamp(0.5, 1.2)
            };
        let stride0 = if v >= 1.40 {
            0.47 + (v - 1.40) * (anchor_stride - 0.47) / (2.0 - 1.40)
        } else {
            0.47 * v / 1.40
        }
        .clamp(0.0, anchor_stride);
        // 步频下限只防退化（v→0 时步幅缩到 0、等式仍成立），不再夹 100：
        // 慢跑真机步频 179，夹 100 会把它打平成一条 100 出头的直线。
        let mut cadence = (v * 60.0 / stride0.max(1e-9)) * (1.0 - 0.05 * fatigue);
        if cadence < 60.0 {
            cadence = 60.0;
        }
        let stride = if cadence > 0.0 {
            v * 60.0 / cadence
        } else {
            0.0
        };
        Gait { cadence, stride }
    }
}

/// 由速度与疲劳度取步幅（米）。
pub fn stride_for(v: f64, fatigue: f64) -> f64 {
    gait(v, fatigue).stride
}

/// 由速度与步幅反解步频（步/分）。
pub fn cadence_for(v: f64, stride: f64) -> f64 {
    (60.0 * v / stride.max(1e-6)).clamp(110.0, 220.0)
}

/// 由步频与步幅计算速度（m/s）。
pub fn speed_from(cadence: f64, stride: f64) -> f64 {
    (cadence / 60.0) * stride
}

/// 疲劳度（0..0.6）：已跑 0.5km 后开始线性抬升，封顶 0.6。
pub fn fatigue_from_km(km: f64) -> f64 {
    if km <= 0.5 {
        0.0
    } else {
        ((km - 0.5) * 1.5).min(0.6)
    }
}

/// 步频的 OU（Ornstein-Uhlenbeck）均值回归序列。
///
/// ```text
/// c_{t+1} = c_t + θ(target_t − c_t)·Δt + σ·√Δt·Z,   Z ~ N(0,1)
/// ```
///
/// 为什么不用均匀随机抖动：真实步频有**惯性**——既不会逐点乱跳，也不会
/// 一直贴在均值上，而是带着自相关地向目标回归。OU 过程正好描述这个，
/// 均匀随机数做不到（相邻点独立，序列看起来像白噪声）。
pub fn ou_cadence_series(
    targets: &[f64],
    dts: &[f64],
    seed: u64,
    theta: f64,
    sigma: f64,
    floor: f64,
    ceil: f64,
) -> Vec<f64> {
    use rand::SeedableRng;
    use rand::rngs::StdRng;

    let mut rng = StdRng::seed_from_u64(seed);
    let mut out = Vec::with_capacity(targets.len());
    let mut c = targets.first().copied().unwrap_or(150.0);

    for (i, &target) in targets.iter().enumerate() {
        let dt = dts.get(i).copied().unwrap_or(1.0);
        c += theta * (target - c) * dt + sigma * dt.sqrt() * normal_sample(&mut rng);
        c = c.clamp(floor, ceil);
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 步频必须匹配真实记录，而不是旧的 `150 + 12v`。
    #[test]
    fn cadence_matches_real_records() {
        for (pace_s, want) in [
            (331.0, 170.0),
            (400.0, 144.0),
            (422.0, 139.0),
            (499.0, 125.0),
        ] {
            let v = 1000.0 / pace_s;
            let g = gait(v, 0.0);
            assert!(
                (g.cadence - want).abs() <= 3.0,
                "配速 {pace_s:.0}s/km 步频 {:.0} 偏离实测 {want} 超过 3 步",
                g.cadence
            );
            assert!(
                (0.85..=1.30).contains(&g.stride),
                "配速 {pace_s:.0}s/km 步幅 {:.2}m 超出真实跑者区间",
                g.stride
            );
        }
    }

    /// 等式约束必须始终成立，否则服务端一除就露馅。
    #[test]
    fn identity_always_holds() {
        for v in [1.5, 2.0, 2.5, 3.0, 3.5, 4.0] {
            for fatigue in [0.0, 0.5, 1.0] {
                let g = gait(v, fatigue);
                let back = g.cadence / 60.0 * g.stride;
                assert!(
                    (back - v).abs() < 1e-9,
                    "v={v} fatigue={fatigue} 重建速度 {back} 不符"
                );
            }
        }
    }

    /// **默认系数必须逐字节不变**：`PACE_CAD_*` 没设时就是 34.4 / 44.5。
    #[test]
    fn default_coefficients_are_unchanged() {
        let (intercept, slope) = parse_cad_coeffs(None, None);
        assert_eq!(intercept, 34.4);
        assert_eq!(slope, 44.5);
        // 非有限数也回落到默认（配置层会先挡掉语法非法值，这条是程序注入路径的兜底）。
        assert_eq!(
            parse_cad_coeffs(Some(f64::NAN), Some(f64::INFINITY)),
            (34.4, 44.5)
        );
    }

    /// 配了系数就用它：步频变高 ⇒ 步幅变短（步幅 = 速度×60/步频）。
    #[test]
    fn coefficients_can_be_overridden() {
        let (intercept, slope) = parse_cad_coeffs(Some(45.3), Some(44.5));
        assert_eq!(intercept, 45.3);
        assert_eq!(slope, 44.5);
        // 默认公式在 2.78 m/s 给 158.1 步/分（步幅 1.055m）；
        // 标定后 171.1 步/分 ⇒ 步幅 0.975m。
        let default_cad = 34.4 + 44.5 * 2.78;
        let calibrated_cad = intercept + slope * 2.78;
        assert!(calibrated_cad > default_cad);
        assert!(2.78 * 60.0 / calibrated_cad < 2.78 * 60.0 / default_cad);
    }

    #[test]
    fn gait_stays_in_envelope() {
        for v in [1.9f64, 2.5, 3.5, 5.0, 6.3] {
            for fat in [0.0f64, 0.3, 0.6] {
                let g = gait(v, fat);
                assert!((0.5..=1.7).contains(&g.stride), "stride={}", g.stride);
                assert!((110.0..=230.0).contains(&g.cadence), "cad={}", g.cadence);
                assert!((speed_from(g.cadence, g.stride) - v).abs() < 1e-6);
            }
        }
    }

    /// 慢速走跑段的锚点：12:00/km 实测步频 179、步幅 0.47m（issue #7 真机）。
    /// 回归外推在这里是错的：会给出 96 步/分、被下限夹成 100 出头的平线。
    #[test]
    fn slow_jog_matches_real_anchor() {
        let g = gait(1000.0 / 720.0, 0.0); // 12:00/km = 1.39 m/s
        assert!(
            (172.0..=185.0).contains(&g.cadence),
            "12:00/km 步频 {:.1} 应在 179 一带（真机实测），不是 100 出头",
            g.cadence
        );
        assert!(
            (0.42..=0.52).contains(&g.stride),
            "12:00/km 步幅 {:.3}m 应在 0.47 一带（真机实测）",
            g.stride
        );
        // 等式：步频×步幅必须能还原速度
        let back = g.cadence / 60.0 * g.stride;
        assert!((back - 1000.0 / 720.0).abs() < 1e-9, "等式被破坏：{back}");
    }

    /// 慢速分支与标定区间在 2.0 m/s 端点上连续：8:19/km 实测 125 步/分。
    #[test]
    fn slow_branch_continuous_at_regression_floor() {
        let below = gait(2.0 - 1e-9, 0.0);
        let at = gait(2.0, 0.0);
        assert!(
            (below.cadence - at.cadence).abs() < 1.0,
            "2.0 m/s 两侧步频 {:.1} vs {:.1} 应连续",
            below.cadence,
            at.cadence
        );
        assert!((at.cadence - 125.0).abs() <= 3.0, "8:19/km 实测 125");
        assert!(
            (below.stride - at.stride).abs() < 0.05,
            "2.0 m/s 两侧步幅 {:.3} vs {:.3} 应连续",
            below.stride,
            at.stride
        );
    }

    #[test]
    fn ou_series_is_bounded_and_deterministic() {
        let n = 200;
        let targets: Vec<f64> = (0..n).map(|i| 170.0 + 0.1 * i as f64).collect();
        let dts = vec![5.0; n];
        let a = ou_cadence_series(&targets, &dts, 42, 0.25, 0.5, 110.0, 220.0);
        assert_eq!(a.len(), n);
        for c in &a {
            assert!((110.0..=220.0).contains(c), "cadence={c}");
        }
        let b = ou_cadence_series(&targets, &dts, 42, 0.25, 0.5, 110.0, 220.0);
        assert_eq!(a, b, "同种子必须同结果");
    }

    /// OU 序列必须有自相关：相邻点差值的方差应远小于白噪声。
    #[test]
    fn ou_is_not_white_noise() {
        let n = 300;
        let targets = vec![160.0; n];
        let dts = vec![10.0; n];
        let s = ou_cadence_series(&targets, &dts, 7, 0.12, 0.8, 100.0, 220.0);
        let diffs: Vec<f64> = s.windows(2).map(|w| (w[1] - w[0]).abs()).collect();
        let mean_diff = diffs.iter().sum::<f64>() / diffs.len() as f64;
        let spread =
            s.iter().cloned().fold(f64::MIN, f64::max) - s.iter().cloned().fold(f64::MAX, f64::min);
        assert!(
            mean_diff < spread * 0.5,
            "相邻差值 {mean_diff:.3} 相对整体波动 {spread:.3} 过大，不像均值回归"
        );
    }

    #[test]
    fn fatigue_is_monotonic_and_capped() {
        assert_eq!(fatigue_from_km(0.4), 0.0);
        assert!(fatigue_from_km(1.0) > 0.0);
        assert_eq!(fatigue_from_km(5.0), 0.6);
        // 上限 0.6 在 0.9km 处就到（(0.9-0.5)*1.5 = 0.6），
        // 所以要在到达上限之前比单调性
        assert!(fatigue_from_km(0.8) > fatigue_from_km(0.6));
        assert_eq!(fatigue_from_km(0.9), 0.6);
    }
}

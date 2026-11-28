//! 类型化配置结构（消费方读这里）。

use crate::value::Sources;

/// Web 驻留进程的身份与落盘位置。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Server {
    /// 控制台口令；空 = 只接受本机访问。
    pub token: String,
    /// 数据目录覆盖；空 = `$HOME/.pace-forge`。
    pub data_dir: String,
}

/// 出口与时间口径。
#[derive(Debug, Clone, PartialEq)]
pub struct Network {
    /// 提交链路出口 `http://host:port`；空 = 直连。网页保存的设置优先于本值。
    pub egress_proxy: String,
    /// 追加信任根的 PEM 文件路径；空 = 不追加。
    pub extra_ca: String,
    /// OBS 时间戳的时区偏移（秒），默认东八区。
    pub tz_offset_sec: i64,
}

impl Default for Network {
    fn default() -> Self {
        Self {
            egress_proxy: String::new(),
            extra_ca: String::new(),
            tz_offset_sec: 8 * 3600,
        }
    }
}

/// 上报的设备口径。
#[derive(Debug, Clone, PartialEq)]
pub struct Device {
    /// 上报的 appVersion，默认取真机版本（7.4.20 / code 411）。
    pub app_version: String,
    /// 机型 code 或平台代号；空 = cpuModel 发 ABI。
    pub model: String,
    /// 五个硬件头（androidId/blMac/wifiMac/IMEI/CustomDeviceId）发派生假值还是空串。
    pub hw_headers: HwHeaders,
}

impl Default for Device {
    fn default() -> Self {
        Self {
            app_version: DEFAULT_APP_VERSION.into(),
            model: String::new(),
            hw_headers: HwHeaders::Empty,
        }
    }
}

/// 真机版本（7.4.20 / code 411，2026-11 从设备上取）。空串配置回落到这里。
pub const DEFAULT_APP_VERSION: &str = "7.4.20";

/// 五个硬件头的口径。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HwHeaders {
    /// 发空串（7.4.21 硬编码口径，默认）。
    #[default]
    Empty,
    /// 发派生假值（对照实验用）。
    Derived,
}

/// 刷跑与提交的行为开关。
#[derive(Debug, Clone, PartialEq)]
pub struct Run {
    /// CLI 刷跑的 runMode：1=计分跑，2=户外跑。
    ///
    /// 默认 **1**（计分跑）：`run` 就是刷一条**计分跑**记录。
    ///
    /// 实测（2026-11-27）：默认值是 2 时，提交出去的那条**里程算进了学期
    /// 有效里程（15.77→18.03km），但「有效次数」还是 0** —— 户外跑口径
    /// 不计次数。而且 `should_try_stadium` 要求 run_mode==1，runMode=2 根本
    /// 不会试操场轨迹 ⇒ 刷出来的是 route 兜底那条（首次经过次序靠种子凑）。
    /// 户外跑要走 `run-free`（显式传 runMode=2 + sportType=4）。
    pub run_mode: i64,
    /// 几何采样间距（米）。
    pub step_m: f64,
    /// 几何规划时弧的鼓出上限（米）。
    pub bulge_max: f64,
    /// 点位 type 口径。
    pub pt_scheme: PtScheme,
    /// 固定点（isFixed/forceNormal）口径。
    pub fixed_point: FixedPoint,
    /// 必到点只认 10 米内，不自动放开半径。
    pub fixed_strict: bool,
    /// 策略拉取失败直接报错，不走 runMode=2 兜底。
    pub strict_policy: bool,
    /// 围栏对象发完整结构（distance/id/name），默认只发 points。
    pub fence_full: bool,
    /// Some(-1) = 提交报文发 runAreaId=-1（旧行为对照）。
    pub run_area_id: Option<i64>,
    /// 显式围栏锚点（成对出现，缺一半在加载时报错）。
    pub center_lat: Option<f64>,
    /// 同上。
    pub center_lng: Option<f64>,
    /// CLI 默认试操场轨迹（web 不读这个，走自己的 mode）。
    ///
    /// 默认 **true**：操场模式算出来的轨迹是「按点位数组序穿过全部打卡点的
    /// 光滑环」，首次经过次序**确定**；路线规划那条兜底路的次序取决于种子
    /// ——实测某校 5 个点的数组序 1→4→5→2→3、环序 1→2→3→4→5，兜底路要靠
    /// 种子凑巧才同序（凑巧时能过，凑不巧就判「按顺序通过所有点位」失败）。
    /// 操场模式失败时本来就会自动退回路线规划，所以这里默认开是安全的。
    pub prefer_stadium: bool,
    /// 超日上限也继续提交。
    pub ignore_daily_limit: bool,
    /// 起步窗口的**里程**占比（0~1）：起步窗口占多少时间（默认 15.5%），
    /// 但几何上只允许走这么点路。
    ///
    /// 默认 **0.011**（1.1%）= **真机同款**（rrid 1328288734：555 秒 /
    /// 1683.83m，起步 86 秒只走 18.4m；逐点反推是 18 点走 13.6m，0.76 m/点）。
    ///
    /// ⚠️ 「起步段的点挤成一坨」**就是真机形态**：真机 GPS 刚锁定那 86 秒
    /// 位移被过滤，18 个点全落在 13.6 米里（密度比 18.4×）。我们的 18 分钟 /
    /// 3 公里是 34 个点落在 26 米里（0.77 m/点），每点密度与真机同量级。
    ///
    /// 早先默认 3%（每点 2.6 m、密度差 6 倍）是为了「看不出点团」，但那**偏离
    /// 真机 3 倍**，而且并没有解决真正的问题：真正的问题是「起步压缩把窗口里
    /// 的**打卡点**一起压扁」（某校 5 个打卡点有 4 个落在窗口内，压缩后相邻
    /// 只剩 5.5 m < 服务端 10 m 半径 ⇒ 「首次经过」次序乱）。修法在几何层：
    /// 把起步里程预算挪到**采样阶段**的弧长分配里（`track::apply_startup_budget`），
    /// 窗口里就再也没有打卡点被压扁 ⇒ 首过序在 1.1% 下也全对。
    ///
    /// 调到 **0.155** 等于「时间占比 = 里程占比」⇒ 起步段完全不压缩 ⇒ 全程
    /// 点间距均匀，但起步段不再有「GPS 未锁定、速度字段发硬 0」的那段结构
    /// （字段清零口径仍按时间窗口走，于是那 167 秒的几何在跑、字段写着 0）。
    pub startup_dist_ratio: f64,
    /// 账号已在服务端违规名单里，也继续提交。
    ///
    /// 默认关：名单里的记录是**过去**留下的（旧轨迹的毛病），新轨迹修好了
    /// 之后本来值得再试一条；但「提交成功」不等于「没被标记」，所以这个
    /// 开关必须显式开、并且把开关名打进日志，留痕。
    pub ignore_cheat_flag: bool,
    /// 策略响应落盘目录（排障用）；空 = 不落盘。
    pub dump_policy: String,
    /// 步频回归截距（`cadence = 截距 + 斜率×v`）；None = 用内置默认 34.4。
    pub cad_intercept: Option<f64>,
    /// 步频回归斜率；None = 用内置默认 44.5。
    pub cad_slope: Option<f64>,
}

impl Default for Run {
    fn default() -> Self {
        Self {
            run_mode: 1,
            step_m: 5.0,
            bulge_max: 60.0,
            pt_scheme: PtScheme::Auto,
            fixed_point: FixedPoint::Auto,
            fixed_strict: false,
            strict_policy: false,
            fence_full: false,
            run_area_id: None,
            center_lat: None,
            cad_intercept: None,
            cad_slope: None,
            center_lng: None,
            prefer_stadium: true,
            ignore_daily_limit: false,
            startup_dist_ratio: 0.011,
            ignore_cheat_flag: false,
            dump_policy: String::new(),
        }
    }
}

/// 点位 type 口径（`PACE_PT_SCHEME` / `run.pt_scheme`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PtScheme {
    /// 按有无必到点自动分流（默认）。
    #[default]
    Auto,
    /// 老样本口径（显式 `old` / `marks`）。
    Old,
    /// 新设备口径（显式 `new`）。
    New,
}

/// 固定点口径（`PACE_FIXED_POINT` / `run.fixed_point`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FixedPoint {
    /// 自动（默认）。
    #[default]
    Auto,
    /// 全部发 0（`all0` / `allnormal`；旧变量 `=1`）。
    AllNormal,
    /// 跟随服务端（`server` / `follow`；旧变量 `=0`）。
    FollowServer,
}

/// 加载完成的配置。消费方按 `cfg.run.bulge_max` 这样的路径读字段，
/// 「值来自哪一层」看 [`Config::sources`]。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Config {
    /// 服务端地址与接口版本。
    pub server: Server,
    /// 代理、出口与超时。
    pub network: Network,
    /// 机型、系统版本等设备头。
    pub device: Device,
    /// 轨迹与提交相关的运行参数。
    pub run: Run,
    /// 每个键最终取自哪一层（默认 / toml / env），供「非默认开关」提示用。
    pub sources: Sources,
}

impl Config {
    /// 全部键的当前显示值（启动摘要和排障用；口令类值在这里就打码）。
    ///
    /// 这份映射是注册表与结构体字段之间的**唯一桥梁**：新增开关时两边
    /// 各改一处，`registry_matches_config_keys` 测试保证不改就测试失败。
    pub fn key_values(&self) -> Vec<(&'static str, String)> {
        let f = |v: f64| fmt_float(v);
        let b = |v: bool| if v { "1" } else { "0" };
        let opt = |v: Option<f64>| v.map(f).unwrap_or_else(|| "未设置".into());
        vec![
            (
                "server.token",
                if self.server.token.is_empty() {
                    "未设置".into()
                } else {
                    "已设置（不显示值）".into()
                },
            ),
            (
                "server.data_dir",
                if self.server.data_dir.is_empty() {
                    "未设置（$HOME/.pace-forge）".into()
                } else {
                    self.server.data_dir.clone()
                },
            ),
            (
                "network.egress_proxy",
                if self.network.egress_proxy.is_empty() {
                    "未设置（直连）".into()
                } else {
                    self.network.egress_proxy.clone()
                },
            ),
            (
                "network.extra_ca",
                if self.network.extra_ca.is_empty() {
                    "未设置".into()
                } else {
                    self.network.extra_ca.clone()
                },
            ),
            (
                "network.tz_offset_sec",
                self.network.tz_offset_sec.to_string(),
            ),
            ("device.app_version", self.device.app_version.clone()),
            (
                "device.model",
                if self.device.model.is_empty() {
                    "未设置（cpuModel 发 ABI）".into()
                } else {
                    self.device.model.clone()
                },
            ),
            (
                "device.hw_headers",
                match self.device.hw_headers {
                    HwHeaders::Empty => "empty".into(),
                    HwHeaders::Derived => "derived".into(),
                },
            ),
            ("run.run_mode", self.run.run_mode.to_string()),
            ("run.step_m", f(self.run.step_m)),
            ("run.bulge_max", f(self.run.bulge_max)),
            (
                "run.pt_scheme",
                match self.run.pt_scheme {
                    PtScheme::Auto => "auto",
                    PtScheme::Old => "old",
                    PtScheme::New => "new",
                }
                .into(),
            ),
            (
                "run.fixed_point",
                match self.run.fixed_point {
                    FixedPoint::Auto => "auto",
                    FixedPoint::AllNormal => "allnormal",
                    FixedPoint::FollowServer => "server",
                }
                .into(),
            ),
            ("run.fixed_strict", b(self.run.fixed_strict).into()),
            ("run.strict_policy", b(self.run.strict_policy).into()),
            ("run.fence_full", b(self.run.fence_full).into()),
            (
                "run.run_area_id",
                match self.run.run_area_id {
                    Some(-1) => "-1（旧行为）".into(),
                    Some(v) => v.to_string(),
                    None => "未设置（发真实围栏 id）".into(),
                },
            ),
            ("run.center_lat", opt(self.run.center_lat)),
            ("run.cad_intercept", opt(self.run.cad_intercept)),
            ("run.cad_slope", opt(self.run.cad_slope)),
            ("run.center_lng", opt(self.run.center_lng)),
            ("run.prefer_stadium", b(self.run.prefer_stadium).into()),
            (
                "run.ignore_daily_limit",
                b(self.run.ignore_daily_limit).into(),
            ),
            (
                "run.ignore_cheat_flag",
                b(self.run.ignore_cheat_flag).into(),
            ),
            (
                "run.dump_policy",
                if self.run.dump_policy.is_empty() {
                    "未设置（不落盘）".into()
                } else {
                    self.run.dump_policy.clone()
                },
            ),
        ]
    }
}

fn fmt_float(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e15 {
        format!("{v:.0}")
    } else {
        v.to_string()
    }
}

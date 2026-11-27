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
    pub prefer_stadium: bool,
    /// 超日上限也继续提交。
    pub ignore_daily_limit: bool,
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
            run_mode: 2,
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
            prefer_stadium: false,
            ignore_daily_limit: false,
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

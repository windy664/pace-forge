//! pace-forge 的类型化配置：`config.toml` + `PACE_*` 环境变量覆盖层。
//!
//! 分层与语义：
//!
//! ```text
//! 环境变量（PACE_*）  >  config.toml  >  内置默认值
//! ```
//!
//! 启动时一次性加载并校验（fail-fast：非法值、未知键、缺一半的围栏锚点
//! 都直接报错退出，不再静默回落默认值），进程内只读快照（[`store`] +
//! [`global`]）。运行期可变的设置（比如网页里保存的出口代理）不属于这里，
//! 仍归各自模块的 runtime 槽位管。
//!
//! 所有开关的键名、环境变量名、解析器都集中在注册表一张表里（[`registry`]）：
//! TOML 解析和 env 解析用同一份定义，不存在「文档表」和「实际读取」两套
//! 东西对不上的问题。键的参考文档在仓库根的 `config.example.toml`
//! （带注释），与注册表一一对应。
//!
//! 与旧实现（`src/config.rs` 手写 ENTRIES 清单 + 各处散读 `env::var`）的
//! 兼容关系：
//! - 全部 `PACE_*` 变量名保留，语义不变（个别例外见条目注释）；
//! - 新增 `config.toml` 一层，环境变量覆盖它；
//! - 非法值从「静默回落默认」变为「启动报错」——这是本次重构的目的。
//!
//! 模块划分：[`value`] 值与来源、[`model`] 类型化配置结构、[`parser`]
//! 单键解析器、[`registry`] 开关注册表、[`load`] 加载与校验、[`runtime`]
//! 进程内快照与启动摘要、[`tests`] 等价性与边界矩阵。

mod load;
mod model;
mod parser;
mod registry;
mod runtime;
#[cfg(test)]
mod tests;
mod value;

pub use load::{CONFIG_FILE_NAME, load};
pub use model::{
    Config, DEFAULT_APP_VERSION, Device, FixedPoint, HwHeaders, Network, PtScheme, Run, Server,
};
pub use runtime::{Guard, global, non_default_line, store};
pub use value::{Source, Sources, Value};

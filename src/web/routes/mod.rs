//! 按业务域拆分的路由处理器；分发仍在 `web::handle`。

mod accounts;
mod login;
mod map_data;
mod meta;
mod records;
mod run;

pub(super) use accounts::*;
pub(super) use login::*;
pub(super) use map_data::*;
pub(super) use meta::*;
pub(super) use records::*;
pub(super) use run::*;

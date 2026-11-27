//! 进程内快照与启动摘要：入口点 store 一次，运行期只读。

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, RwLock};

use crate::model::Config;
use crate::registry::TABLE;
use crate::value::Source::Default;

static CURRENT: RwLock<Option<Arc<Config>>> = RwLock::new(None);
static FALLBACK: OnceLock<Arc<Config>> = OnceLock::new();

/// 装入进程级快照。入口点（CLI `main` / 未来其他入口）在解析完参数后调用
/// 一次；之后配置只读。重复调用会替换快照，正常运行不会发生。
pub fn store(cfg: Config) {
    *CURRENT.write().unwrap_or_else(|p| p.into_inner()) = Some(Arc::new(cfg));
}

/// 当前配置。未 [`store`] 时返回内置默认——库函数在任何入口下都不该 panic。
pub fn global() -> Arc<Config> {
    let cur = CURRENT.read().unwrap_or_else(|p| p.into_inner()).clone();
    cur.unwrap_or_else(|| FALLBACK.get_or_init(|| Arc::new(Config::default())).clone())
}

/// 测试注入：替换快照并在 `Drop` 时还原。内部互斥锁让多个用 Guard 的测试
/// 自然串行，不会互相覆盖。
pub struct Guard {
    prev: Option<Arc<Config>>,
    _lock: MutexGuard<'static, ()>,
}

static GUARD_LOCK: Mutex<()> = Mutex::new(());

impl Guard {
    /// 把全局配置换成 `cfg`，返回的 [`Guard`] 在 drop 时恢复原值。
    ///
    /// 只给测试和「临时切换」用：持有期间其它线程读到的就是 `cfg`。
    pub fn replace(cfg: Config) -> Guard {
        let lock = GUARD_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let prev = CURRENT
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .replace(Arc::new(cfg));
        Guard { prev, _lock: lock }
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        *CURRENT.write().unwrap_or_else(|p| p.into_inner()) = self.prev.take();
    }
}

/// 启动时的「非默认项」一行。全默认返回 `None`。值显示走
/// [`Config::key_values`](Config::key_values)（口令打码），来源看
/// [`Config::sources`](Config::sources)。完整键参考在 `config.example.toml`，
/// 不放 CLI 命令。
pub fn non_default_line() -> Option<String> {
    let cfg = global();
    let kv = cfg.key_values();
    let values: HashMap<&str, &str> = kv.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let mut items: Vec<String> = Vec::new();
    for e in TABLE {
        if cfg.sources.get(e.key).copied().unwrap_or(Default) == Default {
            continue;
        }
        items.push(format!("{}={}（{}）", e.env, values[e.key], "非默认"));
    }
    if items.is_empty() {
        None
    } else {
        Some(items.join(", "))
    }
}

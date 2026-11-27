//! 会话与设备身份的持久化。
//!
//! 两层必须分开存：
//!
//! - **设备身份**（`identity.json`）：`device_id` 全生命周期复用。每次随机生成
//!   会让服务端认成「另一台设备」，直接撞上 `10121 此账号已在其他设备登录`。
//! - **登录会话**（`session.json`）：登录成功即落盘，登出时删除。
//!
//! 会话**必须落盘**，不能只放内存。只在内存的后果是进程一重启 token 就没了，
//! 而服务端那边的会话还活着 —— 此时想登出也拿不出凭证，账号会被锁一小时。
//!
//! 设计要点：
//! `load_identity` / `save_session` / `clear_session` 三段式。

use crate::error::{Error, ErrorKind};
use serde::{Deserialize, Serialize};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// 登录会话。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Session {
    #[serde(default)]
    /// 用户 id。
    pub uid: String,
    #[serde(default)]
    /// 登录 token。
    pub token: String,
    #[serde(default)]
    /// 会话标识。
    pub unid: String,
    #[serde(default)]
    /// 登录名。
    pub username: String,
    #[serde(default)]
    /// 姓名。
    pub name: String,
    #[serde(default)]
    /// 校区名。
    pub campus_name: String,
    #[serde(default)]
    /// 院系。
    pub depart: String,
    #[serde(default)]
    /// 班级。
    pub grade_class: String,
    /// 登录时使用的设备标识，登出时用于确认是同一台机
    #[serde(default)]
    pub device_id: String,
}

impl Session {
    /// token 和 uid 都在才算已登录（不校验是否过期）。
    pub fn is_valid(&self) -> bool {
        !self.token.is_empty() && !self.uid.is_empty()
    }
}

/// 数据目录名（相对用户主目录）。
pub const DATA_DIR_NAME: &str = ".pace-forge";

/// 用户主目录：优先非空 HOME，Windows 再取 USERPROFILE，最后回退系统临时目录。
/// 只读环境变量，不写、不创建任何东西；保留操作系统原始路径编码。
pub(crate) fn home_dir() -> PathBuf {
    #[cfg(windows)]
    let user_profile = std::env::var_os("USERPROFILE");
    #[cfg(not(windows))]
    let user_profile = None;

    resolve_home_dir(std::env::var_os("HOME"), user_profile, std::env::temp_dir())
}

/// 纯路径选择，供测试注入输入而不修改进程全局环境变量。
fn resolve_home_dir(
    home: Option<OsString>,
    user_profile: Option<OsString>,
    fallback: PathBuf,
) -> PathBuf {
    home.filter(|value| !value.is_empty())
        .or_else(|| user_profile.filter(|value| !value.is_empty()))
        .map(PathBuf::from)
        .unwrap_or(fallback)
}

/// 给定 HOME 基目录，拼出数据目录：`<base>/.pace-forge`。
///
/// **纯路径计算，不碰磁盘。** 测试用它把「数据目录」注入成
/// `std::env::temp_dir()` 下的一次性目录，从而完全不碰真实
/// `$HOME/.pace-forge/`（否则跑一次 `cargo test` 就会删掉真实登录态）。
pub fn data_dir_in(base: &Path) -> PathBuf {
    base.join(DATA_DIR_NAME)
}

/// 显式指定的数据目录（配置 `server.data_dir` / env `PACE_FORGE_DATA_DIR`）。
///
/// 设了就用它，**不再追加 `.pace-forge`**——用户给的是「数据放哪儿」，不是
/// 「HOME 在哪儿」。多实例、把凭证放加密盘、CI 隔离都靠这个开关。
pub fn data_dir_override() -> Option<PathBuf> {
    // 原文直用（不 trim）：与旧 `var_os` 语义一致
    let cfg = crate::config::global();
    let dir = &cfg.server.data_dir;
    (!dir.is_empty()).then(|| PathBuf::from(dir))
}

/// 数据目录：`<用户主目录>/.pace-forge/`。
///
/// 只算路径，不做 IO。早先这里带 `create_dir_all`，等于任何一次只读调用
/// 都会在真实 HOME 下建目录；建目录的责任现在落在真正写盘的唯一入口
/// —— [`write_file_private`]（`ensure_dir` + 0600 写文件），
/// `save_session` / `accounts::save` / `client::persistent_device_id` /
/// `track::roadnet` 的四类缓存 / `track::tileroad` 的瓦片 / `policy` 的
/// 调试转储都走它。
pub fn data_dir() -> PathBuf {
    if let Some(dir) = data_dir_override() {
        return dir;
    }
    data_dir_in(&home_dir())
}

// ---------------------------------------------------------------------------
// 落盘原语（全项目唯一的「建目录 + 0600 写文件」实现）
// ---------------------------------------------------------------------------

/// 确保目录存在（幂等）。
///
/// **建目录的责任点就在这里**。`data_dir()` / `data_dir_in()` 只算路径、不做
/// IO，所以任何写盘路径都必须先走这里，否则首次运行时目录不存在、写失败。
pub(crate) fn ensure_dir(dir: &Path) -> Result<(), Error> {
    std::fs::create_dir_all(dir)
        .map_err(|e| ErrorKind::client(format!("建目录 {} 失败: {e}", dir.display())))
}

/// 以 0600 权限写一个文件，**自动建父目录**。
///
/// 用 `File::create` + `set_permissions` 而不是 `fs::write`：`fs::write` 吃
/// umask（常见 0644），同机其他用户能读到 token / 加密后的密码。这些文件
/// 一律 0600。
///
/// 目录不存在时由这里负责建出来 —— `~/.pace-forge/` 在首次运行时并不存在，
/// 早先 `data_dir()` 带 `create_dir_all` 副作用把它掩盖了；副作用去掉之后，
/// 这个责任必须显式落在每个写盘路径上。
pub(crate) fn write_file_private(path: &Path, data: &[u8]) -> Result<(), Error> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        ensure_dir(parent)?;
    }
    // 先写同目录临时文件、fsync、再 rename 覆盖目标 —— 这样断电/崩溃时
    // 要么是旧内容，要么是新内容，不会留下「截断到一半」的文件。
    //
    // 早先这里是 `File::create(path)` 直接截断目标再写：`session.json` 走这条
    // 路径，写一半断电就把本地会话写坏，而 `load` 对损坏文件 `unwrap_or_default()`
    // 静默返回空会话 —— 服务端会话还活着，本地 token 却没了，下次重登直接
    // 撞 10121 锁一小时。`accounts.json` 自己包了一层 tmp+rename，这里补上之后
    // 所有落盘都同一套语义。
    let tmp = temp_sibling(path);
    // 权限必须**在创建时**就是 0600，不能建完再 chmod：
    // `File::create` 走 umask（常见 0644），从建好到 `set_permissions` 之间有
    // 一个微秒级窗口，此时同机其他用户能读到 `session.json` 里的 token 和
    // `accounts.json` 里的密文。Uni x 上直接用 `OpenOptions::mode` 一次到位。
    #[cfg(unix)]
    let mut f = {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)
            .map_err(|e| ErrorKind::client(format!("写入 {} 失败: {e}", tmp.display())))?
    };
    #[cfg(not(unix))]
    let mut f = std::fs::File::create(&tmp)
        .map_err(|e| ErrorKind::client(format!("写入 {} 失败: {e}", tmp.display())))?;
    use std::io::Write;
    let write_res = f.write_all(data).and_then(|_| f.sync_all());
    drop(f);
    if let Err(e) = write_res {
        let _ = std::fs::remove_file(&tmp);
        return Err(ErrorKind::client(format!(
            "写入 {} 失败: {e}",
            tmp.display()
        )));
    }
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(ErrorKind::client(format!(
            "替换 {} 失败: {e}",
            path.display()
        )));
    }
    Ok(())
}

/// 目标文件同目录下的临时文件名（带 pid，避免并发写互相覆盖）。
fn temp_sibling(path: &Path) -> std::path::PathBuf {
    let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("pf");
    let mut p = path.to_path_buf();
    p.set_file_name(format!("{name}.tmp{}", std::process::id()));
    p
}

fn read_json_below<T: serde::de::DeserializeOwned>(
    dir: &Path,
    name: &str,
) -> Result<Option<T>, Error> {
    let path = dir.join(name);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(ErrorKind::client(format!(
                "读取 {} 失败: {error}",
                path.display()
            )));
        }
    };
    // 损坏的会话无法恢复。保留原文件，不因此阻止退出账号。
    Ok(serde_json::from_slice(&bytes).ok())
}

fn write_json_below<T: Serialize>(dir: &Path, name: &str, value: &T) -> Result<(), Error> {
    let json = serde_json::to_string_pretty(value)
        .map_err(|e| ErrorKind::client(format!("序列化 {name} 失败: {e}")))?;
    // 会话文件含 token，write_file_private 会建目录并收紧到 0600
    write_file_private(&dir.join(name), json.as_bytes())
}

/// 读取会话。没有或损坏时返回空会话。
pub fn load_session() -> Session {
    load_session_below(&data_dir())
}

/// 同上，但数据目录由调用方指定（测试注入隔离目录用）。
pub(crate) fn load_session_below(dir: &Path) -> Session {
    read_json_below(dir, "session.json")
        .ok()
        .flatten()
        .unwrap_or_default()
}

/// 写入会话（登录成功后立即调用）。
pub fn save_session(s: &Session) -> Result<(), Error> {
    save_session_below(&data_dir(), s)
}

/// 同上，但数据目录由调用方指定。
pub(crate) fn save_session_below(dir: &Path, s: &Session) -> Result<(), Error> {
    write_json_below(dir, "session.json", s)
}

/// 删除会话文件。
pub fn clear_session() {
    clear_session_below(&data_dir());
}

/// 同上，但数据目录由调用方指定。
///
/// **测试必须用这个而不是 `clear_session()`**：`clear_session()` 删的是真实
/// `$HOME/.pace-forge/session.json`，测试里调用它等于把用户的登录态删掉。
pub(crate) fn clear_session_below(dir: &Path) {
    let _ = remove_session_file(dir);
}

/// 清理符合条件的兼容会话，保留其他账号的会话。
pub(crate) fn clear_session_if_below(
    dir: &Path,
    belongs_to_account: impl FnOnce(&Session) -> bool,
) -> Result<(), Error> {
    if read_json_below::<Session>(dir, "session.json")?
        .as_ref()
        .is_some_and(belongs_to_account)
    {
        remove_session_file(dir)?;
    }
    Ok(())
}

fn remove_session_file(dir: &Path) -> Result<(), Error> {
    let path = dir.join("session.json");
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(ErrorKind::client(format!(
            "删除会话文件 {} 失败: {error}",
            path.display()
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// 一次性隔离目录：`$TMPDIR/pace-forge-test-<tag>-<pid>-<seq>-<nanos>`。
    ///
    /// 刻意**不用** `std::env::set_var("HOME", …)`：测试并行执行，改全局环境
    /// 变量会让「谁先改谁赢」，不同测试互相串；这里改成把 HOME 基目录注入给
    /// `*_below` 系列函数，每个测试各拿一个互不重叠的临时目录，`Drop` 时清掉。
    struct TestDir {
        root: PathBuf,
    }

    impl TestDir {
        fn new(tag: &str) -> Self {
            static SEQ: AtomicU64 = AtomicU64::new(0);
            let n = SEQ.fetch_add(1, Ordering::Relaxed);
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let root = std::env::temp_dir().join(format!(
                "pace-forge-test-{tag}-{}-{n}-{nanos}",
                std::process::id()
            ));
            std::fs::create_dir_all(&root).expect("建临时基目录失败");
            Self { root }
        }

        /// 注入给 `*_below` 的「HOME 基目录」。
        fn home(&self) -> &Path {
            &self.root
        }

        /// 本测试独占的数据目录（等价于隔离环境里的 `$HOME/.pace-forge`）。
        fn data(&self) -> PathBuf {
            data_dir_in(self.home())
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    /// 落盘必须是「写临时文件 → fsync → rename」：中途崩溃只能是旧内容，
    /// 不能留下截断到一半的 JSON，也不能残留临时文件。
    ///
    /// 早先 `write_file_private` 直接 `File::create(目标)` 截断再写，而
    /// `session.json` 走的正是这条路径：写一半断电就把本地会话写坏，
    /// `load` 对损坏文件 `unwrap_or_default()` 静默成空会话。
    #[test]
    fn private_write_is_atomic_and_leaves_no_temp() {
        let t = TestDir::new("atomic");
        let dir = t.data();
        ensure_dir(&dir).unwrap();
        let p = dir.join("session.json");

        write_file_private(&p, b"{\"v\":1}").unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "{\"v\":1}");

        let leftovers: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.contains(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "残留临时文件: {leftovers:?}");

        // 覆盖写：旧内容整体替换，不会新旧混合
        write_file_private(&p, b"{\"v\":2}").unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "{\"v\":2}");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&p).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "会话文件必须 0600");
        }
    }

    fn temp_session() -> Session {
        Session {
            uid: "123".into(),
            token: "abc".into(),
            unid: "88".into(),
            username: "13800000000".into(),
            name: "测试".into(),
            campus_name: "某大学".into(),
            depart: "计算机".into(),
            grade_class: "2024级".into(),
            device_id: "deadbeef".into(),
        }
    }

    #[test]
    fn home_resolution_preserves_home_priority() {
        assert_eq!(
            resolve_home_dir(
                Some(OsString::from("configured-home")),
                Some(OsString::from("windows-profile")),
                PathBuf::from("fallback"),
            ),
            PathBuf::from("configured-home")
        );
    }

    #[test]
    fn home_resolution_uses_windows_profile_without_home() {
        for home in [None, Some(OsString::new())] {
            assert_eq!(
                resolve_home_dir(
                    home,
                    Some(OsString::from("windows-profile")),
                    PathBuf::from("fallback"),
                ),
                PathBuf::from("windows-profile")
            );
        }
    }

    #[test]
    fn home_resolution_uses_fallback_for_missing_or_empty_values() {
        for home in [None, Some(OsString::new())] {
            for profile in [None, Some(OsString::new())] {
                assert_eq!(
                    resolve_home_dir(home.clone(), profile, PathBuf::from("system-temp")),
                    PathBuf::from("system-temp")
                );
            }
        }
    }

    #[test]
    #[cfg(any(unix, windows))]
    fn home_resolution_preserves_non_unicode_paths() {
        #[cfg(unix)]
        let raw = {
            use std::os::unix::ffi::OsStringExt;
            OsString::from_vec(vec![b'/', b'h', 0xff])
        };
        #[cfg(windows)]
        let raw = {
            use std::os::windows::ffi::OsStringExt;
            OsString::from_wide(&[b'C' as u16, b':' as u16, b'\\' as u16, 0xd800])
        };

        assert!(raw.to_str().is_none());
        let expected = PathBuf::from(raw.clone());
        assert_eq!(
            resolve_home_dir(Some(raw.clone()), None, PathBuf::from("fallback")),
            expected
        );
        assert_eq!(
            resolve_home_dir(None, Some(raw), PathBuf::from("fallback")),
            expected
        );
    }

    #[test]
    fn session_roundtrip() {
        let t = TestDir::new("roundtrip");
        let mut s = temp_session();
        s.name = "往返测试".into();
        save_session_below(&t.data(), &s).unwrap();
        let back = load_session_below(&t.data());
        assert_eq!(back.uid, "123");
        assert_eq!(back.token, "abc");
        assert_eq!(back.name, "往返测试");
        assert!(t.data().join("session.json").exists(), "应写在隔离目录里");
    }

    #[test]
    fn json_roundtrip_in_isolated_dir() {
        let t = TestDir::new("json");
        let s = temp_session();
        write_json_below(&t.data(), "session_test.json", &s).unwrap();
        let back: Session = read_json_below(&t.data(), "session_test.json")
            .unwrap()
            .unwrap();
        assert_eq!(back.uid, "123");
        assert!(t.data().join("session_test.json").exists());
    }

    /// 没有会话时返回空会话而不是报错。
    ///
    /// 这个测试以前调 `clear_session()`，删的是**真实**的
    /// `$HOME/.pace-forge/session.json` —— 跑一次 `cargo test` 就把本地登录态
    /// 删了，CLI 立刻变「未登录」，而服务端会话还活着，重登直接撞
    /// `10121 此账号已在其他设备登录`（锁号一小时）。现在只在隔离目录里删。
    #[test]
    fn missing_session_is_empty_not_error() {
        let t = TestDir::new("missing");
        // 先放一个真会话进去，再清掉，模拟「登录过又退出」
        save_session_below(&t.data(), &temp_session()).unwrap();
        clear_session_below(&t.data());
        let s = load_session_below(&t.data());
        assert!(s.token.is_empty(), "没有会话文件时 token 应为空");
        assert!(s.uid.is_empty());
        assert!(!s.is_valid(), "空会话不算有效会话");

        // 从未写过会话的目录同样返回空会话，不 panic、不报错
        let t2 = TestDir::new("never-written");
        let s2 = load_session_below(&t2.data());
        assert!(!s2.is_valid());
    }

    #[test]
    fn validity_requires_token_and_uid() {
        let mut s = temp_session();
        assert!(s.is_valid());
        s.token.clear();
        assert!(!s.is_valid(), "没有 token 就不算有效会话");
        s.token = "x".into();
        s.uid.clear();
        assert!(!s.is_valid(), "没有 uid 也不算了");
    }

    /// 首次写盘时应自动建出数据目录（`data_dir_in` 本身不做 IO）。
    #[test]
    fn data_dir_is_created_on_first_write() {
        let t = TestDir::new("created");
        let d = t.data();
        assert!(d.to_string_lossy().contains(".pace-forge"));
        assert!(!d.exists(), "只算路径的 data_dir_in 不该建目录");
        save_session_below(&d, &temp_session()).unwrap();
        assert!(d.exists(), "首次落盘应自动创建数据目录");
    }

    /// 多级目录（如 `~/.pace-forge/tiles/`）也要一次建成。
    ///
    /// 瓦片缓存写的是 `<data>/tiles/<z>_<x>_<y>.png`，父目录有两级；`create_dir_all`
    /// 语义必须覆盖这种情况，否则首次拉瓦片会失败。
    #[test]
    fn write_creates_nested_dirs_on_first_use() {
        let t = TestDir::new("nested");
        let nested = t.data().join("tiles");
        assert!(!nested.exists());
        write_json_below(&nested, "probe.json", &temp_session()).unwrap();
        assert!(nested.exists(), "嵌套目录也应被建出来");
        assert!(nested.join("probe.json").exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(nested.join("probe.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600, "写出的文件权限必须是 600");
        }
    }

    /// 回归护栏：测试用的数据目录**绝不能**是真实的 `$HOME/.pace-forge`。
    ///
    /// 谁哪天把 `_below` 改回 `data_dir()`，这条会立刻炸。
    #[test]
    fn test_data_dir_is_isolated_from_real_home() {
        let t = TestDir::new("isolated");
        let test_dir = t.data();
        // data_dir_in(&home_dir()) 与 data_dir() 同值，但不创建目录、不读文件
        let real_dir = data_dir_in(&home_dir());

        assert_ne!(
            test_dir, real_dir,
            "测试数据目录不能等于真实 $HOME/.pace-forge"
        );
        assert!(
            !test_dir.starts_with(&real_dir),
            "测试数据目录不能落在真实 $HOME/.pace-forge 之下: {}",
            test_dir.display()
        );
        assert!(
            test_dir.starts_with(std::env::temp_dir()),
            "测试数据目录应在临时目录下: {}",
            test_dir.display()
        );
        assert!(
            test_dir.to_string_lossy().contains("pace-forge-test-"),
            "测试数据目录应带一次性前缀: {}",
            test_dir.display()
        );
    }
}

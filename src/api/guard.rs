//! 提交台账：预写日志 + 时间段去重。
//!
//! 解决的问题只有一类，但后果很重：**提交请求发出去之后，结果未知**。
//!
//! 网络超时、连接被重置、响应体解析失败——这三种情况下服务端可能已经入账，
//! 也可能没有。此时如果用户（或者重试逻辑）再跑一次，服务端就会看到同一
//! 时间段的两条记录。那不是「配速偏快」这种可以解释成跑者习惯的差异，而是
//! **物理上不可能**：一个人不可能在同一段时间里跑两次。这是最容易被判定
//! 异常、也最难解释的形态。
//!
//! 所以这里记的是「意图」而不是「结果」：请求发出**之前**先落盘一条
//! `pending`，拿到 rrid 之后再补一条 `accepted`，明确失败补 `rejected`。
//! 进程在中间崩了，下次启动读到的就是 `pending`——**保持未知，不自动重试**。
//! 「查不到」不等于「没入账」，这条区别是整套逻辑的立足点。
//!
//! 台账按 uid 分文件，不同账号互不影响；同机多进程由 O_APPEND 的原子性兜底
//! （每行一个独立事件，写一行是一次 `write`，不会互相截断）。

use crate::error::{Error, ErrorKind};
use serde::{Deserialize, Serialize};
use std::io::Write as _;
use std::path::{Path, PathBuf};

/// 一条台账事件。`state` 决定其余字段的含义。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Event {
    /// 请求已发出，结果未知。
    Pending {
        /// 这次提交的本地唯一 id。
        uuid: String,
        /// 提交窗口起点（毫秒）。
        start_ms: i64,
        /// 提交窗口终点（毫秒）。
        stop_ms: i64,
    },
    /// 服务端接受了，`rrid` 是记录 id。
    Accepted {
        /// 对应的本地提交 id。
        uuid: String,
        /// 服务端返回的记录 id。
        rrid: i64,
    },
    /// 服务端明确拒绝（有错误码、且能确定没有入账）。
    Rejected {
        /// 被拒的那次提交的本地 id。
        uuid: String,
    },
}

/// 一次提交尝试（由事件流回放出来）。
#[derive(Debug, Clone)]
struct Attempt {
    uuid: String,
    start_ms: i64,
    stop_ms: i64,
    /// `None` = 结果未知；`Some(None)` = 明确被拒；`Some(Some(rrid))` = 已入账。
    outcome: Option<Option<i64>>,
}

/// 提交台账。
#[derive(Debug)]
pub struct Guard {
    path: PathBuf,
    attempts: Vec<Attempt>,
}

impl Guard {
    /// 台账文件路径：`<data_dir>/submissions/<uid>.jsonl`。
    pub fn path_for(data_dir: &Path, uid: &str) -> PathBuf {
        // uid 会进文件名，先滤掉路径分隔符和空白，避免 `../` 之类跑出目录。
        let safe: String = uid
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        let safe = if safe.is_empty() {
            "unknown".to_string()
        } else {
            safe
        };
        data_dir.join("submissions").join(format!("{safe}.jsonl"))
    }

    /// 读台账并回放成尝试列表。文件不存在 = 空台账，不是错误。
    pub fn open(data_dir: &Path, uid: &str) -> Result<Self, Error> {
        let path = Self::path_for(data_dir, uid);
        let mut attempts: Vec<Attempt> = Vec::new();
        if path.exists() {
            let text = std::fs::read_to_string(&path).map_err(|e| {
                ErrorKind::client(format!("读提交台账 {} 失败: {e}", path.display()))
            })?;
            for (lineno, line) in text.lines().enumerate() {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                // 半行（进程在写的时候被杀）会让整份台账读不出来。跳过坏行、
                // 保留其余——宁可少认一次尝试，也不能因为一行垃圾拒绝启动。
                let Ok(ev) = serde_json::from_str::<Event>(line) else {
                    continue;
                };
                let _ = lineno;
                Self::apply(&mut attempts, ev);
            }
        }
        Ok(Self { path, attempts })
    }

    /// 把一条事件作用到尝试列表上。
    fn apply(attempts: &mut Vec<Attempt>, ev: Event) {
        match ev {
            Event::Pending {
                uuid,
                start_ms,
                stop_ms,
            } => {
                // 同一 uuid 重复 pending（重放）只留一条。
                if attempts.iter().any(|a| a.uuid == uuid) {
                    return;
                }
                attempts.push(Attempt {
                    uuid,
                    start_ms,
                    stop_ms,
                    outcome: None,
                });
            }
            Event::Accepted { uuid, rrid } => {
                if let Some(a) = attempts.iter_mut().find(|a| a.uuid == uuid) {
                    a.outcome = Some(Some(rrid));
                } else {
                    // 只有 accepted 没有 pending（台账被截断）：仍然认，
                    // 时间段去重靠它。
                    attempts.push(Attempt {
                        uuid,
                        start_ms: 0,
                        stop_ms: 0,
                        outcome: Some(Some(rrid)),
                    });
                }
            }
            Event::Rejected { uuid } => {
                if let Some(a) = attempts.iter_mut().find(|a| a.uuid == uuid) {
                    a.outcome = Some(None);
                }
            }
        }
    }

    /// 追加一条事件。失败就是失败——台账写不下去时必须中止提交，
    /// 否则「有没有发出去」这件事就失去记录了。
    fn append(&mut self, ev: Event) -> Result<(), Error> {
        if let Some(parent) = self.path.parent() {
            super::session::ensure_dir(parent)?;
        }
        let mut line = serde_json::to_string(&ev)
            .map_err(|e| ErrorKind::client(format!("台账序列化失败: {e}")))?;
        line.push('\n');
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .map_err(|e| {
                ErrorKind::client(format!("打开提交台账 {} 失败: {e}", self.path.display()))
            })?;
        // 整行一次写出：O_APPEND 下单次 write 是原子的，多进程不会交错。
        f.write_all(line.as_bytes())
            .map_err(|e| ErrorKind::client(format!("写提交台账失败: {e}")))?;
        f.sync_all().ok();
        Self::apply(&mut self.attempts, ev);
        Ok(())
    }

    /// 是否还有结果未知的提交。
    pub fn has_pending(&self) -> bool {
        self.attempts.iter().any(|a| a.outcome.is_none())
    }

    /// 结果未知的那次尝试（用于在提示里说清是哪个时间段）。
    pub fn pending_range(&self) -> Option<(i64, i64)> {
        self.attempts
            .iter()
            .find(|a| a.outcome.is_none())
            .map(|a| (a.start_ms, a.stop_ms))
    }

    /// 与给定时间段**重叠**的未知结果尝试。
    ///
    /// 「结果未知」只在**同一段时间**才有害：那种情况再发一次会产生两条
    /// 时间重叠的记录，物理上不可能（一个人不能在同一段时间里跑两次）。
    /// 不同时间段的新提交跟它没有关系，拦下来只会把人卡死——上次 pending
    /// 是昨天的，今天照样该能跑。
    pub fn pending_overlapping(&self, start_ms: i64, stop_ms: i64) -> Option<(i64, i64)> {
        // 半开区间重叠：起点落在对方内部，或终点落在对方内部，或完整包住。
        let overlap = |a: &Attempt| start_ms < a.stop_ms && stop_ms > a.start_ms;
        self.attempts
            .iter()
            .filter(|a| a.outcome.is_none() && overlap(a))
            .map(|a| (a.start_ms, a.stop_ms))
            .next()
    }

    /// 提交前检查：本轮能不能发。
    pub fn check(&self, start_ms: i64, stop_ms: i64) -> Result<(), Error> {
        if start_ms <= 0 || stop_ms <= start_ms {
            return Err(ErrorKind::client("提交时间范围无效".into()));
        }
        // 只有**时间重叠**的未知结果才拦。不同时间段的新提交跟它无关，
        // 一刀切拦死会让一次超时把账号永久卡住。
        if let Some((ps, pe)) = self.pending_overlapping(start_ms, stop_ms) {
            return Err(ErrorKind::client(format!(
                "这段时间（{} ~ {}）上次提交结果未知，已阻止重复提交。\
                 未查到不代表未入账——请先到运动记录页刷新核对，确认没入账再重来。",
                fmt_ms(ps),
                fmt_ms(pe)
            )));
        }
        for a in &self.attempts {
            if let Some(Some(rrid)) = a.outcome {
                // 半开区间重叠：起点在对方内部，或终点在对方内部，或完整包住。
                if start_ms < a.stop_ms && stop_ms > a.start_ms {
                    return Err(ErrorKind::client(format!(
                        "该时间段（{} ~ {}）已有本工具提交的记录 #{rrid}，已阻止重复提交。\
                         请到运动记录查看详情。",
                        fmt_ms(a.start_ms),
                        fmt_ms(a.stop_ms)
                    )));
                }
            }
        }
        Ok(())
    }

    /// 请求发出**之前**调用：写 pending 并做检查。
    pub fn begin(&mut self, uuid: &str, start_ms: i64, stop_ms: i64) -> Result<(), Error> {
        self.check(start_ms, stop_ms)?;
        self.append(Event::Pending {
            uuid: uuid.to_string(),
            start_ms,
            stop_ms,
        })
    }

    /// 拿到 rrid 之后调用。
    pub fn accepted(&mut self, uuid: &str, rrid: i64) -> Result<(), Error> {
        self.append(Event::Accepted {
            uuid: uuid.to_string(),
            rrid,
        })
    }

    /// **明确**被拒（有错误码、能确定没入账）时调用。
    ///
    /// 只在这种情况下用：超时、连接断开、响应看不懂，都算「未知」，不写。
    pub fn rejected(&mut self, uuid: &str) -> Result<(), Error> {
        self.append(Event::Rejected {
            uuid: uuid.to_string(),
        })
    }
}

/// 台账里的毫秒时间戳转成人能读的 `MM-DD HH:MM`（本地时区）。
pub(crate) fn fmt_ms(ms: i64) -> String {
    if ms <= 0 {
        return "未知".into();
    }
    // 本地时区偏移：从系统时区数据库拿不到就按 UTC+8（本项目面向的学校都在
    // 国内）。只用于提示文案，不参与任何判定。
    let secs = ms.div_euclid(1000) + 8 * 3600;
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (h, mi) = (rem / 3600, (rem % 3600) / 60);
    // 从 1970-01-01 起算的日数转月日（不引 chrono，够用就行）
    let mut year_days = days;
    let mut year = 1970i64;
    loop {
        let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
        let yd = if leap { 366 } else { 365 };
        if year_days < yd {
            break;
        }
        year_days -= yd;
        year += 1;
    }
    let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
    let months = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    let mut month = 1;
    for m in months {
        if year_days < m {
            break;
        }
        year_days -= m;
        month += 1;
    }
    format!("{:02}-{:02} {:02}:{:02}", month, year_days + 1, h, mi)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "pf-guard-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    const H: i64 = 3_600_000;
    const T0: i64 = 1_700_000_000_000;

    #[test]
    fn empty_ledger_allows_first_submission() {
        let d = tmpdir("empty");
        let g = Guard::open(&d, "u1").unwrap();
        assert!(!g.has_pending());
        assert!(g.check(T0, T0 + H).is_ok());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn overlapping_pending_blocks_submission() {
        let d = tmpdir("pending");
        let mut g = Guard::open(&d, "u1").unwrap();
        g.begin("uuid-a", T0, T0 + H).unwrap();
        // 同一段时间：再发一次会产生两条时间重叠的记录，必须拦。
        let err = g.check(T0 + H / 2, T0 + H * 2).unwrap_err();
        assert!(err.to_string().contains("结果未知"), "实际: {err}");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// 不同时间段的未知结果**不该**拦住新提交。
    ///
    /// 拦它的理由只有「同一段时间不能跑两次」；昨天那次 pending 跟今天这段
    /// 没有关系，一刀切会让人卡死到手动删台账。
    #[test]
    fn non_overlapping_pending_allows_submission() {
        let d = tmpdir("pending_far");
        let mut g = Guard::open(&d, "u1").unwrap();
        g.begin("uuid-a", T0, T0 + H).unwrap();
        assert!(g.check(T0 + 10 * H, T0 + 11 * H).is_ok(), "不重叠就该放行");
        // 但「有未核对的结果」这件事仍然成立，调用方据此提示。
        assert!(g.has_pending());
        assert_eq!(g.pending_overlapping(T0 + 10 * H, T0 + 11 * H), None);
        assert_eq!(
            g.pending_overlapping(T0, T0 + H),
            Some((T0, T0 + H)),
            "重叠时要能报出是哪个时间段"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn accepted_blocks_overlapping_range_only() {
        let d = tmpdir("overlap");
        let mut g = Guard::open(&d, "u1").unwrap();
        g.begin("uuid-b", T0, T0 + H).unwrap();
        g.accepted("uuid-b", 12345).unwrap();
        // 重叠：起点落在里面
        let e = g.check(T0 + H / 2, T0 + 2 * H).unwrap_err();
        assert!(e.to_string().contains("#12345"), "实际: {e}");
        // 重叠：终点落在里面
        assert!(g.check(T0 - H, T0 + H / 2).is_err());
        // 重叠：完整包住
        assert!(g.check(T0 - H, T0 + 2 * H).is_err());
        // 不重叠：紧接着的下一段（半开区间，边界相接不算重叠）
        assert!(g.check(T0 + H, T0 + 2 * H).is_ok());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn rejected_releases_the_slot() {
        let d = tmpdir("rejected");
        let mut g = Guard::open(&d, "u1").unwrap();
        g.begin("uuid-c", T0, T0 + H).unwrap();
        assert!(g.has_pending());
        g.rejected("uuid-c").unwrap();
        assert!(!g.has_pending());
        // 被拒之后同时间段可以重来（明确没入账）
        assert!(g.check(T0, T0 + H).is_ok());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn reopened_ledger_sees_pending_from_previous_process() {
        let d = tmpdir("reopen");
        {
            let mut g = Guard::open(&d, "u1").unwrap();
            g.begin("uuid-d", T0, T0 + H).unwrap();
            // 进程在这里「崩溃」——没有 accepted/rejected
        }
        let g = Guard::open(&d, "u1").unwrap();
        assert!(g.has_pending(), "重启后必须仍认为上次结果未知");
        // 同一段时间仍然拦
        assert!(g.check(T0, T0 + H).is_err());
        // 不同时间段放行——崩溃留下的 pending 不该变成永久锁
        assert!(g.check(T0 + 5 * H, T0 + 6 * H).is_ok());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn accounts_are_isolated() {
        let d = tmpdir("isolate");
        let mut a = Guard::open(&d, "u1").unwrap();
        a.begin("uuid-e", T0, T0 + H).unwrap();
        let b = Guard::open(&d, "u2").unwrap();
        assert!(!b.has_pending(), "另一个账号不该被牵连");
        assert!(b.check(T0, T0 + H).is_ok());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn corrupt_lines_are_skipped_not_fatal() {
        let d = tmpdir("corrupt");
        let path = Guard::path_for(&d, "u1");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            "{\"state\":\"pending\",\"uuid\":\"ok-1\",\"start_ms\":1700000000000,\"stop_ms\":1700003600000}\n\
             {\"state\":\"pend\n",
        )
        .unwrap();
        let g = Guard::open(&d, "u1").unwrap();
        assert!(g.has_pending(), "好行必须仍然生效");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn uuid_with_separators_stays_inside_dir() {
        let d = tmpdir("traversal");
        let p = Guard::path_for(&d, "../../etc/passwd");
        assert!(!p.to_string_lossy().contains(".."), "路径: {}", p.display());
        let _ = std::fs::remove_dir_all(&d);
    }
}

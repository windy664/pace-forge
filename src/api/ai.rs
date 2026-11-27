//! AI 运动（与跑步完全独立的另一套接口）。
//!
//! 协议依据 neko（NekoSportsWorldTool，7.3.70 实测）：
//! - 项目列表 `GET /api/v1/sport/ai/list`
//! - 项目详情 `GET /api/v1/sport/ai/info?sportId=`（`type`：1 计次 / 2 计时）
//! - 提交 `POST /api/v65/sport/ai/record/upload`（9 字段，无需视频）
//! - 记录 `GET /api/v66/sport/ai/record/infos?sportId=&pageSize=&pageNum=`
//!
//! 数值仿真人：计次类 score=个数、speed=0、消耗=个数×0.07；计时类
//! score=用时毫秒、speed=个/分、消耗=秒×0.2；坐位体前屈(id=16)消耗为 0。
//! `scoreDate` 填过去时刻即可补签（neko GUI 支持前 60 天批量）。

use serde_json::{Value, json};

use super::client::{Client, expect_ok, uuid_v4};
use crate::crypto::consts::hosts;
use crate::error::Error;

/// AI 运动项目列表接口。
pub const AI_LIST_PATH: &str = "/api/v1/sport/ai/list";
/// AI 运动记录上传接口。
pub const AI_UPLOAD_PATH: &str = "/api/v65/sport/ai/record/upload";
/// AI 运动记录列表接口。
pub const AI_RECORDS_PATH: &str = "/api/v66/sport/ai/record/infos";

/// 真人记录区间取来的常量（neko 标定）：持续频率 90 个/分、单次 240ms。
const REPS_PER_MIN: f64 = 90.0;
const MS_PER_REP: f64 = 240.0;
/// 最短时长 30 秒：再短不像一次真实练习。
const MIN_DURATION_MS: i64 = 30_000;

/// AI 运动项目。
#[derive(Debug, Clone)]
pub struct AiSport {
    /// 项目 id，提交时原样带回。
    pub id: i64,
    /// 项目名，仅展示。
    pub name: String,
}

/// 提交意图：按分钟或按次数。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AiMode {
    /// 按分钟计成绩，参数是分钟数。
    Minutes(i64),
    /// 按次数计成绩，参数是次数。
    Count(i64),
}

/// 项目详情：`type` 决定成绩语义（1 计次 / 2 计时）。
#[derive(Debug, Clone, Copy)]
pub struct SportInfo {
    /// 成绩类型：1 计次 / 2 计时。
    pub sport_type: i64,
}

/// 拉项目列表。
pub fn fetch_list(client: &Client) -> Result<Vec<AiSport>, Error> {
    let raw = client.request(
        "GET",
        &format!("{}{}", hosts::RUN, AI_LIST_PATH),
        Some(&json!({})),
    )?;
    let v = expect_ok(&raw)?;
    let data = data_field(&v);
    let arr = data
        .get("list")
        .and_then(|x| x.as_array())
        .cloned()
        .unwrap_or_default();
    Ok(arr
        .iter()
        .filter_map(|it| {
            Some(AiSport {
                id: it.get("id")?.as_i64()?,
                name: it
                    .get("name")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string(),
            })
        })
        .collect())
}

/// 查项目详情；失败按计次类处理（与 neko 一致）。
pub fn fetch_info(client: &Client, sport_id: i64) -> SportInfo {
    let path = format!(
        "{}{}?sportId={sport_id}",
        hosts::RUN,
        "/api/v1/sport/ai/info"
    );
    let sport_type = client
        .request("GET", &path, Some(&json!({})))
        .ok()
        .and_then(|raw| expect_ok(&raw).ok())
        .map(|v| data_field(&v))
        .and_then(|d| d.get("type").and_then(|x| x.as_i64()))
        .unwrap_or(1);
    SportInfo { sport_type }
}

/// 组装 upload 报文（纯函数，便于离线单测）。
///
/// `jitter` 取 [0.9, 1.1] 的随机系数（±10%）；`at` 为指定补签时刻，
/// `None` 时 scoreDate = now - timeConsume（刚练完）。
pub fn record_fields(
    info_type: i64,
    mode: AiMode,
    sport_id: i64,
    jitter: f64,
    now_ms: i64,
    at: Option<i64>,
    uuid: &str,
) -> Value {
    let (body_type, score, time_consume, speed, consume) = match (info_type, mode) {
        // 计时类：score=用时毫秒；个数只用来推 speed/时长
        (2, AiMode::Minutes(mins)) => {
            let ms = mins * 60_000;
            let reps =
                crate::number::round_to_i64(REPS_PER_MIN * mins as f64 * jitter).unwrap_or(0);
            let speed =
                crate::number::round_to_i64(reps as f64 / (ms as f64 / 1000.0) * 60.0).unwrap_or(0);
            (2, ms.to_string(), ms, speed, (ms / 1000) as f64 * 0.2)
        }
        (2, AiMode::Count(reps)) => {
            let ms = crate::number::f64_to_i64(reps as f64 * MS_PER_REP * jitter)
                .unwrap_or(0)
                .max(MIN_DURATION_MS);
            let speed =
                crate::number::round_to_i64(reps as f64 / (ms as f64 / 1000.0) * 60.0).unwrap_or(0);
            (2, ms.to_string(), ms, speed, (ms / 1000) as f64 * 0.2)
        }
        // 计次类：score=个数；speed 固定 0
        (_, AiMode::Minutes(mins)) => {
            let reps =
                crate::number::round_to_i64(REPS_PER_MIN * mins as f64 * jitter).unwrap_or(0);
            let ms = mins * 60_000;
            (1, reps.to_string(), ms, 0, reps as f64 * 0.07)
        }
        (_, AiMode::Count(reps)) => {
            let ms = crate::number::trunc_to_i64(reps as f64 / REPS_PER_MIN * 60_000.0)
                .unwrap_or(0)
                .max(MIN_DURATION_MS);
            (1, reps.to_string(), ms, 0, reps as f64 * 0.07)
        }
    };
    // 坐位体前屈：无消耗
    let consume = if sport_id == 16 {
        "0".to_string()
    } else {
        format!("{consume:.1}")
    };
    let score_date = at.unwrap_or(now_ms - time_consume);
    json!({
        "sportId": sport_id,
        "type": body_type,
        "score": score,
        "timeConsume": time_consume,
        "speed": speed.to_string(),
        "consume": consume,
        "scoreDate": score_date,
        "uuid": uuid,
        "taskId": 0,
    })
}

/// 提交一条 AI 运动记录。
pub fn upload(
    client: &Client,
    sport_id: i64,
    mode: AiMode,
    at: Option<i64>,
) -> Result<Value, Error> {
    use rand::Rng;
    let info = fetch_info(client, sport_id);
    let now = crate::crypto::envelope::now_ms();
    let jitter = 0.9 + rand::rng().random::<f64>() * 0.2;
    let body = record_fields(info.sport_type, mode, sport_id, jitter, now, at, &uuid_v4());
    let raw = client.request(
        "POST",
        &format!("{}{}", hosts::RUN, AI_UPLOAD_PATH),
        Some(&body),
    )?;
    expect_ok(&raw)
}

/// 拉某项目的记录（按天分组），返回 data 原样供 CLI 展示。
pub fn fetch_records(client: &Client, sport_id: i64, page_size: i64) -> Result<Value, Error> {
    let path = format!(
        "{}{}?sportId={sport_id}&pageSize={page_size}&pageNum=1",
        hosts::RUN,
        AI_RECORDS_PATH
    );
    let raw = client.request("GET", &path, Some(&json!({})))?;
    let v = expect_ok(&raw)?;
    Ok(data_field(&v))
}

/// 取 `data` 字段；有的接口把它字符串化了，两种形态都认。
fn data_field(v: &Value) -> Value {
    match v.get("data") {
        Some(Value::String(s)) => serde_json::from_str(s).unwrap_or(Value::Null),
        Some(d) => d.clone(),
        None => v.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_730_000_000_000;

    #[test]
    fn count_type_score_is_reps_speed_zero() {
        let v = record_fields(1, AiMode::Count(100), 2, 1.0, NOW, None, "u");
        assert_eq!(v["type"], json!(1));
        assert_eq!(v["score"], json!("100"));
        assert_eq!(v["speed"], json!("0"));
        // 100 个 × 0.07 = 7.0
        assert_eq!(v["consume"], json!("7.0"));
        // 100 / 90 分 ≈ 66.7s
        assert_eq!(v["timeConsume"], json!(66_666));
        assert_eq!(v["scoreDate"], json!(NOW - 66_666));
        assert_eq!(v["taskId"], json!(0));
    }

    #[test]
    fn timed_type_score_is_millis() {
        let v = record_fields(2, AiMode::Minutes(2), 5, 1.0, NOW, None, "u");
        assert_eq!(v["type"], json!(2));
        assert_eq!(v["score"], json!("120000"));
        assert_eq!(v["timeConsume"], json!(120_000));
        // speed = 个/分 ≈ 90
        assert_eq!(v["speed"], json!("90"));
        // 120s × 0.2 = 24.0
        assert_eq!(v["consume"], json!("24.0"));
    }

    #[test]
    fn seated_reach_has_zero_consume() {
        let v = record_fields(1, AiMode::Count(30), 16, 1.0, NOW, None, "u");
        assert_eq!(v["consume"], json!("0"));
    }

    #[test]
    fn short_count_clamps_to_min_duration() {
        let v = record_fields(1, AiMode::Count(1), 2, 1.0, NOW, None, "u");
        assert_eq!(v["timeConsume"], json!(MIN_DURATION_MS));
    }

    #[test]
    fn explicit_at_overrides_score_date() {
        let v = record_fields(
            1,
            AiMode::Count(50),
            2,
            1.0,
            NOW,
            Some(NOW - 86_400_000),
            "u",
        );
        assert_eq!(v["scoreDate"], json!(NOW - 86_400_000));
    }
}

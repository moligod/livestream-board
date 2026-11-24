// Adapted from CC Switch v4.0.40 (MIT; Copyright (c) 2025 Jason Young).
//! DeepSeek Harness（DSH）会话日志用量导入。
//!
//! DSH 把每个会话写成
//! `$DSH_HOME/sessions/<项目目录>/<会话目录>/session.v4.jsonl.zstd`
//! （关闭压缩时是 `session.v4.jsonl`）。`.zstd` 文件是一串首尾相接、各自独立
//! 可解的 zstd 帧：每追加一个事件批次就追加一帧，所以不能整文件一次解压，
//! 必须逐帧解码，并容忍末尾那一帧只写了一半。
//!
//! 一次模型调用的用量挂在 `assistant/message` 事件上：
//!
//! ```json
//! {"type":"assistant/message","time":1792746645334,"data":{
//!   "turn":1,"step":1,
//!   "message":{"id":"...","source":{"kind":"model","provider":"deepseek-account","model":"deepseek-flash"}},
//!   "usage":{"inputTokens":1895,"outputTokens":561,"cacheReadTokens":8704,"cacheWriteTokens":0,"totalTokens":11160},
//!   "stream":[...]}}
//! ```
//!
//! `inputTokens` 是**不含缓存**的新增输入（DSH 的三种 token 互不重叠，
//! `totalTokens = input + output + cacheRead + cacheWrite`），所以按 fresh-input
//! 语义入库。耗时用 `step/start` 到 `assistant/message` 的时间差估算，首字时间
//! 取 `stream` 里最早的一块时间戳。
//!
//! 数据流：
//! ```text
//! ~/.dsh/sessions/*/*/session.v4.jsonl.zstd → 逐帧解压 → 解析 assistant/message
//!   → 去重 → 费用计算 → proxy_request_logs 表
//! ```

use crate::database::{lock_conn, Database};
use crate::error::AppError;
use crate::proxy::usage::parser::TokenUsage;
use crate::services::{
    session_usage::{
        cost_columns, estimated_first_token_ms, estimated_latency_ms, metadata_modified_nanos,
        SessionSyncResult, SyncCursor,
    },
    sql_helpers::INPUT_TOKEN_SEMANTICS_FRESH,
    usage_stats::find_model_pricing,
};
use rust_decimal::Decimal;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    ffi::OsStr,
    fs,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};

/// 写入 `proxy_request_logs.app_type` 的应用标识。
///
/// DSH 没有路由服务，用量只来自会话日志。Web 和 Desktop 共用 `$DSH_HOME/sessions`，
/// 会话里也没记是哪一端建的，所以两端的用量合并记在 `dsh` 名下。
pub(crate) const DSH_APP_TYPE: &str = "dsh";

/// `session_usage_dedup` / `proxy_request_logs.data_source` 里的来源标识。
const DSH_DATA_SOURCE: &str = "dsh_session";
/// 占位供应商 ID：会话日志不知道用户在 CC Switch 里建过哪张卡，和其余应用一致。
const DSH_PROVIDER_ID: &str = "_dsh_session";
/// `provider_type`：详情面板里标明这条记录来自会话日志导入。
const DSH_PROVIDER_TYPE: &str = "dsh_session";
/// DeepSeek 账号（OAuth 登录）路由的请求单独记一个占位供应商，用量面板能分开看
/// 登录账号和 API Key 供应商各用了多少。Web 和 Desktop 共用同一份登录。
const DSH_ACCOUNT_PROVIDER_ID: &str = "_dsh_account_session";
const DSH_ACCOUNT_PROVIDER_TYPE: &str = "dsh_account_session";
/// DSH 账号路由在会话日志 `source.provider` 里的名字（`@deepseek-ai/dsh-llm-deepseek-account`）。
const DSH_ACCOUNT_ROUTE: &str = "deepseek-account";

/// zstd 帧魔数（little-endian 字节序）。
const ZSTD_MAGIC: u32 = 0xFD2F_B528;

/// 一张从会话日志里解析出来、还没入库的请求。
struct PendingRequest {
    request_id: String,
    model: String,
    usage: TokenUsage,
    created_at_ms: i64,
    latency_ms: i64,
    first_token_ms: Option<i64>,
    session_id: String,
    /// 走的是 DeepSeek 账号（OAuth）路由
    via_account: bool,
}

/// 扫描并导入 DSH 会话用量（后台定时任务与手动同步共用）。
pub(crate) fn sync_dsh_usage(
    db: &Database,
    cursors: &crate::services::session_usage::SyncCursors,
) -> Result<SessionSyncResult, AppError> {
    sync_dsh_usage_from_with_cursors(db, &dsh_sessions_root(), cursors)
}

/// DSH 的会话根目录：`$DSH_HOME/sessions`，未设置环境变量时用 `~/.dsh/sessions`。
///
/// 与 DSH 自身、会话浏览器（`dsh_config::data_dir`）同一套规则，用户把 home
/// 指到别处时统计跟着走。
pub(crate) fn dsh_sessions_root() -> PathBuf {
    crate::dsh_config::data_dir().join("sessions")
}

/// [`sync_dsh_usage`] 的可注入根目录版本，测试用临时目录。
#[cfg(test)]
pub(crate) fn sync_dsh_usage_from(
    db: &Database,
    root: &Path,
) -> Result<SessionSyncResult, AppError> {
    sync_dsh_usage_from_with_cursors(
        db,
        root,
        &crate::services::session_usage::load_sync_cursors(db)?,
    )
}

fn sync_dsh_usage_from_with_cursors(
    db: &Database,
    root: &Path,
    cursors: &crate::services::session_usage::SyncCursors,
) -> Result<SessionSyncResult, AppError> {
    let mut result = SessionSyncResult::default();
    // 目录层级：sessions/<项目>/<会话>/session.v4.jsonl(.zstd)
    let Ok(projects) = fs::read_dir(root) else {
        return Ok(result);
    };
    for project in projects.flatten() {
        let Ok(sessions) = fs::read_dir(project.path()) else {
            continue;
        };
        for session in sessions.flatten() {
            let session_dir = session.path();
            if !session_dir.is_dir() {
                continue;
            }
            // 会话目录名就是编码后的 session id（`session-<uuid>`），先写到
            // 目录名上，读文件时不用等表头也是稳定的。
            let session_id = session.file_name().to_string_lossy().into_owned();
            let Ok(entries) = fs::read_dir(&session_dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if !is_session_log(&path) {
                    continue;
                }
                let key = format!("dsh:{}", path.to_string_lossy());
                match sync_file(db, &path, &session_id, &key, cursors) {
                    Ok(part) => result.merge(part),
                    Err(error) => result
                        .errors
                        .push(format!("DeepSeek Harness 会话 {}: {error}", path.display())),
                }
            }
        }
    }
    Ok(result)
}

/// 只认 DSH 的会话日志文件：`session.<版本>.jsonl` 或 `session.<版本>.jsonl.zstd`。
fn is_session_log(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }
    let Some(name) = path.file_name().and_then(OsStr::to_str) else {
        return false;
    };
    name.starts_with("session.") && (name.ends_with(".jsonl") || name.ends_with(".jsonl.zstd"))
}

/// 一次同步从会话日志里读到的增量。
struct SessionChunk {
    /// 只含完整帧（`.zstd`）或完整行（纯 JSONL）的 JSONL 文本。
    text: Vec<u8>,
    /// 每个完整帧/行在 `text` 里的结束位置，以及它在文件里的绝对结束位置。
    boundaries: Vec<(usize, i64)>,
    /// 续读起点是否落在合法边界上；不是说明文件被重写过，需要从头重扫。
    boundary_ok: bool,
}

fn sync_file(
    db: &Database,
    path: &Path,
    session_id: &str,
    key: &str,
    cursors: &HashMap<String, SyncCursor>,
) -> Result<SessionSyncResult, AppError> {
    let metadata = fs::metadata(path).map_err(|error| AppError::io(path, error))?;
    let modified = metadata_modified_nanos(&metadata);
    let size = metadata.len() as i64;
    let cursor = cursors.get(key);
    // 上次已经处理到文件末尾、文件也没变：直接跳过。
    if cursor.is_some_and(|cursor| {
        cursor.last_modified == modified
            && cursor.last_byte_offset == Some(size)
            && cursor.parser_state.is_some()
    }) {
        return Ok(SessionSyncResult::default());
    }

    // 从上次处理到的字节位置续读。会话日志只会追加，整份重读/重解压是 O(文件长度) 的
    // 无谓开销：一个长会话每次同步都要把所有历史重新解一遍。
    let mut start = cursor
        // Older cursors did not save step timestamps. Replay once to recover the context;
        // the durable request ledger prevents previously imported usage from being counted twice.
        .filter(|cursor| cursor.parser_state.is_some())
        .and_then(|cursor| cursor.last_byte_offset)
        .filter(|offset| (0..=size).contains(offset))
        .unwrap_or(0);
    let mut chunk = read_chunk(path, start)?;
    if !chunk.boundary_ok {
        // 续读点不在帧头说明文件被重写过，从头重扫（去重账本保证不会重复入库）。
        start = 0;
        chunk = read_chunk(path, 0)?;
    }
    // 还没有完整帧（刚创建、正在写第一帧）或没有新内容：这一轮先不动游标，下一轮再看，
    // 别把它当成同步错误。
    if chunk.text.is_empty() {
        return Ok(SessionSyncResult {
            files_scanned: 1,
            ..Default::default()
        });
    }
    let mut step_starts: HashMap<(u64, u64), i64> = if start > 0 {
        serde_json::from_str::<Vec<((u64, u64), i64)>>(
            cursor
                .and_then(|cursor| cursor.parser_state.as_deref())
                .unwrap_or("[]"),
        )
        .map_err(|error| AppError::Config(format!("Invalid DSH session parser state: {error}")))?
        .into_iter()
        .collect()
    } else {
        HashMap::new()
    };
    let pending = collect_requests(&chunk.text, session_id, &mut step_starts);
    let parser_state = serde_json::to_string(&step_starts.into_iter().collect::<Vec<_>>())
        .map_err(|error| AppError::Config(error.to_string()))?;
    // 游标推进到这批增量最后一个完整帧的末尾：下一轮只读新追加的部分。测试与真实日志里
    // 一次 append 就是一帧，所以窗口边界永远落在帧边界上。
    let next_offset = chunk
        .boundaries
        .last()
        .map(|(_, file_end)| *file_end)
        .unwrap_or(start);

    let mut result = SessionSyncResult {
        files_scanned: 1,
        ..Default::default()
    };
    let mut conn = lock_conn!(db.conn);
    let tx = conn.transaction()?;
    for request in pending {
        // 先记长期收据：日志被重写、分叉或明细行被汇总剪掉之后，都不会重导。
        if tx.execute(
            "INSERT OR IGNORE INTO session_usage_dedup (data_source, request_id, semantic_id, has_entry_id)
             VALUES (?1, ?2, ?2, 1)",
            rusqlite::params![DSH_DATA_SOURCE, request.request_id],
        )? == 0
        {
            result.skipped += 1;
            continue;
        }
        // DSH 的模型 id 可能带 `<路由>:<模型>` 前缀（如 `global:deepseek-v4.1-flash`）。
        // 计价按去掉前缀的模型名，并把真正用于计价的 id 记在 pricing_model 上，
        // 面板的模型分组也按它收敛，避免同一个模型被拆成两行。
        let pricing_model = resolve_pricing_model(&tx, &request.model);
        let priced_as = pricing_model.as_deref().unwrap_or(&request.model);
        let amounts = cost_columns(
            DSH_APP_TYPE,
            &request.usage,
            find_model_pricing(&tx, priced_as).as_ref(),
            Decimal::ONE,
        );
        tx.execute(
            "INSERT OR IGNORE INTO proxy_request_logs (
                request_id, provider_id, app_type, model, request_model, pricing_model,
                input_tokens, output_tokens, cache_read_tokens, cache_creation_tokens,
                input_cost_usd, output_cost_usd, cache_read_cost_usd, cache_creation_cost_usd, total_cost_usd,
                latency_ms, first_token_ms, status_code, session_id, provider_type, is_streaming,
                cost_multiplier, created_at, data_source, input_token_semantics
            ) VALUES (?1, ?2, ?3, ?4, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, 200, ?17, ?18, 1, '1', ?19, ?20, ?21)",
            rusqlite::params![
                request.request_id,
                if request.via_account {
                    DSH_ACCOUNT_PROVIDER_ID
                } else {
                    DSH_PROVIDER_ID
                },
                DSH_APP_TYPE,
                request.model,
                pricing_model,
                request.usage.input_tokens,
                request.usage.output_tokens,
                request.usage.cache_read_tokens,
                request.usage.cache_creation_tokens,
                amounts[0],
                amounts[1],
                amounts[2],
                amounts[3],
                amounts[4],
                request.latency_ms,
                request.first_token_ms,
                request.session_id,
                if request.via_account {
                    DSH_ACCOUNT_PROVIDER_TYPE
                } else {
                    DSH_PROVIDER_TYPE
                },
                request.created_at_ms / 1000,
                DSH_DATA_SOURCE,
                INPUT_TOKEN_SEMANTICS_FRESH,
            ],
        )?;
        result.imported += 1;
    }
    update_dsh_sync_state(&tx, key, modified, size, next_offset, &parser_state)?;
    tx.commit()?;
    Ok(result)
}

/// 游标同时记下文件大小（判断“这一轮没新内容”）和已处理到的字节位置（下一轮从这里续读）。
fn update_dsh_sync_state(
    conn: &rusqlite::Connection,
    file_path: &str,
    last_modified: i64,
    file_size: i64,
    byte_offset: i64,
    parser_state: &str,
) -> Result<(), AppError> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0);
    conn.prepare_cached(
        "INSERT OR REPLACE INTO session_log_sync
             (file_path, last_modified, last_line_offset, last_synced_at, last_byte_offset, parser_state)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
    )
    .and_then(|mut stmt| {
        stmt.execute(rusqlite::params![
            file_path,
            last_modified,
            file_size,
            now,
            byte_offset,
            parser_state
        ])
    })
    .map_err(|error| AppError::Database(format!("更新同步状态失败: {error}")))?;
    Ok(())
}

/// 从 `start` 读到文件末尾，只留下完整的帧或完整的行。
fn read_chunk(path: &Path, start: i64) -> Result<SessionChunk, AppError> {
    let mut file = fs::File::open(path).map_err(|error| AppError::io(path, error))?;
    if start > 0 {
        file.seek(SeekFrom::Start(start as u64))
            .map_err(|error| AppError::io(path, error))?;
    }
    let mut tail = Vec::new();
    file.read_to_end(&mut tail)
        .map_err(|error| AppError::io(path, error))?;
    if path.extension().and_then(OsStr::to_str) == Some("zstd") {
        decode_zstd_frames(&tail, start, path)
    } else {
        Ok(complete_lines(&tail, start))
    }
}

/// 逐帧解压拼接的 zstd 流：只在解出完整帧时前移，末尾写了一半的帧留给下一轮。
fn decode_zstd_frames(tail: &[u8], start: i64, path: &Path) -> Result<SessionChunk, AppError> {
    let boundary_ok =
        tail.len() < 4 || u32::from_le_bytes([tail[0], tail[1], tail[2], tail[3]]) == ZSTD_MAGIC;
    let (frames, _torn) = scan_zstd_frames(tail);
    let mut text = Vec::new();
    let mut boundaries = Vec::with_capacity(frames.len());
    for (frame_start, frame_end) in frames {
        let decoded = zstd::stream::decode_all(&tail[frame_start..frame_end]).map_err(|error| {
            AppError::Message(format!(
                "解压 DSH 会话日志帧失败（{} 第 {} 字节处）: {error}",
                path.display(),
                start + frame_start as i64
            ))
        })?;
        text.extend_from_slice(&decoded);
        boundaries.push((text.len(), start + frame_end as i64));
    }
    Ok(SessionChunk {
        text,
        boundaries,
        boundary_ok,
    })
}

/// 纯 JSONL：只取到最后一个换行符，没写完的那一行留给下一轮。
fn complete_lines(tail: &[u8], start: i64) -> SessionChunk {
    let end = tail
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map(|index| index + 1)
        .unwrap_or(0);
    let text = tail[..end].to_vec();
    SessionChunk {
        text,
        boundaries: vec![(end, start + end as i64)],
        boundary_ok: true,
    }
}

/// 按 zstd 帧结构扫描拼接流，返回完整帧的字节区间和可能存在的残缺尾帧起点。
///
/// 只做结构扫描、不解压：这样才能在不解码的情况下判断“最后一帧是否写完整”。
fn scan_zstd_frames(buffer: &[u8]) -> (Vec<(usize, usize)>, Option<usize>) {
    let mut frames = Vec::new();
    let mut offset = 0usize;
    while offset < buffer.len() {
        let start = offset;
        if buffer.len() - offset < 4 {
            return (frames, Some(start));
        }
        let magic = u32::from_le_bytes([
            buffer[offset],
            buffer[offset + 1],
            buffer[offset + 2],
            buffer[offset + 3],
        ]);
        if magic != ZSTD_MAGIC {
            return (frames, Some(start));
        }
        offset += 4;
        if offset == buffer.len() {
            return (frames, Some(start));
        }
        let descriptor = buffer[offset];
        offset += 1;
        if descriptor & 0x18 != 0 {
            // 保留位非零：不是合法的帧头，当作残缺尾帧停下。
            return (frames, Some(start));
        }
        let content_size_flag = descriptor >> 6;
        let single_segment = descriptor & 0x20 != 0;
        let checksum = descriptor & 0x04 != 0;
        let dictionary_flag = descriptor & 0x03;
        let dictionary_bytes = if dictionary_flag == 3 {
            4
        } else {
            usize::from(dictionary_flag)
        };
        let content_size_bytes = if content_size_flag == 0 {
            if single_segment {
                1
            } else {
                0
            }
        } else {
            1usize << content_size_flag
        };
        let window_descriptor_bytes = if single_segment { 0 } else { 1 };
        let remaining = window_descriptor_bytes + dictionary_bytes + content_size_bytes;
        if buffer.len() - offset < remaining {
            return (frames, Some(start));
        }
        offset += remaining;

        loop {
            if buffer.len() - offset < 3 {
                return (frames, Some(start));
            }
            let header = u32::from(buffer[offset])
                | (u32::from(buffer[offset + 1]) << 8)
                | (u32::from(buffer[offset + 2]) << 16);
            offset += 3;
            let last_block = header & 1 != 0;
            let block_type = (header >> 1) & 0x03;
            let block_size = (header >> 3) as usize;
            if block_type == 0x03 {
                // 保留块类型：结构损坏，不再往后扫。
                return (frames, Some(start));
            }
            let payload_bytes = if block_type == 0x01 { 1 } else { block_size };
            if buffer.len() - offset < payload_bytes {
                return (frames, Some(start));
            }
            offset += payload_bytes;
            if last_block {
                break;
            }
        }
        if checksum {
            if buffer.len() - offset < 4 {
                return (frames, Some(start));
            }
            offset += 4;
        }
        frames.push((start, offset));
    }
    (frames, None)
}

/// 逐行解析会话日志，收集所有带用量的 `assistant/message`。
fn collect_requests(
    text: &[u8],
    session_id: &str,
    step_starts: &mut HashMap<(u64, u64), i64>,
) -> Vec<PendingRequest> {
    let mut requests = Vec::new();
    // (turn, step) → step/start 时间，用来估算本次请求的耗时与首字。
    for line in text.split(|byte| *byte == b'\n') {
        if line.iter().all(|byte| byte.is_ascii_whitespace()) {
            continue;
        }
        let Ok(value) = serde_json::from_slice::<Value>(line) else {
            continue;
        };
        match value.get("type").and_then(Value::as_str) {
            Some("step/start") => {
                let data = &value["data"];
                let key = (
                    data.get("turn").and_then(Value::as_u64).unwrap_or(u64::MAX),
                    data.get("step").and_then(Value::as_u64).unwrap_or(u64::MAX),
                );
                if let Some(time) = value.get("time").and_then(Value::as_i64) {
                    step_starts.insert(key, time);
                }
            }
            Some("assistant/message") => {
                if let Some(request) = parse_assistant_message(&value, session_id, step_starts) {
                    requests.push(request);
                    step_starts.remove(&(
                        value["data"]["turn"].as_u64().unwrap_or(u64::MAX),
                        value["data"]["step"].as_u64().unwrap_or(u64::MAX),
                    ));
                }
            }
            _ => {}
        }
    }
    requests
}

fn parse_assistant_message(
    value: &Value,
    session_id: &str,
    step_starts: &HashMap<(u64, u64), i64>,
) -> Option<PendingRequest> {
    let data = value.get("data")?;
    let usage = data.get("usage")?;
    let input_tokens = token_count(usage.get("inputTokens"));
    let output_tokens = token_count(usage.get("outputTokens"));
    let cache_read_tokens = token_count(usage.get("cacheReadTokens"));
    let cache_creation_tokens = token_count(usage.get("cacheWriteTokens"));
    if input_tokens == 0
        && output_tokens == 0
        && cache_read_tokens == 0
        && cache_creation_tokens == 0
    {
        return None;
    }
    let created_at_ms = value.get("time").and_then(Value::as_i64)?;
    let message = data.get("message");
    let message_id = message
        .and_then(|message| message.get("id"))
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .map(str::to_owned)
        // 没有 message id 时退化为整行内容的指纹，同样能保证不重复入库。
        .unwrap_or_else(|| format!("{:x}", Sha256::digest(value.to_string().as_bytes())));
    let model = message
        .and_then(|message| message.pointer("/source/model"))
        .and_then(Value::as_str)
        .filter(|model| !model.is_empty())
        .unwrap_or("unknown");
    let via_account = message
        .and_then(|message| message.pointer("/source/provider"))
        .and_then(Value::as_str)
        == Some(DSH_ACCOUNT_ROUTE);

    let step_start = step_starts.get(&(
        data.get("turn").and_then(Value::as_u64).unwrap_or(u64::MAX),
        data.get("step").and_then(Value::as_u64).unwrap_or(u64::MAX),
    ));
    let latency_ms = step_start
        .and_then(|start| estimated_latency_ms(*start, created_at_ms))
        .unwrap_or(0);
    let first_token_ms = match (step_start, first_output_time(data.get("stream"))) {
        (Some(start), Some(first_output)) if latency_ms > 0 => {
            estimated_first_token_ms(*start, first_output, latency_ms)
        }
        _ => None,
    };

    Some(PendingRequest {
        // 按 message id 去重，不按会话：分叉出来的会话会把父会话的前缀原样抄进新日志
        // （header.seedLength），同一次模型调用会在两个文件里各出现一次，只应记一次。
        request_id: format!("dsh:{message_id}"),
        model: model.to_owned(),
        usage: TokenUsage {
            input_tokens,
            output_tokens,
            cache_read_tokens,
            cache_creation_tokens,
            cache_creation_1h_tokens: 0,
            model: Some(model.to_owned()),
            message_id: Some(message_id),
        },
        created_at_ms,
        latency_ms,
        first_token_ms,
        session_id: session_id.to_owned(),
        via_account,
    })
}

/// `stream` 里最早一块的时间戳：`time` 是整块的发送时间，`time0` 是打包块的起始时间。
fn first_output_time(stream: Option<&Value>) -> Option<i64> {
    stream?
        .as_array()?
        .iter()
        .flat_map(|entry| [entry.get("time"), entry.get("time0")])
        .flatten()
        .filter_map(Value::as_i64)
        .min()
}

fn token_count(value: Option<&Value>) -> u32 {
    value
        .and_then(Value::as_u64)
        .unwrap_or(0)
        .min(u64::from(u32::MAX)) as u32
}

/// DSH 的模型 id 可能带 `<路由>:<模型>` 前缀（例如网关写入的
/// `global:deepseek-v4.1-flash`）。原样查得到定价时不动；查不到就把去掉前缀的
/// 裸模型名交给 `pricing_model` 列。
///
/// 关键在于**当时有没有价都要记下裸名**：`pricing_model` 为空时，
/// `backfill_missing_usage_costs` 会回落到 `model` 列，而
/// `clean_model_id_for_pricing` 取的是 `:` **前**的部分，`global:<新模型>` 会被
/// 当成 `global` 去查，之后定价表补上 `<新模型>` 也永远补不回费用；记下裸名后，
/// 补价时按 `pricing_model` 就能命中。
///
/// 不直接改写 `model` 列：请求日志仍显示 DSH 真实写入的 id，和其余会话导入器
/// 一致；只有计价与模型分组收敛到干净的名字。
fn resolve_pricing_model(conn: &rusqlite::Connection, model: &str) -> Option<String> {
    if find_model_pricing(conn, model).is_some() {
        return None;
    }
    let (_, stripped) = model.split_once(':')?;
    (!stripped.is_empty()).then(|| stripped.to_string())
}

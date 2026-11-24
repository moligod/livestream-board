// Adapted from CC Switch v4.0.40 (MIT; Copyright (c) 2025 Jason Young).
//! 从 WorkBuddy 的会话日志导入用量：`<data dir>/projects/<cwd>/<sessionId>.jsonl`。
//!
//! 每次模型请求的 `providerData.rawUsage` 记在这次请求的最后一个事件上（assistant
//! `message` 或 `function_call`），按事件 `id` 去重；子代理
//! `<sessionId>/subagents/*.jsonl` 的请求算父会话的。`rawUsage` 是上游返回的 OpenAI 口径用量：`prompt_tokens` 已含缓存命中（`prompt_cache_hit_tokens +
//! prompt_cache_miss_tokens == prompt_tokens`），`completion_tokens` 已含思考 token。
//! 入库时把缓存读写从输入里扣掉，按「新输入」口径存（`input_token_semantics = FRESH`），
//! 计费时输入、缓存读、缓存写、输出各按各的单价算。`rawUsage.credit` 是 WorkBuddy 积分，
//! 不是美元，不当作费用。
//!
//! 日志只追加：按字节偏移增量读，只处理以换行结尾的完整行；文件变短说明被重写，从头再读，
//! `session_usage_dedup` 里的永久回执保证同一请求不会重复入库。
use crate::database::{lock_conn, Database};
use crate::error::AppError;
use crate::proxy::usage::parser::TokenUsage;
use crate::services::{
    session_usage::{
        cost_columns, metadata_modified_nanos, update_sync_state_on_conn, SessionSyncResult,
    },
    sql_helpers::INPUT_TOKEN_SEMANTICS_FRESH,
    usage_stats::find_model_pricing,
};
use crate::session_manager::providers::workbuddy;
use rust_decimal::Decimal;
use serde_json::Value;
use std::{
    fs,
    io::{BufRead, BufReader, Seek, SeekFrom},
    path::Path,
};

const DATA_SOURCE: &str = "workbuddy_session";
/// 游标键前缀。改了「哪些记录算请求」时递增，让已同步过的文件从头重读；
/// 已入库的请求由 `session_usage_dedup` 回执挡住，只补新认出的。
const CURSOR_PREFIX: &str = "workbuddy:v2:";

pub(crate) fn sync_workbuddy_usage(
    db: &Database,
    cursors: &crate::services::session_usage::SyncCursors,
) -> Result<SessionSyncResult, AppError> {
    sync_projects_with_cursors(db, &workbuddy::projects_dir(), cursors)
}

#[cfg(test)]
fn sync_projects(db: &Database, projects: &Path) -> Result<SessionSyncResult, AppError> {
    sync_projects_with_cursors(
        db,
        projects,
        &crate::services::session_usage::load_sync_cursors(db)?,
    )
}

fn sync_projects_with_cursors(
    db: &Database,
    projects: &Path,
    cursors: &crate::services::session_usage::SyncCursors,
) -> Result<SessionSyncResult, AppError> {
    let mut result = SessionSyncResult::default();
    let files = workbuddy::usage_files(projects);
    if files.is_empty() {
        return Ok(result);
    }
    for (path, parent_session) in files {
        let Ok(meta) = fs::metadata(&path) else {
            continue;
        };
        let modified = metadata_modified_nanos(&meta);
        let key = format!("{CURSOR_PREFIX}{}", path.to_string_lossy());
        let cursor = cursors.get(&key);
        if cursor
            .is_some_and(|c| c.last_modified == modified && c.last_line_offset == meta.len() as i64)
        {
            continue;
        }
        let offset = cursor
            .map(|c| c.last_line_offset)
            .filter(|offset| *offset >= 0 && *offset <= meta.len() as i64)
            .unwrap_or(0);
        match sync_file(db, &path, parent_session.as_deref(), &key, modified, offset) {
            Ok(part) => result.merge(part),
            Err(error) => result
                .errors
                .push(format!("WorkBuddy session {}: {error}", path.display())),
        }
    }
    Ok(result)
}

struct Entry {
    request_id: String,
    session_id: String,
    model: String,
    usage: TokenUsage,
    created_ms: i64,
}

/// `parent_session`：子代理文件所属的父会话；记录里的 `sessionId` 是子代理自己的。
fn entry(record: &Value, parent_session: Option<&str>) -> Option<Entry> {
    let usage = workbuddy::assistant_usage(record)?;
    if usage.is_empty() {
        return None;
    }
    // 一次请求只有一个事件带 rawUsage，事件 id 就是请求 id（和 v1 入库的行一致，重读不会重复）。
    let id = record.get("id").and_then(Value::as_str)?;
    let session_id = parent_session
        .or_else(|| record.get("sessionId").and_then(Value::as_str))
        .unwrap_or("unknown")
        .to_string();
    let model = usage.model.clone().unwrap_or_else(|| "unknown".into());
    let clamp = |n: u64| n.min(u32::MAX as u64) as u32;
    Some(Entry {
        request_id: format!("workbuddy:{session_id}:{id}"),
        session_id,
        usage: TokenUsage {
            input_tokens: clamp(usage.fresh_input),
            output_tokens: clamp(usage.output),
            cache_read_tokens: clamp(usage.cache_read),
            cache_creation_tokens: clamp(usage.cache_write),
            cache_creation_1h_tokens: 0,
            model: Some(model.clone()),
            message_id: None,
        },
        model,
        created_ms: record.get("timestamp").and_then(Value::as_i64)?,
    })
}

fn sync_file(
    db: &Database,
    path: &Path,
    parent_session: Option<&str>,
    key: &str,
    modified: i64,
    offset: i64,
) -> Result<SessionSyncResult, AppError> {
    let mut file = fs::File::open(path).map_err(|error| AppError::io(path, error))?;
    file.seek(SeekFrom::Start(offset as u64))
        .map_err(|error| AppError::io(path, error))?;
    let mut reader = BufReader::new(file);
    let mut buffer = Vec::new();
    let mut bytes = offset;
    let mut entries = Vec::new();
    loop {
        buffer.clear();
        let size = reader
            .read_until(b'\n', &mut buffer)
            .map_err(|error| AppError::io(path, error))?;
        // 写入方可能只刷了最后一行的一部分，下一轮再读。
        if size == 0 || buffer.last() != Some(&b'\n') {
            break;
        }
        bytes += size as i64;
        if let Some(entry) = serde_json::from_slice::<Value>(&buffer)
            .ok()
            .as_ref()
            .and_then(|record| entry(record, parent_session))
        {
            entries.push(entry);
        }
    }

    let mut result = SessionSyncResult {
        files_scanned: 1,
        ..Default::default()
    };
    let mut conn = lock_conn!(db.conn);
    let tx = conn.transaction()?;
    for entry in entries {
        if tx.execute(
            "INSERT OR IGNORE INTO session_usage_dedup (data_source, request_id, semantic_id, has_entry_id) VALUES (?1, ?2, ?2, 1)",
            [DATA_SOURCE, &entry.request_id],
        )? == 0
        {
            result.skipped += 1;
            continue;
        }
        let usage = &entry.usage;
        let amounts = cost_columns(
            "workbuddy",
            usage,
            find_model_pricing(&tx, &entry.model).as_ref(),
            Decimal::ONE,
        );
        tx.execute(
            "INSERT OR IGNORE INTO proxy_request_logs (
                request_id, provider_id, app_type, model, request_model, input_tokens, output_tokens, cache_read_tokens, cache_creation_tokens,
                input_cost_usd, output_cost_usd, cache_read_cost_usd, cache_creation_cost_usd, total_cost_usd,
                latency_ms, status_code, session_id, provider_type, is_streaming, cost_multiplier, created_at, data_source, input_token_semantics
            ) VALUES (?1, '_workbuddy_session', 'workbuddy', ?2, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, 0, 200, ?12, ?13, 1, '1', ?14, ?13, ?15)",
            rusqlite::params![
                entry.request_id,
                entry.model,
                usage.input_tokens,
                usage.output_tokens,
                usage.cache_read_tokens,
                usage.cache_creation_tokens,
                amounts[0],
                amounts[1],
                amounts[2],
                amounts[3],
                amounts[4],
                entry.session_id,
                DATA_SOURCE,
                entry.created_ms / 1000,
                INPUT_TOKEN_SEMANTICS_FRESH,
            ],
        )?;
        result.imported += 1;
    }
    update_sync_state_on_conn(&tx, key, modified, bytes)?;
    tx.commit()?;
    Ok(result)
}

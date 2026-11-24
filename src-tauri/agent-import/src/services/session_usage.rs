// Adapted from CC Switch v4.0.40 (MIT; Copyright (c) 2025 Jason Young).
use crate::{
    database::{lock_conn, Database},
    error::AppError,
    proxy::usage::{
        calculator::{CostCalculator, ModelPricing},
        parser::TokenUsage,
    },
};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, fs, time::SystemTime};
/// 同步结果
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSyncResult {
    pub imported: u32,
    pub skipped: u32,
    pub files_scanned: u32,
    pub suspected_duplicates: u32,
    pub deferred_files: u32,
    pub errors: Vec<String>,
}

impl SessionSyncResult {
    pub fn merge(&mut self, other: SessionSyncResult) {
        self.imported = self.imported.saturating_add(other.imported);
        self.skipped = self.skipped.saturating_add(other.skipped);
        self.files_scanned = self.files_scanned.saturating_add(other.files_scanned);
        self.suspected_duplicates = self
            .suspected_duplicates
            .saturating_add(other.suspected_duplicates);
        self.deferred_files = self.deferred_files.saturating_add(other.deferred_files);
        self.errors.extend(other.errors);
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct SyncCursor {
    pub last_modified: i64,
    pub last_line_offset: i64,
    pub last_byte_offset: Option<i64>,
    /// 游标边界前尾部字节的指纹（仅 Claude 路径写入），用于识别文件被
    /// 外部重写；NULL 表示无指纹可校验。
    #[cfg(test)]
    #[allow(dead_code)]
    pub last_tail_fingerprint: Option<i64>,
    /// Parser context or generation at the byte offset (DSH's unfinished steps, Kimi's
    /// usage-parser generation). Committed with the cursor.
    pub parser_state: Option<String>,
    pub last_synced_at: i64,
}

pub(crate) type SyncCursors = HashMap<String, SyncCursor>;

/// 一次性预取 session_log_sync 全表游标。
///
/// 失败必须中止本轮同步，不能回退空表当新库处理：`rollup_and_prune` 会把
/// 30 天前的明细汇总后删除，request_id 去重只查明细表，对已剪条目失明——
/// 空表回退意味着全量重导，被剪的旧条目会在下次 rollup 时再次累加进汇总，
/// 永久放大统计。
#[cfg(test)]
pub(crate) fn load_sync_cursors(db: &Database) -> Result<SyncCursors, AppError> {
    let conn = lock_conn!(db.conn);
    let mut stmt = conn
        .prepare(
            "SELECT file_path, last_modified, last_line_offset, last_synced_at, last_byte_offset,
                    last_tail_fingerprint, parser_state
             FROM session_log_sync",
        )
        .map_err(|e| AppError::Database(format!("预取同步游标失败: {e}")))?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            SyncCursor {
                last_modified: row.get(1)?,
                last_line_offset: row.get(2)?,
                last_synced_at: row.get(3)?,
                last_byte_offset: row.get(4)?,
                last_tail_fingerprint: row.get(5)?,
                parser_state: row.get(6)?,
            },
        ))
    });
    rows.and_then(|rows| rows.collect::<Result<HashMap<_, _>, _>>())
        .map_err(|e| AppError::Database(format!("预取同步游标失败: {e}")))
}

const MIN_PLAUSIBLE_LATENCY_MS: i64 = 100;

/// 估算出的请求耗时长于这个毫秒数也不要：中间多半夹了休眠或长时间的重试等待。
const MAX_PLAUSIBLE_LATENCY_MS: i64 = 60 * 60 * 1000;

/// 会话日志没有请求计时，耗时只能拿两个时间戳相减估出来（含首字等待）。
/// 落在合理范围之外的返回 None，入库时写 0，表示没有计时。
///
/// Shared by the Claude and Codex session parsers.
pub(crate) fn estimated_latency_ms(start_ms: i64, end_ms: i64) -> Option<i64> {
    let latency = end_ms.checked_sub(start_ms)?;
    (MIN_PLAUSIBLE_LATENCY_MS..=MAX_PLAUSIBLE_LATENCY_MS)
        .contains(&latency)
        .then_some(latency)
}

pub(crate) fn parse_timestamp_millis(timestamp: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(timestamp)
        .ok()
        .map(|dt| dt.timestamp_millis())
}

pub(crate) fn estimated_first_token_ms(
    start_ms: i64,
    first_output_ms: i64,
    latency_ms: i64,
) -> Option<i64> {
    let first_token = first_output_ms.checked_sub(start_ms)?;
    (first_token > 0 && first_token <= latency_ms).then_some(first_token)
}
pub(crate) fn metadata_modified_nanos(metadata: &fs::Metadata) -> i64 {
    metadata
        .modified()
        .ok()
        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos().min(i64::MAX as u128) as i64)
        .unwrap_or(0)
}

/// 更新 session_log_sync 表中某条目的同步进度。
///
/// Shared by all session_usage_* parsers.
pub(crate) fn update_sync_state(
    db: &Database,
    file_path: &str,
    last_modified: i64,
    last_offset: i64,
) -> Result<(), AppError> {
    let conn = lock_conn!(db.conn);
    update_sync_state_on_conn(&conn, file_path, last_modified, last_offset)
}

/// [`update_sync_state`] 的免锁版本，供调用方在已持锁的事务内把游标推进
/// 与数据插入绑成原子提交。
pub(crate) fn update_sync_state_on_conn(
    conn: &rusqlite::Connection,
    file_path: &str,
    last_modified: i64,
    last_offset: i64,
) -> Result<(), AppError> {
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    conn.prepare_cached(
        "INSERT OR REPLACE INTO session_log_sync (file_path, last_modified, last_line_offset, last_synced_at)
         VALUES (?1, ?2, ?3, ?4)",
    )
    .and_then(|mut stmt| stmt.execute(rusqlite::params![file_path, last_modified, last_offset, now]))
    .map_err(|e| AppError::Database(format!("更新同步状态失败: {e}")))?;
    Ok(())
}

/// 写入字节游标（Grok 等按行流式扫描）。`last_line_offset` 置 0，表示
/// 行号/事件计数游标已退役；存量 Grok 行的事件计数只在 `last_byte_offset`
/// 仍为 NULL 时用一次，转换成字节位置后不再读取。
pub(crate) fn update_byte_sync_state(
    db: &Database,
    file_path: &str,
    last_modified: i64,
    byte_offset: i64,
) -> Result<(), AppError> {
    let conn = lock_conn!(db.conn);
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    conn.prepare_cached(
        "INSERT OR REPLACE INTO session_log_sync
         (file_path, last_modified, last_line_offset, last_synced_at, last_byte_offset)
         VALUES (?1, ?2, 0, ?3, ?4)",
    )
    .and_then(|mut stmt| {
        stmt.execute(rusqlite::params![
            file_path,
            last_modified,
            now,
            byte_offset
        ])
    })
    .map_err(|e| AppError::Database(format!("更新同步状态失败: {e}")))?;
    Ok(())
}
pub(crate) fn cost_columns(
    app_type: &str,
    usage: &TokenUsage,
    pricing: Option<&ModelPricing>,
    multiplier: Decimal,
) -> [String; 5] {
    let Some(pricing) = pricing else {
        return std::array::from_fn(|_| "0".to_string());
    };
    let cost = CostCalculator::calculate_for_app(app_type, usage, pricing, multiplier);
    [
        cost.input_cost,
        cost.output_cost,
        cost.cache_read_cost,
        cost.cache_creation_cost,
        cost.total_cost,
    ]
    .map(|value| value.to_string())
}

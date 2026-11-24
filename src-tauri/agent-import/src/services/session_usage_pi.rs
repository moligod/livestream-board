// Adapted from CC Switch v4.0.40 (MIT; Copyright (c) 2025 Jason Young).
//! Session usage importer for agents that persist Pi's session JSONL format:
//! Pi (pi.dev) and Oh My Pi (omp.sh).
//!
//! They are separate products with separate session stores; only the on-disk
//! format is shared. This importer keeps direct (non-proxy) usage visible in
//! the shared dashboard, one app at a time: each keeps its own `app_type`,
//! `data_source`, placeholder provider and sync cursors (see [`Flavor`]).

use crate::database::{lock_conn, Database};
use crate::error::AppError;
use crate::proxy::usage::calculator::CostCalculator;
use crate::proxy::usage::parser::TokenUsage;
use crate::services::session_usage::{
    estimated_latency_ms, metadata_modified_nanos, update_sync_state_on_conn, SessionSyncResult,
};
use crate::services::sql_helpers::INPUT_TOKEN_SEMANTICS_FRESH;
use crate::services::usage_stats::find_model_pricing;
use rust_decimal::Decimal;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::str::FromStr;

const UNKNOWN_MODEL: &str = "unknown";
const MAX_USAGE_LABEL_BYTES: usize = 512;
const MIN_SQLITE_UNIX_MILLIS: i64 = -62_167_219_200_000;
const MAX_SQLITE_UNIX_MILLIS: i64 = 253_402_300_799_999;
const REVISION_TAIL_BYTES: u64 = 4096;
const REVISION_MARKER_SHIFT: u32 = 61;
const REVISION_COMPLETE_SHIFT: u32 = 60;
const REVISION_SIZE_SHIFT: u32 = 32;
// The original cursor tag. Flavors without official subagent tool results
// keep writing it, so their existing history stays incremental.
const LEGACY_REVISION_MARKER: u64 = 0b101;
// Earlier builds of the timing fix wrote this tag with the same cursor layout.
const COMPAT_REVISION_MARKER: u64 = 0b110;
// Flavors importing official subagent tool results rescan older cursors once
// to recover previously ignored subagent usage. The durable request ledger
// keeps already imported history unchanged.
const SUBAGENT_REVISION_MARKER: u64 = 0b111;
const REVISION_SIZE_MASK: u64 = (1 << 28) - 1;
const PI_REQUEST_DEDUP_SQL: &str = "SELECT EXISTS(
         SELECT 1 FROM session_usage_dedup
         WHERE data_source = ?1 AND request_id = ?2
     )";
const PI_SEMANTIC_DEDUP_SQL: &str = "SELECT EXISTS(
         SELECT 1 FROM session_usage_dedup
         WHERE data_source = ?1 AND semantic_id = ?2
     )";
const PI_LEGACY_SEMANTIC_DEDUP_SQL: &str = "SELECT EXISTS(
         SELECT 1 FROM session_usage_dedup
         WHERE data_source = ?1 AND semantic_id = ?2 AND has_entry_id = 0
     )";

/// Everything that differs between the agents writing the same session format.
///
/// The identity fields carry the app's own names end to end: `app_type` is the
/// dashboard's filter value, `data_source` keys the dedup ledger and the sync
/// cursors, and `provider_placeholder` is the provider row shown for entries
/// that name none. Mixing them up would fold one agent's usage into another's,
/// so each app module declares exactly one flavor from its own constants.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Flavor {
    /// Dashboard app filter, also `proxy_request_logs.app_type`.
    pub(crate) app_type: &'static str,
    /// `proxy_request_logs.data_source` and `session_usage_dedup.data_source`.
    pub(crate) data_source: &'static str,
    /// Placeholder `provider_id` for rows the entry does not attribute.
    pub(crate) provider_placeholder: &'static str,
    /// Reserved `session_log_sync` row that stores this app's child cutoff,
    /// for apps whose parent sessions project nested session usage (Pi's
    /// pi-subagents). `None`: nested session files are the only record of
    /// their usage, so they always count.
    pub(crate) cutoff_key: Option<&'static str>,
    /// Prefix on the synthesized `request_id` and `semantic_id` hashes.
    pub(crate) id_prefix: &'static str,
    /// Hash domain separator. Pi's value is frozen: the dedup ledger outlives
    /// pruned detail rows, so changing it would re-import history.
    pub(crate) hash_tag: &'static str,
    /// Log and error label: the app name, as the user knows it.
    pub(crate) label: &'static str,
    /// Whether Oh My Pi's `model_usage` entries carry usage. They record calls
    /// made outside the conversation: auto-thinking judges, job widgets.
    pub(crate) model_usage_carrier: bool,
    /// Whether a fixed-width `{"type":"title"}` slot may precede the session
    /// header. Only Oh My Pi writes one.
    pub(crate) title_slot: bool,
    /// Whether `subagent` tool results of Pi's official example extension
    /// carry child usage in `details.results`. That extension runs children
    /// with `--no-session`, so the parent's tool result is their only record.
    pub(crate) subagent_tool_results: bool,
    pub(crate) session_files: fn() -> Result<Vec<PathBuf>, String>,
}

pub(crate) const PI: Flavor = Flavor {
    app_type: "pi",
    data_source: "pi_session",
    provider_placeholder: "_pi_session",
    cutoff_key: Some("pi:child-cutoff"),
    id_prefix: "pi_session",
    hash_tag: "pi-session",
    label: "Pi",
    model_usage_carrier: false,
    title_slot: false,
    subagent_tool_results: true,
    session_files: crate::session_manager::providers::pi::session_files,
};

#[derive(Debug, Clone, Copy, Default)]
struct PiCosts {
    input: Decimal,
    output: Decimal,
    cache_read: Decimal,
    cache_write: Decimal,
    total: Decimal,
}

impl PiCosts {
    fn reported(self) -> Option<(Decimal, Decimal, Decimal, Decimal, Decimal)> {
        let component_total = self.input + self.output + self.cache_read + self.cache_write;
        let total = if self.total > Decimal::ZERO {
            self.total
        } else {
            component_total
        };
        (total > Decimal::ZERO).then_some((
            self.input,
            self.output,
            self.cache_read,
            self.cache_write,
            total,
        ))
    }
}

#[derive(Debug)]
struct PiUsageRecord {
    request_id: String,
    semantic_id: String,
    has_entry_id: bool,
    provider_id: String,
    model: String,
    request_model: String,
    input_tokens: u32,
    output_tokens: u32,
    cache_read_tokens: u32,
    cache_write_tokens: u32,
    costs: PiCosts,
    latency_ms: i64,
    status_code: i64,
    error_message: Option<String>,
    created_at: i64,
    session_id: String,
}

#[derive(Debug)]
struct ParsedPiFile {
    records: Vec<PiUsageRecord>,
    last_complete_line: i64,
    incomplete_tail: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PiFileRevision {
    modified_nanos: i64,
    file_size: u64,
    tail_fingerprint: u32,
    complete: bool,
}

fn revision_marker(flavor: &Flavor) -> u64 {
    if flavor.subagent_tool_results {
        SUBAGENT_REVISION_MARKER
    } else {
        LEGACY_REVISION_MARKER
    }
}

fn accepts_revision_marker(flavor: &Flavor, marker: u64) -> bool {
    if flavor.subagent_tool_results {
        marker == SUBAGENT_REVISION_MARKER
    } else {
        marker == LEGACY_REVISION_MARKER || marker == COMPAT_REVISION_MARKER
    }
}

impl PiFileRevision {
    fn encoded(self, flavor: &Flavor) -> i64 {
        ((revision_marker(flavor) << REVISION_MARKER_SHIFT)
            | (u64::from(self.complete) << REVISION_COMPLETE_SHIFT)
            | (self.file_size << REVISION_SIZE_SHIFT)
            | u64::from(self.tail_fingerprint)) as i64
    }
}

#[derive(Debug, Clone, Copy)]
struct PiSyncState {
    revision: PiFileRevision,
    last_line_offset: i64,
}

#[derive(Debug)]
struct PiRequestIdentity {
    request_id: String,
    semantic_id: String,
    has_entry_id: bool,
}

#[derive(Default)]
struct ChildScope {
    /// Canonical paths of every file in this round.
    imported: HashSet<PathBuf>,
    /// Extension-less session paths; a file below one of them is a nested subagent or fork session.
    stems: HashSet<PathBuf>,
    /// Unix seconds of this build's first import; `i64::MIN` when the flavor has no cutoff.
    cutoff: i64,
}

impl ChildScope {
    fn new(files: &[PathBuf], cutoff: i64) -> Self {
        Self {
            imported: files
                .iter()
                .filter_map(|file| fs::canonicalize(file).ok())
                .collect(),
            stems: files.iter().map(|file| file.with_extension("")).collect(),
            cutoff,
        }
    }

    fn is_nested(&self, file: &Path) -> bool {
        file.ancestors().skip(1).any(|dir| self.stems.contains(dir))
    }
}

/// Import usage from every Pi session file under the session browser's current
/// root and layout rules, including nested subagent sessions.
#[cfg(test)]
pub fn sync_pi_usage(db: &Database) -> Result<SessionSyncResult, AppError> {
    sync_flavor(db, &PI)
}

pub(crate) fn sync_pi_usage_with_cursors(
    db: &Database,
    cursors: &crate::services::session_usage::SyncCursors,
) -> Result<SessionSyncResult, AppError> {
    sync_flavor_with_cursors(db, &PI, cursors)
}

#[cfg(test)]
pub(crate) fn sync_flavor(
    db: &Database,
    flavor: &'static Flavor,
) -> Result<SessionSyncResult, AppError> {
    sync_flavor_with_cursors(
        db,
        flavor,
        &crate::services::session_usage::load_sync_cursors(db)?,
    )
}

pub(crate) fn sync_flavor_with_cursors(
    db: &Database,
    flavor: &'static Flavor,
    cursors: &crate::services::session_usage::SyncCursors,
) -> Result<SessionSyncResult, AppError> {
    let files = (flavor.session_files)()
        .map_err(|error| AppError::Config(format!("无法发现 {} 会话: {error}", flavor.label)))?;
    Ok(sync_pi_files_with_cursors(db, flavor, &files, cursors))
}

#[cfg(test)]
pub(crate) fn sync_pi_files(
    db: &Database,
    flavor: &'static Flavor,
    files: &[PathBuf],
) -> SessionSyncResult {
    match crate::services::session_usage::load_sync_cursors(db) {
        Ok(cursors) => sync_pi_files_with_cursors(db, flavor, files, &cursors),
        Err(error) => SessionSyncResult {
            errors: vec![format!("预取同步游标失败: {error}")],
            ..Default::default()
        },
    }
}

fn sync_pi_files_with_cursors(
    db: &Database,
    flavor: &'static Flavor,
    files: &[PathBuf],
    cursors: &crate::services::session_usage::SyncCursors,
) -> SessionSyncResult {
    let mut result = SessionSyncResult {
        files_scanned: files.len().min(u32::MAX as usize) as u32,
        ..Default::default()
    };
    let cutoff = match child_cutoff(db, flavor, cursors) {
        Ok(cutoff) => cutoff,
        Err(error) => {
            result
                .errors
                .push(format!("记录 {} 子会话起算时间失败: {error}", flavor.label));
            return result;
        }
    };
    let scope = ChildScope::new(files, cutoff);
    for file_path in files {
        match sync_single_pi_file(db, flavor, file_path, cursors, &scope) {
            Ok(file_result) => result.merge(file_result),
            Err(error) => {
                let message = format!("{}: {error}", file_path.display());
                log::warn!("[{}-SYNC] 会话文件解析失败: {message}", flavor.label);
                result.errors.push(message);
            }
        }
    }

    if result.imported > 0 {
        log::info!(
            "[{}-SYNC] 同步完成: 导入 {} 条, 跳过 {} 条, 扫描 {} 个文件",
            flavor.label,
            result.imported,
            result.skipped,
            result.files_scanned
        );
    }
    result
}

fn child_cutoff(
    db: &Database,
    flavor: &'static Flavor,
    cursors: &std::collections::HashMap<String, crate::services::session_usage::SyncCursor>,
) -> Result<i64, AppError> {
    let Some(cutoff_key) = flavor.cutoff_key else {
        return Ok(i64::MIN);
    };
    if let Some(cursor) = cursors.get(cutoff_key) {
        return Ok(cursor.last_synced_at);
    }
    let now = chrono::Utc::now().timestamp();
    let conn = lock_conn!(db.conn);
    conn.execute(
        "INSERT INTO session_log_sync (file_path, last_modified, last_line_offset, last_synced_at)
         VALUES (?1, 0, 0, ?2)",
        rusqlite::params![cutoff_key, now],
    )
    .map_err(|error| {
        AppError::Database(format!("写入 {} 子会话起算时间失败: {error}", flavor.label))
    })?;
    Ok(now)
}

fn sync_single_pi_file(
    db: &Database,
    flavor: &'static Flavor,
    file_path: &Path,
    cursors: &std::collections::HashMap<String, crate::services::session_usage::SyncCursor>,
    scope: &ChildScope,
) -> Result<SessionSyncResult, AppError> {
    let metadata = fs::symlink_metadata(file_path).map_err(|error| {
        AppError::Config(format!("无法读取 {} 会话文件元数据: {error}", flavor.label))
    })?;
    if !metadata.file_type().is_file()
        || file_path.extension().and_then(|value| value.to_str()) != Some("jsonl")
    {
        return Err(AppError::Config(format!(
            "{} 会话路径不是普通 JSONL 文件",
            flavor.label
        )));
    }
    if metadata.len() > crate::session_manager::providers::pi::MAX_SESSION_BYTES {
        return Err(AppError::Config(format!(
            "{} 会话文件超过 {} 字节安全上限",
            flavor.label,
            crate::session_manager::providers::pi::MAX_SESSION_BYTES
        )));
    }

    let file_path_string = file_path.to_string_lossy().to_string();
    let modified = metadata_modified_nanos(&metadata);
    let revision = pi_file_revision(flavor, file_path, &metadata, modified)?;
    let previous = decode_pi_sync_state(flavor, cursors.get(&file_path_string));
    if previous.is_some_and(|state| state.revision == revision) {
        return Ok(SessionSyncResult::default());
    }

    // A matching tail at the old EOF identifies Pi's normal append path, so
    // active sessions can seek straight to appended JSONL. Any mismatch is a
    // rewrite and must rescan from the header; the durable request ledger
    // makes that safe.
    let (start_after_line, start_at_byte) = match previous {
        Some(state)
            if state.revision.complete
                && revision.file_size > state.revision.file_size
                && pi_prefix_tail_matches(flavor, file_path, state.revision)? =>
        {
            (state.last_line_offset, Some(state.revision.file_size))
        }
        Some(_) | None => (0, None),
    };
    let parsed = parse_pi_file(
        flavor,
        file_path,
        start_after_line,
        start_at_byte,
        revision.file_size,
        modified,
        scope,
    )?;
    let conn = lock_conn!(db.conn);
    let tx = conn.unchecked_transaction().map_err(|error| {
        AppError::Database(format!("启动 {} 用量导入事务失败: {error}", flavor.label))
    })?;
    let mut result = SessionSyncResult::default();
    for record in &parsed.records {
        if insert_pi_record(&tx, flavor, record)? {
            result.imported = result.imported.saturating_add(1);
        } else {
            result.skipped = result.skipped.saturating_add(1);
        }
    }

    update_pi_sync_state_on_conn(
        &tx,
        flavor,
        &file_path_string,
        revision,
        parsed.last_complete_line,
    )?;
    tx.commit().map_err(|error| {
        AppError::Database(format!("提交 {} 用量导入事务失败: {error}", flavor.label))
    })?;
    if parsed.incomplete_tail {
        result.deferred_files = 1;
    }
    Ok(result)
}

/// 从批量预取的游标解码 Pi 同步状态（revision 编码在 `last_synced_at`）。
fn decode_pi_sync_state(
    flavor: &Flavor,
    cursor: Option<&crate::services::session_usage::SyncCursor>,
) -> Option<PiSyncState> {
    let cursor = cursor?;
    let last_line_offset = cursor.last_line_offset;
    let encoded_revision = cursor.last_synced_at as u64;
    if !accepts_revision_marker(flavor, encoded_revision >> REVISION_MARKER_SHIFT) {
        return None;
    }
    let file_size = (encoded_revision >> REVISION_SIZE_SHIFT) & REVISION_SIZE_MASK;
    if file_size > crate::session_manager::providers::pi::MAX_SESSION_BYTES
        || last_line_offset < 0
        || last_line_offset > crate::session_manager::providers::pi::MAX_TREE_ENTRIES as i64 + 1
    {
        return None;
    }
    Some(PiSyncState {
        revision: PiFileRevision {
            modified_nanos: cursor.last_modified,
            file_size,
            tail_fingerprint: encoded_revision as u32,
            complete: ((encoded_revision >> REVISION_COMPLETE_SHIFT) & 1) == 1,
        },
        last_line_offset,
    })
}

fn update_pi_sync_state_on_conn(
    conn: &rusqlite::Connection,
    flavor: &'static Flavor,
    file_path: &str,
    revision: PiFileRevision,
    last_line_offset: i64,
) -> Result<(), AppError> {
    update_sync_state_on_conn(conn, file_path, revision.modified_nanos, last_line_offset)?;
    // No schema expansion is needed for Pi's append proof. This tagged value
    // is private to Pi rows; no production consumer interprets last_synced_at.
    conn.execute(
        "UPDATE session_log_sync SET last_synced_at = ?2 WHERE file_path = ?1",
        rusqlite::params![file_path, revision.encoded(flavor)],
    )
    .map_err(|error| {
        AppError::Database(format!("更新 {} 会话同步状态失败: {error}", flavor.label))
    })?;
    Ok(())
}

fn pi_file_revision(
    flavor: &'static Flavor,
    file_path: &Path,
    metadata: &fs::Metadata,
    modified_nanos: i64,
) -> Result<PiFileRevision, AppError> {
    let tail_len = metadata.len().min(REVISION_TAIL_BYTES);
    let mut tail = vec![0; tail_len as usize];
    if tail_len > 0 {
        let mut file = File::open(file_path).map_err(|error| {
            AppError::Config(format!("无法打开 {} 会话文件: {error}", flavor.label))
        })?;
        file.seek(SeekFrom::Start(metadata.len() - tail_len))
            .and_then(|_| file.read_exact(&mut tail))
            .map_err(|error| {
                AppError::Config(format!("无法读取 {} 会话文件尾部: {error}", flavor.label))
            })?;
    }

    let complete = tail.last() == Some(&b'\n');
    let tail_fingerprint = pi_tail_fingerprint(&tail);
    Ok(PiFileRevision {
        modified_nanos,
        file_size: metadata.len(),
        tail_fingerprint,
        complete,
    })
}

fn pi_prefix_tail_matches(
    flavor: &'static Flavor,
    file_path: &Path,
    previous: PiFileRevision,
) -> Result<bool, AppError> {
    let tail_len = previous.file_size.min(REVISION_TAIL_BYTES);
    let mut tail = vec![0; tail_len as usize];
    if tail_len > 0 {
        let mut file = File::open(file_path).map_err(|error| {
            AppError::Config(format!("无法打开 {} 会话文件: {error}", flavor.label))
        })?;
        file.seek(SeekFrom::Start(previous.file_size - tail_len))
            .and_then(|_| file.read_exact(&mut tail))
            .map_err(|error| {
                AppError::Config(format!("无法校验 {} 会话追加边界: {error}", flavor.label))
            })?;
    }
    Ok(pi_tail_fingerprint(&tail) == previous.tail_fingerprint)
}

fn pi_tail_fingerprint(tail: &[u8]) -> u32 {
    let mut hasher = Sha256::new();
    hash_field(&mut hasher, b"pi-session-tail-v1");
    hash_field(&mut hasher, tail);
    let digest = hasher.finalize();
    u32::from_be_bytes(digest[..4].try_into().unwrap_or_default())
}

fn parse_pi_file(
    flavor: &'static Flavor,
    file_path: &Path,
    start_after_line: i64,
    start_at_byte: Option<u64>,
    snapshot_size: u64,
    file_modified_nanos: i64,
    scope: &ChildScope,
) -> Result<ParsedPiFile, AppError> {
    let file = File::open(file_path).map_err(|error| {
        AppError::Config(format!("无法打开 {} 会话文件: {error}", flavor.label))
    })?;
    let mut reader = BufReader::new(file);
    let mut buffer = String::new();
    let mut line_number = 0i64;
    let mut bytes_read = 0u64;
    let mut session_id = None;
    let mut session_timestamp = None;
    let mut records = Vec::new();
    let mut incomplete_tail = false;
    let nested = scope.is_nested(file_path);

    loop {
        buffer.clear();
        let remaining = snapshot_size.saturating_sub(bytes_read);
        if remaining == 0 {
            break;
        }
        let read = Read::by_ref(&mut reader)
            .take(remaining)
            .read_line(&mut buffer)
            .map_err(|error| {
                AppError::Config(format!("无法读取 {} 会话文件: {error}", flavor.label))
            })?;
        if read == 0 {
            return Err(AppError::Config(format!(
                "{} 会话文件在读取期间被截断",
                flavor.label
            )));
        }
        bytes_read = bytes_read.saturating_add(read as u64);
        if bytes_read > crate::session_manager::providers::pi::MAX_SESSION_BYTES {
            return Err(AppError::Config(format!(
                "{} 会话文件读取时超过安全上限",
                flavor.label
            )));
        }
        let has_newline = buffer.ends_with('\n');
        let line = buffer.trim();
        let value = if line.is_empty() {
            None
        } else {
            match serde_json::from_str::<Value>(line) {
                Ok(value) => Some(value),
                Err(_) if !has_newline => {
                    incomplete_tail = true;
                    break;
                }
                Err(_) => None,
            }
        };
        line_number = line_number.saturating_add(1);
        if line_number > crate::session_manager::providers::pi::MAX_TREE_ENTRIES as i64 + 1 {
            return Err(AppError::Config(format!(
                "{} 会话超过 {} 条 entry 安全上限",
                flavor.label,
                crate::session_manager::providers::pi::MAX_TREE_ENTRIES
            )));
        }
        if session_id.is_some() && line_number <= start_after_line {
            continue;
        }
        let Some(value) = value else {
            if !has_newline {
                incomplete_tail = true;
                break;
            }
            continue;
        };

        if session_id.is_none() {
            let entry_type = value.get("type").and_then(Value::as_str);
            // Oh My Pi opens with a fixed-width `{"type":"title"}` slot before
            // the header. It carries no usage; skip it and keep looking.
            if flavor.title_slot && entry_type == Some("title") {
                continue;
            }
            if entry_type != Some("session") {
                return Err(AppError::Config(format!(
                    "{} 会话的首条有效 JSON 不是 session header",
                    flavor.label
                )));
            }
            session_id = value
                .get("id")
                .and_then(Value::as_str)
                .filter(|id| crate::session_manager::providers::pi::is_valid_tree_id(id))
                .map(str::to_string);
            if session_id.is_none() {
                return Err(AppError::Config(format!(
                    "{} 会话 header 缺少 id",
                    flavor.label
                )));
            }
            let header_timestamp_millis = value.get("timestamp").and_then(parse_timestamp_millis);
            session_timestamp = header_timestamp_millis.map(|timestamp| timestamp / 1000);
            if let Some(byte_offset) = start_at_byte.filter(|offset| *offset >= bytes_read) {
                reader.seek(SeekFrom::Start(byte_offset)).map_err(|error| {
                    AppError::Config(format!("无法定位 {} 会话增量边界: {error}", flavor.label))
                })?;
                bytes_read = byte_offset;
                line_number = start_after_line;
            }
            continue;
        }
        if let Some(subagent) = parse_subagent_records(
            flavor,
            &value,
            session_id.as_deref().unwrap_or_default(),
            session_timestamp,
            file_modified_nanos / 1_000_000_000,
        ) {
            // Same rule as below: before the cutoff a nested file's usage is
            // carried by its parent's projection.
            records.extend(
                subagent
                    .into_iter()
                    .filter(|record| !nested || record.created_at >= scope.cutoff),
            );
            continue;
        }
        let Some(record) = parse_usage_record(
            flavor,
            &value,
            session_id.as_deref().unwrap_or_default(),
            session_timestamp,
            file_modified_nanos / 1_000_000_000,
        ) else {
            continue;
        };
        // Before the cutoff the parent's projections carry child usage; from it on, the child files do.
        let from_cutoff = record.created_at >= scope.cutoff;
        if (nested && !from_cutoff)
            || (from_cutoff && duplicates_imported_sessions(&value, &scope.imported))
        {
            continue;
        }
        records.push(record);
    }

    if session_id.is_none() && !incomplete_tail {
        return Err(AppError::Config(format!(
            "{} 会话没有有效 header",
            flavor.label
        )));
    }
    Ok(ParsedPiFile {
        records,
        last_complete_line: line_number,
        incomplete_tail,
    })
}

/// Only recognize the official example extension's tool-result contract.
/// Preserve the legacy top-level usage path and request IDs for pi-subagents.
/// Do not walk arbitrary details/compaction snapshots looking for `usage`.
///
/// pi-subagents reuses the `subagent` tool name and a similar `details`
/// shape, but its children keep their own session files, linked through
/// `sessionFile`. Those files (or, before the cutoff, the projection's
/// top-level usage) are the record of that usage, so any linked session
/// leaves the entry to the projection path and nothing is read from details.
fn parse_subagent_records(
    flavor: &'static Flavor,
    entry: &Value,
    session_id: &str,
    session_timestamp: Option<i64>,
    file_timestamp: i64,
) -> Option<Vec<PiUsageRecord>> {
    if !flavor.subagent_tool_results || entry.get("type")?.as_str()? != "message" {
        return None;
    }
    let message = entry.get("message")?;
    if message.get("role")?.as_str()? != "toolResult"
        || message.get("toolName")?.as_str()? != "subagent"
        || message.get("usage").is_some()
    {
        return None;
    }
    let details = message.get("details")?;
    if !matches!(
        details.get("mode")?.as_str()?,
        "single" | "parallel" | "chain"
    ) {
        return None;
    }
    let mut linked_sessions = Vec::new();
    collect_session_files(details, &mut linked_sessions);
    if !linked_sessions.is_empty() {
        return None;
    }
    let results = details.get("results")?.as_array()?;
    let parent_identity =
        pi_request_identity(flavor, entry, "subagent", &Value::Null, Some(message));
    let mut records = Vec::new();
    for (result_index, result) in results.iter().enumerate() {
        let mut has_request_usage = false;
        if let Some(messages) = result.get("messages").and_then(Value::as_array) {
            for (message_index, child) in messages.iter().enumerate() {
                if child.get("role").and_then(Value::as_str) != Some("assistant")
                    || !child.get("usage").is_some_and(Value::is_object)
                {
                    continue;
                }
                has_request_usage = true;
                let child_entry = serde_json::json!({
                    "type": "message",
                    "id": format!("{}:result:{result_index}:message:{message_index}", parent_identity.request_id),
                    // The parent entry is written after the entire task; it is
                    // not a per-request end time. Never infer latency from it.
                    "timestamp": child.get("timestamp").or_else(|| entry.get("timestamp")),
                    "message": child,
                });
                if let Some(record) = parse_usage_record(
                    flavor,
                    &child_entry,
                    session_id,
                    session_timestamp,
                    file_timestamp,
                ) {
                    records.push(record);
                }
            }
        }
        if has_request_usage {
            continue; // `result.usage` is the sum of these requests.
        }
        // Some saved results retain only the summary. Its cost is a scalar,
        // unlike AssistantMessage.usage.cost. Keep it as one aggregate record;
        // `turns` cannot reconstruct individual requests or their timing.
        if let Some(usage) = result.get("usage").filter(|value| value.is_object()) {
            let mut usage = usage.clone();
            usage["cost"] = serde_json::json!({"total": usage.get("cost").cloned()});
            let aggregate_entry = serde_json::json!({
                "type": "message",
                "id": format!("{}:result:{result_index}:aggregate", parent_identity.request_id),
                "timestamp": entry.get("timestamp"),
                "message": {
                    "role": "assistant",
                    "model": result.get("model"),
                    "stopReason": result.get("stopReason"),
                    "errorMessage": result.get("errorMessage"),
                    "usage": usage,
                },
            });
            if let Some(record) = parse_usage_record(
                flavor,
                &aggregate_entry,
                session_id,
                session_timestamp,
                file_timestamp,
            ) {
                records.push(record);
            }
        }
    }
    Some(records)
}

/// A pi-subagents projection sums the usage of the child sessions it links.
fn duplicates_imported_sessions(entry: &Value, imported_sessions: &HashSet<PathBuf>) -> bool {
    let links = projection_session_files(entry);
    !links.is_empty()
        && links
            .into_iter()
            .all(|link| fs::canonicalize(link).is_ok_and(|path| imported_sessions.contains(&path)))
}

fn projection_session_files(entry: &Value) -> Vec<&str> {
    let mut links = Vec::new();
    let message = entry.get("message").filter(|message| {
        entry.get("type").and_then(Value::as_str) == Some("message")
            && message.get("role").and_then(Value::as_str) == Some("toolResult")
            && message.get("usage").is_some()
    });
    if let Some(details) = message.and_then(|message| message.get("details")) {
        collect_session_files(details, &mut links);
    }
    links
}

fn collect_session_files<'a>(value: &'a Value, links: &mut Vec<&'a str>) {
    match value {
        Value::Object(fields) => {
            for (key, field) in fields {
                match field {
                    Value::String(link) if key == "sessionFile" => links.push(link),
                    _ => collect_session_files(field, links),
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_session_files(item, links);
            }
        }
        _ => {}
    }
}

fn parse_usage_record(
    flavor: &'static Flavor,
    entry: &Value,
    session_id: &str,
    session_timestamp: Option<i64>,
    file_timestamp: i64,
) -> Option<PiUsageRecord> {
    let entry_type = entry.get("type").and_then(Value::as_str)?;
    // `carrier` holds the provider/model/stopReason/errorMessage fields: the
    // nested `message` for message entries, the entry itself for the carriers
    // Oh My Pi writes at top level.
    let (kind, usage_value, carrier) = match entry_type {
        "message" => {
            let message = entry.get("message")?;
            match message.get("role").and_then(Value::as_str) {
                Some("assistant") => ("assistant", message.get("usage")?, Some(message)),
                Some("toolResult") => ("tool_result", message.get("usage")?, Some(message)),
                _ => return None,
            }
        }
        "compaction" => ("compaction", entry.get("usage")?, None),
        "branch_summary" => ("branch_summary", entry.get("usage")?, None),
        // Oh My Pi records calls outside the conversation — auto-thinking
        // judges, job widgets, completion probes — here, with the request
        // metadata at the entry's top level.
        "model_usage" if flavor.model_usage_carrier => {
            ("model_usage", entry.get("usage")?, Some(entry))
        }
        _ => return None,
    };
    let attributed = matches!(kind, "assistant" | "model_usage");

    let event_timestamp_millis = entry
        .get("timestamp")
        .and_then(parse_timestamp_millis)
        .or_else(|| {
            carrier
                .and_then(|value| value.get("timestamp"))
                .and_then(parse_timestamp_millis)
        });
    let input_tokens = token_count(usage_value, "input");
    let output_tokens = token_count(usage_value, "output");
    let cache_read_tokens = token_count(usage_value, "cacheRead");
    let cache_write_tokens = token_count(usage_value, "cacheWrite");
    let costs = parse_costs(usage_value.get("cost"));
    let stop_reason = attributed
        .then(|| carrier.and_then(|value| nonempty_string(value.get("stopReason"))))
        .flatten();
    let failed = matches!(stop_reason, Some("error" | "aborted"));
    if input_tokens == 0
        && output_tokens == 0
        && cache_read_tokens == 0
        && cache_write_tokens == 0
        && costs.reported().is_none()
        && !failed
    {
        return None;
    }

    let (provider_id, model, request_model) = if attributed {
        let carrier = carrier?;
        let provider = bounded_label(carrier.get("provider"), flavor.provider_placeholder);
        let requested = bounded_label(carrier.get("model"), UNKNOWN_MODEL);
        let actual = nonempty_string(carrier.get("responseModel"))
            .map(truncate_usage_label)
            .unwrap_or(&requested)
            .to_string();
        (provider, actual, requested)
    } else {
        (
            flavor.provider_placeholder.to_string(),
            UNKNOWN_MODEL.to_string(),
            UNKNOWN_MODEL.to_string(),
        )
    };

    let created_at = event_timestamp_millis
        .map(|timestamp| timestamp / 1000)
        .or(session_timestamp)
        .unwrap_or(file_timestamp)
        .clamp(MIN_SQLITE_UNIX_MILLIS / 1000, MAX_SQLITE_UNIX_MILLIS / 1000);

    let (status_code, error_message) = if attributed {
        match stop_reason {
            Some("error") | Some("aborted") => {
                let fallback = if stop_reason == Some("aborted") {
                    format!("{} request aborted", flavor.label)
                } else {
                    format!("{} request failed", flavor.label)
                };
                let error: String = carrier
                    .and_then(|value| nonempty_string(value.get("errorMessage")))
                    .unwrap_or(&fallback)
                    .chars()
                    .take(4096)
                    .collect();
                (
                    if stop_reason == Some("aborted") {
                        499
                    } else {
                        500
                    },
                    Some(error),
                )
            }
            _ => (200, None),
        }
    } else {
        (200, None)
    };

    let identity = pi_request_identity(flavor, entry, kind, usage_value, carrier);

    // Oh My Pi writes the measured request duration (`duration`, milliseconds,
    // first-token wait included) on the call itself. Pi initializes an assistant message's timestamp
    // before the request and appends the entry on message_end, so there the
    // difference between the two is an estimate that includes first-token
    // waiting; other usage carriers have no equivalent request-start timestamp.
    let latency_ms = if attributed {
        carrier
            .and_then(|value| value.get("duration"))
            .and_then(Value::as_f64)
            .filter(|duration| *duration > 0.0)
            .and_then(|duration| estimated_latency_ms(0, duration.round() as i64))
            .or_else(|| {
                let start = carrier?.get("timestamp").and_then(parse_timestamp_millis)?;
                let end = entry.get("timestamp").and_then(parse_timestamp_millis)?;
                estimated_latency_ms(start, end)
            })
            .unwrap_or(0)
    } else {
        0
    };

    Some(PiUsageRecord {
        request_id: identity.request_id,
        semantic_id: identity.semantic_id,
        has_entry_id: identity.has_entry_id,
        provider_id,
        model,
        request_model,
        input_tokens,
        output_tokens,
        cache_read_tokens,
        cache_write_tokens,
        costs,
        latency_ms,
        status_code,
        error_message,
        created_at,
        session_id: session_id.to_string(),
    })
}

fn nonempty_string(value: Option<&Value>) -> Option<&str> {
    value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn bounded_label(value: Option<&Value>, fallback: &str) -> String {
    truncate_usage_label(nonempty_string(value).unwrap_or(fallback)).to_string()
}

fn truncate_usage_label(value: &str) -> &str {
    if value.len() <= MAX_USAGE_LABEL_BYTES {
        return value;
    }
    let mut end = MAX_USAGE_LABEL_BYTES;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}

fn token_count(usage: &Value, key: &str) -> u32 {
    usage
        .get(key)
        .and_then(Value::as_u64)
        .unwrap_or(0)
        .min(u32::MAX as u64) as u32
}

fn parse_costs(value: Option<&Value>) -> PiCosts {
    let decimal = |key| {
        value
            .and_then(|cost| cost.get(key))
            .and_then(parse_decimal)
            .unwrap_or(Decimal::ZERO)
            .max(Decimal::ZERO)
    };
    PiCosts {
        input: decimal("input"),
        output: decimal("output"),
        cache_read: decimal("cacheRead"),
        cache_write: decimal("cacheWrite"),
        total: decimal("total"),
    }
}

fn parse_decimal(value: &Value) -> Option<Decimal> {
    let raw = match value {
        Value::Number(number) => number.to_string(),
        Value::String(value) => value.clone(),
        _ => return None,
    };
    Decimal::from_str(&raw)
        .or_else(|_| Decimal::from_scientific(&raw))
        .ok()
}

fn parse_timestamp_millis(value: &Value) -> Option<i64> {
    let timestamp = if let Some(timestamp) = value.as_i64() {
        if !(-100_000_000_000..=100_000_000_000).contains(&timestamp) {
            timestamp
        } else {
            timestamp.saturating_mul(1000)
        }
    } else {
        value
            .as_str()
            .and_then(|timestamp| chrono::DateTime::parse_from_rfc3339(timestamp).ok())?
            .timestamp_millis()
    };
    (MIN_SQLITE_UNIX_MILLIS..=MAX_SQLITE_UNIX_MILLIS)
        .contains(&timestamp)
        .then_some(timestamp)
}

fn pi_request_identity(
    flavor: &'static Flavor,
    entry: &Value,
    kind: &str,
    usage: &Value,
    message: Option<&Value>,
) -> PiRequestIdentity {
    let mut hasher = Sha256::new();
    // Pi's tag and version stay byte-identical: the ledger that makes the ids
    // unique is durable, so a new domain separator would re-import history.
    let semantic_tag = format!("{}-semantic-v1", flavor.hash_tag);
    hash_field(&mut hasher, semantic_tag.as_bytes());
    hash_field(&mut hasher, kind.as_bytes());

    for (label, value) in [
        (b"entry_timestamp".as_slice(), entry.get("timestamp")),
        (
            b"message_timestamp".as_slice(),
            message.and_then(|value| value.get("timestamp")),
        ),
    ] {
        if let Some(value) = value {
            hash_field(&mut hasher, label);
            hash_json(&mut hasher, value);
        }
    }
    if let Some(message) = message {
        for key in [
            "provider",
            "model",
            "responseModel",
            "responseId",
            "api",
            "toolCallId",
            "toolName",
            "stopReason",
            "errorMessage",
        ] {
            if let Some(value) = message.get(key) {
                hash_field(&mut hasher, key.as_bytes());
                hash_json(&mut hasher, value);
            }
        }
        if let Some(content) = message.get("content") {
            hash_field(&mut hasher, b"content");
            hash_json(&mut hasher, content);
        }
    } else if let Some(summary) = entry.get("summary") {
        hash_field(&mut hasher, b"summary");
        hash_json(&mut hasher, summary);
    }
    hash_field(&mut hasher, b"usage");
    hash_json(&mut hasher, usage);
    let semantic_id = format!("{}_semantic:{:x}", flavor.id_prefix, hasher.finalize());
    let entry_id = nonempty_string(entry.get("id"));
    let request_id = if let Some(entry_id) = entry_id {
        let request_tag = format!("{}-request-v3", flavor.hash_tag);
        let mut request_hasher = Sha256::new();
        hash_field(&mut request_hasher, request_tag.as_bytes());
        hash_field(&mut request_hasher, kind.as_bytes());
        hash_field(&mut request_hasher, entry_id.as_bytes());
        if let Some(timestamp) = entry.get("timestamp") {
            hash_json(&mut request_hasher, timestamp);
        }
        format!("{}:{:x}", flavor.id_prefix, request_hasher.finalize())
    } else {
        semantic_id.clone()
    };
    PiRequestIdentity {
        request_id,
        semantic_id,
        has_entry_id: entry_id.is_some(),
    }
}

fn hash_json(hasher: &mut Sha256, value: &Value) {
    match value {
        Value::Null => hash_field(hasher, b"null"),
        Value::Bool(value) => {
            hash_field(hasher, b"bool");
            hash_field(hasher, if *value { b"true" } else { b"false" });
        }
        Value::Number(value) => {
            hash_field(hasher, b"number");
            hash_field(hasher, value.to_string().as_bytes());
        }
        Value::String(value) => {
            hash_field(hasher, b"string");
            hash_field(hasher, value.as_bytes());
        }
        Value::Array(values) => {
            hash_field(hasher, b"array");
            hash_field(hasher, &(values.len() as u64).to_be_bytes());
            for value in values {
                hash_json(hasher, value);
            }
        }
        Value::Object(values) => {
            hash_field(hasher, b"object");
            hash_field(hasher, &(values.len() as u64).to_be_bytes());
            let mut keys: Vec<_> = values.keys().collect();
            keys.sort_unstable();
            for key in keys {
                hash_field(hasher, key.as_bytes());
                hash_json(hasher, &values[key]);
            }
        }
    }
}

fn hash_field(hasher: &mut Sha256, value: &[u8]) {
    hasher.update((value.len() as u64).to_be_bytes());
    hasher.update(value);
}

fn insert_pi_record(
    conn: &rusqlite::Connection,
    flavor: &'static Flavor,
    record: &PiUsageRecord,
) -> Result<bool, AppError> {
    let ledger_error = |action: &str, error: rusqlite::Error| {
        AppError::Database(format!(
            "{action} {} 用量去重账本失败: {error}",
            flavor.label
        ))
    };
    let request_seen: bool = conn
        .query_row(
            PI_REQUEST_DEDUP_SQL,
            rusqlite::params![flavor.data_source, record.request_id],
            |row| row.get(0),
        )
        .map_err(|error| ledger_error("查询", error))?;
    let already_seen = request_seen
        || conn
            .query_row(
                if record.has_entry_id {
                    PI_LEGACY_SEMANTIC_DEDUP_SQL
                } else {
                    PI_SEMANTIC_DEDUP_SQL
                },
                rusqlite::params![flavor.data_source, record.semantic_id],
                |row| row.get(0),
            )
            .map_err(|error| ledger_error("查询", error))?;
    if already_seen {
        return Ok(false);
    }
    conn.execute(
        "INSERT OR IGNORE INTO session_usage_dedup
         (data_source, request_id, semantic_id, has_entry_id)
         VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![
            flavor.data_source,
            record.request_id,
            record.semantic_id,
            i64::from(record.has_entry_id),
        ],
    )
    .map_err(|error| ledger_error("写入", error))?;

    let usage = TokenUsage {
        input_tokens: record.input_tokens,
        output_tokens: record.output_tokens,
        cache_read_tokens: record.cache_read_tokens,
        cache_creation_tokens: record.cache_write_tokens,
        cache_creation_1h_tokens: 0,
        model: Some(record.model.clone()),
        message_id: None,
    };
    let costs = record.costs.reported().or_else(|| {
        find_model_pricing(conn, &record.model).map(|pricing| {
            let calculated =
                CostCalculator::calculate_for_app(flavor.app_type, &usage, &pricing, Decimal::ONE);
            (
                calculated.input_cost,
                calculated.output_cost,
                calculated.cache_read_cost,
                calculated.cache_creation_cost,
                calculated.total_cost,
            )
        })
    });
    let (input_cost, output_cost, cache_read_cost, cache_write_cost, total_cost) =
        costs.unwrap_or((
            Decimal::ZERO,
            Decimal::ZERO,
            Decimal::ZERO,
            Decimal::ZERO,
            Decimal::ZERO,
        ));

    conn.execute(
        "INSERT OR IGNORE INTO proxy_request_logs (
            request_id, provider_id, app_type, model, request_model, pricing_model,
            input_tokens, output_tokens, cache_read_tokens, cache_creation_tokens,
            input_token_semantics,
            input_cost_usd, output_cost_usd, cache_read_cost_usd,
            cache_creation_cost_usd, total_cost_usd,
            latency_ms, first_token_ms, status_code, error_message, session_id,
            provider_type, is_streaming, cost_multiplier, created_at, data_source
        ) VALUES (
            ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13,
            ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26
        )",
        rusqlite::params![
            record.request_id,
            record.provider_id,
            flavor.app_type,
            record.model,
            record.request_model,
            record.model,
            record.input_tokens,
            record.output_tokens,
            record.cache_read_tokens,
            record.cache_write_tokens,
            INPUT_TOKEN_SEMANTICS_FRESH,
            input_cost.to_string(),
            output_cost.to_string(),
            cache_read_cost.to_string(),
            cache_write_cost.to_string(),
            total_cost.to_string(),
            record.latency_ms,
            Option::<i64>::None,
            record.status_code,
            record.error_message,
            record.session_id,
            Some(flavor.data_source),
            1i64,
            "1.0",
            record.created_at,
            flavor.data_source,
        ],
    )
    .map(|changed| changed > 0)
    .map_err(|error| AppError::Database(format!("插入 {} 会话用量失败: {error}", flavor.label)))
}

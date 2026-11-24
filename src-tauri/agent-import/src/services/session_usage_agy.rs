// Adapted from CC Switch v4.0.40 (MIT; Copyright (c) 2025 Jason Young).
//! Exact counters exported by agy; absent models remain unknown (never use today's provider).
//! Amounts are the matching Gemini API prices, like every other session-log import.
use super::{
    session_usage::{cost_columns, metadata_modified_nanos, SessionSyncResult},
    sql_helpers::INPUT_TOKEN_SEMANTICS_FRESH,
};
use crate::proxy::usage::calculator::ModelPricing;
use crate::proxy::usage::parser::TokenUsage;
use crate::{
    database::{lock_conn, Database},
    error::AppError,
    session_manager::providers::agy::{self, model_from_settings_change},
};
use rusqlite::{params, OptionalExtension};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

mod native_models;
pub(crate) use native_models::native_state_fingerprint as native_fingerprint;

struct AgySyncCursorUpdate<'a> {
    key: &'a str,
    modified: i64,
    size: i64,
    native_state: &'a str,
}

#[cfg(test)]
pub(crate) fn sync_agy_usage(db: &Database) -> Result<SessionSyncResult, AppError> {
    sync_agy_usage_with_cursors(db, &crate::services::session_usage::load_sync_cursors(db)?)
}

pub(crate) fn sync_agy_usage_with_cursors(
    db: &Database,
    cursors: &crate::services::session_usage::SyncCursors,
) -> Result<SessionSyncResult, AppError> {
    sync_agy_usage_from_files_with_cursors(db, agy::transcript_files(), cursors)
}

#[cfg(test)]
fn sync_agy_usage_from_files(
    db: &Database,
    files: Vec<(String, PathBuf)>,
) -> Result<SessionSyncResult, AppError> {
    sync_agy_usage_from_files_with_cursors(
        db,
        files,
        &crate::services::session_usage::load_sync_cursors(db)?,
    )
}

fn sync_agy_usage_from_files_with_cursors(
    db: &Database,
    files: Vec<(String, PathBuf)>,
    cursors: &crate::services::session_usage::SyncCursors,
) -> Result<SessionSyncResult, AppError> {
    let mut result = SessionSyncResult::default();
    if files.is_empty() {
        return Ok(result);
    }
    for (session_id, path) in files {
        result.files_scanned += 1;
        let cursor_key = format!("agy:{}", path.to_string_lossy());
        let cursor = cursors.get(&cursor_key);
        let metadata = match std::fs::metadata(&path) {
            Ok(meta) => meta,
            Err(e) => {
                result
                    .errors
                    .push(format!("Cannot read Antigravity transcript: {e}"));
                continue;
            }
        };
        let modified = metadata_modified_nanos(&metadata);
        let size = metadata.len() as i64;
        // Invalidate pre-Orchestrator / pre-worker-alias / pre-Neon-Orchestrator cursors once,
        // including unchanged, already timed history.
        let native_state = format!("v4:{}", native_models::native_state_fingerprint(&path));
        if cursor.is_some_and(|c| {
            c.last_modified == modified
                && c.last_byte_offset == Some(size)
                && c.parser_state.as_deref() == Some(native_state.as_str())
        }) {
            continue;
        }
        match agy::records(&path) {
            Ok(rows) => {
                let sync_cursor = AgySyncCursorUpdate {
                    key: &cursor_key,
                    modified,
                    size,
                    native_state: &native_state,
                };
                match import_records_with_cursor(
                    db,
                    &session_id,
                    &rows,
                    Some(&path),
                    Some(sync_cursor),
                ) {
                    Ok((imported, skipped)) => {
                        result.imported += imported;
                        result.skipped += skipped;
                    }
                    Err(e) => result.errors.push(format!("Antigravity {session_id}: {e}")),
                }
            }
            Err(e) => result.errors.push(e),
        }
    }
    Ok(result)
}

/// agy 记下的是显示名（`Gemini 3.8 Flash (High)`）；换成 `agy models` 第一列那样的 ID
/// （`gemini-3.8-flash-high`）才能对上定价表，查价时档位后缀会被剥掉。已经是 ID 的原样返回。
fn model_id_of(display: &str) -> String {
    if !display.contains(' ') {
        return display.to_string();
    }
    display
        .to_ascii_lowercase()
        .replace(" (", "-")
        .replace(['(', ')'], "")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join("-")
}

/// Multi-model modes, not evidence of the model used for an individual response.
/// Display name and native alias of the same mode map to the same planner.
fn orchestrator_mode(model: &str) -> Option<&'static str> {
    match model_id_of(model.trim()).to_ascii_lowercase().as_str() {
        "orchestrator-carbon" | "gemini-pro-orchestrator" => Some("carbon"),
        "orchestrator-argon"
        | "gemini-pro-orchestrator-argon"
        | "gemini-pro-orchestrator-g1-pro" => Some("argon"),
        "orchestrator-neon" | "gemini-pro-orchestrator-neon" => Some("neon"),
        _ => None,
    }
}

fn is_orchestrator(model: &str) -> bool {
    orchestrator_mode(model).is_some()
}

/// Per-step alias exported by Antigravity (`agy` CLI and Desktop 2.33+) on Orchestrator worker
/// generations (`MODEL_PLACEHOLDER_M669`). Unlike `orchestrator_mode`, this already identifies the
/// Hydrogen worker role even without `conversations/<id>.db`, while still preferring native
/// field-21 display/tier metadata when present.
fn is_orchestrator_worker(model: &str) -> bool {
    model_id_of(model.trim()).eq_ignore_ascii_case("gemini-flash-worker-agy")
}

/// Mode names, role aliases, and generic labels that do not name a concrete worker model/tier.
fn is_unresolved_worker_target(model: &str) -> bool {
    let id = model_id_of(model.trim());
    id.is_empty()
        || id.eq_ignore_ascii_case("unknown")
        || is_orchestrator(&id)
        || is_orchestrator_worker(&id)
        || id == "gemini-flash"
}

/// 五个金额（输入、输出、缓存读、缓存写、合计）；查不到价格的记 0。
fn costs(
    conn: &rusqlite::Connection,
    pricing_cache: &mut HashMap<String, Option<ModelPricing>>,
    model: &str,
    tokens: [i64; 4],
) -> [String; 5] {
    let [input, output, cache_read, cache_write] =
        tokens.map(|v| u32::try_from(v).unwrap_or(u32::MAX));
    let usage = TokenUsage {
        input_tokens: input,
        output_tokens: output,
        cache_read_tokens: cache_read,
        cache_creation_tokens: cache_write,
        cache_creation_1h_tokens: 0,
        model: Some(model.to_string()),
        message_id: None,
    };
    let pricing = pricing_cache
        .entry(model.to_string())
        .or_insert_with(|| crate::services::usage_stats::find_model_pricing(conn, model));
    cost_columns("agy", &usage, pricing.as_ref(), rust_decimal::Decimal::ONE)
}

#[cfg(test)]
fn import_records(
    db: &Database,
    session: &str,
    rows: &[Value],
    transcript: Option<&Path>,
) -> Result<(u32, u32), AppError> {
    import_records_with_cursor(db, session, rows, transcript, None)
}

fn import_records_with_cursor(
    db: &Database,
    session: &str,
    rows: &[Value],
    transcript: Option<&Path>,
    sync_cursor: Option<AgySyncCursorUpdate<'_>>,
) -> Result<(u32, u32), AppError> {
    // step_index 顺序决定模型归属：先按顺序扫一遍切换事件，再给每个响应打上当时的模型。
    let mut ordered: Vec<&Value> = rows.iter().collect();
    ordered.sort_by_key(|row| row["step_index"].as_u64().unwrap_or(u64::MAX));
    let mut current_model: Option<String> = None;
    let mut model_by_step = std::collections::HashMap::<u64, String>::new();
    for row in &ordered {
        let Some(step) = row["step_index"].as_u64() else {
            continue;
        };
        if row["type"] == "USER_INPUT" {
            if let Some(model) = row["content"].as_str().and_then(model_from_settings_change) {
                current_model = Some(model);
            }
        } else if row["type"] == "PLANNER_RESPONSE" {
            if let Some(model) = &current_model {
                model_by_step.entry(step).or_insert_with(|| model.clone());
            }
        }
    }

    // 子代理通常没有 Model Selection；Orchestrator 的选择只代表模式，需逐次读取实际模型。
    // 耗时和首字也只在原生库里有（transcript 的时间戳只到秒、没有完成时间）。
    let mut timing_by_step = std::collections::HashMap::new();
    let mut native_model_by_step = HashMap::new();
    let mut native_ok = true;
    if let Some(transcript) = transcript {
        let (settled, pruned) = steps_with_timing(db, session, &ordered)?;
        let wanted: Vec<u64> = ordered
            .iter()
            .filter(|row| row["type"] == "PLANNER_RESPONSE" && row["status"] == "DONE")
            .filter_map(|row| {
                let step = row["step_index"].as_u64()?;
                let selected =
                    known_model(row).or_else(|| model_by_step.get(&step).map(String::as_str));
                // Pruned steps can never be updated again, so their model is moot.
                let needs_model = selected
                    .is_none_or(|m| is_orchestrator(m) || is_orchestrator_worker(m))
                    && !pruned.contains(&step);
                (needs_model || !settled.contains(&step)).then_some(step)
            })
            .collect();
        if !wanted.is_empty() {
            match native_models::generations_for_transcript(transcript, &wanted) {
                Ok(generations) => {
                    for (step, generation) in generations {
                        if let Some(model) = generation.model {
                            native_model_by_step.insert(step, model);
                        }
                        if let Some(timing) = generation.timing {
                            timing_by_step.insert(step, timing);
                        }
                    }
                }
                Err(error) => {
                    native_ok = false;
                    log::debug!("Antigravity {session}: cannot read native models: {error}");
                }
            }
        }
    }

    let mut conn = lock_conn!(db.conn);
    let tx = conn.transaction()?;
    let mut pricing_cache = HashMap::new();
    let (mut imported, mut skipped) = (0, 0);
    for row in rows {
        if row["type"] != "PLANNER_RESPONSE" || row["status"] != "DONE" {
            continue;
        }
        let Some(step) = row["step_index"].as_u64() else {
            continue;
        };
        let token = |name: &str| {
            row[name]
                .as_u64()
                .and_then(|v| i64::try_from(v).ok())
                .unwrap_or(0)
        };
        let input = token("input_tokens");
        let output = token("output_tokens");
        let cache = token("cache_read_tokens");
        let write = token("cache_write_tokens");
        if input == 0 && output == 0 && cache == 0 && write == 0 {
            continue;
        }
        let id = format!("agy:{session}:{step}");
        // Persist a tombstone with the row so retention never imports pruned history again.
        let fresh = tx
            .prepare_cached("INSERT OR IGNORE INTO session_usage_dedup(data_source,request_id,semantic_id,has_entry_id) VALUES ('agy_session',?1,?1,1)")?
            .execute([&id])?;
        // `model` 存 ID（对得上定价和其它应用的统计），`request_model` 留显示名。
        let display = known_model(row)
            .map(str::to_string)
            .or_else(|| model_by_step.get(&step).cloned())
            .or_else(|| native_model_by_step.get(&step).cloned())
            .unwrap_or_else(|| "unknown".to_string());
        let native_worker_model = native_model_by_step
            .get(&step)
            .map(String::as_str)
            .filter(|model| !is_unresolved_worker_target(model));
        let worker_without_native =
            is_orchestrator_worker(&display) && native_worker_model.is_none();
        let actual = if is_orchestrator(&display) {
            native_model_by_step
                .get(&step)
                .map(String::as_str)
                .unwrap_or(&display)
        } else if is_orchestrator_worker(&display) {
            native_worker_model.unwrap_or("gemini-4-hydrogen")
        } else {
            &display
        };
        let model = model_id_of(actual);
        // 原生库暂时不可读时，保留已恢复的模型；Token 仍按最新日志更新。
        let (model, display) = if (model == "unknown"
            || is_orchestrator(&model)
            || worker_without_native)
            && fresh == 0
        {
            let previous = tx
                .prepare_cached(
                    "SELECT model, COALESCE(request_model, model) FROM proxy_request_logs
                  WHERE request_id=?1 AND model!='unknown'",
                )?
                .query_row([&id], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })
                .optional()?;
            match previous {
                Some((old_model, old_display)) if model == "unknown" => (old_model, old_display),
                Some((old_model, old_display))
                    if worker_without_native
                        && ((is_orchestrator_worker(&old_display)
                            && !is_unresolved_worker_target(&old_model))
                            || (is_orchestrator(&old_display)
                                && old_model.starts_with("gemini-4-hydrogen"))) =>
                {
                    (old_model, display)
                }
                Some((old_model, old_display))
                    if !worker_without_native
                        && !is_orchestrator(&old_model)
                        && !is_orchestrator_worker(&old_model)
                        && orchestrator_mode(&old_display) == orchestrator_mode(&display) =>
                {
                    (old_model, old_display)
                }
                _ => (model, display),
            }
        } else {
            (model, display)
        };
        let [input_cost, output_cost, cache_cost, write_cost, total_cost] = costs(
            &tx,
            &mut pricing_cache,
            &model,
            [input, output, cache, write],
        );
        // 原生库记的是精确值；读不到时耗时记 0、首字记 NULL，和其它会话日志一样表示「没有计时」。
        let timing = timing_by_step.get(&step);
        let latency_ms = timing.map_or(0, |t| t.latency_ms);
        let first_token_ms = timing.map(|t| t.first_token_ms);
        if fresh == 0 {
            // A rewritten completed step may correct counters; update retained detail only.
            // If retention already removed it, its tombstone still forbids resurrection.
            // Timing is only ever filled in: an unreadable native database keeps what was stored.
            let changed = tx
                .prepare_cached("UPDATE proxy_request_logs SET model=?2,request_model=?7,input_tokens=?3,output_tokens=?4,cache_read_tokens=?5,cache_creation_tokens=?6,input_cost_usd=?8,output_cost_usd=?9,cache_read_cost_usd=?10,cache_creation_cost_usd=?11,total_cost_usd=?12,latency_ms=CASE WHEN ?13>0 THEN ?13 ELSE latency_ms END,first_token_ms=COALESCE(?14,first_token_ms) WHERE request_id=?1 AND (model!=?2 OR request_model!=?7 OR input_tokens!=?3 OR output_tokens!=?4 OR cache_read_tokens!=?5 OR cache_creation_tokens!=?6 OR total_cost_usd!=?12 OR (?13>0 AND latency_ms!=?13) OR (?14 IS NOT NULL AND first_token_ms IS NOT ?14))")?
                .execute(params![id,model,input,output,cache,write,display,input_cost,output_cost,cache_cost,write_cost,total_cost,latency_ms,first_token_ms])?;
            imported += changed as u32;
            skipped += u32::from(changed == 0);
            continue;
        }
        // Native agy was verified with a local mock: input excludes cache, output includes
        // thinking. Never subtract cache or add thinking a second time. Missing historical
        // models remain unknown and unpriced; never fall back to the active provider.
        tx.prepare_cached("INSERT INTO proxy_request_logs (
            request_id,provider_id,app_type,model,request_model,input_tokens,output_tokens,
            cache_read_tokens,cache_creation_tokens,input_cost_usd,output_cost_usd,
            cache_read_cost_usd,cache_creation_cost_usd,total_cost_usd,latency_ms,first_token_ms,status_code,
            session_id,provider_type,is_streaming,cost_multiplier,created_at,data_source,input_token_semantics
        ) VALUES (?1,'_agy_session','agy',?2,?10,?3,?4,?5,?6,?11,?12,?13,?14,?15,?16,?17,200,?7,'agy_session',1,'1',?8,'agy_session',?9)")?
        .execute(params![id, model, input, output, cache, write, session, agy::timestamp(row).unwrap_or(0)/1000, INPUT_TOKEN_SEMANTICS_FRESH, display, input_cost, output_cost, cache_cost, write_cost, total_cost, latency_ms, first_token_ms])?;
        imported += 1;
    }
    if let Some(cursor) = sync_cursor {
        let parser_state = native_ok.then_some(cursor.native_state);
        tx.prepare_cached(
            "INSERT OR REPLACE INTO session_log_sync
                 (file_path, last_modified, last_line_offset, last_synced_at, last_byte_offset, parser_state)
             VALUES (?1, ?2, 0, unixepoch(), ?3, ?4)",
        )?
        .execute(params![
            cursor.key,
            cursor.modified,
            cursor.size,
            parser_state,
        ])?;
    }
    tx.commit()?;
    Ok((imported, skipped))
}

/// Steps that need nothing more from the native database for timing: imported with a
/// timing already, or imported and since pruned (the tombstone forbids re-importing them).
/// Also returns the pruned subset on its own. Keeps steady-state syncs from reopening
/// every conversation database.
fn steps_with_timing(
    db: &Database,
    session: &str,
    rows: &[&Value],
) -> Result<(HashSet<u64>, HashSet<u64>), AppError> {
    let conn = lock_conn!(db.conn);
    let mut query = conn.prepare(
        "SELECT EXISTS(SELECT 1 FROM session_usage_dedup
                       WHERE data_source='agy_session' AND request_id=?1),
                EXISTS(SELECT 1 FROM proxy_request_logs WHERE request_id=?1),
                EXISTS(SELECT 1 FROM proxy_request_logs
                       WHERE request_id=?1 AND latency_ms<=0)",
    )?;
    let (mut settled, mut pruned) = (HashSet::new(), HashSet::new());
    for step in rows
        .iter()
        .filter(|row| row["type"] == "PLANNER_RESPONSE" && row["status"] == "DONE")
        .filter_map(|row| row["step_index"].as_u64())
    {
        let (imported, detail, untimed) =
            query.query_row([format!("agy:{session}:{step}")], |r| {
                Ok((
                    r.get::<_, bool>(0)?,
                    r.get::<_, bool>(1)?,
                    r.get::<_, bool>(2)?,
                ))
            })?;
        if imported && !untimed {
            settled.insert(step);
        }
        if imported && !detail {
            pruned.insert(step);
        }
    }
    Ok((settled, pruned))
}

fn known_model(row: &Value) -> Option<&str> {
    row["model"]
        .as_str()
        .filter(|model| !model.trim().is_empty() && !model.eq_ignore_ascii_case("unknown"))
}

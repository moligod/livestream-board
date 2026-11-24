// Adapted from CC Switch v4.0.40 (MIT; Copyright (c) 2025 Jason Young).
//! Import Kimi Code's committed request usage. Tokens are already normalized to fresh input.
use crate::database::{lock_conn, Database};
use crate::error::AppError;
use crate::kimi_session::{timestamp_ms, RequestIndex, RequestInfo};
use crate::proxy::usage::parser::TokenUsage;
use crate::services::{
    session_usage::{
        cost_columns, metadata_modified_nanos, update_sync_state_on_conn, SessionSyncResult,
    },
    sql_helpers::INPUT_TOKEN_SEMANTICS_FRESH,
    usage_stats::find_model_pricing,
};
use rusqlite::OptionalExtension;
use rust_decimal::Decimal;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, VecDeque},
    fs,
    io::{BufRead, BufReader},
    path::Path,
};

const KIMI_USAGE_PARSER_STATE: &str = "step-usage-v2";
type UsageKey = (u64, u64, u64, u64);

fn usage_key(usage: &Value) -> UsageKey {
    let token = |field| usage[field].as_u64().unwrap_or(0);
    (
        token("inputOther"),
        token("output"),
        token("inputCacheRead"),
        token("inputCacheCreation"),
    )
}

pub(crate) fn sync_kimi_usage(
    db: &Database,
    cursors: &crate::services::session_usage::SyncCursors,
) -> Result<SessionSyncResult, AppError> {
    sync_sessions_with_cursors(
        db,
        &crate::kimi_config::data_dir().join("sessions"),
        cursors,
    )
}

#[cfg(test)]
fn sync_sessions(db: &Database, root: &Path) -> Result<SessionSyncResult, AppError> {
    sync_sessions_with_cursors(
        db,
        root,
        &crate::services::session_usage::load_sync_cursors(db)?,
    )
}

fn sync_sessions_with_cursors(
    db: &Database,
    root: &Path,
    cursors: &crate::services::session_usage::SyncCursors,
) -> Result<SessionSyncResult, AppError> {
    let mut result = SessionSyncResult::default();
    let Ok(workspaces) = fs::read_dir(root) else {
        return Ok(result);
    };
    for workspace in workspaces.flatten() {
        let Ok(sessions) = fs::read_dir(workspace.path()) else {
            continue;
        };
        for session in sessions.flatten() {
            // The main agent and every subagent (`agents/agent-N`) keep separate wires.
            let Ok(agents) = fs::read_dir(session.path().join("agents")) else {
                continue;
            };
            for agent in agents.flatten() {
                let path = agent.path().join("wire.jsonl");
                if !path.is_file() {
                    continue;
                }
                let modified = fs::metadata(&path)
                    .map(|meta| metadata_modified_nanos(&meta))
                    .unwrap_or(0);
                let key = format!("kimi:{}", path.to_string_lossy());
                if cursors.get(&key).is_some_and(|cursor| {
                    cursor.last_modified == modified
                        && cursor.last_line_offset
                            == fs::metadata(&path)
                                .map(|meta| meta.len() as i64)
                                .unwrap_or(-1)
                        && cursor.parser_state.as_deref() == Some(KIMI_USAGE_PARSER_STATE)
                }) {
                    continue;
                }
                match sync_file(
                    db,
                    &path,
                    &agent.file_name().to_string_lossy(),
                    &session.file_name().to_string_lossy(),
                    &key,
                    modified,
                ) {
                    Ok(part) => result.merge(part),
                    Err(error) => result
                        .errors
                        .push(format!("Kimi Code session {}: {error}", path.display())),
                }
            }
        }
    }
    Ok(result)
}

fn sync_file(
    db: &Database,
    path: &Path,
    agent: &str,
    session: &str,
    key: &str,
    modified: i64,
) -> Result<SessionSyncResult, AppError> {
    let file = fs::File::open(path).map_err(|error| AppError::io(path, error))?;
    let mut reader = BufReader::new(file);
    let mut buffer = Vec::new();
    let mut bytes = 0i64;
    let main = agent == "main";
    let mut requests = if main {
        RequestIndex::default()
    } else {
        RequestIndex::for_agent(agent)
    };
    let mut pending = Vec::new();
    let mut usage_models: HashMap<UsageKey, VecDeque<Option<String>>> = HashMap::new();
    let mut result = SessionSyncResult {
        files_scanned: 1,
        ..Default::default()
    };
    loop {
        buffer.clear();
        let size = reader
            .read_until(b'\n', &mut buffer)
            .map_err(|error| AppError::io(path, error))?;
        if size == 0 {
            break;
        }
        // A live writer may have flushed only part of its last record. Retry it next round.
        if buffer.last() != Some(&b'\n') {
            break;
        }
        bytes += size as i64;
        let Ok(record) = serde_json::from_slice::<Value>(&buffer) else {
            continue;
        };
        if record["agentId"].as_str().is_some_and(|id| id != agent) {
            continue;
        }
        // Interrupted steps may never emit step.end. Their usage must not supply
        // a model to a resumed step with identical token counts.
        match record["type"].as_str() {
            Some("agent.switched" | "agent.turn.started") => usage_models.clear(),
            Some("context.append_loop_event") if record["event"]["type"] == "step.begin" => {
                usage_models.clear();
            }
            _ => {}
        }
        if record["type"].as_str() == Some("usage.record") {
            let model = record["model"]
                .as_str()
                .filter(|model| {
                    !model.trim().is_empty() && !matches!(*model, "agent-loop" | "unknown")
                })
                .map(str::to_owned);
            usage_models
                .entry(usage_key(&record["usage"]))
                .or_default()
                .push_back(model);
            continue;
        }
        requests.observe(&record);
        let legacy_main = main
            && matches!(
                record["type"].as_str(),
                Some("agent.message.appended" | "human.agent.message.appended")
            );
        let (meta, usage) = if legacy_main {
            if record
                .pointer("/message/message/role")
                .and_then(Value::as_str)
                != Some("assistant")
            {
                continue;
            }
            let meta = record["message"]["meta"].clone();
            let Some(usage) = meta.get("usage").cloned() else {
                continue;
            };
            (meta, usage)
        } else {
            // Newer wires carry usage on step.end instead of an assistant projection.
            // Keeping the legacy provider and messageId makes a projection and its
            // equivalent step share one permanent receipt when both are present.
            let event = &record["event"];
            if record["type"].as_str() != Some("context.append_loop_event")
                || event["type"].as_str() != Some("step.end")
            {
                continue;
            }
            // Discard every unmatched record even when the step has no usage.
            let mut step_models = std::mem::take(&mut usage_models);
            let Some(usage) = event.get("usage").cloned() else {
                continue;
            };
            let model = step_models
                .get_mut(&usage_key(&usage))
                .and_then(VecDeque::pop_front)
                .flatten();
            let provider = if main { "agent-loop" } else { "subagent" };
            let meta = serde_json::json!({
                "messageId": event["messageId"],
                "model": {"provider": provider, "model": model},
            });
            (meta, usage)
        };
        let Some(created) = timestamp_ms(&record["time"]) else {
            continue;
        };
        let tokens = |field| usage[field].as_u64().unwrap_or(0).min(u32::MAX as u64) as u32;
        let usage = TokenUsage {
            input_tokens: tokens("inputOther"),
            output_tokens: tokens("output"),
            cache_read_tokens: tokens("inputCacheRead"),
            cache_creation_tokens: tokens("inputCacheCreation"),
            cache_creation_1h_tokens: 0,
            model: None,
            message_id: None,
        };
        if usage.input_tokens == 0
            && usage.output_tokens == 0
            && usage.cache_read_tokens == 0
            && usage.cache_creation_tokens == 0
        {
            continue;
        }
        let identity = meta
            .get("messageId")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| format!("{:x}", Sha256::digest(record.to_string().as_bytes())));
        let provider = meta
            .pointer("/model/provider")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let request = format!("kimi:{provider}:{identity}");
        // Retain only metadata and usage. step.end may appear after the human projection
        // (and after tools); resolve its messageId before taking the database lock.
        pending.push((request, meta, usage, created));
    }
    let mut conn = lock_conn!(db.conn);
    let tx = conn.transaction()?;
    for (request, meta, mut usage, created) in pending {
        let model = requests.model(&meta).unwrap_or("unknown");
        usage.model = Some(model.into());
        let info = requests.get(&meta);
        let latency = info.map_or(0, |info| info.latency_ms);
        let first_token = info.and_then(|info| info.first_token_ms);
        let started = info.and_then(|info| info.request_ms);
        // Permanent receipts prevent re-import after wire rewrites, forks, or log pruning.
        if tx.execute("INSERT OR IGNORE INTO session_usage_dedup (data_source, request_id, semantic_id, has_entry_id) VALUES ('kimi_session', ?1, ?1, 1)", [&request])? == 0 {
            // As with Claude imports, enrichment counts as changed data so the normal
            // usage-log-recorded notification refreshes open tables and detail panels.
            if refresh_existing(&tx, &request, model, &usage, info, created)? {
                result.imported += 1;
            } else {
                result.skipped += 1;
            }
            continue;
        }
        let amounts = costs(&tx, model, &usage, Decimal::ONE);
        tx.execute("INSERT OR IGNORE INTO proxy_request_logs (
            request_id, provider_id, app_type, model, request_model, input_tokens, output_tokens, cache_read_tokens, cache_creation_tokens,
            input_cost_usd, output_cost_usd, cache_read_cost_usd, cache_creation_cost_usd, total_cost_usd,
            latency_ms, first_token_ms, status_code, session_id, provider_type, is_streaming, cost_multiplier, created_at, data_source, input_token_semantics
        ) VALUES (?1, '_kimi_session', 'kimi', ?2, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?15, ?16, 200, ?12, 'kimi_session', 1, '1', ?13, 'kimi_session', ?14)",
            rusqlite::params![request, model, usage.input_tokens, usage.output_tokens, usage.cache_read_tokens, usage.cache_creation_tokens,
                amounts[0], amounts[1], amounts[2], amounts[3], amounts[4], session, started.map_or(created, |start| completed_ms(start, latency, created))/1000, INPUT_TOKEN_SEMANTICS_FRESH, latency, first_token])?;
        result.imported += 1;
    }
    update_sync_state_on_conn(&tx, key, modified, bytes)?;
    tx.execute(
        "UPDATE session_log_sync SET parser_state = ?2 WHERE file_path = ?1",
        rusqlite::params![key, KIMI_USAGE_PARSER_STATE],
    )?;
    tx.commit()?;
    Ok(result)
}

/// Kimi commits a whole turn's messages at once, so a committed record's own time is the
/// commit, shared by every request in the turn. Date each request by when its model call
/// finished instead, which is also when the proxy records requests.
fn completed_ms(started: i64, latency: i64, committed: i64) -> i64 {
    started.saturating_add(latency).min(committed)
}

fn costs(
    conn: &rusqlite::Connection,
    model: &str,
    usage: &TokenUsage,
    multiplier: Decimal,
) -> [String; 5] {
    cost_columns(
        "kimi",
        usage,
        find_model_pricing(conn, model).as_ref(),
        multiplier,
    )
}

/// Enrich retained details only. A receipt without a detail row belongs to pruned history;
/// recreating it would count the request again on top of its daily rollup.
fn refresh_existing(
    conn: &rusqlite::Connection,
    request: &str,
    model: &str,
    usage: &TokenUsage,
    info: Option<&RequestInfo>,
    committed: i64,
) -> Result<bool, AppError> {
    let latency = info.map_or(0, |info| info.latency_ms);
    let first_token = info.and_then(|info| info.first_token_ms);
    let stored: Option<(String, String, i64, Option<i64>, i64)> = conn
        .query_row(
            "SELECT model, cost_multiplier, latency_ms, first_token_ms, created_at FROM proxy_request_logs
         WHERE request_id = ?1 AND app_type = 'kimi' AND data_source = 'kimi_session'",
            [request],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
        )
        .optional()?;
    let Some((old_model, multiplier, old_latency, old_first, old_created)) = stored else {
        return Ok(false);
    };
    let model_changed = model != old_model && model != "unknown";
    // Preserve native timing when a copied history contains only timestamp estimates.
    let latency = if latency > 0 && (old_first.is_none() || first_token.is_some()) {
        latency
    } else {
        old_latency
    };
    let first_token = first_token.or(old_first).filter(|first| *first <= latency);
    // Rows imported before their request events were readable carry the commit time.
    let created = info
        .and_then(|info| info.request_ms)
        .map_or(old_created, |start| {
            completed_ms(start, latency, committed) / 1000
        });
    if !model_changed
        && latency == old_latency
        && first_token == old_first
        && created == old_created
    {
        return Ok(false);
    }
    if model_changed {
        let amounts = costs(
            conn,
            model,
            usage,
            multiplier.parse().unwrap_or(Decimal::ONE),
        );
        conn.execute(
            "UPDATE proxy_request_logs SET model = ?2, request_model = ?2,
             input_cost_usd = ?3, output_cost_usd = ?4, cache_read_cost_usd = ?5,
             cache_creation_cost_usd = ?6, total_cost_usd = ?7
             WHERE request_id = ?1 AND app_type = 'kimi' AND data_source = 'kimi_session'",
            rusqlite::params![
                request, model, amounts[0], amounts[1], amounts[2], amounts[3], amounts[4]
            ],
        )?;
    }
    conn.execute(
        "UPDATE proxy_request_logs SET latency_ms = ?2, first_token_ms = ?3, created_at = ?4
         WHERE request_id = ?1 AND app_type = 'kimi' AND data_source = 'kimi_session'",
        rusqlite::params![request, latency, first_token, created],
    )?;
    Ok(true)
}

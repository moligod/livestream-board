use crate::{codex_rebuild, models::UsageRecord, session_collectors, trae_crypto};
use chrono::Utc;
use rusqlite::Connection;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{collections::HashMap, path::{Path, PathBuf}};

// The web-only `dashboard-json` binary reuses this module directly, while the
// snapshot itself is owned by the Tauri command layer. It is therefore unused
// in that one compilation target but remains required by `commands.rs`.
#[allow(dead_code)]
pub fn dashboard_snapshot_path() -> PathBuf {
    project_root().join("data/cache/dashboard.json")
}

fn project_root() -> PathBuf {
    let current = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    if current.file_name().and_then(|name| name.to_str()) == Some("src-tauri") {
        current.parent().unwrap_or(&current).to_path_buf()
    } else {
        current
    }
}

pub fn claude_path() -> PathBuf {
    std::env::var_os("CLAUDE_CONFIG_DIR").map(PathBuf::from).unwrap_or_else(|| home().join(".claude")).join("projects")
}

pub fn collect_claude_with_warning(path: &Path) -> Result<(Vec<UsageRecord>, Option<String>), String> {
    collect_claude_with_options(path, false)
}

pub fn collect_claude_with_options(path: &Path, changes_only: bool) -> Result<(Vec<UsageRecord>, Option<String>), String> {
    let cache = std::env::var_os("TOKENUSAGE_CACHE_DIR").map(PathBuf::from).unwrap_or_else(|| project_root().join("data/cache"));
    let default = home().join(".claude/projects");
    let name = scoped_cache_name("claude-sessions", path, &default);
    let (records, retained) = session_collectors::collect_claude_with_options(path, &cache.join(name), changes_only)?;
    let warning = (retained > 0).then(|| format!("保留了 {retained} 个已移除日志的历史记录。"));
    Ok((records, warning))
}

#[allow(dead_code)]
pub fn collect_claude(path: &Path) -> Result<Vec<UsageRecord>, String> {
    collect_claude_with_warning(path).map(|(records, _)| records)
}

fn home() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("."))
}

fn project_name(path: Option<String>) -> String {
    path.map(|value| value.replace('\\', "/").rsplit('/').next().unwrap_or("unknown").to_string())
        .filter(|value| !value.is_empty()).unwrap_or_else(|| "unknown".into())
}

pub fn default_paths() -> (PathBuf, PathBuf, PathBuf, PathBuf) {
    let appdata = std::env::var_os("APPDATA").map(PathBuf::from)
        .unwrap_or_else(|| home().join("AppData/Roaming"));
    (
        home().join(".codex"),
        appdata.join("Trae CN/ModularData/ai-agent/database.db"),
        appdata.join("Trae/ModularData/ai-agent/database.db"),
        home().join(".local/share/opencode/opencode.db"),
    )
}

fn scoped_cache_name(prefix: &str, path: &Path, default: &Path) -> String {
    let actual = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let standard = std::fs::canonicalize(default).unwrap_or_else(|_| default.to_path_buf());
    if actual == standard { return format!("{prefix}.json"); }
    let digest = Sha256::digest(actual.to_string_lossy().as_bytes());
    let short = digest[..12].iter().map(|b| format!("{b:02x}")).collect::<String>();
    format!("{prefix}-{short}.json")
}

fn codex_cache_path(path: &Path) -> PathBuf {
    let name = scoped_cache_name("codex-rollouts", path, &home().join(".codex"));
    if let Some(path) = std::env::var_os("TOKENUSAGE_CACHE_DIR") {
        return PathBuf::from(path).join(name);
    }
    project_root().join("data/cache").join(name)
}

pub fn collect_codex_with_warning(path: &Path, rebuild: bool) -> Result<(Vec<UsageRecord>, Option<String>), String> {
    collect_codex_with_options(path, rebuild, false)
}

pub fn collect_codex_with_options(path: &Path, rebuild: bool, changes_only: bool) -> Result<(Vec<UsageRecord>, Option<String>), String> {
    let scan = codex_rebuild::scan_with_options(path, &codex_cache_path(path), rebuild, changes_only)?;
    eprintln!("TokenUsage Codex: checked {}, parsed {}, deferred {}", scan.files_scanned, scan.files_parsed, scan.deferred_files);
    // Like CC Switch, pending fork validation is a collector diagnostic, not
    // a dashboard source error. Keep deferral and retry logic unchanged.
    if scan.deferred_files > 0 {
        let mut reasons: Vec<_> = scan.deferred_reasons.into_iter().collect();
        reasons.sort();
        let reasons = reasons.into_iter().map(|(reason, count)| format!("{reason} {count}")).collect::<Vec<_>>().join("、");
        eprintln!("TokenUsage Codex pending fork validation: {reasons}");
    }
    let warning = (scan.retained_files > 0).then(|| format!("保留了 {} 个已移除日志的历史记录。", scan.retained_files));
    Ok((scan.records, warning))
}

#[allow(dead_code)]
pub fn collect_codex(path: &Path) -> Result<Vec<UsageRecord>, String> {
    collect_codex_with_warning(path, false).map(|(records, _)| records)
}

#[allow(dead_code)]
pub fn rebuild_codex(path: &Path) -> Result<Vec<UsageRecord>, String> {
    collect_codex_with_warning(path, true).map(|(records, _)| records)
}

pub fn collect_opencode(path: &Path) -> Result<Vec<UsageRecord>, String> {
    if !path.exists() { return Ok(Vec::new()); }
    session_collectors::collect_opencode(path)
}

pub fn collect_trae(path: &Path, source: &str) -> Result<Vec<UsageRecord>, String> {
    if !path.exists() { return Ok(Vec::new()); }
    let readable_path = trae_crypto::ensure_decrypted(path, source)?;
    let conn = Connection::open_with_flags(&readable_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(|e| e.to_string())?;
    let mut project_map = HashMap::new();
    if let Ok(mut stmt) = conn.prepare("SELECT sp.session_id, p.absolute_path FROM session_project sp LEFT JOIN project p ON p.project_id=sp.project_id") {
        let rows = stmt.query_map([], |row| Ok((row.get::<_, String>(0)?, project_name(row.get(1)?)))).map_err(|e| e.to_string())?;
        for row in rows.flatten() { project_map.insert(row.0, row.1); }
    }
    let mut statement = conn.prepare("SELECT turn_id, session_id, created_at, context FROM chat_turn WHERE context IS NOT NULL").map_err(|e| e.to_string())?;
    let rows = statement.query_map([], |row| {
        let context: String = row.get(3)?;
        let json: Value = serde_json::from_str(&context).unwrap_or(Value::Null);
        let usage = json.get("token_usage").unwrap_or(&Value::Null);
        let info = json.get("persist_user_message_context").and_then(|value| value.get("model_info")).unwrap_or(&Value::Null);
        let total = usage.get("total_tokens").and_then(Value::as_i64).unwrap_or(0);
        let session: String = row.get(1)?;
        let turn_id: Option<String> = row.get(0)?;
        Ok(UsageRecord {
            source: source.into(), record_id: turn_id.as_ref().map(|id| format!("trae_turn:{id}")), session_id: turn_id.unwrap_or(session.clone()), timestamp: row.get(2)?,
            project: project_map.get(&session).cloned().unwrap_or_else(|| "unknown".into()),
            model: info.get("config_name").and_then(Value::as_str).or(info.get("model_name").and_then(Value::as_str)).unwrap_or("unknown").into(),
            input_tokens: usage.get("prompt_tokens").and_then(Value::as_i64).unwrap_or(0),
            output_tokens: usage.get("completion_tokens").and_then(Value::as_i64).unwrap_or(0),
            reasoning_tokens: usage.get("reasoning_tokens").and_then(Value::as_i64).unwrap_or(0),
            cache_read_tokens: usage.get("cache_read_input_tokens").and_then(Value::as_i64).unwrap_or(0),
            cache_write_tokens: usage.get("cache_creation_input_tokens").and_then(Value::as_i64).unwrap_or(0),
            total_tokens: total, cost: 0.0,
        })
    }).map_err(|e| e.to_string())?;
    rows.filter_map(|row| match row { Ok(record) if record.total_tokens > 0 => Some(Ok(record)), Ok(_) => None, Err(error) => Some(Err(error)) })
        .collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
}

pub fn now() -> i64 { Utc::now().timestamp() }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn custom_session_roots_use_distinct_cache_names() {
        let default = Path::new("default-root");
        assert_eq!(scoped_cache_name("codex-rollouts", default, default), "codex-rollouts.json");
        let first = scoped_cache_name("codex-rollouts", Path::new("other-root-1"), default);
        let second = scoped_cache_name("codex-rollouts", Path::new("other-root-2"), default);
        assert_ne!(first, second);
        assert_ne!(first, "codex-rollouts.json");
    }

    #[test]
    fn trae_turns_expose_stable_record_ids() {
        let path = std::env::temp_dir().join(format!("tokenusage-trae-test-{}-{}.db", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch("CREATE TABLE chat_turn(turn_id TEXT, session_id TEXT, created_at INTEGER, context TEXT);").unwrap();
        let context = serde_json::json!({"token_usage":{"prompt_tokens":10,"completion_tokens":2,"total_tokens":12}}).to_string();
        conn.execute("INSERT INTO chat_turn VALUES(?1,?2,?3,?4)", rusqlite::params!["turn-1", "session-1", 1234, context]).unwrap();
        drop(conn);
        let records = collect_trae(&path, "trae").unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].record_id.as_deref(), Some("trae_turn:turn-1"));
        let _ = std::fs::remove_file(path);
    }
}

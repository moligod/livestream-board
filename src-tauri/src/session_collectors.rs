//! Local Claude and OpenCode usage readers, following CC Switch v4.0.25.
use crate::models::UsageRecord;
use chrono::{DateTime, Utc};
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::HashMap, fs::{self, File}, io::{BufRead, BufReader}, path::{Path, PathBuf}, time::UNIX_EPOCH};

#[derive(Clone, Serialize, Deserialize)]
struct ClaudeMessage { id: String, complete: bool, record: UsageRecord }

#[derive(Clone, Serialize, Deserialize)]
struct ClaudeFile { stamp: String, messages: Vec<ClaudeMessage>, #[serde(default)] retained: bool }

#[derive(Default, Serialize, Deserialize)]
struct ClaudeCache { version: u32, files: HashMap<String, ClaudeFile> }

fn project_name(path: &str) -> String {
    path.replace('\\', "/").trim_end_matches('/').rsplit('/').next().filter(|s| !s.is_empty()).unwrap_or("unknown").to_string()
}

fn stamp(path: &Path) -> Result<String, String> {
    let m = fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let ns = m.modified().map_err(|e| e.to_string())?.duration_since(UNIX_EPOCH).map_err(|e| e.to_string())?.as_nanos();
    Ok(format!("{}:{ns}", m.len()))
}

fn visit_jsonl(dir: &Path, files: &mut Vec<PathBuf>) -> Result<(), String> {
    for entry in fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))? {
        let entry = entry.map_err(|e| e.to_string())?;
        let ty = entry.file_type().map_err(|e| e.to_string())?;
        let path = entry.path();
        if ty.is_dir() { visit_jsonl(&path, files)?; }
        else if ty.is_file() && path.extension().and_then(|s| s.to_str()) == Some("jsonl") { files.push(path); }
    }
    Ok(())
}

fn number(value: Option<&Value>) -> i64 { value.and_then(Value::as_i64).unwrap_or(0).max(0) }
fn parse_date(value: Option<&Value>) -> Option<i64> {
    value.and_then(Value::as_str).and_then(|s| DateTime::parse_from_rfc3339(s).ok()).map(|t| t.with_timezone(&Utc).timestamp())
}

fn better(new: &ClaudeMessage, old: &ClaudeMessage) -> bool {
    (new.complete && !old.complete) || (new.complete == old.complete && new.record.output_tokens > old.record.output_tokens)
}

fn read_claude_file(path: &Path) -> Result<Vec<ClaudeMessage>, String> {
    let reader = BufReader::new(File::open(path).map_err(|e| format!("{}: {e}", path.display()))?);
    let mut messages = HashMap::<String, ClaudeMessage>::new();
    let mut session = String::new();
    for line in reader.lines() {
        let line = line.map_err(|e| format!("{}: {e}", path.display()))?;
        if !line.contains("\"assistant\"") || !line.contains("\"usage\"") { continue; }
        let Ok(value) = serde_json::from_str::<Value>(&line) else { continue };
        if value.get("type").and_then(Value::as_str) != Some("assistant") { continue; }
        if session.is_empty() { session = value.get("sessionId").and_then(Value::as_str).unwrap_or("").to_string(); }
        let Some(msg) = value.get("message") else { continue };
        let Some(id) = msg.get("id").and_then(Value::as_str).filter(|s| !s.is_empty()) else { continue };
        let Some(usage) = msg.get("usage") else { continue };
        let input = number(usage.get("input_tokens"));
        let output = number(usage.get("output_tokens"));
        let read = number(usage.get("cache_read_input_tokens"));
        let write = number(usage.get("cache_creation_input_tokens"));
        if input + output + read + write == 0 { continue; }
        let Some(timestamp) = parse_date(value.get("timestamp")) else { continue };
        let record = UsageRecord {
            source: "claude".into(), record_id: None, session_id: if session.is_empty() { path.file_stem().unwrap_or_default().to_string_lossy().into_owned() } else { session.clone() },
            timestamp, project: project_name(value.get("cwd").and_then(Value::as_str).unwrap_or("")),
            model: msg.get("model").and_then(Value::as_str).unwrap_or("unknown").into(),
            input_tokens: input, output_tokens: output, reasoning_tokens: 0, cache_read_tokens: read,
            cache_write_tokens: write, total_tokens: input + output + read + write, cost: 0.0,
        };
        let candidate = ClaudeMessage { id: id.into(), complete: msg.get("stop_reason").is_some_and(|v| !v.is_null()), record };
        if messages.get(id).is_none_or(|old| better(&candidate, old)) { messages.insert(id.into(), candidate); }
    }
    Ok(messages.into_values().collect())
}

#[allow(dead_code)]
pub fn collect_claude(root: &Path, cache_path: &Path) -> Result<(Vec<UsageRecord>, usize), String> {
    collect_claude_with_options(root, cache_path, false)
}

pub fn collect_claude_with_options(root: &Path, cache_path: &Path, changes_only: bool) -> Result<(Vec<UsageRecord>, usize), String> {
    let mut files = Vec::new();
    if root.exists() { visit_jsonl(root, &mut files)?; }
    files.sort();
    let mut cache = fs::read(cache_path).ok().and_then(|b| serde_json::from_slice::<ClaudeCache>(&b).ok()).filter(|c| c.version == 1).unwrap_or_default();
    let mut previous = HashMap::<String, ClaudeMessage>::new();
    if changes_only {
        for message in cache.files.values().flat_map(|entry| &entry.messages) {
            if previous.get(&message.id).is_none_or(|old| better(message, old)) {
                previous.insert(message.id.clone(), message.clone());
            }
        }
    }
    let mut next = HashMap::new();
    let mut unique = HashMap::<String, ClaudeMessage>::new();
    let mut parsed = 0;
    for path in files {
        let key = path.to_string_lossy().into_owned();
        let current = stamp(&path)?;
        let entry = match cache.files.remove(&key).filter(|e| !e.retained && e.stamp == current) {
            Some(entry) => entry,
            None => { parsed += 1; ClaudeFile { stamp: current, messages: read_claude_file(&path)?, retained: false } }
        };
        for message in &entry.messages {
            if unique.get(&message.id).is_none_or(|old| better(message, old)) { unique.insert(message.id.clone(), message.clone()); }
        }
        next.insert(key, entry);
    }
    let mut previous_files: Vec<_> = std::mem::take(&mut cache.files).into_iter().collect();
    previous_files.sort_by(|a, b| a.0.cmp(&b.0));
    let mut retained = 0;
    for (key, mut entry) in previous_files {
        if next.contains_key(&key) || Path::new(&key).exists() { continue; }
        entry.messages.retain(|message| !unique.contains_key(&message.id));
        if entry.messages.is_empty() { continue; }
        for message in &entry.messages { unique.insert(message.id.clone(), message.clone()); }
        entry.retained = true;
        retained += 1;
        next.insert(key, entry);
    }
    cache.version = 1; cache.files = next;
    if let Some(parent) = cache_path.parent() { fs::create_dir_all(parent).map_err(|e| e.to_string())?; }
    let temp = cache_path.with_extension(format!("{}.tmp", std::process::id()));
    fs::write(&temp, serde_json::to_vec(&cache).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    fs::rename(&temp, cache_path).map_err(|e| e.to_string())?;
    eprintln!("TokenUsage Claude: checked {}, parsed {}", cache.files.len(), parsed);
    let mut records: Vec<_> = unique.into_values().filter(|m| !changes_only || previous.get(&m.id).is_none_or(|old| old.record != m.record)).map(|m| {
        let mut record = m.record;
        record.record_id = Some(format!("claude_message:{}", m.id));
        record
    }).collect();
    records.sort_by(|a,b| (a.timestamp, &a.session_id, &a.model, a.total_tokens, a.output_tokens).cmp(&(b.timestamp, &b.session_id, &b.model, b.total_tokens, b.output_tokens)));
    Ok((records, retained))
}

fn has_table(conn: &Connection, name: &str) -> Result<bool, String> {
    conn.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)", [name], |r| r.get(0)).map_err(|e| e.to_string())
}

pub fn collect_opencode(path: &Path) -> Result<Vec<UsageRecord>, String> {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(|e| e.to_string())?;
    let has_v2 = has_table(&conn, "session_v2")? && has_table(&conn, "session_message")?;
    let has_v1 = has_table(&conn, "message")?;
    if !has_v1 && !has_v2 { return Err("OpenCode message tables not found".into()); }
    let mut records = Vec::new();
    let mut seen = std::collections::HashSet::new();
    // Both generations may still receive messages; prefer the V2 entry when IDs collide.
    for (v2, present) in [(true, has_v2), (false, has_v1)] {
        if !present { continue; }
        let sql = if v2 { "SELECT id, session_id, data, type, time_created FROM session_message WHERE type IN ('assistant','compaction') ORDER BY time_created" }
                  else { "SELECT id, session_id, data, NULL, time_created FROM message ORDER BY time_created" };
        // Directory is optional in V2 and absent in minimal/older layouts.
        let table = if v2 { "session_v2" } else { "session" };
        let mut projects = HashMap::new();
        if let Ok(mut stmt) = conn.prepare(&format!("SELECT id, directory FROM {table}")) {
            let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))).map_err(|e| e.to_string())?;
            for row in rows { let (id, path) = row.map_err(|e| e.to_string())?; projects.insert(id, project_name(path.as_deref().unwrap_or(""))); }
        }
        let mut stmt = conn.prepare(sql).map_err(|e| e.to_string())?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, Option<String>>(3)?, r.get::<_, Option<i64>>(4)?))).map_err(|e| e.to_string())?;
        for row in rows {
            let (id, session, raw, kind, db_created) = row.map_err(|e| e.to_string())?;
            let Ok(v) = serde_json::from_str::<Value>(&raw) else { continue };
            if !v2 && v.get("role").and_then(Value::as_str) != Some("assistant") { continue; }
            let complete = if kind.as_deref() == Some("compaction") { matches!(v.get("status").and_then(Value::as_str), Some("completed" | "failed")) }
                else { v.pointer("/time/completed").is_some_and(|v| !v.is_null()) };
            if !complete { continue; }
            let input = number(v.pointer("/tokens/input")); let output = number(v.pointer("/tokens/output"));
            let reasoning = number(v.pointer("/tokens/reasoning")); let read = number(v.pointer("/tokens/cache/read")); let write = number(v.pointer("/tokens/cache/write"));
            if input + output + read + write + reasoning == 0 { continue; }
            let model = v.get("modelID").and_then(Value::as_str).or_else(|| v.get("model").and_then(Value::as_str)).or_else(|| v.pointer("/model/id").and_then(Value::as_str)).unwrap_or("unknown");
            let timestamp = v.pointer("/time/created").and_then(Value::as_i64).filter(|v| *v > 0).or(db_created).unwrap_or(0) / 1000;
            if !seen.insert(id.clone()) { continue; }
            records.push(UsageRecord { source: "opencode".into(), record_id: Some(format!("opencode_message:{id}")), session_id: session.clone(), timestamp,
                project: projects.get(&session).cloned().unwrap_or_else(|| "unknown".into()), model: model.into(),
                input_tokens: input, output_tokens: output, reasoning_tokens: reasoning, cache_read_tokens: read,
                cache_write_tokens: write, total_tokens: input + output + read + write,
                cost: v.get("cost").and_then(Value::as_f64).unwrap_or(0.0), });
        }
    }
    records.sort_by_key(|record| record.timestamp);
    Ok(records)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::SystemTime;

    fn temp() -> PathBuf {
        let id = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let p = std::env::temp_dir().join(format!("tokenusage-sessions-{}-{id}", std::process::id()));
        fs::create_dir_all(&p).unwrap(); p
    }

    #[test]
    fn claude_deduplicates_message_snapshots_and_rescans_appends() {
        let root = temp(); let projects = root.join("projects/demo/subagents"); fs::create_dir_all(&projects).unwrap();
        let path = projects.join("agent.jsonl"); let cache = root.join("cache.json");
        let entry = |output: i64, stop: bool| serde_json::json!({"type":"assistant","timestamp":"2026-01-01T00:00:00Z","sessionId":"s1","cwd":"C:/demo","message":{"id":"m1","model":"claude-test","stop_reason":if stop {Some("end_turn")} else {None},"usage":{"input_tokens":10,"output_tokens":output,"cache_read_input_tokens":80,"cache_creation_input_tokens":5}}}).to_string();
        fs::write(&path, format!("{}\n", entry(1, false))).unwrap();
        assert_eq!(collect_claude(&root.join("projects"), &cache).unwrap().0[0].total_tokens, 96);
        assert!(collect_claude_with_options(&root.join("projects"), &cache, true).unwrap().0.is_empty());
        fs::write(&path, format!("{}\n{}\n", entry(1, false), entry(7, true))).unwrap();
        let changed = collect_claude_with_options(&root.join("projects"), &cache, true).unwrap().0;
        assert_eq!(changed.len(), 1);
        assert_eq!(changed[0].output_tokens, 7);
        assert!(changed[0].record_id.is_some());
        let records = collect_claude(&root.join("projects"), &cache).unwrap().0;
        assert_eq!(records.len(), 1); assert_eq!(records[0].total_tokens, 102);
        assert_eq!(collect_claude(&root.join("projects"), &cache).unwrap().0[0].output_tokens, 7);
        fs::remove_file(&path).unwrap();
        let (retained, files) = collect_claude(&root.join("projects"), &cache).unwrap();
        assert_eq!(files, 1); assert_eq!(retained[0].output_tokens, 7);
        fs::write(&path, format!("{}\n{}\n", entry(1, false), entry(7, true))).unwrap();
        let (restored, files) = collect_claude(&root.join("projects"), &cache).unwrap();
        assert_eq!(files, 0); assert_eq!(restored.len(), 1);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn opencode_v2_reads_completed_messages_and_compaction_individually() {
        let root = temp(); let db = root.join("opencode.db"); let conn = Connection::open(&db).unwrap();
        conn.execute_batch("CREATE TABLE session_v2(id TEXT, directory TEXT); CREATE TABLE session_message(id TEXT, session_id TEXT, type TEXT, time_created INTEGER, data TEXT); INSERT INTO session_v2 VALUES('s1', 'C:/demo');").unwrap();
        let m1 = serde_json::json!({"model":{"id":"model-a"},"time":{"created":1000,"completed":1200},"tokens":{"input":10,"output":2,"cache":{"read":30,"write":5}}});
        let m2 = serde_json::json!({"model":{"id":"model-b"},"time":{"created":2000},"status":"completed","tokens":{"input":4,"output":3}});
        let m3 = serde_json::json!({"model":{"id":"model-c"},"status":"completed","tokens":{"input":1}});
        let unfinished = serde_json::json!({"time":{"created":3000},"tokens":{"input":999}});
        for (id, kind, item) in [("a", "assistant", m1), ("b", "compaction", m2), ("c", "assistant", unfinished), ("d", "compaction", m3)] {
            conn.execute("INSERT INTO session_message VALUES(?1,'s1',?2,4000,?3)", rusqlite::params![id, kind, item.to_string()]).unwrap();
        }
        drop(conn);
        let records = collect_opencode(&db).unwrap();
        assert_eq!(records.len(), 3); assert_eq!(records[0].total_tokens, 47);
        assert_eq!(records[0].record_id.as_deref(), Some("opencode_message:a"));
        assert_eq!(records[1].record_id.as_deref(), Some("opencode_message:b"));
        assert_eq!(records[1].model, "model-b"); assert_eq!(records[1].timestamp, 2);
        assert_eq!(records[2].model, "model-c"); assert_eq!(records[2].timestamp, 4);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn opencode_v1_uses_each_completed_assistant_message_not_session_totals() {
        let root = temp(); let db = root.join("opencode.db"); let conn = Connection::open(&db).unwrap();
        conn.execute_batch("CREATE TABLE session(id TEXT, directory TEXT); CREATE TABLE message(id TEXT, session_id TEXT, time_created INTEGER, data TEXT); INSERT INTO session VALUES('s1', 'C:/demo');").unwrap();
        for (id, timestamp, model, input) in [("a", 1000, "model-a", 10), ("b", 90000000, "model-b", 20)] {
            let item = serde_json::json!({"role":"assistant","modelID":model,"time":{"created":timestamp,"completed":timestamp+1},"tokens":{"input":input,"output":2,"cache":{"read":5}}});
            conn.execute("INSERT INTO message VALUES(?1,'s1',?2,?3)", rusqlite::params![id, timestamp, item.to_string()]).unwrap();
        }
        drop(conn);
        let records = collect_opencode(&db).unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].record_id.as_deref(), Some("opencode_message:a"));
        assert_eq!(records[1].record_id.as_deref(), Some("opencode_message:b"));
        assert_eq!(records[0].model, "model-a"); assert_eq!(records[1].model, "model-b");
        assert_eq!(records[0].timestamp, 1); assert_eq!(records[1].timestamp, 90000);
        assert_eq!(records[0].total_tokens, 17); assert_eq!(records[1].total_tokens, 27);
        let _ = fs::remove_dir_all(root);
    }
}

//! Codex rollout usage rebuild and incremental scanner.
//!
//! Adapted from CC Switch's `services/session_usage_codex.rs` at commit
//! d8065cc628fcd373d00c4363d718095f19e78c9e. CC Switch is MIT licensed;
//! see `THIRD_PARTY_NOTICES.md` at the project root.

use crate::models::UsageRecord;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet},
    fs::{self, File},
    io::{BufRead, BufReader, Read},
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

const CACHE_VERSION: u32 = 5;
const REQUEST_ID_PREFIX: &str = "codex_session:thread-v1";

#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
struct TokenCountersSignature {
    input: Option<u64>,
    cached_input: Option<u64>,
    output: Option<u64>,
    reasoning_output: Option<u64>,
    total: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct TokenUsageSignature {
    total: Option<TokenCountersSignature>,
    last: Option<TokenCountersSignature>,
}

#[derive(Debug, Clone, Default)]
struct CumulativeTokens {
    input: u64,
    cached_input: u64,
    cache_write_input: u64,
    output: u64,
}

#[derive(Debug, Clone, Default)]
struct DeltaTokens {
    input: u64,
    cached_input: u64,
    cache_write_input: u64,
    output: u64,
}

impl DeltaTokens {
    fn is_zero(&self) -> bool {
        self.input == 0 && self.cached_input == 0 && self.output == 0
    }
}

#[derive(Debug)]
struct ParsedTokenEvent {
    signature: TokenUsageSignature,
    delta: DeltaTokens,
    model: String,
    timestamp: i64,
    timestamp_valid: bool,
    event_index: Option<u32>,
}

#[derive(Debug)]
enum ParentResolution {
    None,
    Parent(String),
    Deferred(String),
}

#[derive(Debug)]
struct ParsedCodexFile {
    root_thread_id: Option<String>,
    meta_thread_id: Option<String>,
    root_meta_seen: bool,
    root_timestamp: Option<DateTime<Utc>>,
    project: String,
    parent: ParentResolution,
    bounded_history: Option<Value>,
    continuation_dependency: Option<String>,
    token_events: Vec<ParsedTokenEvent>,
    usage_record_events: Vec<(String, DeltaTokens, String, i64)>,
    has_billable_tokens: bool,
}

#[derive(Debug)]
struct TimestampedTokenSignature {
    timestamp: DateTime<Utc>,
    signature: TokenUsageSignature,
}

#[derive(Debug)]
struct ParentTimeline {
    events: Vec<TimestampedTokenSignature>,
    max_timestamp: Option<DateTime<Utc>>,
    has_token_without_timestamp: bool,
    high_water: Option<CumulativeTokens>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CacheEntry {
    own_signature: String,
    parent_id: Option<String>,
    parent_signatures: Vec<(String, String)>,
    request_ids: Vec<String>,
    records: Vec<UsageRecord>,
    deferred: bool,
    #[serde(default)]
    deferred_reason: Option<String>,
    #[serde(default)]
    retained: bool,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct CodexCache {
    version: u32,
    entries: HashMap<String, CacheEntry>,
}

#[derive(Debug, Default)]
pub struct ScanResult {
    pub records: Vec<UsageRecord>,
    pub files_scanned: usize,
    pub files_parsed: usize,
    pub deferred_files: usize,
    pub deferred_reasons: HashMap<String, usize>,
    pub retained_files: usize,
}

type RolloutIndex = HashMap<String, Vec<PathBuf>>;

#[allow(dead_code)]
pub fn scan(codex_dir: &Path, cache_path: &Path, rebuild: bool) -> Result<ScanResult, String> {
    scan_with_options(codex_dir, cache_path, rebuild, false)
}

pub fn scan_with_options(codex_dir: &Path, cache_path: &Path, rebuild: bool, changes_only: bool) -> Result<ScanResult, String> {
    let files = collect_codex_session_files(codex_dir)?;
    let index = build_rollout_index(&files);
    // Even a rebuild preserves records whose original files no longer exist:
    // like CC Switch's historical database, these cannot be reconstructed.
    let mut cache = load_cache(cache_path);
    cache.version = CACHE_VERSION;
    // Removing or archiving a file can change which duplicate request is
    // preferred. Replay the remaining source once so the ledger can update
    // that request without deleting historical rows for the vanished file.
    let live_paths: HashSet<_> = files.iter().map(|path| path.to_string_lossy().into_owned()).collect();
    let removed_file = cache.entries.iter().any(|(key, entry)| !entry.retained && !entry.records.is_empty() && !live_paths.contains(key));
    let changes_only = changes_only && !removed_file;
    let mut active = HashMap::new();
    let mut parent_timelines = HashMap::<PathBuf, ParentTimeline>::new();
    let mut result = ScanResult { files_scanned: files.len(), ..ScanResult::default() };
    let mut seen_request_ids = HashSet::new();

    for file_path in files {
        let key = file_path.to_string_lossy().into_owned();
        let own_signature = file_signature(&file_path).unwrap_or_else(|| "missing".into());
        if !rebuild {
            if let Some(entry) = cache.entries.get(&key).filter(|entry| {
                cache_entry_is_current(entry, &own_signature, &index)
            }) {
                if entry.deferred {
                    result.deferred_files += 1;
                    *result.deferred_reasons.entry(entry.deferred_reason.clone().unwrap_or_else(|| "未知原因".into())).or_default() += 1;
                }
                for (request_id, record) in entry.request_ids.iter().zip(&entry.records) {
                    if seen_request_ids.insert(request_id.clone()) && !changes_only {
                        let mut record = record.clone();
                        record.record_id = Some(request_id.clone());
                        result.records.push(record);
                    }
                }
                active.insert(key, entry.clone());
                continue;
            }
        }

        result.files_parsed += 1;
        let parsed = parse_codex_file(&file_path, thread_id_from_filename(&file_path), &index, &mut parent_timelines)?;
        let parent_id = match &parsed.parent {
            ParentResolution::Parent(id) => Some(id.clone()),
            _ => parsed.continuation_dependency.clone(),
        };
        let parent_signatures = parent_id.as_deref().map(|id| parent_stamps(id, &index)).unwrap_or_default();

        if !parsed.has_billable_tokens {
            active.insert(key, CacheEntry { own_signature, parent_id, parent_signatures, request_ids: Vec::new(), records: Vec::new(), deferred: false, deferred_reason: None, retained: false });
            continue;
        }
        let Some(root_thread_id) = parsed.root_thread_id.as_deref() else {
            result.deferred_files += 1;
            *result.deferred_reasons.entry("缺少线程 ID".into()).or_default() += 1;
            active.insert(key, CacheEntry { own_signature, parent_id, parent_signatures, request_ids: Vec::new(), records: Vec::new(), deferred: true, deferred_reason: Some("缺少线程 ID".into()), retained: false });
            continue;
        };
        let session_thread_id = parsed.meta_thread_id.as_deref().unwrap_or(root_thread_id);
        if !parsed.root_meta_seen {
            result.deferred_files += 1;
            *result.deferred_reasons.entry("缺少会话元数据".into()).or_default() += 1;
            active.insert(key, CacheEntry { own_signature, parent_id, parent_signatures, request_ids: Vec::new(), records: Vec::new(), deferred: true, deferred_reason: Some("缺少会话元数据".into()), retained: false });
            continue;
        }

        let replay_prefix = match &parsed.parent {
            ParentResolution::None => 0,
            ParentResolution::Deferred(reason) => {
                eprintln!("TokenUsage Codex deferred {}: {reason}", file_path.display());
                result.deferred_files += 1;
                *result.deferred_reasons.entry("会话元数据不一致".into()).or_default() += 1;
                active.insert(key, CacheEntry { own_signature, parent_id, parent_signatures, request_ids: Vec::new(), records: Vec::new(), deferred: true, deferred_reason: Some("会话元数据不一致".into()), retained: false });
                continue;
            }
            ParentResolution::Parent(parent_thread_id) => {
                let Some(cutoff) = parsed.root_timestamp else {
                    result.deferred_files += 1;
                    *result.deferred_reasons.entry("缺少分支时间".into()).or_default() += 1;
                    active.insert(key, CacheEntry { own_signature, parent_id, parent_signatures, request_ids: Vec::new(), records: Vec::new(), deferred: true, deferred_reason: Some("缺少分支时间".into()), retained: false });
                    continue;
                };
                if verified_bounded_fork(&parsed, &index) { 0 } else {
                let parent = match resolve_parent_signatures(parent_thread_id, cutoff, &index, &mut parent_timelines) {
                    Ok(parent) => parent,
                    Err(reason) => {
                        result.deferred_files += 1;
                        let kind = if reason.contains("找不到父") { "找不到父会话" } else if reason.contains("尚未写到") { "父会话时间不足" } else if reason.contains("不一致") { "父会话副本不一致" } else { "父会话不可验证" };
                        *result.deferred_reasons.entry(kind.into()).or_default() += 1;
                        active.insert(key, CacheEntry { own_signature, parent_id, parent_signatures, request_ids: Vec::new(), records: Vec::new(), deferred: true, deferred_reason: Some(kind.into()), retained: false });
                        continue;
                    }
                };
                matching_replay_prefix(&parsed.token_events, &parent)
                }
            }
        };

        let mut pairs = parsed.token_events.into_iter().skip(replay_prefix).filter_map(|event| {
            let event_index = event.event_index?;
            let input = event.delta.input as i64;
            let cached = event.delta.cached_input.min(event.delta.input) as i64;
            let write = event.delta.cache_write_input.min(event.delta.input - cached as u64) as i64;
            let output = event.delta.output as i64;
            let record = UsageRecord {
                source: "codex".into(),
                record_id: None,
                session_id: session_thread_id.to_string(),
                timestamp: event.timestamp,
                project: parsed.project.clone(),
                model: event.model,
                input_tokens: (input - cached - write).max(0),
                output_tokens: output,
                reasoning_tokens: 0,
                cache_read_tokens: cached,
                cache_write_tokens: write,
                total_tokens: input + output,
                cost: 0.0,
            };
            Some((format!("{REQUEST_ID_PREFIX}:{root_thread_id}:{event_index}"), record))
        }).collect::<Vec<_>>();
        for (key, delta, model, timestamp) in parsed.usage_record_events {
            let input = delta.input as i64;
            let read = delta.cached_input.min(delta.input) as i64;
            let write = delta.cache_write_input.min(delta.input - read as u64) as i64;
            let record = UsageRecord { source: "codex".into(), record_id: None, session_id: session_thread_id.to_string(), timestamp,
                project: parsed.project.clone(), model, input_tokens: input - read - write,
                output_tokens: delta.output as i64, reasoning_tokens: 0, cache_read_tokens: read,
                cache_write_tokens: write, total_tokens: input + delta.output as i64, cost: 0.0 };
            pairs.push((format!("{REQUEST_ID_PREFIX}:{root_thread_id}:usage:{key}"), record));
        }
        let previous_by_id: HashMap<_, _> = cache.entries.get(&key).into_iter()
            .flat_map(|entry| entry.request_ids.iter().zip(&entry.records))
            .map(|(id, record)| (id.as_str(), record)).collect();
        let (request_ids, records): (Vec<_>, Vec<_>) = pairs.into_iter().unzip();
        for (request_id, record) in request_ids.iter().zip(&records) {
            let differs = previous_by_id.get(request_id.as_str()).is_none_or(|previous| *previous != record);
            if seen_request_ids.insert(request_id.clone()) && (!changes_only || differs) {
                let mut record = record.clone();
                record.record_id = Some(request_id.clone());
                result.records.push(record);
            }
        }
        active.insert(key, CacheEntry { own_signature, parent_id, parent_signatures, request_ids, records, deferred: false, deferred_reason: None, retained: false });
    }

    // Prefer currently readable files, then retain previously imported records
    // from vanished files by request ID. This also handles archive moves without
    // double counting and does not keep deferred/empty entries indefinitely.
    let mut previous: Vec<_> = std::mem::take(&mut cache.entries).into_iter().collect();
    previous.sort_by(|a, b| a.0.cmp(&b.0));
    for (key, mut entry) in previous {
        let retained_key = match active.get(&key) {
            // A previously accepted child may become unverifiable after its
            // parent is removed. Keep only its already imported records.
            Some(current) if current.deferred && !entry.records.is_empty() => format!("{key}::retained"),
            Some(_) => continue,
            None if Path::new(&key).exists() => continue,
            None => key,
        };
        let (ids, records): (Vec<_>, Vec<_>) = entry.request_ids.into_iter().zip(entry.records.into_iter())
            .filter(|(id, _)| seen_request_ids.insert(id.clone())).unzip();
        if records.is_empty() { continue; }
        result.retained_files += 1;
        if !changes_only {
            result.records.extend(ids.iter().zip(&records).map(|(request_id, record)| {
                let mut record = record.clone();
                record.record_id = Some(request_id.clone());
                record
            }));
        }
        entry.request_ids = ids;
        entry.records = records;
        entry.deferred = false;
        entry.deferred_reason = None;
        entry.retained = true;
        active.insert(retained_key, entry);
    }
    cache.entries = active;
    save_cache(cache_path, &cache)?;
    Ok(result)
}

fn cache_entry_is_current(entry: &CacheEntry, own_signature: &str, index: &RolloutIndex) -> bool {
    if entry.retained || entry.own_signature != own_signature {
        return false;
    }
    let current = entry.parent_id.as_deref().map(|id| parent_stamps(id, index)).unwrap_or_default();
    current == entry.parent_signatures
}

fn parent_stamps(parent_id: &str, index: &RolloutIndex) -> Vec<(String, String)> {
    index.get(parent_id).into_iter().flatten().map(|path| {
        (path.to_string_lossy().into_owned(), file_signature(path).unwrap_or_else(|| "missing".into()))
    }).collect()
}

fn load_cache(path: &Path) -> CodexCache {
    fs::read(path).ok().and_then(|bytes| serde_json::from_slice::<CodexCache>(&bytes).ok())
        .filter(|cache| cache.version == CACHE_VERSION).unwrap_or_default()
}

fn save_cache(path: &Path, cache: &CodexCache) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let temp = path.with_extension(format!("{}.tmp", std::process::id()));
    fs::write(&temp, serde_json::to_vec(cache).map_err(|error| error.to_string())?).map_err(|error| error.to_string())?;
    fs::rename(&temp, path).map_err(|error| error.to_string())
}

fn collect_codex_session_files(codex_dir: &Path) -> Result<Vec<PathBuf>, String> {
    let mut files = Vec::new();
    let sessions = codex_dir.join("sessions");
    if sessions.is_dir() {
        collect_jsonl_recursive(&sessions, &mut files, 0, 3)?;
    }
    let archived = codex_dir.join("archived_sessions");
    if archived.is_dir() {
        let entries = fs::read_dir(&archived).map_err(|error| format!("{}: {error}", archived.display()))?;
        for entry in entries { let path = entry.map_err(|error| error.to_string())?.path(); if is_rollout_file(&path) { files.push(path); } }
    }
    files.sort();
    let plain: HashSet<PathBuf> = files.iter().filter(|path| !is_compressed(path)).cloned().collect();
    Ok(files.into_iter().filter(|path| !is_compressed(path) || !plain.contains(&logical_path(path))).collect())
}

fn is_compressed(path: &Path) -> bool {
    path.file_name().and_then(|s| s.to_str()).is_some_and(|s| s.ends_with(".jsonl.zst"))
}

fn is_rollout_file(path: &Path) -> bool {
    path.extension().and_then(|s| s.to_str()) == Some("jsonl") || is_compressed(path)
}

fn logical_path(path: &Path) -> PathBuf {
    if is_compressed(path) { path.with_extension("") } else { path.to_path_buf() }
}

fn rollout_reader(path: &Path) -> Result<Box<dyn BufRead>, String> {
    let file = File::open(path).map_err(|error| format!("{}: {error}", path.display()))?;
    if is_compressed(path) {
        let decoder = zstd::stream::read::Decoder::new(file).map_err(|error| error.to_string())?;
        Ok(Box::new(BufReader::with_capacity(1 << 20, decoder)))
    } else {
        Ok(Box::new(BufReader::with_capacity(1 << 20, file)))
    }
}

fn collect_jsonl_recursive(dir: &Path, files: &mut Vec<PathBuf>, depth: u32, max_depth: u32) -> Result<(), String> {
    let entries = fs::read_dir(dir).map_err(|error| format!("{}: {error}", dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|error| error.to_string())?;
        let path = entry.path();
        let kind = entry.file_type().map_err(|error| error.to_string())?;
        if kind.is_dir() && depth < max_depth {
            collect_jsonl_recursive(&path, files, depth + 1, max_depth)?;
        } else if kind.is_file() && is_rollout_file(&path) {
            files.push(path);
        }
    }
    Ok(())
}

fn build_rollout_index(files: &[PathBuf]) -> RolloutIndex {
    let mut index = RolloutIndex::new();
    for path in files {
        if let Some(thread_id) = thread_id_from_filename(path) {
            index.entry(thread_id).or_default().push(path.clone());
        }
        if let Some(leading) = leading_thread_id_from_filename(path) {
            if Some(leading.clone()) != thread_id_from_filename(path) {
                index.entry(leading).or_default().push(path.clone());
            }
        }
    }
    for paths in index.values_mut() { paths.sort(); }
    index
}

fn thread_id_from_filename(path: &Path) -> Option<String> {
    let logical = logical_path(path);
    let stem = logical.file_stem()?.to_str()?;
    let candidate = stem.get(stem.len().checked_sub(36)?..)?;
    uuid::Uuid::parse_str(candidate).ok().map(|value| value.hyphenated().to_string())
}

fn leading_thread_id_from_filename(path: &Path) -> Option<String> {
    let logical = logical_path(path);
    let stem = logical.file_stem()?.to_str()?;
    let (prefix, _) = stem.rsplit_once('_')?;
    let candidate = prefix.get(prefix.len().checked_sub(36)?..)?;
    uuid::Uuid::parse_str(candidate).ok().map(|value| value.hyphenated().to_string())
}

fn parse_codex_file(path: &Path, root_thread_id: Option<String>, index: &RolloutIndex, timelines: &mut HashMap<PathBuf, ParentTimeline>) -> Result<ParsedCodexFile, String> {
    let reader = rollout_reader(path)?;
    let mut root_meta_seen = false;
    let mut meta_thread_id = None;
    let mut root_timestamp = None;
    let mut project = "unknown".to_string();
    let mut parent = ParentResolution::None;
    let mut bounded_history = None;
    let mut continuation_dependency = None;
    let mut is_continuation = false;
    let mut history_base_id = None;
    let mut current_model = "unknown".to_string();
    let mut total_high_water = None;
    let mut last_signature_by_source = HashMap::<Option<String>, TokenUsageSignature>::new();
    let mut previous_token_signature = None;
    let mut events = Vec::new();
    let mut extra = Vec::new();
    let mut pending: Option<(String, CumulativeTokens, String, i64)> = None;
    let mut has_billable_tokens = false;
    let mut event_index = 0u32;

    for (line_index, line) in reader.lines().enumerate() {
        let line = line.map_err(|error| format!("{}: {error}", path.display()))?;
        if !(line.contains("\"event_msg\"") || line.contains("\"turn_context\"") || line.contains("\"session_meta\"") || line.contains("\"token_usage_record\"")) { continue; }
        if line.contains("\"event_msg\"") && !line.contains("\"token_count\"") && !line.contains("\"thread_settings_applied\"") { continue; }
        let Ok(value) = serde_json::from_str::<Value>(&line) else { continue };
        match value.get("type").and_then(Value::as_str) {
            Some("session_meta") if !root_meta_seen => {
                root_meta_seen = true;
                root_timestamp = parse_timestamp(value.get("timestamp"));
                let payload = value.get("payload").unwrap_or(&Value::Null);
                parent = explicit_parent_from_meta(payload);
                bounded_history = payload.get("history_base").cloned();
                project = project_name(payload.get("cwd").and_then(Value::as_str));
                let meta_id = non_empty_string(payload.get("id").or_else(|| payload.get("thread_id")).or_else(|| payload.get("threadId")))
                    .map(|id| uuid::Uuid::parse_str(&id).map(|u| u.hyphenated().to_string()).unwrap_or(id));
                meta_thread_id = meta_id.clone();
                let leading = leading_thread_id_from_filename(path);
                if let (Some(filename), Some(meta)) = (&root_thread_id, meta_id.as_ref()) {
                    if filename != meta && leading.as_ref() != Some(meta) { parent = ParentResolution::Deferred(format!("文件名线程 ID ({filename}) 与 root meta ID ({meta}) 不一致")); }
                }
                let history_base = payload.pointer("/history_base/thread_id").and_then(Value::as_str);
                history_base_id = history_base.map(str::to_owned);
                let continuation = (leading.as_ref().is_some() && leading.as_ref() == meta_id.as_ref()
                    && (payload.get("history_mode").and_then(Value::as_str) == Some("paginated") || history_base.is_some() || matches!(parent, ParentResolution::Parent(_))))
                    || (history_base.is_some() && history_base == meta_id.as_deref());
                if continuation && !matches!(parent, ParentResolution::Deferred(_)) {
                    parent = ParentResolution::None;
                    is_continuation = true;
                    continuation_dependency = meta_id.clone();
                }
                if let ParentResolution::Parent(parent_id) = &mut parent {
                    match uuid::Uuid::parse_str(parent_id) {
                        Ok(value) => *parent_id = value.hyphenated().to_string(),
                        Err(_) => parent = ParentResolution::Deferred(format!("显式 parent_thread_id 不是有效 UUID: {parent_id}")),
                    }
                }
                if matches!((&root_thread_id, &parent), (Some(root), ParentResolution::Parent(parent_id)) if root == parent_id) {
                    parent = ParentResolution::Deferred("parent_thread_id 与 root_thread_id 相同".into());
                }
            }
            Some("turn_context") => {
                if let Some(model) = value.pointer("/payload/model").or_else(|| value.pointer("/payload/info/model")).and_then(Value::as_str) {
                    current_model = normalize_codex_model(model);
                }
            }
            Some("event_msg") => {
                let payload = value.get("payload").unwrap_or(&Value::Null);
                if payload.get("type").and_then(Value::as_str) == Some("thread_settings_applied") {
                    if let Some(model) = payload.pointer("/thread_settings/model").and_then(Value::as_str).filter(|v| !v.is_empty()) { current_model = normalize_codex_model(model); }
                    continue;
                }
                if payload.get("type").and_then(Value::as_str) != Some("token_count") { continue; }
                let Some(info) = payload.get("info").filter(|info| !info.is_null()) else { continue };
                let Some(signature) = parse_token_signature(info) else { continue };
                if let Some(model) = info.get("model").or_else(|| info.get("model_name")).or_else(|| payload.get("model")).and_then(Value::as_str) {
                    current_model = normalize_codex_model(model);
                }
                let source = token_snapshot_source(payload);
                let total = info.get("total_token_usage").and_then(parse_cumulative_tokens);
                let last = info.get("last_token_usage").and_then(parse_cumulative_tokens);
                if let Some((key, usage, model, timestamp)) = pending.take() {
                    if last.as_ref().is_some_and(|last| last.input == 0 && last.cached_input == 0 && last.cache_write_input == 0 && last.output == 0) {
                        has_billable_tokens = true;
                        extra.push((key, DeltaTokens { input: usage.input, cached_input: usage.cached_input, cache_write_input: usage.cache_write_input, output: usage.output }, model, timestamp));
                    }
                }
                if total.is_none() && last.is_none() { continue; }
                let duplicate = total.is_some() && (last_signature_by_source.get(&source) == Some(&signature) || previous_token_signature.as_ref() == Some(&signature));
                if total.is_some() { last_signature_by_source.insert(source, signature.clone()); }
                previous_token_signature = Some(signature.clone());
                let delta = if duplicate {
                    DeltaTokens::default()
                } else if let Some(last) = last {
                    DeltaTokens { input: last.input, cached_input: last.cached_input, cache_write_input: last.cache_write_input, output: last.output }
                } else if let Some(total) = total.as_ref() {
                    if is_continuation && total_high_water.is_none() {
                        match continuation_baseline(path, meta_thread_id.as_deref().unwrap_or(""), history_base_id.as_deref(), index, timelines) {
                            Ok(Some(baseline)) if total.input >= baseline.input => total_high_water = Some(baseline),
                            Ok(_) => {},
                            Err(reason) => { parent = ParentResolution::Deferred(reason); has_billable_tokens = true; break; }
                        }
                    }
                    compute_delta(&total_high_water, total)
                } else { continue };
                if let Some(total) = total {
                    if let Some(high_water) = total_high_water.as_mut() { update_high_water(high_water, &total); } else { total_high_water = Some(total); }
                }
                let delta = DeltaTokens { cached_input: delta.cached_input.min(delta.input), ..delta };
                has_billable_tokens |= !delta.is_zero();
                let nonzero_index = if delta.is_zero() { None } else { event_index = event_index.saturating_add(1); Some(event_index) };
                let event_time = parse_timestamp(value.get("timestamp"));
                events.push(ParsedTokenEvent {
                    signature, delta, model: current_model.clone(),
                    timestamp: event_time.map(|time| time.timestamp()).unwrap_or_else(|| Utc::now().timestamp()),
                    timestamp_valid: event_time.is_some(),
                    event_index: nonzero_index,
                });
            }
            Some("token_usage_record") => {
                pending = None;
                if let Some(usage) = value.pointer("/payload/usage").and_then(parse_cumulative_tokens) {
                    if usage.input + usage.output > 0 {
                        let key = value.pointer("/payload/response_id").and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_owned).unwrap_or_else(|| format!("line{}", line_index + 1));
                        let timestamp = parse_timestamp(value.get("timestamp")).map(|t| t.timestamp()).unwrap_or_else(|| Utc::now().timestamp());
                        pending = Some((key, usage, current_model.clone(), timestamp));
                    }
                }
            }
            _ => {}
        }
    }
    Ok(ParsedCodexFile { root_thread_id, meta_thread_id, root_meta_seen, root_timestamp, project, parent, bounded_history, continuation_dependency, token_events: events, usage_record_events: extra, has_billable_tokens })
}

fn continuation_baseline(path: &Path, thread: &str, history_base: Option<&str>, index: &RolloutIndex, timelines: &mut HashMap<PathBuf, ParentTimeline>) -> Result<Option<CumulativeTokens>, String> {
    for id in [Some(thread), history_base].into_iter().flatten() {
        if let Some(candidates) = index.get(id) {
            let current_name = logical_path(path).file_name().map(|s| s.to_owned());
            let mut previous: Vec<_> = candidates.iter().filter(|p| logical_path(p).file_name().map(|s| s.to_owned()) < current_name).collect();
            previous.sort_by(|a, b| logical_path(b).file_name().cmp(&logical_path(a).file_name()));
            for prev in previous {
                if !timelines.contains_key(prev) { timelines.insert(prev.clone(), load_parent_timeline(prev)?); }
                if let Some(high) = timelines[prev].high_water.clone() { return Ok(Some(high)); }
            }
        }
    }
    if history_base.is_some() { Err("续写页找不到前序累计用量基线".into()) } else { Ok(None) }
}

// Bounded-reference forks contain only their own requests. As in CC Switch,
// accept them without replay removal only after verifying the referenced
// parent's physical prefix, ordinals and inherited token baseline.
fn verified_bounded_fork(parsed: &ParsedCodexFile, index: &RolloutIndex) -> bool {
    (|| -> Option<()> {
        let ParentResolution::Parent(parent_id) = &parsed.parent else { return None };
        let history = parsed.bounded_history.as_ref()?;
        if history.get("thread_id")?.as_str()? != parent_id { return None; }
        let end = history.get("end_byte_offset")?.as_u64()?;
        let count = history.get("end_ordinal_exclusive")?.as_u64()?;
        if end == 0 || end > 64 * 1024 * 1024 || count == 0 { return None; }
        let cutoff = parsed.root_timestamp?.timestamp();
        let first = parsed.token_events.first()?;
        if !parsed.usage_record_events.is_empty() { return None; }
        for event in &parsed.token_events {
            let last = event.signature.last.as_ref()?;
            if last.input.is_none() || last.output.is_none() || !event.timestamp_valid || event.timestamp <= cutoff { return None; }
        }
        let total = first.signature.total.as_ref()?;
        let last = first.signature.last.as_ref()?;
        let baseline = TokenCountersSignature {
            input: Some(total.input?.checked_sub(last.input?)?),
            cached_input: Some(total.cached_input?.checked_sub(last.cached_input?)?),
            output: Some(total.output?.checked_sub(last.output?)?),
            reasoning_output: Some(total.reasoning_output?.checked_sub(last.reasoning_output?)?),
            total: Some(total.total?.checked_sub(last.total?)?),
        };
        let mut found = false;
        for path in index.get(parent_id)? {
            if thread_id_from_filename(path).as_deref() != Some(parent_id.as_str()) { continue; }
            let before = file_signature(path)?;
            let mut reader = rollout_reader(path).ok()?;
            let mut used = 0u64;
            let mut ordinal = 0u64;
            let mut first_line = true;
            let mut parent_total = None;
            while used < end {
                let mut bytes = Vec::new();
                let n = std::io::Read::by_ref(&mut reader).take(end - used).read_until(b'\n', &mut bytes).ok()?;
                if n == 0 || bytes.last() != Some(&b'\n') { return None; }
                used += n as u64;
                let value: Value = serde_json::from_slice(&bytes).ok()?;
                if first_line {
                    ordinal = value.get("ordinal")?.as_u64()?;
                    let inherited = value.pointer("/payload/history_base/end_ordinal_exclusive").and_then(Value::as_u64).unwrap_or(0);
                    if ordinal != inherited || value.get("type")?.as_str()? != "session_meta" || value.pointer("/payload/id")?.as_str()? != parent_id { return None; }
                    first_line = false;
                }
                if value.get("ordinal")?.as_u64()? != ordinal { return None; }
                ordinal = ordinal.checked_add(1)?;
                if value.get("type").and_then(Value::as_str) == Some("event_msg") && value.pointer("/payload/type").and_then(Value::as_str) == Some("token_count") {
                    if let Some(v) = value.pointer("/payload/info/total_token_usage") { parent_total = parse_signature_counters(Some(v)); }
                }
            }
            if file_signature(path)? != before || used != end || ordinal != count || parent_total.as_ref() != Some(&baseline) { return None; }
            found = true;
        }
        found.then_some(())
    })().is_some()
}

fn resolve_parent_signatures(parent_id: &str, cutoff: DateTime<Utc>, index: &RolloutIndex, timelines: &mut HashMap<PathBuf, ParentTimeline>) -> Result<Vec<TokenUsageSignature>, String> {
    let candidates = index.get(parent_id).ok_or_else(|| format!("找不到父 rollout: {parent_id}"))?;
    let mut ordered = candidates.iter().collect::<Vec<_>>();
    ordered.sort_by(|a, b| a.file_name().cmp(&b.file_name()));
    let mut segments = Vec::<(Option<String>, Vec<TokenUsageSignature>, Option<DateTime<Utc>>)>::new();
    for path in ordered {
        if !timelines.contains_key(path) { timelines.insert(path.clone(), load_parent_timeline(path)?); }
        let timeline = &timelines[path];
        if timeline.has_token_without_timestamp { return Err(format!("父 rollout {} 的 token_count 缺少有效 timestamp", path.display())); }
        let snapshot: Vec<_> = timeline.events.iter().filter(|e| e.timestamp <= cutoff).map(|e| e.signature.clone()).collect();
        let rollout_id = thread_id_from_filename(path);
        if let Some((_, existing, max_timestamp)) = segments.iter_mut().find(|(id, _, _)| *id == rollout_id) {
            if *existing != snapshot { return Err(format!("父 rollout UUID {parent_id} 对应多个内容不一致的文件")); }
            *max_timestamp = match (*max_timestamp, timeline.max_timestamp) { (Some(a), Some(b)) => Some(a.min(b)), _ => None };
        } else { segments.push((rollout_id, snapshot, timeline.max_timestamp)); }
    }
    if segments.iter().filter_map(|(_, _, ts)| *ts).max().is_none_or(|ts| ts < cutoff) {
        return Err(format!("父 rollout {parent_id} 尚未写到 child fork 时刻"));
    }
    Ok(segments.into_iter().flat_map(|(_, events, _)| events).collect())
}

fn load_parent_timeline(path: &Path) -> Result<ParentTimeline, String> {
    let reader = rollout_reader(path)?;
    let mut timeline = ParentTimeline { events: Vec::new(), max_timestamp: None, has_token_without_timestamp: false, high_water: None };
    for line in reader.lines() {
        let line = line.map_err(|error| format!("{}: {error}", path.display()))?;
        // Most lines contain full prompts or tool results. Extract just the
        // top-level timestamp without deserializing those large payloads.
        if let Some(start) = line.find("\"timestamp\"").filter(|&pos| line.find("\"payload\"").is_none_or(|payload| pos < payload)) {
            let tail = line[start + "\"timestamp\"".len()..].trim_start();
            let tail = tail.strip_prefix(':').map(str::trim_start).and_then(|v| v.strip_prefix('"'));
            if let Some(timestamp) = tail.and_then(|v| v.split('"').next()) {
                if let Ok(ts) = DateTime::parse_from_rfc3339(timestamp) {
                    let ts = ts.with_timezone(&Utc);
                    timeline.max_timestamp = Some(timeline.max_timestamp.map_or(ts, |current| current.max(ts)));
                }
            }
        }
        if !line.contains("\"token_count\"") { continue; }
        let Ok(value) = serde_json::from_str::<Value>(&line) else { continue };
        let timestamp = parse_timestamp(value.get("timestamp"));
        if let Some(timestamp) = timestamp { timeline.max_timestamp = Some(timeline.max_timestamp.map_or(timestamp, |current| current.max(timestamp))); }
        if value.get("type").and_then(Value::as_str) != Some("event_msg") || value.pointer("/payload/type").and_then(Value::as_str) != Some("token_count") { continue; }
        let Some(info) = value.pointer("/payload/info").filter(|info| !info.is_null()) else { continue };
        if let Some(total) = info.get("total_token_usage").and_then(parse_cumulative_tokens) {
            if let Some(high) = timeline.high_water.as_mut() { update_high_water(high, &total); } else { timeline.high_water = Some(total); }
        }
        let Some(signature) = parse_token_signature(info) else { continue };
        if let Some(timestamp) = timestamp { timeline.events.push(TimestampedTokenSignature { timestamp, signature }); } else { timeline.has_token_without_timestamp = true; }
    }
    Ok(timeline)
}

fn matching_replay_prefix(child: &[ParsedTokenEvent], parent: &[TokenUsageSignature]) -> usize {
    let mut offset = 0;
    let mut matched = 0;
    for event in child {
        let Some(relative) = parent[offset..].iter().position(|signature| signature == &event.signature) else { break };
        offset += relative + 1;
        matched += 1;
    }
    matched
}

fn explicit_parent_from_meta(payload: &Value) -> ParentResolution {
    let forked = non_empty_string(payload.get("forked_from_id"));
    let spawned = payload.pointer("/source/subagent/thread_spawn/parent_thread_id").and_then(Value::as_str).filter(|value| !value.is_empty()).map(str::to_owned);
    match (forked, spawned) {
        (None, None) => ParentResolution::None,
        (Some(parent), None) | (None, Some(parent)) => ParentResolution::Parent(parent),
        (Some(a), Some(b)) if a == b => ParentResolution::Parent(a),
        (Some(a), Some(b)) => ParentResolution::Deferred(format!("forked_from_id ({a}) 与 thread_spawn.parent_thread_id ({b}) 不一致")),
    }
}

fn parse_signature_counters(value: Option<&Value>) -> Option<TokenCountersSignature> {
    let value = value?.as_object()?;
    Some(TokenCountersSignature {
        input: value.get("input_tokens").and_then(Value::as_u64),
        cached_input: value.get("cached_input_tokens").or_else(|| value.get("cache_read_input_tokens")).and_then(Value::as_u64),
        output: value.get("output_tokens").and_then(Value::as_u64),
        reasoning_output: value.get("reasoning_output_tokens").and_then(Value::as_u64),
        total: value.get("total_tokens").and_then(Value::as_u64),
    })
}

fn parse_token_signature(info: &Value) -> Option<TokenUsageSignature> {
    let total = parse_signature_counters(info.get("total_token_usage"));
    let last = parse_signature_counters(info.get("last_token_usage"));
    (total.is_some() || last.is_some()).then_some(TokenUsageSignature { total, last })
}

fn parse_cumulative_tokens(value: &Value) -> Option<CumulativeTokens> {
    let fields = value.as_object()?;
    if !["input_tokens", "cached_input_tokens", "cache_read_input_tokens", "output_tokens", "reasoning_output_tokens", "total_tokens"].iter().any(|field| fields.contains_key(*field)) { return None; }
    Some(CumulativeTokens {
        input: value.get("input_tokens").and_then(Value::as_u64).unwrap_or(0),
        cached_input: value.get("cached_input_tokens").or_else(|| value.get("cache_read_input_tokens")).and_then(Value::as_u64).unwrap_or(0),
        cache_write_input: value.get("cache_write_input_tokens").and_then(Value::as_u64).unwrap_or(0),
        output: value.get("output_tokens").and_then(Value::as_u64).unwrap_or(0),
    })
}

fn compute_delta(previous: &Option<CumulativeTokens>, current: &CumulativeTokens) -> DeltaTokens {
    match previous {
        None => DeltaTokens { input: current.input, cached_input: current.cached_input, cache_write_input: current.cache_write_input, output: current.output },
        Some(previous) => DeltaTokens { input: current.input.saturating_sub(previous.input), cached_input: current.cached_input.saturating_sub(previous.cached_input), cache_write_input: current.cache_write_input.saturating_sub(previous.cache_write_input), output: current.output.saturating_sub(previous.output) },
    }
}

fn update_high_water(high: &mut CumulativeTokens, current: &CumulativeTokens) {
    high.input = high.input.max(current.input);
    high.cached_input = high.cached_input.max(current.cached_input);
    high.cache_write_input = high.cache_write_input.max(current.cache_write_input);
    high.output = high.output.max(current.output);
}

fn token_snapshot_source(payload: &Value) -> Option<String> {
    payload.pointer("/rate_limits/limit_id").and_then(Value::as_str).filter(|value| !value.is_empty()).map(str::to_owned)
}

fn normalize_codex_model(raw: &str) -> String {
    let mut name = raw.to_lowercase();
    if let Some(position) = name.rfind('/') { name = name[position + 1..].to_string(); }
    if name.len() > 11 && name.is_char_boundary(name.len() - 11) {
        let suffix = &name[name.len() - 11..];
        if suffix.is_ascii() && suffix.as_bytes()[0] == b'-' && suffix[1..5].chars().all(|c| c.is_ascii_digit()) && suffix.as_bytes()[5] == b'-' && suffix[6..8].chars().all(|c| c.is_ascii_digit()) && suffix.as_bytes()[8] == b'-' && suffix[9..11].chars().all(|c| c.is_ascii_digit()) { name.truncate(name.len() - 11); }
    }
    if name.len() > 9 {
        let parts = name.rsplitn(2, '-').collect::<Vec<_>>();
        if parts.len() == 2 && parts[0].len() == 8 && parts[0].chars().all(|c| c.is_ascii_digit()) { name = parts[1].to_string(); }
    }
    name
}

fn non_empty_string(value: Option<&Value>) -> Option<String> { value.and_then(Value::as_str).filter(|value| !value.is_empty()).map(str::to_owned) }
fn parse_timestamp(value: Option<&Value>) -> Option<DateTime<Utc>> { value.and_then(Value::as_str).and_then(|value| DateTime::parse_from_rfc3339(value).ok()).map(|value| value.with_timezone(&Utc)) }
fn project_name(value: Option<&str>) -> String { value.and_then(|path| path.replace('\\', "/").rsplit('/').next().map(str::to_owned)).filter(|value| !value.is_empty()).unwrap_or_else(|| "unknown".into()) }
fn file_signature(path: &Path) -> Option<String> { let metadata = fs::metadata(path).ok()?; let modified = metadata.modified().ok()?.duration_since(UNIX_EPOCH).ok()?.as_nanos(); Some(format!("{}:{modified}", metadata.len())) }

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::SystemTime;

    const PARENT: &str = "00000000-0000-4000-8000-000000000001";
    const CHILD: &str = "00000000-0000-4000-8000-000000000002";

    fn temporary_directory(label: &str) -> PathBuf {
        let unique = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let path = std::env::temp_dir().join(format!("tokenusage-{label}-{}-{unique}", std::process::id()));
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn rollout(root: &Path, thread: &str) -> PathBuf {
        let directory = root.join("sessions/2026/01/01");
        fs::create_dir_all(&directory).unwrap();
        directory.join(format!("rollout-2026-01-01T00-00-00-{thread}.jsonl"))
    }

    fn meta(id: &str, timestamp: &str, parent: Option<&str>) -> String {
        let mut payload = serde_json::json!({"id":id,"cwd":"C:/work/project"});
        if let Some(parent) = parent { payload["forked_from_id"] = Value::String(parent.into()); }
        serde_json::json!({"timestamp":timestamp,"type":"session_meta","payload":payload}).to_string()
    }

    fn token(timestamp: &str, input: u64, cached: u64, output: u64, source: &str) -> String {
        serde_json::json!({"timestamp":timestamp,"type":"event_msg","payload":{"type":"token_count","rate_limits":{"limit_id":source},"info":{"total_token_usage":{"input_tokens":input,"cached_input_tokens":cached,"output_tokens":output,"total_tokens":input+output},"last_token_usage":{"input_tokens":input,"cached_input_tokens":cached,"output_tokens":output,"total_tokens":input+output}}}}).to_string()
    }

    fn write_lines(path: &Path, lines: &[String]) { fs::write(path, lines.join("\n")).unwrap(); }

    fn remove_test_directory(root: &Path) {
        let resolved = fs::canonicalize(root).unwrap();
        let temp = fs::canonicalize(std::env::temp_dir()).unwrap();
        assert!(resolved.starts_with(temp) && resolved.file_name().unwrap().to_string_lossy().starts_with("tokenusage-"));
        fs::remove_dir_all(resolved).unwrap();
    }

    #[test]
    fn interleaved_counter_lanes_use_exact_last_usage() {
        let info_a = serde_json::json!({"total_token_usage":{"input_tokens":1000,"cached_input_tokens":800,"output_tokens":100},"last_token_usage":{"input_tokens":100,"cached_input_tokens":80,"output_tokens":10}});
        let info_b = serde_json::json!({"total_token_usage":{"input_tokens":400,"cached_input_tokens":300,"output_tokens":40},"last_token_usage":{"input_tokens":50,"cached_input_tokens":30,"output_tokens":5}});
        let mut by_source = HashMap::<Option<String>, TokenUsageSignature>::new();
        let mut previous = None;
        let mut total = 0;
        for (source, info) in [("a", &info_a), ("b", &info_b), ("a", &info_a)] {
            let signature = parse_token_signature(info).unwrap();
            let duplicate = by_source.get(&Some(source.into())) == Some(&signature) || previous.as_ref() == Some(&signature);
            by_source.insert(Some(source.into()), signature.clone());
            previous = Some(signature);
            if !duplicate { total += parse_cumulative_tokens(info.get("last_token_usage").unwrap()).unwrap().input; }
        }
        assert_eq!(total, 150);
    }

    #[test]
    fn incremental_output_contains_only_new_or_changed_requests() {
        let root = temporary_directory("codex-delta");
        let path = rollout(&root, PARENT);
        let cache = root.join("cache.json");
        let mut lines = vec![meta(PARENT, "2026-01-01T00:00:00Z", None), token("2026-01-01T00:01:00Z", 100, 80, 10, "a")];
        write_lines(&path, &lines);
        let first = scan_with_options(&root, &cache, false, false).unwrap();
        assert_eq!(first.records.len(), 1);
        assert!(first.records[0].record_id.is_some());
        assert!(scan_with_options(&root, &cache, false, true).unwrap().records.is_empty());
        lines.push(token("2026-01-01T00:02:00Z", 50, 30, 5, "b"));
        write_lines(&path, &lines);
        let delta = scan_with_options(&root, &cache, false, true).unwrap();
        assert_eq!(delta.records.len(), 1);
        assert_eq!(delta.records[0].total_tokens, 55);
        assert_eq!(scan_with_options(&root, &cache, false, false).unwrap().records.len(), 2);
        remove_test_directory(&root);
    }

    #[test]
    fn removing_preferred_duplicate_replays_the_remaining_record() {
        let root = temporary_directory("codex-delta-archive");
        let session = rollout(&root, PARENT);
        let archived_dir = root.join("archived_sessions");
        fs::create_dir_all(&archived_dir).unwrap();
        let archived = archived_dir.join(session.file_name().unwrap());
        let cache = root.join("cache.json");
        write_lines(&archived, &[meta(PARENT, "2026-01-01T00:00:00Z", None), token("2026-01-01T00:01:00Z", 100, 80, 10, "a")]);
        write_lines(&session, &[meta(PARENT, "2026-01-01T00:00:00Z", None), token("2026-01-01T00:01:00Z", 200, 80, 20, "a")]);
        assert_eq!(scan_with_options(&root, &cache, false, false).unwrap().records[0].total_tokens, 110);
        fs::remove_file(&archived).unwrap();
        let next = scan_with_options(&root, &cache, false, true).unwrap();
        assert_eq!(next.records.len(), 1);
        assert_eq!(next.records[0].total_tokens, 220);
        remove_test_directory(&root);
    }

    #[test]
    fn rebuild_deduplicates_archived_copy_by_thread_event_request_id() {
        let root = temporary_directory("codex-copy");
        let path = rollout(&root, PARENT);
        let lines = vec![meta(PARENT, "2026-01-01T00:00:00Z", None), token("2026-01-01T00:01:00Z", 100, 80, 10, "a"), token("2026-01-01T00:02:00Z", 50, 30, 5, "b")];
        write_lines(&path, &lines);
        let archived = root.join("archived_sessions");
        fs::create_dir_all(&archived).unwrap();
        write_lines(&archived.join(path.file_name().unwrap()), &lines);
        let result = scan(&root, &root.join("cache.json"), true).unwrap();
        assert_eq!(result.records.len(), 2);
        assert_eq!(result.records.iter().map(|record| record.total_tokens).sum::<i64>(), 165);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rebuild_removes_parent_replay_prefix_from_child() {
        let root = temporary_directory("codex-fork");
        let parent = rollout(&root, PARENT);
        let first = token("2026-01-01T00:01:00Z", 100, 80, 10, "a");
        let second = token("2026-01-01T00:02:00Z", 50, 30, 5, "b");
        write_lines(&parent, &[meta(PARENT, "2026-01-01T00:00:00Z", None), first.clone(), second.clone()]);
        let child = rollout(&root, CHILD);
        write_lines(&child, &[meta(CHILD, "2026-01-01T00:02:00Z", Some(PARENT)), first, second, token("2026-01-01T00:03:00Z", 20, 10, 2, "c")]);
        let result = scan(&root, &root.join("cache.json"), true).unwrap();
        assert_eq!(result.records.len(), 3);
        assert_eq!(result.deferred_files, 0);
        assert_eq!(result.records.iter().filter(|record| record.session_id == CHILD).count(), 1);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rebuild_defers_child_when_parent_rollout_is_missing() {
        let root = temporary_directory("codex-deferred");
        let child = rollout(&root, CHILD);
        write_lines(&child, &[meta(CHILD, "2026-01-01T00:02:00Z", Some(PARENT)), token("2026-01-01T00:03:00Z", 20, 10, 2, "c")]);
        let result = scan(&root, &root.join("cache.json"), true).unwrap();
        assert!(result.records.is_empty());
        assert_eq!(result.deferred_files, 1);
        let unchanged = scan(&root, &root.join("cache.json"), false).unwrap();
        assert_eq!(unchanged.files_parsed, 0);
        assert_eq!(unchanged.deferred_files, 1);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn paginated_continuation_with_branch_metadata_is_not_a_new_fork() {
        let root = temporary_directory("codex-continuation");
        let directory = root.join("sessions/2026/01/01");
        fs::create_dir_all(&directory).unwrap();
        let page = directory.join(format!("rollout-2026-01-01T00-00-00-{CHILD}_{PARENT}.jsonl"));
        let mut first: Value = serde_json::from_str(&meta(CHILD, "2026-01-01T00:02:00Z", Some(PARENT))).unwrap();
        first["payload"]["history_mode"] = Value::String("paginated".into());
        write_lines(&page, &[first.to_string(), token("2026-01-01T00:03:00Z", 20, 10, 2, "c")]);
        let result = scan(&root, &root.join("cache.json"), true).unwrap();
        assert_eq!(result.deferred_files, 0);
        assert_eq!(result.records.iter().map(|record| record.total_tokens).sum::<i64>(), 22);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn compressed_archives_are_read_and_plain_sibling_is_not_double_counted() {
        let root = temporary_directory("codex-zstd");
        let plain = rollout(&root, PARENT);
        let data = [meta(PARENT, "2026-01-01T00:00:00Z", None), token("2026-01-01T00:01:00Z", 100, 80, 10, "a")].join("\n");
        let compressed = plain.with_file_name(format!("{}.zst", plain.file_name().unwrap().to_string_lossy()));
        fs::write(&compressed, zstd::stream::encode_all(data.as_bytes(), 1).unwrap()).unwrap();
        let first = scan(&root, &root.join("cache.json"), true).unwrap();
        assert_eq!(first.records.iter().map(|r| r.total_tokens).sum::<i64>(), 110);
        fs::write(&plain, data).unwrap();
        let second = scan(&root, &root.join("cache.json"), true).unwrap();
        assert_eq!(second.files_scanned, 1);
        assert_eq!(second.records.iter().map(|r| r.total_tokens).sum::<i64>(), 110);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn continuation_cumulative_only_uses_previous_segment_baseline() {
        let root = temporary_directory("codex-baseline");
        let first = rollout(&root, CHILD);
        let mut first_total: Value = serde_json::from_str(&token("2026-01-01T00:01:00Z", 100, 80, 10, "a")).unwrap();
        first_total["payload"]["info"].as_object_mut().unwrap().remove("last_token_usage");
        write_lines(&first, &[meta(CHILD, "2026-01-01T00:00:00Z", None), first_total.to_string()]);
        let page = first.parent().unwrap().join(format!("rollout-2026-01-02T00-00-00-{CHILD}_{PARENT}.jsonl"));
        let mut page_meta: Value = serde_json::from_str(&meta(CHILD, "2026-01-02T00:00:00Z", None)).unwrap();
        page_meta["payload"]["history_mode"] = Value::String("paginated".into());
        page_meta["payload"]["history_base"] = serde_json::json!({"thread_id": CHILD});
        let mut second_total: Value = serde_json::from_str(&token("2026-01-02T00:01:00Z", 120, 90, 12, "a")).unwrap();
        second_total["payload"]["info"].as_object_mut().unwrap().remove("last_token_usage");
        write_lines(&page, &[page_meta.to_string(), second_total.to_string()]);
        let result = scan(&root, &root.join("cache.json"), true).unwrap();
        assert_eq!(result.deferred_files, 0);
        assert_eq!(result.records.iter().map(|r| r.total_tokens).sum::<i64>(), 132);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn token_usage_record_is_added_only_when_following_last_usage_is_zero() {
        let root = temporary_directory("codex-usage-record");
        let path = rollout(&root, CHILD);
        let usage = serde_json::json!({"type":"token_usage_record","timestamp":"2026-01-01T00:01:00Z","payload":{"response_id":"response-1","usage":{"input_tokens":100,"cached_input_tokens":70,"cache_write_input_tokens":10,"output_tokens":5}}}).to_string();
        let empty = token("2026-01-01T00:02:00Z", 0, 0, 0, "a");
        write_lines(&path, &[meta(CHILD, "2026-01-01T00:00:00Z", None), usage, empty]);
        let result = scan(&root, &root.join("cache.json"), true).unwrap();
        assert_eq!(result.records.len(), 1);
        assert_eq!(result.records[0].total_tokens, 105);
        assert_eq!(result.records[0].input_tokens, 20);
        assert_eq!(result.records[0].cache_write_tokens, 10);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn bounded_fork_requires_verified_parent_prefix_and_baseline() {
        let root = temporary_directory("codex-bounded");
        let parent = rollout(&root, PARENT);
        let mut pmeta: Value = serde_json::from_str(&meta(PARENT, "2026-01-01T00:00:00Z", None)).unwrap();
        pmeta["ordinal"] = serde_json::json!(0);
        let mut pt: Value = serde_json::from_str(&token("2026-01-01T00:01:00Z", 100, 80, 10, "a")).unwrap();
        pt["ordinal"] = serde_json::json!(1);
        pt["payload"]["info"]["total_token_usage"]["reasoning_output_tokens"] = serde_json::json!(0);
        pt["payload"]["info"]["last_token_usage"]["reasoning_output_tokens"] = serde_json::json!(0);
        let prefix = format!("{}\n{}\n", pmeta, pt);
        fs::write(&parent, &prefix).unwrap();
        let child = rollout(&root, CHILD);
        let mut cmeta: Value = serde_json::from_str(&meta(CHILD, "2026-01-01T00:02:00Z", Some(PARENT))).unwrap();
        cmeta["payload"]["history_base"] = serde_json::json!({"thread_id":PARENT,"end_byte_offset":prefix.len(),"end_ordinal_exclusive":2});
        let mut ct: Value = serde_json::from_str(&token("2026-01-01T00:03:00Z", 20, 10, 2, "b")).unwrap();
        ct["payload"]["info"]["total_token_usage"] = serde_json::json!({"input_tokens":120,"cached_input_tokens":90,"output_tokens":12,"reasoning_output_tokens":0,"total_tokens":132});
        ct["payload"]["info"]["last_token_usage"]["reasoning_output_tokens"] = serde_json::json!(0);
        write_lines(&child, &[cmeta.to_string(), ct.to_string()]);
        let result = scan(&root, &root.join("cache.json"), true).unwrap();
        assert_eq!(result.deferred_files, 0);
        assert_eq!(result.records.iter().map(|r| r.total_tokens).sum::<i64>(), 132);
        cmeta["payload"]["history_base"]["end_ordinal_exclusive"] = serde_json::json!(3);
        write_lines(&child, &[cmeta.to_string(), ct.to_string()]);
        let bad = scan(&root, &root.join("cache.json"), true).unwrap();
        assert_eq!(bad.deferred_files, 1);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn previously_imported_usage_survives_missing_logs_and_archive_moves() {
        let root = temporary_directory("codex-retention"); let cache = root.join("cache.json");
        let path = rollout(&root, PARENT);
        let content = [meta(PARENT, "2026-01-01T00:00:00Z", None), token("2026-01-01T00:01:00Z", 100, 80, 10, "a")].join("\n");
        fs::write(&path, &content).unwrap();
        assert_eq!(scan(&root, &cache, true).unwrap().records.len(), 1);
        fs::remove_file(&path).unwrap();
        let retained = scan(&root, &cache, true).unwrap();
        assert_eq!(retained.retained_files, 1);
        assert_eq!(retained.records[0].total_tokens, 110);
        let archive = root.join("archived_sessions"); fs::create_dir_all(&archive).unwrap();
        fs::write(archive.join(path.file_name().unwrap()), &content).unwrap();
        let moved = scan(&root, &cache, false).unwrap();
        assert_eq!(moved.records.len(), 1);
        assert_eq!(moved.retained_files, 0);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn previously_accepted_child_is_preserved_if_parent_disappears() {
        let root = temporary_directory("codex-parent-retention"); let cache = root.join("cache.json");
        let parent = rollout(&root, PARENT); let child = rollout(&root, CHILD);
        let inherited = token("2026-01-01T00:01:00Z", 100, 80, 10, "a");
        write_lines(&parent, &[meta(PARENT, "2026-01-01T00:00:00Z", None), inherited.clone(), token("2026-01-01T00:02:00Z", 50, 30, 5, "b")]);
        write_lines(&child, &[meta(CHILD, "2026-01-01T00:02:00Z", Some(PARENT)), inherited, token("2026-01-01T00:03:00Z", 20, 10, 2, "c")]);
        let initial = scan(&root, &cache, true).unwrap();
        assert_eq!(initial.records.iter().map(|r| r.total_tokens).sum::<i64>(), 187);
        fs::remove_file(&parent).unwrap();
        let retained = scan(&root, &cache, false).unwrap();
        assert_eq!(retained.deferred_files, 1);
        assert_eq!(retained.records.iter().map(|r| r.total_tokens).sum::<i64>(), 187);
        let again = scan(&root, &cache, false).unwrap();
        assert_eq!(again.records.iter().map(|r| r.total_tokens).sum::<i64>(), 187);
        let _ = fs::remove_dir_all(root);
    }
}

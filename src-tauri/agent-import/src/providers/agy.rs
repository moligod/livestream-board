// Adapted from CC Switch v4.0.40 (MIT; Copyright (c) 2025 Jason Young).
use serde_json::Value;
use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
};
pub(crate) fn transcript_files() -> Vec<(String, PathBuf)> {
    transcript_files_in_roots(&session_roots())
}

pub(crate) fn transcript_files_in_roots(roots: &[PathBuf]) -> Vec<(String, PathBuf)> {
    let mut seen = HashSet::new();
    let mut files = Vec::new();
    for root in roots {
        let canonical_root = root.canonicalize().ok();
        for (id, path) in transcript_files_in_root(root, canonical_root.as_deref()) {
            if seen.insert(id.clone()) {
                files.push((id, path));
            }
        }
    }
    files
}

fn transcript_files_in_root(root: &Path, canonical_root: Option<&Path>) -> Vec<(String, PathBuf)> {
    fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|t| t.is_dir()))
        .filter_map(|entry| {
            let id = entry
                .file_name()
                .into_string()
                .ok()
                .filter(|id| valid_session_id(id))?;
            Some((id.clone(), valid_transcript(root, canonical_root, &id)?))
        })
        .collect()
}

fn valid_transcript(root: &Path, canonical_root: Option<&Path>, id: &str) -> Option<PathBuf> {
    let path = root
        .join(id)
        .join(".system_generated/logs/transcript_full.jsonl");
    (path.is_file()
        && path
            .canonicalize()
            .ok()
            .zip(canonical_root)
            .is_some_and(|(path, root)| path.starts_with(root)))
    .then_some(path)
}

fn valid_session_id(id: &str) -> bool {
    !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}
pub(crate) fn model_from_settings_change(content: &str) -> Option<String> {
    let start = content.find("Model Selection")?;
    let rest = &content[start..];
    let to = rest.find(" to ")? + 4;
    let tail = &rest[to..];
    // 模型名里本身有点（Gemini 3.8），只把「句号 + 空格」当成句子结束。
    let end = tail
        .find(". ")
        .or_else(|| tail.find('\n'))
        .or_else(|| tail.find('`'))
        .unwrap_or(tail.len());
    let model = tail[..end].trim().trim_matches('.').trim();
    if model.is_empty() || model.eq_ignore_ascii_case("none") {
        None
    } else {
        Some(model.to_string())
    }
}

pub(crate) fn timestamp(value: &Value) -> Option<i64> {
    parse_timestamp_to_ms(&value["created_at"])
}

pub fn session_roots() -> Vec<PathBuf> {
    let home = crate::config::get_home_dir();
    let gemini = crate::gemini_config::get_gemini_dir();
    let mut roots = ["antigravity-cli", "antigravity", "antigravity-acp"]
        .map(|n| gemini.join(n).join("brain"))
        .to_vec();
    let profile_root = home.join(".cc-switch/agy");
    for entry in fs::read_dir(profile_root).into_iter().flatten().flatten() {
        if entry.file_type().is_ok_and(|t| t.is_dir()) {
            roots.push(entry.path().join("antigravity-cli/brain"));
        }
    }
    let mut homes = vec![home.join(".t3")];
    if let Some(path) = crate::env_path("T3CODE_HOME") {
        if !homes.contains(&path) {
            homes.push(path);
        }
    }
    for root in homes {
        for state in ["userdata", "dev"] {
            for entry in fs::read_dir(root.join(state).join("providers/antigravity"))
                .into_iter()
                .flatten()
                .flatten()
            {
                if entry.file_type().is_ok_and(|t| t.is_dir()) {
                    roots.push(entry.path().join("antigravity-acp/brain"));
                }
            }
        }
    }
    roots
}
pub fn records(path: &Path) -> Result<Vec<Value>, String> {
    use std::io::{BufRead, BufReader};
    const MAX_LINE: usize = 32 * 1024 * 1024;
    let file = fs::File::open(path).map_err(|e| e.to_string())?;
    let mut reader = BufReader::new(file);
    let mut rows = Vec::new();
    let mut line = Vec::new();
    loop {
        line.clear();
        let count = std::io::Read::take(&mut reader, (MAX_LINE + 1) as u64)
            .read_until(b'\n', &mut line)
            .map_err(|e| e.to_string())?;
        if count == 0 {
            break;
        }
        if count > MAX_LINE {
            return Err("Antigravity transcript row exceeds size limit".to_string());
        }
        if let Ok(row) = serde_json::from_slice(&line) {
            rows.push(row);
        }
    }
    Ok(rows)
}
fn parse_timestamp_to_ms(value: &Value) -> Option<i64> {
    if let Some(n) = value.as_i64() {
        return Some(if n < 100_000_000_000 {
            n.saturating_mul(1000)
        } else {
            n
        });
    }
    crate::services::session_usage::parse_timestamp_millis(value.as_str()?)
}

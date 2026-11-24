mod database;
mod error;
mod kimi_session;
mod providers;
mod services;

use serde::Serialize;
use services::{
    session_usage_agy as agy, session_usage_copilot as copilot, session_usage_dsh as dsh,
    session_usage_gemini as gemini, session_usage_grokbuild as grok, session_usage_kimi as kimi,
    session_usage_mcode as mcode, session_usage_omp as omp, session_usage_pi as pi,
    session_usage_workbuddy as workbuddy,
};
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
};

pub const SOURCES: [&str; 10] = [
    "gemini",
    "grokbuild",
    "pi",
    "omp",
    "mcode",
    "agy",
    "kimi",
    "copilot",
    "workbuddy",
    "dsh",
];

pub fn roots(source: &str) -> Vec<PathBuf> {
    use providers as p;
    match source {
        "gemini" => vec![gemini_config::get_gemini_dir().join("tmp")],
        "grokbuild" => p::grokbuild::session_roots(),
        "pi" => p::pi::session_roots(),
        "omp" => p::omp::session_roots(),
        "mcode" => vec![p::mcode::database_path()],
        "agy" => p::agy::session_roots(),
        "kimi" => vec![kimi_config::data_dir().join("sessions")],
        "copilot" => vec![copilot_config::session_state_dir()],
        "workbuddy" => vec![p::workbuddy::projects_dir()],
        "dsh" => vec![dsh::dsh_sessions_root()],
        _ => Vec::new(),
    }
}

pub fn display_path(source: &str) -> String {
    let paths = roots(source);
    let path = paths.iter().find(|p| p.exists()).or_else(|| paths.first());
    path.map_or_else(String::new, |path| path.display().to_string())
}

pub fn fingerprint(source: &str) -> Result<String, String> {
    let mut hash = Sha256::new();
    hash.update(b"tokenusage-agent-import-v1");
    for root in roots(source) {
        stamp(&root, &mut hash);
        if source == "mcode" {
            stamp(&append(&root, "-wal"), &mut hash);
        } else if root.is_dir() {
            fingerprint_dir(source, &root, &mut hash, 0)
                .map_err(|e| format!("{}: {e}", root.display()))?;
        }
    }
    if source == "agy" {
        let mut transcripts = providers::agy::transcript_files();
        transcripts.sort();
        for (_, path) in transcripts {
            hash.update(agy::native_fingerprint(&path));
        }
    }
    Ok(format!("{:x}", hash.finalize()))
}

fn append(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    name.into()
}

fn stamp(path: &Path, hash: &mut Sha256) {
    hash.update(path.to_string_lossy().as_bytes());
    hash.update([0]);
    if let Ok(metadata) = fs::metadata(path) {
        hash.update(metadata.len().to_le_bytes());
        hash.update(services::session_usage::metadata_modified_nanos(&metadata).to_le_bytes());
    } else {
        hash.update(b"absent");
    }
}

fn fingerprint_dir(source: &str, path: &Path, hash: &mut Sha256, depth: u8) -> std::io::Result<()> {
    if depth > 16 {
        return Ok(());
    }
    let mut entries = match fs::read_dir(path) {
        Ok(entries) => entries.collect::<Result<Vec<_>, _>>()?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let kind = match entry.file_type() {
            Ok(kind) => kind,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        let path = entry.path();
        if kind.is_dir() {
            fingerprint_dir(source, &path, hash, depth + 1)?;
        } else if kind.is_file() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let relevant = match source {
                "gemini" => {
                    name.starts_with("session-")
                        && (name.ends_with(".json") || name.ends_with(".jsonl"))
                }
                "grokbuild" => name == "updates.jsonl",
                "agy" => name == "transcript_full.jsonl",
                "copilot" => name == "events.jsonl",
                "dsh" => matches!(name.as_ref(), "session.v4.jsonl" | "session.v4.jsonl.zstd"),
                _ => name.ends_with(".jsonl"),
            };
            if relevant {
                stamp(&path, hash);
            }
        }
    }
    Ok(())
}

#[derive(Debug, Serialize)]
pub struct Record {
    pub source: String,
    pub record_id: String,
    pub session_id: String,
    pub timestamp: i64,
    pub project: String,
    pub model: String,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub reasoning_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_write_tokens: i64,
    pub total_tokens: i64,
    pub cost: f64,
}

pub struct Collection {
    pub records: Vec<Record>,
    pub status: &'static str,
    pub warning: Option<String>,
}

pub fn collect(source: &str) -> Result<Collection, String> {
    let paths = roots(source);
    // Pi-family malformed configuration must be reported, not passed off as an absent install.
    if !paths.iter().any(|p| p.exists()) && !(matches!(source, "pi" | "omp") && paths.is_empty()) {
        return Ok(Collection {
            records: Vec::new(),
            status: "missing",
            warning: None,
        });
    }
    let db = database::Database::memory().map_err(|e| e.to_string())?;
    let cursors = Default::default();
    let summary = match source {
        "gemini" => gemini::sync_gemini_usage(&db, &cursors),
        "grokbuild" => grok::sync_grokbuild_usage(&db, &cursors),
        "pi" => pi::sync_pi_usage_with_cursors(&db, &cursors),
        "omp" => omp::sync_omp_usage(&db, &cursors),
        "mcode" => mcode::sync_mcode_usage(&db),
        "agy" => agy::sync_agy_usage_with_cursors(&db, &cursors),
        "kimi" => kimi::sync_kimi_usage(&db, &cursors),
        "copilot" => copilot::sync_copilot_usage(&db, &cursors),
        "workbuddy" => workbuddy::sync_workbuddy_usage(&db, &cursors),
        "dsh" => dsh::sync_dsh_usage(&db, &cursors),
        _ => return Err(format!("Unknown usage source: {source}")),
    }
    .map_err(|e| e.to_string())?;
    let warning = if summary.errors.is_empty() {
        None
    } else {
        Some(format!(
            "{} 个会话读取失败：{}",
            summary.errors.len(),
            summary.errors[0]
        ))
    };
    let records = db.records(source).map_err(|e| e.to_string())?;
    let status = if records.is_empty() && warning.is_some() {
        "error"
    } else {
        "ready"
    };
    Ok(Collection {
        records,
        status,
        warning,
    })
}

mod app_config {
    pub enum AppType {
        Copilot,
    }
}
mod session_manager {
    pub(crate) use crate::providers;
}
mod config {
    pub fn get_home_dir() -> std::path::PathBuf {
        super::env_path("TOKENUSAGE_AGENT_HOME")
            .or_else(dirs::home_dir)
            .unwrap_or_default()
    }
}
fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}
mod gemini_config {
    use super::*;
    pub fn get_gemini_dir() -> PathBuf {
        env_path("GEMINI_CLI_HOME")
            .map(|p| p.join(".gemini"))
            .unwrap_or_else(|| config::get_home_dir().join(".gemini"))
    }
}
mod kimi_config {
    pub fn data_dir() -> std::path::PathBuf {
        super::env_path("KIMI_CODE_HOME")
            .unwrap_or_else(|| super::config::get_home_dir().join(".kimi-code"))
    }
}
mod copilot_config {
    pub fn session_state_dir() -> std::path::PathBuf {
        super::env_path("COPILOT_HOME")
            .unwrap_or_else(|| super::config::get_home_dir().join(".copilot"))
            .join("session-state")
    }
    pub fn selected_provider_name(model: &str) -> Option<&str> {
        model
            .split_once('/')
            .map(|(name, _)| name)
            .filter(|name| !name.is_empty())
    }
}
mod dsh_config {
    pub fn data_dir() -> std::path::PathBuf {
        super::env_path("DSH_HOME").unwrap_or_else(|| super::config::get_home_dir().join(".dsh"))
    }
}
mod workbuddy_config {
    pub fn data_dir() -> std::path::PathBuf {
        super::env_path("WORKBUDDY_CONFIG_DIR")
            .unwrap_or_else(|| super::config::get_home_dir().join(".workbuddy"))
    }
}
mod mcode_config {
    pub fn data_dir() -> std::path::PathBuf {
        super::env_path("MINIMAX_DATA_DIR")
            .or_else(|| super::env_path("MAVIS_DATA_DIR"))
            .unwrap_or_else(|| super::config::get_home_dir().join(".minimax"))
    }
}
mod pi_config {
    use super::*;
    use error::AppError;
    pub struct Defaults {
        pub session_dir: Option<String>,
    }
    pub fn get_pi_agent_dir() -> Result<PathBuf, AppError> {
        Ok(env_path("PI_CODING_AGENT_DIR")
            .unwrap_or_else(|| config::get_home_dir().join(".pi/agent")))
    }
    pub fn read_pi_native_defaults() -> Result<Defaults, AppError> {
        let path = get_pi_agent_dir()?.join("settings.json");
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Defaults { session_dir: None })
            }
            Err(e) => return Err(AppError::io(&path, e)),
        };
        let value: serde_json::Value = serde_json::from_slice(&bytes)
            .map_err(|e| AppError::Config(format!("{}: {e}", path.display())))?;
        Ok(Defaults {
            session_dir: value
                .get("sessionDir")
                .and_then(|v| v.as_str())
                .map(str::to_owned),
        })
    }
}
mod omp_config;

mod proxy {
    pub mod usage {
        pub mod parser {
            #[allow(dead_code)]
            pub struct TokenUsage {
                pub input_tokens: u32,
                pub output_tokens: u32,
                pub cache_read_tokens: u32,
                pub cache_creation_tokens: u32,
                pub cache_creation_1h_tokens: u32,
                pub model: Option<String>,
                pub message_id: Option<String>,
            }
        }
        pub mod calculator {
            use super::parser::TokenUsage;
            use rust_decimal::Decimal;
            pub struct ModelPricing;
            pub struct Cost {
                pub input_cost: Decimal,
                pub output_cost: Decimal,
                pub cache_read_cost: Decimal,
                pub cache_creation_cost: Decimal,
                pub total_cost: Decimal,
            }
            pub struct CostCalculator;
            impl CostCalculator {
                // The TokenUsage ledger estimates costs using its own configured model prices.
                // No CC Switch pricing or provider configuration is consulted by this adapter.
                pub fn calculate_for_app(
                    _: &str,
                    _: &TokenUsage,
                    _: &ModelPricing,
                    _: Decimal,
                ) -> Cost {
                    Cost {
                        input_cost: Decimal::ZERO,
                        output_cost: Decimal::ZERO,
                        cache_read_cost: Decimal::ZERO,
                        cache_creation_cost: Decimal::ZERO,
                        total_cost: Decimal::ZERO,
                    }
                }
            }
        }
    }
}

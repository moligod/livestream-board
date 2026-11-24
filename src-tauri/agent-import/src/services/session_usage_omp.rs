// Adapted from CC Switch v4.0.40 (MIT; Copyright (c) 2025 Jason Young).
//! Oh My Pi (omp.sh) session usage importer.
//!
//! Oh My Pi is its own coding agent, not Pi (pi.dev): it keeps its session
//! store under the Oh My Pi agent dir and records a few entry kinds Pi never
//! writes. It does persist Pi's session JSONL format, so the parsing engine is
//! shared with [`super::session_usage_pi`] and this module only declares what
//! differs.

use crate::database::Database;
use crate::error::AppError;
use crate::services::session_usage::SessionSyncResult;
use crate::services::session_usage_pi::Flavor;
use crate::session_manager::providers::{omp, pi};
use serde_json::Value;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};

/// Room for the 256-byte title slot plus any multi-byte title, never a whole
/// session line.
const TITLE_SLOT_PROBE_BYTES: u64 = 4096;

pub(crate) const OMP: Flavor = Flavor {
    app_type: "omp",
    data_source: "omp_session",
    provider_placeholder: "_omp_session",
    cutoff_key: None,
    id_prefix: "omp_session",
    hash_tag: "omp-session",
    label: "Oh My Pi",
    model_usage_carrier: true,
    title_slot: true,
    // Oh My Pi's subagents (its `task` tool) keep their own session files;
    // Pi's official `subagent` example extension contract is Pi-only.
    subagent_tool_results: false,
    session_files,
};

fn session_files() -> Result<Vec<PathBuf>, String> {
    let files = pi::session_files_for(&omp::OMP)?;
    Ok(own_files(files, shares_root_with_pi()))
}

/// Both apps honor `PI_CODING_AGENT_SESSION_DIR`, so one store can hold both.
/// There only files opening with Oh My Pi's title slot are its own; the rest
/// are Pi's and stay with the Pi importer, or they would be counted twice.
fn own_files(files: Vec<PathBuf>, shared_with_pi: bool) -> Vec<PathBuf> {
    if !shared_with_pi {
        return files;
    }
    files
        .into_iter()
        .filter(|file| opens_with_title_slot(file))
        .collect()
}

fn shares_root_with_pi() -> bool {
    let canonical = |root: PathBuf| fs::canonicalize(&root).unwrap_or(root);
    let pi_roots: Vec<PathBuf> = pi::session_roots().into_iter().map(canonical).collect();
    omp::session_roots()
        .into_iter()
        .map(canonical)
        .any(|root| pi_roots.contains(&root))
}

fn opens_with_title_slot(file: &Path) -> bool {
    let Ok(handle) = File::open(file) else {
        return false;
    };
    let mut line = String::new();
    if BufReader::new(handle.take(TITLE_SLOT_PROBE_BYTES))
        .read_line(&mut line)
        .is_err()
    {
        return false;
    }
    serde_json::from_str::<Value>(line.trim())
        .is_ok_and(|value| value.get("type").and_then(Value::as_str) == Some("title"))
}

/// Import usage from every Oh My Pi session file under the session browser's
/// current root and layout rules, including nested subagent sessions.
pub(crate) fn sync_omp_usage(
    db: &Database,
    cursors: &crate::services::session_usage::SyncCursors,
) -> Result<SessionSyncResult, AppError> {
    super::session_usage_pi::sync_flavor_with_cursors(db, &OMP, cursors)
}

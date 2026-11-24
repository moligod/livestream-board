// Adapted from CC Switch v4.0.40 (MIT; Copyright (c) 2025 Jason Young).
pub(crate) const MAX_TREE_ENTRIES: usize = 500_000;

pub(crate) const MAX_SESSION_BYTES: u64 = 128 * 1024 * 1024;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SessionLayout {
    Flat,
    ProjectDirectories,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum SessionRootResolution {
    Available {
        root: PathBuf,
        layout: SessionLayout,
    },
    RequiresProjectContext {
        configured_path: String,
    },
    Unavailable {
        reason: String,
    },
}
pub fn session_roots() -> Vec<PathBuf> {
    session_roots_for(&PI)
}

pub(crate) fn session_roots_for(d: &Dialect) -> Vec<PathBuf> {
    match (d.resolve_root)() {
        SessionRootResolution::Available { root, .. } => vec![root],
        SessionRootResolution::RequiresProjectContext { .. }
        | SessionRootResolution::Unavailable { .. } => Vec::new(),
    }
}
pub(crate) fn session_files() -> Result<Vec<PathBuf>, String> {
    session_files_for(&PI)
}

/// Same discovery rules as [`session_files`], for a Pi-family dialect.
pub(crate) fn session_files_for(d: &Dialect) -> Result<Vec<PathBuf>, String> {
    session_files_from_resolution(d, (d.resolve_root)())
}

fn session_files_from_resolution(
    d: &Dialect,
    resolution: SessionRootResolution,
) -> Result<Vec<PathBuf>, String> {
    match resolution {
        SessionRootResolution::Available { root, layout } => {
            let mut files = Vec::new();
            collect_jsonl_files(&root, layout, &mut files, false);
            let mut children = Vec::new();
            for session in &files {
                collect_child_session_files(&session.with_extension(""), &mut children);
            }
            files.extend(children);
            files.sort();
            Ok(files)
        }
        SessionRootResolution::RequiresProjectContext { configured_path } => Err(format!(
            "{} sessionDir '{configured_path}' requires a project cwd and cannot be globally enumerated",
            d.label
        )),
        SessionRootResolution::Unavailable { reason } => Err(reason),
    }
}
fn resolve_session_root() -> SessionRootResolution {
    let home = crate::config::get_home_dir();
    if let Some(raw) = std::env::var_os("PI_CODING_AGENT_SESSION_DIR") {
        if !raw.is_empty() {
            return classify_configured_session_dir(
                raw.to_string_lossy().as_ref(),
                &home,
                "environment",
            );
        }
    }

    match crate::pi_config::read_pi_native_defaults() {
        Ok(defaults) => {
            if let Some(value) = defaults.session_dir.filter(|value| !value.is_empty()) {
                return classify_configured_session_dir(&value, &home, "settings");
            }
        }
        Err(error) => {
            return SessionRootResolution::Unavailable {
                reason: error.to_string(),
            };
        }
    }

    match crate::pi_config::get_pi_agent_dir() {
        Ok(agent_dir) => SessionRootResolution::Available {
            root: agent_dir.join("sessions"),
            layout: SessionLayout::ProjectDirectories,
        },
        Err(error) => SessionRootResolution::Unavailable {
            reason: error.to_string(),
        },
    }
}

fn classify_configured_session_dir(
    value: &str,
    home: &Path,
    source: &'static str,
) -> SessionRootResolution {
    match resolve_global_session_dir(value, home) {
        Some(root) => match fs::metadata(&root) {
            Ok(metadata) if !metadata.is_dir() => SessionRootResolution::Unavailable {
                reason: format!(
                    "Configured Pi session directory from {source} is not a directory: {}",
                    root.display()
                ),
            },
            Ok(_) => match fs::read_dir(&root) {
                Ok(_) => SessionRootResolution::Available {
                    root,
                    layout: SessionLayout::Flat,
                },
                Err(error) => SessionRootResolution::Unavailable {
                    reason: format!(
                        "Configured Pi session directory from {source} is not readable ({}): {error}",
                        root.display()
                    ),
                },
            },
            Err(error) => SessionRootResolution::Unavailable {
                reason: format!(
                    "Configured Pi session directory from {source} is unavailable ({}): {error}",
                    root.display()
                ),
            },
        },
        None => SessionRootResolution::RequiresProjectContext {
            configured_path: value.to_string(),
        },
    }
}

fn resolve_global_session_dir(value: &str, home: &Path) -> Option<PathBuf> {
    let path = if value == "~" {
        home.to_path_buf()
    } else if let Some(suffix) = value
        .strip_prefix("~/")
        .or_else(|| value.strip_prefix("~\\"))
    {
        home.join(suffix)
    } else {
        PathBuf::from(value)
    };
    path.is_absolute().then_some(path)
}
pub(crate) fn is_valid_tree_id(id: &str) -> bool {
    let bytes = id.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= MAX_TREE_ID_BYTES
        && bytes.first().is_some_and(u8::is_ascii_alphanumeric)
        && bytes.last().is_some_and(u8::is_ascii_alphanumeric)
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}
fn collect_child_session_files(dir: &Path, output: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            if entry.file_name() != "subagent-artifacts" {
                collect_child_session_files(&entry.path(), output);
            }
        } else if file_type.is_file() {
            // 不限大小时 push_jsonl_file 不读元数据，扫描状态用不上
            if entry.path().extension().is_some_and(|e| e == "jsonl") { output.push(entry.path()); }
        }
    }
}

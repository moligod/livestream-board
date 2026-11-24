// Adapted from CC Switch v4.0.40 (MIT; Copyright (c) 2025 Jason Young).
/// omp 与 Pi 共用 `PI_CODING_AGENT_SESSION_DIR`（上游行为）；未设置时按 profile 放在数据目录下。
fn resolve_session_root() -> SessionRootResolution {
    if let Some(raw) = std::env::var_os("PI_CODING_AGENT_SESSION_DIR").filter(|raw| !raw.is_empty())
    {
        let root = PathBuf::from(&raw);
        return if root.is_absolute() {
            SessionRootResolution::Available {
                root,
                layout: SessionLayout::Flat,
            }
        } else {
            SessionRootResolution::RequiresProjectContext {
                configured_path: raw.to_string_lossy().into_owned(),
            }
        };
    }
    match crate::omp_config::get_omp_data_dir() {
        Ok(dir) => SessionRootResolution::Available {
            root: dir.join("sessions"),
            layout: SessionLayout::ProjectDirectories,
        },
        Err(error) => SessionRootResolution::Unavailable {
            reason: error.to_string(),
        },
    }
}

pub fn session_roots() -> Vec<PathBuf> {
    pi::session_roots_for(&OMP)
}

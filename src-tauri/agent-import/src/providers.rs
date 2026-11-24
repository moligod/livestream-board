pub(crate) mod agy;
pub(crate) mod gemini;
pub(crate) mod workbuddy;

pub(crate) mod grokbuild {
    pub fn session_roots() -> Vec<std::path::PathBuf> {
        let root = crate::env_path("GROK_HOME")
            .unwrap_or_else(|| crate::config::get_home_dir().join(".grok"));
        vec![root.join("sessions"), root.join("archived_sessions")]
    }
}
pub(crate) mod mcode {
    use std::path::PathBuf;
    pub fn database_path() -> PathBuf {
        crate::mcode_config::data_dir().join("v2/sqlite/runtime-state.sqlite")
    }
    pub fn open_database() -> rusqlite::Result<rusqlite::Connection> {
        rusqlite::Connection::open_with_flags(
            database_path(),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
    }
}

pub(crate) mod pi {
    use std::{
        fs,
        path::{Path, PathBuf},
    };
    #[derive(Clone, Copy)]
    pub struct Dialect {
        pub label: &'static str,
        pub resolve_root: fn() -> SessionRootResolution,
    }
    const PI: Dialect = Dialect {
        label: "Pi",
        resolve_root: resolve_session_root,
    };
    const MAX_TREE_ID_BYTES: usize = 256;
    fn collect_jsonl_files(root: &Path, layout: SessionLayout, output: &mut Vec<PathBuf>, _: bool) {
        fn files(dir: &Path, out: &mut Vec<PathBuf>) {
            for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
                if entry.file_type().is_ok_and(|t| t.is_file())
                    && entry.path().extension().is_some_and(|e| e == "jsonl")
                {
                    out.push(entry.path());
                }
            }
        }
        match layout {
            SessionLayout::Flat => files(root, output),
            SessionLayout::ProjectDirectories => {
                for entry in fs::read_dir(root).into_iter().flatten().flatten() {
                    if entry.file_type().is_ok_and(|t| t.is_dir()) {
                        files(&entry.path(), output);
                    }
                }
            }
        }
    }
    include!("providers/pi_slice.rs");
}
pub(crate) mod omp {
    use super::pi::{self, Dialect, SessionLayout, SessionRootResolution};
    use std::path::PathBuf;
    pub(crate) const OMP: Dialect = Dialect {
        label: "Oh My Pi",
        resolve_root: resolve_session_root,
    };
    include!("providers/omp_slice.rs");
}

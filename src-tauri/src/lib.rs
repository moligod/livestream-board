#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
// MSVC reports successful import-library creation on stdout. Rust surfaces
// that informational line as `linker_messages`, even though linking succeeded.
#![cfg_attr(target_env = "msvc", allow(linker_messages))]

mod collectors;
mod codex_rebuild;
mod commands;
mod models;
mod session_collectors;
mod trae_crypto;

pub fn run() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            commands::load_dashboard,
            commands::rebuild_codex_usage,
            commands::export_usage
        ])
        .run(tauri::generate_context!())
        .expect("error while running TokenUsage application");
}

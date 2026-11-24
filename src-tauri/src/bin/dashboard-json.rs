#[path = "../collectors.rs"]
mod collectors;
#[path = "../codex_rebuild.rs"]
mod codex_rebuild;
#[path = "../models.rs"]
mod models;
#[path = "../session_collectors.rs"]
mod session_collectors;
#[path = "../trae_crypto.rs"]
mod trae_crypto;

use models::{DashboardData, SourceErrors, SourcePaths, SourceStatus, UsageRecord};
use std::collections::HashSet;

fn collect_source<F>(
    path: &std::path::PathBuf,
    collector: F,
) -> (Vec<UsageRecord>, String, Option<String>)
where
    F: FnOnce() -> Result<Vec<UsageRecord>, String>,
{
    if !path.exists() {
        return (Vec::new(), "missing".into(), None);
    }
    match collector() {
        Ok(records) => (records, "ready".into(), None),
        Err(error) => {
            let status = if error.contains("权限") || error.contains("管理员") {
                "needs-permission"
            } else {
                "error"
            };
            (Vec::new(), status.into(), Some(error))
        }
    }
}

fn main() -> Result<(), String> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let selected = args
        .windows(2)
        .find(|pair| pair[0] == "--sources")
        .map(|pair| pair[1].split(',').map(str::to_string).collect::<HashSet<_>>());
    let wanted = |source: &str| selected.as_ref().is_none_or(|items| items.contains(source));
    if args.iter().any(|arg| arg == "--source-fingerprints") {
        let fingerprints = tokenusage_agent_import::SOURCES
            .into_iter()
            .filter(|source| wanted(source))
            .map(|source| (source, tokenusage_agent_import::fingerprint(source)))
            .collect::<std::collections::BTreeMap<_, _>>();
        println!("{}", serde_json::to_string(&fingerprints).map_err(|e| e.to_string())?);
        return Ok(());
    }
    let rebuild_codex = args.iter().any(|argument| argument == "--rebuild-codex");
    let changes_only = args.iter().any(|argument| argument == "--changes-only");
    let (mut codex_path, cn_path, trae_path, mut opencode_path) = collectors::default_paths();
    if let Some(pair) = args.windows(2).find(|pair| pair[0] == "--codex-root") { codex_path = pair[1].clone().into(); }
    if let Some(pair) = args.windows(2).find(|pair| pair[0] == "--opencode-db") { opencode_path = pair[1].clone().into(); }
    let claude_path = collectors::claude_path();
    let (codex, codex_status, codex_error) = if wanted("codex") {
        match collectors::collect_codex_with_options(&codex_path, rebuild_codex, changes_only) {
            Ok((records, warning)) => (records, if codex_path.exists() { "ready" } else { "missing" }.into(), warning),
            Err(error) => (Vec::new(), "error".into(), Some(error)),
        }
    } else { (Vec::new(), "skipped".into(), None) };
    let (claude, claude_status, claude_error) = if wanted("claude") {
        match collectors::collect_claude_with_options(&claude_path, changes_only) {
            Ok((records, warning)) => (records, if claude_path.exists() { "ready" } else { "missing" }.into(), warning),
            Err(error) => (Vec::new(), "error".into(), Some(error)),
        }
    } else { (Vec::new(), "skipped".into(), None) };
    let (cn, cn_status, cn_error) = if wanted("trae-cn") {
        collect_source(&cn_path, || collectors::collect_trae(&cn_path, "trae-cn"))
    } else { (Vec::new(), "skipped".into(), None) };
    let (trae, trae_status, trae_error) = if wanted("trae") {
        collect_source(&trae_path, || collectors::collect_trae(&trae_path, "trae"))
    } else { (Vec::new(), "skipped".into(), None) };
    let (opencode, opencode_status, opencode_error) = if wanted("opencode") {
        collect_source(&opencode_path, || collectors::collect_opencode(&opencode_path))
    } else { (Vec::new(), "skipped".into(), None) };
    let mut records = codex;
    records.extend(claude);
    records.extend(cn);
    records.extend(trae);
    records.extend(opencode);
    let mut dashboard = DashboardData {
        records,
        scanned_at: collectors::now(),
        source_status: SourceStatus {
            agents: Default::default(),
            codex: codex_status,
            claude: claude_status,
            trae_cn: cn_status,
            trae: trae_status,
            opencode: opencode_status,
        },
        source_paths: SourcePaths {
            agents: Default::default(),
            codex: codex_path.display().to_string(),
            claude: claude_path.display().to_string(),
            trae_cn: cn_path.display().to_string(),
            trae: trae_path.display().to_string(),
            opencode: opencode_path.display().to_string(),
        },
        source_errors: SourceErrors {
            agents: Default::default(),
            codex: codex_error,
            claude: claude_error,
            trae_cn: cn_error,
            trae: trae_error,
            opencode: opencode_error,
        },
    };
    dashboard.add_agent_sources(wanted);
    println!(
        "{}",
        serde_json::to_string(&dashboard).map_err(|error| error.to_string())?
    );
    Ok(())
}

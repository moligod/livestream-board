use crate::{
    collectors,
    models::{DashboardData, SourceErrors, SourcePaths, SourceStatus, UsageRecord},
};
use std::{
    fs,
    path::PathBuf,
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

static DASHBOARD_CACHE: OnceLock<Mutex<Option<(Instant, DashboardData)>>> = OnceLock::new();
static DASHBOARD_SCAN_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
const CACHE_TTL: Duration = Duration::from_secs(15);
// Snapshot schema is independent from the Codex parser cache version. A parser
// upgrade must not blank the last known dashboard while the new scan rebuilds.
const DASHBOARD_SNAPSHOT_VERSION: u32 = 2;

#[derive(serde::Serialize, serde::Deserialize)]
struct DashboardSnapshot {
    version: u32,
    dashboard: DashboardData,
}

#[tauri::command]
pub async fn load_dashboard() -> Result<DashboardData, String> {
    if let Some(cached) = recent_dashboard()? {
        return Ok(cached);
    }
    if let Some(snapshot) = load_dashboard_snapshot()? {
        if let Ok(mut cache) = DASHBOARD_CACHE.get_or_init(|| Mutex::new(None)).lock() {
            *cache = Some((Instant::now(), snapshot.clone()));
        }
        return Ok(snapshot);
    }
    tauri::async_runtime::spawn_blocking(|| {
        let _scan = DASHBOARD_SCAN_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .map_err(|error| error.to_string())?;
        if let Some(cached) = recent_dashboard()? {
            return Ok(cached);
        }
        load_dashboard_sync()
    })
    .await
    .map_err(|error| error.to_string())?
}

fn recent_dashboard() -> Result<Option<DashboardData>, String> {
    Ok(DASHBOARD_CACHE
        .get_or_init(|| Mutex::new(None))
        .lock()
        .map_err(|error| error.to_string())?
        .as_ref()
        .filter(|(created, _)| created.elapsed() < CACHE_TTL)
        .map(|(_, dashboard)| dashboard.clone()))
}

fn load_dashboard_sync() -> Result<DashboardData, String> {
    let (codex_path, cn_path, trae_path, opencode_path) = collectors::default_paths();
    let claude_path = collectors::claude_path();
    let mut records = Vec::<UsageRecord>::new();
    let (codex, codex_status, codex_error) =
        collect_source(&codex_path, || collectors::collect_codex(&codex_path));
    let (claude, claude_status, claude_error) =
        collect_source(&claude_path, || collectors::collect_claude(&claude_path));
    let (cn, cn_status, cn_error) =
        collect_source(&cn_path, || collectors::collect_trae(&cn_path, "trae-cn"));
    let (trae, trae_status, trae_error) =
        collect_source(&trae_path, || collectors::collect_trae(&trae_path, "trae"));
    let (opencode, opencode_status, opencode_error) = collect_source(&opencode_path, || {
        collectors::collect_opencode(&opencode_path)
    });
    records.extend(codex.iter().cloned());
    records.extend(claude.iter().cloned());
    records.extend(cn.iter().cloned());
    records.extend(trae.iter().cloned());
    records.extend(opencode.iter().cloned());
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
    dashboard.add_agent_sources(|_| true);
    if let Ok(mut cache) = DASHBOARD_CACHE.get_or_init(|| Mutex::new(None)).lock() {
        *cache = Some((Instant::now(), dashboard.clone()));
    }
    save_dashboard_snapshot(&dashboard);
    Ok(dashboard)
}

fn load_dashboard_snapshot() -> Result<Option<DashboardData>, String> {
    let path = collectors::dashboard_snapshot_path();
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(_) => return Ok(None),
    };
    if let Some(snapshot) = serde_json::from_slice::<DashboardSnapshot>(&bytes)
        .ok()
        .filter(|snapshot| snapshot.version == DASHBOARD_SNAPSHOT_VERSION)
        .map(|snapshot| snapshot.dashboard)
    {
        return Ok(Some(snapshot));
    }
    // Compatibility with the first snapshot format written before the
    // versioned wrapper was introduced.
    Ok(serde_json::from_slice::<DashboardData>(&bytes).ok())
}

fn save_dashboard_snapshot(dashboard: &DashboardData) {
    let path = collectors::dashboard_snapshot_path();
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let snapshot = DashboardSnapshot {
        version: DASHBOARD_SNAPSHOT_VERSION,
        dashboard: dashboard.clone(),
    };
    if let Ok(bytes) = serde_json::to_vec(&snapshot) {
        let _ = fs::write(path, bytes);
    }
}

fn collect_source<F>(path: &PathBuf, collector: F) -> (Vec<UsageRecord>, String, Option<String>)
where
    F: FnOnce() -> Result<Vec<UsageRecord>, String>,
{
    if !path.exists() {
        return (Vec::new(), "missing".into(), None);
    }
    match collector() {
        Ok(records) => (records, "ready".into(), None),
        Err(error) => {
            eprintln!("TokenUsage: failed to read {}: {}", path.display(), error);
            let status = if error.contains("权限") || error.contains("管理员") {
                "needs-permission"
            } else {
                "error"
            };
            (Vec::new(), status.into(), Some(error))
        }
    }
}

#[tauri::command]
pub async fn rebuild_codex_usage() -> Result<DashboardData, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let _scan = DASHBOARD_SCAN_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .map_err(|error| error.to_string())?;
        let (codex_path, _, _, _) = collectors::default_paths();
        collectors::rebuild_codex(&codex_path)?;
        load_dashboard_sync()
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
pub async fn export_usage(format: String) -> Result<String, String> {
    let dashboard = load_dashboard().await?;
    let export_dir = dirs::download_dir().unwrap_or_else(|| PathBuf::from("."));
    let extension = if format == "json" { "json" } else { "csv" };
    let path = export_dir.join(format!(
        "tokenusage-export-{}.{}",
        collectors::now(),
        extension
    ));
    if extension == "json" {
        fs::write(
            &path,
            serde_json::to_vec_pretty(&dashboard.records).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
    } else {
        let mut writer = csv::Writer::from_path(&path).map_err(|e| e.to_string())?;
        for record in dashboard.records {
            writer.serialize(record).map_err(|e| e.to_string())?;
        }
        writer.flush().map_err(|e| e.to_string())?;
    }
    Ok(path.display().to_string())
}

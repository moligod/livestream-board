// Adapted from CC Switch v4.0.40 (MIT; Copyright (c) 2025 Jason Young).
use crate::{config::get_home_dir, error::AppError};
use std::path::{Path, PathBuf};
pub(crate) fn get_omp_active_profile() -> Result<Option<String>, AppError> {
    let selected = std::env::var_os("OMP_PROFILE").or_else(|| std::env::var_os("PI_PROFILE"));
    normalize_profile(
        selected
            .as_deref()
            .map(|name| name.to_string_lossy())
            .as_deref(),
    )
}

fn normalize_profile(raw: Option<&str>) -> Result<Option<String>, AppError> {
    let name = raw.unwrap_or_default().trim();
    if name.is_empty() || name == "default" {
        return Ok(None);
    }
    let reserved = name
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();
    let reserved_device = matches!(reserved.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (reserved.len() == 4
            && (reserved.starts_with("COM") || reserved.starts_with("LPT"))
            && reserved.as_bytes()[3].is_ascii_digit());
    if name.len() > 64
        || !name.as_bytes()[0].is_ascii_lowercase() && !name.as_bytes()[0].is_ascii_digit()
        || !name
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || b"._-".contains(&c))
        || name.ends_with('.')
        || reserved_device
    {
        return Err(AppError::Config(format!(
            "Invalid Oh My Pi profile: {name}"
        )));
    }
    Ok(Some(name.to_string()))
}

fn native_agent_dir(
    home: &Path,
    config_dir: Option<&str>,
    profile: Option<&str>,
    inherited_profile: Option<&str>,
    agent_dir: Option<&Path>,
) -> Result<PathBuf, AppError> {
    // PI_CONFIG_DIR is a directory name relative to home, as in omp's path.join(home, name).
    let config_dir = config_dir.filter(|name| !name.is_empty()).unwrap_or(".omp");
    let root = home.join(config_dir.trim_start_matches(std::path::MAIN_SEPARATOR));
    if let Some(profile) = profile {
        return Ok(root.join("profiles").join(profile).join("agent"));
    }
    // A parent omp process may have exported its named profile directory.
    // Explicit default mode must ignore that derived override.
    let agent_dir = agent_dir.filter(|agent| {
        inherited_profile
            .is_none_or(|profile| *agent != root.join("profiles").join(profile).join("agent"))
    });
    if let Some(agent) = agent_dir.filter(|path| !path.as_os_str().is_empty()) {
        return if agent.is_absolute() {
            Ok(agent.to_path_buf())
        } else {
            std::env::current_dir()
                .map(|cwd| cwd.join(agent))
                .map_err(|error| {
                    AppError::Config(format!("Cannot resolve Oh My Pi agent directory: {error}"))
                })
        };
    }
    Ok(root.join("agent"))
}

pub(crate) fn get_omp_native_agent_dir() -> Result<PathBuf, AppError> {
    let profile = get_omp_active_profile()?;
    let config_dir = std::env::var("PI_CONFIG_DIR").ok();
    let agent_dir = std::env::var_os("PI_CODING_AGENT_DIR").map(PathBuf::from);
    let inherited_profile = normalize_profile(std::env::var("PI_PROFILE").ok().as_deref())
        .ok()
        .flatten();
    native_agent_dir(
        &get_home_dir(),
        config_dir.as_deref(),
        profile.as_deref(),
        inherited_profile.as_deref(),
        agent_dir.as_deref(),
    )
}

pub(crate) fn get_omp_data_dir() -> Result<PathBuf, AppError> {
    let agent = get_omp_native_agent_dir()?;
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        let profile = get_omp_active_profile()?;
        let config_dir = std::env::var("PI_CONFIG_DIR").ok();
        let default_agent = native_agent_dir(
            &get_home_dir(),
            config_dir.as_deref(),
            profile.as_deref(),
            None,
            None,
        )?;
        let xdg = std::env::var_os("XDG_DATA_HOME")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from);
        Ok(data_dir(
            &agent,
            &default_agent,
            profile.as_deref(),
            xdg.as_deref(),
        ))
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    Ok(agent)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn data_dir(
    agent: &Path,
    default_agent: &Path,
    profile: Option<&str>,
    xdg: Option<&Path>,
) -> PathBuf {
    if agent == default_agent {
        if let Some(xdg) = xdg {
            let mut root = xdg.join("omp");
            if let Some(profile) = profile {
                root = root.join("profiles").join(profile);
            }
            if root.exists() {
                return root;
            }
        }
    }
    agent.to_path_buf()
}

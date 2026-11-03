//! Every path the tool touches, resolved in one place.
//!
//! The XDG variables are honoured **on every platform**, not just Linux. The
//! platform default (`~/Library/Application Support` on macOS, `%APPDATA%` on
//! Windows) is a fallback, not an override: a tool that ignores `XDG_DATA_HOME`
//! when the user set it is a tool you cannot point at a scratch directory — which
//! is also what makes the integration tests able to sandbox it.

use std::ffi::OsString;
use std::path::PathBuf;

use thiserror::Error;

/// Must match the `ProjectDirs` application name, because on Linux the platform
/// path already ends with it and the XDG path has to be made to match.
const PROJECT: &str = "libretto";

#[derive(Debug, Error)]
pub enum PathError {
    #[error("cannot determine a home directory; set XDG_DATA_HOME")]
    NoHome,
}

/// `$XDG_DATA_HOME/libretto` — the library file lives here.
pub fn data_dir() -> Result<PathBuf, PathError> {
    resolve(std::env::var_os("XDG_DATA_HOME"), || {
        Ok(project_dirs()?.data_dir().to_path_buf())
    })
}

/// `$XDG_CACHE_HOME/libretto` — synthesized audio, safe to delete at any time.
pub fn cache_dir() -> Result<PathBuf, PathError> {
    resolve(std::env::var_os("XDG_CACHE_HOME"), || {
        Ok(project_dirs()?.cache_dir().to_path_buf())
    })
}

/// `$XDG_CONFIG_HOME/libretto` — user preferences (voice, endpoint, speed).
pub fn config_dir() -> Result<PathBuf, PathError> {
    resolve(std::env::var_os("XDG_CONFIG_HOME"), || {
        Ok(project_dirs()?.config_dir().to_path_buf())
    })
}

/// An explicitly set, non-empty XDG variable wins; otherwise the platform default.
/// Split out so the rule is testable without touching the process environment,
/// which is process-wide state and therefore racy under a parallel test run.
fn resolve(
    xdg: Option<OsString>,
    platform: impl FnOnce() -> Result<PathBuf, PathError>,
) -> Result<PathBuf, PathError> {
    match xdg.filter(|value| !value.is_empty()) {
        Some(dir) => Ok(PathBuf::from(dir).join(PROJECT)),
        None => platform(),
    }
}

fn project_dirs() -> Result<directories::ProjectDirs, PathError> {
    directories::ProjectDirs::from("", "", PROJECT).ok_or(PathError::NoHome)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn platform_default() -> Result<PathBuf, PathError> {
        Ok(PathBuf::from("/platform/default/libretto"))
    }

    #[test]
    fn an_explicit_xdg_variable_wins_everywhere() {
        let resolved =
            resolve(Some(OsString::from("/scratch/data")), platform_default).expect("resolves");
        assert_eq!(resolved, PathBuf::from("/scratch/data/libretto"));
    }

    #[test]
    fn an_empty_xdg_variable_is_not_an_override() {
        let resolved = resolve(Some(OsString::new()), platform_default).expect("resolves");
        assert_eq!(resolved, PathBuf::from("/platform/default/libretto"));
    }

    #[test]
    fn without_the_variable_the_platform_default_is_used() {
        let resolved = resolve(None, platform_default).expect("resolves");
        assert_eq!(resolved, PathBuf::from("/platform/default/libretto"));
    }

    #[test]
    fn the_real_paths_end_with_the_project_name() {
        // Whichever branch this machine takes, the last segment is the project.
        if let Ok(dir) = data_dir() {
            assert!(dir.ends_with(PROJECT), "{dir:?}");
        }
        if let Ok(dir) = cache_dir() {
            assert!(dir.ends_with(PROJECT), "{dir:?}");
        }
        if let Ok(dir) = config_dir() {
            assert!(dir.ends_with(PROJECT), "{dir:?}");
        }
    }
}

//! Every path the tool touches, resolved in one place.
//!
//! One owner means a test can point `XDG_DATA_HOME` at a temp dir and the whole
//! binary is contained: no code path reaches `~/.local/share` on its own.

use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum PathError {
    #[error("cannot determine a home directory; set XDG_DATA_HOME")]
    NoHome,
}

/// `$XDG_DATA_HOME/libretto` — the library file lives here.
pub fn data_dir() -> Result<PathBuf, PathError> {
    Ok(project_dirs()?.data_dir().to_path_buf())
}

/// `$XDG_CACHE_HOME/libretto` — synthesized audio, safe to delete at any time.
pub fn cache_dir() -> Result<PathBuf, PathError> {
    Ok(project_dirs()?.cache_dir().to_path_buf())
}

/// `$XDG_CONFIG_HOME/libretto` — user preferences (voice, endpoint, speed).
pub fn config_dir() -> Result<PathBuf, PathError> {
    Ok(project_dirs()?.config_dir().to_path_buf())
}

fn project_dirs() -> Result<directories::ProjectDirs, PathError> {
    directories::ProjectDirs::from("", "", "libretto").ok_or(PathError::NoHome)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_end_with_the_project_name() {
        // The test harness sets XDG vars for the whole process; assert only the
        // shape, since the temp dir itself differs run to run.
        if let Ok(dir) = data_dir() {
            assert!(dir.ends_with("libretto"), "{dir:?}");
        }
        if let Ok(dir) = cache_dir() {
            assert!(dir.ends_with("libretto"), "{dir:?}");
        }
    }
}

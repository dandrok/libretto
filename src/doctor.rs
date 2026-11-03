//! `libretto doctor`: the five things that make a reader silent, checked in one
//! command — library readable, config found, player present, endpoint answering,
//! cache sane. Every check reports what it actually saw.

use std::path::Path;
use std::process::Command;
use std::time::Duration;

use crate::cache::Cache;
use crate::config::{Settings, config_path};
use crate::paths;
use crate::store::Store;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    pub label: String,
    pub detail: String,
    pub ok: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub checks: Vec<Check>,
}

impl Report {
    pub fn all_ok(&self) -> bool {
        self.checks.iter().all(|check| check.ok)
    }

    /// `label: detail` lines, prefixed so a failed check is visible at a glance.
    pub fn lines(&self) -> Vec<String> {
        self.checks
            .iter()
            .map(|check| {
                format!(
                    "{} {:<14} {}",
                    if check.ok { "ok  " } else { "FAIL" },
                    check.label,
                    check.detail
                )
            })
            .collect()
    }
}

pub fn run(settings: &Settings) -> Report {
    Report {
        checks: vec![
            check_library(),
            check_config(),
            check_player(&settings.player),
            check_endpoint(&settings.endpoint),
            check_cache(),
        ],
    }
}

fn check_library() -> Check {
    let label = "library".to_string();
    let store = match Store::open() {
        Ok(store) => store,
        Err(error) => {
            return Check {
                label,
                detail: error.to_string(),
                ok: false,
            };
        }
    };
    match store.load() {
        Ok(library) if library.books.is_empty() => Check {
            label,
            detail: format!("{} (no books yet)", store.library_path().display()),
            ok: true,
        },
        Ok(library) => Check {
            label,
            detail: format!(
                "{} book(s) in {}",
                library.books.len(),
                store.library_path().display()
            ),
            ok: true,
        },
        Err(error) => Check {
            label,
            detail: error.to_string(),
            ok: false,
        },
    }
}

fn check_config() -> Check {
    let label = "config".to_string();
    match config_path() {
        Ok(path) => {
            let exists = Path::new(&path).is_file();
            Check {
                label,
                detail: format!(
                    "{} ({})",
                    path.display(),
                    if exists {
                        "found"
                    } else {
                        "absent, defaults in use"
                    }
                ),
                // A missing config file is normal, not a failure.
                ok: true,
            }
        }
        Err(error) => Check {
            label,
            detail: error.to_string(),
            ok: false,
        },
    }
}

fn check_player(program: &str) -> Check {
    let label = "player".to_string();
    match Command::new(program).arg("--version").output() {
        Ok(output) if output.status.success() => {
            let first = String::from_utf8_lossy(&output.stdout)
                .lines()
                .next()
                .unwrap_or("")
                .trim()
                .to_string();
            Check {
                label,
                detail: format!("{program} — {first}"),
                ok: true,
            }
        }
        Ok(output) => Check {
            label,
            detail: format!("{program} ran but exited with {}", output.status),
            ok: false,
        },
        Err(error) => Check {
            label,
            detail: format!("{program} not runnable ({error})"),
            ok: false,
        },
    }
}

/// Ask the endpoint for its voice list: a service that answers that is a service
/// that will answer a speech request.
fn check_endpoint(endpoint: &str) -> Check {
    let label = "tts".to_string();
    let url = format!("{}/audio/voices", endpoint.trim_end_matches('/'));
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(5)))
        .build()
        .new_agent();

    match agent.get(&url).call() {
        Ok(mut response) => {
            let body = response
                .body_mut()
                .with_config()
                .limit(1024 * 1024)
                .read_to_vec()
                .unwrap_or_default();
            let voices = serde_json::from_slice::<serde_json::Value>(&body)
                .ok()
                .and_then(|json| {
                    json.get("voices")
                        .and_then(|voices| voices.as_array().map(Vec::len))
                })
                .unwrap_or(0);
            Check {
                label,
                detail: format!("{endpoint} answering ({voices} voices)"),
                ok: true,
            }
        }
        Err(error) => Check {
            label,
            detail: format!("{endpoint} — {error}"),
            ok: false,
        },
    }
}

fn check_cache() -> Check {
    let label = "cache".to_string();
    let root = match Cache::open() {
        Ok(cache) => cache.root().to_path_buf(),
        Err(error) => {
            return Check {
                label,
                detail: error.to_string(),
                ok: false,
            };
        }
    };
    let (files, bytes) = dir_usage(&root);
    Check {
        label,
        detail: format!(
            "{files} file(s) · {:.1} MB · {}",
            bytes as f64 / (1024.0 * 1024.0),
            root.display()
        ),
        // An empty or missing cache is normal: it is rebuilt on demand.
        ok: true,
    }
}

fn dir_usage(path: &Path) -> (usize, u64) {
    fn walk(dir: &Path, files: &mut usize, bytes: &mut u64) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            match entry.file_type() {
                Ok(kind) if kind.is_dir() => walk(&path, files, bytes),
                Ok(kind) if kind.is_file() => {
                    *files += 1;
                    *bytes += entry.metadata().map(|meta| meta.len()).unwrap_or(0);
                }
                _ => {}
            }
        }
    }
    let mut files = 0;
    let mut bytes = 0;
    walk(path, &mut files, &mut bytes);
    (files, bytes)
}

/// Where the cache lives, for the doctor output and for tests.
pub fn cache_root() -> Option<std::path::PathBuf> {
    paths::cache_dir().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_report_is_ok_only_when_every_check_is() {
        let report = Report {
            checks: vec![
                Check {
                    label: "a".into(),
                    detail: "fine".into(),
                    ok: true,
                },
                Check {
                    label: "b".into(),
                    detail: "broken".into(),
                    ok: false,
                },
            ],
        };
        assert!(!report.all_ok());
        let lines = report.lines();
        assert!(lines[0].starts_with("ok  "), "{}", lines[0]);
        assert!(lines[1].starts_with("FAIL"), "{}", lines[1]);
    }

    #[test]
    fn a_missing_directory_is_zero_bytes_not_an_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let missing = dir.path().join("nope");
        assert_eq!(dir_usage(&missing), (0, 0));
    }

    #[test]
    fn usage_counts_files_and_bytes_recursively() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("ab")).expect("dir");
        std::fs::write(dir.path().join("ab/one.mp3"), b"12345").expect("write");
        std::fs::write(dir.path().join("two.mp3"), b"123").expect("write");

        assert_eq!(dir_usage(dir.path()), (2, 8));
    }
}

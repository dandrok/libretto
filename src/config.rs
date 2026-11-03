//! User preferences: one TOML file, every field optional, command-line flags win.
//!
//! Defaults are chosen so `libretto` works with no config file at all against a
//! Kokoro on the default port — the file exists to change the voice, not to make
//! the tool run.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::paths;

pub const DEFAULT_ENDPOINT: &str = "http://127.0.0.1:8880/v1";
pub const DEFAULT_MODEL: &str = "kokoro";
pub const DEFAULT_VOICE: &str = "af_heart";
pub const DEFAULT_SPEED: f32 = 1.0;
pub const DEFAULT_PREFETCH: usize = 5;
pub const DEFAULT_PLAYER: &str = "mpv";

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("cannot read {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("{path} is not valid TOML: {reason}")]
    Parse { path: String, reason: String },
    #[error(transparent)]
    Path(#[from] paths::PathError),
}

/// What is in the file. Everything is optional so a partial file is valid.
/// Unknown keys are refused: a misspelled `voise = "bf_emma"` must not be
/// silently ignored while the default voice is used.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub endpoint: Option<String>,
    pub model: Option<String>,
    pub voice: Option<String>,
    pub speed: Option<f32>,
    pub prefetch: Option<usize>,
    pub player: Option<String>,
}

/// What a run actually uses: file values, then flags, then defaults.
#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    pub endpoint: String,
    pub model: String,
    pub voice: String,
    pub speed: f32,
    pub prefetch: usize,
    pub player: String,
}

impl Settings {
    /// `overrides` are the flags the user actually passed; anything `None` falls
    /// through to the file, then to the default.
    pub fn resolve(config: &Config, overrides: Overrides) -> Self {
        Self {
            endpoint: overrides
                .endpoint
                .or_else(|| config.endpoint.clone())
                .unwrap_or_else(|| DEFAULT_ENDPOINT.to_string()),
            model: overrides
                .model
                .or_else(|| config.model.clone())
                .unwrap_or_else(|| DEFAULT_MODEL.to_string()),
            voice: overrides
                .voice
                .or_else(|| config.voice.clone())
                .unwrap_or_else(|| DEFAULT_VOICE.to_string()),
            speed: overrides
                .speed
                .or(config.speed)
                .filter(|speed| speed.is_finite())
                .unwrap_or(DEFAULT_SPEED)
                .clamp(0.25, 4.0),
            prefetch: overrides
                .prefetch
                .or(config.prefetch)
                .unwrap_or(DEFAULT_PREFETCH)
                .min(64),
            player: overrides
                .player
                .or_else(|| config.player.clone())
                .unwrap_or_else(|| DEFAULT_PLAYER.to_string()),
        }
    }
}

/// Flags from the command line. Same fields, all optional, so "not passed" and
/// "passed the default value" stay distinguishable.
#[derive(Debug, Clone, Default)]
pub struct Overrides {
    pub endpoint: Option<String>,
    pub model: Option<String>,
    pub voice: Option<String>,
    pub speed: Option<f32>,
    pub prefetch: Option<usize>,
    pub player: Option<String>,
}

pub fn config_path() -> Result<PathBuf, ConfigError> {
    Ok(paths::config_dir()?.join("config.toml"))
}

/// A missing file is not an error: it means "use the defaults".
pub fn load() -> Result<Config, ConfigError> {
    let path = config_path()?;
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Config::default()),
        Err(source) => {
            return Err(ConfigError::Io {
                path: path.display().to_string(),
                source,
            });
        }
    };
    toml::from_str(&text).map_err(|error| ConfigError::Parse {
        path: path.display().to_string(),
        reason: error.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_config_and_no_flags_means_defaults() {
        let settings = Settings::resolve(&Config::default(), Overrides::default());
        assert_eq!(settings.endpoint, DEFAULT_ENDPOINT);
        assert_eq!(settings.voice, DEFAULT_VOICE);
        assert_eq!(settings.speed, DEFAULT_SPEED);
        assert_eq!(settings.player, DEFAULT_PLAYER);
    }

    #[test]
    fn a_flag_beats_the_file_which_beats_the_default() {
        let config = Config {
            voice: Some("bf_emma".into()),
            speed: Some(1.5),
            ..Config::default()
        };
        let flags = Overrides {
            voice: Some("am_michael".into()),
            ..Overrides::default()
        };
        let settings = Settings::resolve(&config, flags);

        assert_eq!(settings.voice, "am_michael", "flag wins");
        assert_eq!(settings.speed, 1.5, "file value kept when no flag");
        assert_eq!(settings.endpoint, DEFAULT_ENDPOINT, "default still applies");
    }

    #[test]
    fn absurd_values_are_clamped_rather_than_passed_on() {
        let config = Config {
            speed: Some(99.0),
            prefetch: Some(10_000),
            ..Config::default()
        };
        let settings = Settings::resolve(&config, Overrides::default());
        assert_eq!(settings.speed, 4.0);
        assert_eq!(settings.prefetch, 64);
    }

    #[test]
    fn a_partial_file_parses() {
        let config: Config = toml::from_str("voice = \"bm_george\"\n").expect("partial toml");
        assert_eq!(config.voice.as_deref(), Some("bm_george"));
        assert_eq!(config.speed, None);
    }

    #[test]
    fn a_misspelled_key_is_refused_instead_of_ignored() {
        // `voise` would otherwise parse fine and leave the default voice in
        // place: a silent typo is worse than a broken config.
        let parsed: Result<Config, _> = toml::from_str("voise = \"bf_emma\"\n");
        let error = parsed.expect_err("unknown keys must be refused");
        assert!(
            error.to_string().contains("voise"),
            "names the key: {error}"
        );
    }

    #[test]
    fn a_non_finite_speed_falls_back_instead_of_reaching_the_endpoint() {
        for speed in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let settings = Settings::resolve(
                &Config::default(),
                Overrides {
                    speed: Some(speed),
                    ..Overrides::default()
                },
            );
            assert!(
                settings.speed.is_finite(),
                "speed {speed} must not survive: serde would send null"
            );
            assert_eq!(settings.speed, DEFAULT_SPEED);
        }
    }
}

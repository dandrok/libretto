//! Synthesized audio on disk, addressed by content.
//!
//! The key is a hash of (model, voice, speed, text), so:
//! - replaying a chapter costs zero requests,
//! - changing the voice is a different entry, not a stale hit,
//! - deleting the whole cache is always safe, which is what makes it a cache
//!   and not state.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::paths;
use crate::tts::{SpeechRequest, TtsEngine, TtsError};

#[derive(Debug, Error)]
pub enum CacheError {
    #[error("cannot use the audio cache: {source}")]
    Io {
        #[source]
        source: std::io::Error,
    },
    #[error(transparent)]
    Tts(#[from] TtsError),
    #[error(transparent)]
    Path(#[from] paths::PathError),
}

pub struct Cache {
    root: PathBuf,
}

impl Cache {
    /// The real cache, under `$XDG_CACHE_HOME/libretto/audio`.
    pub fn open() -> Result<Self, CacheError> {
        Ok(Self {
            root: paths::cache_dir()?.join("audio"),
        })
    }

    /// A cache at a chosen directory — what tests use.
    pub fn at(dir: impl Into<PathBuf>) -> Self {
        Self { root: dir.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Content address: every field that changes the audio is in the hash.
    pub fn key(request: &SpeechRequest) -> String {
        blake3::hash(request.fingerprint().as_bytes())
            .to_hex()
            .to_string()
    }

    /// Sharded one level, because a 700-page book is ~10k sentences and one
    /// directory with 10k files is slow to list on some filesystems.
    pub fn path_for(&self, key: &str) -> PathBuf {
        let shard = key.get(0..2).unwrap_or("00");
        self.root.join(shard).join(format!("{key}.mp3"))
    }

    pub fn get(&self, key: &str) -> Option<PathBuf> {
        let path = self.path_for(key);
        path.is_file().then_some(path)
    }

    /// Write the audio where `get` will find it. Temp file + rename, `O_EXCL`,
    /// `0600`: the file is derived from a book, so treat it as private.
    pub fn put(&self, key: &str, bytes: &[u8]) -> Result<PathBuf, CacheError> {
        let path = self.path_for(key);
        let dir = path.parent().unwrap_or(&self.root);
        fs::create_dir_all(dir).map_err(|source| CacheError::Io { source })?;
        restrict_dir(dir)?;

        let temp = dir.join(format!("{key}.mp3.tmp"));
        let mut file = create_exclusive(&temp)?;
        restrict_file(&file)?;
        file.write_all(bytes)
            .and_then(|()| file.sync_all())
            .map_err(|source| CacheError::Io { source })?;
        drop(file);
        fs::rename(&temp, &path).map_err(|source| CacheError::Io { source })?;
        Ok(path)
    }

    /// The one place synthesis and storage meet: return a playable file,
    /// synthesizing only when the key is absent.
    pub fn audio_for(
        &self,
        engine: &dyn TtsEngine,
        request: &SpeechRequest,
    ) -> Result<PathBuf, CacheError> {
        let key = Self::key(request);
        if let Some(hit) = self.get(&key) {
            return Ok(hit);
        }
        let bytes = engine.synthesize(request)?;
        self.put(&key, &bytes)
    }
}

fn create_exclusive(path: &Path) -> Result<fs::File, CacheError> {
    match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
    {
        Ok(file) => Ok(file),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let _ = fs::remove_file(path); // a crashed run's leftover
            fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(path)
                .map_err(|source| CacheError::Io { source })
        }
        Err(source) => Err(CacheError::Io { source }),
    }
}

fn restrict_file(file: &fs::File) -> Result<(), CacheError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|source| CacheError::Io { source })?;
    }
    #[cfg(not(unix))]
    let _ = file;
    Ok(())
}

fn restrict_dir(dir: &Path) -> Result<(), CacheError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
            .map_err(|source| CacheError::Io { source })?;
    }
    #[cfg(not(unix))]
    let _ = dir;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_key_changes_with_every_field_that_changes_the_audio() {
        let base = SpeechRequest::new("one sentence");
        let other_text = SpeechRequest::new("another sentence");
        let other_voice = SpeechRequest::new("one sentence").with_voice("bm_george");
        let other_speed = SpeechRequest::new("one sentence").with_speed(1.5);

        let keys = [
            Cache::key(&base),
            Cache::key(&other_text),
            Cache::key(&other_voice),
            Cache::key(&other_speed),
        ];
        for (i, a) in keys.iter().enumerate() {
            for b in keys.iter().skip(i + 1) {
                assert_ne!(a, b, "each field must change the key");
            }
        }
        assert_eq!(keys[0], Cache::key(&SpeechRequest::new("one sentence")));
    }

    #[test]
    fn speed_is_compared_as_text_so_config_and_flag_agree() {
        let from_flag = SpeechRequest::new("x").with_speed(1.2);
        let from_config = SpeechRequest::new("x").with_speed(1.200);
        assert_eq!(Cache::key(&from_flag), Cache::key(&from_config));
    }

    #[test]
    fn paths_are_sharded_and_end_in_mp3() {
        let cache = Cache::at("/tmp/cache");
        let key = "abcdef0123456789";
        let path = cache.path_for(key);
        assert_eq!(path, PathBuf::from("/tmp/cache/ab/abcdef0123456789.mp3"));
    }
}

//! Speaking one sentence: an OpenAI-compatible `/audio/speech` call, and the
//! trait that keeps Kokoro, a fake and a future cloud engine interchangeable.

use std::time::Duration;

use std::sync::atomic::{AtomicU8, Ordering};

use base64::Engine as _;
use serde::Deserialize;
use thiserror::Error;

use crate::captions::{self, Word};

/// One request for one sentence. The cache key is derived from every field, so
/// changing the voice or the speed is a different piece of audio.
#[derive(Debug, Clone, PartialEq)]
pub struct SpeechRequest {
    pub text: String,
    pub voice: String,
    pub speed: f32,
    pub model: String,
}

impl SpeechRequest {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            voice: "af_heart".to_string(),
            speed: 1.0,
            model: "kokoro".to_string(),
        }
    }

    pub fn with_voice(mut self, voice: impl Into<String>) -> Self {
        self.voice = voice.into();
        self
    }

    pub fn with_speed(mut self, speed: f32) -> Self {
        self.speed = speed;
        self
    }

    /// Stable text for the cache key. Speed is formatted rather than hashed as
    /// bits so `1.2` from a config file and `1.2` from a flag agree.
    pub fn fingerprint(&self) -> String {
        format!(
            "{}\u{1f}{}\u{1f}{:.2}\u{1f}{}",
            self.model, self.voice, self.speed, self.text
        )
    }
}

#[derive(Debug, Error)]
pub enum TtsError {
    #[error("endpoint {endpoint} is not an http(s) URL")]
    BadEndpoint { endpoint: String },
    #[error("cannot reach the TTS endpoint {endpoint}: {reason}")]
    Unreachable { endpoint: String, reason: String },
    #[error("the TTS endpoint {endpoint} answered HTTP {status}")]
    Status { endpoint: String, status: u16 },
    #[error("the TTS endpoint {endpoint} returned no audio")]
    Empty { endpoint: String },
    #[error("the TTS endpoint {endpoint} returned more than {limit_bytes} bytes for one sentence")]
    TooLarge { endpoint: String, limit_bytes: u64 },
}

/// One sentence is tens to hundreds of KB of audio. 32 MiB is a hundred times
/// that, and it bounds what a misbehaving endpoint can make this process
/// allocate — the same rule as a zip entry or a state file.
const MAX_AUDIO_BYTES: u64 = 32 * 1024 * 1024;

/// One sentence's audio, and the engine's own word timings when it can give them.
#[derive(Debug, Clone, PartialEq)]
pub struct Speech {
    pub audio: Vec<u8>,
    /// `None` when the engine has no such thing, or declined to answer this time.
    pub words: Option<Vec<Word>>,
}

/// Anything that can turn a sentence into mp3 bytes.
///
/// `Send + Sync` because the playback loop synthesizes on a worker thread while
/// the main thread plays: engines are shared, not owned per sentence.
pub trait TtsEngine: Send + Sync {
    fn synthesize(&self, request: &SpeechRequest) -> Result<Vec<u8>, TtsError>;

    /// The same sentence, asking for word timings as well. The default is a plain
    /// sentence: word timings are a Kokoro extension, not part of the OpenAI shape every
    /// other server speaks.
    fn synthesize_captioned(&self, request: &SpeechRequest) -> Result<Speech, TtsError> {
        Ok(Speech {
            audio: self.synthesize(request)?,
            words: None,
        })
    }
}

/// Where a captioned sentence is asked for: the same server, a sibling path. Kokoro serves
/// `/v1/audio/speech` and `/dev/captioned_speech`, so a configured `/v1` becomes `/dev`. An
/// endpoint whose last segment is not a version has no sibling we can guess, and the
/// read-along interpolates instead of guessing.
pub fn captioned_url(endpoint: &str) -> Option<String> {
    let (base, last) = endpoint.rsplit_once('/')?;
    let version = last.strip_prefix('v').filter(|digits| !digits.is_empty())?;
    if !version.chars().all(|digit| digit.is_ascii_digit()) {
        return None;
    }
    Some(format!("{base}/dev/captioned_speech"))
}

/// Kokoro-FastAPI (or any server speaking the OpenAI speech shape).
pub struct Kokoro {
    endpoint: String,
    agent: ureq::Agent,
    max_audio_bytes: u64,
    /// Whether captioned speech works here: 0 unknown, 2 no, anything else yes. One
    /// refusal per run is enough — asking 600 times to be told the same 404 is what makes
    /// a feature feel broken.
    captions: AtomicU8,
}

/// What a captioned reply carries. The timings inside the same body are read by
/// [`crate::captions::parse_reply`].
#[derive(Deserialize)]
struct CaptionedReply {
    audio: String,
}

impl Kokoro {
    /// The endpoint is validated here, not at request time: a config typo should
    /// fail once, with the value, rather than per sentence.
    pub fn new(endpoint: impl Into<String>) -> Result<Self, TtsError> {
        Self::with_max_audio_bytes(endpoint, MAX_AUDIO_BYTES)
    }

    /// The ceiling is injectable so a test can exercise the limit with a 2 KiB
    /// body instead of allocating 33 MiB and racing the socket: the property is
    /// "a reply past the limit is refused", not "a big write survives a reset".
    pub fn with_max_audio_bytes(
        endpoint: impl Into<String>,
        max_audio_bytes: u64,
    ) -> Result<Self, TtsError> {
        let endpoint = endpoint.into().trim_end_matches('/').to_string();
        if !(endpoint.starts_with("http://") || endpoint.starts_with("https://")) {
            return Err(TtsError::BadEndpoint { endpoint });
        }
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(120)))
            .build()
            .new_agent();
        Ok(Self {
            endpoint,
            agent,
            max_audio_bytes,
            captions: AtomicU8::new(0),
        })
    }

    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    fn captioned(&self, request: &SpeechRequest) -> Option<Result<Speech, TtsError>> {
        if self.captions.load(Ordering::Relaxed) == 2 {
            return None;
        }
        let url = captioned_url(&self.endpoint)?;
        Some(self.post_captioned(&url, request))
    }

    fn post_captioned(&self, url: &str, request: &SpeechRequest) -> Result<Speech, TtsError> {
        let body = serde_json::json!({
            "model": request.model,
            "input": request.text,
            "voice": request.voice,
            "speed": request.speed,
            "response_format": "mp3",
            "stream": false,
        });
        let mut response = self
            .agent
            .post(url)
            .header("Content-Type", "application/json")
            .send_json(&body)
            .map_err(|error| match error {
                ureq::Error::StatusCode(status) => TtsError::Status {
                    endpoint: self.endpoint.clone(),
                    status,
                },
                other => TtsError::Unreachable {
                    endpoint: self.endpoint.clone(),
                    reason: other.to_string(),
                },
            })?;
        // The audio comes back base64'd inside JSON: a third bigger on the wire, bounded by
        // the same ceiling as a plain reply.
        let bytes = response
            .body_mut()
            .with_config()
            .limit(self.max_audio_bytes.saturating_mul(2))
            .read_to_vec()
            .map_err(|error| TtsError::Unreachable {
                endpoint: self.endpoint.clone(),
                reason: error.to_string(),
            })?;
        let words = captions::parse_reply(&bytes);
        let reply: CaptionedReply =
            serde_json::from_slice(&bytes).map_err(|error| TtsError::Unreachable {
                endpoint: self.endpoint.clone(),
                reason: format!("captioned reply was not JSON: {error}"),
            })?;
        let audio = base64::engine::general_purpose::STANDARD
            .decode(&reply.audio)
            .map_err(|error| TtsError::Unreachable {
                endpoint: self.endpoint.clone(),
                reason: format!("captioned reply carried unusable audio: {error}"),
            })?;
        if audio.is_empty() {
            return Err(TtsError::Empty {
                endpoint: self.endpoint.clone(),
            });
        }
        if audio.len() as u64 > self.max_audio_bytes {
            return Err(TtsError::TooLarge {
                endpoint: self.endpoint.clone(),
                limit_bytes: self.max_audio_bytes,
            });
        }
        Ok(Speech { audio, words })
    }
}

impl TtsEngine for Kokoro {
    /// A refusal on this path means this server has no captioned speech — not that the
    /// endpoint is broken — so the sentence is asked for the plain way and no later
    /// sentence tries again.
    fn synthesize_captioned(&self, request: &SpeechRequest) -> Result<Speech, TtsError> {
        if let Some(attempt) = self.captioned(request) {
            match attempt {
                Ok(speech) => return Ok(speech),
                Err(_) => self.captions.store(2, Ordering::Relaxed),
            }
        }
        Ok(Speech {
            audio: self.synthesize(request)?,
            words: None,
        })
    }

    fn synthesize(&self, request: &SpeechRequest) -> Result<Vec<u8>, TtsError> {
        let url = format!("{}/audio/speech", self.endpoint);
        let body = serde_json::json!({
            "model": request.model,
            "input": request.text,
            "voice": request.voice,
            "speed": request.speed,
            "response_format": "mp3",
        });

        let mut response = self
            .agent
            .post(&url)
            .header("Content-Type", "application/json")
            .send_json(&body)
            .map_err(|error| match error {
                ureq::Error::StatusCode(status) => TtsError::Status {
                    endpoint: self.endpoint.clone(),
                    status,
                },
                other => TtsError::Unreachable {
                    endpoint: self.endpoint.clone(),
                    reason: other.to_string(),
                },
            })?;

        let bytes = response
            .body_mut()
            .with_config()
            .limit(self.max_audio_bytes)
            .read_to_vec()
            .map_err(|error| match error {
                // ureq reports the ceiling it enforced rather than truncating.
                ureq::Error::BodyExceedsLimit(limit_bytes) => TtsError::TooLarge {
                    endpoint: self.endpoint.clone(),
                    limit_bytes,
                },
                other => TtsError::Unreachable {
                    endpoint: self.endpoint.clone(),
                    reason: other.to_string(),
                },
            })?;

        if bytes.len() as u64 > self.max_audio_bytes {
            return Err(TtsError::TooLarge {
                endpoint: self.endpoint.clone(),
                limit_bytes: self.max_audio_bytes,
            });
        }
        if bytes.is_empty() {
            return Err(TtsError::Empty {
                endpoint: self.endpoint.clone(),
            });
        }
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_versioned_endpoint_has_a_captioned_sibling() {
        assert_eq!(
            captioned_url("https://tts.example.com:8880/v1").as_deref(),
            Some("https://tts.example.com:8880/dev/captioned_speech")
        );
        assert_eq!(
            captioned_url("https://tts.example.com/openai/v2").as_deref(),
            Some("https://tts.example.com/openai/dev/captioned_speech")
        );
    }

    #[test]
    fn an_endpoint_we_cannot_guess_is_not_guessed() {
        assert_eq!(
            captioned_url("https://tts.example.com"),
            None,
            "no path at all"
        );
        assert_eq!(
            captioned_url("https://tts.example.com/api"),
            None,
            "not a version"
        );
        assert_eq!(
            captioned_url("https://tts.example.com/v"),
            None,
            "v alone is not one"
        );
    }

    #[test]
    fn a_plain_engine_says_it_has_no_timings() {
        struct Plain;
        impl TtsEngine for Plain {
            fn synthesize(&self, _request: &SpeechRequest) -> Result<Vec<u8>, TtsError> {
                Ok(b"ID3".to_vec())
            }
        }
        let speech = Plain
            .synthesize_captioned(&SpeechRequest::new("x"))
            .expect("plain speech");
        assert_eq!(speech.audio, b"ID3");
        assert_eq!(speech.words, None, "and the caller interpolates instead");
    }
}

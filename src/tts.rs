//! Speaking one sentence: an OpenAI-compatible `/audio/speech` call, and the
//! trait that keeps Kokoro, a fake and a future cloud engine interchangeable.

use std::time::Duration;

use thiserror::Error;

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

/// Anything that can turn a sentence into mp3 bytes.
pub trait TtsEngine {
    fn synthesize(&self, request: &SpeechRequest) -> Result<Vec<u8>, TtsError>;
}

/// Kokoro-FastAPI (or any server speaking the OpenAI speech shape).
pub struct Kokoro {
    endpoint: String,
    agent: ureq::Agent,
    max_audio_bytes: u64,
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
        })
    }

    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }
}

impl TtsEngine for Kokoro {
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

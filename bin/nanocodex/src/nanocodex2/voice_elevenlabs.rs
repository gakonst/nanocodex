//! Local ElevenLabs transport. Credentials never travel through the managed service.
use super::error;
use nanocodex_managed::ManagedError;
use serde::Deserialize;
use std::{path::PathBuf, time::Duration};

#[derive(Clone)]
pub(crate) struct Client {
    endpoint: String,
    http: reqwest::Client,
    key: reqwest::header::HeaderValue,
}
#[derive(Debug, Deserialize)]
pub(crate) struct Voice {
    pub voice_id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub category: String,
    #[serde(default)]
    pub requires_verification: bool,
}
impl Client {
    pub(crate) fn from_env() -> Result<Self, ManagedError> {
        nanocodex::oai::transport::install_default_rustls_crypto_provider();
        let key = std::env::var("ELEVENLABS_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| error("Set ELEVENLABS_API_KEY in your environment"))?;
        let mut key = reqwest::header::HeaderValue::from_str(&key)
            .map_err(|_| error("Invalid ElevenLabs API key format"))?;
        key.set_sensitive(true);
        Ok(Self {
            endpoint: "https://api.elevenlabs.io/v1".into(),
            http: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(Duration::from_secs(10))
                .timeout(Duration::from_secs(90))
                .build()
                .map_err(|_| error("Could not initialize ElevenLabs client"))?,
            key,
        })
    }
    fn request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        self.http
            .request(method, format!("{}/{path}", self.endpoint))
            .header("xi-api-key", self.key.clone())
    }
    async fn checked(request: reqwest::RequestBuilder) -> Result<reqwest::Response, ManagedError> {
        let response = request
            .send()
            .await
            .map_err(|_| error("ElevenLabs request failed (connection or timeout)"))?;
        if !response.status().is_success() {
            return Err(error(format!(
                "ElevenLabs returned HTTP {}",
                response.status().as_u16()
            )));
        }
        Ok(response)
    }
    pub(crate) async fn voices(&self) -> Result<Vec<Voice>, ManagedError> {
        #[derive(Deserialize)]
        struct Catalog {
            voices: Vec<Voice>,
            #[serde(default)]
            has_more: bool,
            next_page_token: Option<String>,
        }
        let mut voices = Vec::new();
        let mut token = None;
        let mut seen = std::collections::HashSet::new();
        for _ in 0..20 {
            let mut request = self
                .http
                .get(format!(
                    "{}/v2/voices",
                    self.endpoint.trim_end_matches("/v1")
                ))
                .header("xi-api-key", self.key.clone())
                .query(&[("page_size", "100")]);
            if let Some(token) = &token {
                request = request.query(&[("next_page_token", token)]);
            }
            let page: Catalog = Self::checked(request)
                .await?
                .json()
                .await
                .map_err(|_| error("Invalid ElevenLabs voice catalog"))?;
            if page.voices.len() > 100 {
                return Err(error("ElevenLabs catalog page exceeded limit"));
            }
            voices.extend(page.voices);
            if !page.has_more {
                return Ok(voices);
            }
            let next = page
                .next_page_token
                .filter(|s| !s.is_empty() && s.len() <= 4096)
                .ok_or_else(|| error("Invalid ElevenLabs catalog pagination"))?;
            if !seen.insert(next.clone()) {
                return Err(error("Repeated ElevenLabs catalog page"));
            }
            token = Some(next);
        }
        Err(error("ElevenLabs catalog exceeds 2000 voices"))
    }
    pub(crate) async fn clone_voice(
        &self,
        name: &str,
        samples: &[PathBuf],
        consent: bool,
    ) -> Result<Voice, ManagedError> {
        if !consent {
            return Err(error(
                "Explicit consent is required before uploading voice samples",
            ));
        }
        if name.trim().is_empty() || name.len() > 128 || samples.is_empty() || samples.len() > 10 {
            return Err(error("Provide a voice name and 1–10 audio sample files"));
        }
        let mut form = reqwest::multipart::Form::new().text("name", name.to_owned());
        let mut total = 0;
        for (index, path) in samples.iter().enumerate() {
            let (extension, mime) = sample_format(path)?;
            let metadata =
                std::fs::metadata(path).map_err(|_| error("Cannot inspect voice sample"))?;
            if !metadata.is_file() {
                return Err(error("Voice samples must be regular audio files"));
            }
            let file = std::fs::File::open(path).map_err(|_| error("Cannot open voice sample"))?;
            use std::io::Read;
            let mut bytes = Vec::new();
            file.take(20 * 1024 * 1024 + 1)
                .read_to_end(&mut bytes)
                .map_err(|_| error("Cannot read voice sample"))?;
            total += bytes.len();
            if bytes.is_empty() || total > 20 * 1024 * 1024 {
                return Err(error(
                    "Voice samples must be nonempty and at most 20 MiB combined",
                ));
            }
            let filename = format!("sample-{}.{}", index + 1, extension);
            form = form.part(
                "files",
                reqwest::multipart::Part::bytes(bytes)
                    .file_name(filename)
                    .mime_str(mime)
                    .map_err(|_| error("Invalid sample audio type"))?,
            );
        }
        let mut voice = Self::checked(
            self.request(reqwest::Method::POST, "voices/add")
                .multipart(form),
        )
        .await?
        .json::<Voice>()
        .await
        .map_err(|_| {
            error("Invalid ElevenLabs clone response; check your voice catalog before retrying")
        })?;
        voice.name = name.to_owned();
        Ok(voice)
    }
    #[cfg(test)]
    pub(super) async fn speech(&self, voice: &str, text: &str) -> Result<Vec<u8>, ManagedError> {
        let mut response = self.speech_response(voice, text, false).await?;
        let mut audio = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| error("ElevenLabs audio download failed"))?
        {
            if audio.len() + chunk.len() > 16 * 1024 * 1024 {
                return Err(error("ElevenLabs audio exceeded 16 MiB"));
            }
            audio.extend_from_slice(&chunk);
        }
        if audio.is_empty() {
            return Err(error("ElevenLabs returned empty audio"));
        }
        Ok(audio)
    }

    /// Raw mono 24 kHz signed 16-bit PCM; response is consumed incrementally.
    pub(super) async fn speech_stream(
        &self,
        voice: &str,
        text: &str,
    ) -> Result<reqwest::Response, ManagedError> {
        self.speech_response(voice, text, true).await
    }

    async fn speech_response(
        &self,
        voice: &str,
        text: &str,
        streaming: bool,
    ) -> Result<reqwest::Response, ManagedError> {
        if voice.is_empty()
            || voice.len() > 128
            || !voice
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
        {
            return Err(error("Invalid ElevenLabs voice ID"));
        }
        if text.trim().is_empty() || text.len() > 16000 {
            return Err(error("Speech must contain 1–16000 bytes"));
        }
        let path = if streaming {
            format!("text-to-speech/{voice}/stream?output_format=pcm_24000")
        } else {
            format!("text-to-speech/{voice}?output_format=mp3_44100_128")
        };
        Self::checked(
            self.request(reqwest::Method::POST, &path)
                .json(&serde_json::json!({"text":text,"model_id":"eleven_flash_v2_5"})),
        )
        .await
    }
}

fn sample_format(path: &std::path::Path) -> Result<(&'static str, &'static str), ManagedError> {
    match path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "wav" => Ok(("wav", "audio/wav")),
        "mp3" => Ok(("mp3", "audio/mpeg")),
        "m4a" => Ok(("m4a", "audio/mp4")),
        "flac" => Ok(("flac", "audio/flac")),
        "ogg" => Ok(("ogg", "audio/ogg")),
        "webm" => Ok(("webm", "audio/webm")),
        _ => Err(error("Use WAV, MP3, M4A, FLAC, OGG, or WebM audio samples")),
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    pub(crate) fn client(endpoint: String) -> Client {
        nanocodex::oai::transport::install_default_rustls_crypto_provider();
        Client {
            endpoint,
            http: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .unwrap(),
            key: reqwest::header::HeaderValue::from_static("synthetic-test-key"),
        }
    }
    #[test]
    fn sample_formats_and_verification_are_explicit() {
        assert_eq!(
            sample_format(std::path::Path::new("private-name.WAV")).unwrap(),
            ("wav", "audio/wav")
        );
        assert!(sample_format(std::path::Path::new("secret.env")).is_err());
        let voice: Voice = serde_json::from_value(
            serde_json::json!({"voice_id":"test", "requires_verification":true}),
        )
        .unwrap();
        assert!(voice.requires_verification);
    }
    #[tokio::test]
    async fn catalog_follows_v2_pages_and_rejects_repeated_tokens() {
        use axum::{Json, Router, extract::Query, routing::get};
        let app = Router::new().route("/v2/voices", get(|Query(q): Query<std::collections::HashMap<String, String>>| async move {
            assert_eq!(q["page_size"], "100");
            Json(serde_json::json!({"voices":[], "has_more":true,"next_page_token":"repeat"}))
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let client = client(format!("http://{}/v1", listener.local_addr().unwrap()));
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        assert!(
            client
                .voices()
                .await
                .unwrap_err()
                .to_string()
                .contains("Repeated")
        );
        server.abort();
    }
    #[tokio::test]
    async fn redirects_are_rejected_before_credentials_can_follow() {
        use axum::{Router, http::StatusCode, routing::post};
        let app = Router::new().route(
            "/text-to-speech/test",
            post(|| async {
                (
                    StatusCode::TEMPORARY_REDIRECT,
                    [("location", "http://127.0.0.1:1/secret")],
                )
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let client = client(format!("http://{}", listener.local_addr().unwrap()));
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        assert!(
            client
                .speech("test", "hello")
                .await
                .unwrap_err()
                .to_string()
                .contains("307")
        );
        server.abort();
    }
    #[tokio::test]
    async fn clone_rejects_non_regular_audio_paths_before_opening() {
        let directory = tempfile::Builder::new().suffix(".wav").tempdir().unwrap();
        let result = client("http://127.0.0.1:1".into())
            .clone_voice("Test", &[directory.path().to_owned()], true)
            .await;
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("regular audio files")
        );
    }
    #[tokio::test]
    async fn clone_requires_consent_before_reading_samples_or_network() {
        let client = client("http://127.0.0.1:1".into());
        let result = client
            .clone_voice("Test", &[PathBuf::from("missing.wav")], false)
            .await;
        assert!(result.unwrap_err().to_string().contains("consent"));
    }
    #[tokio::test]
    #[ignore = "requires local ffmpeg; records a synthetic tone, never a microphone"]
    async fn recorded_wav_uploads_as_multipart_and_cleans_up() {
        use axum::{Json, Router, extract::Request, routing::post};
        let app = Router::new().route("/voices/add", post(|request: Request| async move {
            assert_eq!(request.headers()["xi-api-key"], "synthetic-test-key");
            assert!(request.headers()["content-type"].to_str().unwrap().starts_with("multipart/form-data; boundary="));
            let body = axum::body::to_bytes(request.into_body(), 1024 * 1024).await.unwrap();
            let text = String::from_utf8_lossy(&body);
            assert!(text.contains("name=\"name\""));
            assert!(text.contains("Synthetic test clone"));
            assert!(text.contains("filename=\"sample-1.wav\""));
            assert!(body.windows(4).any(|bytes| bytes == b"RIFF"));
            Json(serde_json::json!({"voice_id":"synthetic_clone", "requires_verification":false}))
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let client = client(format!("http://{}", listener.local_addr().unwrap()));
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let recorder = crate::voice_recording::Recorder::synthetic().await.unwrap();
        tokio::time::sleep(Duration::from_millis(750)).await;
        assert!(
            recorder.peak() > 0,
            "live meter must read the recorded tone before stop"
        );
        let sample = recorder.stop().await.unwrap();
        let path = sample.path().to_owned();
        assert!(std::fs::metadata(&path).unwrap().len() > 1000);
        let voice = client
            .clone_voice("Synthetic test clone", std::slice::from_ref(&path), true)
            .await
            .unwrap();
        assert_eq!(voice.voice_id, "synthetic_clone");
        assert!(!voice.requires_verification);
        drop(sample);
        assert!(!path.exists());
        server.abort();
    }

    #[tokio::test]
    async fn speech_uses_direct_provider_contract_and_redacts_error_bodies() {
        use axum::{
            Json, Router,
            extract::Query,
            http::{HeaderMap, StatusCode},
            routing::post,
        };
        let app = Router::new().route(
            "/text-to-speech/test_voice",
            post(
                |headers: HeaderMap,
                 Query(query): Query<std::collections::HashMap<String, String>>,
                 Json(body): Json<serde_json::Value>| async move {
                    assert_eq!(headers["xi-api-key"], "synthetic-test-key");
                    assert_eq!(query["output_format"], "mp3_44100_128");
                    assert_eq!(body["model_id"], "eleven_flash_v2_5");
                    if body["text"] == "fail" {
                        (StatusCode::UNAUTHORIZED, "sensitive provider body")
                    } else {
                        (StatusCode::OK, "audio bytes")
                    }
                },
            ),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let client = client(format!("http://{}", listener.local_addr().unwrap()));
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        assert_eq!(
            client.speech("test_voice", "hello").await.unwrap(),
            b"audio bytes"
        );
        let error = client
            .speech("test_voice", "fail")
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("401"));
        assert!(!error.contains("sensitive"));
        assert!(client.speech("../invalid", "hello").await.is_err());
        server.abort();
    }
}

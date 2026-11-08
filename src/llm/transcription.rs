//! 语音转写：调用 OpenAI 兼容的 `/audio/transcriptions` 接口，实现 `VoiceTranscriber` 契约。

use std::path::Path;
use std::time::Duration;

use tracing::{info, warn};

use crate::contracts::{TranscriptionError, VoiceTranscriber};
use crate::domain::TranscriptionConfig;

const DEFAULT_API_BASE: &str = "https://api.openai.com/v1";
/// OpenAI 转写接口的单文件上限。
const MAX_AUDIO_BYTES: u64 = 25 * 1024 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

/// OpenAI 兼容 `/audio/transcriptions` 接口的转写实现。
#[derive(Debug, Clone)]
pub struct Transcriber {
    client: reqwest::Client,
    endpoint: String,
    api_key: Option<String>,
    model: String,
    language: Option<String>,
}

impl Transcriber {
    pub fn from_config(config: &TranscriptionConfig) -> Self {
        let api_key = std::env::var(&config.api_key_env).ok();
        if api_key.is_none() {
            warn!(
                event = "TranscriptionApiKeyMissing",
                env = %config.api_key_env,
                "transcription api key env var not set; requests will be sent without auth"
            );
        }
        let base = config
            .api_base
            .as_deref()
            .unwrap_or(DEFAULT_API_BASE)
            .trim_end_matches('/');
        let client = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .unwrap_or_default();
        Self {
            client,
            endpoint: format!("{base}/audio/transcriptions"),
            api_key,
            model: config.model.clone(),
            language: config.language.clone(),
        }
    }
}

#[async_trait::async_trait]
impl VoiceTranscriber for Transcriber {
    async fn transcribe(&self, path: &Path) -> Result<String, TranscriptionError> {
        let (upload_name, mime) = upload_format(path)?;
        if tokio::fs::metadata(path).await?.len() > MAX_AUDIO_BYTES {
            return Err(TranscriptionError::TooLarge(MAX_AUDIO_BYTES));
        }
        let bytes = tokio::fs::read(path).await?;
        let size = bytes.len();
        let file = reqwest::multipart::Part::bytes(bytes)
            .file_name(upload_name)
            .mime_str(mime)
            .map_err(request_error)?;
        let mut form = reqwest::multipart::Form::new()
            .part("file", file)
            .text("model", self.model.clone())
            .text("response_format", "json");
        if let Some(language) = &self.language {
            form = form.text("language", language.clone());
        }
        let mut request = self.client.post(&self.endpoint).multipart(form);
        if let Some(key) = &self.api_key {
            request = request.bearer_auth(key);
        }
        let started = std::time::Instant::now();
        let response = request.send().await.map_err(request_error)?;
        let status = response.status();
        if !status.is_success() {
            warn!(
                event = "TranscriptionFailed",
                status = status.as_u16(),
                path = %path.display(),
                "transcription service returned error status"
            );
            return Err(TranscriptionError::Status(status.as_u16()));
        }
        let body: TranscriptionResponse = response.json().await.map_err(request_error)?;
        let text = body.text.trim().to_string();
        if text.is_empty() {
            return Err(TranscriptionError::Empty);
        }
        info!(
            event = "TranscriptionCompleted",
            path = %path.display(),
            bytes = size,
            chars = text.chars().count(),
            elapsed_ms = started.elapsed().as_millis() as u64,
            "voice transcribed"
        );
        Ok(text)
    }
}

fn request_error(e: reqwest::Error) -> TranscriptionError {
    TranscriptionError::Request(e.to_string())
}

#[derive(serde::Deserialize)]
struct TranscriptionResponse {
    text: String,
}

/// 按扩展名确定上传文件名与 MIME。
///
/// OpenAI 按文件名后缀判断格式：Telegram 语音为 `.oga`（OGG/Opus），需改为 `.ogg` 上传。
/// QQ 的 silk、amr 等格式主流服务不接受，直接报不支持而不引入转码依赖。
fn upload_format(path: &Path) -> Result<(String, &'static str), TranscriptionError> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let (ext, mime) = match ext.as_str() {
        "wav" => ("wav", "audio/wav"),
        "mp3" | "mpga" | "mpeg" => ("mp3", "audio/mpeg"),
        "m4a" | "mp4" => ("m4a", "audio/mp4"),
        "ogg" | "oga" | "opus" => ("ogg", "audio/ogg"),
        "webm" => ("webm", "audio/webm"),
        "flac" => ("flac", "audio/flac"),
        _ => return Err(TranscriptionError::UnsupportedFormat(ext)),
    };
    Ok((format!("voice.{ext}"), mime))
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn transcriber(base: &str) -> Transcriber {
        let mut t = Transcriber::from_config(&TranscriptionConfig {
            api_key_env: "HARNESS_TEST_TRANSCRIPTION_KEY_UNSET".into(),
            api_base: Some(format!("{base}/v1/")),
            model: "whisper-1".into(),
            language: Some("zh".into()),
        });
        t.api_key = Some("test-key".into());
        t
    }

    fn audio_file(name: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(name);
        std::fs::write(&path, b"OggS fake audio").unwrap();
        (dir, path)
    }

    #[tokio::test]
    async fn uploads_oga_as_ogg_and_returns_trimmed_text() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/audio/transcriptions"))
            .and(header("authorization", "Bearer test-key"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"text":" 你好 "})),
            )
            .expect(1)
            .mount(&server)
            .await;
        let (_dir, file) = audio_file("file_0.oga");
        let text = transcriber(&server.uri()).transcribe(&file).await.unwrap();
        assert_eq!(text, "你好");

        let request = &server.received_requests().await.unwrap()[0];
        let body = String::from_utf8_lossy(&request.body);
        assert!(body.contains("filename=\"voice.ogg\""), "{body}");
        assert!(body.contains("Content-Type: audio/ogg"), "{body}");
        assert!(body.contains("whisper-1"));
        assert!(body.contains("name=\"language\""));
    }

    #[tokio::test]
    async fn error_status_is_reported_without_body() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(401).set_body_string("secret detail"))
            .mount(&server)
            .await;
        let (_dir, file) = audio_file("a.wav");
        let err = transcriber(&server.uri())
            .transcribe(&file)
            .await
            .unwrap_err();
        assert!(matches!(err, TranscriptionError::Status(401)));
        assert!(!err.to_string().contains("secret"));
    }

    #[tokio::test]
    async fn empty_text_and_unsupported_format_fail_without_request() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"text":"  "})),
            )
            .mount(&server)
            .await;
        let t = transcriber(&server.uri());
        let (_dir, wav) = audio_file("a.wav");
        assert!(matches!(
            t.transcribe(&wav).await,
            Err(TranscriptionError::Empty)
        ));

        let (_dir2, silk) = audio_file("a.silk");
        assert!(matches!(
            t.transcribe(&silk).await,
            Err(TranscriptionError::UnsupportedFormat(_))
        ));
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
    }
}

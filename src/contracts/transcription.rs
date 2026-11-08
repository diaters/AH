//! 语音转写契约
//!
//! 通道下载语音后调用转写服务，把结果以 `<VOICE_TRANSCRIPTION>` 文本块写进入站消息。
//! 转写只作用于通道自己下载的文件，用户文本中手写的 `[VOICE:path]` 标记不会被读取上传。

use std::path::Path;
use std::sync::Arc;

#[derive(thiserror::Error, Debug)]
pub enum TranscriptionError {
    #[error("unsupported audio format: {0}")]
    UnsupportedFormat(String),
    #[error("audio file exceeds {0} bytes")]
    TooLarge(u64),
    #[error("failed to read audio file: {0}")]
    Io(#[from] std::io::Error),
    #[error("transcription request failed: {0}")]
    Request(String),
    #[error("transcription service returned HTTP {0}")]
    Status(u16),
    #[error("transcription service returned empty text")]
    Empty,
}

impl TranscriptionError {
    /// 给用户与模型看的简短原因，不包含服务端响应体。
    pub fn user_reason(&self) -> &'static str {
        match self {
            Self::UnsupportedFormat(_) => "不支持的音频格式",
            Self::TooLarge(_) => "音频文件过大",
            Self::Io(_) => "无法读取音频文件",
            Self::Request(_) | Self::Status(_) => "转写服务不可用",
            Self::Empty => "未识别到语音内容",
        }
    }
}

#[async_trait::async_trait]
pub trait VoiceTranscriber: Send + Sync + 'static {
    /// 转写本地音频文件，返回去除首尾空白的非空文本。
    async fn transcribe(&self, path: &Path) -> Result<String, TranscriptionError>;
}

pub type SharedVoiceTranscriber = Arc<dyn VoiceTranscriber>;

/// 渲染一条消息中各段语音的转写结果：成功为 `<VOICE_TRANSCRIPTION>` 文本块，失败为说明行。
pub fn render_transcription_outcomes(outcomes: &[Result<String, TranscriptionError>]) -> String {
    let texts: Vec<String> = outcomes
        .iter()
        .filter_map(|o| o.as_ref().ok())
        .cloned()
        .collect();
    let mut parts = vec![crate::domain::voice_transcription_text(&texts)];
    parts.extend(
        outcomes
            .iter()
            .filter_map(|o| o.as_ref().err())
            .map(|e| format!("（语音转写失败：{}）", e.user_reason())),
    );
    parts.retain(|p| !p.is_empty());
    parts.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_keeps_transcripts_and_failure_notes() {
        let outcomes = vec![
            Ok("第一段".to_string()),
            Err(TranscriptionError::Status(500)),
            Ok("第二段".to_string()),
        ];
        assert_eq!(
            render_transcription_outcomes(&outcomes),
            "<VOICE_TRANSCRIPTION_0>第一段</VOICE_TRANSCRIPTION_0>\n\
             <VOICE_TRANSCRIPTION_1>第二段</VOICE_TRANSCRIPTION_1>\n\
             （语音转写失败：转写服务不可用）"
        );
        assert_eq!(render_transcription_outcomes(&[]), "");
    }
}

/// 测试用转写器：文件名含 `fail` 时返回服务错误，否则返回 `转写:<文件名>`，并记录调用次数。
#[cfg(test)]
#[derive(Default)]
pub(crate) struct FakeTranscriber {
    pub calls: std::sync::atomic::AtomicUsize,
}

#[cfg(test)]
#[async_trait::async_trait]
impl VoiceTranscriber for FakeTranscriber {
    async fn transcribe(&self, path: &Path) -> Result<String, TranscriptionError> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if name.contains("fail") {
            return Err(TranscriptionError::Status(500));
        }
        Ok(format!("转写:{name}"))
    }
}

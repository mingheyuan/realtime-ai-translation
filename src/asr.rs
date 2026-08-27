use std::{path::Path, process::Stdio};

use serde::Deserialize;
use thiserror::Error;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines},
    process::{Child, ChildStdin, ChildStdout, Command},
    time::{timeout, Duration},
};

use crate::domain::AudioSource;

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AsrEvent {
    Ready { locale: String },
    Partial { text: String },
    Final { text: String },
    Error { message: String },
}

#[derive(Debug, Error)]
pub enum AsrError {
    #[error("Apple Speech bridge is missing at {0}; run scripts/build-macos-speech.sh")]
    BridgeMissing(String),
    #[error("failed to start Apple Speech bridge: {0}")]
    Start(#[source] std::io::Error),
    #[error("Apple Speech bridge I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("Apple Speech bridge returned invalid JSON: {0}")]
    Protocol(#[from] serde_json::Error),
    #[error("Apple Speech bridge exited")]
    Exited,
}

pub struct AppleSpeechBridge {
    child: Child,
    stdin: ChildStdin,
    lines: Lines<BufReader<ChildStdout>>,
}

impl AppleSpeechBridge {
    pub fn spawn(
        executable: &Path,
        source_language: &str,
        audio_source: AudioSource,
        hotwords: &[String],
    ) -> Result<Self, AsrError> {
        if !executable.is_file() {
            return Err(AsrError::BridgeMissing(executable.display().to_string()));
        }
        let locale = apple_locale(source_language);
        let mut command = Command::new(executable);
        command
            .arg("--locale")
            .arg(locale)
            .arg("--audio-source")
            .arg(audio_source.bridge_argument())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true);
        for hotword in hotwords {
            command.arg("--term").arg(hotword);
        }
        let mut child = command.spawn().map_err(AsrError::Start)?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| AsrError::Start(std::io::Error::other("missing bridge stdin")))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| AsrError::Start(std::io::Error::other("missing bridge stdout")))?;
        Ok(Self {
            child,
            stdin,
            lines: BufReader::new(stdout).lines(),
        })
    }

    pub async fn next_event(&mut self) -> Result<AsrEvent, AsrError> {
        let line = self.lines.next_line().await?.ok_or(AsrError::Exited)?;
        serde_json::from_str(&line).map_err(Into::into)
    }

    pub async fn stop(mut self) -> Result<(), AsrError> {
        self.stdin.write_all(b"stop\n").await?;
        self.stdin.flush().await?;
        if timeout(Duration::from_secs(3), self.child.wait())
            .await
            .is_err()
        {
            self.child.kill().await?;
        }
        Ok(())
    }
}

pub fn apple_locale(language: &str) -> &'static str {
    if language.to_ascii_lowercase().starts_with("en") {
        "en-US"
    } else {
        "zh-CN"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_mvp_languages_to_apple_locales() {
        assert_eq!(apple_locale("zh"), "zh-CN");
        assert_eq!(apple_locale("zh-CN"), "zh-CN");
        assert_eq!(apple_locale("en"), "en-US");
    }
}

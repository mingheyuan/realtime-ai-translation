use std::{path::Path, process::Stdio};

use serde::Deserialize;
use thiserror::Error;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines},
    net::{unix::OwnedReadHalf, unix::OwnedWriteHalf, UnixListener},
    process::{Child, Command},
    time::{sleep, timeout, Duration},
};
use uuid::Uuid;

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
    #[error("Apple Speech bridge app is missing at {0}; run scripts/build-macos-speech.sh")]
    BridgeMissing(String),
    #[error("failed to start Apple Speech bridge: {0}")]
    Start(#[source] std::io::Error),
    #[error("Apple Speech bridge I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("Apple Speech bridge returned invalid JSON: {0}")]
    Protocol(#[from] serde_json::Error),
    #[error("Apple Speech bridge exited before connecting: {0}")]
    LaunchExited(String),
    #[error("timed out waiting for Apple Speech bridge to connect")]
    ConnectionTimeout,
    #[error("Apple Speech bridge exited")]
    Exited,
}

pub struct AppleSpeechBridge {
    child: Child,
    writer: OwnedWriteHalf,
    lines: Lines<BufReader<OwnedReadHalf>>,
}

impl AppleSpeechBridge {
    pub async fn spawn(
        app_bundle: &Path,
        source_language: &str,
        audio_source: AudioSource,
        hotwords: &[String],
    ) -> Result<Self, AsrError> {
        if !app_bundle.is_dir() {
            return Err(AsrError::BridgeMissing(app_bundle.display().to_string()));
        }
        // sockaddr_un.sun_path is only 104 bytes on macOS. TMPDIR can be a
        // long /var/folders path, so use a deliberately short, unique path.
        let socket_path =
            Path::new("/tmp").join(format!("rt-speech-{}.sock", Uuid::new_v4().simple()));
        let listener = UnixListener::bind(&socket_path)?;
        let locale = apple_locale(source_language);
        // LaunchServices makes the bridge app the responsible TCC process. A
        // direct child executable inherits the terminal/Codex identity, whose
        // Info.plist does not contain our Speech usage description.
        let mut command = Command::new("/usr/bin/open");
        command
            .arg("-n")
            .arg("-W")
            .arg("-a")
            .arg(app_bundle)
            .arg("--args")
            .arg("--socket")
            .arg(&socket_path)
            .arg("--locale")
            .arg(locale)
            .arg("--audio-source")
            .arg(audio_source.bridge_argument())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .kill_on_drop(true);
        for hotword in hotwords {
            command.arg("--term").arg(hotword);
        }
        let mut child = command.spawn().map_err(AsrError::Start)?;
        let connection = tokio::select! {
            accepted = listener.accept() => accepted?.0,
            status = child.wait() => {
                let _ = std::fs::remove_file(&socket_path);
                return Err(AsrError::LaunchExited(status?.to_string()));
            }
            _ = sleep(Duration::from_secs(15)) => {
                let _ = child.kill().await;
                let _ = std::fs::remove_file(&socket_path);
                return Err(AsrError::ConnectionTimeout);
            }
        };
        drop(listener);
        let _ = std::fs::remove_file(&socket_path);
        let (reader, writer) = connection.into_split();
        Ok(Self {
            child,
            writer,
            lines: BufReader::new(reader).lines(),
        })
    }

    pub async fn next_event(&mut self) -> Result<AsrEvent, AsrError> {
        let line = self.lines.next_line().await?.ok_or(AsrError::Exited)?;
        serde_json::from_str(&line).map_err(Into::into)
    }

    pub async fn stop(mut self) -> Result<(), AsrError> {
        self.writer.write_all(b"stop\n").await?;
        self.writer.flush().await?;
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

use std::{path::PathBuf, process::Stdio, sync::Arc};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines},
    net::{unix::OwnedReadHalf, unix::OwnedWriteHalf, UnixListener},
    process::{Child, Command},
    time::{sleep, timeout, Duration},
};
use uuid::Uuid;

use crate::domain::{AsrEngine, AudioSource};

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AsrEvent {
    Ready { locale: String },
    Partial { text: String },
    Final { text: String },
    Error { message: String },
}

#[derive(Debug, Clone, Serialize)]
pub struct AsrProviderStatus {
    pub id: &'static str,
    pub label: &'static str,
    pub available: bool,
    pub load_policy: &'static str,
}

#[derive(Debug, Error)]
pub enum AsrError {
    #[error("{engine} 尚未配置；请设置 {environment_variable}")]
    ProviderUnavailable {
        engine: &'static str,
        environment_variable: &'static str,
    },
    #[error("{engine} bridge 不存在：{path}")]
    BridgeMissing { engine: &'static str, path: String },
    #[error("无法启动 {engine} bridge：{source}")]
    Start {
        engine: &'static str,
        #[source]
        source: std::io::Error,
    },
    #[error("ASR bridge I/O 失败：{0}")]
    Io(#[from] std::io::Error),
    #[error("ASR bridge 返回了无效 JSON：{0}")]
    Protocol(#[from] serde_json::Error),
    #[error("{engine} bridge 在连接前退出：{status}")]
    LaunchExited {
        engine: &'static str,
        status: String,
    },
    #[error("等待 {0} bridge 连接超时")]
    ConnectionTimeout(&'static str),
    #[error("ASR bridge 已退出")]
    Exited,
}

#[async_trait]
pub trait AsrSession: Send {
    async fn next_event(&mut self) -> Result<AsrEvent, AsrError>;
    async fn stop(self: Box<Self>) -> Result<(), AsrError>;
}

#[async_trait]
trait AsrProvider: Send + Sync {
    fn engine(&self) -> AsrEngine;
    fn available(&self) -> bool;

    async fn start(
        &self,
        source_language: &str,
        audio_source: AudioSource,
        hotwords: &[String],
    ) -> Result<Box<dyn AsrSession>, AsrError>;

    fn status(&self) -> AsrProviderStatus {
        let engine = self.engine();
        AsrProviderStatus {
            id: engine.id(),
            label: engine.description(),
            available: self.available(),
            load_policy: "on_session_start",
        }
    }
}

#[derive(Clone)]
pub struct AsrProviderRegistry {
    providers: Arc<Vec<Arc<dyn AsrProvider>>>,
}

impl AsrProviderRegistry {
    pub fn new(apple_speech_path: PathBuf, sherpa_onnx_path: Option<PathBuf>) -> Self {
        let providers: Vec<Arc<dyn AsrProvider>> = vec![
            Arc::new(BridgeProvider::apple(apple_speech_path)),
            Arc::new(BridgeProvider::sherpa(sherpa_onnx_path)),
        ];
        Self {
            providers: Arc::new(providers),
        }
    }

    pub fn statuses(&self) -> Vec<AsrProviderStatus> {
        self.providers
            .iter()
            .map(|provider| provider.status())
            .collect()
    }

    pub fn available(&self, engine: AsrEngine) -> bool {
        self.provider(engine)
            .is_some_and(|provider| provider.available())
    }

    pub async fn start(
        &self,
        engine: AsrEngine,
        source_language: &str,
        audio_source: AudioSource,
        hotwords: &[String],
    ) -> Result<Box<dyn AsrSession>, AsrError> {
        self.provider(engine)
            .expect("all serialized ASR engines must be registered")
            .start(source_language, audio_source, hotwords)
            .await
    }

    fn provider(&self, engine: AsrEngine) -> Option<&Arc<dyn AsrProvider>> {
        self.providers
            .iter()
            .find(|provider| provider.engine() == engine)
    }
}

#[derive(Debug, Clone, Copy)]
enum LaunchMode {
    AppBundle,
    ExecutableOrApp,
}

struct BridgeProvider {
    engine: AsrEngine,
    path: Option<PathBuf>,
    environment_variable: &'static str,
    launch_mode: LaunchMode,
}

impl BridgeProvider {
    fn apple(path: PathBuf) -> Self {
        Self {
            engine: AsrEngine::AppleSpeech,
            path: Some(path),
            environment_variable: "RT_TRANSLATION_SPEECH_BRIDGE",
            launch_mode: LaunchMode::AppBundle,
        }
    }

    fn sherpa(path: Option<PathBuf>) -> Self {
        Self {
            engine: AsrEngine::SherpaOnnx,
            path,
            environment_variable: "RT_TRANSLATION_SHERPA_BRIDGE",
            launch_mode: LaunchMode::ExecutableOrApp,
        }
    }

    fn configured_path(&self) -> Result<&PathBuf, AsrError> {
        self.path.as_ref().ok_or(AsrError::ProviderUnavailable {
            engine: self.engine.description(),
            environment_variable: self.environment_variable,
        })
    }
}

#[async_trait]
impl AsrProvider for BridgeProvider {
    fn engine(&self) -> AsrEngine {
        self.engine
    }

    fn available(&self) -> bool {
        self.path
            .as_ref()
            .is_some_and(|path| match self.launch_mode {
                LaunchMode::AppBundle => path.is_dir(),
                LaunchMode::ExecutableOrApp => path.is_dir() || path.is_file(),
            })
    }

    async fn start(
        &self,
        source_language: &str,
        audio_source: AudioSource,
        hotwords: &[String],
    ) -> Result<Box<dyn AsrSession>, AsrError> {
        let path = self.configured_path()?;
        if !self.available() {
            return Err(AsrError::BridgeMissing {
                engine: self.engine.description(),
                path: path.display().to_string(),
            });
        }
        let session = ProcessAsrSession::spawn(
            self.engine,
            path,
            self.launch_mode,
            source_language,
            audio_source,
            hotwords,
        )
        .await?;
        Ok(Box::new(session))
    }
}

struct ProcessAsrSession {
    child: Child,
    writer: OwnedWriteHalf,
    lines: Lines<BufReader<OwnedReadHalf>>,
}

impl ProcessAsrSession {
    async fn spawn(
        engine: AsrEngine,
        bridge_path: &PathBuf,
        launch_mode: LaunchMode,
        source_language: &str,
        audio_source: AudioSource,
        hotwords: &[String],
    ) -> Result<Self, AsrError> {
        // sockaddr_un.sun_path is only 104 bytes on macOS. Keep this deliberately short.
        let socket_path =
            PathBuf::from("/tmp").join(format!("rt-asr-{}.sock", Uuid::new_v4().simple()));
        let listener = UnixListener::bind(&socket_path)?;
        let locale = normalized_locale(source_language);

        let mut command = match launch_mode {
            LaunchMode::AppBundle => app_command(bridge_path),
            LaunchMode::ExecutableOrApp if bridge_path.is_dir() => app_command(bridge_path),
            LaunchMode::ExecutableOrApp => Command::new(bridge_path),
        };
        command
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

        let mut child = command.spawn().map_err(|source| AsrError::Start {
            engine: engine.description(),
            source,
        })?;
        let connection = tokio::select! {
            accepted = listener.accept() => accepted?.0,
            status = child.wait() => {
                let _ = std::fs::remove_file(&socket_path);
                return Err(AsrError::LaunchExited {
                    engine: engine.description(),
                    status: status?.to_string(),
                });
            }
            _ = sleep(Duration::from_secs(15)) => {
                let _ = child.kill().await;
                let _ = std::fs::remove_file(&socket_path);
                return Err(AsrError::ConnectionTimeout(engine.description()));
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
}

fn app_command(app_bundle: &PathBuf) -> Command {
    // LaunchServices makes the app the responsible TCC process on macOS.
    let mut command = Command::new("/usr/bin/open");
    command
        .arg("-n")
        .arg("-W")
        .arg("-a")
        .arg(app_bundle)
        .arg("--args");
    command
}

#[async_trait]
impl AsrSession for ProcessAsrSession {
    async fn next_event(&mut self) -> Result<AsrEvent, AsrError> {
        let line = self.lines.next_line().await?.ok_or(AsrError::Exited)?;
        serde_json::from_str(&line).map_err(Into::into)
    }

    async fn stop(mut self: Box<Self>) -> Result<(), AsrError> {
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

pub fn normalized_locale(language: &str) -> &'static str {
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
    fn maps_mvp_languages_to_provider_locales() {
        assert_eq!(normalized_locale("zh"), "zh-CN");
        assert_eq!(normalized_locale("zh-CN"), "zh-CN");
        assert_eq!(normalized_locale("en"), "en-US");
    }

    #[test]
    fn registry_reports_lazy_provider_availability_without_starting_processes() {
        let registry = AsrProviderRegistry::new(PathBuf::from("/missing/apple"), None);
        let statuses = registry.statuses();

        assert_eq!(statuses.len(), 2);
        assert_eq!(statuses[0].id, "apple_speech");
        assert!(!statuses[0].available);
        assert_eq!(statuses[0].load_policy, "on_session_start");
        assert_eq!(statuses[1].id, "sherpa_onnx");
        assert!(!statuses[1].available);
    }
}

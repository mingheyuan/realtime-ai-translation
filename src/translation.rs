use std::{path::PathBuf, process::Stdio, sync::Arc, time::Instant};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::Command,
    sync::{mpsc, oneshot},
};
use uuid::Uuid;

use crate::domain::GlossaryEntry;

#[derive(Debug, Clone)]
pub struct TranslationOutput {
    pub text: String,
    pub latency_ms: u128,
}

#[derive(Debug, Error)]
pub enum TranslationError {
    #[error("model worker is not installed; run scripts/setup-models.sh")]
    WorkerNotInstalled,
    #[error("failed to start model worker: {0}")]
    Start(#[source] std::io::Error),
    #[error("model worker I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("model worker protocol failed: {0}")]
    Protocol(#[from] serde_json::Error),
    #[error("model worker stopped unexpectedly")]
    Stopped,
    #[error("translation failed: {0}")]
    Model(String),
}

#[async_trait]
pub trait TranslationProvider: Send + Sync {
    async fn translate(
        &self,
        text: &str,
        source_language: &str,
        target_language: &str,
        glossary: &[GlossaryEntry],
    ) -> Result<TranslationOutput, TranslationError>;

    async fn warmup(
        &self,
        source_language: &str,
        target_language: &str,
    ) -> Result<(), TranslationError> {
        let probe = if source_language.to_ascii_lowercase().starts_with("zh") {
            "准备"
        } else {
            "Ready"
        };
        self.translate(probe, source_language, target_language, &[])
            .await
            .map(|_| ())
    }
}

pub type SharedTranslationProvider = Arc<dyn TranslationProvider>;

pub struct PythonMarianTranslator {
    sender: mpsc::Sender<ActorRequest>,
}

impl PythonMarianTranslator {
    pub fn start(python_path: PathBuf, worker_path: PathBuf) -> Self {
        let (sender, receiver) = mpsc::channel(16);
        tokio::spawn(run_actor(python_path, worker_path, receiver));
        Self { sender }
    }
}

#[async_trait]
impl TranslationProvider for PythonMarianTranslator {
    async fn translate(
        &self,
        text: &str,
        source_language: &str,
        target_language: &str,
        glossary: &[GlossaryEntry],
    ) -> Result<TranslationOutput, TranslationError> {
        if let Some(target) = exact_glossary_translation(text, glossary) {
            return Ok(TranslationOutput {
                text: target,
                latency_ms: 0,
            });
        }
        let (response_tx, response_rx) = oneshot::channel();
        self.sender
            .send(ActorRequest {
                payload: WorkerRequest {
                    id: Uuid::new_v4(),
                    text: text.to_owned(),
                    source_language: source_language.to_owned(),
                    target_language: target_language.to_owned(),
                    glossary: glossary.to_vec(),
                },
                response: response_tx,
            })
            .await
            .map_err(|_| TranslationError::Stopped)?;
        response_rx.await.map_err(|_| TranslationError::Stopped)?
    }
}

pub struct FakeTranslator;

#[async_trait]
impl TranslationProvider for FakeTranslator {
    async fn translate(
        &self,
        text: &str,
        _source_language: &str,
        target_language: &str,
        glossary: &[GlossaryEntry],
    ) -> Result<TranslationOutput, TranslationError> {
        let started = Instant::now();
        let translated = exact_glossary_translation(text, glossary)
            .unwrap_or_else(|| format!("[{target_language}] {text}"));
        Ok(TranslationOutput {
            text: translated,
            latency_ms: started.elapsed().as_millis(),
        })
    }
}

struct ActorRequest {
    payload: WorkerRequest,
    response: oneshot::Sender<Result<TranslationOutput, TranslationError>>,
}

#[derive(Serialize)]
struct WorkerRequest {
    id: Uuid,
    text: String,
    source_language: String,
    target_language: String,
    glossary: Vec<GlossaryEntry>,
}

#[derive(Deserialize)]
struct WorkerResponse {
    id: Option<Uuid>,
    translation: Option<String>,
    latency_ms: Option<u128>,
    error: Option<String>,
}

async fn run_actor(
    python_path: PathBuf,
    worker_path: PathBuf,
    mut receiver: mpsc::Receiver<ActorRequest>,
) {
    if !python_path.is_file() || !worker_path.is_file() {
        while let Some(request) = receiver.recv().await {
            let _ = request
                .response
                .send(Err(TranslationError::WorkerNotInstalled));
        }
        return;
    }
    let mut child = match Command::new(&python_path)
        .arg("-u")
        .arg(&worker_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
    {
        Ok(child) => child,
        Err(error) => {
            while let Some(request) = receiver.recv().await {
                let _ = request
                    .response
                    .send(Err(TranslationError::Start(std::io::Error::new(
                        error.kind(),
                        error.to_string(),
                    ))));
            }
            return;
        }
    };
    let Some(mut stdin) = child.stdin.take() else {
        drain_with_stopped(&mut receiver).await;
        return;
    };
    let Some(stdout) = child.stdout.take() else {
        drain_with_stopped(&mut receiver).await;
        return;
    };
    let mut lines = BufReader::new(stdout).lines();

    while let Some(request) = receiver.recv().await {
        let result = async {
            let line = serde_json::to_string(&request.payload)?;
            stdin.write_all(line.as_bytes()).await?;
            stdin.write_all(b"\n").await?;
            stdin.flush().await?;
            let response_line = lines.next_line().await?.ok_or(TranslationError::Stopped)?;
            let response: WorkerResponse = serde_json::from_str(&response_line)?;
            if response.id != Some(request.payload.id) {
                return Err(TranslationError::Model(
                    "worker response id did not match request".to_owned(),
                ));
            }
            if let Some(error) = response.error {
                return Err(TranslationError::Model(error));
            }
            let text = response
                .translation
                .filter(|text| !text.trim().is_empty())
                .ok_or_else(|| TranslationError::Model("empty translation".to_owned()))?;
            Ok(TranslationOutput {
                text,
                latency_ms: response.latency_ms.unwrap_or_default(),
            })
        }
        .await;
        let _ = request.response.send(result);
    }
}

async fn drain_with_stopped(receiver: &mut mpsc::Receiver<ActorRequest>) {
    while let Some(request) = receiver.recv().await {
        let _ = request.response.send(Err(TranslationError::Stopped));
    }
}

fn exact_glossary_translation(text: &str, glossary: &[GlossaryEntry]) -> Option<String> {
    let normalized = text.trim();
    glossary.iter().find_map(|entry| {
        (entry.source.eq_ignore_ascii_case(normalized)
            || entry
                .aliases
                .iter()
                .any(|alias| alias.eq_ignore_ascii_case(normalized)))
        .then(|| entry.target.clone())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn fake_translator_honors_an_exact_glossary_term() {
        let translator = FakeTranslator;
        let output = translator
            .translate(
                "实时翻译",
                "zh",
                "en",
                &[GlossaryEntry {
                    id: None,
                    source: "实时翻译".to_owned(),
                    source_language: "zh".to_owned(),
                    target: "real-time translation".to_owned(),
                    target_language: "en".to_owned(),
                    aliases: Vec::new(),
                    domain: "AI".to_owned(),
                    confidence: 1.0,
                    evidence_count: 1,
                    active: true,
                }],
            )
            .await
            .expect("translation");
        assert_eq!(output.text, "real-time translation");
    }

    #[tokio::test]
    async fn translator_warmup_runs_a_small_inference() {
        let translator = FakeTranslator;
        translator
            .warmup("zh-CN", "en-US")
            .await
            .expect("warmup succeeds");
    }
}

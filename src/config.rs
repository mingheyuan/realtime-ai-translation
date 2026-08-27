use std::{env, net::SocketAddr, path::PathBuf};

#[derive(Debug, Clone)]
pub struct AppConfig {
    pub listen: SocketAddr,
    pub database_path: PathBuf,
    pub speech_bridge_path: PathBuf,
    pub python_path: PathBuf,
    pub model_worker_path: PathBuf,
    pub fake_translation: bool,
    pub segment_idle_ms: u64,
    pub llm: LlmConfig,
}

#[derive(Debug, Clone)]
pub struct LlmConfig {
    pub enabled: bool,
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub timeout_seconds: u64,
}

impl AppConfig {
    pub fn from_env() -> anyhow::Result<Self> {
        let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let listen = env::var("RT_TRANSLATION_LISTEN")
            .unwrap_or_else(|_| "127.0.0.1:8765".to_owned())
            .parse()?;
        let database_path = env::var_os("RT_TRANSLATION_DB")
            .map(PathBuf::from)
            .unwrap_or_else(|| manifest_dir.join("data/dictionary.sqlite3"));
        let speech_bridge_path = env::var_os("RT_TRANSLATION_SPEECH_BRIDGE")
            .map(PathBuf::from)
            .unwrap_or_else(|| manifest_dir.join("target/RealtimeTranslationSpeechBridge.app"));
        let python_path = env::var_os("RT_TRANSLATION_PYTHON")
            .map(PathBuf::from)
            .unwrap_or_else(|| manifest_dir.join(".venv/bin/python"));
        let model_worker_path = env::var_os("RT_TRANSLATION_MODEL_WORKER")
            .map(PathBuf::from)
            .unwrap_or_else(|| manifest_dir.join("model-worker/worker.py"));
        let fake_translation = env_bool("RT_TRANSLATION_FAKE_TRANSLATION", false);
        let segment_idle_ms = env::var("RT_TRANSLATION_SEGMENT_IDLE_MS")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(900);
        let llm = LlmConfig {
            enabled: env_bool("RT_TRANSLATION_LLM_ENABLED", false),
            base_url: env::var("RT_TRANSLATION_LLM_BASE_URL")
                .unwrap_or_else(|_| "https://api.openai.com/v1".to_owned()),
            api_key: env::var("RT_TRANSLATION_LLM_API_KEY").unwrap_or_default(),
            model: env::var("RT_TRANSLATION_LLM_MODEL").unwrap_or_default(),
            timeout_seconds: env::var("RT_TRANSLATION_LLM_TIMEOUT")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(20),
        };
        Ok(Self {
            listen,
            database_path,
            speech_bridge_path,
            python_path,
            model_worker_path,
            fake_translation,
            segment_idle_ms,
            llm,
        })
    }
}

fn env_bool(name: &str, default: bool) -> bool {
    env::var(name)
        .ok()
        .map(|value| {
            matches!(
                value.to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(default)
}

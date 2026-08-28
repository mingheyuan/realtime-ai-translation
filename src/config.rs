use std::{
    env, fs,
    io::ErrorKind,
    net::SocketAddr,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone)]
pub struct AppConfig {
    pub listen: SocketAddr,
    pub database_path: PathBuf,
    pub speech_bridge_path: PathBuf,
    pub sherpa_bridge_path: Option<PathBuf>,
    pub overlay_app_path: PathBuf,
    pub python_path: PathBuf,
    pub model_worker_path: PathBuf,
    pub fake_translation: bool,
    pub segment_idle_ms: u64,
    pub preview_interval_ms: u64,
    pub llm: LlmConfig,
}

#[derive(Debug, Clone)]
pub struct LlmConfig {
    pub enabled: bool,
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub timeout_seconds: u64,
    pub thinking_disabled: bool,
}

impl AppConfig {
    pub fn from_env() -> anyhow::Result<Self> {
        let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        load_local_env(&manifest_dir.join(".env.local"))?;
        let listen = env::var("RT_TRANSLATION_LISTEN")
            .unwrap_or_else(|_| "127.0.0.1:8765".to_owned())
            .parse()?;
        let database_path = env::var_os("RT_TRANSLATION_DB")
            .map(PathBuf::from)
            .unwrap_or_else(|| manifest_dir.join("data/dictionary.sqlite3"));
        let speech_bridge_path = env::var_os("RT_TRANSLATION_SPEECH_BRIDGE")
            .map(PathBuf::from)
            .unwrap_or_else(|| manifest_dir.join("target/RealtimeTranslationSpeechBridge.app"));
        let sherpa_bridge_path = env::var_os("RT_TRANSLATION_SHERPA_BRIDGE").map(PathBuf::from);
        let overlay_app_path = env::var_os("RT_TRANSLATION_OVERLAY_APP")
            .map(PathBuf::from)
            .unwrap_or_else(|| manifest_dir.join("target/RealtimeTranslationOverlay.app"));
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
            .unwrap_or(1_500);
        let preview_interval_ms = env::var("RT_TRANSLATION_PREVIEW_INTERVAL_MS")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(200)
            .clamp(100, 1_000);
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
            thinking_disabled: env_bool("RT_TRANSLATION_LLM_THINKING_DISABLED", false),
        };
        Ok(Self {
            listen,
            database_path,
            speech_bridge_path,
            sherpa_bridge_path,
            overlay_app_path,
            python_path,
            model_worker_path,
            fake_translation,
            segment_idle_ms,
            preview_interval_ms,
            llm,
        })
    }
}

fn load_local_env(path: &Path) -> anyhow::Result<()> {
    let contents = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    for (line_number, line) in contents.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((name, value)) = line.split_once('=') else {
            anyhow::bail!("invalid .env.local entry on line {}", line_number + 1);
        };
        let name = name.trim();
        if name.is_empty()
            || !name
                .chars()
                .all(|character| character == '_' || character.is_ascii_alphanumeric())
        {
            anyhow::bail!("invalid .env.local variable on line {}", line_number + 1);
        }
        // Explicit process environment always wins over the local file.
        if env::var_os(name).is_none() {
            env::set_var(name, unquote(value.trim()));
        }
    }
    Ok(())
}

fn unquote(value: &str) -> &str {
    if value.len() >= 2
        && ((value.starts_with('"') && value.ends_with('"'))
            || (value.starts_with('\'') && value.ends_with('\'')))
    {
        &value[1..value.len() - 1]
    } else {
        value
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unquotes_local_values() {
        assert_eq!(unquote("plain"), "plain");
        assert_eq!(unquote("\"quoted\""), "quoted");
        assert_eq!(unquote("'single'"), "single");
    }
}

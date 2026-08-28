use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CaptionState {
    Partial,
    Draft,
    Final,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SegmentSnapshot {
    pub segment_id: u64,
    pub revision: u64,
    pub state: CaptionState,
    pub source_text: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerEvent {
    SessionStatus {
        running: bool,
        message: String,
    },
    Caption {
        segment_id: u64,
        revision: u64,
        state: CaptionState,
        source_text: String,
        translation_text: String,
        source_language: String,
        target_language: String,
        llm_applied: bool,
        latency_ms: u128,
    },
    Error {
        code: String,
        message: String,
    },
    DictionaryChanged,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AudioSource {
    #[default]
    Microphone,
    SystemAudio,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AsrEngine {
    #[default]
    AppleSpeech,
    SherpaOnnx,
}

impl AsrEngine {
    pub fn id(self) -> &'static str {
        match self {
            Self::AppleSpeech => "apple_speech",
            Self::SherpaOnnx => "sherpa_onnx",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::AppleSpeech => "Apple Speech",
            Self::SherpaOnnx => "Sherpa-ONNX",
        }
    }
}

impl AudioSource {
    pub fn bridge_argument(self) -> &'static str {
        match self {
            Self::Microphone => "microphone",
            Self::SystemAudio => "system_audio",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::Microphone => "麦克风",
            Self::SystemAudio => "系统音频",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StartSessionRequest {
    pub source_language: String,
    pub target_language: String,
    #[serde(default)]
    pub audio_source: AudioSource,
    #[serde(default)]
    pub asr_engine: AsrEngine,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GlossaryEntry {
    pub id: Option<i64>,
    pub source: String,
    pub source_language: String,
    pub target: String,
    pub target_language: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(default = "default_domain")]
    pub domain: String,
    #[serde(default = "default_confidence")]
    pub confidence: f64,
    #[serde(default = "default_evidence")]
    pub evidence_count: u32,
    #[serde(default = "default_active")]
    pub active: bool,
}

impl GlossaryEntry {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.source.trim().is_empty() || self.target.trim().is_empty() {
            return Err("source and target are required");
        }
        if self.source_language == self.target_language {
            return Err("source and target languages must differ");
        }
        if !(0.0..=1.0).contains(&self.confidence) {
            return Err("confidence must be between 0 and 1");
        }
        Ok(())
    }
}

fn default_domain() -> String {
    "general".to_owned()
}

fn default_confidence() -> f64 {
    1.0
}

fn default_evidence() -> u32 {
    1
}

fn default_active() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize)]
pub struct CorrectionRequest {
    pub original_source: String,
    pub corrected_source: String,
    pub original_translation: String,
    pub corrected_translation: String,
    pub source_language: String,
    pub target_language: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_defaults_to_microphone_for_old_clients() {
        let request: StartSessionRequest = serde_json::from_value(serde_json::json!({
            "source_language": "zh-CN",
            "target_language": "en-US"
        }))
        .expect("valid session request");

        assert_eq!(request.audio_source, AudioSource::Microphone);
        assert_eq!(request.asr_engine, AsrEngine::AppleSpeech);
    }

    #[test]
    fn session_accepts_system_audio() {
        let request: StartSessionRequest = serde_json::from_value(serde_json::json!({
            "source_language": "en-US",
            "target_language": "zh-CN",
            "audio_source": "system_audio"
        }))
        .expect("valid system audio request");

        assert_eq!(request.audio_source, AudioSource::SystemAudio);
        assert_eq!(request.audio_source.bridge_argument(), "system_audio");
    }

    #[test]
    fn session_accepts_replaceable_asr_engine() {
        let request: StartSessionRequest = serde_json::from_value(serde_json::json!({
            "source_language": "en-US",
            "target_language": "zh-CN",
            "asr_engine": "sherpa_onnx"
        }))
        .expect("valid ASR engine");

        assert_eq!(request.asr_engine, AsrEngine::SherpaOnnx);
        assert_eq!(request.asr_engine.id(), "sherpa_onnx");
    }
}

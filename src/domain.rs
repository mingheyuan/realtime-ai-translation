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

#[derive(Debug, Clone, Deserialize)]
pub struct StartSessionRequest {
    pub source_language: String,
    pub target_language: String,
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

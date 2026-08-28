use std::{
    collections::{HashMap, VecDeque},
    fs,
    path::PathBuf,
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::domain::{AsrEngine, AudioSource, CorrectionRequest, SegmentSnapshot};

#[derive(Debug, Clone, Copy)]
pub enum SegmentFinalizationReason {
    IdleOrBoundary,
    AsrFinal,
    SessionStop,
}

#[derive(Debug, Clone)]
pub struct SessionMetricDescriptor {
    pub session_id: Uuid,
    pub asr_engine: AsrEngine,
    pub audio_source: AudioSource,
    pub source_language: String,
    pub target_language: String,
    pub reference_context_chars: usize,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct DistributionSummary {
    pub count: usize,
    pub average: Option<f64>,
    pub p50: Option<u128>,
    pub p95: Option<u128>,
    pub maximum: Option<u128>,
}

impl DistributionSummary {
    fn from_samples(samples: &[u128]) -> Self {
        if samples.is_empty() {
            return Self::default();
        }
        let mut sorted = samples.to_vec();
        sorted.sort_unstable();
        let sum = sorted.iter().copied().sum::<u128>();
        Self {
            count: sorted.len(),
            average: Some(sum as f64 / sorted.len() as f64),
            p50: percentile(&sorted, 50),
            p95: percentile(&sorted, 95),
            maximum: sorted.last().copied(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionMetricsSnapshot {
    pub schema_version: u32,
    pub session_id: Uuid,
    pub captured_at_unix_ms: u128,
    pub running: bool,
    pub duration_ms: u128,
    pub asr_engine: String,
    pub audio_source: String,
    pub source_language: String,
    pub target_language: String,
    pub reference_context_chars: usize,

    pub asr_startup_ms: Option<u128>,
    pub first_asr_text_ms: Option<u128>,
    pub asr_partial_updates: usize,
    pub asr_partial_interval_ms: DistributionSummary,
    pub asr_revision_events: usize,
    pub asr_revision_rate_percent: Option<f64>,

    pub finalized_segments: usize,
    pub short_segments: usize,
    pub short_segment_rate_percent: Option<f64>,
    pub segment_characters: DistributionSummary,
    pub segment_duration_ms: DistributionSummary,
    pub finalization_silence_ms: DistributionSummary,
    pub idle_or_boundary_segments: usize,
    pub asr_final_segments: usize,
    pub stop_segments: usize,

    pub translation_calls: usize,
    pub translation_failures: usize,
    pub translation_model_latency_ms: DistributionSummary,
    pub draft_ready_ms: DistributionSummary,
    pub final_draft_ready_ms: DistributionSummary,

    pub llm_calls: usize,
    pub llm_failures: usize,
    pub llm_latency_ms: DistributionSummary,
    pub llm_input_characters: DistributionSummary,
    pub llm_reference_characters: DistributionSummary,
    pub llm_source_revision_rate_percent: Option<f64>,
    pub llm_translation_revision_rate_percent: Option<f64>,
    pub final_ready_ms: DistributionSummary,

    pub user_corrections: usize,
    pub user_source_revision_rate_percent: Option<f64>,
    pub user_translation_revision_rate_percent: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MetricsResponse {
    pub current: Option<SessionMetricsSnapshot>,
    pub last_completed: Option<SessionMetricsSnapshot>,
    pub recent_completed: Vec<SessionMetricsSnapshot>,
    pub baseline: Option<SessionMetricsSnapshot>,
}

pub struct MetricsStore {
    current: Option<SessionMetrics>,
    completed: VecDeque<SessionMetrics>,
    baseline: Option<SessionMetricsSnapshot>,
    baseline_path: PathBuf,
}

impl MetricsStore {
    pub fn new(baseline_path: PathBuf) -> Self {
        let baseline = fs::read(&baseline_path)
            .ok()
            .and_then(|content| serde_json::from_slice(&content).ok());
        Self {
            current: None,
            completed: VecDeque::with_capacity(10),
            baseline,
            baseline_path,
        }
    }

    pub fn begin(&mut self, descriptor: SessionMetricDescriptor) {
        self.current = Some(SessionMetrics::new(descriptor));
    }

    pub fn record_asr_ready(&mut self) {
        if let Some(current) = self.current.as_mut() {
            current.asr_startup_ms.get_or_insert(current.elapsed_ms());
        }
    }

    pub fn record_asr_snapshot(&mut self, snapshot: &SegmentSnapshot, is_partial: bool) {
        if let Some(current) = self.current.as_mut() {
            current.record_asr_snapshot(snapshot, is_partial);
        }
    }

    pub fn record_segment_final(
        &mut self,
        snapshot: &SegmentSnapshot,
        reason: SegmentFinalizationReason,
    ) {
        if let Some(current) = self.current.as_mut() {
            current.record_segment_final(snapshot, reason);
        }
    }

    pub fn record_translation(&mut self, latency_ms: u128) {
        if let Some(current) = self.current.as_mut() {
            current.translation_model_latency_ms.push(latency_ms);
        }
    }

    pub fn record_translation_failure(&mut self) {
        if let Some(current) = self.current.as_mut() {
            current.translation_failures = current.translation_failures.saturating_add(1);
        }
    }

    pub fn record_draft_ready(&mut self, latency_ms: u128, final_snapshot: bool) {
        if let Some(current) = self.current.as_mut() {
            current.draft_ready_ms.push(latency_ms);
            if final_snapshot {
                current.final_draft_ready_ms.push(latency_ms);
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn record_llm(
        &mut self,
        latency_ms: u128,
        input_chars: usize,
        reference_chars: usize,
        original_source: &str,
        corrected_source: Option<&str>,
        draft_translation: &str,
        final_translation: &str,
    ) {
        if let Some(current) = self.current.as_mut() {
            current.llm_latency_ms.push(latency_ms);
            current.llm_input_characters.push(input_chars as u128);
            current
                .llm_reference_characters
                .push(reference_chars as u128);
            let source_comparison = corrected_source.unwrap_or(original_source);
            add_revision(
                original_source,
                source_comparison,
                &mut current.llm_source_edits,
                &mut current.llm_source_characters,
            );
            add_revision(
                draft_translation,
                final_translation,
                &mut current.llm_translation_edits,
                &mut current.llm_translation_characters,
            );
        }
    }

    pub fn record_llm_failure(&mut self) {
        if let Some(current) = self.current.as_mut() {
            current.llm_failures = current.llm_failures.saturating_add(1);
        }
    }

    pub fn record_final_ready(&mut self, latency_ms: u128) {
        if let Some(current) = self.current.as_mut() {
            current.final_ready_ms.push(latency_ms);
        }
    }

    pub fn record_user_correction(&mut self, correction: &CorrectionRequest) {
        if let Some(current) = self.current.as_mut().or(self.completed.back_mut()) {
            current.user_corrections = current.user_corrections.saturating_add(1);
            add_revision(
                &correction.original_source,
                &correction.corrected_source,
                &mut current.user_source_edits,
                &mut current.user_source_characters,
            );
            add_revision(
                &correction.original_translation,
                &correction.corrected_translation,
                &mut current.user_translation_edits,
                &mut current.user_translation_characters,
            );
        }
    }

    pub fn finish(&mut self) {
        if let Some(current) = self.current.take() {
            self.completed.push_back(current);
            while self.completed.len() > 10 {
                self.completed.pop_front();
            }
        }
    }

    pub fn response(&self) -> MetricsResponse {
        MetricsResponse {
            current: self.current.as_ref().map(|current| current.snapshot(true)),
            last_completed: self
                .completed
                .back()
                .map(|completed| completed.snapshot(false)),
            recent_completed: self
                .completed
                .iter()
                .map(|completed| completed.snapshot(false))
                .collect(),
            baseline: self.baseline.clone(),
        }
    }

    pub fn save_latest_as_baseline(&mut self) -> Result<SessionMetricsSnapshot, std::io::Error> {
        if self.current.is_some() {
            return Err(std::io::Error::other("请先结束当前会话，再保存稳定基线"));
        }
        let latest = self
            .completed
            .back()
            .ok_or_else(|| std::io::Error::other("还没有可设为基线的会话指标"))?;
        let compatible = self
            .completed
            .iter()
            .rev()
            .filter(|candidate| candidate.is_compatible_with(latest))
            .take(3)
            .map(|completed| completed.snapshot(false))
            .collect::<Vec<_>>();
        if compatible.len() < 3 {
            return Err(std::io::Error::other(
                "需要先完成 3 次 ASR、音频来源、语言方向和背景长度一致的会话",
            ));
        }
        let latest = aggregate_snapshots(&compatible).map_err(std::io::Error::other)?;
        let encoded = serde_json::to_vec_pretty(&latest).map_err(std::io::Error::other)?;
        fs::write(&self.baseline_path, encoded)?;
        self.baseline = Some(latest.clone());
        Ok(latest)
    }
}

struct SessionMetrics {
    descriptor: SessionMetricDescriptor,
    started_at: Instant,
    asr_startup_ms: Option<u128>,
    first_asr_text_ms: Option<u128>,
    last_partial_at: Option<Instant>,
    last_asr_at: Option<Instant>,
    asr_partial_intervals: Vec<u128>,
    asr_partial_updates: usize,
    asr_revision_events: usize,
    asr_revised_characters: usize,
    asr_prior_characters: usize,
    previous_segment_text: HashMap<u64, String>,
    segment_started_at: HashMap<u64, Instant>,
    segment_characters: Vec<u128>,
    segment_duration_ms: Vec<u128>,
    finalization_silence_ms: Vec<u128>,
    short_segments: usize,
    idle_or_boundary_segments: usize,
    asr_final_segments: usize,
    stop_segments: usize,
    translation_model_latency_ms: Vec<u128>,
    translation_failures: usize,
    draft_ready_ms: Vec<u128>,
    final_draft_ready_ms: Vec<u128>,
    llm_latency_ms: Vec<u128>,
    llm_failures: usize,
    llm_input_characters: Vec<u128>,
    llm_reference_characters: Vec<u128>,
    llm_source_edits: usize,
    llm_source_characters: usize,
    llm_translation_edits: usize,
    llm_translation_characters: usize,
    final_ready_ms: Vec<u128>,
    user_corrections: usize,
    user_source_edits: usize,
    user_source_characters: usize,
    user_translation_edits: usize,
    user_translation_characters: usize,
}

impl SessionMetrics {
    fn new(descriptor: SessionMetricDescriptor) -> Self {
        Self {
            descriptor,
            started_at: Instant::now(),
            asr_startup_ms: None,
            first_asr_text_ms: None,
            last_partial_at: None,
            last_asr_at: None,
            asr_partial_intervals: Vec::new(),
            asr_partial_updates: 0,
            asr_revision_events: 0,
            asr_revised_characters: 0,
            asr_prior_characters: 0,
            previous_segment_text: HashMap::new(),
            segment_started_at: HashMap::new(),
            segment_characters: Vec::new(),
            segment_duration_ms: Vec::new(),
            finalization_silence_ms: Vec::new(),
            short_segments: 0,
            idle_or_boundary_segments: 0,
            asr_final_segments: 0,
            stop_segments: 0,
            translation_model_latency_ms: Vec::new(),
            translation_failures: 0,
            draft_ready_ms: Vec::new(),
            final_draft_ready_ms: Vec::new(),
            llm_latency_ms: Vec::new(),
            llm_failures: 0,
            llm_input_characters: Vec::new(),
            llm_reference_characters: Vec::new(),
            llm_source_edits: 0,
            llm_source_characters: 0,
            llm_translation_edits: 0,
            llm_translation_characters: 0,
            final_ready_ms: Vec::new(),
            user_corrections: 0,
            user_source_edits: 0,
            user_source_characters: 0,
            user_translation_edits: 0,
            user_translation_characters: 0,
        }
    }

    fn elapsed_ms(&self) -> u128 {
        self.started_at.elapsed().as_millis()
    }

    fn is_compatible_with(&self, other: &Self) -> bool {
        self.descriptor.asr_engine == other.descriptor.asr_engine
            && self.descriptor.audio_source == other.descriptor.audio_source
            && self.descriptor.source_language == other.descriptor.source_language
            && self.descriptor.target_language == other.descriptor.target_language
            && self.descriptor.reference_context_chars == other.descriptor.reference_context_chars
    }

    fn record_asr_snapshot(&mut self, snapshot: &SegmentSnapshot, is_partial: bool) {
        let now = Instant::now();
        self.first_asr_text_ms.get_or_insert(self.elapsed_ms());
        self.last_asr_at = Some(now);
        self.segment_started_at
            .entry(snapshot.segment_id)
            .or_insert(now);
        if is_partial {
            self.asr_partial_updates = self.asr_partial_updates.saturating_add(1);
            if let Some(previous_at) = self.last_partial_at.replace(now) {
                self.asr_partial_intervals
                    .push(now.saturating_duration_since(previous_at).as_millis());
            }
        }

        if let Some(previous) = self
            .previous_segment_text
            .insert(snapshot.segment_id, snapshot.source_text.clone())
        {
            let previous_length = previous.chars().count();
            if previous_length > 0 {
                let stable_prefix = common_prefix_characters(&previous, &snapshot.source_text);
                let revised = previous_length.saturating_sub(stable_prefix);
                self.asr_prior_characters =
                    self.asr_prior_characters.saturating_add(previous_length);
                self.asr_revised_characters = self.asr_revised_characters.saturating_add(revised);
                if revised > 0 {
                    self.asr_revision_events = self.asr_revision_events.saturating_add(1);
                }
            }
        }
    }

    fn record_segment_final(
        &mut self,
        snapshot: &SegmentSnapshot,
        reason: SegmentFinalizationReason,
    ) {
        let now = Instant::now();
        self.segment_characters
            .push(meaningful_character_count(&snapshot.source_text) as u128);
        if is_short_segment(&snapshot.source_text) {
            self.short_segments = self.short_segments.saturating_add(1);
        }
        if let Some(started_at) = self.segment_started_at.remove(&snapshot.segment_id) {
            self.segment_duration_ms
                .push(now.saturating_duration_since(started_at).as_millis());
        }
        if let Some(last_asr_at) = self.last_asr_at {
            self.finalization_silence_ms
                .push(now.saturating_duration_since(last_asr_at).as_millis());
        }
        self.previous_segment_text.remove(&snapshot.segment_id);
        match reason {
            SegmentFinalizationReason::IdleOrBoundary => {
                self.idle_or_boundary_segments = self.idle_or_boundary_segments.saturating_add(1);
            }
            SegmentFinalizationReason::AsrFinal => {
                self.asr_final_segments = self.asr_final_segments.saturating_add(1);
            }
            SegmentFinalizationReason::SessionStop => {
                self.stop_segments = self.stop_segments.saturating_add(1);
            }
        }
    }

    fn snapshot(&self, running: bool) -> SessionMetricsSnapshot {
        let finalized_segments = self.segment_characters.len();
        SessionMetricsSnapshot {
            schema_version: 1,
            session_id: self.descriptor.session_id,
            captured_at_unix_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis(),
            running,
            duration_ms: self.elapsed_ms(),
            asr_engine: self.descriptor.asr_engine.id().to_owned(),
            audio_source: self.descriptor.audio_source.bridge_argument().to_owned(),
            source_language: self.descriptor.source_language.clone(),
            target_language: self.descriptor.target_language.clone(),
            reference_context_chars: self.descriptor.reference_context_chars,
            asr_startup_ms: self.asr_startup_ms,
            first_asr_text_ms: self.first_asr_text_ms,
            asr_partial_updates: self.asr_partial_updates,
            asr_partial_interval_ms: DistributionSummary::from_samples(&self.asr_partial_intervals),
            asr_revision_events: self.asr_revision_events,
            asr_revision_rate_percent: percentage(
                self.asr_revised_characters,
                self.asr_prior_characters,
            ),
            finalized_segments,
            short_segments: self.short_segments,
            short_segment_rate_percent: percentage(self.short_segments, finalized_segments),
            segment_characters: DistributionSummary::from_samples(&self.segment_characters),
            segment_duration_ms: DistributionSummary::from_samples(&self.segment_duration_ms),
            finalization_silence_ms: DistributionSummary::from_samples(
                &self.finalization_silence_ms,
            ),
            idle_or_boundary_segments: self.idle_or_boundary_segments,
            asr_final_segments: self.asr_final_segments,
            stop_segments: self.stop_segments,
            translation_calls: self.translation_model_latency_ms.len(),
            translation_failures: self.translation_failures,
            translation_model_latency_ms: DistributionSummary::from_samples(
                &self.translation_model_latency_ms,
            ),
            draft_ready_ms: DistributionSummary::from_samples(&self.draft_ready_ms),
            final_draft_ready_ms: DistributionSummary::from_samples(&self.final_draft_ready_ms),
            llm_calls: self.llm_latency_ms.len(),
            llm_failures: self.llm_failures,
            llm_latency_ms: DistributionSummary::from_samples(&self.llm_latency_ms),
            llm_input_characters: DistributionSummary::from_samples(&self.llm_input_characters),
            llm_reference_characters: DistributionSummary::from_samples(
                &self.llm_reference_characters,
            ),
            llm_source_revision_rate_percent: percentage(
                self.llm_source_edits,
                self.llm_source_characters,
            ),
            llm_translation_revision_rate_percent: percentage(
                self.llm_translation_edits,
                self.llm_translation_characters,
            ),
            final_ready_ms: DistributionSummary::from_samples(&self.final_ready_ms),
            user_corrections: self.user_corrections,
            user_source_revision_rate_percent: percentage(
                self.user_source_edits,
                self.user_source_characters,
            ),
            user_translation_revision_rate_percent: percentage(
                self.user_translation_edits,
                self.user_translation_characters,
            ),
        }
    }
}

fn percentile(sorted: &[u128], percentile: usize) -> Option<u128> {
    if sorted.is_empty() {
        return None;
    }
    let index = (sorted.len() - 1).saturating_mul(percentile).div_ceil(100);
    sorted.get(index).copied()
}

fn aggregate_snapshots(
    snapshots: &[SessionMetricsSnapshot],
) -> Result<SessionMetricsSnapshot, serde_json::Error> {
    let values = snapshots
        .iter()
        .map(serde_json::to_value)
        .collect::<Result<Vec<_>, _>>()?;
    let mut aggregate: SessionMetricsSnapshot = serde_json::from_value(median_json(&values))?;
    aggregate.running = false;
    aggregate.captured_at_unix_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    Ok(aggregate)
}

fn median_json(values: &[serde_json::Value]) -> serde_json::Value {
    let Some(template) = values.last() else {
        return serde_json::Value::Null;
    };
    match template {
        serde_json::Value::Object(object) => serde_json::Value::Object(
            object
                .keys()
                .map(|key| {
                    let children = values
                        .iter()
                        .filter_map(|value| value.get(key).cloned())
                        .collect::<Vec<_>>();
                    (key.clone(), median_json(&children))
                })
                .collect(),
        ),
        serde_json::Value::Number(_) => median_number(values).unwrap_or_else(|| template.clone()),
        serde_json::Value::Null => median_number(values).unwrap_or(serde_json::Value::Null),
        _ => template.clone(),
    }
}

fn median_number(values: &[serde_json::Value]) -> Option<serde_json::Value> {
    let unsigned = values
        .iter()
        .filter_map(serde_json::Value::as_u64)
        .collect::<Vec<_>>();
    if !unsigned.is_empty()
        && values
            .iter()
            .filter(|value| !value.is_null())
            .all(|value| value.as_u64().is_some())
    {
        let mut sorted = unsigned;
        sorted.sort_unstable();
        return Some(serde_json::Value::Number(sorted[sorted.len() / 2].into()));
    }

    let mut floating = values
        .iter()
        .filter_map(serde_json::Value::as_f64)
        .collect::<Vec<_>>();
    if floating.is_empty() {
        return None;
    }
    floating.sort_by(f64::total_cmp);
    serde_json::Number::from_f64(floating[floating.len() / 2]).map(serde_json::Value::Number)
}

fn percentage(numerator: usize, denominator: usize) -> Option<f64> {
    (denominator > 0).then(|| numerator as f64 * 100.0 / denominator as f64)
}

fn common_prefix_characters(left: &str, right: &str) -> usize {
    left.chars()
        .zip(right.chars())
        .take_while(|(left, right)| left == right)
        .count()
}

fn meaningful_character_count(text: &str) -> usize {
    text.chars()
        .filter(|character| character.is_alphanumeric())
        .count()
}

fn is_short_segment(text: &str) -> bool {
    let words = text
        .split_whitespace()
        .filter(|word| !word.is_empty())
        .count();
    let non_ascii = text
        .chars()
        .filter(|character| character.is_alphanumeric() && !character.is_ascii())
        .count();
    if non_ascii > 0 {
        non_ascii < 10 && words < 6
    } else {
        words < 6
    }
}

fn add_revision(
    original: &str,
    revised: &str,
    edit_total: &mut usize,
    character_total: &mut usize,
) {
    let original_length = original.chars().count();
    *edit_total = edit_total.saturating_add(character_edit_distance(original, revised));
    *character_total = character_total.saturating_add(original_length.max(1));
}

fn character_edit_distance(left: &str, right: &str) -> usize {
    let right = right.chars().collect::<Vec<_>>();
    let mut previous = (0..=right.len()).collect::<Vec<_>>();
    let mut current = vec![0; right.len() + 1];
    for (left_index, left_character) in left.chars().enumerate() {
        current[0] = left_index + 1;
        for (right_index, right_character) in right.iter().enumerate() {
            let substitution = usize::from(left_character != *right_character);
            current[right_index + 1] = (previous[right_index + 1] + 1)
                .min(current[right_index] + 1)
                .min(previous[right_index] + substitution);
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[right.len()]
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;
    use crate::domain::CaptionState;

    fn descriptor() -> SessionMetricDescriptor {
        SessionMetricDescriptor {
            session_id: Uuid::new_v4(),
            asr_engine: AsrEngine::SherpaOnnx,
            audio_source: AudioSource::SystemAudio,
            source_language: "zh-CN".to_owned(),
            target_language: "en-US".to_owned(),
            reference_context_chars: 1200,
        }
    }

    fn snapshot(text: &str) -> SegmentSnapshot {
        SegmentSnapshot {
            segment_id: 1,
            revision: 1,
            state: CaptionState::Partial,
            source_text: text.to_owned(),
        }
    }

    #[test]
    fn distribution_uses_nearest_rank_percentiles() {
        let summary = DistributionSummary::from_samples(&[10, 20, 30, 40, 100]);
        assert_eq!(summary.p50, Some(30));
        assert_eq!(summary.p95, Some(100));
        assert_eq!(summary.average, Some(40.0));
    }

    #[test]
    fn appended_asr_text_is_not_counted_as_a_revision() {
        let directory = TempDir::new().expect("metrics directory");
        let mut metrics = MetricsStore::new(directory.path().join("baseline.json"));
        metrics.begin(descriptor());
        metrics.record_asr_snapshot(&snapshot("我想做"), true);
        metrics.record_asr_snapshot(&snapshot("我想做实时翻译"), true);
        let response = metrics.response();
        let current = response.current.expect("current metrics");
        assert_eq!(current.asr_revision_events, 0);
        assert_eq!(current.asr_revision_rate_percent, Some(0.0));
    }

    #[test]
    fn changed_visible_prefix_is_counted_as_an_asr_revision() {
        let directory = TempDir::new().expect("metrics directory");
        let mut metrics = MetricsStore::new(directory.path().join("baseline.json"));
        metrics.begin(descriptor());
        metrics.record_asr_snapshot(&snapshot("我想使用低配色可"), true);
        metrics.record_asr_snapshot(&snapshot("我想使用 DeepSeek"), true);
        let response = metrics.response();
        let current = response.current.expect("current metrics");
        assert_eq!(current.asr_revision_events, 1);
        assert!(current.asr_revision_rate_percent.unwrap_or_default() > 0.0);
    }

    #[test]
    fn completed_session_can_be_persisted_as_the_next_baseline() {
        let directory = TempDir::new().expect("metrics directory");
        let path = directory.path().join("baseline.json");
        let mut metrics = MetricsStore::new(path.clone());
        let mut compatible_descriptor = descriptor();
        for latency in [100, 300, 200] {
            compatible_descriptor.session_id = Uuid::new_v4();
            metrics.begin(compatible_descriptor.clone());
            metrics.record_asr_snapshot(&snapshot("这是一个足够长的测试句子"), true);
            metrics.record_segment_final(
                &snapshot("这是一个足够长的测试句子"),
                SegmentFinalizationReason::AsrFinal,
            );
            metrics.record_translation(latency);
            metrics.finish();
        }
        let baseline = metrics.save_latest_as_baseline().expect("save baseline");

        assert_eq!(baseline.finalized_segments, 1);
        assert_eq!(baseline.translation_model_latency_ms.p50, Some(200));
        let reloaded = MetricsStore::new(path).response();
        assert_eq!(reloaded.baseline, Some(baseline));
    }

    #[test]
    fn baseline_requires_three_compatible_sessions() {
        let directory = TempDir::new().expect("metrics directory");
        let mut metrics = MetricsStore::new(directory.path().join("baseline.json"));
        metrics.begin(descriptor());
        metrics.finish();

        let error = metrics
            .save_latest_as_baseline()
            .expect_err("one session is not a stable baseline");
        assert!(error.to_string().contains("3 次"));
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TranslationPhase {
    Collecting,
    TranslatingWindow,
    Finalizing,
    Finalized,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceChunk {
    // Exact source slice, including boundary whitespace. This is used to move
    // the committed cursor without losing byte-for-byte prefix matching.
    pub raw_source: String,
    // Trimmed model input. MarianMT does not need boundary whitespace.
    pub model_text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranslationPlan {
    pub segment_id: u64,
    pub revision: u64,
    pub source_text: String,
    pub chunks_to_commit: Vec<SourceChunk>,
    pub mutable_window: String,
    pub full_reconcile: bool,
    committed_source_before: String,
}

#[derive(Debug)]
pub struct TranslationStateMachine {
    segment_id: Option<u64>,
    latest_revision: u64,
    previous_hypothesis: String,
    committed_source: String,
    committed_translation: String,
    phase: TranslationPhase,
    limits_override: Option<(usize, usize)>,
}

impl Default for TranslationStateMachine {
    fn default() -> Self {
        Self {
            segment_id: None,
            latest_revision: 0,
            previous_hypothesis: String::new(),
            committed_source: String::new(),
            committed_translation: String::new(),
            phase: TranslationPhase::Collecting,
            limits_override: None,
        }
    }
}

impl TranslationStateMachine {
    pub fn reset(&mut self) {
        let limits_override = self.limits_override;
        *self = Self {
            limits_override,
            ..Self::default()
        };
    }

    pub fn begin(
        &mut self,
        segment_id: u64,
        revision: u64,
        source_text: &str,
        source_language: &str,
        is_final: bool,
    ) -> Option<TranslationPlan> {
        let source = source_text.trim();
        if source.is_empty() {
            return None;
        }
        if self.segment_id != Some(segment_id) {
            self.reset_segment(segment_id);
        }
        if revision <= self.latest_revision {
            return None;
        }

        // Apple Speech can revise words well before the current tail. If that
        // crosses our commit boundary, discard the cached prefix and rebuild.
        if !source.starts_with(&self.committed_source) {
            self.reset_segment(segment_id);
        }

        self.latest_revision = revision;
        if is_final {
            self.previous_hypothesis = source.to_owned();
            self.phase = TranslationPhase::Finalizing;
            return Some(TranslationPlan {
                segment_id,
                revision,
                source_text: source.to_owned(),
                chunks_to_commit: Vec::new(),
                mutable_window: source.to_owned(),
                full_reconcile: true,
                committed_source_before: self.committed_source.clone(),
            });
        }

        let stable_end = common_prefix_byte_len(&self.previous_hypothesis, source);
        self.previous_hypothesis = source.to_owned();
        let (max_window_chars, rollback_chars) = self
            .limits_override
            .unwrap_or_else(|| language_limits(source_language));
        let mut cursor = self.committed_source.len();
        let mut chunks_to_commit = Vec::new();

        while source[cursor..].chars().count() > max_window_chars && stable_end > cursor {
            let stable_available = source[cursor..stable_end].chars().count();
            if stable_available <= rollback_chars + 1 {
                break;
            }
            let desired = (max_window_chars.saturating_sub(rollback_chars))
                .max(1)
                .min(stable_available - rollback_chars);
            let cut = choose_boundary(&source[cursor..stable_end], desired);
            if cut == 0 {
                break;
            }
            let end = cursor + cut;
            let raw_source = source[cursor..end].to_owned();
            let model_text = raw_source.trim().to_owned();
            if model_text.is_empty() {
                cursor = end;
                continue;
            }
            chunks_to_commit.push(SourceChunk {
                raw_source,
                model_text,
            });
            cursor = end;
        }

        self.phase = TranslationPhase::TranslatingWindow;
        Some(TranslationPlan {
            segment_id,
            revision,
            source_text: source.to_owned(),
            chunks_to_commit,
            mutable_window: source[cursor..].trim().to_owned(),
            full_reconcile: false,
            committed_source_before: self.committed_source.clone(),
        })
    }

    pub fn finish_window(
        &mut self,
        plan: &TranslationPlan,
        committed_chunk_translations: &[String],
        mutable_translation: &str,
        target_language: &str,
    ) -> Option<String> {
        if !self.matches(plan, TranslationPhase::TranslatingWindow)
            || plan.full_reconcile
            || committed_chunk_translations.len() != plan.chunks_to_commit.len()
        {
            return None;
        }
        for (chunk, translation) in plan
            .chunks_to_commit
            .iter()
            .zip(committed_chunk_translations)
        {
            self.committed_source.push_str(&chunk.raw_source);
            self.committed_translation =
                join_translation(&self.committed_translation, translation, target_language);
        }
        let assembled = join_translation(
            &self.committed_translation,
            mutable_translation,
            target_language,
        );
        self.phase = TranslationPhase::Collecting;
        Some(assembled)
    }

    pub fn finish_final(&mut self, plan: &TranslationPlan, translation: &str) -> Option<String> {
        if !self.matches(plan, TranslationPhase::Finalizing) || !plan.full_reconcile {
            return None;
        }
        self.committed_source = plan.source_text.clone();
        self.committed_translation = translation.trim().to_owned();
        self.phase = TranslationPhase::Finalized;
        Some(self.committed_translation.clone())
    }

    fn reset_segment(&mut self, segment_id: u64) {
        self.segment_id = Some(segment_id);
        self.latest_revision = 0;
        self.previous_hypothesis.clear();
        self.committed_source.clear();
        self.committed_translation.clear();
        self.phase = TranslationPhase::Collecting;
    }

    fn matches(&self, plan: &TranslationPlan, phase: TranslationPhase) -> bool {
        self.segment_id == Some(plan.segment_id)
            && self.latest_revision == plan.revision
            && self.phase == phase
            && self.committed_source == plan.committed_source_before
    }

    #[cfg(test)]
    fn with_limits(max_window_chars: usize, rollback_chars: usize) -> Self {
        Self {
            limits_override: Some((max_window_chars, rollback_chars)),
            ..Self::default()
        }
    }
}

fn language_limits(language: &str) -> (usize, usize) {
    if language.to_ascii_lowercase().starts_with("zh") {
        (36, 12)
    } else {
        // Character limits avoid splitting inside UTF-8 while roughly mapping
        // to 12-18 English words for ordinary speech.
        (96, 32)
    }
}

fn common_prefix_byte_len(left: &str, right: &str) -> usize {
    let mut length = 0;
    for (left_char, right_char) in left.chars().zip(right.chars()) {
        if left_char != right_char {
            break;
        }
        length += left_char.len_utf8();
    }
    length
}

fn choose_boundary(text: &str, desired_chars: usize) -> usize {
    let mut exact = 0;
    let mut preferred = None;
    let minimum = desired_chars / 2;
    for (index, character) in text.char_indices().take(desired_chars) {
        exact = index + character.len_utf8();
        let count = text[..exact].chars().count();
        if count >= minimum && is_boundary(character) {
            preferred = Some(exact);
        }
    }
    preferred.unwrap_or(exact)
}

fn is_boundary(character: char) -> bool {
    character.is_whitespace()
        || matches!(
            character,
            '，' | '。' | '；' | '！' | '？' | ',' | '.' | ';' | '!' | '?'
        )
}

fn join_translation(prefix: &str, suffix: &str, target_language: &str) -> String {
    let prefix = prefix.trim();
    let suffix = suffix.trim();
    if prefix.is_empty() {
        return suffix.to_owned();
    }
    if suffix.is_empty() {
        return prefix.to_owned();
    }
    if target_language.to_ascii_lowercase().starts_with("zh")
        || prefix.ends_with(char::is_whitespace)
        || suffix.starts_with([',', '.', ';', '!', '?', '，', '。', '；', '！', '？'])
    {
        format!("{prefix}{suffix}")
    } else {
        format!("{prefix} {suffix}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_prefix_moves_out_of_the_mutable_window() {
        let mut machine = TranslationStateMachine::with_limits(12, 4);
        let first = machine
            .begin(1, 1, "abcdefghijkl", "en", false)
            .expect("first plan");
        assert!(first.chunks_to_commit.is_empty());
        machine
            .finish_window(&first, &[], "first translation", "en")
            .expect("first result");

        let second = machine
            .begin(1, 2, "abcdefghijklmnop", "en", false)
            .expect("second plan");
        assert_eq!(second.chunks_to_commit[0].model_text, "abcdefgh");
        assert_eq!(second.mutable_window, "ijklmnop");
        let assembled = machine
            .finish_window(&second, &["ABCDEFGH".to_owned()], "IJKLMNOP", "en")
            .expect("second result");
        assert_eq!(assembled, "ABCDEFGH IJKLMNOP");

        let third = machine
            .begin(1, 3, "abcdefghijklmnopqrst", "en", false)
            .expect("third plan");
        assert_eq!(third.mutable_window, "ijklmnopqrst");
        assert!(!third.mutable_window.starts_with("a"));
    }

    #[test]
    fn correction_before_commit_boundary_rolls_back_cached_translation() {
        let mut machine = TranslationStateMachine::with_limits(10, 3);
        let first = machine.begin(1, 1, "abcdefghij", "en", false).unwrap();
        machine.finish_window(&first, &[], "old", "en").unwrap();
        let second = machine.begin(1, 2, "abcdefghijklmn", "en", false).unwrap();
        machine
            .finish_window(&second, &["prefix".to_owned()], "tail", "en")
            .unwrap();

        let corrected = machine
            .begin(1, 3, "abXdefghijklmnop", "en", false)
            .unwrap();
        assert!(corrected.chunks_to_commit.is_empty());
        assert_eq!(corrected.mutable_window, "abXdefghijklmnop");
    }

    #[test]
    fn final_reconciles_the_complete_sentence_once() {
        let mut machine = TranslationStateMachine::with_limits(10, 3);
        let partial = machine.begin(7, 1, "abcdefghij", "en", false).unwrap();
        machine.finish_window(&partial, &[], "draft", "en").unwrap();
        let final_plan = machine.begin(7, 2, "abcdefghijkl", "en", true).unwrap();
        assert!(final_plan.full_reconcile);
        assert_eq!(final_plan.mutable_window, "abcdefghijkl");
        assert_eq!(
            machine.finish_final(&final_plan, "natural final"),
            Some("natural final".to_owned())
        );
        assert_eq!(machine.phase, TranslationPhase::Finalized);
    }

    #[test]
    fn a_new_generation_rejects_a_late_window_result() {
        let mut machine = TranslationStateMachine::with_limits(12, 4);
        let old = machine.begin(1, 1, "abcdefgh", "en", false).unwrap();
        let current = machine.begin(1, 2, "abcdefghijkl", "en", false).unwrap();

        assert_eq!(machine.finish_window(&old, &[], "OLD", "en"), None);
        assert_eq!(
            machine.finish_window(&current, &[], "CURRENT", "en"),
            Some("CURRENT".to_owned())
        );
    }
}

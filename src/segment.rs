use std::time::{Duration, Instant};

use crate::domain::{CaptionState, SegmentSnapshot};

#[derive(Debug)]
pub struct SegmentManager {
    committed_prefix: String,
    latest_full_text: String,
    current_segment_text: String,
    segment_id: u64,
    revision: u64,
    updated_at: Option<Instant>,
    idle_timeout: Duration,
}

impl SegmentManager {
    pub fn new(idle_timeout: Duration) -> Self {
        Self {
            committed_prefix: String::new(),
            latest_full_text: String::new(),
            current_segment_text: String::new(),
            segment_id: 1,
            revision: 0,
            updated_at: None,
            idle_timeout,
        }
    }

    pub fn update(&mut self, full_text: &str, now: Instant) -> Option<SegmentSnapshot> {
        let text = full_text.trim();
        if text.is_empty() || text == self.latest_full_text {
            return None;
        }
        // Apple Speech can briefly roll a cumulative hypothesis back to a
        // shorter prefix after we have committed a sentence. That is not a new
        // segment and must not push a duplicate transcript into history.
        if !self.committed_prefix.is_empty() && self.committed_prefix.starts_with(text) {
            return None;
        }
        let current = if self.committed_prefix.is_empty() {
            text
        } else if let Some(suffix) = text.strip_prefix(&self.committed_prefix) {
            suffix.trim()
        } else if let Some(boundary) = aligned_committed_boundary(&self.committed_prefix, text) {
            text[boundary..].trim_start_matches(is_segment_separator)
        } else {
            // A genuinely unrelated hypothesis means Apple started a fresh
            // recognition task. finalize() already advanced segment_id.
            self.committed_prefix.clear();
            text
        };
        self.latest_full_text = text.to_owned();
        self.updated_at = Some(now);
        self.current_segment_text = current.to_owned();
        let current = self.current_segment_text.clone();
        if current.is_empty() {
            return None;
        }
        self.revision = self.revision.saturating_add(1);
        Some(self.snapshot(CaptionState::Partial, &current))
    }

    pub fn should_finalize(&self, now: Instant) -> bool {
        let Some(updated_at) = self.updated_at else {
            return false;
        };
        !self.current_segment_text.is_empty()
            && (now.saturating_duration_since(updated_at) >= self.idle_timeout
                || ends_sentence(&self.current_segment_text))
    }

    pub fn finalize(&mut self) -> Option<SegmentSnapshot> {
        let current = self.current_segment_text.clone();
        if current.is_empty() {
            return None;
        }
        self.revision = self.revision.saturating_add(1);
        let snapshot = self.snapshot(CaptionState::Final, &current);
        self.committed_prefix = self.latest_full_text.clone();
        self.current_segment_text.clear();
        self.segment_id = self.segment_id.saturating_add(1);
        self.revision = 0;
        self.updated_at = None;
        Some(snapshot)
    }

    fn snapshot(&self, state: CaptionState, text: &str) -> SegmentSnapshot {
        SegmentSnapshot {
            segment_id: self.segment_id,
            revision: self.revision,
            state,
            source_text: text.to_owned(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TranscriptToken {
    normalized: String,
    end: usize,
}

fn aligned_committed_boundary(committed: &str, revised: &str) -> Option<usize> {
    let committed_tokens = transcript_tokens(committed);
    let revised_tokens = transcript_tokens(revised);
    if committed_tokens.len() < 6 || revised_tokens.len() < 6 {
        return None;
    }

    let committed_head = &committed_tokens[..committed_tokens.len().min(12)];
    let revised_head = &revised_tokens[..revised_tokens.len().min(16)];
    if lcs_len(committed_head, revised_head) < 4 {
        return None;
    }

    for skipped_tail in 0..=4.min(committed_tokens.len().saturating_sub(3)) {
        let committed_end = committed_tokens.len() - skipped_tail;
        let maximum_anchor = committed_end.min(8);
        for anchor_len in (3..=maximum_anchor).rev() {
            let anchor = &committed_tokens[committed_end - anchor_len..committed_end];
            let Some(revised_end) = last_token_sequence_end(&revised_tokens, anchor) else {
                continue;
            };
            let estimated_boundary = (revised_end + skipped_tail).min(revised_tokens.len());
            return Some(revised_tokens[estimated_boundary - 1].end);
        }
    }
    None
}

fn transcript_tokens(text: &str) -> Vec<TranscriptToken> {
    let mut tokens = Vec::new();
    let mut ascii_start = None;
    for (index, character) in text.char_indices() {
        if character.is_ascii_alphanumeric() || (character == '\'' && ascii_start.is_some()) {
            ascii_start.get_or_insert(index);
            continue;
        }
        if let Some(start) = ascii_start.take() {
            tokens.push(TranscriptToken {
                normalized: text[start..index].to_ascii_lowercase(),
                end: index,
            });
        }
        if character.is_alphanumeric() {
            tokens.push(TranscriptToken {
                normalized: character.to_lowercase().collect(),
                end: index + character.len_utf8(),
            });
        }
    }
    if let Some(start) = ascii_start {
        tokens.push(TranscriptToken {
            normalized: text[start..].to_ascii_lowercase(),
            end: text.len(),
        });
    }
    tokens
}

fn lcs_len(left: &[TranscriptToken], right: &[TranscriptToken]) -> usize {
    let mut previous = vec![0; right.len() + 1];
    for left_token in left {
        let mut current = vec![0; right.len() + 1];
        for (right_index, right_token) in right.iter().enumerate() {
            current[right_index + 1] = if left_token.normalized == right_token.normalized {
                previous[right_index] + 1
            } else {
                current[right_index].max(previous[right_index + 1])
            };
        }
        previous = current;
    }
    previous[right.len()]
}

fn last_token_sequence_end(
    tokens: &[TranscriptToken],
    anchor: &[TranscriptToken],
) -> Option<usize> {
    tokens
        .windows(anchor.len())
        .enumerate()
        .filter(|(_, window)| {
            window
                .iter()
                .zip(anchor)
                .all(|(left, right)| left.normalized == right.normalized)
        })
        .map(|(start, _)| start + anchor.len())
        .next_back()
}

fn is_segment_separator(character: char) -> bool {
    character.is_whitespace()
        || matches!(
            character,
            ',' | '.' | '!' | '?' | ';' | ':' | '，' | '。' | '！' | '？' | '；' | '：' | '…'
        )
}

fn ends_sentence(text: &str) -> bool {
    let text = text.trim_end_matches(|character: char| {
        matches!(
            character,
            '\"' | '\'' | '”' | '’' | ')' | '）' | ']' | '】' | '}' | '》'
        )
    });
    matches!(
        text.chars().last(),
        Some('。' | '！' | '？' | '.' | '!' | '?' | '；' | ';' | '…' | '\n')
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_updates_replace_one_segment_and_idle_finalizes_it() {
        let start = Instant::now();
        let mut manager = SegmentManager::new(Duration::from_millis(900));
        let first = manager.update("我想做", start).expect("first partial");
        let second = manager
            .update("我想做实时翻译", start + Duration::from_millis(200))
            .expect("second partial");
        assert_eq!(first.segment_id, second.segment_id);
        assert!(second.revision > first.revision);
        assert!(!manager.should_finalize(start + Duration::from_millis(800)));
        assert!(manager.should_finalize(start + Duration::from_millis(1_200)));
        let final_segment = manager.finalize().expect("final segment");
        assert_eq!(final_segment.state, CaptionState::Final);
        assert_eq!(final_segment.source_text, "我想做实时翻译");
    }

    #[test]
    fn committed_prefix_is_removed_from_the_next_partial() {
        let start = Instant::now();
        let mut manager = SegmentManager::new(Duration::from_secs(1));
        manager.update("第一句。", start);
        manager.finalize();
        let next = manager
            .update("第一句。第二句", start + Duration::from_secs(1))
            .expect("next segment");
        assert_eq!(next.source_text, "第二句");
        assert_eq!(next.segment_id, 2);
    }

    #[test]
    fn a_shorter_cumulative_hypothesis_does_not_create_duplicate_history() {
        let start = Instant::now();
        let mut manager = SegmentManager::new(Duration::from_secs(1));
        let committed = "This is the first sentence. This is the second sentence.";
        manager.update(committed, start);
        manager.finalize();

        assert!(manager
            .update(
                "This is the first sentence.",
                start + Duration::from_millis(100),
            )
            .is_none());
        assert!(manager.finalize().is_none());
        let next = manager
            .update(
                "This is the first sentence. This is the second sentence. New words",
                start + Duration::from_millis(200),
            )
            .expect("new suffix");
        assert_eq!(next.segment_id, 2);
        assert_eq!(next.source_text, "New words");
    }

    #[test]
    fn a_fresh_apple_recognition_task_uses_the_next_segment_id_once() {
        let start = Instant::now();
        let mut manager = SegmentManager::new(Duration::from_secs(1));
        manager.update("First task text.", start);
        manager.finalize();

        let next = manager
            .update("Fresh task text", start + Duration::from_millis(100))
            .expect("fresh task");
        assert_eq!(next.segment_id, 2);
        assert_eq!(next.source_text, "Fresh task text");
    }

    #[test]
    fn apple_word_corrections_align_to_the_committed_tail() {
        let start = Instant::now();
        let mut manager = SegmentManager::new(Duration::from_secs(1));
        let committed = "Let's see if you know how in your CS 107 withdrew all these memory diagrams, and like here's the heap and the archived caption must remain exactly the same.";
        manager.update(committed, start);
        manager.finalize();

        let revised = "Let's see if you know how in your CS 107 drew all these memory diagrams and like here's the heap the first caption must remain exactly the same. New words belong to the current segment";
        let next = manager
            .update(revised, start + Duration::from_millis(100))
            .expect("aligned suffix");
        assert_eq!(next.segment_id, 2);
        assert_eq!(next.source_text, "New words belong to the current segment");
    }

    #[test]
    fn sentence_boundary_accepts_bilingual_punctuation_and_closing_quotes() {
        assert!(ends_sentence("这一段结束了。”"));
        assert!(ends_sentence("Is this finished?\""));
        assert!(ends_sentence("先暂停；"));
        assert!(ends_sentence("未完待续……"));
        assert!(!ends_sentence("这一段还没有结束"));
    }
}

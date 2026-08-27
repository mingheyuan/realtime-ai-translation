use std::time::{Duration, Instant};

use crate::domain::{CaptionState, SegmentSnapshot};

#[derive(Debug)]
pub struct SegmentManager {
    committed_prefix: String,
    latest_full_text: String,
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
        if !self.committed_prefix.is_empty() && !text.starts_with(&self.committed_prefix) {
            self.committed_prefix.clear();
            self.segment_id = self.segment_id.saturating_add(1);
            self.revision = 0;
        }
        self.latest_full_text = text.to_owned();
        self.updated_at = Some(now);
        let current = self.current_text().to_owned();
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
        !self.current_text().is_empty()
            && (now.saturating_duration_since(updated_at) >= self.idle_timeout
                || ends_sentence(self.current_text()))
    }

    pub fn finalize(&mut self) -> Option<SegmentSnapshot> {
        let current = self.current_text().to_owned();
        if current.is_empty() {
            return None;
        }
        self.revision = self.revision.saturating_add(1);
        let snapshot = self.snapshot(CaptionState::Final, &current);
        self.committed_prefix = self.latest_full_text.clone();
        self.segment_id = self.segment_id.saturating_add(1);
        self.revision = 0;
        self.updated_at = None;
        Some(snapshot)
    }

    fn current_text(&self) -> &str {
        self.latest_full_text
            .strip_prefix(&self.committed_prefix)
            .unwrap_or(&self.latest_full_text)
            .trim()
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
    fn sentence_boundary_accepts_bilingual_punctuation_and_closing_quotes() {
        assert!(ends_sentence("这一段结束了。”"));
        assert!(ends_sentence("Is this finished?\""));
        assert!(ends_sentence("先暂停；"));
        assert!(ends_sentence("未完待续……"));
        assert!(!ends_sentence("这一段还没有结束"));
    }
}

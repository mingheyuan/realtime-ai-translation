use std::{
    cmp::{min, Reverse},
    fs,
    path::PathBuf,
};

use rusqlite::{params, Connection, OptionalExtension};
use thiserror::Error;

use crate::domain::{CorrectionRequest, GlossaryEntry};

#[derive(Debug, Error)]
pub enum DictionaryError {
    #[error("dictionary storage failed: {0}")]
    Storage(#[from] rusqlite::Error),
    #[error("dictionary filesystem failed: {0}")]
    Filesystem(#[from] std::io::Error),
    #[error("dictionary data is invalid: {0}")]
    Invalid(&'static str),
    #[error("dictionary aliases are invalid JSON: {0}")]
    Aliases(#[from] serde_json::Error),
}

#[derive(Debug, Clone)]
pub struct DictionaryStore {
    path: PathBuf,
}

impl DictionaryStore {
    pub fn open(path: PathBuf) -> Result<Self, DictionaryError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let store = Self { path };
        store.migrate()?;
        Ok(store)
    }

    pub fn list(&self) -> Result<Vec<GlossaryEntry>, DictionaryError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT id, source, source_language, target, target_language, aliases_json,
                    domain, confidence, evidence_count, active
             FROM glossary_entries ORDER BY updated_at_ms DESC, id DESC",
        )?;
        let rows = statement.query_map([], row_to_entry)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn upsert(&self, entry: &GlossaryEntry) -> Result<GlossaryEntry, DictionaryError> {
        entry.validate().map_err(DictionaryError::Invalid)?;
        let source = entry.source.trim();
        let target = entry.target.trim();
        let aliases = normalized_aliases(&entry.aliases, source);
        let aliases_json = serde_json::to_string(&aliases)?;
        let connection = self.connection()?;
        connection.execute(
            "INSERT INTO glossary_entries (
                source, source_language, target, target_language, aliases_json,
                domain, confidence, evidence_count, active, created_at_ms, updated_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, unixepoch('subsec') * 1000,
                       unixepoch('subsec') * 1000)
             ON CONFLICT(source, source_language, target_language) DO UPDATE SET
                target = excluded.target,
                aliases_json = excluded.aliases_json,
                domain = excluded.domain,
                confidence = MAX(glossary_entries.confidence, excluded.confidence),
                evidence_count = MAX(glossary_entries.evidence_count, excluded.evidence_count),
                active = excluded.active,
                updated_at_ms = excluded.updated_at_ms",
            params![
                source,
                entry.source_language,
                target,
                entry.target_language,
                aliases_json,
                entry.domain.trim(),
                entry.confidence,
                entry.evidence_count,
                entry.active,
            ],
        )?;
        self.find(source, &entry.source_language, &entry.target_language)?
            .ok_or(DictionaryError::Invalid("saved entry was not found"))
    }

    pub fn delete(&self, id: i64) -> Result<bool, DictionaryError> {
        let connection = self.connection()?;
        Ok(connection.execute("DELETE FROM glossary_entries WHERE id = ?1", [id])? > 0)
    }

    pub fn hotwords(&self, source_language: &str) -> Result<Vec<String>, DictionaryError> {
        let mut words = Vec::new();
        for entry in self.list()?.into_iter().filter(|entry| {
            entry.active && language_matches(&entry.source_language, source_language)
        }) {
            push_unique(&mut words, entry.source);
            for alias in entry.aliases {
                push_unique(&mut words, alias);
            }
            if words.len() >= 50 {
                break;
            }
        }
        words.truncate(50);
        Ok(words)
    }

    pub fn relevant(
        &self,
        text: &str,
        source_language: &str,
        target_language: &str,
    ) -> Result<Vec<GlossaryEntry>, DictionaryError> {
        let normalized = text.to_lowercase();
        Ok(self
            .list()?
            .into_iter()
            .filter(|entry| {
                entry.active
                    && language_matches(&entry.source_language, source_language)
                    && language_matches(&entry.target_language, target_language)
                    && (normalized.contains(&entry.source.to_lowercase())
                        || entry
                            .aliases
                            .iter()
                            .any(|alias| normalized.contains(&alias.to_lowercase())))
            })
            .take(20)
            .collect())
    }

    pub fn normalize_source(
        &self,
        text: &str,
        source_language: &str,
    ) -> Result<String, DictionaryError> {
        let mut result = text.to_owned();
        let mut replacements = self
            .list()?
            .into_iter()
            .filter(|entry| {
                entry.active && language_matches(&entry.source_language, source_language)
            })
            .flat_map(|entry| {
                entry
                    .aliases
                    .into_iter()
                    .map(move |alias| (alias, entry.source.clone()))
            })
            .collect::<Vec<_>>();
        replacements.sort_by_key(|left| Reverse(left.0.chars().count()));
        for (alias, canonical) in replacements {
            result = replace_case_insensitive(&result, &alias, &canonical);
        }
        Ok(result)
    }

    pub fn learn_correction(
        &self,
        correction: &CorrectionRequest,
    ) -> Result<Option<GlossaryEntry>, DictionaryError> {
        let (_, corrected_source) = changed_fragment(
            correction.original_source.trim(),
            correction.corrected_source.trim(),
        );
        let (original_source, _) = changed_fragment(
            correction.original_source.trim(),
            correction.corrected_source.trim(),
        );
        let (_, corrected_target) = changed_fragment(
            correction.original_translation.trim(),
            correction.corrected_translation.trim(),
        );
        let source = corrected_source.trim();
        let target = corrected_target.trim();
        if !eligible_term(source) || !eligible_term(target) {
            return Ok(None);
        }
        let aliases = if eligible_term(original_source.trim()) && original_source.trim() != source {
            vec![original_source.trim().to_owned()]
        } else {
            Vec::new()
        };
        let entry = GlossaryEntry {
            id: None,
            source: source.to_owned(),
            source_language: correction.source_language.clone(),
            target: target.to_owned(),
            target_language: correction.target_language.clone(),
            aliases,
            domain: "learned".to_owned(),
            confidence: 1.0,
            evidence_count: 1,
            active: true,
        };
        self.upsert(&entry).map(Some)
    }

    fn migrate(&self) -> Result<(), DictionaryError> {
        let connection = self.connection()?;
        connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS glossary_entries (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                source TEXT NOT NULL,
                source_language TEXT NOT NULL,
                target TEXT NOT NULL,
                target_language TEXT NOT NULL,
                aliases_json TEXT NOT NULL DEFAULT '[]',
                domain TEXT NOT NULL DEFAULT 'general',
                confidence REAL NOT NULL DEFAULT 1.0,
                evidence_count INTEGER NOT NULL DEFAULT 1,
                active INTEGER NOT NULL DEFAULT 1,
                created_at_ms INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL,
                UNIQUE(source, source_language, target_language)
            );",
        )?;
        Ok(())
    }

    fn find(
        &self,
        source: &str,
        source_language: &str,
        target_language: &str,
    ) -> Result<Option<GlossaryEntry>, DictionaryError> {
        let connection = self.connection()?;
        connection
            .query_row(
                "SELECT id, source, source_language, target, target_language, aliases_json,
                        domain, confidence, evidence_count, active
                 FROM glossary_entries
                 WHERE source = ?1 AND source_language = ?2 AND target_language = ?3",
                params![source, source_language, target_language],
                row_to_entry,
            )
            .optional()
            .map_err(Into::into)
    }

    fn connection(&self) -> Result<Connection, rusqlite::Error> {
        Connection::open(&self.path)
    }
}

fn row_to_entry(row: &rusqlite::Row<'_>) -> rusqlite::Result<GlossaryEntry> {
    let aliases_json: String = row.get(5)?;
    let aliases = serde_json::from_str(&aliases_json).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            aliases_json.len(),
            rusqlite::types::Type::Text,
            Box::new(error),
        )
    })?;
    Ok(GlossaryEntry {
        id: row.get(0)?,
        source: row.get(1)?,
        source_language: row.get(2)?,
        target: row.get(3)?,
        target_language: row.get(4)?,
        aliases,
        domain: row.get(6)?,
        confidence: row.get(7)?,
        evidence_count: row.get(8)?,
        active: row.get(9)?,
    })
}

fn normalized_aliases(aliases: &[String], source: &str) -> Vec<String> {
    let mut result = Vec::new();
    for alias in aliases {
        let alias = alias.trim();
        if !alias.is_empty() && alias != source {
            push_unique(&mut result, alias.to_owned());
        }
    }
    result
}

fn push_unique(values: &mut Vec<String>, value: String) {
    if !values
        .iter()
        .any(|existing| existing.eq_ignore_ascii_case(&value))
    {
        values.push(value);
    }
}

fn language_matches(configured: &str, requested: &str) -> bool {
    configured.eq_ignore_ascii_case(requested)
        || configured
            .split(['-', '_'])
            .next()
            .zip(requested.split(['-', '_']).next())
            .is_some_and(|(left, right)| left.eq_ignore_ascii_case(right))
}

fn replace_case_insensitive(text: &str, needle: &str, replacement: &str) -> String {
    if needle.is_empty() {
        return text.to_owned();
    }
    let lower_text = text.to_lowercase();
    let lower_needle = needle.to_lowercase();
    let mut result = String::new();
    let mut cursor = 0;
    while let Some(relative) = lower_text[cursor..].find(&lower_needle) {
        let start = cursor + relative;
        let end = start + needle.len();
        if !text.is_char_boundary(start) || !text.is_char_boundary(end) {
            break;
        }
        result.push_str(&text[cursor..start]);
        result.push_str(replacement);
        cursor = end;
    }
    result.push_str(&text[cursor..]);
    result
}

fn changed_fragment<'a>(before: &'a str, after: &'a str) -> (&'a str, &'a str) {
    let prefix_bytes = before
        .chars()
        .zip(after.chars())
        .take_while(|(left, right)| left == right)
        .map(|(character, _)| character.len_utf8())
        .sum::<usize>();
    let before_tail = &before[prefix_bytes..];
    let after_tail = &after[prefix_bytes..];
    let suffix_chars = before_tail
        .chars()
        .rev()
        .zip(after_tail.chars().rev())
        .take_while(|(left, right)| left == right)
        .count();
    let before_suffix_bytes = before_tail
        .chars()
        .rev()
        .take(suffix_chars)
        .map(char::len_utf8)
        .sum::<usize>();
    let after_suffix_bytes = after_tail
        .chars()
        .rev()
        .take(suffix_chars)
        .map(char::len_utf8)
        .sum::<usize>();
    let before_end = before.len().saturating_sub(before_suffix_bytes);
    let after_end = after.len().saturating_sub(after_suffix_bytes);
    (
        &before[prefix_bytes..min(before_end, before.len())],
        &after[prefix_bytes..min(after_end, after.len())],
    )
}

fn eligible_term(value: &str) -> bool {
    let count = value.chars().count();
    (1..=64).contains(&count) && !value.contains('\n') && !value.contains('\r')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> DictionaryStore {
        let directory = tempfile::tempdir().expect("tempdir").keep();
        DictionaryStore::open(directory.join("dictionary.sqlite3")).expect("store")
    }

    #[test]
    fn aliases_normalize_asr_and_terms_feed_hotwords() {
        let store = store();
        store
            .upsert(&GlossaryEntry {
                id: None,
                source: "Saymore".to_owned(),
                source_language: "zh".to_owned(),
                target: "Saymore".to_owned(),
                target_language: "en".to_owned(),
                aliases: vec!["CM".to_owned()],
                domain: "product".to_owned(),
                confidence: 1.0,
                evidence_count: 2,
                active: true,
            })
            .expect("entry");
        assert_eq!(
            store
                .normalize_source("我们使用 CM 开发", "zh")
                .expect("normalize"),
            "我们使用 Saymore 开发"
        );
        let words = store.hotwords("zh-CN").expect("hotwords");
        assert!(words.contains(&"Saymore".to_owned()));
        assert!(words.contains(&"CM".to_owned()));
    }

    #[test]
    fn explicit_correction_creates_a_bilingual_entry() {
        let store = store();
        let entry = store
            .learn_correction(&CorrectionRequest {
                original_source: "我们使用 CM 开发".to_owned(),
                corrected_source: "我们使用 Saymore 开发".to_owned(),
                original_translation: "We develop with CM".to_owned(),
                corrected_translation: "We develop with Saymore".to_owned(),
                source_language: "zh".to_owned(),
                target_language: "en".to_owned(),
            })
            .expect("correction")
            .expect("entry");
        assert_eq!(entry.source, "Saymore");
        assert_eq!(entry.target, "Saymore");
        assert_eq!(entry.aliases, vec!["CM"]);
    }
}

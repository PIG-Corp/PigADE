//! Stable Markdown section selection for startup workspace memory.
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

use crate::ContextDocument;

pub const MAX_SELECTED_MEMORY_BYTES: usize = 32 * 1024;
const MAX_REVIEW_SECTIONS: usize = 1024;
const MAX_REVIEW_TITLE_CHARS: usize = 160;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemorySelection {
    pub revision: u64,
    pub content_hash: String,
    pub sections: Vec<usize>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemorySection {
    pub index: usize,
    pub title: String,
    pub bytes: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemoryReview {
    pub revision: u64,
    pub content_hash: String,
    pub sections: Vec<MemorySection>,
    pub selected: Vec<usize>,
    pub selected_bytes: usize,
    #[serde(default)]
    pub unavailable_reason: Option<String>,
}

#[derive(Clone, Debug)]
struct SourceSection {
    review: MemorySection,
    start: usize,
    end: usize,
}

fn source_sections(memory: &str) -> Option<Vec<SourceSection>> {
    if memory.is_empty() {
        return Some(Vec::new());
    }
    let mut boundaries: Vec<(usize, String)> = Vec::new();
    let mut offset = 0;
    let mut fence: Option<(u8, usize)> = None;
    for line in memory.split_inclusive('\n') {
        let indent = line.bytes().take_while(|byte| *byte == b' ').count();
        let trimmed = if indent <= 3 { &line[indent..] } else { line };
        let marker = trimmed.as_bytes().first().copied();
        if let Some((fence_char, fence_len)) = fence {
            let run_len = trimmed
                .bytes()
                .take_while(|byte| *byte == fence_char)
                .count();
            if marker == Some(fence_char)
                && run_len >= fence_len
                && trimmed[run_len..].trim().is_empty()
            {
                fence = None;
            }
        } else if marker == Some(b'`') || marker == Some(b'~') {
            let ch = marker.expect("marker checked");
            let run_len = trimmed.bytes().take_while(|byte| *byte == ch).count();
            if run_len >= 3 && (ch != b'`' || !trimmed[run_len..].contains('`')) {
                fence = Some((ch, run_len));
            }
        } else if trimmed.starts_with('#') {
            let hashes = trimmed.bytes().take_while(|byte| *byte == b'#').count();
            if (1..=2).contains(&hashes)
                && trimmed
                    .as_bytes()
                    .get(hashes)
                    .is_none_or(u8::is_ascii_whitespace)
            {
                let needs_preamble = boundaries
                    .first()
                    .map_or(offset > 0, |(start, _)| *start > 0);
                if boundaries.len() + 1 + usize::from(needs_preamble) > MAX_REVIEW_SECTIONS {
                    return None;
                }
                let raw_title = trimmed[hashes..].trim();
                let closing = raw_title.trim_end_matches('#');
                let title = if closing.ends_with([' ', '\t']) {
                    closing.trim_end()
                } else {
                    raw_title
                }
                .chars()
                .take(MAX_REVIEW_TITLE_CHARS)
                .collect();
                boundaries.push((offset, title));
            }
        }
        offset += line.len();
    }
    let mut starts = Vec::new();
    if boundaries.first().is_some_and(|(start, _)| *start > 0) {
        starts.push((0, "Вступление".to_owned()));
    }
    starts.extend(boundaries);
    if starts.is_empty() {
        starts.push((0, "Память".to_owned()));
    }
    Some(
        starts
            .iter()
            .enumerate()
            .map(|(index, (start, title))| {
                let end = starts
                    .get(index + 1)
                    .map_or(memory.len(), |(next, _)| *next);
                SourceSection {
                    review: MemorySection {
                        index,
                        title: title.clone(),
                        bytes: end - start,
                    },
                    start: *start,
                    end,
                }
            })
            .collect(),
    )
}

impl MemoryReview {
    pub fn from_document(
        document: &ContextDocument,
        selection: Option<&MemorySelection>,
        include_memory: bool,
    ) -> Result<Self> {
        ensure!(
            document.memory.len() <= crate::MAX_CONTEXT_BYTES,
            "workspace memory exceeds 1 MiB"
        );
        let Some(source) = source_sections(&document.memory) else {
            ensure!(
                !include_memory
                    && selection.is_none_or(|selection| {
                        selection.revision == document.revision
                            && selection.content_hash == document.content_hash
                            && selection.sections.is_empty()
                    }),
                "workspace memory has too many sections to review; reduce the heading count first"
            );
            return Ok(Self {
                revision: document.revision,
                content_hash: document.content_hash.clone(),
                sections: Vec::new(),
                selected: Vec::new(),
                selected_bytes: 0,
                unavailable_reason: Some(
                    "Память содержит слишком много разделов для предварительного просмотра. Уменьшите число заголовков, чтобы выбирать разделы.".to_owned(),
                ),
            });
        };
        let sections: Vec<_> = source
            .iter()
            .map(|section| section.review.clone())
            .collect();
        let selected = if let Some(selection) = selection {
            ensure!(
                selection.revision == document.revision
                    && selection.content_hash == document.content_hash,
                "workspace memory selection is stale; review again"
            );
            ensure!(
                selection.sections.windows(2).all(|pair| pair[0] < pair[1]),
                "workspace memory section indexes must be unique and ascending"
            );
            ensure!(
                selection
                    .sections
                    .iter()
                    .all(|index| *index < sections.len()),
                "workspace memory section index is invalid"
            );
            selection.sections.clone()
        } else if include_memory {
            sections.iter().map(|section| section.index).collect()
        } else {
            Vec::new()
        };
        let selected_bytes = selected.iter().try_fold(0usize, |sum, index| {
            sum.checked_add(source[*index].review.bytes)
                .ok_or_else(|| anyhow::anyhow!("selected memory byte count overflow"))
        })?;
        ensure!(
            selected_bytes <= MAX_SELECTED_MEMORY_BYTES,
            "selected workspace memory exceeds 32 KiB"
        );
        Ok(Self {
            revision: document.revision,
            content_hash: document.content_hash.clone(),
            sections,
            selected,
            selected_bytes,
            unavailable_reason: None,
        })
    }

    /// Return the exact selected source slices concatenated in document order.
    pub fn selected_text(&self, document: &ContextDocument) -> Result<String> {
        ensure!(
            self.revision == document.revision && self.content_hash == document.content_hash,
            "workspace memory review does not match its source document"
        );
        let source = source_sections(&document.memory)
            .ok_or_else(|| anyhow::anyhow!("workspace memory has too many sections to review"))?;
        ensure!(
            self.sections
                == source
                    .iter()
                    .map(|section| section.review.clone())
                    .collect::<Vec<_>>(),
            "workspace memory section metadata was altered"
        );
        let mut text = String::with_capacity(self.selected_bytes);
        for index in &self.selected {
            let section = source
                .get(*index)
                .ok_or_else(|| anyhow::anyhow!("workspace memory section index is invalid"))?;
            text.push_str(&document.memory[section.start..section.end]);
        }
        ensure!(
            text.len() == self.selected_bytes && text.len() <= MAX_SELECTED_MEMORY_BYTES,
            "selected workspace memory byte count is invalid"
        );
        Ok(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn doc(memory: &str) -> ContextDocument {
        ContextDocument {
            scope_id: Uuid::new_v4(),
            revision: 7,
            instructions: String::new(),
            role: String::new(),
            memory: memory.to_owned(),
            content_hash: "doc-hash".into(),
            saved_at_unix_ms: None,
            author: String::new(),
        }
    }

    fn selection(document: &ContextDocument, sections: Vec<usize>) -> MemorySelection {
        MemorySelection {
            revision: document.revision,
            content_hash: document.content_hash.clone(),
            sections,
        }
    }

    #[test]
    fn selected_sections_preserve_exact_unicode_source_and_allow_small_slice_of_large_document()
    -> Result<()> {
        let source = format!(
            "# Facts\n{}\n## Decisions\nλ決定\n# Agreements\nlast\n",
            "x".repeat(40 * 1024)
        );
        let document = doc(&source);
        let chosen = selection(&document, vec![1]);
        let review = MemoryReview::from_document(&document, Some(&chosen), true)?;
        assert_eq!(review.selected_bytes, "## Decisions\nλ決定\n".len());
        assert_eq!(review.selected_text(&document)?, "## Decisions\nλ決定\n");
        assert_eq!(review.sections.len(), 3);
        assert!(MemoryReview::from_document(&document, None, true).is_err());
        Ok(())
    }

    #[test]
    fn preamble_and_fenced_heading_are_lossless_boundaries() -> Result<()> {
        let source = "intro\r\n```md\r\n# hidden\r\n```\r\n# Facts\r\nbody\r\n## Decisions\r\nend";
        let document = doc(source);
        let review = MemoryReview::from_document(&document, None, true)?;
        assert_eq!(review.sections.len(), 3);
        assert_eq!(review.sections[0].title, "Вступление");
        assert_eq!(review.sections[1].title, "Facts");
        assert_eq!(review.sections[2].title, "Decisions");
        assert_eq!(review.selected_text(&document)?, source);
        let chosen =
            MemoryReview::from_document(&document, Some(&selection(&document, vec![2, 0])), false);
        assert!(chosen.is_err(), "indexes must be ascending");
        Ok(())
    }

    #[test]
    fn stale_and_invalid_selections_are_rejected_even_when_not_included() {
        let document = doc("# Facts\na\n# Decisions\nb\n");
        let mut stale = selection(&document, vec![0]);
        stale.revision += 1;
        assert!(MemoryReview::from_document(&document, Some(&stale), false).is_err());
        assert!(
            MemoryReview::from_document(&document, Some(&selection(&document, vec![2])), false)
                .is_err()
        );
        assert!(
            MemoryReview::from_document(&document, Some(&selection(&document, vec![0, 0])), false)
                .is_err()
        );
    }

    #[test]
    fn non_heading_and_empty_memory_have_expected_sections() -> Result<()> {
        let plain = doc("plain\ntext");
        let review = MemoryReview::from_document(&plain, None, true)?;
        assert_eq!(review.sections.len(), 1);
        assert_eq!(review.selected_text(&plain)?, plain.memory);
        let empty = doc("");
        assert!(
            MemoryReview::from_document(&empty, None, true)?
                .sections
                .is_empty()
        );
        Ok(())
    }

    #[test]
    fn too_many_sections_allow_explicit_empty_no_memory_review_with_reason() -> Result<()> {
        let source = (0..MAX_REVIEW_SECTIONS + 1)
            .map(|index| format!("# H{index}\n"))
            .collect::<String>();
        let document = doc(&source);
        let empty = selection(&document, vec![]);
        let review = MemoryReview::from_document(&document, Some(&empty), false)?;
        assert!(review.sections.is_empty());
        assert!(review.selected.is_empty());
        assert!(review.unavailable_reason.is_some());
        assert!(MemoryReview::from_document(&document, Some(&empty), true).is_err());
        let selected = selection(&document, vec![0]);
        assert!(MemoryReview::from_document(&document, Some(&selected), false).is_err());
        let at_limit = doc(&format!(
            "intro\n{}",
            "# C#\n".repeat(MAX_REVIEW_SECTIONS - 1)
        ));
        let review = MemoryReview::from_document(&at_limit, None, false)?;
        assert_eq!(review.sections.len(), MAX_REVIEW_SECTIONS);
        assert_eq!(review.sections[1].title, "C#");
        let over_limit = doc(&format!("intro\n{}", "# C#\n".repeat(MAX_REVIEW_SECTIONS)));
        assert!(
            MemoryReview::from_document(&over_limit, None, false)?
                .unavailable_reason
                .is_some()
        );
        Ok(())
    }
}

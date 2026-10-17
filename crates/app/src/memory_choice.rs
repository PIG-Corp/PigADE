//! UI draft selection uses host-provided section identities, never reparses Markdown.
use pigade_storage::{MemoryReview, MemorySelection};

pub const LIMIT: usize = 32 * 1024;

pub fn format_sections(indices: &[usize]) -> String {
    indices
        .iter()
        .map(|index| (index + 1).to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

pub fn parse_sections(text: &str, review: &MemoryReview) -> Result<Vec<usize>, String> {
    if text.len() > 8192 {
        return Err("Слишком длинный список разделов памяти".into());
    }
    let mut indices = Vec::new();
    for token in text.split(|ch: char| ch == ',' || ch.is_whitespace()) {
        if token.is_empty() {
            continue;
        }
        let index = token
            .parse::<usize>()
            .ok()
            .and_then(|number| number.checked_sub(1))
            .filter(|index| {
                review
                    .sections
                    .get(*index)
                    .is_some_and(|s| s.index == *index)
            })
            .ok_or_else(|| {
                format!(
                    "Нет раздела памяти «{}»",
                    token.chars().take(24).collect::<String>()
                )
            })?;
        if indices.contains(&index) {
            return Err(format!("Раздел {} указан дважды", index + 1));
        }
        indices.push(index);
    }
    indices.sort_unstable();
    Ok(indices)
}

pub fn selected_bytes(indices: &[usize], review: &MemoryReview) -> usize {
    indices
        .iter()
        .filter_map(|index| review.sections.get(*index))
        .fold(0usize, |sum, section| sum.saturating_add(section.bytes))
}

pub fn selection(text: &str, review: &MemoryReview) -> Result<MemorySelection, String> {
    let sections = parse_sections(text, review)?;
    let bytes = selected_bytes(&sections, review);
    if bytes > LIMIT {
        return Err(format!(
            "Выбрано {bytes} байт памяти при лимите {LIMIT}. Исключите разделы; текст не обрезается."
        ));
    }
    Ok(MemorySelection {
        revision: review.revision,
        content_hash: review.content_hash.clone(),
        sections,
    })
}

pub fn matches(text: &str, review: &MemoryReview, include_memory: bool) -> bool {
    selection(text, review).is_ok_and(|selection| {
        selection.sections == review.selected && include_memory == !selection.sections.is_empty()
    })
}

pub fn matches_preview(text: &str, review: Option<&MemoryReview>, include_memory: bool) -> bool {
    match review {
        Some(review) => matches(text, review, include_memory),
        None => text.trim().is_empty() && !include_memory,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pigade_storage::MemorySection;

    #[test]
    fn draft_selection_is_explicit_revision_bound_and_never_truncated() {
        let review = MemoryReview {
            revision: 7,
            content_hash: "f".repeat(64),
            sections: [12, LIMIT, 6]
                .into_iter()
                .enumerate()
                .map(|(index, bytes)| MemorySection {
                    index,
                    title: format!("Раздел {index}"),
                    bytes,
                })
                .collect(),
            selected: vec![0, 2],
            selected_bytes: 18,
            unavailable_reason: None,
        };
        let selected = selection(" 3, 1 ", &review).unwrap();
        assert_eq!(selected.sections, vec![0, 2]);
        assert_eq!(selected.revision, 7);
        assert_eq!(selected.content_hash, review.content_hash);
        assert_eq!(format_sections(&selected.sections), "1, 3");
        assert!(matches("1 3", &review, true));
        assert!(!matches("1 3", &review, false));
        assert!(!matches("", &review, true));
        assert!(matches_preview("", None, false));
        assert!(!matches_preview("1", None, false));
        assert!(!matches_preview("", None, true));
        for invalid in [
            "0",
            "4",
            "-1",
            "1,1",
            "1-3",
            "9999999999999999999999999999999999999",
        ] {
            assert!(selection(invalid, &review).is_err(), "{invalid}");
        }
        assert!(
            selection("1,2", &review)
                .unwrap_err()
                .contains("не обрезается")
        );
        assert!(selection("2", &review).is_ok());
        assert!(selection("", &review).unwrap().sections.is_empty());
        assert_eq!(review.selected, vec![0, 2]);
    }
}

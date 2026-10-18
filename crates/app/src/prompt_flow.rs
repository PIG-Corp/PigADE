//! Reusable prompt drafts never own a process or send terminal input.
use pigade_storage::{CatalogWrite, Prompt};
use uuid::Uuid;

use crate::product::{Dialog, DialogKind};

pub(crate) const PROMPT_PAGE_SIZE: usize = 5;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct PromptRef {
    pub id: Uuid,
    pub revision: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PromptRow {
    pub prompt: PromptRef,
    pub title: String,
    pub snippet: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SyncKey {
    dialog: Uuid,
    query_revision: u64,
    prompt_count: usize,
    prompts: Vec<PromptRef>,
}

#[derive(Default)]
pub(crate) struct Library {
    pub rows: Vec<PromptRow>,
    pub selected: Option<PromptRef>,
    pub offset: usize,
    /// Invalid host catalog data is rejected as a whole and can be reported by
    /// the owning UI instead of being presented as an empty search result.
    pub error: Option<String>,
    sync_key: Option<SyncKey>,
}

impl Library {
    /// Rebuild search rows only when the query revision or prompt roster changes.
    /// Prompt content is scanned only on those transitions, never per paint.
    pub fn sync(&mut self, dialog: &Dialog, prompts: &[Prompt]) -> bool {
        let query_field = dialog.fields.first();
        let query_revision = query_field.map_or(0, |field| field.revision());
        let prompt_refs = if prompts.len() <= pigade_storage::MAX_PROMPTS {
            prompts
                .iter()
                .map(|prompt| PromptRef {
                    id: prompt.id,
                    revision: prompt.revision,
                })
                .collect()
        } else {
            Vec::new()
        };
        let key = SyncKey {
            dialog: dialog.id,
            query_revision,
            prompt_count: prompts.len(),
            prompts: prompt_refs,
        };
        if self.sync_key.as_ref() == Some(&key) {
            return false;
        }
        self.sync_key = Some(key);

        let previous_id = self.selected.map(|selected| selected.id);
        self.rows.clear();
        self.offset = 0;
        self.error = invalid_prompt_source(prompts).map(str::to_owned);
        if self.error.is_none() {
            let query = query_field
                .map_or("", |field| field.text())
                .trim()
                .to_lowercase();
            self.rows = prompts
                .iter()
                .filter(|prompt| {
                    query.is_empty()
                        || prompt.title.to_lowercase().contains(&query)
                        || prompt.content.to_lowercase().contains(&query)
                })
                .map(|prompt| PromptRow {
                    prompt: PromptRef {
                        id: prompt.id,
                        revision: prompt.revision,
                    },
                    title: prompt.title.clone(),
                    snippet: prompt_snippet(&prompt.content),
                })
                .collect();
        }

        self.selected = if self.error.is_some() {
            None
        } else {
            previous_id
                .and_then(|id| self.rows.iter().find(|row| row.prompt.id == id))
                .map(|row| row.prompt)
                .or_else(|| self.rows.first().map(|row| row.prompt))
        };
        if let Some(selected) = self.selected
            && let Some(index) = self
                .rows
                .iter()
                .position(|row| row.prompt.id == selected.id)
        {
            self.reveal(index);
        }
        true
    }

    pub fn visible_rows(&self) -> &[PromptRow] {
        let end = self
            .offset
            .saturating_add(PROMPT_PAGE_SIZE)
            .min(self.rows.len());
        &self.rows[self.offset.min(end)..end]
    }

    /// Select only a row from the current filtered revision.
    pub fn select(&mut self, prompt: PromptRef) -> bool {
        let Some(index) = self.rows.iter().position(|row| row.prompt == prompt) else {
            return false;
        };
        self.selected = Some(prompt);
        self.reveal(index);
        true
    }

    pub fn navigate(&mut self, delta: isize) {
        if self.rows.is_empty() {
            self.selected = None;
            self.offset = 0;
            return;
        }
        let current = self
            .selected
            .and_then(|selected| {
                self.rows
                    .iter()
                    .position(|row| row.prompt.id == selected.id)
            })
            .unwrap_or(0);
        let last = self.rows.len() - 1;
        let target = (current as isize)
            .saturating_add(delta)
            .clamp(0, last as isize) as usize;
        self.selected = Some(self.rows[target].prompt);
        self.reveal(target);
    }

    pub fn page(&mut self, forward: bool) {
        let delta = if forward {
            PROMPT_PAGE_SIZE as isize
        } else {
            -(PROMPT_PAGE_SIZE as isize)
        };
        self.navigate(delta);
    }

    pub fn selected(&self) -> Option<PromptRef> {
        self.selected
    }

    fn reveal(&mut self, index: usize) {
        let page = index / PROMPT_PAGE_SIZE;
        self.offset = page * PROMPT_PAGE_SIZE;
    }
}

fn invalid_prompt_source(prompts: &[Prompt]) -> Option<&'static str> {
    if prompts.len() > pigade_storage::MAX_PROMPTS {
        return Some("Библиотека содержит слишком много промптов");
    }
    let mut ids = std::collections::HashSet::with_capacity(prompts.len());
    for prompt in prompts {
        if !ids.insert(prompt.id) {
            return Some("Библиотека содержит повторяющийся ID промпта");
        }
        if prompt.id.is_nil()
            || prompt.revision == 0
            || prompt.revision >= i64::MAX as u64
            || pigade_storage::validate_prompt(&prompt.title, &prompt.content).is_err()
        {
            return Some("Библиотека содержит некорректные данные промпта");
        }
    }
    None
}

fn prompt_snippet(content: &str) -> String {
    const LIMIT: usize = 120;
    let mut snippet = String::new();
    let mut visible_chars = 0;
    let mut pending_space = false;
    for ch in content.chars() {
        if ch.is_whitespace() {
            pending_space |= visible_chars > 0;
            continue;
        }
        if pending_space {
            snippet.push(' ');
            visible_chars += 1;
            pending_space = false;
            if visible_chars > LIMIT {
                break;
            }
        }
        snippet.push(ch);
        visible_chars += 1;
        if visible_chars > LIMIT {
            break;
        }
    }
    if visible_chars > LIMIT {
        let bounded = snippet.chars().take(LIMIT).collect::<String>();
        format!("{bounded}…")
    } else {
        snippet
    }
}

#[derive(Clone, Debug)]
pub(crate) struct PromptEditor {
    pub id: Uuid,
    pub saved: Option<Prompt>,
    pub origin: Option<Dialog>,
}

impl PromptEditor {
    pub fn new() -> Self {
        Self {
            id: Uuid::new_v4(),
            saved: None,
            origin: None,
        }
    }

    pub fn saved(prompt: Prompt) -> Self {
        Self {
            id: prompt.id,
            saved: Some(prompt),
            origin: None,
        }
    }

    pub fn write(&self, fields: &[String]) -> Result<CatalogWrite<Prompt>, String> {
        let title = fields.first().map_or("", String::as_str).trim();
        let content = fields.get(1).map_or("", String::as_str);
        pigade_storage::validate_prompt(title, content).map_err(|error| error.to_string())?;
        Ok(CatalogWrite {
            value: Prompt {
                id: self.id,
                title: title.into(),
                content: content.into(),
                revision: self.saved.as_ref().map_or(0, |saved| saved.revision),
            },
            expected_revision: self.saved.as_ref().map(|saved| saved.revision),
        })
    }
}

/// Only product instruction drafts accept a library insertion. A terminal,
/// history view, path, conversation ID or single-line role is never a target.
pub(crate) fn accepts_prompt(dialog: &Dialog) -> bool {
    matches!(
        dialog.kind,
        DialogKind::Context
            | DialogKind::ProjectContext
            | DialogKind::LaunchReview
            | DialogKind::AgentProfile
            | DialogKind::RecipeSlot
    ) && crate::product::dialog_field_multiline(dialog.kind, dialog.active_field)
        && dialog.fields.get(dialog.active_field).is_some()
}

pub(crate) fn insert_prompt(origin: &Dialog, content: &str) -> Result<Dialog, String> {
    if !accepts_prompt(origin) {
        return Err("Выберите поле инструкций для вставки".into());
    }
    let mut draft = origin.clone();
    draft.fields[draft.active_field]
        .replace(None, content, true)
        .map_err(str::to_owned)?;
    Ok(draft)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prompt(id: u128, title: &str, content: &str, revision: u64) -> Prompt {
        Prompt {
            id: Uuid::from_u128(id),
            title: title.to_owned(),
            content: content.to_owned(),
            revision,
        }
    }

    fn library_dialog(query: &str) -> Dialog {
        let mut dialog = Dialog::new(DialogKind::Prompts);
        if dialog.fields.is_empty() {
            dialog.fields.push(crate::text_edit::EditBuffer::new(query));
        } else {
            dialog.fields[0] = crate::text_edit::EditBuffer::new(query);
        }
        dialog
    }

    fn replace_query(dialog: &mut Dialog, query: &str) {
        let field = &mut dialog.fields[0];
        let end = field.text().encode_utf16().count();
        field.select_utf16(0..end);
        field.replace(None, query, false).unwrap();
    }

    #[test]
    fn library_search_is_trimmed_literal_case_insensitive_and_unicode_aware() {
        let mut dialog = library_dialog("  РЕВЬЮ ");
        let entries = [
            prompt(1, "Ревью базы", "Check implementation", 1),
            prompt(2, "Проверка", "Проверь СТРОКИ и формат", 3),
            prompt(3, "Regex", "literal .* text", 1),
        ];
        let mut library = Library::default();
        assert!(library.sync(&dialog, &entries));
        assert_eq!(library.rows.len(), 1);
        assert_eq!(library.rows[0].prompt.id, Uuid::from_u128(1));
        assert_eq!(library.rows[0].snippet, "Check implementation");

        replace_query(&mut dialog, "проверь строки");
        assert!(library.sync(&dialog, &entries));
        assert_eq!(library.rows.len(), 1);
        assert_eq!(library.rows[0].prompt.id, Uuid::from_u128(2));

        replace_query(&mut dialog, ".*");
        assert!(library.sync(&dialog, &entries));
        assert_eq!(library.rows.len(), 1);
        assert_eq!(library.rows[0].prompt.id, Uuid::from_u128(3));
        replace_query(&mut dialog, "no such phrase");
        assert!(library.sync(&dialog, &entries));
        assert!(library.rows.is_empty());
        assert_eq!(library.selected(), None);
        assert_eq!(library.visible_rows(), &[]);
    }

    #[test]
    fn library_preserves_selection_by_id_across_reorder_and_revision_changes() {
        let dialog = library_dialog("");
        let first = prompt(11, "First", "first content", 1);
        let selected = prompt(12, "Selected", "selected content", 4);
        let third = prompt(13, "Third", "third content", 1);
        let mut library = Library::default();
        assert!(library.sync(&dialog, &[first.clone(), selected.clone(), third.clone()]));
        let old_ref = PromptRef {
            id: selected.id,
            revision: selected.revision,
        };
        assert!(library.select(old_ref));

        assert!(library.sync(&dialog, &[third.clone(), selected.clone(), first.clone()]));
        assert_eq!(library.selected(), Some(old_ref));
        let changed = prompt(12, "Selected updated", "new revision", 5);
        assert!(library.sync(&dialog, &[third.clone(), changed.clone(), first.clone()]));
        let new_ref = PromptRef {
            id: changed.id,
            revision: changed.revision,
        };
        assert_eq!(library.selected(), Some(new_ref));
        assert!(!library.select(old_ref));

        assert!(library.sync(&dialog, &[third.clone(), first]));
        assert_eq!(
            library.selected().map(|reference| reference.id),
            Some(third.id)
        );
    }

    #[test]
    fn library_cache_tracks_query_and_prompt_revisions_without_rescanning_paints() {
        let mut dialog = library_dialog("");
        let original = prompt(21, "Reusable", "some content", 1);
        let mut library = Library::default();
        assert!(library.sync(&dialog, std::slice::from_ref(&original)));
        assert!(!library.sync(&dialog, std::slice::from_ref(&original)));

        replace_query(&mut dialog, "reusable");
        assert!(library.sync(&dialog, std::slice::from_ref(&original)));
        assert!(!library.sync(&dialog, std::slice::from_ref(&original)));

        let changed = prompt(21, "Reusable", "some content", 2);
        assert!(library.sync(&dialog, std::slice::from_ref(&changed)));
        assert_eq!(
            library.selected().map(|reference| reference.revision),
            Some(2)
        );
    }

    #[test]
    fn library_pages_five_rows_and_keeps_selection_visible() {
        let dialog = library_dialog("");
        let entries = (1..=12)
            .map(|index| prompt(index, &format!("Prompt {index}"), "content", 1))
            .collect::<Vec<_>>();
        let mut library = Library::default();
        assert!(library.sync(&dialog, &entries));
        assert_eq!(library.visible_rows().len(), PROMPT_PAGE_SIZE);
        assert_eq!(library.selected().unwrap().id, Uuid::from_u128(1));

        library.page(true);
        assert_eq!(library.selected().unwrap().id, Uuid::from_u128(6));
        assert_eq!(library.offset, 5);
        assert_eq!(
            library.visible_rows().first().unwrap().prompt.id,
            Uuid::from_u128(6)
        );
        assert_eq!(
            library.visible_rows().last().unwrap().prompt.id,
            Uuid::from_u128(10)
        );

        library.page(true);
        assert_eq!(library.selected().unwrap().id, Uuid::from_u128(11));
        assert_eq!(library.offset, 10);
        assert_eq!(library.visible_rows().len(), 2);
        library.navigate(100);
        assert_eq!(library.selected().unwrap().id, Uuid::from_u128(12));
        library.page(false);
        assert_eq!(library.selected().unwrap().id, Uuid::from_u128(7));
        assert_eq!(library.offset, 5);
        library.navigate(-100);
        assert_eq!(library.selected().unwrap().id, Uuid::from_u128(1));
        assert_eq!(library.offset, 0);
    }

    #[test]
    fn library_rejects_duplicate_or_oversized_host_catalogs() {
        let dialog = library_dialog("");
        let duplicate = prompt(31, "Duplicate", "content", 1);
        let mut library = Library::default();
        assert!(library.sync(&dialog, &[duplicate.clone(), duplicate]));
        assert!(library.rows.is_empty());
        assert!(library.selected().is_none());
        assert!(
            library
                .error
                .as_deref()
                .unwrap()
                .contains("повторяющийся ID")
        );

        let too_many = (1..=pigade_storage::MAX_PROMPTS + 1)
            .map(|id| prompt(id as u128, "Title", "content", 1))
            .collect::<Vec<_>>();
        assert!(library.sync(&dialog, &too_many));
        assert!(library.rows.is_empty());
        assert!(library.selected().is_none());
        assert!(library.error.as_deref().unwrap().contains("слишком много"));

        let zero_revision = prompt(132, "Unsaved", "content", 0);
        assert!(library.sync(&dialog, &[zero_revision]));
        assert!(library.rows.is_empty());
        assert!(library.selected().is_none());
        assert!(
            library
                .error
                .as_deref()
                .unwrap()
                .contains("некорректные данные")
        );
    }

    #[test]
    fn prompt_snippet_normalizes_whitespace_and_truncates_unicode() {
        assert_eq!(
            prompt_snippet("  первое\n\tвторое   третье  "),
            "первое второе третье"
        );
        let snippet = prompt_snippet(&format!("{}🧪 хвост", "я".repeat(120)));
        assert_eq!(snippet, format!("{}…", "я".repeat(120)));
        assert_eq!(snippet.chars().count(), 121);
    }

    #[test]
    fn saved_prompt_inserts_into_frozen_selection_as_one_undo_without_changing_source() {
        let mut origin = Dialog::new(DialogKind::Context);
        origin.fields[0] = "before old after".into();
        origin.fields[0].select_bytes(7..10, false);
        origin.fields[1] = "unchanged role".into();
        let mut draft = insert_prompt(&origin, "проверить\nтесты").unwrap();
        assert_eq!(draft.id, origin.id);
        assert_eq!(draft.fields[0].text(), "before проверить\nтесты after");
        assert_eq!(draft.fields[1], origin.fields[1]);
        assert_eq!(origin.fields[0].text(), "before old after");
        draft.fields[0].undo();
        assert_eq!(draft.fields[0].text(), origin.fields[0].text());
        let mut path = Dialog::new(DialogKind::CreateProject);
        path.active_field = 1;
        assert!(insert_prompt(&path, "not a path").is_err());
        origin.active_field = 1;
        assert!(insert_prompt(&origin, "not a single-line role").is_err());
    }

    #[test]
    fn create_retries_keep_id_and_edits_keep_opened_revision() {
        let editor = PromptEditor::new();
        let fields = vec!["Review".into(), "Review the code".into()];
        let first = editor.write(&fields).unwrap();
        assert_eq!(editor.write(&fields).unwrap().value, first.value);
        let mut saved = first.value;
        saved.revision = 4;
        let editor = PromptEditor::saved(saved.clone());
        let write = editor
            .write(&["New title".into(), "New text".into()])
            .unwrap();
        assert_eq!(write.expected_revision, Some(4));
        assert_eq!(write.value.id, saved.id);
        assert_eq!(editor.saved, Some(saved));
    }
}

//! Reusable prompt drafts never own a process or send terminal input.
use pigade_storage::{CatalogWrite, Prompt};
use uuid::Uuid;

use crate::product::{Dialog, DialogKind};

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

//! Reusable launch drafts; no process or workspace identity is stored here.
use pigade_storage::{AgentProfile, CatalogWrite, LaunchSettings};
use uuid::Uuid;

#[derive(Clone, Debug)]
pub(crate) struct ProfileEditor {
    pub id: Uuid,
    pub saved: Option<AgentProfile>,
    pub settings: LaunchSettings,
    pub origin: Option<crate::product::Dialog>,
}

impl ProfileEditor {
    pub fn new(settings: LaunchSettings) -> Self {
        Self {
            id: Uuid::new_v4(),
            saved: None,
            settings,
            origin: None,
        }
    }
    pub fn saved(profile: AgentProfile) -> Self {
        Self {
            id: profile.id,
            settings: profile.settings.clone(),
            saved: Some(profile),
            origin: None,
        }
    }
    pub fn write(&self, fields: &[String]) -> Result<CatalogWrite<AgentProfile>, String> {
        let name = fields.first().map_or("", String::as_str).trim();
        if name.is_empty() || name.len() > 256 || name.chars().any(char::is_control) {
            return Err("Введите название без переносов строк, до256 байт UTF-8".into());
        }
        let mut settings = self.settings.clone();
        settings.role = fields.get(1).cloned().unwrap_or_default();
        settings.instructions = fields.get(2).cloned().unwrap_or_default();
        pigade_storage::validate_agent_profile_settings(&settings)
            .map_err(|error| error.to_string())?;
        Ok(CatalogWrite {
            value: AgentProfile {
                id: self.id,
                name: name.into(),
                settings,
                revision: self.saved.as_ref().map_or(0, |saved| saved.revision),
            },
            expected_revision: self.saved.as_ref().map(|saved| saved.revision),
        })
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Pending {
    pub request: Uuid,
    pub dialog: Uuid,
    pub tab: Uuid,
    pub profile: Uuid,
    pub revision: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_keeps_create_identity_and_saved_edit_uses_opened_revision() {
        let mut editor = ProfileEditor::new(crate::template_flow::default_settings("codex"));
        let fields = vec![
            "Reviewer".into(),
            "Review changes".into(),
            "Check regressions".into(),
        ];
        let first = editor.write(&fields).unwrap();
        assert_eq!(first.value, editor.write(&fields).unwrap().value);
        assert_eq!(first.expected_revision, None);
        let mut saved = first.value;
        saved.revision = 7;
        editor = ProfileEditor::saved(saved.clone());
        let changed = editor
            .write(&[
                "New name".into(),
                "New role".into(),
                "New instructions".into(),
            ])
            .unwrap();
        assert_eq!(changed.value.id, saved.id);
        assert_eq!(changed.expected_revision, Some(7));
        assert_eq!(editor.saved.unwrap(), saved);
        assert_eq!(changed.value.settings.instructions, "New instructions");
    }
}

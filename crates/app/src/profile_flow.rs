//! Reusable launch drafts; no process or workspace identity is stored here.
use pigade_storage::{AgentProfile, CatalogWrite, LaunchSettings};
use uuid::Uuid;

pub(crate) fn restore_roster_selection(
    dialog: &mut crate::product::Dialog,
    expected_dialog: Uuid,
    selected_profile: Uuid,
    profile_ids: &[Uuid],
    roster: Option<&pigade_storage::AgentProfileRoster>,
) -> bool {
    if dialog.id != expected_dialog || dialog.kind != crate::product::DialogKind::AgentProfiles {
        return false;
    }
    let order = crate::profile_roster::display_order(profile_ids, roster);
    if let Some(index) = order.visible_index(selected_profile) {
        dialog.active_field = index + 1;
        true
    } else {
        false
    }
}

#[derive(Clone, Debug)]
pub(crate) struct RosterPending {
    pub request: Uuid,
    pub dialog: Uuid,
    pub expected_revision: u64,
    pub selected_profile: Uuid,
    pub ordered_ids: Vec<Uuid>,
    pub pinned_ids: Vec<Uuid>,
}

#[derive(Clone, Debug)]
pub(crate) struct RosterReload {
    pub request: Uuid,
    pub dialog: Uuid,
    pub selected_profile: Option<Uuid>,
}

#[derive(Clone, Debug)]
pub(crate) struct ProfileEditor {
    pub id: Uuid,
    pub saved: Option<AgentProfile>,
    pub settings: LaunchSettings,
    pub origin: Option<crate::product::Dialog>,
    builtin_draft: Option<Vec<crate::text_edit::EditBuffer>>,
    custom_draft: Option<Vec<crate::text_edit::EditBuffer>>,
}

impl ProfileEditor {
    pub fn new(settings: LaunchSettings) -> Self {
        Self {
            id: Uuid::new_v4(),
            saved: None,
            settings,
            origin: None,
            builtin_draft: None,
            custom_draft: None,
        }
    }
    pub fn saved(profile: AgentProfile) -> Self {
        Self {
            id: profile.id,
            settings: profile.settings.clone(),
            saved: Some(profile),
            origin: None,
            builtin_draft: None,
            custom_draft: None,
        }
    }

    pub(crate) fn field_labels(&self) -> &'static [&'static str] {
        if self.settings.provider == "custom" {
            &[
                "Название",
                "Исполняемый файл (абсолютный путь)",
                "Аргументы",
            ]
        } else {
            &["Название", "Роль", "Инструкции", "Модель", "Усилие"]
        }
    }

    pub(crate) fn initial_fields(&self, name: String) -> Vec<crate::text_edit::EditBuffer> {
        if self.settings.provider == "custom" {
            let command = self.settings.custom_command.as_ref();
            vec![
                name,
                command.map_or_else(String::new, |value| value.executable.clone()),
                command.map_or_else(String::new, |value| {
                    crate::custom_cli::format_arguments(&value.args)
                }),
            ]
            .into_iter()
            .map(crate::text_edit::EditBuffer::new)
            .collect()
        } else {
            vec![
                name,
                self.settings.role.clone(),
                self.settings.instructions.clone(),
                self.settings.engine.model.clone().unwrap_or_default(),
                self.settings
                    .engine
                    .reasoning_effort
                    .clone()
                    .unwrap_or_default(),
            ]
            .into_iter()
            .map(crate::text_edit::EditBuffer::new)
            .collect()
        }
    }

    /// Save the exact editor buffers before changing provider and restore the
    /// other provider's draft, including invalid argument text.
    pub(crate) fn switch_provider(
        &mut self,
        provider: &str,
        dialog: &mut crate::product::Dialog,
    ) -> bool {
        let custom = provider == "custom";
        if !custom && !matches!(provider, "codex" | "claude" | "opencode") {
            return false;
        }
        if self.settings.provider == provider {
            return true;
        }
        let current = std::mem::take(&mut dialog.fields);
        let current_name = current
            .first()
            .map(|field| field.text().to_owned())
            .unwrap_or_default();
        if self.settings.provider == "custom" {
            self.custom_draft = Some(current);
        } else {
            self.builtin_draft = Some(current);
        }
        self.settings.provider = provider.to_owned();
        let restored = if custom {
            self.custom_draft.take()
        } else {
            self.builtin_draft.take()
        };
        dialog.fields = restored.unwrap_or_else(|| self.initial_fields(current_name));
        // Reused buffers can have the same revision but different field meanings.
        // A new form scope invalidates shaped text and in-flight editor callbacks.
        dialog.id = Uuid::new_v4();
        dialog.active_field = dialog
            .active_field
            .min(dialog.fields.len().saturating_sub(1));
        true
    }

    pub fn write(&self, fields: &[String]) -> Result<CatalogWrite<AgentProfile>, String> {
        let name = fields.first().map_or("", String::as_str).trim();
        if name.is_empty() || name.len() > 256 || name.chars().any(char::is_control) {
            return Err("Введите название без переносов строк, до256 байт UTF-8".into());
        }
        let mut settings = self.settings.clone();
        if settings.provider == "custom" {
            let command = pigade_storage::CustomCommand {
                executable: fields.get(1).cloned().unwrap_or_default(),
                args: crate::custom_cli::parse_arguments(fields.get(2).map_or("", String::as_str))?,
            };
            command.validate().map_err(|error| error.to_string())?;
            settings.role.clear();
            settings.instructions.clear();
            settings.engine = pigade_storage::EngineOptions::default();
            settings.deliver_context = false;
            settings.custom_command = Some(command);
        } else {
            settings.custom_command = None;
            settings.role = fields.get(1).cloned().unwrap_or_default();
            settings.instructions = fields.get(2).cloned().unwrap_or_default();
            settings.engine = pigade_storage::EngineOptions::from_fields(
                fields.get(3).map_or("", String::as_str),
                fields.get(4).map_or("", String::as_str),
            )
            .map_err(|error| error.to_string())?;
        }
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
    fn roster_reply_preserves_identity_selection_and_does_not_replace_new_dialog() {
        let a = Uuid::from_u128(1);
        let b = Uuid::from_u128(2);
        let c = Uuid::from_u128(3);
        let roster = pigade_storage::AgentProfileRoster {
            schema: 1,
            revision: 2,
            ordered_ids: vec![c, a, b],
            pinned_ids: vec![a],
        };
        let mut original = crate::product::Dialog::new(crate::product::DialogKind::AgentProfiles);
        let id = original.id;
        assert!(restore_roster_selection(
            &mut original,
            id,
            b,
            &[a, b, c],
            Some(&roster)
        ));
        assert_eq!(original.active_field, 3); // visible order is [A, C, B]

        let mut newer = crate::product::Dialog::new(crate::product::DialogKind::Prompts);
        let newer_id = newer.id;
        assert!(!restore_roster_selection(
            &mut newer,
            id,
            b,
            &[a, b, c],
            Some(&roster)
        ));
        assert_eq!(newer.id, newer_id);
        assert_eq!(newer.kind, crate::product::DialogKind::Prompts);
    }

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
        saved.settings.engine.model = Some("gpt-review".into());
        saved.settings.engine.reasoning_effort = Some("high".into());
        saved.revision = 7;
        editor = ProfileEditor::saved(saved.clone());
        let changed = editor
            .write(&[
                "New name".into(),
                "New role".into(),
                "New instructions".into(),
                "gpt-review".into(),
                "high".into(),
            ])
            .unwrap();
        assert_eq!(changed.value.id, saved.id);
        assert_eq!(changed.expected_revision, Some(7));
        assert_eq!(editor.saved.unwrap(), saved);
        assert_eq!(changed.value.settings.instructions, "New instructions");
        assert_eq!(
            changed.value.settings.engine.model.as_deref(),
            Some("gpt-review")
        );
        assert_eq!(
            changed.value.settings.engine.reasoning_effort.as_deref(),
            Some("high")
        );
    }

    #[test]
    fn model_and_effort_are_preserved_on_profile_writes() {
        let editor = ProfileEditor::new(crate::template_flow::default_settings("codex"));
        let fields = vec![
            "Reviewer".into(),
            "Review changes".into(),
            "Check regressions".into(),
            "  gpt-test  ".into(),
            " high ".into(),
        ];

        let write = editor.write(&fields).unwrap();
        assert_eq!(
            write.value.settings.engine.model.as_deref(),
            Some("gpt-test")
        );
        assert_eq!(
            write.value.settings.engine.reasoning_effort.as_deref(),
            Some("high")
        );
    }

    #[test]
    fn blank_model_and_effort_restore_cli_defaults() {
        let mut editor = ProfileEditor::new(crate::template_flow::default_settings("codex"));
        editor.settings.engine.model = Some("old-model".into());
        editor.settings.engine.reasoning_effort = Some("old-effort".into());
        let fields = vec![
            "Reviewer".into(),
            String::new(),
            String::new(),
            "  ".into(),
            String::new(),
        ];

        let write = editor.write(&fields).unwrap();
        assert_eq!(write.value.settings.engine.model, None);
        assert_eq!(write.value.settings.engine.reasoning_effort, None);
    }

    #[test]
    fn invalid_engine_options_are_rejected_by_storage_validation() {
        let editor = ProfileEditor::new(crate::template_flow::default_settings("codex"));
        let fields = vec![
            "Reviewer".into(),
            String::new(),
            String::new(),
            "bad\0model".into(),
            String::new(),
        ];

        assert!(editor.write(&fields).is_err());
    }

    #[test]
    fn custom_profile_writes_exact_literal_arguments_and_clears_builtin_fields() {
        let mut settings = crate::template_flow::default_settings("custom");
        settings.role = "discard me".into();
        settings.instructions = "discard me too".into();
        settings.deliver_context = true;
        settings.engine.model = Some("unsupported".into());
        let editor = ProfileEditor::new(settings);
        let write = editor
            .write(&[
                "My CLI".into(),
                "/usr/bin/tool".into(),
                "--label 'two words' $(literal)".into(),
            ])
            .unwrap();
        assert_eq!(write.value.settings.provider, "custom");
        assert_eq!(
            write.value.settings.custom_command,
            Some(pigade_storage::CustomCommand {
                executable: "/usr/bin/tool".into(),
                args: vec!["--label".into(), "two words".into(), "$(literal)".into()],
            })
        );
        assert!(write.value.settings.role.is_empty());
        assert!(write.value.settings.instructions.is_empty());
        assert!(!write.value.settings.deliver_context);
        assert!(write.value.settings.engine.is_empty());
    }

    #[test]
    fn custom_profile_rejects_relative_executable_and_unmatched_argument_quote() {
        let editor = ProfileEditor::new(crate::template_flow::default_settings("custom"));
        assert!(
            editor
                .write(&["Tool".into(), "bin/tool".into(), "ok".into()])
                .is_err()
        );
        assert!(
            editor
                .write(&["Tool".into(), "/usr/bin/tool".into(), "'unfinished".into()])
                .is_err()
        );
    }

    #[test]
    fn reopening_and_saving_custom_profile_preserves_carriage_returns_and_backslashes() {
        let mut settings = crate::template_flow::default_settings("custom");
        let command = pigade_storage::CustomCommand {
            executable: "/usr/bin/tool".into(),
            args: vec![
                "before\rafter".into(),
                "line\r\nend".into(),
                "slash\\".into(),
            ],
        };
        settings.custom_command = Some(command.clone());
        let editor = ProfileEditor::saved(pigade_storage::AgentProfile {
            id: Uuid::new_v4(),
            name: "Stored CLI".into(),
            settings,
            revision: 4,
        });
        let fields = editor.initial_fields("Stored CLI".into());
        let write = editor
            .write(
                &fields
                    .iter()
                    .map(|field| field.text().to_owned())
                    .collect::<Vec<_>>(),
            )
            .unwrap();
        assert_eq!(write.value.settings.custom_command, Some(command));
        assert_eq!(write.expected_revision, Some(4));
    }

    #[test]
    fn provider_switch_keeps_exact_custom_and_builtin_buffers_including_invalid_args() {
        let editor_settings = crate::template_flow::default_settings("codex");
        let mut editor = ProfileEditor::new(editor_settings);
        let mut dialog = crate::product::Dialog::new(crate::product::DialogKind::AgentProfile);
        dialog.fields = editor.initial_fields("Draft".into());
        dialog.fields[1] = "builtin role".into();
        let previous_scope = dialog.id;
        assert!(editor.switch_provider("custom", &mut dialog));
        assert_ne!(dialog.id, previous_scope);
        assert_eq!(dialog.fields.len(), 3);
        assert_eq!(dialog.fields[0].text(), "Draft");
        dialog.fields[1] = "relative/tool".into();
        dialog.fields[2] = "'unfinished".into();
        assert!(editor.switch_provider("claude", &mut dialog));
        assert_eq!(dialog.fields.len(), 5);
        assert_eq!(dialog.fields[1].text(), "builtin role");
        dialog.fields[2] = "builtin instructions".into();
        assert!(editor.switch_provider("custom", &mut dialog));
        assert_eq!(dialog.fields[1].text(), "relative/tool");
        assert_eq!(dialog.fields[2].text(), "'unfinished");
        assert!(
            editor
                .write(
                    &dialog
                        .fields
                        .iter()
                        .map(|field| field.text().to_owned())
                        .collect::<Vec<_>>()
                )
                .is_err()
        );
    }
}

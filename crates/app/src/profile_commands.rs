//! Explicit profile editing and reviewed launches through the ordinary host gate.
use super::*;
use product::{Dialog, DialogKind, ProductAction as A};

impl CanvasApp {
    fn profile_editor_open(&mut self, editor: profile_flow::ProfileEditor, name: String) {
        let mut dialog = Dialog::new(DialogKind::AgentProfile);
        dialog.fields = vec![
            name.into(),
            editor.settings.role.clone().into(),
            editor.settings.instructions.clone().into(),
            editor
                .settings
                .engine
                .model
                .clone()
                .unwrap_or_default()
                .into(),
            editor
                .settings
                .engine
                .reasoning_effort
                .clone()
                .unwrap_or_default()
                .into(),
        ];
        dialog.fields[0].select_all();
        self.product.profile_editor = Some(editor);
        self.product.dialog = Some(dialog);
        self.product.notice = None;
    }

    pub(super) fn profile_action(&mut self, action: A, cx: &mut Context<Self>) {
        if self.product.busy
            && !matches!(
                action,
                A::OpenAgentProfiles
                    | A::NewAgentProfile
                    | A::EditAgentProfile(_)
                    | A::DeleteAgentProfile(_)
            )
        {
            self.product.notice = Some("Дождитесь завершения предыдущего действия".into());
            return;
        }
        let supported = self
            .product
            .catalog
            .as_ref()
            .is_some_and(|catalog| catalog.agent_profiles.is_some());
        if !supported {
            self.product.notice =
                Some("Текущий session host не поддерживает профили агентов".into());
            return;
        }
        match action {
            A::ReloadAgentProfiles => {
                let request = uuid::Uuid::new_v4();
                if let Some(dialog) = self
                    .product
                    .dialog
                    .as_ref()
                    .filter(|dialog| dialog.kind == DialogKind::AgentProfiles)
                {
                    let dialog_id = dialog.id;
                    if self
                        .try_product_command(json!({"command":"catalog","_ui_request":request}), cx)
                    {
                        self.profile_reload = Some((request, dialog_id));
                    }
                }
            }
            A::OpenAgentProfiles => {
                self.product.dialog = Some(Dialog::new(DialogKind::AgentProfiles));
                self.product.notice = None;
                if self.product.providers.is_none() {
                    self.detect_providers(cx);
                }
            }
            A::NewAgentProfile => {
                let provider = ["codex", "claude", "opencode"]
                    .into_iter()
                    .find(|provider| {
                        self.product.providers.as_ref().is_some_and(|items| {
                            items
                                .iter()
                                .any(|item| item.provider == *provider && item.available)
                        })
                    })
                    .unwrap_or("codex");
                let mut settings = template_flow::default_settings(provider);
                settings.deliver_context = matches!(provider, "claude" | "codex");
                self.profile_editor_open(
                    profile_flow::ProfileEditor::new(settings),
                    "Новый агент".into(),
                );
            }
            A::SaveLaunchAsProfile => {
                if let Some(preview) = &self.product.launch_preview
                    && matches!(preview.provider.as_str(), "claude" | "codex" | "opencode")
                    && let Some(dialog) = &self.product.dialog
                    && dialog.kind == DialogKind::LaunchReview
                {
                    let settings = pigade_storage::LaunchSettings {
                        provider: preview.provider.clone(),
                        role: dialog.fields[0].text().into(),
                        instructions: dialog.fields[1].text().into(),
                        deliver_context: preview.deliver_context,
                        engine: match pigade_storage::EngineOptions::from_fields(
                            dialog
                                .fields
                                .get(3)
                                .map(text_edit::EditBuffer::text)
                                .unwrap_or(""),
                            dialog
                                .fields
                                .get(4)
                                .map(text_edit::EditBuffer::text)
                                .unwrap_or(""),
                        ) {
                            Ok(engine) => engine,
                            Err(error) => {
                                self.product.notice = Some(error.to_string());
                                return;
                            }
                        },
                    };
                    let mut editor = profile_flow::ProfileEditor::new(settings);
                    editor.origin = Some(dialog.clone());
                    self.profile_editor_open(editor, "Новый агент".into());
                }
            }
            A::EditAgentProfile(id) | A::DeleteAgentProfile(id) | A::LaunchAgentProfile(id) => {
                let Some(profile) = self
                    .product
                    .catalog
                    .as_ref()
                    .and_then(|catalog| catalog.agent_profiles.as_ref())
                    .and_then(|profiles| profiles.iter().find(|value| value.id == id))
                    .cloned()
                else {
                    self.product.notice = Some("Профиль не найден; обновите библиотеку".into());
                    return;
                };
                match action {
                    A::EditAgentProfile(_) => self.profile_editor_open(
                        profile_flow::ProfileEditor::saved(profile.clone()),
                        profile.name,
                    ),
                    A::DeleteAgentProfile(_) => {
                        self.product.profile_delete = Some(profile);
                        self.product.dialog = Some(Dialog::new(DialogKind::DeleteAgentProfile));
                    }
                    _ => {
                        if self.product_launch_unknown {
                            self.product.notice = Some(
                                "Сначала сверьте предыдущий неизвестный запуск через «Сессии»"
                                    .into(),
                            );
                            return;
                        }
                        let Some((tab, layout)) = self.tab_layout() else {
                            self.product.notice =
                                Some("Выберите воркспейс и вкладку перед запуском".into());
                            return;
                        };
                        let Some(dialog) = self.product.dialog.as_ref() else {
                            return;
                        };
                        let pending = profile_flow::Pending {
                            request: uuid::Uuid::new_v4(),
                            dialog: dialog.id,
                            tab,
                            profile: id,
                            revision: profile.revision,
                        };
                        let command = json!({"command":"prepare_profile_launch","_ui_request":pending.request,"tab":tab,
                            "profile":id,"expected_revision":profile.revision,"side":"Right","target":self.selected_pane().or(layout.focused)});
                        if self.try_product_command(command, cx) {
                            self.profile_prepare = Some(pending);
                        }
                    }
                }
            }
            A::SetProfileProvider(provider) => {
                if matches!(provider.as_str(), "codex" | "claude" | "opencode")
                    && let Some(editor) = &mut self.product.profile_editor
                {
                    editor.settings.provider = provider;
                }
            }
            A::ToggleProfileContext => {
                if let Some(editor) = &mut self.product.profile_editor {
                    editor.settings.deliver_context = !editor.settings.deliver_context;
                }
            }
            _ => (),
        }
        cx.notify();
    }

    pub(super) fn submit_profile_dialog(
        &mut self,
        kind: DialogKind,
        fields: &[String],
        cx: &mut Context<Self>,
    ) -> bool {
        match kind {
            DialogKind::AgentProfiles => self.product.dialog = None,
            DialogKind::AgentProfile => {
                if self.product.busy {
                    return true;
                }
                let has_engine_options = [3, 4].into_iter().any(|index| {
                    fields
                        .get(index)
                        .is_some_and(|value| !value.trim().is_empty())
                });
                if has_engine_options && !self.product.engine_options_supported() {
                    self.product.notice = Some(
                        "Этот session host не поддерживает сохранение параметров модели и усилия. Очистите эти поля или обновите host.".into(),
                    );
                    return true;
                }
                if let Some(editor) = &self.product.profile_editor {
                    match editor.write(fields) {
                        Ok(write) => self.edit_catalog(
                            pigade_storage::CatalogBatch {
                                agent_profiles: vec![write],
                                ..Default::default()
                            },
                            "Профиль сохранён",
                            Some(DialogKind::AgentProfiles),
                            cx,
                        ),
                        Err(error) => self.product.notice = Some(error),
                    }
                }
            }
            DialogKind::DeleteAgentProfile => {
                if self.product.busy {
                    return true;
                }
                if let Some(profile) = &self.product.profile_delete {
                    self.edit_catalog(
                        pigade_storage::CatalogBatch {
                            remove_agent_profiles: vec![pigade_storage::AgentProfileDelete {
                                id: profile.id,
                                expected_revision: profile.revision,
                            }],
                            ..Default::default()
                        },
                        "Профиль удалён. Сессии продолжают работать",
                        Some(DialogKind::AgentProfiles),
                        cx,
                    );
                }
            }
            _ => return false,
        }
        true
    }

    /// An old prepare receipt cannot replace a closed editor or a newer review.
    pub(super) fn accept_profile_preview(
        &mut self,
        request: Option<uuid::Uuid>,
        preview: &protocol::LaunchPreview,
        confirmed: bool,
        phase: Option<&str>,
    ) -> bool {
        if self
            .profile_prepare
            .as_ref()
            .is_some_and(|pending| Some(pending.request) == request)
        {
            let pending = self
                .profile_prepare
                .take()
                .expect("matching pending profile");
            let valid = self
                .product
                .dialog
                .as_ref()
                .is_some_and(|dialog| dialog.id == pending.dialog)
                && self
                    .product
                    .catalog
                    .as_ref()
                    .and_then(|catalog| catalog.selection.tab_id)
                    == Some(pending.tab)
                && preview.tab_id == pending.tab
                && preview.profile.as_ref().is_some_and(|profile| {
                    profile.id == pending.profile && profile.revision == pending.revision
                });
            self.pending_launch_review = valid;
            return valid;
        }
        preview.profile.is_none() || confirmed || phase.is_some()
    }
}

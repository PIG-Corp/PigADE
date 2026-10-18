//! Explicit profile editing and reviewed launches through the ordinary host gate.
use super::*;
use product::{Dialog, DialogKind, ProductAction as A};

impl CanvasApp {
    fn update_profile_roster(
        &mut self,
        id: uuid::Uuid,
        pinned: Option<bool>,
        delta: Option<isize>,
        cx: &mut Context<Self>,
    ) {
        if self.product.profile_roster_error.is_some() {
            self.product.notice =
                Some("Обновите библиотеку (Ctrl+R) перед изменением порядка.".into());
            return;
        }
        if self.profile_roster_pending.is_some() {
            self.product.notice = Some("Дождитесь завершения обновления порядка профилей".into());
            return;
        }
        let Some(dialog) = self
            .product
            .dialog
            .as_ref()
            .filter(|dialog| dialog.kind == DialogKind::AgentProfiles)
        else {
            return;
        };
        let dialog_id = dialog.id;
        let Some(catalog) = self.product.catalog.as_ref() else {
            return;
        };
        let Some(profiles) = catalog.agent_profiles.as_ref() else {
            return;
        };
        let ids: Vec<_> = profiles.iter().map(|profile| profile.id).collect();
        let Some(roster) = catalog.agent_profile_roster.as_ref() else {
            self.product.notice =
                Some("Текущий host не поддерживает изменение порядка профилей".into());
            return;
        };
        let order = profile_roster::display_order(&ids, Some(roster));
        if !order.can_mutate {
            self.product.notice = Some(
                order
                    .warning
                    .unwrap_or("Обновите библиотеку перед изменением порядка профилей.")
                    .into(),
            );
            return;
        }
        let changed = if let Some(pinned) = pinned {
            profile_roster::set_pinned(&ids, roster, id, pinned)
        } else if let Some(delta) = delta {
            profile_roster::move_within_partition(&ids, roster, id, delta)
        } else {
            None
        };
        let Some((ordered_ids, pinned_ids)) = changed else {
            self.product.notice = Some("Профиль нельзя переместить дальше в этой группе".into());
            return;
        };
        let request = uuid::Uuid::new_v4();
        let expected_revision = roster.revision;
        let command =
            profile_roster::command_payload(request, expected_revision, &ordered_ids, &pinned_ids);
        if self.try_product_command(command, cx) {
            self.profile_roster_pending = Some(profile_flow::RosterPending {
                request,
                dialog: dialog_id,
                expected_revision,
                selected_profile: id,
                ordered_ids,
                pinned_ids,
            });
        }
    }

    fn profile_editor_open(&mut self, editor: profile_flow::ProfileEditor, name: String) {
        let mut dialog = Dialog::new(DialogKind::AgentProfile);
        dialog.fields = editor.initial_fields(name);
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
                    let selected_profile = self.product.catalog.as_ref().and_then(|catalog| {
                        let profiles = catalog.agent_profiles.as_ref()?;
                        let ids: Vec<_> = profiles.iter().map(|profile| profile.id).collect();
                        profile_roster::display_order(&ids, catalog.agent_profile_roster.as_ref())
                            .selected_id(dialog.active_field)
                    });
                    if self
                        .try_product_command(json!({"command":"catalog","_ui_request":request}), cx)
                    {
                        self.profile_reload = Some(profile_flow::RosterReload {
                            request,
                            dialog: dialog_id,
                            selected_profile,
                        });
                    }
                }
            }
            A::SetAgentProfilePinned(id, pinned) => {
                self.update_profile_roster(id, Some(pinned), None, cx)
            }
            A::MoveAgentProfile(id, delta) => self.update_profile_roster(id, None, Some(delta), cx),
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
            A::NewCustomProfile => {
                let custom_supported = self
                    .product
                    .catalog
                    .as_ref()
                    .is_some_and(|catalog| catalog.custom_cli_schema == Some(1));
                if !custom_supported {
                    self.product.notice = Some(
                        "Текущий session host не поддерживает пользовательские CLI-профили".into(),
                    );
                    return;
                }
                self.profile_editor_open(
                    profile_flow::ProfileEditor::new(template_flow::default_settings("custom")),
                    "Новый профиль CLI".into(),
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
                        custom_command: None,
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
                        if profile.settings.provider == "custom"
                            && !self
                                .product
                                .catalog
                                .as_ref()
                                .is_some_and(|catalog| catalog.custom_cli_schema == Some(1))
                        {
                            self.product.notice = Some(
                                "Текущий session host не поддерживает пользовательские CLI-профили"
                                    .into(),
                            );
                            return;
                        }
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
                let custom_supported = self
                    .product
                    .catalog
                    .as_ref()
                    .is_some_and(|catalog| catalog.custom_cli_schema == Some(1));
                if (provider != "custom" || custom_supported)
                    && let (Some(editor), Some(dialog)) =
                        (&mut self.product.profile_editor, &mut self.product.dialog)
                {
                    editor.switch_provider(&provider, dialog);
                }
            }
            A::ToggleProfileContext => {
                if let Some(editor) = &mut self.product.profile_editor
                    && editor.settings.provider != "custom"
                {
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
                    if editor.settings.provider == "custom"
                        && !self
                            .product
                            .catalog
                            .as_ref()
                            .is_some_and(|catalog| catalog.custom_cli_schema == Some(1))
                    {
                        self.product.notice = Some(
                            "Этот session host не поддерживает сохранение пользовательских CLI-профилей".into(),
                        );
                        return true;
                    }
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

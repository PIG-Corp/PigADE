//! Native recipe actions. Only matching host receipts can advance a launch flow.
use super::*;
use product::{Dialog, DialogKind, ProductAction as A};
use template_flow::{Origin, Pending, RecipeEditor, RequestKind};

fn slot_dialog(config: &pigade_storage::LaunchSettings) -> Dialog {
    let mut dialog = Dialog::new(DialogKind::RecipeSlot);
    if config.provider == "custom" {
        dialog.fields.clear();
    } else {
        dialog.fields[0] = config.role.clone().into();
        dialog.fields[1] = config.instructions.clone().into();
        dialog.fields[2] = config.engine.model.clone().unwrap_or_default().into();
        dialog.fields[3] = config
            .engine
            .reasoning_effort
            .clone()
            .unwrap_or_default()
            .into();
    }
    dialog
}

impl CanvasApp {
    fn template_supported(&self) -> bool {
        self.product
            .catalog
            .as_ref()
            .is_some_and(|catalog| catalog.template_launch_schema == Some(1))
    }
    fn sync_template_flags(&mut self) {
        self.product.run_busy = self.template_flow.pending.is_some();
        self.product.run_automatic = self.template_flow.automatic;
        self.product.run_unverified = self.template_flow.unknown;
    }
    fn show_recipe(&mut self, editor: RecipeEditor, kind: DialogKind) {
        let mut dialog = Dialog::new(kind);
        if kind == DialogKind::NewTemplateTab {
            dialog.fields[0] = editor.name.clone().into();
            dialog.fields[0].select_all();
        }
        self.product.recipe_editor = Some(editor);
        self.product.dialog = Some(dialog);
        self.product.notice = None;
    }
    fn remember_recipe_name(&mut self) {
        if let Some(dialog) = &self.product.dialog
            && dialog.kind == DialogKind::NewTemplateTab
            && let Some(editor) = &mut self.product.recipe_editor
        {
            editor.name = dialog.fields[0].text().to_owned();
        }
    }
    fn request_template(
        &mut self,
        command: Value,
        kind: RequestKind,
        run: Option<uuid::Uuid>,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.template_flow.pending.is_some() {
            self.product.notice = Some("Дождитесь подтверждения предыдущего запроса".into());
            cx.notify();
            return false;
        }
        if kind == RequestKind::Advance && self.template_flow.unknown {
            self.product.notice = Some("Сначала обновите состояние запуска".into());
            cx.notify();
            return false;
        }
        let request = uuid::Uuid::new_v4();
        let mut command = command;
        command["_ui_request"] = json!(request);
        if !self.try_product_command(command, cx) {
            return false;
        }
        self.template_flow.pending = Some(Pending {
            request,
            dialog: self.product.dialog.as_ref().map(|dialog| dialog.id),
            kind,
            run,
        });
        self.sync_template_flags();
        true
    }
    fn advance_template(&mut self, cx: &mut Context<Self>) {
        if let Some(run) = &self.product.template_run
            && run.can_advance
            && !matches!(run.status.as_str(), "complete" | "cancelled")
        {
            if !self.request_template(json!({"command":"advance_template_run","run":run.id,"expected_revision":run.revision}),RequestKind::Advance,Some(run.id),cx) {self.template_flow.pause();}
        } else {
            self.template_flow.pause();
            self.sync_template_flags();
        }
    }
    fn cancel_template(&mut self, cx: &mut Context<Self>) {
        // Send a queued cancellation once. A lost ACK only authorizes a read.
        self.template_flow.cancel_requested = false;
        if let Some(run) = &self.product.template_run {
            self.request_template(json!({"command":"cancel_template_run","run":run.id,"expected_revision":run.revision}),RequestKind::Cancel,Some(run.id),cx);
        }
    }

    pub(super) fn template_action(
        &mut self,
        action: A,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match action {
            A::ChoosePreset(preset) => {
                if !self.template_supported() {
                    self.product.notice =
                        Some("Шаблоны запуска недоступны в текущем session host".into());
                    return;
                }
                if self
                    .product
                    .catalog
                    .as_ref()
                    .is_none_or(|catalog| catalog.selection.workspace_id.is_none())
                {
                    self.product.notice = Some("Сначала выберите воркспейс".into());
                    return;
                }
                let provider = ["codex", "claude", "opencode"]
                    .into_iter()
                    .find(|provider| {
                        self.product.providers.as_ref().is_some_and(|infos| {
                            infos
                                .iter()
                                .any(|info| info.available && info.provider == *provider)
                        })
                    })
                    .unwrap_or("shell");
                self.show_recipe(preset.draft(provider), DialogKind::NewTemplateTab);
            }
            A::ConfigureTemplate(id) | A::NewTabFromTemplate(id) => {
                if !self.template_supported() {
                    self.product.notice =
                        Some("Текущий session host поддерживает только геометрию".into());
                    return;
                }
                let template = self
                    .product
                    .catalog
                    .as_ref()
                    .and_then(|catalog| catalog.layout_templates.as_ref())
                    .and_then(|templates| templates.iter().find(|template| template.id == id))
                    .cloned();
                if let Some(template) = template {
                    let origin = if matches!(action, A::ConfigureTemplate(_)) {
                        Origin::Saved
                    } else {
                        Origin::NewTab
                    };
                    if origin == Origin::NewTab && template.launches.is_none() {
                        self.product.notice =
                            Some("Сначала настройте агентов в этом шаблоне".into());
                        return;
                    }
                    match RecipeEditor::from_saved(template, origin) {
                        Ok(editor) => self.show_recipe(
                            editor,
                            if origin == Origin::Saved {
                                DialogKind::Recipe
                            } else {
                                DialogKind::NewTemplateTab
                            },
                        ),
                        Err(error) => self.product.notice = Some(error),
                    }
                }
            }
            A::EditRecipe => {
                self.remember_recipe_name();
                self.product.dialog = Some(Dialog::new(DialogKind::Recipe));
            }
            A::EditTemplateSlot(index) => {
                self.remember_recipe_name();
                let parent = self.product.dialog.as_ref().map(|dialog| dialog.kind);
                if let Some(editor) = &mut self.product.recipe_editor
                    && let Some(config) = editor.launches.get(index)
                {
                    let dialog = slot_dialog(config);
                    editor.slot = index;
                    editor.slot_parent = parent;
                    self.product.dialog = Some(dialog);
                }
            }
            A::OpenRecipeProfiles => {
                let Some(origin) = self
                    .product
                    .dialog
                    .as_ref()
                    .filter(|dialog| dialog.kind == DialogKind::RecipeSlot)
                    .cloned()
                else {
                    return;
                };
                let Some(editor) = self.product.recipe_editor.as_ref() else {
                    return;
                };
                let Some(baseline) = editor.launches.get(editor.slot).cloned() else {
                    return;
                };
                let Some(catalog) = &self.product.catalog else {
                    return;
                };
                let Some(profiles) = catalog.agent_profiles.as_ref() else {
                    self.product.notice = Some("Профили недоступны в текущем session host".into());
                    return;
                };
                match crate::recipe_profiles::Picker::new(
                    origin,
                    editor.slot,
                    baseline,
                    profiles,
                    catalog.agent_profile_roster.as_ref(),
                ) {
                    Ok(picker) => {
                        self.product.recipe_profiles = Some(picker);
                        self.product.recipe_profile_scroll.scroll_to_item(0);
                        self.product.dialog = Some(Dialog::new(DialogKind::RecipeProfiles));
                        self.product.notice = None;
                    }
                    Err(error) => self.product.notice = Some(error),
                }
            }
            A::CancelRecipeProfiles => {
                if self
                    .product
                    .dialog
                    .as_ref()
                    .is_some_and(|dialog| dialog.kind == DialogKind::RecipeProfiles)
                    && let Some(picker) = self.product.recipe_profiles.take()
                {
                    self.product.dialog = Some(picker.origin);
                    self.product.notice = None;
                }
            }
            A::ApplyRecipeProfile(id) => self.apply_recipe_profile(id),
            A::SetTemplateProvider(provider) => {
                if !matches!(provider.as_str(), "shell" | "codex" | "claude" | "opencode") {
                    return;
                }
                if self
                    .product
                    .dialog
                    .as_ref()
                    .is_none_or(|dialog| dialog.kind != DialogKind::RecipeSlot)
                {
                    return;
                }
                if let Some(editor) = &mut self.product.recipe_editor
                    && let Some(config) = editor.launches.get_mut(editor.slot)
                {
                    if config.provider == "custom" {
                        *config = template_flow::default_settings(&provider);
                        self.product.dialog = Some(slot_dialog(config));
                    } else {
                        config.provider = provider;
                    }
                    editor.changed();
                }
            }
            A::SetTemplateRole(role) => {
                if let Some(dialog) = &mut self.product.dialog
                    && dialog.kind == DialogKind::RecipeSlot
                {
                    dialog.fields[0] = template_flow::role_text(&role).into();
                    self.editor_layouts.remove(&0);
                    window.notify_text_input_changed(&self.focus);
                }
            }
            A::ToggleTemplateContext => {
                if let Some(editor) = &mut self.product.recipe_editor {
                    let config = &mut editor.launches[editor.slot];
                    if config.provider == "custom" {
                        return;
                    }
                    config.deliver_context = !config.deliver_context;
                    editor.changed();
                }
            }
            A::ClearTemplateLaunches => {
                if let Some(mut template) = self
                    .product
                    .recipe_editor
                    .as_ref()
                    .and_then(|editor| editor.saved.clone())
                {
                    template.launches = None;
                    self.edit_catalog(
                        pigade_storage::CatalogBatch {
                            layout_templates: vec![pigade_storage::CatalogWrite {
                                expected_revision: Some(template.revision),
                                value: template,
                            }],
                            ..Default::default()
                        },
                        "В шаблоне сохранена только геометрия",
                        Some(DialogKind::Layouts),
                        cx,
                    );
                }
            }
            A::ConfirmTemplateRun => {
                if self.template_flow.pending.is_some() || self.template_flow.unknown {
                    return;
                }
                self.template_flow.automatic = true;
                self.advance_template(cx);
            }
            A::RefreshTemplateRun => {
                self.template_flow.pause();
                if let Some(run) = &self.product.template_run {
                    self.request_template(
                        json!({"command":"get_template_run","run":run.id}),
                        RequestKind::Read,
                        Some(run.id),
                        cx,
                    );
                }
            }
            A::CancelTemplateRun => {
                self.template_flow.pause();
                if self.template_flow.pending.is_some() {
                    self.template_flow.cancel_requested = true;
                } else {
                    self.cancel_template(cx);
                }
            }
            A::InspectTemplateRunSlot(index) => {
                self.template_flow.pause();
                if self
                    .product
                    .template_run
                    .as_ref()
                    .is_some_and(|run| index < run.slots.len())
                {
                    self.product.run_context_slot = index;
                    self.product.dialog = Some(Dialog::new(DialogKind::TemplateRunContext));
                }
            }
            A::ReturnTemplateRun => {
                self.product.dialog = Some(Dialog::new(DialogKind::TemplateRun))
            }
            A::OpenTemplateRuns | A::LoadTemplateRuns(_) => {
                self.template_flow.pause();
                if !self.template_supported() {
                    self.product.notice =
                        Some("Незавершённые запуски недоступны в текущем session host".into());
                    return;
                }
                let before = if let A::LoadTemplateRuns(before) = action {
                    before
                } else {
                    None
                };
                self.product.dialog = Some(Dialog::new(DialogKind::TemplateRuns));
                self.request_template(
                    json!({"command":"list_template_runs","before":before}),
                    RequestKind::List,
                    None,
                    cx,
                );
            }
            A::SelectTemplateRun(id) => {
                self.template_flow.pause();
                self.request_template(
                    json!({"command":"get_template_run","run":id}),
                    RequestKind::Read,
                    Some(id),
                    cx,
                );
            }
            _ => unreachable!("non-template product action"),
        }
        self.sync_template_flags();
    }

    fn apply_recipe_profile(&mut self, id: uuid::Uuid) {
        let Some(dialog) = self
            .product
            .dialog
            .as_ref()
            .filter(|dialog| dialog.kind == DialogKind::RecipeProfiles)
        else {
            return;
        };
        let Some(picker) = self.product.recipe_profiles.as_ref() else {
            return;
        };
        let query = dialog
            .fields
            .first()
            .map_or("", text_edit::EditBuffer::text);
        if !picker
            .profiles(query)
            .iter()
            .any(|profile| profile.id == id)
        {
            return;
        }
        let custom_supported = self
            .product
            .catalog
            .as_ref()
            .is_some_and(|catalog| catalog.custom_cli_schema == Some(1));
        let engine_supported = self.product.engine_options_supported();
        let Some(editor) = self.product.recipe_editor.as_mut() else {
            return;
        };
        match picker.apply(id, editor, custom_supported, engine_supported) {
            Ok(profile) => {
                self.product.dialog = Some(slot_dialog(&profile.settings));
                self.product.recipe_profiles = None;
                self.product.notice = Some(format!(
                    "Скопирован профиль «{}» · ревизия {}. Изменения профиля не обновят шаблон.",
                    profile.name, profile.revision
                ));
            }
            Err(error) => self.product.notice = Some(error),
        }
    }

    pub(super) fn submit_template_dialog(
        &mut self,
        kind: DialogKind,
        fields: &[String],
        cx: &mut Context<Self>,
    ) -> bool {
        if matches!(
            kind,
            DialogKind::RecipeSlot | DialogKind::Recipe | DialogKind::NewTemplateTab
        ) && self.product.recipe_editor.as_ref().is_some_and(|editor| {
            editor
                .launches
                .iter()
                .any(|config| config.provider == "custom")
        }) && self
            .product
            .catalog
            .as_ref()
            .is_none_or(|catalog| catalog.custom_cli_schema != Some(1))
        {
            self.product.notice = Some("Текущий session host не поддерживает пользовательские команды. Настройки сохранены в черновике.".into());
            return true;
        }
        match kind {
            DialogKind::RecipeProfiles => {
                let selected = self
                    .product
                    .recipe_profiles
                    .as_ref()
                    .and_then(|picker| {
                        picker.selected(fields.first().map(String::as_str).unwrap_or(""))
                    })
                    .map(|profile| profile.id);
                if let Some(id) = selected {
                    self.apply_recipe_profile(id);
                } else {
                    self.product.notice = Some("Подходящих профилей нет. Измените поиск или вернитесь к настройкам панели.".into());
                }
            }
            DialogKind::RecipeSlot => {
                let engine_options_supported = self.product.engine_options_supported();
                if let Some(editor) = &mut self.product.recipe_editor {
                    let mut config = editor.launches[editor.slot].clone();
                    if config.provider != "custom" {
                        config.role = fields.first().cloned().unwrap_or_default();
                        config.instructions = fields.get(1).cloned().unwrap_or_default();
                        config.engine = match pigade_storage::EngineOptions::from_fields(
                            fields.get(2).map(String::as_str).unwrap_or(""),
                            fields.get(3).map(String::as_str).unwrap_or(""),
                        ) {
                            Ok(engine) => engine,
                            Err(error) => {
                                self.product.notice = Some(error.to_string());
                                return true;
                            }
                        };
                    }
                    if !config.engine.is_empty() && !engine_options_supported {
                        self.product.notice=Some("Session host не поддерживает параметры модели. Обновите host; значения не будут отброшены молча.".into());
                        return true;
                    }
                    if let Err(error) = config.validate() {
                        self.product.notice = Some(error.to_string());
                        return true;
                    }
                    if editor.launches[editor.slot] != config {
                        editor.launches[editor.slot] = config;
                        editor.changed();
                    }
                    let parent = editor.slot_parent.unwrap_or(DialogKind::Recipe);
                    let editor = editor.clone();
                    self.show_recipe(editor, parent);
                }
            }
            DialogKind::Recipe => {
                if let Some(editor) = &self.product.recipe_editor {
                    if editor.origin == Origin::NewTab {
                        let editor = editor.clone();
                        self.show_recipe(editor, DialogKind::NewTemplateTab);
                    } else if let Some(mut template) = editor.saved.clone() {
                        template.launches = Some(editor.launches.clone());
                        self.edit_catalog(
                            pigade_storage::CatalogBatch {
                                layout_templates: vec![pigade_storage::CatalogWrite {
                                    expected_revision: Some(template.revision),
                                    value: template,
                                }],
                                ..Default::default()
                            },
                            "Состав агентов сохранён. Работающие CLI не изменены.",
                            Some(DialogKind::Layouts),
                            cx,
                        );
                    }
                }
            }
            DialogKind::NewTemplateTab => {
                if !self.template_supported() {
                    self.product.notice =
                        Some("Шаблоны запуска недоступны в текущем session host".into());
                    return true;
                }
                let name = fields.first().map(|value| value.trim()).unwrap_or_default();
                if name.is_empty() || name.len() > 256 || name.chars().any(char::is_control) {
                    self.product.notice = Some("Введите название вкладки до 256 байт UTF-8".into());
                    return true;
                }
                if let (Some(editor), Some(workspace)) = (
                    &self.product.recipe_editor,
                    self.product
                        .catalog
                        .as_ref()
                        .and_then(|catalog| catalog.selection.workspace_id),
                ) {
                    let operation = uuid::Uuid::new_v4();
                    self.template_flow.pause();
                    self.template_flow.unknown = false;
                    self.request_template(json!({"command":"prepare_template_run","request":{"operation":operation,"workspace":workspace,"name":name,"source":editor.source()}}),RequestKind::Prepare,Some(operation),cx);
                } else {
                    self.product.notice = Some("Сначала выберите воркспейс".into());
                }
            }
            DialogKind::TemplateRun | DialogKind::TemplateRuns => {
                self.template_flow.pause();
                self.product.dialog = None;
            }
            DialogKind::TemplateRunContext => {
                self.product.dialog = Some(Dialog::new(DialogKind::TemplateRun))
            }
            _ => return false,
        }
        self.sync_template_flags();
        true
    }

    pub(super) fn receive_template_run(
        &mut self,
        run: protocol::TemplateRunView,
        pending: Pending,
        cx: &mut Context<Self>,
    ) {
        self.product.run_busy = false;
        if Some(run.id) != pending.run || run.slots.is_empty() || run.slots.len() > 32 {
            self.template_flow.failed(pending.kind);
            self.product.notice = Some(
                "Получен ответ для другого запуска; автоматическое добавление остановлено".into(),
            );
            self.sync_template_flags();
            return;
        }
        let same_dialog = pending.dialog == self.product.dialog.as_ref().map(|dialog| dialog.id);
        let completed = run.status == "complete";
        if completed {
            self.template_flow.cancel_requested = false;
        }
        let next = run.can_advance && run.error.is_none();
        self.template_flow.unknown = false;
        self.product.template_run = Some(run);
        if same_dialog && matches!(pending.kind, RequestKind::Prepare | RequestKind::Read) {
            self.product.dialog = Some(Dialog::new(DialogKind::TemplateRun));
        }
        if pending.kind == RequestKind::Cancel {
            self.template_flow.cancel_requested = false;
            self.template_flow.pause();
        }
        if self.template_flow.cancel_requested && !completed {
            self.cancel_template(cx);
        } else if self.template_flow.automatic && same_dialog {
            if completed {
                self.template_flow.pause();
                self.product.dialog = None;
                self.product.notice = Some("Вкладка создана. Новые сессии запущены.".into());
            } else if next {
                self.advance_template(cx);
            } else {
                self.template_flow.pause();
            }
        } else {
            self.template_flow.pause();
        }
        self.sync_template_flags();
        cx.notify();
    }

    pub(super) fn template_request_failed(
        &mut self,
        pending: Pending,
        message: String,
        cx: &mut Context<Self>,
    ) {
        self.template_flow.failed(pending.kind);
        if matches!(
            pending.kind,
            RequestKind::Prepare | RequestKind::Advance | RequestKind::Cancel
        ) && let Some(id) = pending.run
        {
            self.request_template(
                json!({"command":"get_template_run","run":id}),
                RequestKind::Read,
                Some(id),
                cx,
            );
        }
        self.product.notice = Some(format!("Добавление приостановлено: {message}"));
        self.sync_template_flags();
        cx.notify();
    }

    pub(super) fn receive_template_list(
        &mut self,
        page: pigade_storage::TemplateRunPage,
        pending: Pending,
        cx: &mut Context<Self>,
    ) {
        if pending.kind == RequestKind::List && page.entries.len() <= 32 {
            self.product.template_runs = Some(page);
        } else {
            self.product.notice =
                Some("Получена некорректная страница незавершённых вкладок".into());
        }
        self.sync_template_flags();
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::slot_dialog;
    use crate::template_flow;

    #[test]
    fn custom_slot_has_no_stale_builtin_editors_and_new_form_scope() {
        let mut config = template_flow::default_settings("codex");
        config.role = "Exact role".into();
        config.engine.model = Some("model".into());
        let builtin = slot_dialog(&config);
        assert_eq!(builtin.fields.len(), 4);
        assert_eq!(builtin.fields[0].text(), "Exact role");
        assert_eq!(builtin.fields[2].text(), "model");
        let mut custom = template_flow::default_settings("custom");
        custom.custom_command = Some(pigade_storage::CustomCommand {
            executable: "/usr/bin/tool".into(),
            args: vec!["two words".into()],
        });
        let readonly = slot_dialog(&custom);
        assert!(readonly.fields.is_empty());
        assert_ne!(builtin.id, readonly.id);
        let returned = slot_dialog(&config);
        assert_ne!(returned.id, builtin.id);
        assert_eq!(returned.fields[2].text(), "model");
    }
}

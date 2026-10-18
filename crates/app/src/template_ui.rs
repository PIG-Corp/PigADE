//! Optional recipe editing and explicit launch progress, using the product chrome.
use super::*;
use crate::template_flow::{Origin, Preset};

fn provider_label(provider: &str) -> &str {
    match provider {
        "shell" => "Терминал",
        "codex" => "Codex",
        "claude" => "Claude Code",
        "opencode" => "OpenCode",
        "custom" => "Пользовательский CLI",
        other => other,
    }
}

fn row_note(muted: u32, text: impl Into<String>) -> Div {
    div()
        .text_size(px(11.))
        .text_color(rgb(muted))
        .whitespace_normal()
        .child(text.into())
}

fn phase_label(phase: &str) -> &'static str {
    match phase {
        "prepared" => "Ожидает запуска",
        "attempting" => "Запуск начат",
        "not_requested" => "Запущен без контекста",
        "delivered_unconfirmed" => "Запущен · передача контекста не подтверждена",
        "unknown" => "Исход запуска неизвестен",
        _ => "Требует проверки",
    }
}

impl ProductUi {
    pub(super) fn recipe_supported(&self) -> bool {
        self.catalog
            .as_ref()
            .is_some_and(|catalog| catalog.template_launch_schema == Some(1))
    }

    pub(super) fn recipe_menu_entries(&self) -> Vec<(String, ProductAction)> {
        match self.dialog.as_ref().map(|dialog| dialog.kind) {
            Some(DialogKind::Recipe) => self
                .recipe_editor
                .as_ref()
                .map(|editor| {
                    editor
                        .launches
                        .iter()
                        .enumerate()
                        .map(|(index, config)| {
                            (
                                format!("{}. {}", index + 1, provider_label(&config.provider)),
                                ProductAction::EditTemplateSlot(index),
                            )
                        })
                        .collect()
                })
                .unwrap_or_default(),
            Some(DialogKind::TemplateRun) => {
                let Some(run) = &self.template_run else {
                    return vec![];
                };
                let mut entries = Vec::new();
                if run.can_advance && !self.run_unverified {
                    entries.push((
                        "Запустить / продолжить".into(),
                        ProductAction::ConfirmTemplateRun,
                    ));
                }
                if run.status == "complete" {
                    entries.push((
                        "Открыть вкладку".into(),
                        ProductAction::SelectTab(run.tab_id),
                    ));
                }
                entries.push((
                    "Обновить состояние".into(),
                    ProductAction::RefreshTemplateRun,
                ));
                entries.extend(run.slots.iter().enumerate().map(|(index, slot)| {
                    (
                        format!(
                            "{} {} · {}",
                            if slot.preview.provider == "custom" {
                                "Команда"
                            } else {
                                "Контекст"
                            },
                            index + 1,
                            provider_label(&slot.preview.provider)
                        ),
                        ProductAction::InspectTemplateRunSlot(index),
                    )
                }));
                if !matches!(run.status.as_str(), "complete" | "cancelled") {
                    entries.push((
                        "Отменить оставшиеся запуски".into(),
                        ProductAction::CancelTemplateRun,
                    ));
                }
                entries
            }
            Some(DialogKind::TemplateRuns) => {
                let mut entries = self
                    .template_runs
                    .as_ref()
                    .map(|page| {
                        page.entries
                            .iter()
                            .map(|run| (run.name.clone(), ProductAction::SelectTemplateRun(run.id)))
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                if let Some(before) = self
                    .template_runs
                    .as_ref()
                    .and_then(|page| page.next_before)
                {
                    entries.push((
                        "Следующая страница".into(),
                        ProductAction::LoadTemplateRuns(Some(before)),
                    ));
                }
                entries
            }
            Some(DialogKind::TemplateRunContext) => {
                vec![("Назад к запуску".into(), ProductAction::ReturnTemplateRun)]
            }
            Some(DialogKind::RecipeProfiles) => self
                .recipe_profiles
                .as_ref()
                .and_then(|picker| {
                    let query = self.dialog.as_ref()?.fields[0].text();
                    picker.selected(query).map(|profile| {
                        (
                            format!("Применить · {}", profile.name),
                            ProductAction::ApplyRecipeProfile(profile.id),
                        )
                    })
                })
                .into_iter()
                .collect(),
            _ => vec![],
        }
    }

    pub(super) fn template_dialog_fields(
        &self,
        dialog: &Dialog,
        mut content: Div,
        cx: &mut Context<CanvasApp>,
    ) -> Div {
        let colors = self.colors();
        if dialog.kind == DialogKind::NewTab && self.recipe_supported() {
            content = content.child(row_note(
                chrome::Colors::for_theme(self.appearance.theme).muted,
                "Или начните с состава агентов:",
            ));
            for (index, preset) in Preset::ALL.into_iter().enumerate() {
                content = content.child(self.action_control(
                    format!("new-tab-preset-{index}"),
                    preset.label().into(),
                    ProductAction::ChoosePreset(preset),
                    ControlStyle {
                        role: Role::Button,
                        selected: false,
                        compact: false,
                    },
                    cx,
                ));
            }
        }
        if matches!(dialog.kind, DialogKind::Recipe | DialogKind::NewTemplateTab)
            && let Some(editor) = &self.recipe_editor
        {
            content = content.child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(div().w(px(86.)).h(px(52.)).flex().children(
                        editor.geometry.root.as_ref().map(|node| {
                            template_thumbnail(
                                node,
                                chrome::Colors::for_theme(self.appearance.theme),
                            )
                        }),
                    ))
                    .child(
                        div()
                            .flex_1()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_size(px(13.))
                                    .child(editor.name.clone()),
                            )
                            .child(row_note(
                                chrome::Colors::for_theme(self.appearance.theme).muted,
                                format!("{} новых процессов", editor.launches.len()),
                            )),
                    ),
            );
            content=content.child(row_note(chrome::Colors::for_theme(self.appearance.theme).muted, "Каждый агент запускается как новая сессия. Память, авторизация и conversation ID в шаблон не копируются."));
            if editor.origin == Origin::Saved {
                content = content.child(row_note(
                    chrome::Colors::for_theme(self.appearance.theme).muted,
                    "Это настройки будущих сессий. Открытые CLI сохранят свои параметры.",
                ));
            }
            for (index, config) in editor.launches.iter().enumerate() {
                let label = format!("{}. {}", index + 1, provider_label(&config.provider));
                let detail = if config.provider == "custom" {
                    "Пользовательская команда запуска"
                } else if config.deliver_context {
                    config
                        .role
                        .lines()
                        .next()
                        .filter(|text| !text.is_empty())
                        .unwrap_or("Контекст воркспейса")
                } else {
                    "Без передачи контекста"
                };
                let mut slot = div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(self.action_control(
                        format!("recipe-slot-{index}"),
                        label,
                        ProductAction::EditTemplateSlot(index),
                        ControlStyle {
                            role: Role::Button,
                            selected: dialog.kind == DialogKind::Recipe
                                && dialog.active_field == index,
                            compact: false,
                        },
                        cx,
                    ))
                    .child(row_note(
                        chrome::Colors::for_theme(self.appearance.theme).muted,
                        detail,
                    ));
                if config.provider == "custom" {
                    if let Some(command) = &config.custom_command {
                        slot = slot.child(row_note(
                            chrome::Colors::for_theme(self.appearance.theme).muted,
                            command.executable.clone(),
                        ));
                        for (arg_index, arg) in command.args.iter().enumerate() {
                            slot = slot.child(row_note(
                                chrome::Colors::for_theme(self.appearance.theme).muted,
                                format!("Аргумент {}: {arg:?}", arg_index + 1),
                            ));
                        }
                    }
                } else {
                    slot = slot.child(row_note(
                        chrome::Colors::for_theme(self.appearance.theme).muted,
                        format!(
                            "Модель: {} · reasoning: {}",
                            config.engine.model.as_deref().unwrap_or("настройки CLI"),
                            config
                                .engine
                                .reasoning_effort
                                .as_deref()
                                .unwrap_or("настройки CLI")
                        ),
                    ));
                }
                content = content.child(slot);
            }
            if dialog.kind == DialogKind::Recipe {
                content = content.child(row_note(
                    chrome::Colors::for_theme(self.appearance.theme).muted,
                    "↑ ↓ — панель · Enter — настроить · Ctrl+Enter — сохранить",
                ));
                if editor.origin == Origin::Saved
                    && editor
                        .saved
                        .as_ref()
                        .is_some_and(|template| template.launches.is_some())
                {
                    content = content.child(self.action_control(
                        "recipe-geometry-only".into(),
                        "Оставить только геометрию".into(),
                        ProductAction::ClearTemplateLaunches,
                        ControlStyle {
                            role: Role::Button,
                            selected: false,
                            compact: true,
                        },
                        cx,
                    ));
                }
            } else {
                let cwd =
                    self.catalog
                        .as_ref()
                        .and_then(|catalog| {
                            catalog.workspaces.iter().find(|workspace| {
                                Some(workspace.id) == catalog.selection.workspace_id
                            })
                        })
                        .map(|workspace| workspace.cwd.display().to_string())
                        .unwrap_or_else(|| "Воркспейс не выбран".into());
                content = content
                    .child(row_note(
                        chrome::Colors::for_theme(self.appearance.theme).muted,
                        format!("Папка: {cwd}"),
                    ))
                    .child(self.action_control(
                        "new-template-edit".into(),
                        "Изменить состав агентов…".into(),
                        ProductAction::EditRecipe,
                        ControlStyle {
                            role: Role::Button,
                            selected: false,
                            compact: true,
                        },
                        cx,
                    ));
            }
        }
        if dialog.kind == DialogKind::RecipeProfiles {
            let colors = chrome::Colors::for_theme(self.appearance.theme);
            let query = dialog
                .fields
                .first()
                .map(|field| field.text())
                .unwrap_or("");
            content = content.child(row_note(
                colors.muted,
                "Выбор заменит настройки этой панели. Это копия: изменения профиля не обновят шаблон. Ничего не запускается.",
            ));
            let mut rows = div()
                .id("recipe-profile-rows")
                .max_h(px(300.))
                .min_h_0()
                .flex()
                .flex_col()
                .gap_2()
                .overflow_y_scroll()
                .track_scroll(&self.recipe_profile_scroll);
            if let Some(picker) = &self.recipe_profiles {
                let profiles = picker.profiles(query);
                if profiles.is_empty() {
                    rows = rows.child(row_note(
                        colors.muted,
                        if query.trim().is_empty() {
                            "Нет сохранённых профилей. Создайте профиль в библиотеке «Профили агентов», затем вернитесь к составу."
                        } else {
                            "Подходящие профили не найдены."
                        },
                    ));
                }
                let selected_id = picker.selected(query).map(|profile| profile.id);
                for profile in profiles.into_iter().take(128) {
                    let selected = selected_id == Some(profile.id);
                    let custom = profile.settings.provider == "custom";
                    let revision = if custom {
                        format!("Ревизия {} · команда", profile.revision)
                    } else {
                        format!("Ревизия {}", profile.revision)
                    };
                    rows = rows.child(self.action_control(
                        format!("recipe-profile-{}", profile.id),
                        format!(
                            "{} · {} · {}",
                            profile.name,
                            provider_label(&profile.settings.provider),
                            revision
                        ),
                        ProductAction::ApplyRecipeProfile(profile.id),
                        ControlStyle {
                            role: Role::Button,
                            selected,
                            compact: false,
                        },
                        cx,
                    ));
                }
            } else {
                rows = rows.child(row_note(
                    colors.muted,
                    "Список сохранённых профилей недоступен.",
                ));
            }
            content = content.child(rows);
        }
        if dialog.kind == DialogKind::RecipeSlot
            && let Some(config) = self
                .recipe_editor
                .as_ref()
                .and_then(|editor| editor.launches.get(editor.slot))
        {
            if self
                .catalog
                .as_ref()
                .is_some_and(|catalog| catalog.agent_profiles.is_some())
            {
                content = content.child(self.action_control(
                    "recipe-open-profiles".into(),
                    "Выбрать сохранённый профиль… · Ctrl+P".into(),
                    ProductAction::OpenRecipeProfiles,
                    ControlStyle {
                        role: Role::Button,
                        selected: false,
                        compact: true,
                    },
                    cx,
                ));
            }
            let custom = config.provider == "custom";
            let mut providers = div().flex().flex_wrap().gap_1();
            for provider in ["shell", "claude", "codex", "opencode"] {
                providers = providers.child(self.action_control(
                    format!("recipe-provider-{provider}"),
                    provider_label(provider).into(),
                    ProductAction::SetTemplateProvider(provider.into()),
                    ControlStyle {
                        role: Role::Button,
                        selected: config.provider == provider,
                        compact: true,
                    },
                    cx,
                ));
            }
            if custom {
                if let Some(command) = &config.custom_command {
                    content = content.child(row_note(
                        chrome::Colors::for_theme(self.appearance.theme).muted,
                        format!("Команда: {}", command.executable),
                    ));
                    let mut args = div()
                        .id("recipe-custom-args")
                        .max_h(px(180.))
                        .flex()
                        .flex_col()
                        .overflow_y_scroll()
                        .track_scroll(&self.recipe_profile_scroll);
                    for (index, arg) in command.args.iter().enumerate() {
                        args = args.child(row_note(
                            chrome::Colors::for_theme(self.appearance.theme).muted,
                            format!("Аргумент {}: {arg:?}", index + 1),
                        ));
                    }
                    content = content.child(args);
                }
                content = content.child(providers).child(row_note(chrome::Colors::for_theme(self.appearance.theme).muted, "Пользовательская команда получает только точные аргументы; контекст не передаётся."));
            } else {
                let mut roles = div().flex().flex_wrap().gap_1();
                for role in ["implementer", "reviewer", "researcher", "tester", "custom"] {
                    roles = roles.child(self.action_control(
                        format!("recipe-role-{role}"),
                        role.into(),
                        ProductAction::SetTemplateRole(role.into()),
                        ControlStyle {
                            role: Role::Button,
                            selected: false,
                            compact: true,
                        },
                        cx,
                    ));
                }
                content=content.child(providers).child(roles)
                .child(self.action_control("recipe-deliver-context".into(),format!("{} Контекст через адаптер CLI",if config.deliver_context {"✓"} else {"○"}),ProductAction::ToggleTemplateContext,ControlStyle{role:Role::Button,selected:config.deliver_context,compact:false},cx))
                .child(row_note(chrome::Colors::for_theme(self.appearance.theme).muted, "Роль — инструкции, а не ограничение прав. Доставка зависит от подтверждённой версии CLI. Память в этот запуск не включается."));
            }
        }
        if dialog.kind == DialogKind::TemplateRun
            && let Some(run) = &self.template_run
        {
            let status = if self.run_unverified {
                "Результат запроса не подтверждён. Обновите состояние перед продолжением."
            } else if self.run_automatic {
                "Добавляем агентов…"
            } else {
                match run.status.as_str() {
                    "complete" => "Вкладка создана",
                    "cancelled" => "Оставшиеся запуски отменены",
                    _ => "Проверьте состав и начните запуск",
                }
            };
            content = content
                .child(
                    div()
                        .text_size(px(14.))
                        .font_weight(FontWeight::MEDIUM)
                        .child(run.name.clone()),
                )
                .child(
                    div()
                        .id("template-run-status")
                        .role(Role::Status)
                        .text_size(px(12.))
                        .text_color(colors.accent)
                        .child(status),
                );
            if let Some(workspace) = self.catalog.as_ref().and_then(|catalog| {
                catalog
                    .workspaces
                    .iter()
                    .find(|workspace| workspace.id == run.workspace_id)
            }) {
                content = content.child(row_note(
                    chrome::Colors::for_theme(self.appearance.theme).muted,
                    format!("Воркспейс: {}", workspace_name(workspace)),
                ));
            }
            if let Some(cwd) = run.slots.first().and_then(|slot| slot.preview.cwd.as_ref()) {
                content = content.child(row_note(
                    chrome::Colors::for_theme(self.appearance.theme).muted,
                    format!("Папка: {}", cwd.display()),
                ));
            }
            for (index, slot) in run.slots.iter().enumerate() {
                let mut card = div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .p(px(9.))
                    .rounded(px(6.))
                    .bg(colors.base)
                    .child(div().text_size(px(12.)).child(format!(
                        "{}. {} · {}",
                        index + 1,
                        provider_label(&slot.preview.provider),
                        phase_label(&slot.phase)
                    )))
                    .child(row_note(
                        chrome::Colors::for_theme(self.appearance.theme).muted,
                        if slot.preview.provider == "custom" {
                            "Запуск по пользовательской команде"
                        } else if !slot.preview.can_launch {
                            slot.preview
                                .unavailable_reason
                                .as_deref()
                                .unwrap_or("Запуск недоступен")
                        } else if !slot.placed
                            && matches!(
                                slot.phase.as_str(),
                                "not_requested" | "delivered_unconfirmed"
                            )
                        {
                            "Процесс уже запускался; требуется восстановить его размещение"
                        } else if slot.preview.deliver_context {
                            "Сохранённая ревизия контекста · доступна для просмотра"
                        } else {
                            "Без передачи контекста"
                        },
                    ));
                if slot.preview.provider == "custom" {
                    if let Some(command) = &slot.preview.custom_command {
                        card = card.child(row_note(
                            chrome::Colors::for_theme(self.appearance.theme).muted,
                            format!("Команда: {}", command.executable),
                        ));
                        for (arg_index, arg) in command.args.iter().enumerate() {
                            card = card.child(row_note(
                                chrome::Colors::for_theme(self.appearance.theme).muted,
                                format!("Аргумент {}: {arg:?}", arg_index + 1),
                            ));
                        }
                    }
                } else {
                    card = card.child(row_note(
                        chrome::Colors::for_theme(self.appearance.theme).muted,
                        format!(
                            "Модель: {} · reasoning: {}",
                            slot.preview
                                .engine
                                .model
                                .as_deref()
                                .unwrap_or("настройки CLI"),
                            slot.preview
                                .engine
                                .reasoning_effort
                                .as_deref()
                                .unwrap_or("настройки CLI")
                        ),
                    ));
                }
                content = content.child(card);
            }
            if let Some(error) = &run.error {
                content = content.child(row_note(
                    chrome::Colors::for_theme(self.appearance.theme).muted,
                    error,
                ));
            }
            if self.run_busy {
                content = content.child(row_note(
                    chrome::Colors::for_theme(self.appearance.theme).muted,
                    "Подтверждаем состояние. Запрос запуска автоматически не повторяется.",
                ));
            }
            for (index, (label, action)) in self.recipe_menu_entries().into_iter().enumerate() {
                content = content.child(self.action_control(
                    format!("template-run-action-{index}"),
                    label,
                    action,
                    ControlStyle {
                        role: Role::Button,
                        selected: dialog.active_field == index,
                        compact: false,
                    },
                    cx,
                ));
            }
            content=content.child(row_note(chrome::Colors::for_theme(self.appearance.theme).muted, "Закрытие приостанавливает добавление остальных агентов. Уже запущенные CLI продолжают работу."));
        }
        if dialog.kind == DialogKind::TemplateRunContext
            && let Some(slot) = self
                .template_run
                .as_ref()
                .and_then(|run| run.slots.get(self.run_context_slot))
        {
            if slot.preview.provider == "custom" {
                let command = slot.preview.custom_command.as_ref();
                if let Some(cwd) = &slot.preview.cwd {
                    content = content.child(row_note(
                        chrome::Colors::for_theme(self.appearance.theme).muted,
                        format!("Папка: {}", cwd.display()),
                    ));
                }
                content = content.child(row_note(
                    chrome::Colors::for_theme(self.appearance.theme).muted,
                    format!(
                        "{} · {}",
                        provider_label("custom"),
                        command
                            .map(|value| value.executable.as_str())
                            .unwrap_or("Команда недоступна")
                    ),
                ));
                if let Some(command) = command {
                    for (index, arg) in command.args.iter().enumerate() {
                        content = content.child(row_note(
                            chrome::Colors::for_theme(self.appearance.theme).muted,
                            format!("Аргумент {}: {arg:?}", index + 1),
                        ));
                    }
                }
                content = content.child(row_note(chrome::Colors::for_theme(self.appearance.theme).muted, "Рабочая папка и точные аргументы заморожены при проверке запуска. Контекстные блоки не передаются."));
            } else {
                content=content.child(row_note(chrome::Colors::for_theme(self.appearance.theme).muted, format!("{} · CLI {} · {}",provider_label(&slot.preview.provider),slot.preview.cli_version,slot.preview.delivery_method)))
                .child(row_note(chrome::Colors::for_theme(self.appearance.theme).muted, format!("Ревизия {} · {}",slot.preview.context.id,if slot.preview.deliver_context {"Передача запрошена; применение моделью не подтверждено"} else {"Контекст не передаётся"})))
                .child(effective_context_blocks(&slot.preview.context,"template-run-context-blocks","Контекст нового запуска",280.,self.catalog.as_ref(),self.colors()))
                .child(row_note(chrome::Colors::for_theme(self.appearance.theme).muted, "Пользовательские конфиги и credentials не изменяются. Нативные запросы разрешений остаются в CLI."));
            }
        }
        if dialog.kind == DialogKind::TemplateRuns {
            content=content.child(row_note(chrome::Colors::for_theme(self.appearance.theme).muted, "Ничего не возобновляется автоматически. Откройте запись, чтобы проверить сохранённое состояние."));
            for (index, (label, action)) in self.recipe_menu_entries().into_iter().enumerate() {
                content = content.child(self.action_control(
                    format!("template-run-history-{index}"),
                    label,
                    action,
                    ControlStyle {
                        role: Role::Button,
                        selected: dialog.active_field == index,
                        compact: false,
                    },
                    cx,
                ));
            }
            if self
                .template_runs
                .as_ref()
                .is_some_and(|page| page.entries.is_empty())
            {
                content = content.child(row_note(
                    chrome::Colors::for_theme(self.appearance.theme).muted,
                    "Незавершённых вкладок нет.",
                ));
            }
        }
        content
    }
}

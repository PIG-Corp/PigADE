//! Rendering for the reusable agent profile library and editor.
use super::*;
use crate::profile_roster;

fn profile_provider_label(provider: &str) -> &str {
    match provider {
        "codex" => "Codex",
        "claude" => "Claude Code",
        "opencode" => "OpenCode",
        "custom" => "Пользовательский CLI",
        other => other,
    }
}

fn profile_note(muted: u32, text: impl Into<String>) -> Div {
    div()
        .text_size(px(11.))
        .text_color(rgb(muted))
        .whitespace_normal()
        .child(text.into())
}

impl ProductUi {
    /// Actions in the same order as the library's keyboard navigation.
    pub(super) fn profile_menu_entries(&self) -> Vec<(String, ProductAction)> {
        match self.dialog.as_ref().map(|dialog| dialog.kind) {
            Some(DialogKind::AgentProfiles) => {
                let mut entries = vec![("Новый профиль…".into(), ProductAction::NewAgentProfile)];
                if let Some(catalog) = &self.catalog
                    && let Some(profiles) = &catalog.agent_profiles
                {
                    let ids: Vec<_> = profiles.iter().map(|profile| profile.id).collect();
                    let order =
                        profile_roster::display_order(&ids, catalog.agent_profile_roster.as_ref());
                    for id in order.visible_ids() {
                        let Some(profile) = profiles.iter().find(|profile| profile.id == id) else {
                            continue;
                        };
                        entries.push((
                            format!("Запустить · {}", profile.name),
                            ProductAction::LaunchAgentProfile(profile.id),
                        ));
                    }
                }
                entries
            }
            _ => Vec::new(),
        }
    }

    pub(super) fn profile_dialog_fields(
        &self,
        dialog: &Dialog,
        mut content: Div,
        cx: &mut Context<CanvasApp>,
    ) -> Div {
        let colors = self.colors();
        match dialog.kind {
            DialogKind::AgentProfiles => {
                let Some(profiles) = self
                    .catalog
                    .as_ref()
                    .and_then(|catalog| catalog.agent_profiles.as_ref())
                else {
                    return content.child(
                        div()
                            .id("agent-profiles-unsupported")
                            .role(Role::Status)
                            .text_size(px(12.))
                            .text_color(colors.muted)
                            .child("Этот session host не поддерживает профили агентов."),
                    );
                };

                content = content.child(profile_note(chrome::Colors::for_theme(self.appearance.theme).muted,
                    "Профили хранят параметры будущих запусков. Уже работающие CLI сохраняют свои настройки.",
                ));
                let mut rows = div()
                    .id("agent-profiles-rows")
                    .max_h(px(360.))
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .overflow_y_scroll()
                    .track_scroll(&self.layout_scroll)
                    .child(self.action_control(
                        "agent-profile-new".into(),
                        "Создать профиль…".into(),
                        ProductAction::NewAgentProfile,
                        ControlStyle {
                            role: Role::Button,
                            selected: dialog.active_field == 0,
                            compact: false,
                        },
                        cx,
                    ));

                if profiles.is_empty() {
                    rows = rows.child(
                        div()
                            .py(px(12.))
                            .text_size(px(12.))
                            .text_color(colors.muted)
                            .child("Сохранённых профилей пока нет."),
                    );
                }

                let profile_ids: Vec<_> = profiles.iter().map(|profile| profile.id).collect();
                let roster = self
                    .catalog
                    .as_ref()
                    .and_then(|catalog| catalog.agent_profile_roster.as_ref());
                let order = profile_roster::display_order(&profile_ids, roster);
                if let Some(warning) = order.warning {
                    content = content.child(
                        div()
                            .id("agent-profile-roster-warning")
                            .role(Role::Status)
                            .text_size(px(11.))
                            .text_color(colors.status_amber)
                            .whitespace_normal()
                            .child(warning),
                    );
                } else if roster.is_none() {
                    content = content.child(profile_note(
                        chrome::Colors::for_theme(self.appearance.theme).muted,
                        "Этот host показывает исходный порядок; закрепление и перестановка недоступны.",
                    ));
                }
                if let Some(error) = &self.profile_roster_error {
                    content = content.child(
                        div()
                            .id("agent-profile-roster-error")
                            .role(Role::Alert)
                            .text_size(px(11.))
                            .text_color(colors.status_amber)
                            .whitespace_normal()
                            .child(error.clone()),
                    );
                }

                for (index, id) in order.visible_ids().iter().enumerate() {
                    let Some(profile) = profiles.iter().find(|profile| profile.id == *id) else {
                        continue;
                    };
                    let id = profile.id;
                    let provider = profile_provider_label(&profile.settings.provider);
                    let provider_state = match self.providers.as_ref() {
                        None => "Проверка CLI ещё не завершена".to_owned(),
                        Some(providers) => providers
                            .iter()
                            .find(|info| info.provider == profile.settings.provider)
                            .map(|info| {
                                if info.available {
                                    "CLI доступен".to_owned()
                                } else {
                                    info.reason
                                        .as_deref()
                                        .map(|reason| format!("CLI недоступен · {reason}"))
                                        .unwrap_or_else(|| "CLI недоступен".into())
                                }
                            })
                            .unwrap_or_else(|| {
                                if profile.settings.provider == "custom" {
                                    "Команда будет проверена при подготовке запуска".into()
                                } else {
                                    "Нет сведений о доступности CLI".into()
                                }
                            }),
                    };
                    let detail = if profile.settings.deliver_context {
                        "Контекст запрошен · совместимость проверяется при запуске"
                    } else {
                        "Без передачи контекста"
                    };
                    let engine_summary = match (
                        profile.settings.engine.model.as_deref(),
                        profile.settings.engine.reasoning_effort.as_deref(),
                    ) {
                        (Some(model), Some(effort)) => Some(format!(
                            "CLI параметры · модель: {model} · усилие: {effort}"
                        )),
                        (Some(model), None) => Some(format!("CLI параметр · модель: {model}")),
                        (None, Some(effort)) => Some(format!("CLI параметр · усилие: {effort}")),
                        (None, None) => None,
                    };
                    let selected = dialog.active_field == index + 1;
                    let pinned = order.pinned_ids.contains(&id);
                    let partition: Vec<_> = order
                        .ordered_ids
                        .iter()
                        .copied()
                        .filter(|value| order.pinned_ids.contains(value) == pinned)
                        .collect();
                    let can_move_up =
                        order.can_mutate && partition.first().is_some_and(|first| *first != id);
                    let can_move_down =
                        order.can_mutate && partition.last().is_some_and(|last| *last != id);
                    rows = rows.child(
                        div()
                            .id(format!("agent-profile-row-{id}"))
                            .flex_shrink_0()
                            .flex()
                            .items_center()
                            .gap_2()
                            .p(px(8.))
                            .rounded(px(8.))
                            .border_1()
                            .border_color(if selected {
                                colors.accent
                            } else {
                                colors.border
                            })
                            .bg(if selected {
                                colors.selected
                            } else {
                                colors.base
                            })
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .flex()
                                    .flex_col()
                                    .gap_1()
                                    .child(
                                        div()
                                            .text_size(px(13.))
                                            .font_weight(FontWeight::MEDIUM)
                                            .truncate()
                                            .child(profile.name.clone()),
                                    )
                                    .when(pinned, |view| {
                                        view.child(profile_note(
                                            chrome::Colors::for_theme(self.appearance.theme).muted,
                                            "Закреплён",
                                        ))
                                    })
                                    .child(profile_note(
                                        chrome::Colors::for_theme(self.appearance.theme).muted,
                                        format!("{provider} · {provider_state}"),
                                    ))
                                    .child(profile_note(
                                        chrome::Colors::for_theme(self.appearance.theme).muted,
                                        detail,
                                    ))
                                    .children(engine_summary.map(|summary| {
                                        profile_note(
                                            chrome::Colors::for_theme(self.appearance.theme).muted,
                                            summary,
                                        )
                                    })),
                            )
                            .when(order.can_mutate, |row| {
                                row.child(
                                    self.action_control(
                                        format!("agent-profile-pin-{id}"),
                                        if pinned {
                                            "Открепить"
                                        } else {
                                            "Закрепить"
                                        }
                                        .into(),
                                        ProductAction::SetAgentProfilePinned(id, !pinned),
                                        ControlStyle {
                                            role: Role::Button,
                                            selected: pinned,
                                            compact: true,
                                        },
                                        cx,
                                    ),
                                )
                                .when(can_move_up, |row| {
                                    row.child(self.action_control_with_accessible_name(
                                        format!("agent-profile-up-{id}"),
                                        "↑".into(),
                                        format!("Переместить профиль {} выше", profile.name),
                                        ProductAction::MoveAgentProfile(id, -1),
                                        ControlStyle {
                                            role: Role::Button,
                                            selected: false,
                                            compact: true,
                                        },
                                        cx,
                                    ))
                                })
                                .when(can_move_down, |row| {
                                    row.child(self.action_control_with_accessible_name(
                                        format!("agent-profile-down-{id}"),
                                        "↓".into(),
                                        format!("Переместить профиль {} ниже", profile.name),
                                        ProductAction::MoveAgentProfile(id, 1),
                                        ControlStyle {
                                            role: Role::Button,
                                            selected: false,
                                            compact: true,
                                        },
                                        cx,
                                    ))
                                })
                            })
                            .child(self.icon_control(
                                &format!("agent-profile-launch-{id}"),
                                &format!("Запустить профиль {}", profile.name),
                                "terminal",
                                ProductAction::LaunchAgentProfile(id),
                                cx,
                            ))
                            .child(self.icon_control(
                                &format!("agent-profile-edit-{id}"),
                                &format!("Изменить профиль {}", profile.name),
                                "more",
                                ProductAction::EditAgentProfile(id),
                                cx,
                            ))
                            .child(self.icon_control(
                                &format!("agent-profile-duplicate-{id}"),
                                &format!("Создать копию профиля {}", profile.name),
                                "layers",
                                ProductAction::DuplicateAgentProfile(id),
                                cx,
                            ))
                            .child(self.icon_control(
                                &format!("agent-profile-delete-{id}"),
                                &format!("Удалить профиль {}", profile.name),
                                "close",
                                ProductAction::DeleteAgentProfile(id),
                                cx,
                            )),
                    );
                }
                content = content.child(rows);
                if self
                    .catalog
                    .as_ref()
                    .is_some_and(|catalog| catalog.custom_cli_schema == Some(1))
                {
                    content = content.child(self.action_control(
                        "agent-profile-custom-new".into(),
                        "Добавить пользовательский CLI…".into(),
                        ProductAction::NewCustomProfile,
                        ControlStyle {
                            role: Role::Button,
                            selected: false,
                            compact: true,
                        },
                        cx,
                    ));
                }
                content.child(self.action_control("profiles-refresh".into(),"Обновить библиотеку".into(),ProductAction::ReloadAgentProfiles,ControlStyle{role:Role::Button,selected:false,compact:true},cx)).child(profile_note(chrome::Colors::for_theme(self.appearance.theme).muted,
                    "↑ ↓ — выбрать · Enter — запуск · Ctrl+E — изменить · Ctrl+D — копировать · Delete — удалить · Ctrl+P — закрепить · Alt+↑/↓ — переместить · Ctrl+R — обновить",
                ))
            }
            DialogKind::AgentProfile => {
                let Some(editor) = &self.profile_editor else {
                    return content.child(profile_note(
                        chrome::Colors::for_theme(self.appearance.theme).muted,
                        "Редактор профиля недоступен.",
                    ));
                };
                if !self.engine_options_supported()
                    && dialog.fields.get(3..5).is_some_and(|fields| {
                        fields.iter().any(|field| !field.text().trim().is_empty())
                    })
                {
                    content = content.child(
                        div()
                            .id("profile-engine-unsupported")
                            .role(Role::Alert)
                            .text_size(px(12.))
                            .text_color(colors.status_amber)
                            .whitespace_normal()
                            .child("Этот session host не поддерживает параметры модели и усилия. Очистите эти поля или обновите host; сохранить их нельзя."),
                    );
                }
                let mut providers = div().flex().flex_wrap().gap_1();
                let custom_supported = self
                    .catalog
                    .as_ref()
                    .is_some_and(|catalog| catalog.custom_cli_schema == Some(1));
                let provider_choices: &[&str] = if custom_supported {
                    &["claude", "codex", "opencode", "custom"]
                } else {
                    &["claude", "codex", "opencode"]
                };
                for &provider in provider_choices {
                    providers = providers.child(self.action_control(
                        format!("agent-profile-provider-{provider}"),
                        profile_provider_label(provider).into(),
                        ProductAction::SetProfileProvider(provider.into()),
                        ControlStyle {
                            role: Role::Button,
                            selected: editor.settings.provider == provider,
                            compact: true,
                        },
                        cx,
                    ));
                }
                let readiness = if editor.settings.provider == "custom" {
                    "Команда запускается только после отдельной подготовки профиля".to_owned()
                } else {
                    match self.providers.as_ref() {
                        None => "Проверка доступности CLI ещё не завершена".to_owned(),
                        Some(providers) => providers
                            .iter()
                            .find(|info| info.provider == editor.settings.provider)
                            .map(|info| {
                                if info.available {
                                    "Выбранный CLI доступен".to_owned()
                                } else {
                                    info.reason
                                        .as_deref()
                                        .map(|reason| {
                                            format!("Выбранный CLI недоступен · {reason}")
                                        })
                                        .unwrap_or_else(|| "Выбранный CLI недоступен".into())
                                }
                            })
                            .unwrap_or_else(|| "Нет сведений о доступности выбранного CLI".into()),
                    }
                };
                if editor.settings.provider == "custom" {
                    return content
                        .child(profile_note(
                            chrome::Colors::for_theme(self.appearance.theme).muted,
                            "Пользовательский CLI запускается с указанным абсолютным путём. Аргументы передаются отдельно, без shell-интерпретации; двойные кавычки используют JSON-экранирование.",
                        ))
                        .child(profile_note(
                            chrome::Colors::for_theme(self.appearance.theme).muted,
                            readiness,
                        ))
                        .child(providers)
                        .child(profile_note(
                            chrome::Colors::for_theme(self.appearance.theme).muted,
                            "Ctrl+4 — пользовательский CLI · Ctrl+Enter — сохранить. Контекст и параметры встроенных CLI не передаются.",
                        ));
                }
                content = content
                    .child(profile_note(
                        chrome::Colors::for_theme(self.appearance.theme).muted,
                        "Роль задаёт инструкции агенту, а не его технические права.",
                    ))
                    .child(profile_note(
                        chrome::Colors::for_theme(self.appearance.theme).muted,
                        "Профиль хранит только явно заданные модель и усилие CLI; пустые поля используют настройки CLI по умолчанию. Авторизация, рабочая папка и разрешения сюда не входят.",
                    ))
                    .child(profile_note(
                        chrome::Colors::for_theme(self.appearance.theme).muted,
                        "Совместимость параметров проверяется при подготовке. Профиль не подтверждает доступность модели или её применение.",
                    ))
                    .child(profile_note(
                        chrome::Colors::for_theme(self.appearance.theme).muted,
                        readiness,
                    ))
                    .child(providers)
                    .child(self.action_control(
                        "agent-profile-context-delivery".into(),
                        format!(
                            "{} Передавать контекст через адаптер CLI",
                            if editor.settings.deliver_context {
                                "✓"
                            } else {
                                "○"
                            }
                        ),
                        ProductAction::ToggleProfileContext,
                        ControlStyle {
                            role: Role::Button,
                            selected: editor.settings.deliver_context,
                            compact: false,
                        },
                        cx,
                    ));
                if editor.settings.provider == "opencode" && editor.settings.deliver_context {
                    content = content.child(profile_note(chrome::Colors::for_theme(self.appearance.theme).muted,
                        "Для OpenCode передача контекста недоступна. Профиль можно сохранить; запуск будет отдельно проверен.",
                    ));
                }
                content.child(profile_note(chrome::Colors::for_theme(self.appearance.theme).muted,
                    "Ctrl+1/2/3 — CLI · Ctrl+M — контекст · Ctrl+Enter — сохранить. Роль не ограничивает права CLI. Проверка версии и снимка контекста выполняется перед запуском.",
                ))
            }
            DialogKind::DeleteAgentProfile => {
                if let Some(profile) = &self.profile_delete {
                    content
                        .child(div().text_size(px(13.)).child(profile.name.clone()))
                        .child(profile_note(chrome::Colors::for_theme(self.appearance.theme).muted, format!(
                            "Будет удалена только сохранённая настройка {}. Уже подготовленные снимки запуска и работающие CLI сохранятся.",
                            profile_provider_label(&profile.settings.provider)
                        )))
                } else {
                    content.child(profile_note(
                        chrome::Colors::for_theme(self.appearance.theme).muted,
                        "Профиль больше недоступен.",
                    ))
                }
            }
            _ => content,
        }
    }
}

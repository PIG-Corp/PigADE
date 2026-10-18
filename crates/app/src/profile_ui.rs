//! Rendering for the reusable agent profile library and editor.
use super::*;

fn profile_provider_label(provider: &str) -> &str {
    match provider {
        "codex" => "Codex",
        "claude" => "Claude Code",
        "opencode" => "OpenCode",
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
                if let Some(profiles) = self
                    .catalog
                    .as_ref()
                    .and_then(|catalog| catalog.agent_profiles.as_ref())
                {
                    for profile in profiles {
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

                for (index, profile) in profiles.iter().enumerate() {
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
                            .unwrap_or_else(|| "Нет сведений о доступности CLI".into()),
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
                                &format!("agent-profile-delete-{id}"),
                                &format!("Удалить профиль {}", profile.name),
                                "close",
                                ProductAction::DeleteAgentProfile(id),
                                cx,
                            )),
                    );
                }
                content.child(rows).child(self.action_control("profiles-refresh".into(),"Обновить библиотеку".into(),ProductAction::ReloadAgentProfiles,ControlStyle{role:Role::Button,selected:false,compact:true},cx)).child(profile_note(chrome::Colors::for_theme(self.appearance.theme).muted,
                    "↑ ↓ — выбрать · Enter — запуск · Ctrl+E — изменить · Delete — удалить · Ctrl+R — обновить",
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
                for provider in ["claude", "codex", "opencode"] {
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
                let readiness = match self.providers.as_ref() {
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
                                    .map(|reason| format!("Выбранный CLI недоступен · {reason}"))
                                    .unwrap_or_else(|| "Выбранный CLI недоступен".into())
                            }
                        })
                        .unwrap_or_else(|| "Нет сведений о доступности выбранного CLI".into()),
                };
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

//! User-reviewed UI for handing visible terminal material to a new agent.
use super::*;

fn handoff_provider_label(provider: &str) -> &'static str {
    match provider {
        "claude" => "Claude",
        "codex" => "Codex",
        "opencode" => "OpenCode",
        _ => "Агент",
    }
}

impl ProductUi {
    pub(super) fn handoff_overlay(
        &self,
        dialog: &Dialog,
        layouts: &BTreeMap<usize, FieldLayout>,
        cx: &mut Context<CanvasApp>,
    ) -> AnyElement {
        let colors = self.colors();
        let Some(sheet) = self.handoff_sheet.as_ref() else {
            return div()
                .id("handoff-overlay-unavailable")
                .role(Role::Dialog)
                .aria_label("Передать контекст")
                .child("Источник передачи недоступен. Закройте окно и выберите панель снова.")
                .into_any_element();
        };

        let generation = sheet.source.generation.to_string();
        let generation_short: String = generation.chars().take(8).collect();
        let source = div()
            .id("handoff-source")
            .role(Role::Group)
            .aria_label("Источник передачи")
            .flex()
            .flex_col()
            .gap_1()
            .rounded(px(8.))
            .border_1()
            .border_color(colors.border)
            .bg(colors.base)
            .p(px(10.))
            .child(
                div()
                    .text_size(px(13.))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(colors.text)
                    .whitespace_normal()
                    .child(sheet.source.title.clone()),
            )
            .child(
                div()
                    .text_size(px(11.))
                    .text_color(colors.muted)
                    .whitespace_normal()
                    .child(format!(
                        "{} · поколение {} · последовательность {}",
                        sheet.source.scope, generation_short, sheet.source.sequence
                    )),
            );

        let mut provider_controls = div()
            .id("handoff-providers")
            .role(Role::Group)
            .aria_label("Целевой CLI")
            .flex()
            .flex_wrap()
            .gap_2();
        for (provider, label, shortcut) in [
            ("claude", "Claude", "Ctrl+1"),
            ("codex", "Codex", "Ctrl+2"),
            ("opencode", "OpenCode", "Ctrl+3"),
        ] {
            provider_controls = provider_controls.child(self.action_control(
                format!("handoff-provider-{provider}"),
                format!("{label} · {shortcut}"),
                ProductAction::SetHandoffProvider(provider.into()),
                ControlStyle {
                    role: Role::Button,
                    selected: sheet.provider == provider,
                    compact: true,
                },
                cx,
            ));
        }

        let mut fields = div().id("handoff-edit-fields").flex().flex_col().gap_2();
        for (index, label, height) in [
            (0, "Видимый текст терминала · можно отредактировать", 112.),
            (1, "Ваш запрос новому агенту", 76.),
        ] {
            let Some(buffer) = dialog.fields.get(index) else {
                continue;
            };
            let active = dialog.active_field == index;
            fields = fields.child(
                div()
                    .id(format!("handoff-field-{index}"))
                    .role(Role::TextInput)
                    .aria_label(label)
                    .w_full()
                    .h(px(height))
                    .px(px(10.))
                    .py(px(6.))
                    .rounded(px(7.))
                    .border_1()
                    .border_color(if active { colors.accent } else { colors.border })
                    .bg(colors.base)
                    .text_color(colors.text)
                    .overflow_hidden()
                    .child(crate::edit_field::content(
                        buffer,
                        crate::edit_field::FieldOptions {
                            cache: layouts.get(&index),
                            dialog_id: dialog.id,
                            index,
                            active,
                            multiline: true,
                            placeholder: label,
                            accent: colors.accent,
                            muted: colors.muted,
                        },
                        cx,
                    ))
                    .on_mouse_down(
                        gpui::MouseButton::Left,
                        cx.listener(move |this, event, window, cx| {
                            this.editor_pointer_down(index, event, window, cx);
                        }),
                    )
                    .on_scroll_wheel(cx.listener(move |this, event, window, cx| {
                        this.editor_wheel(index, event, window, cx);
                    })),
            );
        }

        let prepared_for_current_fields = sheet.prepared.as_ref().filter(|prepared| {
            prepared.provider == sheet.provider
                && prepared.fields.len() == dialog.fields.len()
                && prepared
                    .fields
                    .iter()
                    .zip(&dialog.fields)
                    .all(|(prepared, current)| prepared == current.text())
        });
        // Reuse the frozen prompt for an unchanged provider and exact field set.
        // Format again only after the user edits a field or changes providers.
        let live_prompt = prepared_for_current_fields
            .is_none()
            .then(|| self.handoff_prompt());
        let prompt_matches_prepared = prepared_for_current_fields.is_some();
        let preview_text = live_prompt
            .as_ref()
            .and_then(|result| result.as_ref().ok())
            .map(|prompt| prompt.text.as_str())
            .or_else(|| {
                sheet
                    .prepared
                    .as_ref()
                    .map(|prepared| prepared.prompt.text.as_str())
            });
        let mut review = div().id("handoff-review").flex().flex_col().gap_2();
        if sheet.pending {
            review = review.child(
                div()
                    .id("handoff-pending")
                    .role(Role::Status)
                    .text_size(px(12.))
                    .text_color(colors.accent)
                    .child(if sheet.prepared.is_some() {
                        "Запускаем нового агента…"
                    } else {
                        "Проверяем CLI и готовим контекст…"
                    }),
            );
        }
        if let Some(prompt) = prepared_for_current_fields
            .map(|prepared| &prepared.prompt)
            .or_else(|| live_prompt.as_ref().and_then(|result| result.as_ref().ok()))
        {
            review = review.child(
                div()
                    .id("handoff-preview-count")
                    .role(Role::Status)
                    .text_size(px(11.))
                    .text_color(colors.muted)
                    .child(format!(
                        "{} байт UTF-8 в передаче{}",
                        prompt.utf8_bytes,
                        if prompt.truncated {
                            " · есть явная отметка об обрезке"
                        } else {
                            ""
                        }
                    )),
            );
        } else if let Some(Err(error)) = live_prompt.as_ref() {
            review = review.child(
                div()
                    .id("handoff-prompt-invalid")
                    .role(Role::Alert)
                    .text_size(px(11.))
                    .text_color(colors.status_amber)
                    .whitespace_normal()
                    .child(error.clone()),
            );
        }
        if sheet.prepared.is_some() && !prompt_matches_prepared {
            review = review.child(
                div()
                    .id("handoff-reprepare-needed")
                    .role(Role::Status)
                    .text_size(px(11.))
                    .text_color(colors.status_amber)
                    .whitespace_normal()
                    .child("Поля изменились после проверки. Проверьте передачу ещё раз перед запуском."),
            );
        }
        if let Some(text) = preview_text.filter(|_| !prompt_matches_prepared) {
            review = review.child(
                div()
                    .id("handoff-preview")
                    .role(Role::Group)
                    .aria_label("Полный текст передачи, прокручиваемый предварительный просмотр")
                    .max_h(px(188.))
                    .min_h(px(68.))
                    .overflow_y_scroll()
                    .rounded(px(7.))
                    .border_1()
                    .border_color(colors.border)
                    .bg(colors.base)
                    .p(px(10.))
                    .text_size(px(11.))
                    .text_color(colors.text)
                    .whitespace_normal()
                    .child(text.to_owned()),
            );
        }

        if let Some(error) = &sheet.error {
            review = review.child(
                div()
                    .id("handoff-error")
                    .role(Role::Alert)
                    .text_size(px(12.))
                    .text_color(colors.status_amber)
                    .whitespace_normal()
                    .child(error.clone()),
            );
        }

        if let Some(prepared) = &sheet.prepared {
            let preview = &prepared.preview;
            let mut prepared_details = div()
                .id("handoff-prepared-details")
                .role(Role::Group)
                .aria_label(if prompt_matches_prepared {
                    "Сведения проверенного запуска"
                } else {
                    "Сведения прошлой проверки запуска"
                })
                .flex()
                .flex_col()
                .gap_1()
                .rounded(px(7.))
                .border_1()
                .border_color(colors.border)
                .bg(colors.base)
                .p(px(10.))
                .text_size(px(11.))
                .text_color(colors.muted)
                .child(if prompt_matches_prepared {
                    "Проверенные параметры запуска"
                } else {
                    "Прошлая проверка: поля или выбранный CLI изменились; запуск скрыт"
                })
                .child(format!(
                    "Исполняемый файл: {}",
                    preview
                        .executable
                        .as_ref()
                        .map(|path| path.display().to_string())
                        .unwrap_or_else(|| "Недоступен".into())
                ))
                .child(format!(
                    "Рабочая папка: {}",
                    preview
                        .cwd
                        .as_ref()
                        .map(|path| path.display().to_string())
                        .unwrap_or_else(|| "Недоступна".into())
                ))
                .child(format!(
                    "{} · CLI {} · способ доставки: {}",
                    handoff_provider_label(&prepared.provider),
                    preview.cli_version,
                    preview.delivery_method
                ))
                .child(format!(
                    "Эффективный контекст · сборка {} · {}",
                    preview.context.id, preview.context.content_hash
                ))
                .child("Применение параметров CLI после запуска не подтверждено.")
                .child("Запрошенный контекст воркспейса также добавит host PigADE.");
            if let Some(reason) = &preview.unavailable_reason {
                prepared_details = prepared_details.child(
                    div()
                        .id("handoff-unavailable-reason")
                        .role(Role::Alert)
                        .text_color(colors.status_amber)
                        .whitespace_normal()
                        .child(reason.clone()),
                );
            }
            prepared_details = prepared_details.child(effective_context_blocks(
                &preview.context,
                "handoff-effective-context",
                "Блоки эффективного контекста",
                180.,
                self.catalog.as_ref(),
                colors,
            ));
            review = review.child(prepared_details);
        }

        let mut footer = div()
            .id("handoff-footer")
            .flex()
            .flex_wrap()
            .items_center()
            .justify_between()
            .gap_2()
            .mt(px(4.));
        footer = footer.child(
            div()
                .text_size(px(10.))
                .text_color(colors.muted)
                .whitespace_normal()
                .child("Передаются только видимые данные выбранной панели. Проверьте текст и удалите лишнее перед отправкой."),
        );
        let mut footer_actions =
            div()
                .flex()
                .flex_wrap()
                .justify_end()
                .gap_2()
                .child(self.action_control(
                    "handoff-cancel".into(),
                    "Отмена".into(),
                    ProductAction::CloseDialog,
                    ControlStyle {
                        role: Role::Button,
                        selected: false,
                        compact: true,
                    },
                    cx,
                ));
        footer_actions = footer_actions.child(self.action_control(
            "handoff-copy".into(),
            "Копировать".into(),
            ProductAction::CopyHandoff,
            ControlStyle {
                role: Role::Button,
                selected: false,
                compact: true,
            },
            cx,
        ));
        if matches!(sheet.provider.as_str(), "codex" | "claude") {
            let can_prepare = !self.busy
                && !sheet.pending
                && !self.handoff_ready()
                && (prepared_for_current_fields.is_some()
                    || live_prompt.as_ref().is_some_and(|result| {
                        result
                            .as_ref()
                            .is_ok_and(|prompt| !prompt.text.trim().is_empty())
                    }));
            if can_prepare {
                footer_actions = footer_actions.child(self.action_control(
                    "handoff-prepare".into(),
                    "Проверить запуск".into(),
                    ProductAction::PrepareHandoff,
                    ControlStyle {
                        role: Role::Button,
                        selected: true,
                        compact: true,
                    },
                    cx,
                ));
            }
            if prompt_matches_prepared && self.handoff_ready() {
                footer_actions = footer_actions.child(self.action_control(
                    "handoff-confirm".into(),
                    "Запустить агента · Ctrl+Enter".into(),
                    ProductAction::ConfirmHandoff,
                    ControlStyle {
                        role: Role::Button,
                        selected: true,
                        compact: true,
                    },
                    cx,
                ));
            }
        } else {
            footer_actions = footer_actions.child(
                div()
                    .id("handoff-footer-notice")
                    .role(Role::Status)
                    .text_size(px(10.))
                    .text_color(colors.muted)
                    .child("OpenCode: скопируйте текст и передайте его вручную. Автоматическая доставка недоступна."),
            );
        }
        footer = footer.child(footer_actions);

        div()
            .id("handoff-overlay")
            .accessibility_id("handoff-overlay")
            .absolute()
            .inset_0()
            .flex()
            .items_start()
            .justify_center()
            .overflow_y_scroll()
            .py(px(18.))
            .bg(rgb(0x090b0e).opacity(0.72))
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                this.product_action(ProductAction::CloseDialog, window, cx);
            }))
            .child(
                div()
                    .id("handoff-sheet")
                    .role(Role::Dialog)
                    .aria_label("Передать контекст")
                    .w_full()
                    .max_w(px(760.))
                    .mx(px(16.))
                    .flex()
                    .flex_col()
                    .gap_3()
                    .rounded(px(12.))
                    .border_1()
                    .border_color(colors.border)
                    .bg(colors.raised)
                    .p(px(16.))
                    .text_color(colors.text)
                    .on_click(cx.listener(|_, _: &ClickEvent, _, cx| cx.stop_propagation()))
                    .child(
                        div()
                            .id("handoff-title")
                            .role(Role::Heading)
                            .aria_level(2)
                            .text_size(px(18.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Передать контекст"),
                    )
                    .child(
                        div()
                            .id("handoff-content")
                            .max_h(px(530.))
                            .min_h_0()
                            .overflow_y_scroll()
                            .flex()
                            .flex_col()
                            .gap_3()
                            .child(source)
                            .child(provider_controls)
                            .child(
                                div()
                                    .text_size(px(10.))
                                    .text_color(colors.muted)
                                    .whitespace_normal()
                                    .child("Новый процесс получит отдельный диалог. Переписки, скрытые поля и авторизация провайдеров не импортируются. Выбранный фрагмент остаётся виден перед передачей."),
                            )
                            .child(fields)
                            .child(review),
                    )
                    .child(footer),
            )
            .into_any_element()
    }
}

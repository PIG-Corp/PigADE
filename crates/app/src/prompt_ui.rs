//! Rendering for the reusable prompt library and editor.
use super::*;

fn prompt_note(muted: u32, text: impl Into<String>) -> Div {
    div()
        .text_size(px(11.))
        .text_color(rgb(muted))
        .whitespace_normal()
        .child(text.into())
}

impl ProductUi {
    /// Actions in the same order as the library's keyboard navigation.
    pub(super) fn prompt_menu_entries(&self) -> Vec<(String, ProductAction)> {
        if self.dialog.as_ref().map(|dialog| dialog.kind) != Some(DialogKind::Prompts) {
            return Vec::new();
        }

        let mut entries = vec![("Новый промпт…".into(), ProductAction::NewPrompt)];
        entries.extend(self.prompt_library.rows.iter().map(|row| {
            (
                format!("Выбрать · {} · ревизия {}", row.title, row.prompt.revision),
                ProductAction::SelectPrompt(row.prompt),
            )
        }));
        entries
    }

    pub(super) fn prompt_dialog_fields(
        &self,
        dialog: &Dialog,
        mut content: Div,
        cx: &mut Context<CanvasApp>,
    ) -> Div {
        let colors = self.colors();
        let note_color = chrome::Colors::for_theme(self.appearance.theme).muted;
        match dialog.kind {
            DialogKind::Prompts => {
                let Some(prompts) = self
                    .catalog
                    .as_ref()
                    .and_then(|catalog| catalog.prompts.as_ref())
                else {
                    return content.child(
                        div()
                            .id("prompts-unsupported")
                            .role(Role::Status)
                            .text_size(px(12.))
                            .text_color(colors.muted)
                            .child("Этот session host не поддерживает библиотеку промптов."),
                    );
                };
                content = content.child(prompt_note(
                    note_color,
                    self.prompt_origin.as_ref().map_or_else(
                        || "Выберите ревизию, чтобы просмотреть полный текст.".into(),
                        |origin| {
                            format!(
                                "Вставка в «{}» · {}",
                                origin.kind.title(),
                                origin.kind.fields()[origin.active_field]
                            )
                        },
                    ),
                ));

                let library = &self.prompt_library;
                if let Some(error) = &library.error {
                    return content.child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .child(
                                div()
                                    .id("prompts-unavailable")
                                    .role(Role::Alert)
                                    .text_size(px(12.))
                                    .text_color(colors.status_amber)
                                    .whitespace_normal()
                                    .child(format!("Библиотека промптов недоступна: {error}")),
                            )
                            .child(self.action_control(
                                "prompts-retry".into(),
                                "Обновить библиотеку".into(),
                                ProductAction::ReloadPrompts,
                                ControlStyle {
                                    role: Role::Button,
                                    selected: false,
                                    compact: true,
                                },
                                cx,
                            )),
                    );
                }

                let mut rows = div()
                    .id("prompts-rows")
                    .role(Role::List)
                    .aria_label("Библиотека промптов")
                    .flex()
                    .flex_col()
                    .gap_1()
                    .min_w_0()
                    .flex_1()
                    .max_h(px(if self.compact { 155. } else { 290. }))
                    .min_h(px(if self.compact { 90. } else { 160. }))
                    .overflow_y_scroll()
                    .overflow_x_hidden()
                    .track_scroll(&self.layout_scroll);
                if prompts.is_empty() {
                    rows = rows.child(prompt_note(note_color, "Сохранённых промптов пока нет."));
                } else if library.rows.is_empty() {
                    rows = rows.child(prompt_note(note_color, "Совпадений по запросу нет."));
                }
                for row in library.visible_rows() {
                    let prompt_ref = row.prompt;
                    let selected = library.selected == Some(prompt_ref);
                    let row_colors = colors;
                    rows = rows.child(
                        div()
                            .id(format!(
                                "prompt-row-{}-{}",
                                prompt_ref.id, prompt_ref.revision
                            ))
                            .role(Role::ListItem)
                            .aria_label(format!("{}, ревизия {}", row.title, prompt_ref.revision))
                            .aria_selected(selected)
                            .min_w_0()
                            .overflow_hidden()
                            .flex_shrink_0()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .p(px(8.))
                            .rounded(px(7.))
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
                            .hover(move |style| style.bg(row_colors.hover))
                            .child(
                                div()
                                    .text_size(px(13.))
                                    .font_weight(FontWeight::MEDIUM)
                                    .truncate()
                                    .child(row.title.clone()),
                            )
                            .child(prompt_note(
                                note_color,
                                format!("Ревизия {}", prompt_ref.revision),
                            ))
                            .child(
                                div()
                                    .text_size(px(11.))
                                    .text_color(colors.muted)
                                    .truncate()
                                    .child(row.snippet.clone()),
                            )
                            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                this.product_action(
                                    ProductAction::SelectPrompt(prompt_ref),
                                    window,
                                    cx,
                                );
                                cx.stop_propagation();
                            })),
                    );
                }

                let selected_prompt = library.selected.and_then(|selected| {
                    prompts
                        .iter()
                        .find(|prompt| {
                            prompt.id == selected.id && prompt.revision == selected.revision
                        })
                        .map(|prompt| (selected, prompt))
                });
                let mut preview = div()
                    .id("prompt-revision-preview")
                    .role(Role::Region)
                    .aria_label("Полный текст выбранной ревизии")
                    .flex()
                    .flex_col()
                    .gap_2()
                    .flex_1()
                    .min_w_0()
                    .min_h_0()
                    .max_h(px(if self.compact { 255. } else { 310. }))
                    .overflow_x_hidden();
                if let Some((reference, prompt)) = selected_prompt {
                    preview = preview
                        .child(
                            div()
                                .text_size(px(14.))
                                .font_weight(FontWeight::SEMIBOLD)
                                .whitespace_normal()
                                .child(prompt.title.clone()),
                        )
                        .child(prompt_note(
                            note_color,
                            format!("Ревизия {}", prompt.revision),
                        ))
                        .child(
                            div()
                                .id("prompt-full-text")
                                .role(Role::Document)
                                .aria_label("Текст промпта, только для чтения")
                                .flex_1()
                                .min_h(px(if self.compact { 90. } else { 120. }))
                                .max_h(px(if self.compact { 155. } else { 225. }))
                                .overflow_y_scroll()
                                .track_scroll(&self.prompt_preview_scroll)
                                .p(px(10.))
                                .rounded(px(7.))
                                .border_1()
                                .border_color(colors.border)
                                .bg(colors.base)
                                .text_size(px(12.))
                                .text_color(colors.text)
                                .whitespace_normal()
                                .child(prompt.content.clone()),
                        );
                    let mut actions = vec![(
                        "prompt-use",
                        if self.prompt_origin.is_some() {
                            "Вставить в черновик"
                        } else {
                            "Копировать"
                        },
                        ProductAction::UsePrompt(reference),
                    )];
                    if self.prompt_origin.is_some() {
                        actions.push((
                            "prompt-copy",
                            "Копировать отдельно",
                            ProductAction::CopyPrompt(reference),
                        ));
                    }
                    actions.extend([
                        (
                            "prompt-edit",
                            "Изменить",
                            ProductAction::EditPrompt(reference),
                        ),
                        (
                            "prompt-duplicate",
                            "Дублировать",
                            ProductAction::DuplicatePrompt(reference),
                        ),
                        (
                            "prompt-delete",
                            "Удалить…",
                            ProductAction::DeletePrompt(reference),
                        ),
                    ]);
                    let mut preview_actions = div().flex().flex_wrap().gap_1();
                    for (id, label, action) in actions {
                        preview_actions = preview_actions.child(self.action_control(
                            id.into(),
                            label.into(),
                            action,
                            ControlStyle {
                                role: Role::Button,
                                selected: false,
                                compact: true,
                            },
                            cx,
                        ));
                    }
                    preview = preview.child(preview_actions);
                } else {
                    preview = preview.child(
                        div()
                            .id("prompt-preview-empty")
                            .role(Role::Status)
                            .flex_1()
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_size(px(12.))
                            .text_color(colors.muted)
                            .whitespace_normal()
                            .child("Выберите результат, чтобы увидеть весь текст и действия."),
                    );
                }

                let body = if self.compact {
                    div().flex().flex_col().gap_2().child(rows).child(preview)
                } else {
                    div()
                        .flex()
                        .gap_3()
                        .min_w_0()
                        .min_h_0()
                        .child(rows)
                        .child(preview)
                };
                let page = library.offset / crate::prompt_flow::PROMPT_PAGE_SIZE + 1;
                let page_count = library
                    .rows
                    .len()
                    .max(1)
                    .div_ceil(crate::prompt_flow::PROMPT_PAGE_SIZE);
                let mut footer = div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_1()
                    .child(self.action_control(
                        "prompt-new".into(),
                        "Новый промпт…".into(),
                        ProductAction::NewPrompt,
                        ControlStyle {
                            role: Role::Button,
                            selected: false,
                            compact: true,
                        },
                        cx,
                    ))
                    .child(self.action_control(
                        "prompts-refresh".into(),
                        "Обновить".into(),
                        ProductAction::ReloadPrompts,
                        ControlStyle {
                            role: Role::Button,
                            selected: false,
                            compact: true,
                        },
                        cx,
                    ));
                if library.offset > 0 {
                    footer = footer.child(self.action_control(
                        "prompts-previous-page".into(),
                        "‹ Назад".into(),
                        ProductAction::PromptPage(false),
                        ControlStyle {
                            role: Role::Button,
                            selected: false,
                            compact: true,
                        },
                        cx,
                    ));
                }
                footer = footer.child(prompt_note(
                    note_color,
                    format!(
                        "{} результатов · стр. {page}/{page_count}",
                        library.rows.len()
                    ),
                ));
                if library
                    .offset
                    .saturating_add(crate::prompt_flow::PROMPT_PAGE_SIZE)
                    < library.rows.len()
                {
                    footer = footer.child(self.action_control(
                        "prompts-next-page".into(),
                        "Дальше ›".into(),
                        ProductAction::PromptPage(true),
                        ControlStyle {
                            role: Role::Button,
                            selected: false,
                            compact: true,
                        },
                        cx,
                    ));
                }

                content
                    .child(body)
                    .child(footer)
                    .child(prompt_note(note_color, "↑ ↓ — выбрать · Enter — использовать · PgUp/PgDn — страницы · Ctrl+N — новый · Ctrl+E — изменить · Ctrl+D — дубликат · Ctrl+Shift+C — копировать · Ctrl+Shift+Delete — удалить с подтверждением · Ctrl+R — обновить · Alt+PgUp/PgDn — прокрутить просмотр · Delete — удалить символ в поиске"))
            }
            DialogKind::Prompt => {
                let Some(editor) = &self.prompt_editor else {
                    return content.child(prompt_note(
                        chrome::Colors::for_theme(self.appearance.theme).muted,
                        "Редактор промпта недоступен.",
                    ));
                };
                content = content.child(prompt_note(
                    chrome::Colors::for_theme(self.appearance.theme).muted,
                    if editor.saved.is_some() {
                        "Изменение сохранённого промпта."
                    } else {
                        "Новый промпт."
                    },
                ));
                if let Some(saved) = &editor.saved {
                    content = content.child(prompt_note(
                        chrome::Colors::for_theme(self.appearance.theme).muted,
                        format!("Текущая ревизия: {}", saved.revision),
                    ));
                }
                content.child(prompt_note(chrome::Colors::for_theme(self.appearance.theme).muted,
                    "Промпт хранится в библиотеке и используется только по вашему действию. Сохранение не отправляет задачу в терминал.",
                ))
            }
            DialogKind::DeletePrompt => {
                if let Some(prompt) = &self.prompt_delete {
                    content
                        .child(div().text_size(px(13.)).child(prompt.title.clone()))
                        .child(prompt_note(
                            chrome::Colors::for_theme(self.appearance.theme).muted,
                            format!(
                                "Будет удалена только запись библиотеки, ревизия {}.",
                                prompt.revision
                            ),
                        ))
                } else {
                    content.child(prompt_note(
                        chrome::Colors::for_theme(self.appearance.theme).muted,
                        "Промпт больше недоступен.",
                    ))
                }
            }
            _ => content,
        }
    }
}

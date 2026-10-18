//! Rendering for the reusable prompt library and editor.
use super::*;

fn prompt_note(text: impl Into<String>) -> Div {
    div()
        .text_size(px(11.))
        .text_color(rgb(chrome::MUTED))
        .whitespace_normal()
        .child(text.into())
}

fn prompt_snippet(content: &str) -> String {
    let first_line = content.lines().next().unwrap_or_default();
    let mut snippet = first_line.chars().take(100).collect::<String>();
    if first_line.chars().count() > 100 {
        snippet.push('…');
    }
    snippet
}

impl ProductUi {
    /// Actions in the same order as the library's keyboard navigation.
    pub(super) fn prompt_menu_entries(&self) -> Vec<(String, ProductAction)> {
        if self.dialog.as_ref().map(|dialog| dialog.kind) != Some(DialogKind::Prompts) {
            return Vec::new();
        }

        let mut entries = vec![("Новый промпт…".into(), ProductAction::NewPrompt)];
        if let Some(prompts) = self
            .catalog
            .as_ref()
            .and_then(|catalog| catalog.prompts.as_ref())
        {
            let use_label = if self.prompt_origin.is_some() {
                "Вставить в черновик"
            } else {
                "Скопировать"
            };
            entries.extend(prompts.iter().map(|prompt| {
                (
                    format!("{use_label} · {}", prompt.title),
                    ProductAction::UsePrompt(prompt.id),
                )
            }));
        }
        entries
    }

    pub(super) fn prompt_dialog_fields(
        &self,
        dialog: &Dialog,
        mut content: Div,
        cx: &mut Context<CanvasApp>,
    ) -> Div {
        let colors = ProductColors::dark();
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

                content = content.child(prompt_note(self.prompt_origin.as_ref().map_or_else(
                    || "Выберите текст для копирования.".into(),
                    |origin| {
                        format!(
                            "Вставка в «{}» · {}",
                            origin.kind.title(),
                            origin.kind.fields()[origin.active_field]
                        )
                    },
                )));
                let mut rows = div()
                    .id("prompts-rows")
                    .role(Role::List)
                    .aria_label("Библиотека промптов")
                    .max_h(px(360.))
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .overflow_y_scroll()
                    .track_scroll(&self.layout_scroll)
                    .child(self.action_control(
                        "prompt-new".into(),
                        "Создать промпт…".into(),
                        ProductAction::NewPrompt,
                        ControlStyle {
                            role: Role::Button,
                            selected: dialog.active_field == 0,
                            compact: false,
                        },
                        cx,
                    ));

                if prompts.is_empty() {
                    rows = rows.child(
                        div()
                            .py(px(12.))
                            .text_size(px(12.))
                            .text_color(colors.muted)
                            .child("Сохранённых промптов пока нет."),
                    );
                }

                for (index, prompt) in prompts.iter().take(128).enumerate() {
                    let id = prompt.id;
                    let selected = dialog.active_field == index + 1;
                    let snippet = prompt_snippet(&prompt.content);
                    rows = rows.child(
                        div()
                            .id(format!("prompt-row-{id}"))
                            .role(Role::ListItem)
                            .aria_label(format!("{} · ревизия {}", prompt.title, prompt.revision))
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
                                            .child(prompt.title.clone()),
                                    )
                                    .child(prompt_note(format!("Ревизия {}", prompt.revision)))
                                    .child(
                                        div()
                                            .text_size(px(11.))
                                            .text_color(colors.muted)
                                            .truncate()
                                            .child(snippet),
                                    ),
                            )
                            .child(
                                self.action_control(
                                    format!("prompt-use-{id}"),
                                    if self.prompt_origin.is_some() {
                                        "Вставить"
                                    } else {
                                        "Копировать"
                                    }
                                    .into(),
                                    ProductAction::UsePrompt(id),
                                    ControlStyle {
                                        role: Role::Button,
                                        selected: false,
                                        compact: true,
                                    },
                                    cx,
                                ),
                            )
                            .children(self.prompt_origin.as_ref().map(|_| {
                                self.icon_control(
                                    &format!("prompt-copy-{id}"),
                                    &format!("Скопировать промпт {}", prompt.title),
                                    "command",
                                    ProductAction::CopyPrompt(id),
                                    cx,
                                )
                            }))
                            .child(self.icon_control(
                                &format!("prompt-edit-{id}"),
                                &format!("Изменить промпт {}", prompt.title),
                                "more",
                                ProductAction::EditPrompt(id),
                                cx,
                            ))
                            .child(self.icon_control(
                                &format!("prompt-delete-{id}"),
                                &format!("Удалить промпт {}", prompt.title),
                                "close",
                                ProductAction::DeletePrompt(id),
                                cx,
                            )),
                    );
                }

                content
                    .child(rows)
                    .child(self.action_control(
                        "prompts-refresh".into(),
                        "Обновить библиотеку".into(),
                        ProductAction::ReloadPrompts,
                        ControlStyle {
                            role: Role::Button,
                            selected: false,
                            compact: true,
                        },
                        cx,
                    ))
                    .child(prompt_note(
                        "↑ ↓ — выбрать · Enter — использовать · Ctrl+E — изменить · Delete — удалить · Ctrl+R — обновить",
                    ))
            }
            DialogKind::Prompt => {
                let Some(editor) = &self.prompt_editor else {
                    return content.child(prompt_note("Редактор промпта недоступен."));
                };
                content = content.child(prompt_note(if editor.saved.is_some() {
                    "Изменение сохранённого промпта."
                } else {
                    "Новый промпт."
                }));
                if let Some(saved) = &editor.saved {
                    content =
                        content.child(prompt_note(format!("Текущая ревизия: {}", saved.revision)));
                }
                content.child(prompt_note(
                    "Промпт хранится в библиотеке и используется только по вашему действию. Сохранение не отправляет задачу в терминал.",
                ))
            }
            DialogKind::DeletePrompt => {
                if let Some(prompt) = &self.prompt_delete {
                    content
                        .child(div().text_size(px(13.)).child(prompt.title.clone()))
                        .child(prompt_note(format!(
                            "Будет удалена только запись библиотеки, ревизия {}.",
                            prompt.revision
                        )))
                } else {
                    content.child(prompt_note("Промпт больше недоступен."))
                }
            }
            _ => content,
        }
    }
}

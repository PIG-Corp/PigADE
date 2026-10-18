//! Review the destination, separately from text printed by a CLI.
use super::*;
use crate::terminal_links::{BlockReason, LinkDecision, SheetStage};

fn blocked_reason(reason: BlockReason) -> &'static str {
    match reason {
        BlockReason::UnsupportedScheme => "Открываются только веб-ссылки HTTP и HTTPS.",
        BlockReason::InvalidAddress | BlockReason::MissingHost => {
            "Адрес не является корректной веб-ссылкой."
        }
        BlockReason::Credentials => "Адрес содержит данные авторизации. Открытие отключено.",
        BlockReason::UnsafeCharacters => {
            "Адрес содержит неоднозначные или управляющие символы. Открытие отключено."
        }
        BlockReason::Oversized => "Адрес превышает допустимый размер.",
    }
}

impl ProductUi {
    pub(super) fn terminal_links_overlay(&self, cx: &mut Context<CanvasApp>) -> AnyElement {
        let colors = self.colors();
        let Some(sheet) = self.terminal_links.as_ref().filter(|sheet| {
            self.dialog.as_ref().is_some_and(|dialog| {
                dialog.kind == DialogKind::TerminalLinks && dialog.id == sheet.dialog_id
            })
        }) else {
            return div()
                .id("terminal-links-unavailable")
                .role(Role::Status)
                .child("Ссылки панели недоступны")
                .into_any_element();
        };
        let button = |id: &str, label: &str, action: ProductAction, cx: &mut Context<CanvasApp>| {
            self.action_control(
                id.into(),
                label.into(),
                action,
                ControlStyle {
                    role: Role::Button,
                    selected: false,
                    compact: true,
                },
                cx,
            )
        };
        let mut content = div().flex().flex_col().gap_2().min_h(px(0.));
        let mut actions = div().mt(px(14.)).flex().flex_wrap().justify_end().gap_2();
        if sheet.stage == SheetStage::List {
            let mut rows = div()
                .id("terminal-links-list")
                .role(Role::List)
                .aria_label("Ссылки видимого экрана")
                .flex()
                .flex_col()
                .gap_1()
                .max_h(px(300.))
                .overflow_y_scroll()
                .track_scroll(&self.terminal_links_scroll);
            for (index, row) in sheet.rows.iter().enumerate() {
                let label = if row.label.is_empty() {
                    "Ссылка".into()
                } else if row.label_truncated {
                    format!("{}…", row.label)
                } else {
                    row.label.clone()
                };
                let destination = match &row.target.decision {
                    LinkDecision::Allowed { domain, .. } => domain.as_str(),
                    LinkDecision::Blocked(_) => "Открытие недоступно",
                };
                rows = rows.child(
                    div()
                        .id(format!("terminal-link-row-{index}"))
                        .role(Role::ListItem)
                        .aria_label(format!("{label} · {destination}"))
                        .px(px(10.))
                        .py(px(8.))
                        .rounded(px(7.))
                        .border_1()
                        .border_color(if index == sheet.selected {
                            colors.accent
                        } else {
                            colors.border
                        })
                        .bg(if index == sheet.selected {
                            colors.selected
                        } else {
                            colors.base
                        })
                        .cursor_pointer()
                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                            this.product_action(
                                ProductAction::SelectTerminalLink(index),
                                window,
                                cx,
                            )
                        }))
                        .child(div().text_size(px(12.)).truncate().child(label))
                        .child(
                            div()
                                .mt(px(3.))
                                .text_size(px(11.))
                                .text_color(colors.muted)
                                .truncate()
                                .child(destination.to_owned()),
                        ),
                );
            }
            if sheet.rows.is_empty() {
                rows = rows.child(
                    div()
                        .id("terminal-links-empty")
                        .role(Role::Status)
                        .text_size(px(12.))
                        .text_color(colors.muted)
                        .child("На показанном экране нет доступных OSC 8 ссылок."),
                );
            }
            content = content.child(rows);
            if sheet.omitted > 0 {
                content=content.child(div().id("terminal-links-partial").role(Role::Status).text_size(px(11.)).text_color(colors.status_amber).whitespace_normal().child("Показана часть ссылок: достигнут лимит размера. Прокрутите исходную панель к нужной ссылке и откройте список снова."));
            }
            content=content.child(div().text_size(px(10.)).text_color(colors.muted).whitespace_normal().child("↑ ↓ — выбрать · Enter — проверить адрес · Ctrl+R — обновить · Esc — закрыть"));
            actions = actions.child(button(
                "terminal-links-refresh",
                "Обновить",
                ProductAction::RefreshTerminalLinks,
                cx,
            ));
            if !sheet.rows.is_empty() {
                actions = actions.child(button(
                    "terminal-links-review",
                    "Проверить адрес…",
                    ProductAction::ReviewTerminalLink,
                    cx,
                ));
            }
        } else if sheet.stage == SheetStage::Review
            && let Some(row) = sheet.rows.get(sheet.selected)
        {
            let literal = sheet
                .blocked_literal_display(sheet.dialog_id)
                .unwrap_or_else(|| crate::terminal_links::display_literal(&row.target.literal));
            content = content
                .child(
                    div()
                        .text_size(px(11.))
                        .text_color(colors.muted)
                        .child("Текст в терминале"),
                )
                .child(div().text_size(px(12.)).whitespace_normal().child(
                    if row.label_truncated {
                        format!("{}…", row.label)
                    } else {
                        row.label.clone()
                    },
                ));
            let mut addresses = div()
                .id("terminal-link-addresses")
                .role(Role::Group)
                .aria_label("Проверяемый адрес")
                .flex()
                .flex_col()
                .gap_2()
                .p(px(10.))
                .rounded(px(7.))
                .border_1()
                .border_color(colors.border)
                .bg(colors.base)
                .max_h(px(230.))
                .overflow_y_scroll()
                .track_scroll(&self.terminal_link_address_scroll);
            match &row.target.decision {
                LinkDecision::Allowed { canonical, domain } => {
                    addresses = addresses
                        .child(
                            div()
                                .text_size(px(11.))
                                .text_color(colors.muted)
                                .child(format!("Назначение · {domain}")),
                        )
                        .child(
                            div()
                                .id("terminal-link-canonical")
                                .text_size(px(12.))
                                .whitespace_normal()
                                .child(canonical.as_str().to_owned()),
                        );
                    if canonical.as_str() != row.target.literal {
                        addresses = addresses
                            .child(
                                div()
                                    .text_size(px(10.))
                                    .text_color(colors.muted)
                                    .child("Исходная запись OSC 8"),
                            )
                            .child(
                                div()
                                    .text_size(px(11.))
                                    .whitespace_normal()
                                    .child(literal.0),
                            );
                    }
                    actions = actions
                        .child(button(
                            "terminal-links-copy",
                            "Копировать адрес",
                            ProductAction::CopyTerminalLink,
                            cx,
                        ))
                        .child(button(
                            "terminal-links-confirm",
                            "Открыть в браузере",
                            ProductAction::ConfirmTerminalLink,
                            cx,
                        ));
                    content = content.child(addresses).child(
                        div()
                            .text_size(px(11.))
                            .text_color(colors.muted)
                            .whitespace_normal()
                            .child(
                                "Ссылка передаётся системному браузеру только после подтверждения.",
                            ),
                    );
                }
                LinkDecision::Blocked(reason) => {
                    addresses =
                        addresses
                            .child(
                                div()
                                    .text_size(px(11.))
                                    .text_color(colors.muted)
                                    .child("Исходная запись · специальные символы экранированы"),
                            )
                            .child(div().text_size(px(12.)).whitespace_normal().child(
                                if literal.1 {
                                    format!("{}…", literal.0)
                                } else {
                                    literal.0
                                },
                            ));
                    content = content.child(addresses).child(
                        div()
                            .id("terminal-link-blocked")
                            .role(Role::Status)
                            .text_size(px(12.))
                            .text_color(colors.status_amber)
                            .whitespace_normal()
                            .child(blocked_reason(*reason)),
                    );
                }
            }
            let hints = if matches!(row.target.decision, LinkDecision::Allowed { .. }) {
                "Ctrl+Enter — открыть · Ctrl+C — копировать · Alt+← — к списку · Esc — отмена"
            } else {
                "Alt+← — к списку · Esc — закрыть"
            };
            content = content.child(
                div()
                    .text_size(px(10.))
                    .text_color(colors.muted)
                    .whitespace_normal()
                    .child(hints),
            );
            actions = actions.child(button(
                "terminal-links-back",
                "К списку",
                ProductAction::BackTerminalLinks,
                cx,
            ));
        }
        actions = actions.child(button(
            "terminal-links-close",
            "Закрыть",
            ProductAction::CloseDialog,
            cx,
        ));
        if let Some(notice) = &self.notice {
            content = content.child(
                div()
                    .id("terminal-links-notice")
                    .role(Role::Status)
                    .text_size(px(11.))
                    .text_color(colors.accent)
                    .whitespace_normal()
                    .child(notice.clone()),
            );
        }
        let source = if matches!(
            sheet.source.state.as_str(),
            "history" | "historical" | "history_unavailable"
        ) {
            "Сохранённая история"
        } else {
            "Снимок экрана"
        };
        div()
            .id("terminal-links-overlay")
            .absolute()
            .inset_0()
            .flex()
            .items_start()
            .justify_center()
            .overflow_y_scroll()
            .py(px(24.))
            .bg(rgb(0x090b0e).opacity(0.72))
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                this.product_action(ProductAction::CloseDialog, window, cx)
            }))
            .child(
                div()
                    .id("terminal-links-dialog")
                    .role(Role::Dialog)
                    .aria_label("Ссылки панели")
                    .w_full()
                    .max_w(px(640.))
                    .mx(px(16.))
                    .p(px(18.))
                    .rounded(px(12.))
                    .border_1()
                    .border_color(colors.border)
                    .bg(colors.raised)
                    .text_color(colors.text)
                    .on_click(cx.listener(|_, _: &ClickEvent, _, cx| cx.stop_propagation()))
                    .child(
                        div()
                            .text_size(px(18.))
                            .child(if sheet.stage == SheetStage::Review {
                                "Проверить ссылку"
                            } else {
                                "Ссылки панели"
                            }),
                    )
                    .child(
                        div()
                            .mt(px(5.))
                            .mb(px(12.))
                            .text_size(px(11.))
                            .text_color(colors.muted)
                            .child(format!(
                                "{source} · последовательность {}",
                                sheet.source.sequence
                            )),
                    )
                    .child(content)
                    .child(actions),
            )
            .into_any_element()
    }
}

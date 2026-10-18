//! Explicit, confirmed control for one exact CLI generation.
use super::*;

impl ProductUi {
    pub(super) fn stop_overlay(&self, cx: &mut Context<CanvasApp>) -> AnyElement {
        let colors = self.colors();
        let Some(sheet) = &self.stop_sheet else {
            return div()
                .id("stop-dialog-unavailable")
                .role(Role::Dialog)
                .child("Состояние остановки недоступно. Обновите список сессий.")
                .into_any_element();
        };
        if !self.dialog.as_ref().is_some_and(|dialog| {
            dialog.kind == DialogKind::StopSession && dialog.id == sheet.dialog
        }) {
            return div()
                .id("stop-dialog-stale")
                .role(Role::Dialog)
                .child("Диалог остановки устарел. Откройте его снова из списка сессий.")
                .into_any_element();
        }

        let controls = match sheet.phase {
            crate::session_control::Phase::Draft => div()
                .flex()
                .justify_end()
                .gap_2()
                .child(
                    self.action_control(
                        "stop-dialog-cancel".into(),
                        "Отмена".into(),
                        ProductAction::CloseDialog,
                        ControlStyle {
                            role: Role::Button,
                            selected: self
                                .dialog
                                .as_ref()
                                .is_some_and(|dialog| dialog.active_field == 0),
                            compact: true,
                        },
                        cx,
                    ),
                )
                .child(
                    self.action_control(
                        "stop-dialog-confirm".into(),
                        "Остановить CLI".into(),
                        ProductAction::ConfirmStop,
                        ControlStyle {
                            role: Role::Button,
                            selected: self
                                .dialog
                                .as_ref()
                                .is_some_and(|dialog| dialog.active_field == 1),
                            compact: true,
                        },
                        cx,
                    ),
                ),
            crate::session_control::Phase::Waiting
            | crate::session_control::Phase::Unknown
            | crate::session_control::Phase::Unavailable => div()
                .flex()
                .justify_end()
                .gap_2()
                .child(
                    self.action_control(
                        "stop-dialog-inspect".into(),
                        "Проверить состояние".into(),
                        ProductAction::InspectStop,
                        ControlStyle {
                            role: Role::Button,
                            selected: self
                                .dialog
                                .as_ref()
                                .is_some_and(|dialog| dialog.active_field == 0),
                            compact: true,
                        },
                        cx,
                    ),
                )
                .child(
                    self.action_control(
                        "stop-dialog-close".into(),
                        "Закрыть".into(),
                        ProductAction::CloseDialog,
                        ControlStyle {
                            role: Role::Button,
                            selected: self
                                .dialog
                                .as_ref()
                                .is_some_and(|dialog| dialog.active_field == 1),
                            compact: true,
                        },
                        cx,
                    ),
                ),
            crate::session_control::Phase::Sending | crate::session_control::Phase::Complete => {
                let label = if sheet.phase == crate::session_control::Phase::Complete {
                    "Готово"
                } else {
                    "Закрыть"
                };
                div().flex().justify_end().child(
                    self.action_control(
                        "stop-dialog-close".into(),
                        label.into(),
                        ProductAction::CloseDialog,
                        ControlStyle {
                            role: Role::Button,
                            selected: self
                                .dialog
                                .as_ref()
                                .is_some_and(|dialog| dialog.active_field == 0),
                            compact: true,
                        },
                        cx,
                    ),
                )
            }
        };

        div()
            .id("stop-dialog-overlay")
            .accessibility_id("stop-dialog-overlay")
            .absolute()
            .inset_0()
            .flex()
            .items_start()
            .justify_center()
            .py(px(24.))
            .bg(rgb(0x090b0e).opacity(0.72))
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                this.product_action(ProductAction::CloseDialog, window, cx);
            }))
            .child(
                div()
                    .id("stop-dialog")
                    .role(Role::Dialog)
                    .aria_label("Остановить CLI?")
                    .w_full()
                    .max_w(px(460.))
                    .mx(px(16.))
                    .flex()
                    .flex_col()
                    .gap_2()
                    .rounded(px(12.))
                    .border_1()
                    .border_color(colors.border)
                    .bg(colors.raised)
                    .px(px(20.))
                    .py(px(18.))
                    .text_color(colors.text)
                    .on_click(cx.listener(|_, _: &ClickEvent, _, cx| cx.stop_propagation()))
                    .child(
                        div()
                            .id("stop-dialog-title")
                            .role(Role::Heading)
                            .aria_level(2)
                            .text_size(px(16.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(match sheet.phase {
                                crate::session_control::Phase::Draft => "Остановить CLI?",
                                crate::session_control::Phase::Complete => "CLI завершён",
                                _ => "Остановка CLI",
                            }),
                    )
                    .child(
                        div()
                            .id("stop-dialog-target")
                            .text_size(px(14.))
                            .font_weight(FontWeight::MEDIUM)
                            .truncate()
                            .child(sheet.target.title.clone()),
                    )
                    .child(
                        div()
                            .id("stop-dialog-status")
                            .role(Role::Status)
                            .text_size(px(12.))
                            .text_color(colors.accent)
                            .whitespace_normal()
                            .child(sheet.phase.message()),
                    )
                    .child(
                        div()
                            .id("stop-dialog-scope")
                            .text_size(px(11.))
                            .text_color(colors.muted)
                            .whitespace_normal()
                            .child("Дочерние процессы CLI могут продолжить работу."),
                    )
                    .children(self.notice.clone().map(|notice| {
                        div()
                            .id("stop-dialog-notice")
                            .role(Role::Status)
                            .text_size(px(11.))
                            .text_color(colors.accent)
                            .whitespace_normal()
                            .child(notice)
                    }))
                    .child(controls),
            )
            .into_any_element()
    }
}

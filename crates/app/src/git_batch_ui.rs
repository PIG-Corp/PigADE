//! Explicit whole-index review and retained batch outcomes. No implicit retries.
use super::*;
use crate::git_batch;
use ProductAction as A;
use protocol::{GitIndexAction, GitIndexPhase};

const PAGE: usize = 8;

fn action_label(action: GitIndexAction) -> &'static str {
    match action {
        GitIndexAction::Stage => "Подготовить всё к коммиту",
        GitIndexAction::Unstage => "Снять всё с подготовки",
    }
}

impl CanvasApp {
    pub(crate) fn git_batch_action(&mut self, action: A, cx: &mut Context<Self>) {
        if self.product.maintenance.mutations_frozen() {
            self.product.notice = Some("Дождитесь завершения обновления службы".into());
            return;
        }
        if self.git_flow.request_pending() || self.product.git_batch.request_pending() {
            self.product.notice = Some("Дождитесь ответа на предыдущую операцию Git".into());
            cx.notify();
            return;
        }
        let current_dialog = self.product.dialog.as_ref().map(|dialog| dialog.id);
        let command = match action {
            A::GitPrepareBatch(action) => {
                if self.git_flow.close_uncertainty().is_some() {
                    self.product.notice =
                        Some("Сначала проверьте исход предыдущей операции Git".into());
                    return;
                }
                let Some(sheet) = &self.product.git_sheet else {
                    return;
                };
                if !sheet.can_prepare_batch(action)
                    || !sheet.target.current(self.product.catalog.as_ref())
                {
                    self.product.notice =
                        Some("Для пакетного действия нужен полный актуальный снимок Git".into());
                    return;
                }
                let target = sheet.target.clone();
                let dialog = Dialog::new(DialogKind::WorkspaceGitBatch);
                let command = self.product.git_batch.prepare(target, dialog.id, action);
                if let Ok(command) = command {
                    if let Err(error) = self.git_worker.submit(command) {
                        self.product.git_batch.abort_unsent();
                        self.product.notice = Some(error);
                    } else {
                        self.product.dialog = Some(dialog);
                        self.product.notice = None;
                    }
                } else if let Err(error) = command {
                    self.product.notice = Some(error);
                }
                cx.notify();
                return;
            }
            A::GitShowBatchOutcome => {
                let Some(operation) = &self.product.git_batch.operation else {
                    return;
                };
                let dialog = Dialog::new(DialogKind::WorkspaceGitBatch);
                self.product.git_batch.sheet = Some(git_batch::Sheet {
                    dialog: dialog.id,
                    target: operation.target.clone(),
                    action: operation.state.action,
                    plan: None,
                    error: None,
                    page: 0,
                });
                self.product.dialog = Some(dialog);
                cx.notify();
                return;
            }
            A::GitBatchPage(next) => {
                if let Some(sheet) = &mut self.product.git_batch.sheet
                    && current_dialog == Some(sheet.dialog)
                {
                    let count = sheet
                        .plan
                        .as_ref()
                        .map(|plan| plan.paths.len())
                        .or_else(|| {
                            self.product
                                .git_batch
                                .operation
                                .as_ref()
                                .filter(|operation| operation.target.same_scope(&sheet.target))
                                .map(|operation| operation.state.paths.len())
                        })
                        .unwrap_or(0);
                    let pages = count.div_ceil(PAGE).max(1);
                    sheet.page = if next {
                        (sheet.page + 1).min(pages - 1)
                    } else {
                        sheet.page.saturating_sub(1)
                    };
                }
                cx.notify();
                return;
            }
            A::GitConfirmBatch => {
                if self.git_flow.close_uncertainty().is_some() {
                    self.product.notice =
                        Some("Сначала проверьте исход предыдущей операции Git".into());
                    return;
                }
                let Some(dialog) = current_dialog else {
                    return;
                };
                self.product
                    .git_batch
                    .confirm(self.product.catalog.as_ref(), dialog)
            }
            A::GitInspectBatch => {
                let Some(dialog) = current_dialog else {
                    return;
                };
                self.product.git_batch.inspect(dialog)
            }
            A::GitAcknowledgeBatch => {
                let Some(dialog) = current_dialog else {
                    return;
                };
                self.product.git_batch.acknowledge(dialog)
            }
            _ => return,
        };
        match command {
            Ok(command) => {
                if let Err(error) = self.git_worker.submit(command) {
                    self.product.git_batch.abort_unsent();
                    self.product.notice = Some(error);
                } else {
                    self.product.notice = None;
                }
            }
            Err(error) => self.product.notice = Some(error),
        }
        cx.notify();
    }

    pub(crate) fn receive_git_batch(
        &mut self,
        reply: Result<protocol::ProductReply, protocol::ProductError>,
        cx: &mut Context<Self>,
    ) {
        let dialog = self
            .product
            .dialog
            .as_ref()
            .filter(|dialog| dialog.kind == DialogKind::WorkspaceGitBatch)
            .map(|dialog| dialog.id);
        self.product
            .git_batch
            .receive(reply, dialog, self.product.catalog.as_ref());
        if self.product.git_batch.unresolved() {
            self.close_flow
                .fail("Исход пакетной операции Git требует проверки");
        }
        cx.notify();
    }
}

impl ProductUi {
    pub(super) fn git_batch_overlay(&self, cx: &mut Context<CanvasApp>) -> AnyElement {
        let colors = self.colors();
        let Some(sheet) = self.git_batch.sheet.as_ref().filter(|sheet| {
            self.dialog.as_ref().is_some_and(|dialog| {
                dialog.kind == DialogKind::WorkspaceGitBatch && dialog.id == sheet.dialog
            })
        }) else {
            return div()
                .id("git-batch-unavailable")
                .role(Role::Status)
                .child("Предпросмотр недоступен")
                .into_any_element();
        };
        let operation = self.git_batch.operation.as_ref().filter(|operation| {
            operation.target.same_scope(&sheet.target)
                && operation.state.action == sheet.action
                && sheet
                    .plan
                    .as_ref()
                    .is_none_or(|plan| plan.ticket == operation.state.ticket)
        });
        let applying = operation.is_some_and(|operation| operation.applying);
        let busy = self.git_batch.request_pending();
        let plan = sheet.plan.as_ref();
        let paths = plan
            .map(|plan| plan.paths.as_slice())
            .or_else(|| operation.map(|operation| operation.state.paths.as_slice()))
            .unwrap_or(&[]);
        let pages = paths.len().div_ceil(PAGE).max(1);
        let page = sheet.page.min(pages - 1);
        let mut body = div()
            .id("git-batch-paths")
            .role(Role::List)
            .aria_label("Точные пути пакетной операции")
            .min_h(px(0.))
            .max_h(px(330.))
            .overflow_y_scroll()
            .overflow_x_scroll()
            .flex()
            .flex_col()
            .gap_1()
            .mt(px(12.));
        for (index, path) in paths.iter().enumerate().skip(page * PAGE).take(PAGE) {
            body = body.child(
                div()
                    .id(format!("git-batch-path-{index}"))
                    .role(Role::ListItem)
                    .flex_shrink_0()
                    .text_size(px(12.))
                    .whitespace_nowrap()
                    .child(
                        path.chars()
                            .flat_map(|ch| {
                                if ch.is_control() {
                                    ch.escape_default().collect::<Vec<_>>()
                                } else {
                                    vec![ch]
                                }
                            })
                            .collect::<String>(),
                    ),
            );
        }
        if let Some(error) = &sheet.error {
            body = body.child(
                div()
                    .id("git-batch-status-1")
                    .role(Role::Status)
                    .text_size(px(12.))
                    .text_color(colors.status_amber)
                    .whitespace_normal()
                    .child(error.clone()),
            );
        }
        if busy {
            body = body.child(
                div()
                    .id("git-batch-status-2")
                    .role(Role::Status)
                    .text_size(px(12.))
                    .text_color(colors.accent)
                    .child(if applying {
                        "Применяем один раз…"
                    } else {
                        "Проверяем состояние…"
                    }),
            );
        }
        if let Some(operation) = operation {
            let label = match operation.state.phase {
                GitIndexPhase::Applied => "Изменение индекса подтверждено",
                GitIndexPhase::Rejected => "Пакет отклонён — обновите снимок Git",
                GitIndexPhase::Unknown if operation.state.acknowledged => {
                    "Неизвестный исход просмотрен; применение не повторяется"
                }
                GitIndexPhase::Unknown => "Исход неизвестен — проверьте состояние",
                GitIndexPhase::Unavailable => "Квитанция недоступна — проверьте состояние",
            };
            body = body.child(
                div()
                    .id("git-batch-status-3")
                    .role(Role::Status)
                    .mt(px(8.))
                    .text_size(px(12.))
                    .text_color(colors.accent)
                    .whitespace_normal()
                    .child(label),
            );
            if let Some(message) = &operation.state.message {
                body = body.child(
                    div()
                        .text_size(px(11.))
                        .text_color(colors.muted)
                        .whitespace_normal()
                        .child(message.clone()),
                );
            }
        }
        let button = |id: &str, label: &str, action: A, cx: &mut Context<CanvasApp>| {
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
        let mut controls = div().flex().flex_wrap().justify_end().gap_2().mt(px(14.));
        if page > 0 {
            controls = controls.child(button(
                "git-batch-prev",
                "Назад",
                A::GitBatchPage(false),
                cx,
            ));
        }
        if page + 1 < pages {
            controls = controls.child(button("git-batch-next", "Далее", A::GitBatchPage(true), cx));
        }
        if !busy
            && operation.is_some_and(|operation| {
                !operation.applying
                    && matches!(
                        operation.state.phase,
                        GitIndexPhase::Unknown | GitIndexPhase::Unavailable
                    )
            })
        {
            controls = controls.child(button(
                "git-batch-inspect",
                "Проверить исход",
                A::GitInspectBatch,
                cx,
            ));
            if self.git_batch.unresolved() {
                controls = controls.child(button(
                    "git-batch-ack",
                    "Подтвердить просмотр",
                    A::GitAcknowledgeBatch,
                    cx,
                ));
            }
        }
        if !busy && plan.is_some() && operation.is_none() && !self.git_batch.unresolved() {
            controls = controls.child(button(
                "git-batch-confirm",
                "Применить",
                A::GitConfirmBatch,
                cx,
            ));
        }
        controls = controls
            .child(button(
                "git-batch-back",
                "К списку Git",
                A::OpenWorkspaceGit,
                cx,
            ))
            .child(button("git-batch-close", "Закрыть", A::CloseDialog, cx));
        let detail = format!(
            "{} · {} путей · страница {} / {}",
            sheet.target.name,
            paths.len(),
            page + 1,
            pages
        );
        div().id("git-batch-overlay").absolute().inset_0().flex().items_start().justify_center()
            .overflow_y_scroll().py(px(20.)).bg(rgb(0x090b0e).opacity(0.72))
            .on_click(cx.listener(|this,_:&ClickEvent,window,cx|this.product_action(A::CloseDialog,window,cx)))
            .child(div().id("git-batch-dialog").role(Role::Dialog).aria_label(action_label(sheet.action))
                .w_full().max_w(px(720.)).mx(px(16.)).max_h(gpui::relative(1.)).p(px(20.))
                .flex().flex_col().rounded(px(12.)).border_1().border_color(colors.border)
                .bg(colors.raised).text_color(colors.text)
                .on_click(cx.listener(|_,_:&ClickEvent,_,cx|cx.stop_propagation()))
                .child(div().text_size(px(18.)).child(action_label(sheet.action)))
                .child(div().mt(px(5.)).text_size(px(11.)).text_color(colors.muted).whitespace_normal().child(detail))
                .child(div().mt(px(5.)).text_size(px(11.)).text_color(colors.muted).whitespace_normal()
                    .child("Меняется только индекс. Рабочие файлы сохраняются. Git filters могут выполняться при подготовке файлов. Изменившийся снимок отклоняет весь пакет."))
                .child(body)
                .child(div().mt(px(10.)).text_size(px(10.)).text_color(colors.muted).whitespace_normal()
                    .child("Ctrl+Enter — применить · Ctrl+R — проверить исход · Ctrl+Shift+R — подтвердить просмотр · PgUp/PgDn — пути · Esc — закрыть"))
                .when_some(self.notice.as_ref(),|view,notice|view.child(div().mt(px(6.)).text_size(px(11.)).text_color(colors.status_amber).whitespace_normal().child(notice.clone())))
                .child(controls))
            .into_any_element()
    }
}

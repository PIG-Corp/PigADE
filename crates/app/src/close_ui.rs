//! Modal close review preserves the underlying editor and independent host.
use super::*;
use close_flow::Phase;

impl CanvasApp {
    fn close_drafts(&self) -> Vec<dirty_documents::DraftNotice> {
        let baseline = self.navigation.rename_session().and_then(|session| {
            self.product
                .catalog
                .as_ref()?
                .pane_labels
                .as_ref()?
                .iter()
                .find(|label| label.session == session)
                .map(|label| label.name.as_str())
        });
        let mut drafts = dirty_documents::collect(
            &self.product,
            &self.git_flow,
            baseline,
            self.product.create_project_parent_baseline.as_deref(),
        );
        if self.resize_gesture.is_none() && self.resize_pending.is_some() {
            drafts.push(dirty_documents::DraftNotice {
                key: "keyboard-layout".into(),
                label: "Неприменённое изменение пропорций панелей".into(),
            });
        }
        drafts
    }

    fn close_pending(&self) -> Vec<&'static str> {
        let mut pending = Vec::new();
        if self.activity_writer.pending(&self.product.activity.saved()) {
            pending.push("Отметки просмотра активности");
        }
        if self.product.busy || self.catalog_edit.is_some() || self.picker_request.is_some() {
            pending.push("Ответ session host на отправленную операцию");
        }
        if self.focus_queue.is_saving()
            || self.focus_queue.pending().is_some()
            || self.deferred_placement.is_some()
            || self.deferred_tab_drop.is_some()
        {
            pending.push("Фокус и размещение панелей");
        }
        if !self.canvas_dirty.is_empty() || self.canvas_saving.is_some() {
            pending.push("Положение canvas");
        }
        if self.workspace_nav_writer.pending() {
            pending.push("Последние воркспейсы и дерево проектов");
        }
        if self.product.controls_saving {
            pending.push("Клавиша управления");
        }
        if self.product.maintenance.mutations_frozen() {
            pending.push("Обновление фоновой службы PigADE");
        }
        if self.product.appearance_pending.is_some() {
            pending.push("Оформление и размер шрифта");
        }
        if self.git_flow.request_pending() {
            pending.push("Ответ на отправленную операцию Git");
        }
        pending.extend(self.git_flow.unresolved_resources());
        if self.mouse_delivery.has_pending_receipts()
            || self
                .pointer
                .as_ref()
                .is_some_and(|route| route.encoder.has_pressed())
        {
            pending.push("Приём ввода и отпускания кнопок session host");
        }
        pending
    }

    pub(super) fn request_window_close(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.close_flow.active() {
            return false;
        }
        // Do not put a modal over a live native composition: its eventual commit
        // must keep the existing editor/terminal destination.
        if !self.text_input.preedit.is_empty()
            || self
                .product
                .active_editor()
                .is_some_and(|buffer| buffer.marked().is_some())
        {
            self.product.notice = Some("Завершите ввод IME перед закрытием окна.".into());
            self.input_notice = self.product.notice.clone();
            cx.notify();
            return false;
        }
        self.cancel_pane_drag(cx);
        if self.resize_gesture.is_some() {
            self.finish_resize(false, cx);
        }
        self.canvas_drag = None;
        self.editor_drag = None;
        self.selection_dragging = false;
        self.clear_external_files(cx);
        self.finish_pointer(cx);
        self.input_epoch = self.input_epoch.wrapping_add(1);
        // An unsent deferred navigation/launch is cancelled, never launched by quit.
        self.deferred_product_action = None;
        self.prefix = false;
        let drafts = self.close_drafts();
        let pending = self.close_pending();
        if self.close_flow.request(drafts, pending, Instant::now()) {
            return true;
        }
        if let Some(reason) = self.git_flow.close_uncertainty() {
            self.close_flow.fail(reason);
        }
        self.canvas_save_at = Some(Instant::now());
        self.flush_canvas_offset(cx);
        self.focus.focus(window, cx);
        window.notify_text_input_changed(&self.focus);
        cx.notify();
        false
    }

    pub(super) fn service_window_close(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.close_flow.active() {
            return false;
        }
        if self.close_flow.phase != Some(Phase::Blocked)
            && let Some(reason) = self.git_flow.close_uncertainty()
        {
            self.close_flow.fail(reason);
        }
        let previous = (
            self.close_flow.phase,
            self.close_flow.drafts.clone(),
            self.close_flow.pending.clone(),
        );
        if self
            .close_flow
            .service(self.close_drafts(), self.close_pending(), Instant::now())
        {
            window.remove_window();
            return true;
        }
        if previous
            != (
                self.close_flow.phase,
                self.close_flow.drafts.clone(),
                self.close_flow.pending.clone(),
            )
        {
            cx.notify();
        }
        false
    }

    fn cancel_window_close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close_flow.cancel();
        window.notify_text_input_changed(&self.focus);
        cx.notify();
    }

    fn confirm_window_close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Refresh before granting consent, including drafts from late ACKs.
        let current = self.close_drafts();
        if current != self.close_flow.drafts {
            self.close_flow.drafts = current;
            self.close_flow.phase = Some(Phase::Review);
            self.close_flow.confirm_selected = false;
            cx.notify();
            return;
        }
        if self.close_flow.confirm(Instant::now()) {
            window.remove_window();
        } else {
            self.service_window_close(window, cx);
        }
        cx.notify();
    }

    pub(super) fn close_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.close_flow.active() {
            return false;
        }
        let key = &event.keystroke;
        if !event.is_held && !key.modifiers.alt && !key.modifiers.platform {
            match key.key.as_str() {
                "escape" if !key.modifiers.control && !key.modifiers.shift => {
                    self.cancel_window_close(window, cx)
                }
                "enter" if key.modifiers.control && !key.modifiers.shift => {
                    self.confirm_window_close(window, cx)
                }
                "enter" if !key.modifiers.control && !key.modifiers.shift => {
                    if self.close_flow.confirm_selected {
                        self.confirm_window_close(window, cx);
                    } else {
                        self.cancel_window_close(window, cx);
                    }
                }
                "tab" | "left" | "right"
                    if !key.modifiers.control && self.close_flow.phase != Some(Phase::Flushing) =>
                {
                    self.close_flow.confirm_selected = !self.close_flow.confirm_selected;
                    cx.notify();
                }
                _ => (),
            }
        }
        cx.stop_propagation();
        true
    }

    pub(super) fn close_overlay(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let phase = self.close_flow.phase?;
        let colors = chrome::Colors::for_theme(self.product.appearance.theme);
        let flushing = phase == Phase::Flushing;
        let blocked = phase == Phase::Blocked;
        let confirm = if blocked {
            "Закрыть с неподтверждённым сохранением"
        } else {
            "Закрыть без черновиков"
        };
        let mut body = div().flex().flex_col().gap_2();
        for draft in &self.close_flow.drafts {
            body = body.child(div().text_size(px(13.)).child(format!("• {}", draft.label)));
        }
        if !self.close_flow.pending.is_empty() {
            body = body.child(
                div()
                    .mt(px(8.))
                    .text_size(px(12.))
                    .text_color(rgb(colors.muted))
                    .child(if flushing {
                        "Ожидание подтверждения:"
                    } else {
                        "Отправленные изменения:"
                    }),
            );
            for label in &self.close_flow.pending {
                body = body.child(div().text_size(px(12.)).child(format!("• {label}")));
            }
        }
        if let Some(error) = &self.close_flow.error {
            body = body.child(
                div()
                    .id("quit-error")
                    .role(Role::Status)
                    .text_size(px(12.))
                    .text_color(rgb(colors.status_amber))
                    .child(error.clone()),
            );
        }
        let observed_running = self
            .state
            .views
            .iter()
            .filter(|session| session.state == "running")
            .count();
        body = body.child(
            div()
                .text_size(px(12.))
                .text_color(rgb(colors.muted))
                .child(format!(
                    "Видимых работающих терминальных панелей — {observed_running}."
                )),
        );
        let return_selected = !self.close_flow.confirm_selected;
        let cancel = div()
            .id("quit-cancel")
            .role(Role::Button)
            .aria_label("Вернуться к редактированию")
            .px(px(12.))
            .py(px(8.))
            .rounded(px(6.))
            .border_1()
            .border_color(rgb(if return_selected {
                colors.accent
            } else {
                colors.border
            }))
            .bg(rgb(colors.hover))
            .child("Вернуться к редактированию")
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    this.cancel_window_close(window, cx);
                    cx.stop_propagation();
                }),
            );
        let buttons = div()
            .flex()
            .flex_wrap()
            .gap_2()
            .mt(px(14.))
            .child(cancel)
            .when(!flushing, |row| {
                row.child(
                    div()
                        .id("quit-confirm")
                        .role(Role::Button)
                        .aria_label(confirm)
                        .px(px(12.))
                        .py(px(8.))
                        .rounded(px(6.))
                        .border_1()
                        .border_color(rgb(if return_selected {
                            colors.border
                        } else {
                            colors.accent
                        }))
                        .bg(rgb(colors.raised))
                        .child(confirm)
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, window, cx| {
                                this.confirm_window_close(window, cx);
                                cx.stop_propagation();
                            }),
                        ),
                )
            });
        Some(div().id("quit-overlay").absolute().inset_0().flex().justify_center().items_start()
            .py(px(36.)).overflow_y_scroll().bg(rgb(colors.base).opacity(0.82))
            .on_mouse_down(MouseButton::Left,cx.listener(|_,_,_,cx|cx.stop_propagation()))
            .on_mouse_down(MouseButton::Right,cx.listener(|_,_,_,cx|cx.stop_propagation()))
            .child(div().id("quit-dialog").role(Role::Dialog).aria_label("Закрыть PigADE?")
                .w_full().max_w(px(590.)).mx(px(16.)).p(px(24.)).rounded(px(12.)).border_1()
                .border_color(rgb(colors.border)).bg(rgb(colors.raised)).text_color(rgb(colors.text))
                .flex().flex_col().gap_2().whitespace_normal()
                .child(div().id("quit-heading").role(Role::Heading).aria_level(2).text_size(px(18.)).child(if flushing { "Завершаем сохранение…" } else { "Закрыть PigADE?" }))
                .child(div().text_size(px(13.)).text_color(rgb(colors.muted)).child(if flushing {
                    "Окно закроется после ответа на отправленные операции."
                } else {
                    "Перечисленные черновики будут потеряны. Вернитесь к редактированию, чтобы сохранить их."
                }))
                .child(body)
                .child(div().mt(px(8.)).text_size(px(12.)).text_color(rgb(colors.muted))
                    .child("Закрывается только интерфейс. Команда остановки CLI в session host не отправляется."))
                .child(buttons)
                .child(div().text_size(px(11.)).text_color(rgb(colors.muted)).child(if flushing {
                    "Escape — вернуться"
                } else { "Escape / Enter — вернуться · Tab — выбрать · Ctrl+Enter — закрыть" })))
            .into_any_element())
    }
}

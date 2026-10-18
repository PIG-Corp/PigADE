//! Explicit insertion into one frozen terminal generation. Preparation never
//! sends input; confirmation uses the existing bounded, non-retrying worker.
use crate::{
    CanvasApp, CommandTarget,
    product::{Dialog, DialogKind},
    terminal_insert::{self, Origin, Review},
};
use gpui::{Context, DragMoveEvent, ExternalPaths, Pixels, Point, Window};

impl CanvasApp {
    fn file_target_at(&self, position: Point<Pixels>) -> Option<usize> {
        if self.product.dialog.is_some()
            || self.pane_drag.is_some()
            || self.resize_gesture.is_some()
            || self.canvas_drag.is_some()
            || self.selection_dragging
            || self
                .pointer
                .as_ref()
                .is_some_and(|route| route.encoder.has_pressed())
            || !self.text_input.preedit.is_empty()
            || !self
                .canvas_bounds
                .is_some_and(|bounds| bounds.contains(&position))
        {
            return None;
        }
        self.panes.iter().enumerate().find_map(|(index, pane)| {
            (pane.bounds.size.width > gpui::px(0.)
                && pane.bounds.size.height > gpui::px(0.)
                && pane.bounds.contains(&position)
                && self
                    .state
                    .views
                    .get(index)
                    .is_some_and(|view| self.target_can_send(view)))
            .then_some(index)
        })
    }

    pub(super) fn external_files_over(
        &mut self,
        event: &DragMoveEvent<ExternalPaths>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.close_flow.active() {
            return;
        }
        let started = !self.external_file_drag;
        if started {
            self.input_epoch = self.input_epoch.wrapping_add(1);
        }
        self.external_file_drag = true;
        let paths = event.drag(cx).paths();
        let target = (1..=32)
            .contains(&paths.len())
            .then(|| self.file_target_at(event.event.position))
            .flatten()
            .and_then(|index| self.state.views.get(index))
            .map(|view| CommandTarget {
                session: view.session.clone(),
                generation: view.generation.clone(),
            });
        if started || self.file_drop_target != target {
            self.file_drop_target = target;
            cx.notify();
        }
    }

    pub(super) fn clear_external_files(&mut self, cx: &mut Context<Self>) {
        if self.external_file_drag || self.file_drop_target.is_some() {
            self.external_file_drag = false;
            self.file_drop_target = None;
            cx.notify();
        }
    }

    pub(super) fn drop_external_files(
        &mut self,
        paths: &ExternalPaths,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.close_flow.active() {
            return;
        }
        let index = self.file_target_at(window.mouse_position());
        self.clear_external_files(cx);
        if let Some(index) = index {
            self.prepare_file_insert(index, paths.paths(), window, cx);
        } else {
            self.input_notice =
                Some("Файлы не вставлены: выберите доступную работающую панель".into());
            cx.notify();
        }
        cx.stop_propagation();
    }

    pub(super) fn prepare_file_insert(
        &mut self,
        index: usize,
        paths: &[std::path::PathBuf],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if paths.is_empty() {
            self.input_notice =
                Some("Не удалось получить полный список файлов; вставка отменена".into());
            cx.notify();
            return;
        }
        match terminal_insert::file_paths(paths) {
            Ok(text) => self.prepare_terminal_insert(
                index,
                text,
                Origin::Files { count: paths.len() },
                window,
                cx,
            ),
            Err(reason) => {
                self.input_notice = Some(format!("Файлы не вставлены: {reason}"));
                cx.notify();
            }
        }
    }

    pub(super) fn prepare_terminal_insert(
        &mut self,
        index: usize,
        text: String,
        origin: Origin,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.product.dialog.is_some()
            || self.external_file_drag
            || self.pane_drag.is_some()
            || self.resize_gesture.is_some()
            || self.canvas_drag.is_some()
            || self.selection_dragging
            || self
                .pointer
                .as_ref()
                .is_some_and(|route| route.encoder.has_pressed())
            || !self.text_input.preedit.is_empty()
        {
            self.input_notice = Some("Завершите текущее действие перед вставкой".into());
            cx.notify();
            return;
        }
        let Some(view) = self
            .state
            .views
            .get(index)
            .filter(|view| self.target_can_send(view))
        else {
            self.input_notice = Some("Панель недоступна для вставки".into());
            cx.notify();
            return;
        };
        let target = CommandTarget {
            session: view.session.clone(),
            generation: view.generation.clone(),
        };
        let review = match Review::new(
            target,
            self.product.pane_title(view).to_owned(),
            text,
            origin,
            view.frame.modes,
        ) {
            Ok(review) => review,
            Err(reason) => {
                self.input_notice = Some(reason);
                cx.notify();
                return;
            }
        };
        self.insert_return_target = self
            .state
            .views
            .get(self.selected)
            .map(|view| CommandTarget {
                session: view.session.clone(),
                generation: view.generation.clone(),
            });
        self.selected = index;
        self.selection_dragging = false;
        self.selection = None;
        self.input_epoch = self.input_epoch.wrapping_add(1);
        self.text_input.cancel();
        self.product.notice = None;
        self.product.insert_review = Some(review);
        self.product
            .insert_scroll
            .set_offset(gpui::point(gpui::px(0.), gpui::px(0.)));
        self.product.dialog = Some(Dialog::new(DialogKind::TerminalInsert));
        self.focus.focus(window, cx);
        self.sync_editor_scope(window, cx);
        cx.notify();
    }

    pub(super) fn cancel_terminal_insert(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.deferred_product_action = None;
        self.product.dialog = None;
        self.product.insert_review = None;
        if let Some(target) = self.insert_return_target.take()
            && let Some(index) = self.state.views.iter().position(|view| {
                view.session == target.session && view.generation == target.generation
            })
        {
            self.selected = index;
        }
        self.input_epoch = self.input_epoch.wrapping_add(1);
        self.text_input.cancel();
        self.sync_editor_scope(window, cx);
        cx.notify();
    }

    pub(super) fn confirm_terminal_insert(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self
            .product
            .dialog
            .as_ref()
            .is_some_and(|dialog| dialog.kind == DialogKind::TerminalInsert)
        {
            return;
        }
        let Some(review) = self.product.insert_review.as_ref() else {
            return;
        };
        let candidate = self
            .state
            .views
            .get(self.selected)
            .filter(|view| self.target_can_send(view));
        let bytes = match candidate
            .map(|view| review.encode(view))
            .unwrap_or_else(|| Err("Панель недоступна для вставки".into()))
        {
            Ok(bytes) => bytes,
            Err(reason) => {
                self.product.notice = Some(format!(
                    "Вставка не отправлена: {reason}. Закройте проверку и подготовьте её снова."
                ));
                cx.notify();
                return;
            }
        };
        if self.send_to(self.selected, bytes, cx) {
            self.product.dialog = None;
            self.product.insert_review = None;
            self.insert_return_target = None;
            self.input_epoch = self.input_epoch.wrapping_add(1);
            self.input_notice =
                Some("Текст поставлен в очередь ввода. Enter отдельно не добавлен.".into());
            self.sync_editor_scope(window, cx);
            self.persist_focus(cx);
        } else {
            self.product.notice = Some(
                self.input_notice
                    .clone()
                    .unwrap_or_else(|| "Очередь ввода недоступна; вставка не принята".into()),
            );
        }
        cx.notify();
    }

    pub(super) fn paste_clipboard_files(
        &mut self,
        item: &gpui::ClipboardItem,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.product.dialog.is_some() {
            return false;
        }
        let has_paths = item
            .entries()
            .iter()
            .any(|entry| matches!(entry, gpui::ClipboardEntry::ExternalPaths(_)));
        if !has_paths {
            return false;
        }
        let mut bytes = 0usize;
        let mut paths = Vec::new();
        for path in item
            .entries()
            .iter()
            .filter_map(|entry| match entry {
                gpui::ClipboardEntry::ExternalPaths(paths) => Some(paths.paths()),
                _ => None,
            })
            .flatten()
            .take(33)
        {
            bytes = bytes.saturating_add(path.as_os_str().len());
            if paths.len() == 32 || bytes > 16 * 1024 {
                self.input_notice =
                    Some("Не более 32 файлов и 16 KiB путей за одну вставку".into());
                cx.notify();
                return true;
            }
            paths.push(path.clone());
        }
        self.prepare_file_insert(self.selected, &paths, window, cx);
        true
    }
}

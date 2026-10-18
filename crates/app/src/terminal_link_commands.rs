//! Explicit, frozen OSC 8 link review. No terminal input or host effects.
use crate::{
    CanvasApp,
    product::{Dialog, DialogKind, ProductAction as A},
    protocol::View,
    terminal_links::{self, SheetStage},
};
use gpui::{ClipboardItem, Context};

impl CanvasApp {
    fn displayed_link_source(&self) -> Option<&View> {
        let view = self.state.views.get(self.selected)?;
        if self.history_target(view) {
            self.scrollback_display.as_ref().filter(|history| {
                history.session == view.session && history.generation == view.generation
            })
        } else {
            Some(view)
        }
    }

    pub(crate) fn terminal_link_action(&mut self, action: A, cx: &mut Context<Self>) {
        if action == A::OpenTerminalLinks {
            if self.product.busy {
                self.product.notice = Some("Дождитесь завершения текущей операции".into());
                cx.notify();
                return;
            }
            let dialog = Dialog::new(DialogKind::TerminalLinks);
            let captured = self
                .displayed_link_source()
                .ok_or("Выберите панель с доступным экраном")
                .and_then(|view| {
                    terminal_links::capture(view, dialog.id)
                        .map_err(|_| "Ссылки этого экрана недоступны: некорректные метаданные")
                });
            match captured {
                Ok(sheet) => {
                    self.product.terminal_links = Some(sheet);
                    self.product.dialog = Some(dialog);
                    self.product.notice = None;
                    self.product
                        .terminal_links_scroll
                        .set_offset(gpui::point(gpui::px(0.), gpui::px(0.)));
                }
                Err(error) => self.product.notice = Some(error.into()),
            }
            cx.notify();
            return;
        }
        let Some(dialog) = self
            .product
            .dialog
            .as_ref()
            .filter(|dialog| dialog.kind == DialogKind::TerminalLinks)
            .map(|dialog| dialog.id)
        else {
            return;
        };
        if !self
            .product
            .terminal_links
            .as_ref()
            .is_some_and(|sheet| sheet.dialog_id == dialog)
        {
            return;
        }
        if action == A::RefreshTerminalLinks {
            let sheet = self
                .product
                .terminal_links
                .as_ref()
                .expect("validated sheet");
            if sheet.stage != SheetStage::List {
                return;
            }
            let source = sheet.source.clone();
            let captured = self
                .displayed_link_source()
                .filter(|view| {
                    view.session == source.session.to_string()
                        && view.generation == source.generation.to_string()
                        && (view.state == "history") == (source.state == "history")
                })
                .ok_or(
                    "Показанная панель или поколение изменились. Закройте список и откройте снова.",
                )
                .and_then(|view| {
                    terminal_links::capture(view, dialog)
                        .map_err(|_| "Ссылки обновлённого экрана недоступны")
                });
            match captured {
                Ok(sheet) => {
                    self.product.terminal_links = Some(sheet);
                    self.product.notice = None;
                    self.product
                        .terminal_links_scroll
                        .set_offset(gpui::point(gpui::px(0.), gpui::px(0.)));
                }
                Err(error) => self.product.notice = Some(error.into()),
            }
            cx.notify();
            return;
        }
        let sheet = self
            .product
            .terminal_links
            .as_mut()
            .expect("validated sheet");
        match action {
            A::SelectTerminalLink(index) => {
                if sheet.select(index) {
                    self.product.terminal_links_scroll.scroll_to_item(index);
                    self.product.notice = None;
                }
            }
            A::ReviewTerminalLink => {
                if sheet.enter_review() {
                    self.product.notice = None;
                    self.product
                        .terminal_link_address_scroll
                        .set_offset(gpui::point(gpui::px(0.), gpui::px(0.)));
                }
            }
            A::BackTerminalLinks => {
                if sheet.back_to_list() {
                    self.product.notice = None;
                    self.product
                        .terminal_links_scroll
                        .scroll_to_item(sheet.selected);
                }
            }
            A::CopyTerminalLink => {
                if let Some(target) = sheet.copy_target(dialog) {
                    cx.write_to_clipboard(ClipboardItem::new_string(target.into()));
                    self.product.notice = Some("Адрес скопирован".into());
                }
            }
            A::ConfirmTerminalLink => {
                if let Some(target) = sheet.confirm_once(dialog) {
                    // The only external effect: a fresh explicit confirmation
                    // consumes the immutable reviewed destination exactly once.
                    cx.open_url(target.as_str());
                    self.product.terminal_links = None;
                    self.product.dialog = None;
                    self.product.notice = Some("Запрошено открытие ссылки в браузере".into());
                }
            }
            _ => (),
        }
        cx.notify();
    }
}

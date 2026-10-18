//! Appearance belongs to this app, independently of terminal/host state.
use super::*;
use product::{Dialog, DialogKind, ProductAction as A};

impl CanvasApp {
    pub(super) fn appearance_action(
        &mut self,
        action: A,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if action == A::OpenAppearance {
            self.prefix = false;
            self.product.dialog = Some(Dialog::new(DialogKind::Appearance));
            cx.notify();
            return;
        }
        if self.product.appearance_pending.is_some() {
            return;
        }
        let current = self.product.appearance.clone();
        let reload = action == A::ReloadAppearance;
        let font_size = match action {
            A::AdjustTerminalFont(delta) => current
                .terminal_font_size
                .saturating_add_signed(delta)
                .clamp(
                    appearance::MIN_TERMINAL_FONT_SIZE,
                    appearance::MAX_TERMINAL_FONT_SIZE,
                ),
            A::ResetTerminalFont => appearance::DEFAULT_TERMINAL_FONT_SIZE,
            _ => current.terminal_font_size,
        };
        let (theme, motion) = match action {
            A::SetTheme(theme) => (theme, current.reduced_motion),
            A::ToggleReducedMotion => (current.theme, !current.reduced_motion),
            A::ReloadAppearance | A::AdjustTerminalFont(_) | A::ResetTerminalFont => {
                (current.theme, current.reduced_motion)
            }
            _ => return,
        };
        if !reload
            && theme == current.theme
            && motion == current.reduced_motion
            && font_size == current.terminal_font_size
        {
            if self.product.dialog.is_none() {
                self.product.notice = Some(format!(
                    "Шрифт терминала · {font_size} px · диапазон 10–24 px"
                ));
            }
            return;
        }
        let request = uuid::Uuid::new_v4();
        self.product.appearance_pending = Some(request);
        self.product.appearance_error = None;
        let result = cx.background_spawn(async move {
            if reload {
                appearance::load()
            } else {
                appearance::save(current.revision, theme, motion, font_size)
            }
        });
        cx.spawn_in(window, async move |weak,cx| {
            let result=result.await;
            let _=weak.update_in(cx,|this,window,cx| {
                if this.product.appearance_pending!=Some(request) { return; }
                this.product.appearance_pending=None;
                match result {
                    Ok(config)=> {
                        if this.product.appearance.terminal_font_size != config.terminal_font_size {
                            this.cancel_pane_drag(cx);
                            this.finish_resize(false,cx);
                            if let Some(drag)=this.canvas_drag.take() { this.set_canvas_offset(drag.initial); }
                            this.finish_pointer(cx);
                            this.selection_dragging=false;
                            this.selection=None;
                            this.text_input.cancel();
                            window.notify_text_input_changed(&this.focus);
                            this.input_epoch=this.input_epoch.wrapping_add(1);
                            this.canvas_reveal=this.selected_pane();
                            if this.product.dialog.is_none() {
                                this.product.notice=Some(format!("Шрифт терминала · {} px",config.terminal_font_size));
                            }
                        }
                        this.product.appearance=config;
                        cx.set_reduce_motion(this.product.appearance.reduced_motion || this.product.motion_forced);
                    }
                    Err(error)=> {
                        let message=format!("Не удалось подтвердить настройку: {error}. Обновите состояние перед повторным сохранением.");
                        if this.product.dialog.is_none() { this.product.notice=Some(message.clone()); }
                        this.product.appearance_error=Some(message);
                    },
                }
                cx.notify();
            });
        }).detach();
        cx.notify();
    }
}

//! Appearance belongs to this app, independently of terminal/host state.
use super::*;
use product::{Dialog, DialogKind, ProductAction as A};

impl CanvasApp {
    pub(super) fn appearance_action(&mut self, action: A, cx: &mut Context<Self>) {
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
        let (theme, motion) = match action {
            A::SetTheme(theme) => (theme, current.reduced_motion),
            A::ToggleReducedMotion => (current.theme, !current.reduced_motion),
            A::ReloadAppearance => (current.theme, current.reduced_motion),
            _ => return,
        };
        let request = uuid::Uuid::new_v4();
        self.product.appearance_pending = Some(request);
        self.product.appearance_error = None;
        let result = cx.background_spawn(async move {
            if reload {
                appearance::load()
            } else {
                appearance::save(current.revision, theme, motion)
            }
        });
        cx.spawn(async move |weak,cx| {
            let result=result.await;
            let _=weak.update(cx,|this,cx| {
                if this.product.appearance_pending!=Some(request) { return; }
                this.product.appearance_pending=None;
                match result {
                    Ok(config)=> {
                        this.product.appearance=config;
                        cx.set_reduce_motion(this.product.appearance.reduced_motion || this.product.motion_forced);
                    }
                    Err(error)=>this.product.appearance_error=Some(format!("Не удалось подтвердить настройку: {error}. Обновите состояние перед повторным сохранением.")),
                }
                cx.notify();
            });
        }).detach();
        cx.notify();
    }
}

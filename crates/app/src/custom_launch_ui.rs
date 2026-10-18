//! A custom command is reviewed as an exact executable and argument vector.
use super::*;

impl ProductUi {
    pub(crate) fn custom_launch_ready(&self) -> bool {
        self.catalog
            .as_ref()
            .and_then(|catalog| catalog.custom_cli_schema)
            == Some(1)
            && !self.busy
            && self.launch_phase.is_none()
            && self.launch_preview.as_ref().is_some_and(|preview| {
                preview.provider == "custom"
                    && self
                        .catalog
                        .as_ref()
                        .and_then(|catalog| catalog.selection.tab_id)
                        == Some(preview.tab_id)
                    && preview.can_launch
                    && preview.resume.is_none()
                    && !preview.deliver_context
                    && !preview.include_memory
                    && preview.engine.is_empty()
                    && preview.delivery_method == "none"
                    && !preview.capabilities.startup_context
                    && !preview.capabilities.context_receipt
                    && !preview.capabilities.native_resume
                    && !preview.capabilities.conversation_id
                    && !preview.capabilities.model_selection
                    && !preview.capabilities.reasoning_effort
                    && preview.custom_command.as_ref().is_some_and(|command| {
                        command.validate().is_ok()
                            && preview.executable.as_ref().and_then(|path| path.to_str())
                                == Some(command.executable.as_str())
                    })
            })
    }

    pub(super) fn custom_launch_overlay(&self, cx: &mut Context<CanvasApp>) -> AnyElement {
        let colors = self.colors();
        let mut details = div().id("custom-launch-details").flex().flex_col().gap_2();
        if let Some(preview) = &self.launch_preview {
            if let Some(profile) = &preview.profile {
                details = details.child(format!("{} · ревизия {}", profile.name, profile.revision));
            }
            details = details.child(
                div()
                    .text_size(px(11.))
                    .text_color(colors.muted)
                    .whitespace_normal()
                    .child(format!(
                        "Рабочая папка: {}",
                        preview
                            .cwd
                            .as_ref()
                            .map(|path| path.display().to_string())
                            .unwrap_or_else(|| "Недоступна".into())
                    )),
            );
            if let Some(command) = &preview.custom_command {
                details = details.child(
                    div()
                        .text_size(px(12.))
                        .whitespace_normal()
                        .child(command.executable.clone()),
                );
                let mut arguments = div()
                    .id("custom-launch-arguments")
                    .max_h(px(220.))
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .p_2()
                    .bg(colors.base)
                    .rounded_md();
                if command.args.is_empty() {
                    arguments = arguments.child("Без аргументов");
                }
                for (index, argument) in command.args.iter().enumerate() {
                    arguments = arguments.child(
                        div()
                            .text_size(px(12.))
                            .whitespace_normal()
                            .child(format!("{} · {argument:?}", index + 1)),
                    );
                }
                details = details.child(arguments);
            }
            if let Some(reason) = &preview.unavailable_reason {
                details = details.child(
                    div()
                        .id("custom-launch-error")
                        .role(Role::Alert)
                        .whitespace_normal()
                        .text_color(colors.status_amber)
                        .child(reason.clone()),
                );
            }
        }
        let mut controls = div()
            .flex()
            .justify_end()
            .gap_2()
            .child(self.action_control(
                "custom-launch-cancel".into(),
                "Отмена".into(),
                ProductAction::CloseDialog,
                ControlStyle {
                    role: Role::Button,
                    selected: false,
                    compact: true,
                },
                cx,
            ));
        if self.custom_launch_ready() {
            controls = controls.child(self.action_control(
                "custom-launch-confirm".into(),
                "Запустить · Ctrl+Enter".into(),
                ProductAction::SubmitDialog,
                ControlStyle {
                    role: Role::Button,
                    selected: true,
                    compact: true,
                },
                cx,
            ));
        }
        div().id("custom-launch-overlay").role(Role::Dialog).aria_label("Запустить CLI")
            .absolute().inset_0().flex().items_start().justify_center().overflow_y_scroll()
            .py(px(24.)).bg(rgb(0x090b0e).opacity(0.72))
            .child(div().id("custom-launch-review").w_full().max_w(px(560.)).mx(px(16.))
                .p_4().rounded_lg().border_1().border_color(colors.border).bg(colors.raised)
                .text_color(colors.text).flex().flex_col().gap_3()
                .child(div().id("custom-launch-heading").role(Role::Heading).text_size(px(18.)).child("Запустить CLI"))
                .child(details)
                .child(div().text_size(px(11.)).text_color(colors.muted).whitespace_normal()
                    .child("Аргументы передаются программе по отдельности. Shell-подстановки не выполняются. Контекст PigADE не отправляется; resume не поддерживается."))
                .children(self.notice.clone().map(|notice|div().id("custom-launch-notice").role(Role::Status).whitespace_normal().child(notice)))
                .child(controls)).into_any_element()
    }
}

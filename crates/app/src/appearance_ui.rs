//! Native local appearance settings; terminal colors are a separate profile.
use super::*;

impl ProductUi {
    pub(super) fn appearance_fields(&self, mut fields: Div, cx: &mut Context<CanvasApp>) -> Div {
        let colors = self.colors();
        let mut themes = div().flex().gap_2();
        for (theme, label) in [
            (crate::appearance::Theme::Dark, "Тёмная · 1"),
            (crate::appearance::Theme::Light, "Светлая · 2"),
        ] {
            themes = themes.child(self.action_control(
                format!("appearance-theme-{theme:?}"),
                label.into(),
                ProductAction::SetTheme(theme),
                ControlStyle {
                    role: Role::Button,
                    selected: self.appearance.theme == theme,
                    compact: false,
                },
                cx,
            ));
        }
        fields = fields
            .child(themes)
            .child(self.action_control(
                "appearance-reduced-motion".into(),
                format!(
                    "{} Меньше движения · R",
                    if self.appearance.reduced_motion {
                        "●"
                    } else {
                        "○"
                    }
                ),
                ProductAction::ToggleReducedMotion,
                ControlStyle {
                    role: Role::CheckBox,
                    selected: self.appearance.reduced_motion,
                    compact: false,
                },
                cx,
            ))
            .child(
                div()
                    .text_size(px(11.))
                    .text_color(colors.muted)
                    .whitespace_normal()
                    .child("Тема меняет интерфейс. Палитра CLI остаётся независимой."),
            );
        let mut font_controls = div().flex().items_center().gap_2();
        for (id, label, name, action) in [
            (
                "appearance-font-smaller",
                "−",
                "Уменьшить шрифт терминала · −",
                ProductAction::AdjustTerminalFont(-1),
            ),
            (
                "appearance-font-larger",
                "+",
                "Увеличить шрифт терминала · +",
                ProductAction::AdjustTerminalFont(1),
            ),
            (
                "appearance-font-reset",
                "14 px · 0",
                "Сбросить размер шрифта терминала до 14 px · 0",
                ProductAction::ResetTerminalFont,
            ),
        ] {
            font_controls = font_controls.child(self.action_control_with_accessible_name(
                id.into(),
                label.into(),
                name.into(),
                action,
                ControlStyle {
                    role: Role::Button,
                    selected: false,
                    compact: true,
                },
                cx,
            ));
        }
        fields = fields
            .child(
                div()
                    .id("appearance-font-value")
                    .role(Role::Status)
                    .text_size(px(12.))
                    .child(format!(
                        "Шрифт терминала · {} px",
                        self.appearance.terminal_font_size
                    )),
            )
            .child(font_controls)
            .child(
                div()
                    .text_size(px(11.))
                    .text_color(colors.muted)
                    .whitespace_normal()
                    .child("10–24 px. Масштаб интерфейса PigADE не меняется."),
            );
        if self.motion_forced {
            fields = fields.child(
                div()
                    .text_size(px(11.))
                    .text_color(colors.muted)
                    .child("Reduced motion также включён параметром запуска."),
            );
        }
        let status = if self.appearance_pending.is_some() {
            "Сохраняем настройку…".into()
        } else if self.appearance.revision == 0 {
            "Настройки по умолчанию".into()
        } else {
            format!("Сохранено · ревизия {}", self.appearance.revision)
        };
        fields = fields.child(
            div()
                .id("appearance-status")
                .role(Role::Status)
                .text_size(px(11.))
                .text_color(colors.muted)
                .child(status),
        );
        if let Some(error) = &self.appearance_error {
            fields = fields.child(
                div()
                    .id("appearance-error")
                    .role(Role::Alert)
                    .text_size(px(12.))
                    .whitespace_normal()
                    .child(error.clone()),
            );
        }
        fields.child(self.action_control(
            "appearance-reload".into(),
            "Обновить настройки · Ctrl+R".into(),
            ProductAction::ReloadAppearance,
            ControlStyle {
                role: Role::Button,
                selected: false,
                compact: true,
            },
            cx,
        ))
    }
}

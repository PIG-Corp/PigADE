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
            .child(div().text_size(px(12.)).child("Интерфейс"))
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
                    .child("Интерфейс и терминал настраиваются отдельно."),
            );
        let mut terminal_themes = div().flex().gap_2();
        for (theme, label) in [
            (crate::appearance::TerminalTheme::Dark, "Тёмная · 3"),
            (crate::appearance::TerminalTheme::Light, "Светлая · 4"),
            (
                crate::appearance::TerminalTheme::FollowApp,
                "Как интерфейс · 5",
            ),
        ] {
            terminal_themes = terminal_themes.child(self.action_control(
                format!("appearance-terminal-{theme:?}"),
                label.into(),
                ProductAction::SetTerminalTheme(theme),
                ControlStyle {
                    role: Role::Button,
                    selected: self.appearance.terminal_theme == theme,
                    compact: false,
                },
                cx,
            ));
        }
        let profile = match self
            .appearance
            .terminal_theme
            .resolved(self.appearance.theme)
        {
            crate::appearance::Theme::Dark => crate::palette::Profile::Dark,
            crate::appearance::Theme::Light => crate::palette::Profile::Light,
        };
        let mut sample = div()
            .id("appearance-terminal-sample")
            .role(Role::Status)
            .bg(rgb(profile.background()))
            .text_color(rgb(profile.foreground()))
            .rounded_md()
            .p_2()
            .flex()
            .gap_3()
            .text_size(px(12.))
            .child("Образец · Привет");
        for (label, color) in [
            ("Red", "Named(Red)"),
            ("Green", "Named(Green)"),
            ("Blue", "Named(Blue)"),
        ] {
            sample = sample.child(
                div()
                    .text_color(rgb(crate::palette::color_for(profile, color, true, &[])))
                    .child(label),
            );
        }
        fields = fields
            .child(div().text_size(px(12.)).child("Терминал"))
            .child(terminal_themes)
            .child(sample)
            .child(div().text_size(px(11.)).text_color(colors.muted).whitespace_normal()
                .child("Палитра задаёт цвета по умолчанию. Явные RGB и цвета, заданные CLI, сохраняются."));
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

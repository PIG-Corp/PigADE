//! Compact workspace switcher rows backed by the host's cached navigation view.
use super::*;

const WORKSPACE_PAGE_SIZE: usize = crate::workspace_navigation::PAGE;

impl ProductUi {
    pub(super) fn workspace_switcher_fields(
        &self,
        mut fields: Div,
        cx: &mut Context<CanvasApp>,
    ) -> Div {
        let colors = self.colors();
        let navigation = &self.workspace_navigation;
        let start = navigation.offset.min(navigation.rows.len());
        let end = start
            .saturating_add(WORKSPACE_PAGE_SIZE)
            .min(navigation.rows.len());
        let current = self
            .catalog
            .as_ref()
            .and_then(|catalog| catalog.selection.workspace_id);

        let mut list = div()
            .id("workspace-switcher-list")
            .role(Role::ListBox)
            .flex()
            .flex_col()
            .gap_1();
        let mut recent_section: Option<bool> = None;
        for (index, row) in navigation.rows[start..end].iter().enumerate() {
            if recent_section != Some(row.recent) {
                recent_section = Some(row.recent);
                list = list.child(
                    div()
                        .id(format!("workspace-switcher-section-{index}"))
                        .text_size(px(11.))
                        .text_color(colors.muted)
                        .mt(px(if index == 0 { 2. } else { 7. }))
                        .child(if row.recent {
                            "Недавние"
                        } else {
                            "Все воркспейсы"
                        }),
                );
            }

            let is_current = current == Some(row.workspace);
            let unread = self.workspace_activity(row.workspace);
            let status = workspace_switcher_status(row.status);
            let current_marker = if is_current { " · текущий" } else { "" };
            let unread_marker = if unread > 0 {
                format!(" · непрочитано: {unread}")
            } else {
                String::new()
            };
            let status_suffix = status.map_or_else(String::new, |label| format!(" · {label}"));
            let accessible_name = format!(
                "{} · {}{}{}{}",
                row.name, row.detail, current_marker, status_suffix, unread_marker
            );
            let label = format!(
                "{}{}{}{}",
                if is_current { "● " } else { "" },
                row.name,
                status_suffix,
                unread_marker
            );
            list = list.child(
                div()
                    .flex()
                    .flex_col()
                    .gap_0()
                    .child(self.action_control_with_accessible_name(
                        format!("workspace-switcher-{}", row.workspace),
                        label,
                        accessible_name,
                        // The host owns the ready/non-ready decision: choosing
                        // a non-ready workspace routes to its management view.
                        ProductAction::ChooseWorkspace(row.workspace),
                        ControlStyle {
                            role: Role::ListBoxOption,
                            selected: navigation.selected == Some(row.workspace),
                            compact: false,
                        },
                        cx,
                    ))
                    .child(
                        div()
                            .px(px(10.))
                            .pb(px(3.))
                            .text_size(px(10.))
                            .text_color(colors.muted)
                            .truncate()
                            .child(row.detail.clone()),
                    ),
            );
        }

        if navigation.rows.is_empty() {
            let message = if self
                .catalog
                .as_ref()
                .is_some_and(|catalog| catalog.workspaces.is_empty())
            {
                "Воркспейсов пока нет"
            } else {
                "По запросу ничего не найдено"
            };
            list = list.child(
                div()
                    .id("workspace-switcher-empty")
                    .role(Role::Status)
                    .py(px(12.))
                    .text_size(px(12.))
                    .text_color(colors.muted)
                    .child(message),
            );
        }

        fields = fields.child(list);

        let mut footer = div()
            .id("workspace-switcher-footer")
            .flex()
            .items_center()
            .justify_between()
            .gap_2()
            .text_size(px(10.))
            .text_color(colors.muted);
        let count = if navigation.rows.is_empty() {
            "0 воркспейсов".to_owned()
        } else {
            format!("{}–{} из {}", start + 1, end, navigation.rows.len())
        };
        footer = footer.child(div().child(count));

        let mut paging = div().flex().items_center().gap_1();
        if start > 0 {
            paging = paging.child(self.action_control_with_accessible_name(
                "workspace-switcher-previous".into(),
                "← Назад".into(),
                "Предыдущие воркспейсы · Page Up".into(),
                ProductAction::WorkspacePage(-1),
                ControlStyle {
                    role: Role::Button,
                    selected: false,
                    compact: true,
                },
                cx,
            ));
        }
        if end < navigation.rows.len() {
            paging = paging.child(self.action_control_with_accessible_name(
                "workspace-switcher-next".into(),
                "Далее →".into(),
                "Следующие воркспейсы · Page Down".into(),
                ProductAction::WorkspacePage(1),
                ControlStyle {
                    role: Role::Button,
                    selected: false,
                    compact: true,
                },
                cx,
            ));
        }
        footer = footer.child(paging);
        fields = fields.child(footer);

        fields = fields.child(
            div()
                .id("workspace-switcher-keys")
                .text_size(px(10.))
                .text_color(colors.muted)
                .child("↑ ↓ — выбрать · Enter — открыть · Page Up/Down — страница · Ctrl+R — обновить · Alt+←/→ — свернуть/развернуть проект · Esc — закрыть"),
        );

        fields = fields.child(
            div()
                .id("workspace-switcher-save-state")
                .role(Role::Status)
                .text_size(px(10.))
                .text_color(colors.muted)
                .child(if navigation.error.is_some() {
                    "Сохранение настроек не подтверждено"
                } else if navigation.saving {
                    "Сохраняем последние вкладки и дерево…"
                } else {
                    "Последние вкладки и дерево сохранены"
                }),
        );
        if let Some(error) = &navigation.error {
            fields = fields.child(
                div()
                    .id("workspace-switcher-error")
                    .role(Role::Alert)
                    .text_size(px(11.))
                    .whitespace_normal()
                    .child(error.clone()),
            );
        }
        fields.child(self.action_control(
            "workspace-switcher-refresh".into(),
            "Обновить настройки · Ctrl+R".into(),
            ProductAction::RefreshWorkspaceNavigation,
            ControlStyle {
                role: Role::Button,
                selected: false,
                compact: true,
            },
            cx,
        ))
    }
}

fn workspace_switcher_status(status: WorkspaceStatus) -> Option<&'static str> {
    match status {
        WorkspaceStatus::Ready => None,
        WorkspaceStatus::Preparing => Some("подготавливается · открыть управление"),
        WorkspaceStatus::Failed => Some("ошибка · открыть управление"),
        WorkspaceStatus::Archived => Some("архив · открыть управление"),
        WorkspaceStatus::MissingPath => Some("папка недоступна · открыть управление"),
    }
}

//! Explicit workspace recovery overview; snapshots never impersonate live CLI.
use super::*;
use protocol::{WorkspaceResumeState as Resume, WorkspaceSessionEntry};

fn overview_text(value: &str) -> String {
    let mut text = String::with_capacity(value.len());
    for ch in value.chars() {
        if ch.is_control() {
            text.extend(ch.escape_default());
        } else {
            text.push(ch);
        }
    }
    text
}

fn session_state(state: &str) -> &'static str {
    match state {
        "running" => "Работает",
        "degraded" => "Ошибка терминала",
        "exited" => "CLI завершён",
        "history" | "historical" => "Сохранённая история",
        "history_unavailable" => "Модель истории недоступна",
        "launch_outcome_unknown" => "Исход запуска неизвестен",
        _ => "Состояние не подтверждено",
    }
}

fn resume_note(entry: &WorkspaceSessionEntry) -> &'static str {
    match entry.resume_state {
        Resume::CheckAvailable => {
            "Можно проверить продолжение привязанного диалога. CLI и его возможности будут проверены перед отдельным подтверждением."
        }
        Resume::NeedsBinding => {
            "Сначала откройте панель и сохраните точный ID в меню «Диалог CLI». Последний диалог автоматически не выбирается."
        }
        Resume::Running => {
            "Процесс продолжает работать. Откройте панель, чтобы подключиться к нему."
        }
        Resume::Unknown => {
            "Исход предыдущего запуска неизвестен. Новый процесс не запускается; сначала проверьте исходную сессию."
        }
        Resume::PinnedHistory => {
            "В панели выбрано историческое поколение. Откройте панель и выберите «Текущее» в меню поколений, чтобы продолжить текущий диалог."
        }
        Resume::Unsupported => {
            "Native resume для этого типа CLI не предусмотрен. Сохранённую историю можно открыть в панели."
        }
        Resume::Unavailable => {
            "Для безопасного продолжения не хватает подтверждённых метаданных. Сохранённая панель остаётся доступна."
        }
    }
}

impl ProductUi {
    pub(super) fn workspace_sessions_overlay(&self, cx: &mut Context<CanvasApp>) -> AnyElement {
        let colors = self.colors();
        let Some(sheet) = self.workspace_sessions.sheet.as_ref().filter(|sheet| {
            self.dialog.as_ref().is_some_and(|dialog| {
                dialog.id == sheet.dialog && dialog.kind == DialogKind::WorkspaceSessions
            })
        }) else {
            return div()
                .id("workspace-sessions-unavailable")
                .role(Role::Status)
                .child("Обзор сессий недоступен")
                .into_any_element();
        };
        let busy = self.busy || self.workspace_sessions.request_pending();
        let current = sheet.target.current(self.catalog.as_ref());
        let actionable = !busy && current && sheet.error.is_none();
        let name = self
            .catalog
            .as_ref()
            .and_then(|catalog| {
                catalog
                    .workspaces
                    .iter()
                    .find(|workspace| workspace.id == sheet.target.workspace)
            })
            .map(|workspace| workspace.task.as_str())
            .unwrap_or("Воркспейс");
        let mut rows = div()
            .id("workspace-sessions-rows")
            .role(Role::List)
            .aria_label("Панели всех вкладок воркспейса")
            .flex()
            .flex_col()
            .gap_1()
            .min_h(px(0.))
            .max_h(px(330.))
            .overflow_y_scroll()
            .track_scroll(&self.workspace_sessions_scroll);
        if let Some(page) = &sheet.page {
            if page.entries.is_empty() {
                rows = rows.child(
                    div()
                        .id("workspace-sessions-empty")
                        .role(Role::Status)
                        .text_size(px(12.))
                        .text_color(colors.muted)
                        .child("В этом воркспейсе пока нет размещённых сессий."),
                );
            }
            for (index, entry) in page.entries.iter().enumerate() {
                let selected = index == sheet.selected;
                let detail = format!(
                    "{} · {} · {}",
                    overview_text(&entry.tab_name),
                    entry.provider.as_deref().unwrap_or("CLI неизвестен"),
                    session_state(&entry.state)
                );
                rows = rows.child(
                    div()
                        .id(format!("workspace-session-{}", entry.session))
                        .role(Role::ListItem)
                        .aria_label(format!("{} · {detail}", overview_text(&entry.title)))
                        .px(px(10.))
                        .py(px(8.))
                        .rounded(px(7.))
                        .border_1()
                        .border_color(if selected {
                            colors.accent
                        } else {
                            colors.border
                        })
                        .bg(if selected {
                            colors.selected
                        } else {
                            colors.base
                        })
                        .cursor_pointer()
                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                            this.product_action(
                                ProductAction::SelectWorkspaceSession(index),
                                window,
                                cx,
                            )
                        }))
                        .child(
                            div()
                                .text_size(px(12.))
                                .truncate()
                                .child(overview_text(&entry.title)),
                        )
                        .child(
                            div()
                                .mt(px(3.))
                                .text_size(px(10.))
                                .text_color(colors.muted)
                                .whitespace_normal()
                                .child(detail),
                        ),
                );
            }
        }
        let button = |id: &str, label: &str, action: ProductAction, cx: &mut Context<CanvasApp>| {
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
        if !busy {
            controls = controls.child(button(
                "workspace-sessions-refresh",
                "Обновить",
                ProductAction::RefreshWorkspaceSessions,
                cx,
            ));
            if sheet.page.as_ref().is_some_and(|page| page.after.is_some()) {
                controls = controls.child(button(
                    "workspace-sessions-first",
                    "Первая",
                    ProductAction::FirstWorkspaceSessions,
                    cx,
                ));
            }
            if !sheet.previous_cursors.is_empty() {
                controls = controls.child(button(
                    "workspace-sessions-prev",
                    "Предыдущая",
                    ProductAction::WorkspaceSessionsPage(false),
                    cx,
                ));
            }
            if sheet
                .page
                .as_ref()
                .is_some_and(|page| page.next_after.is_some())
            {
                controls = controls.child(button(
                    "workspace-sessions-next",
                    "Следующая",
                    ProductAction::WorkspaceSessionsPage(true),
                    cx,
                ));
            }
        }
        let entry = self.workspace_sessions.selected_entry();
        if actionable && entry.is_some() {
            controls = controls.child(button(
                "workspace-sessions-open",
                "Открыть панель",
                ProductAction::OpenWorkspaceSession,
                cx,
            ));
        }
        if actionable && entry.is_some_and(|entry| entry.resume_state == Resume::CheckAvailable) {
            controls = controls.child(button(
                "workspace-sessions-resume",
                "Проверить продолжение…",
                ProductAction::PrepareWorkspaceSessionResume,
                cx,
            ));
        }
        if !busy {
            controls = controls.child(button(
                "workspace-sessions-unplaced",
                "Неразмещённые…",
                ProductAction::OpenSessions,
                cx,
            ));
        }
        controls = controls.child(button(
            "workspace-sessions-close",
            "Закрыть",
            ProductAction::CloseDialog,
            cx,
        ));
        let mut details = div()
            .id("workspace-session-details")
            .flex()
            .flex_col()
            .gap_1()
            .mt(px(12.))
            .text_size(px(11.))
            .text_color(colors.muted);
        if let Some(entry) = entry {
            details = details.child(div().whitespace_normal().child(resume_note(entry)));
            if let Some(conversation) = &entry.conversation {
                details = details.child(div().whitespace_normal().child(format!(
                    "Диалог {} · {} · привязка v{}",
                    conversation.provider,
                    overview_text(&conversation.conversation_id),
                    conversation.revision
                )));
            }
            if entry.head != entry.selected_generation {
                details = details.child(div().whitespace_normal().child(format!(
                    "Текущее поколение: {}",
                    session_state(&entry.head_state)
                )));
            }
        }
        if busy {
            details = details.child(
                div()
                    .id("workspace-sessions-loading")
                    .role(Role::Status)
                    .text_color(colors.accent)
                    .child("Проверяем состояние…"),
            );
        }
        if !current {
            details = details.child(
                div()
                    .id("workspace-sessions-stale")
                    .role(Role::Status)
                    .text_color(colors.status_amber)
                    .child("Воркспейс изменился. Закройте обзор и откройте снова."),
            );
        }
        if let Some(error) = &sheet.error {
            details = details.child(
                div()
                    .id("workspace-sessions-error")
                    .role(Role::Status)
                    .text_color(colors.status_amber)
                    .whitespace_normal()
                    .child(overview_text(error)),
            );
        }
        if let Some(notice) = &self.notice {
            details = details.child(
                div()
                    .id("workspace-sessions-notice")
                    .role(Role::Status)
                    .text_color(colors.status_amber)
                    .whitespace_normal()
                    .child(overview_text(notice)),
            );
        }
        div().id("workspace-sessions-overlay").absolute().inset_0().flex().items_start().justify_center().overflow_y_scroll().py(px(20.)).bg(rgb(0x090b0e).opacity(0.72))
            .on_click(cx.listener(|this,_:&ClickEvent,window,cx|this.product_action(ProductAction::CloseDialog,window,cx)))
            .child(div().id("workspace-sessions-dialog").role(Role::Dialog).aria_label("Сессии воркспейса")
                .w_full().max_w(px(720.)).mx(px(16.)).max_h(gpui::relative(1.)).p(px(18.)).rounded(px(12.)).border_1().border_color(colors.border).bg(colors.raised).text_color(colors.text)
                .on_click(cx.listener(|_,_:&ClickEvent,_,cx|cx.stop_propagation()))
                .child(div().text_size(px(18.)).child(format!("Сессии · {}",overview_text(name))))
                .child(div().mt(px(5.)).mb(px(12.)).text_size(px(11.)).text_color(colors.muted).whitespace_normal().child("Размещённые панели всех вкладок · снимок по запросу. Продолжение всегда требует отдельного подтверждения; последнее задание не повторяется."))
                .child(rows).child(details)
                .child(div().mt(px(12.)).text_size(px(10.)).text_color(colors.muted).whitespace_normal().child("↑ ↓ — выбрать · Enter — открыть панель · Ctrl+Enter — проверить продолжение · Ctrl+R — обновить · PgUp/PgDn — страницы · Ctrl+Home — первая · Esc — закрыть"))
                .child(controls)).into_any_element()
    }
}

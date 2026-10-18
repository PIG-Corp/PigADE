//! Read-only activity center for unread host summaries.
//!
//! Activity metadata is deliberately kept separate from terminal content: this
//! view renders session identity, catalog placement, and the tracker's flags.
use super::*;

const MAX_ACTIVITY_ROWS: usize = 32;

#[derive(Clone, Debug)]
struct ActivityRow {
    session: Uuid,
    generation: Uuid,
    sequence: u64,
    title: String,
    location: String,
    details: String,
    destination: ProductAction,
}

impl ProductUi {
    /// Number of current sessions with at least one unread activity flag.
    pub(crate) fn activity_count(&self) -> usize {
        if !self.activity_supported {
            return 0;
        }
        let Some(snapshot) = self.activity.snapshot() else {
            return 0;
        };
        snapshot
            .sessions
            .iter()
            .filter(|session| self.activity.flags(session.session).any())
            .count()
    }

    /// Unread sessions whose panes occur in the requested tab's valid layout.
    pub(crate) fn tab_activity(&self, tab: Uuid) -> usize {
        if !self.activity_supported {
            return 0;
        }
        self.activity_tab_counts.get(&tab).copied().unwrap_or(0)
    }

    /// Unread sessions across a workspace's tabs, counting each pane once.
    pub(crate) fn workspace_activity(&self, workspace: Uuid) -> usize {
        if !self.activity_supported {
            return 0;
        }
        self.activity_workspace_counts
            .get(&workspace)
            .copied()
            .unwrap_or(0)
    }

    /// Refresh all workspace and tab badge totals with one parse of each tab.
    /// The cache depends on layout revision and the sorted unread pane IDs.
    pub(crate) fn refresh_activity_badges(&mut self) {
        let Some(catalog) = self.catalog.as_ref() else {
            self.activity_badge_key = None;
            self.activity_tab_counts.clear();
            self.activity_workspace_counts.clear();
            return;
        };
        let layout_revision = catalog.layout.revision;
        let mut unread_ids: Vec<_> = if self.activity_supported {
            self.activity
                .snapshot()
                .into_iter()
                .flat_map(|snapshot| &snapshot.sessions)
                .filter(|session| self.activity.flags(session.session).any())
                .map(|session| session.session)
                .collect()
        } else {
            Vec::new()
        };
        unread_ids.sort_unstable();
        unread_ids.dedup();
        let key = (layout_revision, unread_ids.clone());
        if self.activity_badge_key.as_ref() == Some(&key) {
            return;
        }

        let unread: HashSet<_> = unread_ids.into_iter().collect();
        let mut tab_counts = HashMap::new();
        let mut workspace_panes: HashMap<Uuid, HashSet<Uuid>> = HashMap::new();
        for tab in &catalog.tabs {
            let panes: HashSet<_> =
                serde_json::from_str::<pigade_layout::TabLayout>(&tab.layout_json)
                    .map(|layout| layout.panes().into_iter().collect())
                    .unwrap_or_default();
            tab_counts.insert(tab.id, panes.intersection(&unread).count());
            workspace_panes
                .entry(tab.workspace_id)
                .or_default()
                .extend(panes);
        }
        let workspace_counts = workspace_panes
            .into_iter()
            .map(|(workspace, panes)| (workspace, panes.intersection(&unread).count()))
            .collect();
        self.activity_tab_counts = tab_counts;
        self.activity_workspace_counts = workspace_counts;
        self.activity_badge_key = Some(key);
    }

    /// Small, screen-reader-labelled unread count for sidebar or tab chrome.
    pub(crate) fn activity_badge(&self, count: usize) -> AnyElement {
        let count = if self.activity_supported { count } else { 0 };
        if count == 0 {
            return div().w(px(0.)).h(px(0.)).into_any_element();
        }
        let colors = self.colors();
        let visible = if count > 99 {
            "99+".to_owned()
        } else {
            count.to_string()
        };
        div()
            .id("activity-badge")
            .role(Role::Status)
            .aria_label(format!("Непросмотренная активность: {count}"))
            .min_w(px(18.))
            .h(px(18.))
            .px(px(5.))
            .flex_shrink_0()
            .flex()
            .items_center()
            .justify_center()
            .rounded_full()
            .bg(if self.activity_supported {
                colors.accent
            } else {
                colors.muted
            })
            .text_color(if self.activity_supported {
                rgb(chrome::Colors::for_theme(self.appearance.theme).on_accent)
            } else {
                rgb(chrome::Colors::for_theme(self.appearance.theme).base)
            })
            .text_size(px(10.))
            .font_weight(FontWeight::SEMIBOLD)
            .child(visible)
            .into_any_element()
    }

    /// Unread summaries in stable UUID order for menu, keyboard, and sheet use.
    pub(crate) fn activity_menu_entries(&self) -> Vec<(String, ProductAction)> {
        self.activity_rows()
            .into_iter()
            .map(|row| {
                (
                    format!("{} · {} · {}", row.title, row.location, row.details),
                    row.destination,
                )
            })
            .collect()
    }

    /// Exact unread summary selected by the Activity dialog's shared menu index.
    pub(crate) fn selected_activity_target(&self) -> Option<(Uuid, Uuid, u64)> {
        let dialog = self.dialog.as_ref()?;
        if dialog.kind != DialogKind::Activity || !self.activity_supported {
            return None;
        }
        let rows = self.activity_rows();
        let row = rows.get(dialog.active_field.min(rows.len().checked_sub(1)?))?;
        Some((row.session, row.generation, row.sequence))
    }

    fn activity_rows(&self) -> Vec<ActivityRow> {
        if !self.activity_supported {
            return Vec::new();
        }
        let Some(snapshot) = self.activity.snapshot() else {
            return Vec::new();
        };
        let mut sessions: Vec<_> = snapshot
            .sessions
            .iter()
            .filter(|session| self.activity.flags(session.session).any())
            .collect();
        sessions.sort_by_key(|session| session.session);

        sessions
            .into_iter()
            .map(|session| {
                let flags = self.activity.flags(session.session);
                let title = self.activity_title(session.session);
                let (location, destination) = self.activity_location(session.session);
                ActivityRow {
                    session: session.session,
                    generation: session.generation,
                    sequence: session.sequence,
                    title,
                    location,
                    details: activity_flag_labels(flags).join(" · "),
                    destination,
                }
            })
            .collect()
    }

    fn activity_title(&self, session: Uuid) -> String {
        let alias = self
            .catalog
            .as_ref()
            .and_then(|catalog| catalog.pane_labels.as_ref())
            .and_then(|labels| labels.iter().find(|label| label.session == session))
            .map(|label| label.name.trim())
            .filter(|label| !label.is_empty());
        alias.map_or_else(
            || format!("Терминал · {}", short_uuid(session)),
            str::to_owned,
        )
    }

    fn activity_location(&self, session: Uuid) -> (String, ProductAction) {
        let Some(catalog) = self.catalog.as_ref() else {
            return (
                "Каталог пока недоступен".into(),
                ProductAction::OpenSessions,
            );
        };

        let mut placements = Vec::new();
        for tab in &catalog.tabs {
            let Ok(layout) = serde_json::from_str::<pigade_layout::TabLayout>(&tab.layout_json)
            else {
                continue;
            };
            if !layout.panes().contains(&session) {
                continue;
            }
            let workspace = catalog
                .workspaces
                .iter()
                .find(|workspace| workspace.id == tab.workspace_id);
            let project = workspace.and_then(|workspace| {
                catalog
                    .projects
                    .iter()
                    .find(|project| project.id == workspace.project_id)
                    .map(|project| project.name.clone())
            });
            let workspace_label = workspace
                .map(workspace_name)
                .unwrap_or_else(|| "Воркспейс".into());
            let project_label = project.unwrap_or_else(|| "Проект".into());
            let location = format!("{project_label} · {workspace_label} · {}", tab.name);
            let destination = match workspace {
                Some(workspace) if workspace.status == WorkspaceStatus::Ready => {
                    ProductAction::ActivatePane {
                        tab: tab.id,
                        session,
                    }
                }
                Some(workspace) => ProductAction::ManageWorkspace(workspace.id),
                None => ProductAction::OpenSessions,
            };
            let ready =
                workspace.is_some_and(|workspace| workspace.status == WorkspaceStatus::Ready);
            placements.push((ready, location, destination));
        }

        placements
            .iter()
            .find(|(ready, _, _)| *ready)
            .or_else(|| placements.first())
            .map(|(_, location, action)| (location.clone(), action.clone()))
            .unwrap_or_else(|| ("Вне вкладок · Сессии".into(), ProductAction::OpenSessions))
    }

    /// Bounded activity sheet; this view only acknowledges displayed summaries.
    pub(super) fn activity_overlay(&self, cx: &mut Context<CanvasApp>) -> AnyElement {
        let colors = self.colors();
        let rows = self.activity_rows();
        let mut list = div()
            .id("activity-rows")
            .max_h(px(400.))
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&self.activity_scroll)
            .flex()
            .flex_col()
            .gap_1();

        if !self.activity_supported {
            let message = if self.activity_feed_error.is_some() {
                "Ожидается корректная сводка активности; сохранённые данные сейчас скрыты."
            } else {
                "Этот session host не поддерживает сводку активности."
            };
            list = list.child(
                div()
                    .id("activity-unavailable")
                    .role(Role::Status)
                    .text_size(px(12.))
                    .text_color(colors.muted)
                    .whitespace_normal()
                    .child(message),
            );
        } else if rows.is_empty() {
            list = list.child(
                div()
                    .id("activity-empty")
                    .role(Role::Status)
                    .text_size(px(12.))
                    .text_color(colors.muted)
                    .whitespace_normal()
                    .child("Непросмотренной активности нет."),
            );
        } else {
            let selected_index = self
                .dialog
                .as_ref()
                .filter(|dialog| dialog.kind == DialogKind::Activity)
                .map(|dialog| dialog.active_field.min(rows.len().saturating_sub(1)));
            for (index, row) in rows.iter().take(MAX_ACTIVITY_ROWS).enumerate() {
                let selected = selected_index == Some(index);
                list = list.child(
                    div()
                        .id(format!("activity-row-{}", row.session))
                        .flex_shrink_0()
                        .h(px(70.))
                        .min_h(px(70.))
                        .flex()
                        .items_center()
                        .gap_2()
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
                        .p_2()
                        .child(
                            div()
                                .min_w_0()
                                .flex_1()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(
                                    div()
                                        .text_size(px(12.))
                                        .font_weight(FontWeight::MEDIUM)
                                        .truncate()
                                        .child(row.title.clone()),
                                )
                                .child(
                                    div()
                                        .text_size(px(10.))
                                        .text_color(colors.muted)
                                        .truncate()
                                        .child(row.location.clone()),
                                )
                                .child(
                                    div()
                                        .text_size(px(10.))
                                        .text_color(colors.accent)
                                        .whitespace_normal()
                                        .child(row.details.clone()),
                                ),
                        )
                        .child(self.action_control(
                            format!("activity-open-{}", row.session),
                            "Открыть".into(),
                            row.destination.clone(),
                            ControlStyle {
                                role: Role::Button,
                                selected,
                                compact: true,
                            },
                            cx,
                        ))
                        .child(self.action_control(
                            format!("activity-read-{}", row.session),
                            "Просмотрено".into(),
                            ProductAction::MarkActivityRead(
                                row.session,
                                row.generation,
                                row.sequence,
                            ),
                            ControlStyle {
                                role: Role::Button,
                                selected: false,
                                compact: true,
                            },
                            cx,
                        )),
                );
            }
        }

        let more = rows.len().saturating_sub(MAX_ACTIVITY_ROWS);
        let mut dialog = div()
            .id("activity-dialog")
            .role(Role::Dialog)
            .aria_label("Центр активности")
            .w_full()
            .max_w(px(720.))
            .max_h(px(620.))
            .mx(px(16.))
            .flex()
            .flex_col()
            .gap_3()
            .rounded(px(12.))
            .border_1()
            .border_color(colors.border)
            .bg(colors.raised)
            .px(px(20.))
            .py(px(18.))
            .text_color(colors.text)
            .on_click(cx.listener(|_, _: &ClickEvent, _, cx| cx.stop_propagation()))
            .child(
                div()
                    .id("activity-heading")
                    .role(Role::Heading)
                    .aria_level(2)
                    .text_size(px(17.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("Активность"),
            )
            .child(
                div()
                    .id("activity-explainer")
                    .text_size(px(11.))
                    .text_color(colors.muted)
                    .whitespace_normal()
                    .child(
                        "Новый вывод и завершения CLI в фоне. Открытие списка не очищает отметки.",
                    ),
            )
            .child(list);

        if more > 0 {
            dialog = dialog.child(
                div()
                    .id("activity-overflow-note")
                    .text_size(px(10.))
                    .text_color(colors.muted)
                    .child(format!(
                        "Показаны первые {MAX_ACTIVITY_ROWS} записей; ещё {more}."
                    )),
            );
        }
        if self.activity_error.is_some() {
            dialog = dialog.child(
                div()
                    .id("activity-persistence-error")
                    .role(Role::Alert)
                    .text_size(px(11.))
                    .text_color(colors.status_amber)
                    .whitespace_normal()
                    .child("Не удалось сохранить отметки просмотра. Они могут не сохраниться после перезапуска."),
            );
            if let Some(error) = self.activity_error.as_deref() {
                dialog = dialog.child(
                    div()
                        .id("activity-persistence-error-detail")
                        .role(Role::Status)
                        .text_size(px(10.))
                        .text_color(colors.muted)
                        .whitespace_normal()
                        .child(error.chars().take(160).collect::<String>()),
                );
            }
        }
        if let Some(error) = self.activity_feed_error.as_deref() {
            dialog = dialog.child(
                div()
                    .id("activity-feed-error")
                    .role(Role::Alert)
                    .text_size(px(11.))
                    .text_color(colors.status_amber)
                    .whitespace_normal()
                    .child(error.to_owned()),
            );
        }

        let mut controls = div().flex().justify_end().gap_2();
        if !rows.is_empty() {
            controls = controls.child(self.action_control(
                "activity-mark-all-read".into(),
                "Отметить всё просмотренным".into(),
                ProductAction::MarkAllActivityRead,
                ControlStyle {
                    role: Role::Button,
                    selected: false,
                    compact: true,
                },
                cx,
            ));
        }
        if self.activity_error.is_some() {
            controls = controls.child(self.action_control(
                "activity-retry-save".into(),
                "Повторить сохранение".into(),
                ProductAction::RetryActivitySave,
                ControlStyle {
                    role: Role::Button,
                    selected: false,
                    compact: true,
                },
                cx,
            ));
        }
        controls = controls.child(self.action_control(
            "activity-close".into(),
            "Закрыть".into(),
            ProductAction::CloseDialog,
            ControlStyle {
                role: Role::Button,
                selected: false,
                compact: true,
            },
            cx,
        ));
        dialog = dialog.child(controls);
        dialog = dialog.child(
            div()
                .id("activity-keyboard-hint")
                .text_size(px(10.))
                .text_color(colors.muted)
                .whitespace_normal()
                .child(if self.activity_error.is_some() {
                    "↑/↓, Home/End, PgUp/PgDn — выбор · Enter — открыть · Ctrl+R — отметить · Ctrl+Shift+R — отметить всё · Ctrl+S — повторить сохранение · Esc — закрыть"
                } else {
                    "↑/↓, Home/End, PgUp/PgDn — выбор · Enter — открыть · Ctrl+R — отметить · Ctrl+Shift+R — отметить всё · Esc — закрыть"
                }),
        );

        div()
            .id("activity-overlay")
            .accessibility_id("activity-overlay")
            .absolute()
            .inset_0()
            .flex()
            .items_start()
            .justify_center()
            .overflow_y_scroll()
            .py(px(24.))
            .bg(rgb(0x090b0e).opacity(0.72))
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                this.product_action(ProductAction::CloseDialog, window, cx)
            }))
            .child(dialog)
            .into_any_element()
    }
}

fn activity_flag_labels(flags: crate::activity::Flags) -> Vec<&'static str> {
    let mut labels = Vec::with_capacity(3);
    if flags.output {
        labels.push("Новый вывод");
    }
    if flags.ended {
        labels.push("CLI завершён");
    }
    if flags.error {
        labels.push("Ошибка хранения");
    }
    labels
}

fn short_uuid(id: Uuid) -> String {
    id.simple().to_string()[..8].to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use pigade_layout::{Node, TabLayout};
    use pigade_storage::{
        Catalog, LayoutDocument, Project, Selection, Tab, Workspace, WorkspaceMode,
    };
    use std::path::PathBuf;

    fn id(value: u128) -> Uuid {
        Uuid::from_u128(value)
    }

    fn product(workspace_status: WorkspaceStatus) -> ProductUi {
        let session = id(1);
        let host = id(2);
        let workspace_id = id(3);
        let project_id = id(4);
        let tab_a = id(5);
        let tab_b = id(6);
        let mut ui = ProductUi {
            activity_supported: true,
            ..ProductUi::default()
        };
        ui.activity = crate::activity::Tracker::new(crate::activity::Saved {
            revision: 0,
            schema: 1,
            host_instance: Some(host),
            seen: vec![crate::activity::Seen {
                session,
                generation: id(7),
                sequence: 0,
                output_bytes: 0,
                state: "running".into(),
                storage_error: false,
            }],
        });
        ui.activity
            .observe(crate::activity::Snapshot {
                schema: 1,
                host_instance: host,
                revision: 1,
                sessions: vec![crate::activity::Session {
                    session,
                    generation: id(7),
                    sequence: 1,
                    output_bytes: 9,
                    state: "running".into(),
                    storage_error: false,
                }],
            })
            .unwrap();

        let layout = || {
            serde_json::to_string(&TabLayout {
                root: Some(Node::pane(session)),
                focused: Some(session),
                focus_mode: false,
            })
            .unwrap()
        };
        ui.catalog = Some(Catalog {
            projects: vec![Project {
                id: project_id,
                canonical_path: PathBuf::new(),
                git_common_dir: PathBuf::new(),
                name: "Project".into(),
                roots: vec![],
                revision: 1,
            }],
            workspaces: vec![Workspace {
                id: workspace_id,
                project_id,
                task: "Workspace".into(),
                cwd: PathBuf::new(),
                mode: WorkspaceMode::SharedCheckout,
                status: workspace_status,
                error: None,
                context_revision: None,
                revision: 1,
            }],
            tabs: vec![
                Tab {
                    id: tab_a,
                    workspace_id,
                    name: "First".into(),
                    layout_json: layout(),
                    active_pane: Some(session),
                    revision: 1,
                },
                Tab {
                    id: tab_b,
                    workspace_id,
                    name: "Second".into(),
                    layout_json: layout(),
                    active_pane: Some(session),
                    revision: 1,
                },
            ],
            tab_orders: None,
            selection: Selection::default(),
            layout: LayoutDocument::default(),
            layout_templates: Some(vec![]),
            template_launch_schema: None,
            pane_labels: Some(vec![]),
            agent_profiles: None,
            agent_profile_roster: None,
            prompts: None,
            tab_close_schema: None,
            engine_options_schema: None,
            pane_tab_drop_schema: None,
            pane_workspace_drop_schema: None,
            workspace_recovery_schema: None,
            history_grid_schema: None,
            custom_cli_schema: None,
            workspace_sessions_schema: None,
            history_pan_schema: None,
        });
        ui.dialog = Some(Dialog::new(DialogKind::Activity));
        ui.refresh_activity_badges();
        ui
    }

    #[test]
    fn unread_counts_deduplicate_a_pane_referenced_by_multiple_tabs() {
        let mut ui = product(WorkspaceStatus::Ready);
        assert_eq!(ui.activity_count(), 1);
        assert_eq!(ui.tab_activity(id(5)), 1);
        assert_eq!(ui.tab_activity(id(6)), 1);
        assert_eq!(ui.workspace_activity(id(3)), 1);

        ui.activity_supported = false;
        ui.refresh_activity_badges();
        assert_eq!(ui.activity_count(), 0);
        assert_eq!(ui.tab_activity(id(5)), 0);
        assert_eq!(ui.workspace_activity(id(3)), 0);

        ui.activity_supported = true;
        ui.refresh_activity_badges();
        assert_eq!(ui.workspace_activity(id(3)), 1);
    }

    #[test]
    fn selected_target_uses_stable_row_and_routes_archived_workspace_to_management() {
        let ui = product(WorkspaceStatus::Archived);
        assert_eq!(ui.selected_activity_target(), Some((id(1), id(7), 1)));
        assert_eq!(
            ui.activity_menu_entries()[0].1,
            ProductAction::ManageWorkspace(id(3))
        );

        let ready = product(WorkspaceStatus::Ready);
        assert_eq!(
            ready.activity_menu_entries()[0].1,
            ProductAction::ActivatePane {
                tab: id(5),
                session: id(1)
            }
        );
    }
}

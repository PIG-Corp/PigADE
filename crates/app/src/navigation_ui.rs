//! Shared palette results for mouse and keyboard activation.
use super::*;
use crate::navigation::{PaletteEntry, search_matches, search_score};

fn command_aliases(action: &ProductAction) -> &'static [&'static str] {
    match action {
        ProductAction::OpenAppearance => &["appearance", "theme", "внешний вид", "тема"],
        ProductAction::OpenCloseTab => &["close tab", "закрыть вкладку"],
        ProductAction::OpenPrompts => &["prompts", "шаблоны текста", "промпты"],
        ProductAction::RenamePane => &["rename", "alias", "имя"],
        ProductAction::OpenStop => &["stop", "terminate", "остановить"],
        ProductAction::OpenWorkspaceGit => &["git", "status", "branch", "изменения"],
        ProductAction::OpenSessions => &["sessions", "hidden", "сессии"],
        _ => &[],
    }
}

impl ProductUi {
    pub(crate) fn pane_names_supported(&self) -> bool {
        self.catalog
            .as_ref()
            .is_some_and(|catalog| catalog.pane_labels.is_some())
    }

    pub(crate) fn pane_title<'a>(&'a self, view: &'a protocol::View) -> &'a str {
        self.display_title(&view.session, &view.frame.title)
    }

    pub(crate) fn display_title<'a>(&'a self, session: &str, title: &'a str) -> &'a str {
        let session = session.parse::<Uuid>().ok();
        self.catalog
            .as_ref()
            .and_then(|catalog| catalog.pane_labels.as_ref())
            .and_then(|labels| labels.iter().find(|label| Some(label.session) == session))
            .map(|label| label.name.as_str())
            .filter(|name| !name.is_empty())
            .unwrap_or(if title.is_empty() {
                "Терминал"
            } else {
                title
            })
    }

    pub(crate) fn palette_entries(&self) -> Vec<PaletteEntry> {
        let Some(dialog) = &self.dialog else {
            return vec![];
        };
        let query = dialog
            .fields
            .first()
            .map(EditBuffer::text)
            .unwrap_or("")
            .trim();
        let directory = self
            .panel_directory
            .as_ref()
            .filter(|page| page.query == query);
        let waiting_for_directory = dialog.kind == DialogKind::Palette
            && !query.is_empty()
            && self.pane_names_supported()
            && directory.is_none()
            && self.palette_error.is_none();
        let mut result = Vec::new();
        for (label, action) in palette_commands() {
            if waiting_for_directory && !crate::navigation::local_palette_command(&action) {
                continue;
            }
            if dialog.kind == DialogKind::PaneMenu && !panel_action(&action) {
                continue;
            }
            let keywords = format!("{label} {}", command_aliases(&action).join(" "));
            if search_matches(query, &keywords) {
                result.push(PaletteEntry {
                    label: label.into(),
                    detail: if action == ProductAction::RenamePane && !self.pane_names_supported() {
                        "Недоступно: нужен session host с поддержкой имён панелей".into()
                    } else {
                        String::new()
                    },
                    action,
                });
            }
        }
        if dialog.kind == DialogKind::PaneMenu || waiting_for_directory {
            return result;
        }
        if let Some(catalog) = &self.catalog {
            for project in &catalog.projects {
                let text = format!("{} {}", project.name, project.canonical_path.display());
                if search_matches(query, &text) {
                    result.push(PaletteEntry {
                        label: project.name.clone(),
                        detail: "Проект".into(),
                        action: ProductAction::SelectProject(project.id),
                    });
                }
            }
            for workspace in catalog
                .workspaces
                .iter()
                .filter(|workspace| workspace.status == WorkspaceStatus::Ready)
            {
                let project = catalog
                    .projects
                    .iter()
                    .find(|project| project.id == workspace.project_id);
                let path = format!(
                    "{} › {}",
                    project
                        .map(|project| project.name.as_str())
                        .unwrap_or("Проект"),
                    workspace_name(workspace)
                );
                let text = format!("{path} {}", workspace.cwd.display());
                if search_matches(query, &text) {
                    result.push(PaletteEntry {
                        label: workspace_name(workspace),
                        detail: project
                            .map(|project| format!("Воркспейс · {}", project.name))
                            .unwrap_or_default(),
                        action: ProductAction::SelectWorkspace(workspace.id),
                    });
                }
                for tab in catalog
                    .tabs
                    .iter()
                    .filter(|tab| tab.workspace_id == workspace.id)
                {
                    let text = format!("{path} › {}", tab.name);
                    if search_matches(query, &text) {
                        result.push(PaletteEntry {
                            label: tab.name.clone(),
                            detail: format!("Вкладка · {path}"),
                            action: ProductAction::SelectTab(tab.id),
                        });
                    }
                }
            }
            if let Some(directory) = directory {
                for entry in &directory.entries {
                    let project = catalog
                        .projects
                        .iter()
                        .find(|project| project.id == entry.project_id);
                    let workspace = catalog
                        .workspaces
                        .iter()
                        .find(|workspace| workspace.id == entry.workspace_id);
                    let tab = catalog.tabs.iter().find(|tab| tab.id == entry.tab_id);
                    let detail = format!(
                        "{} › {} › {} · {} · {}{}",
                        project
                            .map(|project| project.name.as_str())
                            .unwrap_or("Проект"),
                        workspace.map(workspace_name).unwrap_or_default(),
                        tab.map(|tab| tab.name.as_str()).unwrap_or("Вкладка"),
                        session_state_label(&entry.state),
                        entry.provider.as_deref().unwrap_or("CLI"),
                        if entry.role.is_empty() {
                            String::new()
                        } else {
                            format!(
                                " · {}{}",
                                entry.role.chars().take(160).collect::<String>(),
                                if entry.deliver_context {
                                    ""
                                } else {
                                    " (не передана)"
                                }
                            )
                        }
                    );
                    result.push(PaletteEntry {
                        label: entry.title.clone(),
                        detail,
                        action: ProductAction::ActivatePane {
                            tab: entry.tab_id,
                            session: entry.session,
                        },
                    });
                }
            }
        }
        if !query.is_empty() {
            result.sort_by(|first, second| {
                let score = |entry: &PaletteEntry| {
                    let mut rank = search_score(query, &entry.label);
                    for alias in command_aliases(&entry.action) {
                        rank = rank.max(search_score(query, alias));
                    }
                    (
                        rank,
                        matches!(entry.action, ProductAction::ActivatePane { .. }),
                    )
                };
                score(second)
                    .cmp(&score(first))
                    .then_with(|| first.label.cmp(&second.label))
            });
        }
        // Build bounded widget rows even if a catalog contains many tabs.
        result.truncate(128);
        if self.palette_after.is_some() {
            result.push(PaletteEntry {
                label: "Первые результаты".into(),
                detail: "Вернуться к началу поиска панелей".into(),
                action: ProductAction::FirstPanelResults,
            });
        }
        if directory.is_some_and(|page| page.next_after.is_some()) {
            result.push(PaletteEntry {
                label: "Ещё панели".into(),
                detail: "Следующие результаты поиска".into(),
                action: ProductAction::MorePanelResults,
            });
        }
        if self.palette_error.is_some() {
            result.push(PaletteEntry {
                label: "Обновить список".into(),
                detail: "Прочитать каталог и повторить поиск".into(),
                action: ProductAction::ReloadPanelResults,
            });
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn profiles_are_available_before_a_remote_panel_search_finishes() {
        let ui = ui("profiles");
        assert_eq!(ui.palette_action(), Some(ProductAction::OpenAgentProfiles));
        assert!(ui.panel_directory.is_none());
    }
    fn ui(query: &str) -> ProductUi {
        let catalog:Catalog=serde_json::from_value(serde_json::json!({
            "projects":[],"workspaces":[],"tabs":[],"selection":pigade_storage::Selection::default(),
            "layout":pigade_storage::LayoutDocument::default(),"pane_labels":[]
        })).unwrap();
        let mut dialog = Dialog::new(DialogKind::Palette);
        dialog.fields[0] = query.into();
        ProductUi {
            catalog: Some(catalog),
            dialog: Some(dialog),
            ..Default::default()
        }
    }
    #[test]
    fn a_pending_panel_query_does_not_activate_a_similarly_named_launch_command() {
        let mut ui = ui("review");
        assert!(ui.palette_action().is_none());
        ui.panel_directory = Some(protocol::PanelDirectory {
            query: "review".into(),
            entries: vec![protocol::PanelEntry {
                session: Uuid::from_u128(1),
                tab_id: Uuid::from_u128(2),
                workspace_id: Uuid::from_u128(3),
                project_id: Uuid::from_u128(4),
                generation: None,
                title: "Review".into(),
                provider: Some("codex".into()),
                role: "Reviewer".into(),
                deliver_context: true,
                state: "history".into(),
            }],
            next_after: None,
        });
        assert_eq!(
            ui.palette_action(),
            Some(ProductAction::ActivatePane {
                tab: Uuid::from_u128(2),
                session: Uuid::from_u128(1)
            })
        );
        ui.dialog.as_mut().unwrap().fields[0] = "another query".into();
        assert!(ui.palette_action().is_none());
    }
    #[test]
    fn keyboard_selection_uses_the_same_result_order_as_mouse_rows() {
        let mut ui = ui("");
        let entries = ui.palette_entries();
        assert!(entries.len() > 2);
        ui.palette_index = 2;
        assert_eq!(ui.palette_action(), Some(entries[2].action.clone()));
        ui.dialog.as_mut().unwrap().fields[0] = "rename".into();
        ui.panel_directory = Some(protocol::PanelDirectory {
            query: "rename".into(),
            entries: vec![],
            next_after: None,
        });
        ui.palette_index = 0;
        ui.catalog
            .as_mut()
            .unwrap()
            .projects
            .push(pigade_storage::Project {
                id: Uuid::from_u128(99),
                canonical_path: "/project".into(),
                git_common_dir: "/project/.git".into(),
                name: "Renamed".into(),
                roots: vec![],
                revision: 0,
            });
        // An exact command alias must outrank an incidental project/tab prefix.
        assert_eq!(ui.palette_action(), Some(ProductAction::RenamePane));
    }
}

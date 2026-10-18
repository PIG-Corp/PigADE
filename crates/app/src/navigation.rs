//! Panel navigation changes metadata only. Search receipts are scoped to the
//! exact palette/query so stale replies cannot redirect a later user action.
use super::{CanvasApp, product, protocol};
use gpui::{Context, Window};
use product::{Dialog, DialogKind, ProductAction as A};
use serde_json::{Value, json};
use std::time::{Duration, Instant};

#[derive(Clone)]
pub(crate) struct PaletteEntry {
    pub label: String,
    pub detail: String,
    pub action: A,
}

pub(crate) fn search_matches(query: &str, text: &str) -> bool {
    let text = text.to_lowercase();
    query
        .split_whitespace()
        .all(|word| text.contains(&word.to_lowercase()))
}

pub(crate) fn search_score(query: &str, label: &str) -> u8 {
    let query = query.trim().to_lowercase();
    let label = label.to_lowercase();
    if !query.is_empty() && label == query {
        2
    } else if !query.is_empty() && label.starts_with(&query) {
        1
    } else {
        0
    }
}

pub(crate) fn local_palette_command(action: &A) -> bool {
    matches!(
        action,
        A::OpenHostMaintenance
            | A::OpenWorkspaceSwitcher
            | A::OpenActivity
            | A::OpenWorkspaceSessions
            | A::OpenTerminalLinks
            | A::OpenAppearance
            | A::AdjustTerminalFont(_)
            | A::ResetTerminalFont
            | A::OpenCloseTab
            | A::OpenPrompts
            | A::OpenAgentProfiles
            | A::OpenControls
            | A::OpenLayouts
            | A::OpenPaneMenu
    )
}

fn can_choose_palette(action: &A, pending: Option<&Kind>) -> bool {
    match pending {
        None => true,
        Some(Kind::Search { .. }) => local_palette_command(action),
        Some(_) => false,
    }
}

#[derive(Clone)]
pub(crate) enum Kind {
    Search {
        query: String,
        after: Option<uuid::Uuid>,
    },
    Rename {
        session: uuid::Uuid,
        editor_revision: u64,
    },
    Reload,
    Activate {
        tab: uuid::Uuid,
        session: uuid::Uuid,
    },
}

#[derive(Clone)]
pub(crate) struct Pending {
    pub request: uuid::Uuid,
    pub dialog: Option<uuid::Uuid>,
    pub editor_revision: Option<u64>,
    pub kind: Kind,
}

impl Pending {
    fn matches_query(&self, dialog: Option<&Dialog>, query: &str) -> bool {
        dialog.is_some_and(|dialog| {
            Some(dialog.id) == self.dialog
                && dialog.kind == DialogKind::Palette
                && dialog.fields.first().is_some_and(|field| {
                    field.text().trim() == query && Some(field.revision()) == self.editor_revision
                })
        })
    }
}

#[derive(Default)]
pub(crate) struct Navigation {
    pub pending: Option<Pending>,
    scope: Option<(uuid::Uuid, String, u64)>,
    due: Option<Instant>,
    rename: Option<(uuid::Uuid, Option<u64>)>,
}

impl Navigation {
    pub fn rename_session(&self) -> Option<uuid::Uuid> {
        self.rename.map(|(session, _)| session)
    }

    pub fn take_reply(&mut self, id: Option<uuid::Uuid>) -> Option<Pending> {
        if self
            .pending
            .as_ref()
            .is_some_and(|pending| Some(pending.request) == id)
        {
            self.pending.take()
        } else {
            None
        }
    }
}

impl CanvasApp {
    fn navigation_request(
        &mut self,
        mut command: Value,
        kind: Kind,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.navigation.pending.is_some() {
            return false;
        }
        let request = uuid::Uuid::new_v4();
        command["_ui_request"] = json!(request);
        if !self.try_product_command(command, cx) {
            return false;
        }
        self.navigation.pending = Some(Pending {
            request,
            dialog: self.product.dialog.as_ref().map(|dialog| dialog.id),
            editor_revision: self
                .product
                .dialog
                .as_ref()
                .and_then(|dialog| dialog.fields.first().map(|field| field.revision())),
            kind,
        });
        true
    }

    pub(super) fn service_panel_search(&mut self, cx: &mut Context<Self>) {
        let scope = self
            .product
            .dialog
            .as_ref()
            .filter(|dialog| dialog.kind == DialogKind::Palette)
            .map(|dialog| {
                (
                    dialog.id,
                    dialog.fields[0].text().trim().to_owned(),
                    dialog.fields[0].revision(),
                )
            });
        if scope != self.navigation.scope {
            self.navigation.scope = scope.clone();
            self.navigation.due = scope
                .as_ref()
                .map(|_| Instant::now() + Duration::from_millis(120));
            self.product.panel_directory = None;
            self.product.palette_error = None;
            self.product.palette_after = None;
            self.product.palette_index = 0;
            self.product.palette_scroll.scroll_to_item(0);
            self.product.palette_loading = scope.is_some() && self.product.pane_names_supported();
            cx.notify();
        }
        let Some((_, query, _)) = scope else {
            return;
        };
        if !self.product.pane_names_supported()
            || self.product.busy
            || self.navigation.pending.is_some()
            || self
                .product
                .active_editor()
                .is_some_and(|buffer| buffer.marked().is_some())
            || !self
                .navigation
                .due
                .is_some_and(|deadline| Instant::now() >= deadline)
        {
            return;
        }
        if query.len() > 256 {
            self.navigation.due = None;
            self.product.palette_loading = false;
            self.product.palette_error = Some("Поиск панелей: максимум 256 байт UTF-8".into());
            return;
        }
        if self.navigation_request(
            json!({"command":"search_panels","query":query,"after":null}),
            Kind::Search { query, after: None },
            cx,
        ) {
            self.navigation.due = None;
        }
    }

    pub(super) fn choose_palette(
        &mut self,
        action: A,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !can_choose_palette(
            &action,
            self.navigation
                .pending
                .as_ref()
                .map(|pending| &pending.kind),
        ) {
            self.product.notice = Some("Дождитесь результата поиска".into());
            cx.notify();
            return;
        }
        let keep = matches!(
            action,
            A::ActivatePane { .. }
                | A::MorePanelResults
                | A::FirstPanelResults
                | A::ReloadPanelResults
        );
        if !keep {
            self.product.dialog = None;
        }
        self.product_action(action, window, cx);
    }

    pub(super) fn navigation_action(&mut self, action: A, cx: &mut Context<Self>) {
        match action {
            A::RenamePane => {
                if !self.product.pane_names_supported() {
                    self.product.notice =
                        Some("Имена панелей недоступны в текущем session host".into());
                    return;
                }
                if let Some(session) = self.selected_pane() {
                    let label = self
                        .product
                        .catalog
                        .as_ref()
                        .and_then(|catalog| catalog.pane_labels.as_ref())
                        .and_then(|labels| labels.iter().find(|label| label.session == session));
                    self.navigation.rename = Some((session, label.map(|label| label.revision)));
                    let mut dialog = Dialog::new(DialogKind::RenamePane);
                    dialog.fields[0] = label
                        .map(|label| label.name.clone())
                        .unwrap_or_default()
                        .into();
                    dialog.fields[0].select_all();
                    self.product.dialog = Some(dialog);
                } else {
                    self.product.notice =
                        Some("Сначала откройте и выберите терминальную панель".into());
                }
            }
            A::ActivatePane { tab, session } => {
                if let Some(catalog) = &self.product.catalog {
                    self.navigation_request(json!({"command":"activate_pane","tab":tab,"session":session,"expected_layout_revision":catalog.layout.revision}),Kind::Activate {tab,session},cx);
                }
            }
            A::ReloadPanelResults => {
                self.navigation_request(json!({"command":"catalog"}), Kind::Reload, cx);
            }
            A::FirstPanelResults => {
                self.navigation.scope = None;
                self.service_panel_search(cx);
            }
            A::MorePanelResults => {
                if let Some(directory) = &self.product.panel_directory
                    && let Some(after) = directory.next_after
                {
                    let query = directory.query.clone();
                    if self.navigation_request(
                        json!({"command":"search_panels","query":query,"after":after}),
                        Kind::Search {
                            query,
                            after: Some(after),
                        },
                        cx,
                    ) {
                        self.product.palette_loading = true;
                    }
                }
            }
            _ => (),
        }
    }

    pub(super) fn submit_pane_name(&mut self, cx: &mut Context<Self>) {
        if let Some((session, revision)) = self.navigation.rename
            && let Some(dialog) = &self.product.dialog
        {
            let name = dialog.fields[0].text().trim().to_owned();
            if name.len() > 512 || name.chars().count() > 128 || name.chars().any(char::is_control)
            {
                self.product.notice =
                    Some("Имя: до 128 символов и 512 байт UTF-8, без управляющих символов".into());
                return;
            }
            let editor_revision = dialog.fields[0].revision();
            self.navigation_request(json!({"command":"rename_pane","session":session,"name":name,"expected_revision":revision}),Kind::Rename{session,editor_revision},cx);
        }
    }

    pub(super) fn receive_navigation(
        &mut self,
        pending: Pending,
        directory: Option<protocol::PanelDirectory>,
        cx: &mut Context<Self>,
    ) {
        let same_dialog = self.product.dialog.as_ref().map(|dialog| dialog.id) == pending.dialog;
        let current_query = if let Kind::Search { query, .. } = &pending.kind {
            pending.matches_query(self.product.dialog.as_ref(), query)
        } else {
            false
        };
        match pending.kind {
            Kind::Search { query, after } => {
                if !current_query {
                    return;
                }
                self.product.palette_loading = false;
                match directory {
                    Some(page) if page.query == query && page.entries.len() <= 64 => {
                        let mut ids = std::collections::HashSet::new();
                        let valid = page.entries.iter().all(|entry| {
                            !entry.session.is_nil()
                                && !entry.tab_id.is_nil()
                                && !entry.workspace_id.is_nil()
                                && !entry.project_id.is_nil()
                                && entry
                                    .generation
                                    .is_none_or(|generation| !generation.is_nil())
                                && entry.title.len() <= 4096
                                && entry.role.len() <= 8192
                                && ids.insert(entry.session)
                        });
                        if !valid {
                            self.product.palette_error =
                                Some("Некорректные результаты поиска панелей".into());
                            return;
                        }
                        self.product.palette_after = after;
                        self.product.palette_index = 0;
                        self.product.palette_scroll.scroll_to_item(0);
                        self.product.panel_directory = Some(page);
                    }
                    _ => {
                        self.product.palette_error =
                            Some("Ответ поиска не соответствует запросу".into())
                    }
                }
            }
            Kind::Reload => {
                if same_dialog {
                    self.navigation.scope = None;
                    self.service_panel_search(cx);
                }
            }
            Kind::Activate { tab, session } => {
                let confirmed = self.product.catalog.as_ref().is_some_and(|catalog| {
                    catalog.selection.tab_id == Some(tab)
                        && catalog
                            .tabs
                            .iter()
                            .any(|value| value.id == tab && value.active_pane == Some(session))
                });
                if confirmed && same_dialog {
                    self.product.dialog = None;
                } else if !confirmed {
                    self.product.notice =
                        Some("Выбранная панель не подтверждена каталогом; обновите список".into());
                }
            }
            Kind::Rename {
                session,
                editor_revision,
            } => {
                if same_dialog
                    && self
                        .navigation
                        .rename
                        .is_some_and(|(target, _)| target == session)
                {
                    let revision = self
                        .product
                        .catalog
                        .as_ref()
                        .and_then(|catalog| catalog.pane_labels.as_ref())
                        .and_then(|labels| labels.iter().find(|label| label.session == session))
                        .map(|label| label.revision);
                    self.navigation.rename = Some((session, revision));
                }
                if same_dialog {
                    if self
                        .product
                        .dialog
                        .as_ref()
                        .is_some_and(|dialog| dialog.fields[0].revision() == editor_revision)
                    {
                        self.product.dialog = None;
                    }
                    self.product.notice = Some("Имя панели сохранено".into());
                }
            }
        }
        cx.notify();
    }

    pub(super) fn navigation_failed(
        &mut self,
        pending: Pending,
        message: String,
        cx: &mut Context<Self>,
    ) {
        if self.product.dialog.as_ref().map(|dialog| dialog.id) != pending.dialog {
            return;
        }
        if let Kind::Search { query, .. } = pending.kind {
            if self.product.dialog.as_ref().is_some_and(|dialog| {
                dialog.fields[0].text().trim() == query
                    && Some(dialog.fields[0].revision()) == pending.editor_revision
            }) {
                self.product.palette_loading = false;
                self.product.palette_error = Some(message);
            }
        } else {
            if self
                .product
                .dialog
                .as_ref()
                .is_some_and(|dialog| dialog.kind == DialogKind::Palette)
            {
                self.product.palette_error = Some(message.clone());
            }
            self.product.notice = Some(message);
        }
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn local_palette_commands_do_not_wait_for_remote_search_but_mutations_do() {
        let search = Kind::Search {
            query: "profiles".into(),
            after: None,
        };
        assert!(can_choose_palette(&A::OpenCloseTab, Some(&search)));
        assert!(can_choose_palette(&A::OpenPrompts, Some(&search)));
        assert!(can_choose_palette(&A::OpenAgentProfiles, Some(&search)));
        assert!(can_choose_palette(&A::OpenControls, Some(&search)));
        let activation = A::ActivatePane {
            tab: uuid::Uuid::new_v4(),
            session: uuid::Uuid::new_v4(),
        };
        assert!(!can_choose_palette(&activation, Some(&search)));
        assert!(can_choose_palette(&activation, None));
        assert!(!can_choose_palette(
            &A::OpenAgentProfiles,
            Some(&Kind::Reload)
        ));
    }
    #[test]
    fn unicode_words_match_all_terms_and_exact_titles_rank_first() {
        assert!(search_matches("РЕВЬЮ codex", "Codex • Ревью проекта"));
        assert!(!search_matches("ревью shell", "Codex • Ревью проекта"));
        assert!(
            search_score("Review", "Review") > search_score("Review", "Reviewer + implementer")
        );
    }
    #[test]
    fn a_foreign_search_receipt_cannot_consume_a_pending_activation() {
        let request = uuid::Uuid::new_v4();
        let mut state = Navigation {
            pending: Some(Pending {
                request,
                dialog: Some(uuid::Uuid::new_v4()),
                editor_revision: None,
                kind: Kind::Activate {
                    tab: uuid::Uuid::from_u128(1),
                    session: uuid::Uuid::from_u128(2),
                },
            }),
            ..Default::default()
        };
        assert!(state.take_reply(Some(uuid::Uuid::new_v4())).is_none());
        assert!(state.take_reply(Some(request)).is_some());
        assert!(state.take_reply(Some(request)).is_none());
    }
    #[test]
    fn returning_to_the_same_query_cannot_accept_an_older_editor_receipt() {
        let mut dialog = Dialog::new(DialogKind::Palette);
        dialog.fields[0] = "Review".into();
        let pending = Pending {
            request: uuid::Uuid::new_v4(),
            dialog: Some(dialog.id),
            editor_revision: Some(dialog.fields[0].revision()),
            kind: Kind::Search {
                query: "Review".into(),
                after: None,
            },
        };
        assert!(pending.matches_query(Some(&dialog), "Review"));
        dialog.fields[0]
            .replace(Some(0..6), "Other", false)
            .unwrap();
        dialog.fields[0]
            .replace(Some(0..5), "Review", false)
            .unwrap();
        assert!(!pending.matches_query(Some(&dialog), "Review"));
    }
}

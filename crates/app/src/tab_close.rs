//! Close a frozen tab placement; its processes remain in the host registry.
use super::*;
use product::{Dialog, DialogKind, ProductAction as A};

#[derive(Clone, Debug)]
pub(crate) struct Sheet {
    pub dialog: uuid::Uuid,
    pub tab: pigade_storage::Tab,
    pub layout_revision: u64,
    pub selection_revision: u64,
    pub panes: usize,
    pub request: Option<uuid::Uuid>,
    pub inspecting: bool,
    pub uncertain: bool,
}

impl Sheet {
    fn can_confirm(&self) -> bool {
        self.request.is_none() && !self.uncertain
    }

    fn reconcile(&self, catalog: &pigade_storage::Catalog) -> Result<Option<Self>, &'static str> {
        if catalog.tabs.iter().all(|tab| tab.id != self.tab.id) {
            return Ok(None);
        }
        let mut next = Self::freeze(catalog, self.tab.id, self.dialog)
            .ok_or("Не удалось прочитать размещение вкладки; закрытие не подтверждено")?;
        next.uncertain = !self.inspecting;
        Ok(Some(next))
    }

    pub(crate) fn freeze(
        catalog: &pigade_storage::Catalog,
        tab: uuid::Uuid,
        dialog: uuid::Uuid,
    ) -> Option<Self> {
        let tab = catalog.tabs.iter().find(|value| value.id == tab)?.clone();
        let layout: pigade_layout::TabLayout = serde_json::from_str(&tab.layout_json).ok()?;
        Some(Self {
            dialog,
            tab,
            layout_revision: catalog.layout.revision,
            selection_revision: catalog.selection.revision,
            panes: layout.panes().len(),
            request: None,
            inspecting: false,
            uncertain: false,
        })
    }
}

impl CanvasApp {
    pub(super) fn tab_close_action(&mut self, action: A, cx: &mut Context<Self>) {
        // Opening the local review is safe while palette search is in flight.
        // Confirmation and inspection still wait for the writer lane.
        if self.product.busy && !matches!(action, A::OpenCloseTab | A::CloseManagedTab) {
            return;
        }
        match action {
            A::OpenCloseTab | A::CloseManagedTab => {
                let Some(catalog) = &self.product.catalog else {
                    return;
                };
                if catalog.tab_close_schema != Some(1) {
                    self.product.notice =
                        Some("Текущий host не поддерживает закрытие вкладок".into());
                    return;
                }
                let target = if action == A::CloseManagedTab {
                    match &self.product.management_target {
                        Some(catalog_flow::ManagedEntity::Tab(tab)) => Some(tab.id),
                        _ => None,
                    }
                } else {
                    catalog.selection.tab_id
                };
                let dialog = Dialog::new(DialogKind::CloseTab);
                if let Some(sheet) = target.and_then(|tab| Sheet::freeze(catalog, tab, dialog.id)) {
                    self.product.tab_close_sheet = Some(sheet);
                    self.product.dialog = Some(dialog);
                    self.product.notice = None;
                }
            }
            A::ConfirmCloseTab | A::InspectCloseTab => {
                let Some(sheet) = &self.product.tab_close_sheet else {
                    return;
                };
                if sheet.request.is_some() || (action == A::ConfirmCloseTab && !sheet.can_confirm())
                {
                    return;
                }
                if !self.product.dialog.as_ref().is_some_and(|dialog| {
                    dialog.kind == DialogKind::CloseTab && dialog.id == sheet.dialog
                }) {
                    return;
                }
                let request = uuid::Uuid::new_v4();
                let inspecting = action == A::InspectCloseTab;
                let command = if inspecting {
                    json!({"command":"catalog","_ui_request":request})
                } else {
                    json!({"command":"close_tab","_ui_request":request,"tab":sheet.tab.id,
                        "expected_revision":sheet.tab.revision,"expected_layout_revision":sheet.layout_revision,
                        "expected_selection_revision":sheet.selection_revision})
                };
                if self.try_product_command(command, cx)
                    && let Some(sheet) = &mut self.product.tab_close_sheet
                {
                    sheet.request = Some(request);
                    sheet.inspecting = inspecting;
                }
            }
            _ => (),
        }
        cx.notify();
    }

    pub(super) fn receive_tab_close(&mut self, request: Option<uuid::Uuid>) {
        let Some(previous) = self
            .product
            .tab_close_sheet
            .as_ref()
            .filter(|sheet| sheet.request.is_some() && sheet.request == request)
            .cloned()
        else {
            return;
        };
        let Some(catalog) = &self.product.catalog else {
            return;
        };
        let same_dialog = self
            .product
            .dialog
            .as_ref()
            .is_some_and(|dialog| dialog.id == previous.dialog);
        match previous.reconcile(catalog) {
            Ok(Some(refreshed)) => {
                self.product.tab_close_sheet = Some(refreshed);
                self.product.notice = Some(
                    if previous.inspecting {
                        "Вкладка доступна. Проверьте обновлённые сведения перед закрытием."
                    } else {
                        "Закрытие не подтверждено; обновите состояние."
                    }
                    .into(),
                );
            }
            Ok(None) => {
                self.canvas_offsets.remove(&previous.tab.id);
                self.canvas_dirty.remove(&previous.tab.id);
                if self.canvas_tab == Some(previous.tab.id) {
                    self.canvas_tab = None;
                }
                self.product.tab_close_sheet = None;
                if same_dialog {
                    self.product.dialog = None;
                }
                self.product.notice = Some(format!(
                    "Вкладка «{}» закрыта. Её CLI доступны через «Сессии».",
                    previous.tab.name
                ));
            }
            Err(error) => {
                self.tab_close_failed(request);
                self.product.notice = Some(error.into());
            }
        }
    }

    pub(super) fn tab_close_failed(&mut self, request: Option<uuid::Uuid>) {
        if let Some(sheet) = &mut self.product.tab_close_sheet
            && sheet.request.is_some()
            && sheet.request == request
        {
            sheet.request = None;
            sheet.uncertain = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Sheet;
    use serde_json::json;

    fn catalog() -> pigade_storage::Catalog {
        serde_json::from_value(json!({"projects":[],"workspaces":[],"tabs":[{
            "id":uuid::Uuid::from_u128(1),"workspace_id":uuid::Uuid::from_u128(2),"name":"Development",
            "revision":4,"layout_json":serde_json::to_string(&pigade_layout::TabLayout::default()).unwrap(),"active_pane":null}],
            "selection":pigade_storage::Selection::default(),"layout":{"json":"null","revision":7}})).unwrap()
    }

    #[test]
    fn unknown_close_requires_explicit_read_before_another_confirmation() {
        let mut catalog = catalog();
        let mut sheet =
            Sheet::freeze(&catalog, uuid::Uuid::from_u128(1), uuid::Uuid::new_v4()).unwrap();
        assert!(sheet.can_confirm());
        sheet.request = Some(uuid::Uuid::new_v4());
        assert!(!sheet.can_confirm());
        sheet.request = None;
        sheet.uncertain = true;
        assert!(!sheet.can_confirm());
        // Only an explicit inspect receipt authorizes a new frozen confirmation.
        assert!(!sheet.reconcile(&catalog).unwrap().unwrap().can_confirm());
        sheet.inspecting = true;
        catalog.tabs[0].revision += 1;
        let refreshed = sheet.reconcile(&catalog).unwrap().unwrap();
        assert!(refreshed.can_confirm());
        assert_eq!(refreshed.tab.revision, 5);
        assert_eq!(refreshed.dialog, sheet.dialog);
        catalog.tabs.clear();
        assert!(sheet.reconcile(&catalog).unwrap().is_none());
    }

    #[test]
    fn malformed_existing_tab_is_not_reported_as_closed() {
        let mut catalog = catalog();
        let sheet =
            Sheet::freeze(&catalog, uuid::Uuid::from_u128(1), uuid::Uuid::new_v4()).unwrap();
        catalog.tabs[0].layout_json = "unknown-format".into();
        assert!(sheet.reconcile(&catalog).is_err());
    }
}

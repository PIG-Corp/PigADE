//! Host command dispatch for explicit, revision-checked tab ordering.
use super::*;
use product::ProductAction as A;

impl CanvasApp {
    pub(super) fn tab_order_action(&mut self, action: A, cx: &mut Context<Self>) {
        let (direction, refresh) = match action {
            A::MoveManagedTab(direction) => (Some(direction), false),
            A::RefreshTabOrder => (None, true),
            _ => return,
        };
        let Some(dialog) = self
            .product
            .dialog
            .as_ref()
            .filter(|d| d.kind == product::DialogKind::Manage)
            .cloned()
        else {
            return;
        };
        let Some(crate::catalog_flow::ManagedEntity::Tab(tab)) =
            self.product.management_target.as_ref()
        else {
            return;
        };
        let tab = tab.clone();
        if self.tab_order_pending.is_some() {
            return;
        }
        if refresh {
            let Some(catalog) = self.product.catalog.as_ref() else {
                return;
            };
            let workspace_revision = catalog
                .workspaces
                .iter()
                .find(|workspace| workspace.id == tab.workspace_id)
                .map_or(0, |workspace| workspace.revision);
            let known = catalog.tab_orders.as_ref().and_then(|orders| {
                orders
                    .iter()
                    .find(|order| order.workspace_id == tab.workspace_id)
            });
            let request = uuid::Uuid::new_v4();
            let pending = crate::tab_order::Pending {
                request,
                dialog: dialog.id,
                tab: tab.id,
                workspace: tab.workspace_id,
                ordered_ids: known.map_or_else(Vec::new, |order| order.ordered_ids.clone()),
                expected_workspace_revision: workspace_revision,
                expected_revision: known.map_or(0, |order| order.revision),
                refresh: true,
            };
            if self.try_product_command(
                serde_json::json!({"command":"catalog","_ui_request":request}),
                cx,
            ) {
                self.tab_order_pending = Some(pending);
            }
            cx.notify();
            return;
        }
        if self.product.tab_order_error.is_some() {
            return;
        }
        let Some(catalog) = self.product.catalog.as_ref() else {
            return;
        };
        let Some(workspace) = catalog.workspaces.iter().find(|workspace| {
            workspace.id == tab.workspace_id
                && workspace.status == pigade_storage::WorkspaceStatus::Ready
        }) else {
            self.product.tab_order_error =
                Some("Перемещение доступно только для вкладок готового воркспейса".into());
            cx.notify();
            return;
        };
        if catalog.tabs.iter().find(|current| current.id == tab.id) != Some(&tab) {
            self.product.tab_order_error =
                Some("Сведения о вкладке изменились; обновите состояние".into());
            cx.notify();
            return;
        }
        let proposed =
            match crate::tab_order::propose(catalog, tab.id, direction.unwrap_or_default()) {
                Ok(Some(proposed)) => proposed,
                Ok(None) => return,
                Err(error) => {
                    self.product.tab_order_error = Some(error.into());
                    cx.notify();
                    return;
                }
            };
        let request = uuid::Uuid::new_v4();
        let pending = crate::tab_order::Pending {
            request,
            dialog: dialog.id,
            tab: tab.id,
            workspace: workspace.id,
            ordered_ids: proposed.ordered_ids.clone(),
            expected_workspace_revision: proposed.expected_workspace_revision,
            expected_revision: proposed.expected_revision,
            refresh: false,
        };
        if self.try_product_command(
            serde_json::json!({"command":"set_tab_order","_ui_request":request,"workspace":proposed.workspace,"expected_workspace_revision":proposed.expected_workspace_revision,"expected_revision":proposed.expected_revision,"ordered_ids":proposed.ordered_ids}),
            cx,
        ) {
            self.tab_order_pending = Some(pending);
        }
        cx.notify();
    }

    /// Called after a matching catalog receipt has been applied, or for a
    /// matching command failure. It never changes which dialog is open.
    pub(super) fn receive_tab_order(&mut self, request: Option<uuid::Uuid>, error: Option<&str>) {
        let Some(pending) = self
            .tab_order_pending
            .as_ref()
            .filter(|pending| crate::tab_order::pending_matches(pending, request))
            .cloned()
        else {
            return;
        };
        self.tab_order_pending = None;
        let same_dialog = self.product.dialog.as_ref().is_some_and(|dialog| {
            dialog.id == pending.dialog && dialog.kind == product::DialogKind::Manage
        });
        if !same_dialog {
            return;
        }
        if let Some(error) = error {
            self.product.tab_order_error = Some(error.into());
            return;
        }
        let Some(catalog) = self.product.catalog.as_ref() else {
            self.product.tab_order_error =
                Some("Состояние вкладок недоступно; обновите снова".into());
            return;
        };
        let verified = if pending.refresh {
            crate::tab_order::receipt_matches_refresh(catalog, &pending)
        } else {
            crate::tab_order::receipt_matches_move(catalog, &pending)
        };
        if !verified {
            let unsupported = pending.refresh
                && catalog.tabs.iter().any(|tab| tab.id == pending.tab)
                && crate::tab_order::position(catalog, pending.tab).is_none();
            self.product.tab_order_error = Some(if unsupported {
                "Текущий host не сообщил поддерживаемый порядок вкладок".into()
            } else if pending.refresh {
                "Обновление не подтвердило целостный порядок вкладок; повторите обновление".into()
            } else {
                "Перемещение не подтверждено host; обновите порядок вкладок перед следующим действием".into()
            });
            return;
        }
        self.product.management_target = catalog
            .tabs
            .iter()
            .find(|tab| tab.id == pending.tab)
            .cloned()
            .map(crate::catalog_flow::ManagedEntity::Tab);
        if pending.refresh {
            self.product.tab_order_error =
                self.product
                    .management_target
                    .as_ref()
                    .and_then(|target| match target {
                        crate::catalog_flow::ManagedEntity::Tab(tab)
                            if crate::tab_order::position(catalog, tab.id).is_none() =>
                        {
                            Some("Текущий host не сообщил поддерживаемый порядок вкладок".into())
                        }
                        _ => None,
                    });
        } else {
            self.product.tab_order_error = None;
        }
        let menu_count = self.product.menu_entries().len();
        if let Some(dialog) = self.product.dialog.as_mut() {
            if dialog.id == pending.dialog && dialog.kind == product::DialogKind::Manage {
                dialog.active_field = dialog.active_field.min(menu_count.saturating_sub(1));
            }
        }
    }
}

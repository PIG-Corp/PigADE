//! Pure validation and frozen request model for durable workspace tab order.
use pigade_storage::{Catalog, TabOrder, WorkspaceStatus};
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Move {
    pub workspace: Uuid,
    pub expected_workspace_revision: u64,
    pub expected_revision: u64,
    pub ordered_ids: Vec<Uuid>,
}

#[derive(Clone, Debug)]
pub(crate) struct Pending {
    pub request: Uuid,
    pub dialog: Uuid,
    pub tab: Uuid,
    pub workspace: Uuid,
    pub ordered_ids: Vec<Uuid>,
    pub expected_workspace_revision: u64,
    pub expected_revision: u64,
    pub refresh: bool,
}

pub(crate) fn pending_matches(pending: &Pending, request: Option<Uuid>) -> bool {
    request == Some(pending.request)
}

fn order_for_workspace(catalog: &Catalog, workspace_id: Uuid) -> Option<&TabOrder> {
    let order = catalog
        .tab_orders
        .as_ref()?
        .iter()
        .find(|order| order.workspace_id == workspace_id)?;
    if order.schema != 1 || order.revision == 0 {
        return None;
    }
    let members: Vec<_> = catalog
        .tabs
        .iter()
        .filter(|item| item.workspace_id == workspace_id)
        .map(|item| item.id)
        .collect();
    validate(&order.ordered_ids, &members).ok()?;
    Some(order)
}

fn current(
    catalog: &Catalog,
    tab_id: Uuid,
) -> Option<(&pigade_storage::Tab, &pigade_storage::Workspace, &TabOrder)> {
    let tab = catalog.tabs.iter().find(|tab| tab.id == tab_id)?;
    let workspace = catalog
        .workspaces
        .iter()
        .find(|workspace| workspace.id == tab.workspace_id)?;
    if workspace.status != WorkspaceStatus::Ready {
        return None;
    }
    let order = order_for_workspace(catalog, workspace.id)?;
    Some((tab, workspace, order))
}

pub(crate) fn receipt_matches_move(catalog: &Catalog, pending: &Pending) -> bool {
    let expected_revision = pending.expected_revision.checked_add(1);
    let workspace = catalog
        .workspaces
        .iter()
        .find(|workspace| workspace.id == pending.workspace);
    let tab_exists = catalog
        .tabs
        .iter()
        .any(|tab| tab.id == pending.tab && tab.workspace_id == pending.workspace);
    workspace.is_some_and(|workspace| workspace.revision == pending.expected_workspace_revision)
        && tab_exists
        && expected_revision.is_some_and(|revision| {
            order_for_workspace(catalog, pending.workspace).is_some_and(|order| {
                order.revision == revision && order.ordered_ids == pending.ordered_ids
            })
        })
}

pub(crate) fn receipt_matches_refresh(catalog: &Catalog, pending: &Pending) -> bool {
    let Some(tab) = catalog.tabs.iter().find(|tab| tab.id == pending.tab) else {
        return true;
    };
    let Some(workspace) = catalog
        .workspaces
        .iter()
        .find(|workspace| workspace.id == tab.workspace_id)
    else {
        return false;
    };
    let Some(order) = order_for_workspace(catalog, tab.workspace_id) else {
        return false;
    };
    workspace.revision
        >= if tab.workspace_id == pending.workspace {
            pending.expected_workspace_revision
        } else {
            0
        }
        && order.revision
            >= if tab.workspace_id == pending.workspace {
                pending.expected_revision
            } else {
                1
            }
}

fn validate(order: &[Uuid], members: &[Uuid]) -> Result<(), &'static str> {
    if order.len() != members.len() {
        return Err("Порядок вкладок не совпадает с составом воркспейса");
    }
    if order.iter().chain(members).any(Uuid::is_nil) {
        return Err("Порядок вкладок содержит пустой ID");
    }
    let unique: std::collections::HashSet<_> = order.iter().copied().collect();
    let existing: std::collections::HashSet<_> = members.iter().copied().collect();
    if unique.len() != order.len() || existing.len() != members.len() || unique != existing {
        return Err("Порядок вкладок повреждён; обновите состояние");
    }
    Ok(())
}

pub(crate) fn position(catalog: &Catalog, tab: Uuid) -> Option<(usize, usize)> {
    let tab = catalog.tabs.iter().find(|value| value.id == tab)?;
    let order = order_for_workspace(catalog, tab.workspace_id)?;
    Some((
        order.ordered_ids.iter().position(|id| *id == tab)? + 1,
        order.ordered_ids.len(),
    ))
}

pub(crate) fn can_move(catalog: &Catalog, tab: Uuid, direction: i32) -> bool {
    let Some((_, _, order)) = current(catalog, tab) else {
        return false;
    };
    let Some(index) = order.ordered_ids.iter().position(|id| *id == tab) else {
        return false;
    };
    match direction {
        -1 => index > 0,
        1 => index + 1 < order.ordered_ids.len(),
        _ => false,
    }
}

pub(crate) fn propose(
    catalog: &Catalog,
    tab_id: Uuid,
    direction: i32,
) -> Result<Option<Move>, &'static str> {
    let (tab, workspace, order) = current(catalog, tab_id)
        .ok_or("Порядок вкладок не поддерживается или недоступен; обновите состояние")?;
    let mut ordered_ids = order.ordered_ids.clone();
    let index = ordered_ids
        .iter()
        .position(|id| *id == tab_id)
        .ok_or("Вкладка отсутствует в сохранённом порядке")?;
    let target = match direction {
        -1 if index > 0 => index - 1,
        1 if index + 1 < ordered_ids.len() => index + 1,
        -1 | 1 => return Ok(None),
        _ => return Err("Неизвестное направление перемещения"),
    };
    ordered_ids.swap(index, target);
    Ok(Some(Move {
        workspace: workspace.id,
        expected_workspace_revision: workspace.revision,
        expected_revision: order.revision,
        ordered_ids,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const WS: Uuid = Uuid::from_u128(10);
    const FIRST: Uuid = Uuid::from_u128(1);
    const SECOND: Uuid = Uuid::from_u128(2);
    const THIRD: Uuid = Uuid::from_u128(3);

    fn catalog(order: Option<(u64, Vec<Uuid>)>) -> Catalog {
        let tabs = [FIRST, SECOND, THIRD]
            .into_iter()
            .map(|id| {
                json!({
                    "id": id, "workspace_id": WS, "name": "Tab", "layout_json": "null",
                    "active_pane": null, "revision": 1
                })
            })
            .collect::<Vec<_>>();
        let mut value: Catalog = serde_json::from_value(json!({
            "projects": [],
            "workspaces": [{
                "id": WS, "project_id": Uuid::from_u128(20), "task": "Work",
                "cwd": "/work", "mode": "shared_checkout", "status": "ready",
                "error": null, "context_revision": null, "revision": 4
            }],
            "tabs": tabs,
            "selection": pigade_storage::Selection::default(),
            "layout": {"json": "null", "revision": 1}
        }))
        .unwrap();
        value.tab_orders = order.map(|(revision, ordered_ids)| {
            vec![TabOrder {
                schema: 1,
                workspace_id: WS,
                revision,
                ordered_ids,
            }]
        });
        value
    }

    #[test]
    fn move_is_one_adjacent_swap_and_edges_are_noops() {
        let value = catalog(Some((3, vec![FIRST, SECOND, THIRD])));
        assert_eq!(position(&value, SECOND), Some((2, 3)));
        assert!(!can_move(&value, FIRST, -1));
        assert_eq!(propose(&value, FIRST, -1).unwrap(), None);
        assert_eq!(
            propose(&value, SECOND, 1).unwrap().unwrap().ordered_ids,
            vec![FIRST, THIRD, SECOND]
        );
        assert_eq!(
            propose(&value, FIRST, 2),
            Err("Неизвестное направление перемещения")
        );
    }

    #[test]
    fn old_hosts_and_malformed_permutations_never_offer_a_move() {
        let legacy = catalog(None);
        assert!(!can_move(&legacy, FIRST, 1));
        assert!(propose(&legacy, FIRST, 1).is_err());

        let duplicate = catalog(Some((3, vec![FIRST, FIRST, THIRD])));
        assert!(!can_move(&duplicate, FIRST, 1));
        let foreign = catalog(Some((3, vec![FIRST, SECOND, Uuid::from_u128(99)])));
        assert!(!can_move(&foreign, FIRST, 1));
    }

    #[test]
    fn move_receipt_requires_frozen_revisions_workspace_membership_and_order() {
        let pending = Pending {
            request: Uuid::from_u128(30),
            dialog: Uuid::from_u128(31),
            tab: SECOND,
            workspace: WS,
            ordered_ids: vec![FIRST, THIRD, SECOND],
            expected_workspace_revision: 4,
            expected_revision: 3,
            refresh: false,
        };
        assert!(receipt_matches_move(
            &catalog(Some((4, pending.ordered_ids.clone()))),
            &pending
        ));
        assert!(!pending_matches(&pending, Some(Uuid::from_u128(99))));
        assert!(pending_matches(&pending, Some(pending.request)));
        assert!(!receipt_matches_move(
            &catalog(Some((5, pending.ordered_ids.clone()))),
            &pending
        ));
        let mut wrong_workspace = catalog(Some((4, pending.ordered_ids.clone())));
        wrong_workspace.tabs[1].workspace_id = Uuid::from_u128(11);
        assert!(!receipt_matches_move(&wrong_workspace, &pending));
        assert!(!receipt_matches_move(
            &catalog(Some((4, vec![FIRST, SECOND, THIRD]))),
            &pending
        ));
    }

    #[test]
    fn refresh_receipt_rejects_bad_order_but_accepts_removed_tab() {
        let pending = Pending {
            request: Uuid::from_u128(30),
            dialog: Uuid::from_u128(31),
            tab: SECOND,
            workspace: WS,
            ordered_ids: vec![FIRST, SECOND, THIRD],
            expected_workspace_revision: 4,
            expected_revision: 3,
            refresh: true,
        };
        assert!(receipt_matches_refresh(
            &catalog(Some((3, vec![FIRST, SECOND, THIRD]))),
            &pending
        ));
        assert!(!receipt_matches_refresh(
            &catalog(Some((3, vec![FIRST, FIRST, THIRD]))),
            &pending
        ));
        let mut removed = catalog(Some((3, vec![FIRST, SECOND, THIRD])));
        removed.tabs.retain(|tab| tab.id != SECOND);
        assert!(receipt_matches_refresh(&removed, &pending));
    }
}

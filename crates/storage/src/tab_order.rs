//! Durable ordering of tabs within each workspace.
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Ordered tab IDs for one workspace. `revision` changes only when the order
/// or membership changes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TabOrder {
    pub schema: u16,
    pub workspace_id: Uuid,
    pub revision: u64,
    pub ordered_ids: Vec<Uuid>,
}

pub(crate) const TAB_ORDER_SCHEMA: u16 = 1;
pub(crate) const MAX_TAB_ORDER_BYTES: usize = 1024 * 1024;

pub(crate) fn parse_order(workspace_id: &str, revision: i64, json: &str) -> Result<TabOrder> {
    ensure!(revision > 0, "invalid stored tab order revision");
    ensure!(
        json.len() <= MAX_TAB_ORDER_BYTES,
        "stored tab order exceeds byte bound"
    );
    let ids: Vec<String> = serde_json::from_str(json).context("invalid stored tab order JSON")?;
    ensure!(ids.len() <= 16_384, "stored tab order exceeds tab capacity");
    let ordered_ids = ids
        .into_iter()
        .map(|id| Uuid::parse_str(&id).context("invalid stored tab order ID"))
        .collect::<Result<Vec<_>>>()?;
    let order = TabOrder {
        schema: TAB_ORDER_SCHEMA,
        workspace_id: Uuid::parse_str(workspace_id)?,
        revision: revision as u64,
        ordered_ids,
    };
    ensure!(
        order.ordered_ids.iter().all(|id| !id.is_nil()),
        "stored tab order contains nil ID"
    );
    let unique: std::collections::HashSet<_> = order.ordered_ids.iter().copied().collect();
    ensure!(
        unique.len() == order.ordered_ids.len(),
        "stored tab order contains duplicate ID"
    );
    Ok(order)
}

pub(crate) fn validate_order(order: &[Uuid], members: &[Uuid]) -> Result<()> {
    ensure!(order.len() <= 16_384, "tab order exceeds tab capacity");
    ensure!(
        order.iter().all(|id| !id.is_nil()),
        "tab order cannot contain nil IDs"
    );
    let ordered: std::collections::HashSet<_> = order.iter().copied().collect();
    ensure!(
        ordered.len() == order.len(),
        "tab order contains duplicate IDs"
    );
    let existing: std::collections::HashSet<_> = members.iter().copied().collect();
    ensure!(
        ordered == existing,
        "tab order must be an exact workspace tab permutation"
    );
    let json = serde_json::to_vec(&order)?;
    ensure!(
        json.len() <= MAX_TAB_ORDER_BYTES,
        "tab order exceeds byte bound"
    );
    Ok(())
}

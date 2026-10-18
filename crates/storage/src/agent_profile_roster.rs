//! Durable ordering and pin state for reusable agent profiles.
use anyhow::{Context, Result, ensure};
use rusqlite::{Transaction, params};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use uuid::Uuid;

use crate::{MetadataStore, catalog::Catalog};

pub const MAX_AGENT_PROFILE_ROSTER_IDS: usize = 128;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentProfileRoster {
    pub schema: u16,
    pub revision: u64,
    pub ordered_ids: Vec<Uuid>,
    pub pinned_ids: Vec<Uuid>,
}

pub(crate) fn validate_roster(roster: &AgentProfileRoster, profile_ids: &[Uuid]) -> Result<()> {
    ensure!(
        roster.schema == 1,
        "unsupported agent profile roster schema"
    );
    ensure!(
        roster.revision > 0 && roster.revision <= i64::MAX as u64,
        "invalid roster revision"
    );
    ensure!(
        roster.ordered_ids.len() <= MAX_AGENT_PROFILE_ROSTER_IDS
            && roster.pinned_ids.len() <= MAX_AGENT_PROFILE_ROSTER_IDS,
        "agent profile roster exceeds bound"
    );
    let mut ordered = HashSet::new();
    for id in &roster.ordered_ids {
        ensure!(
            !id.is_nil() && ordered.insert(*id),
            "invalid or duplicate roster profile ID"
        );
    }
    let expected: HashSet<_> = profile_ids.iter().copied().collect();
    ensure!(
        ordered == expected && roster.ordered_ids.len() == profile_ids.len(),
        "roster must contain each current profile exactly once"
    );
    let mut pinned = HashSet::new();
    for id in &roster.pinned_ids {
        ensure!(
            !id.is_nil() && pinned.insert(*id) && ordered.contains(id),
            "roster pin must be a unique current profile ID"
        );
    }
    Ok(())
}

pub(crate) fn read(
    connection: &rusqlite::Connection,
    profile_ids: &[Uuid],
) -> Result<AgentProfileRoster> {
    let (schema, revision, ordered, pinned): (u16, i64, String, String) = connection.query_row(
        "SELECT schema,revision,ordered_ids,pinned_ids FROM agent_profile_roster WHERE singleton=1
         AND length(CAST(ordered_ids AS BLOB))<=8192 AND length(CAST(pinned_ids AS BLOB))<=8192", [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    ).context("agent profile roster is missing or exceeds its byte bound")?;
    ensure!(
        ordered.len() <= 8192 && pinned.len() <= 8192,
        "stored roster exceeds byte bound"
    );
    let roster = AgentProfileRoster {
        schema,
        revision: u64::try_from(revision).context("invalid roster revision")?,
        ordered_ids: serde_json::from_str(&ordered)?,
        pinned_ids: serde_json::from_str(&pinned)?,
    };
    validate_roster(&roster, profile_ids)?;
    Ok(roster)
}

pub(crate) fn set_in_transaction(
    tx: &Transaction<'_>,
    expected_revision: u64,
    ordered_ids: Vec<Uuid>,
    pinned_ids: Vec<Uuid>,
) -> Result<()> {
    ensure!(
        expected_revision > 0 && expected_revision <= i64::MAX as u64,
        "invalid roster expected revision"
    );
    let ids = profile_ids(tx)?;
    let current = read(tx, &ids)?;
    ensure!(
        current.revision == expected_revision,
        "agent profile roster revision conflict"
    );
    let proposed = AgentProfileRoster {
        schema: 1,
        revision: expected_revision,
        ordered_ids,
        pinned_ids,
    };
    validate_roster(&proposed, &ids)?;
    if current.ordered_ids == proposed.ordered_ids && current.pinned_ids == proposed.pinned_ids {
        return Ok(());
    }
    let roster = AgentProfileRoster {
        revision: expected_revision
            .checked_add(1)
            .filter(|revision| *revision <= i64::MAX as u64)
            .context("roster revision overflow")?,
        ..proposed
    };
    let ordered = serde_json::to_string(&roster.ordered_ids)?;
    let pinned = serde_json::to_string(&roster.pinned_ids)?;
    let changed = tx.execute("UPDATE agent_profile_roster SET schema=1,revision=?1,ordered_ids=?2,pinned_ids=?3 WHERE singleton=1 AND revision=?4", params![roster.revision as i64, ordered, pinned, expected_revision as i64])?;
    ensure!(changed == 1, "agent profile roster revision conflict");
    Ok(())
}

pub(crate) fn profile_ids(connection: &rusqlite::Connection) -> Result<Vec<Uuid>> {
    let mut stmt = connection.prepare("SELECT id FROM agent_profiles ORDER BY rowid")?;
    let mut ids = Vec::new();
    for value in stmt.query_map([], |row| row.get::<_, String>(0))? {
        ensure!(
            ids.len() < MAX_AGENT_PROFILE_ROSTER_IDS,
            "too many agent profiles"
        );
        ids.push(Uuid::parse_str(&value?)?);
    }
    ensure!(
        ids.len() <= MAX_AGENT_PROFILE_ROSTER_IDS,
        "too many agent profiles"
    );
    Ok(ids)
}

pub(crate) fn profile_created(tx: &Transaction<'_>, id: Uuid) -> Result<()> {
    let (revision, ordered): (i64, String) = tx.query_row(
        "SELECT revision,ordered_ids FROM agent_profile_roster WHERE singleton=1",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let revision = u64::try_from(revision)
        .context("invalid roster revision")?
        .checked_add(1)
        .filter(|v| *v <= i64::MAX as u64)
        .context("roster revision overflow")?;
    let mut ordered: Vec<Uuid> = serde_json::from_str(&ordered)?;
    ordered.push(id);
    tx.execute(
        "UPDATE agent_profile_roster SET revision=?1,ordered_ids=?2 WHERE singleton=1",
        params![revision as i64, serde_json::to_string(&ordered)?],
    )?;
    Ok(())
}

pub(crate) fn profile_deleted(tx: &Transaction<'_>, id: Uuid) -> Result<()> {
    let (revision, ordered, pinned): (i64, String, String) = tx.query_row(
        "SELECT revision,ordered_ids,pinned_ids FROM agent_profile_roster WHERE singleton=1",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    let revision = u64::try_from(revision)
        .context("invalid roster revision")?
        .checked_add(1)
        .filter(|v| *v <= i64::MAX as u64)
        .context("roster revision overflow")?;
    let mut ordered: Vec<Uuid> = serde_json::from_str(&ordered)?;
    ordered.retain(|value| *value != id);
    let mut pinned: Vec<Uuid> = serde_json::from_str(&pinned)?;
    pinned.retain(|value| *value != id);
    tx.execute("UPDATE agent_profile_roster SET revision=?1,ordered_ids=?2,pinned_ids=?3 WHERE singleton=1", params![revision as i64, serde_json::to_string(&ordered)?, serde_json::to_string(&pinned)?])?;
    Ok(())
}

impl MetadataStore {
    pub fn set_agent_profile_roster(
        &mut self,
        expected_revision: u64,
        ordered_ids: Vec<Uuid>,
        pinned_ids: Vec<Uuid>,
    ) -> Result<Catalog> {
        let tx = self
            .connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        set_in_transaction(&tx, expected_revision, ordered_ids, pinned_ids)?;
        let catalog = crate::catalog::read_catalog_for_roster(&tx)?;
        tx.commit()?;
        Ok(catalog)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AgentProfile, AgentProfileDelete, CatalogBatch, CatalogWrite, LaunchSettings};

    fn seed(store: &mut MetadataStore) -> Result<(Uuid, Uuid)> {
        let ids = (Uuid::new_v4(), Uuid::new_v4());
        store.apply_catalog(CatalogBatch {
            agent_profiles: [ids.0, ids.1]
                .into_iter()
                .map(|id| CatalogWrite {
                    value: AgentProfile {
                        id,
                        name: format!("Reviewer {id}"),
                        revision: 0,
                        settings: LaunchSettings {
                            provider: "codex".into(),
                            role: String::new(),
                            instructions: String::new(),
                            deliver_context: false,
                            engine: Default::default(),
                        },
                    },
                    expected_revision: None,
                })
                .collect(),
            ..Default::default()
        })?;
        Ok(ids)
    }

    #[test]
    fn invalid_stored_membership_cannot_be_silently_repaired_by_set_or_delete() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let mut store = MetadataStore::open(dir.path().join("roster.sqlite"))?;
        let (a, b) = seed(&mut store)?;
        store.connection.execute(
            "UPDATE agent_profile_roster SET ordered_ids=?1",
            [serde_json::to_string(&vec![a])?],
        )?;
        assert!(
            store
                .set_agent_profile_roster(3, vec![a, b], vec![])
                .is_err()
        );
        assert!(
            store
                .apply_catalog(CatalogBatch {
                    remove_agent_profiles: vec![AgentProfileDelete {
                        id: b,
                        expected_revision: 1
                    }],
                    ..Default::default()
                })
                .is_err()
        );
        assert_eq!(profile_ids(&store.connection)?, vec![a, b]);
        let stored: String = store.connection.query_row(
            "SELECT ordered_ids FROM agent_profile_roster",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(stored, serde_json::to_string(&vec![a])?);
        Ok(())
    }

    #[test]
    fn overflow_and_oversized_stored_roster_refuse_without_changing_profiles() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let mut store = MetadataStore::open(dir.path().join("roster.sqlite"))?;
        let (a, b) = seed(&mut store)?;
        store
            .connection
            .execute("UPDATE agent_profile_roster SET revision=?1", [i64::MAX])?;
        assert!(
            store
                .set_agent_profile_roster(i64::MAX as u64, vec![b, a], vec![a])
                .is_err()
        );
        assert_eq!(
            store.catalog()?.agent_profile_roster.unwrap().ordered_ids,
            vec![a, b]
        );
        store.connection.execute(
            "UPDATE agent_profile_roster SET ordered_ids=?1",
            [" ".repeat(8193)],
        )?;
        assert!(store.catalog().is_err());
        assert!(
            store
                .set_agent_profile_roster(i64::MAX as u64, vec![a, b], vec![])
                .is_err()
        );
        assert_eq!(profile_ids(&store.connection)?, vec![a, b]);
        Ok(())
    }
}

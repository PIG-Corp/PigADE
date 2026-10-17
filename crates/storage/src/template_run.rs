//! Durable, opaque run snapshots for configured layout templates.
//!
//! Payload bytes remain uninterpreted here. The host is the trust boundary for
//! what it serializes; storage enforces only size, identity, status, and CAS.
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{CatalogWrite, MetadataStore};

const MAX_RUNS: usize = 32;
const MAX_RUN_PAYLOAD_BYTES: usize = 2 * 1024 * 1024;
const MAX_ACTIVE_PAYLOAD_BYTES: usize = 16 * 1024 * 1024;
type RunIdentity = (Uuid, Uuid, Option<Uuid>, String, u16, String, usize, u64);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredTemplateRun {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub tab_id: Uuid,
    pub template_id: Option<Uuid>,
    pub name: String,
    pub slots: u16,
    pub status: String,
    pub payload: String,
    pub revision: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TemplateRunSummary {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub tab_id: Uuid,
    pub template_id: Option<Uuid>,
    pub name: String,
    pub slots: u16,
    pub status: String,
    pub revision: u64,
    pub ordinal: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TemplateRunPage {
    pub entries: Vec<TemplateRunSummary>,
    pub next_before: Option<u64>,
}

pub(crate) fn migrate_template_runs(connection: &Connection) -> Result<()> {
    connection.execute_batch(
        "CREATE TABLE template_runs (
           ordinal INTEGER PRIMARY KEY AUTOINCREMENT,
           id TEXT NOT NULL UNIQUE,
           workspace_id TEXT NOT NULL,
           tab_id TEXT NOT NULL,
           template_id TEXT,
           name TEXT NOT NULL CHECK(length(CAST(name AS BLOB)) BETWEEN 1 AND 256),
           slots INTEGER NOT NULL CHECK(slots BETWEEN 1 AND 32),
           status TEXT NOT NULL CHECK(status IN ('prepared','running','complete','cancelled')),
           payload TEXT NOT NULL CHECK(length(CAST(payload AS BLOB))<=2097152),
           revision INTEGER NOT NULL CHECK(revision>0));
         CREATE INDEX template_runs_active_ordinal
           ON template_runs(status,ordinal DESC);",
    )?;
    Ok(())
}

fn validate_run(run: &StoredTemplateRun) -> Result<()> {
    ensure!(
        !run.name.trim().is_empty() && run.name.len() <= 256 && !run.name.contains('\0'),
        "template run name is empty or exceeds bounds"
    );
    ensure!(
        (1..=32).contains(&run.slots),
        "template run slots outside 1..=32"
    );
    ensure!(
        matches!(
            run.status.as_str(),
            "prepared" | "running" | "complete" | "cancelled"
        ),
        "invalid template run status"
    );
    ensure!(
        run.payload.len() <= MAX_RUN_PAYLOAD_BYTES,
        "template run payload exceeds 2 MiB"
    );
    serde_json::from_str::<serde_json::Value>(&run.payload)
        .map_err(|error| anyhow::anyhow!("invalid template run payload JSON: {error}"))?;
    Ok(())
}

fn next_revision(revision: u64, expected: Option<u64>) -> Result<i64> {
    ensure!(
        revision == expected.unwrap_or(0),
        "template run revision must match expected revision"
    );
    match expected {
        None => Ok(1),
        Some(value) => {
            ensure!(
                value > 0 && value < i64::MAX as u64,
                "invalid template run revision"
            );
            Ok((value + 1) as i64)
        }
    }
}

fn active(status: &str) -> bool {
    matches!(status, "prepared" | "running")
}

fn validate_summary(run: &TemplateRunSummary) -> Result<()> {
    ensure!(
        !run.name.trim().is_empty() && run.name.len() <= 256 && !run.name.contains('\0'),
        "stored template run name is empty or exceeds bounds"
    );
    ensure!(
        (1..=32).contains(&run.slots),
        "stored template run slots outside 1..=32"
    );
    ensure!(active(&run.status), "stored template run is not active");
    Ok(())
}

fn read_previous_identity(tx: &Transaction<'_>, id: Uuid) -> Result<Option<RunIdentity>> {
    tx.query_row(
        "SELECT workspace_id,tab_id,template_id,name,slots,status,
                length(CAST(payload AS BLOB)),revision
           FROM template_runs WHERE id=?1",
        [id.to_string()],
        |row| {
            let workspace: String = row.get(0)?;
            let tab: String = row.get(1)?;
            let template: Option<String> = row.get(2)?;
            Ok((
                Uuid::parse_str(&workspace).map_err(to_sql_conversion)?,
                Uuid::parse_str(&tab).map_err(to_sql_conversion)?,
                template
                    .map(|value| Uuid::parse_str(&value).map_err(to_sql_conversion))
                    .transpose()?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
                row.get(6)?,
                row.get(7)?,
            ))
        },
    )
    .optional()
    .context("read template run identity")
}

fn to_sql_conversion(error: uuid::Error) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(error))
}

fn enforce_active_bounds(
    tx: &Transaction<'_>,
    previous: Option<&RunIdentity>,
    next_status: &str,
    next_payload_bytes: usize,
) -> Result<()> {
    let (mut count, mut bytes): (usize, usize) = tx.query_row(
        "SELECT COUNT(*),COALESCE(SUM(length(CAST(payload AS BLOB))),0)
           FROM template_runs WHERE status IN ('prepared','running')",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    if let Some(previous) = previous
        && active(&previous.5)
    {
        count = count.saturating_sub(1);
        bytes = bytes.saturating_sub(previous.6);
    }
    if active(next_status) {
        count = count
            .checked_add(1)
            .context("active template run count overflow")?;
        bytes = bytes
            .checked_add(next_payload_bytes)
            .context("active template run payload total overflow")?;
    }
    ensure!(
        count <= MAX_RUNS,
        "active template run limit of 32 exceeded"
    );
    ensure!(
        bytes <= MAX_ACTIVE_PAYLOAD_BYTES,
        "active template run payload total exceeds 16 MiB"
    );
    Ok(())
}

pub(crate) fn write_template_run(
    tx: &Transaction<'_>,
    write: CatalogWrite<StoredTemplateRun>,
) -> Result<StoredTemplateRun> {
    let mut run = write.value;
    validate_run(&run)?;
    let next = next_revision(run.revision, write.expected_revision)?;
    let previous = read_previous_identity(tx, run.id)?;
    match (&write.expected_revision, &previous) {
        (None, None) => {}
        (None, Some(_)) => anyhow::bail!("template run ID already exists"),
        (Some(_), None) => anyhow::bail!("template run revision conflict"),
        (Some(expected), Some(old)) => {
            ensure!(
                old.0 == run.workspace_id
                    && old.1 == run.tab_id
                    && old.2 == run.template_id
                    && old.3 == run.name
                    && old.4 == run.slots,
                "template run identity, name, workspace, tab and slots are immutable"
            );
            ensure!(
                old.7 == *expected,
                "template run revision conflict (expected {expected})"
            );
        }
    }
    enforce_active_bounds(tx, previous.as_ref(), &run.status, run.payload.len())?;
    match write.expected_revision {
        None => {
            tx.execute(
                "INSERT INTO template_runs(id,workspace_id,tab_id,template_id,name,slots,status,payload,revision)
                 VALUES(?1,?2,?3,?4,?5,?6,?7,?8,1)",
                params![
                    run.id.to_string(), run.workspace_id.to_string(), run.tab_id.to_string(),
                    run.template_id.map(|id| id.to_string()), run.name, run.slots,
                    run.status, run.payload
                ],
            )?;
            run.revision = 1;
        }
        Some(expected) => {
            let changed = tx.execute(
                "UPDATE template_runs SET status=?2,payload=?3,revision=?4 WHERE id=?1 AND revision=?5",
                params![run.id.to_string(), run.status, run.payload, next, expected as i64],
            )?;
            ensure!(
                changed == 1,
                "template run revision conflict (expected {expected})"
            );
            run.revision = next as u64;
        }
    }
    Ok(run)
}

impl MetadataStore {
    pub fn save_template_run(
        &mut self,
        write: CatalogWrite<StoredTemplateRun>,
    ) -> Result<StoredTemplateRun> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let run = write_template_run(&tx, write)?;
        tx.commit()?;
        Ok(run)
    }

    pub fn template_run(&self, id: Uuid) -> Result<Option<StoredTemplateRun>> {
        let payload_bytes: Option<usize> = self
            .connection
            .query_row(
                "SELECT length(CAST(payload AS BLOB)) FROM template_runs WHERE id=?1",
                [id.to_string()],
                |row| row.get(0),
            )
            .optional()?;
        let Some(payload_bytes) = payload_bytes else {
            return Ok(None);
        };
        ensure!(
            payload_bytes <= MAX_RUN_PAYLOAD_BYTES,
            "stored template run payload exceeds 2 MiB"
        );
        let run = self
            .connection
            .query_row(
                "SELECT id,workspace_id,tab_id,template_id,name,slots,status,payload,revision
                   FROM template_runs WHERE id=?1",
                [id.to_string()],
                |row| {
                    Ok(StoredTemplateRun {
                        id: parse_uuid(row.get(0)?)?,
                        workspace_id: parse_uuid(row.get(1)?)?,
                        tab_id: parse_uuid(row.get(2)?)?,
                        template_id: row
                            .get::<_, Option<String>>(3)?
                            .map(parse_uuid)
                            .transpose()?,
                        name: row.get(4)?,
                        slots: row.get(5)?,
                        status: row.get(6)?,
                        payload: row.get(7)?,
                        revision: row.get(8)?,
                    })
                },
            )
            .optional()
            .context("read template run payload")?;
        if let Some(run) = &run {
            validate_run(run)?;
        }
        Ok(run)
    }

    /// Read active run metadata, never payloads. The full active set is bounded
    /// before a page is materialized; `before` is an exclusive ordinal cursor.
    pub fn template_runs(&self, before: Option<u64>) -> Result<TemplateRunPage> {
        let oversized_active: usize = self.connection.query_row(
            "SELECT COUNT(*) FROM template_runs
               WHERE status IN ('prepared','running')
                 AND length(CAST(payload AS BLOB))>?1",
            [MAX_RUN_PAYLOAD_BYTES],
            |row| row.get(0),
        )?;
        ensure!(
            oversized_active == 0,
            "active template run payload exceeds 2 MiB"
        );
        let (active_count, active_bytes): (usize, usize) = self.connection.query_row(
            "SELECT COUNT(*),COALESCE(SUM(length(CAST(payload AS BLOB))),0)
               FROM template_runs WHERE status IN ('prepared','running')",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        ensure!(
            active_count <= MAX_RUNS,
            "too many active template runs stored"
        );
        ensure!(
            active_bytes <= MAX_ACTIVE_PAYLOAD_BYTES,
            "active template run payload total exceeds 16 MiB"
        );
        let before = before
            .map(|value| {
                ensure!(
                    value > 0 && value <= i64::MAX as u64,
                    "invalid template run page cursor"
                );
                Ok::<i64, anyhow::Error>(value as i64)
            })
            .transpose()?;
        let mut statement = self.connection.prepare(
            "SELECT ordinal,id,workspace_id,tab_id,template_id,name,slots,status,revision
               FROM template_runs WHERE status IN ('prepared','running')
                 AND (?1 IS NULL OR ordinal < ?1)
               ORDER BY ordinal DESC LIMIT 33",
        )?;
        let rows = statement.query_map([before], |row| {
            Ok(TemplateRunSummary {
                ordinal: row.get::<_, u64>(0)?,
                id: parse_uuid(row.get(1)?)?,
                workspace_id: parse_uuid(row.get(2)?)?,
                tab_id: parse_uuid(row.get(3)?)?,
                template_id: row
                    .get::<_, Option<String>>(4)?
                    .map(parse_uuid)
                    .transpose()?,
                name: row.get(5)?,
                slots: row.get(6)?,
                status: row.get(7)?,
                revision: row.get(8)?,
            })
        })?;
        let mut entries = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        for entry in &entries {
            validate_summary(entry)?;
        }
        let next_before = if entries.len() > MAX_RUNS {
            entries.pop();
            entries.last().map(|entry| entry.ordinal)
        } else {
            None
        };
        Ok(TemplateRunPage {
            entries,
            next_before,
        })
    }
}

fn parse_uuid(value: String) -> rusqlite::Result<Uuid> {
    Uuid::parse_str(&value).map_err(to_sql_conversion)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CatalogBatch, LayoutDocument};

    fn draft(status: &str) -> StoredTemplateRun {
        StoredTemplateRun {
            id: Uuid::new_v4(),
            workspace_id: Uuid::new_v4(),
            tab_id: Uuid::new_v4(),
            template_id: Some(Uuid::new_v4()),
            name: "Four panes".into(),
            slots: 4,
            status: status.into(),
            payload: "{\"opaque\":true}".into(),
            revision: 0,
        }
    }

    fn new_write(run: StoredTemplateRun) -> CatalogWrite<StoredTemplateRun> {
        CatalogWrite {
            value: run,
            expected_revision: None,
        }
    }

    #[test]
    fn template_runs_are_durable_cas_immutable_and_active_only_in_pages() -> Result<()> {
        let _serial = crate::STORAGE_TEST_LOCK.lock().unwrap();
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("state.sqlite");
        let mut store = MetadataStore::open(&path)?;
        let saved = store.save_template_run(new_write(draft("prepared")))?;
        assert_eq!(saved.revision, 1);
        assert_eq!(store.template_run(saved.id)?, Some(saved.clone()));
        let page = store.template_runs(None)?;
        assert_eq!(page.entries.len(), 1);
        assert_eq!(page.entries[0].id, saved.id);
        assert!(page.entries[0].ordinal > 0);
        assert_eq!(page.next_before, None);
        assert!(
            store
                .template_runs(Some(page.entries[0].ordinal))?
                .entries
                .is_empty()
        );

        let mut running = saved.clone();
        running.status = "running".into();
        running.revision = 1;
        let running = store.save_template_run(CatalogWrite {
            value: running.clone(),
            expected_revision: Some(1),
        })?;
        assert_eq!(running.revision, 2);

        let mut renamed = running.clone();
        renamed.name = "changed identity".into();
        assert!(
            store
                .save_template_run(CatalogWrite {
                    value: renamed,
                    expected_revision: Some(2),
                })
                .is_err()
        );
        assert!(
            store
                .save_template_run(CatalogWrite {
                    value: running.clone(),
                    expected_revision: Some(1),
                })
                .is_err()
        );

        let mut complete = running.clone();
        complete.status = "complete".into();
        complete.payload = "{\"opaque\":\"completed\"}".into();
        complete.revision = 2;
        let complete = store.save_template_run(CatalogWrite {
            value: complete,
            expected_revision: Some(2),
        })?;
        assert_eq!(complete.revision, 3);
        assert_eq!(store.template_run(saved.id)?, Some(complete));
        assert!(store.template_runs(None)?.entries.is_empty());
        drop(store);
        assert_eq!(
            MetadataStore::open(path)?.template_run(saved.id)?,
            Some({
                let mut value = saved;
                value.status = "complete".into();
                value.payload = "{\"opaque\":\"completed\"}".into();
                value.revision = 3;
                value
            })
        );
        Ok(())
    }

    #[test]
    fn active_template_run_count_and_aggregate_payload_are_bounded() -> Result<()> {
        let _serial = crate::STORAGE_TEST_LOCK.lock().unwrap();
        let dir = tempfile::tempdir()?;
        let mut store = MetadataStore::open(dir.path().join("state.sqlite"))?;
        let large = serde_json::to_string(&"x".repeat(MAX_RUN_PAYLOAD_BYTES - 2))?;
        for _ in 0..8 {
            let mut run = draft("prepared");
            run.payload = large.clone();
            store.save_template_run(new_write(run))?;
        }
        assert!(
            store
                .save_template_run(new_write(draft("running")))
                .is_err()
        );

        let dir = tempfile::tempdir()?;
        let mut count_store = MetadataStore::open(dir.path().join("count.sqlite"))?;
        let mut saved = Vec::new();
        for _ in 0..MAX_RUNS {
            let mut run = draft("prepared");
            run.payload = "null".into();
            saved.push(count_store.save_template_run(new_write(run))?);
        }
        let mut over_limit = draft("prepared");
        over_limit.payload = "null".into();
        assert!(
            count_store
                .save_template_run(new_write(over_limit))
                .is_err()
        );
        let mut completed = saved.remove(0);
        completed.status = "cancelled".into();
        completed.revision = 1;
        count_store.save_template_run(CatalogWrite {
            value: completed,
            expected_revision: Some(1),
        })?;
        let mut now_allowed = draft("prepared");
        now_allowed.payload = "null".into();
        count_store.save_template_run(new_write(now_allowed))?;
        assert_eq!(count_store.template_runs(None)?.entries.len(), MAX_RUNS);
        Ok(())
    }

    #[test]
    fn catalog_layout_and_template_run_commit_or_rollback_together() -> Result<()> {
        let _serial = crate::STORAGE_TEST_LOCK.lock().unwrap();
        let dir = tempfile::tempdir()?;
        let mut store = MetadataStore::open(dir.path().join("state.sqlite"))?;
        let mut too_large = draft("prepared");
        too_large.payload = serde_json::to_string(&"x".repeat(MAX_RUN_PAYLOAD_BYTES - 1))?;
        assert!(
            store
                .apply_catalog_with_template_run(
                    CatalogBatch {
                        layout: Some(CatalogWrite {
                            value: LayoutDocument {
                                json: "{}".into(),
                                revision: 0
                            },
                            expected_revision: Some(0),
                        }),
                        ..CatalogBatch::default()
                    },
                    new_write(too_large.clone()),
                )
                .is_err()
        );
        assert_eq!(store.catalog()?.layout.json, "null");
        assert_eq!(store.template_run(too_large.id)?, None);

        let run = draft("prepared");
        let run_id = run.id;
        store.apply_catalog_with_template_run(
            CatalogBatch {
                layout: Some(CatalogWrite {
                    value: LayoutDocument {
                        json: "{}".into(),
                        revision: 0,
                    },
                    expected_revision: Some(0),
                }),
                ..CatalogBatch::default()
            },
            new_write(run),
        )?;
        assert_eq!(store.catalog()?.layout.json, "{}");
        assert!(store.template_run(run_id)?.is_some());
        Ok(())
    }

    #[test]
    fn oversized_corrupt_payload_is_rejected_before_payload_read() -> Result<()> {
        let _serial = crate::STORAGE_TEST_LOCK.lock().unwrap();
        let dir = tempfile::tempdir()?;
        let mut store = MetadataStore::open(dir.path().join("state.sqlite"))?;
        let run = store.save_template_run(new_write(draft("prepared")))?;
        let oversized_json = serde_json::to_string(&"x".repeat(MAX_RUN_PAYLOAD_BYTES - 1))?;
        store
            .connection
            .pragma_update(None, "ignore_check_constraints", "ON")?;
        store.connection.execute(
            "UPDATE template_runs SET payload=?2 WHERE id=?1",
            params![run.id.to_string(), oversized_json],
        )?;
        assert!(store.template_run(run.id).is_err());
        assert!(store.template_runs(None).is_err());
        Ok(())
    }
}

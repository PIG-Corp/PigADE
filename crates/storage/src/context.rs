//! Versioned project/workspace context stored by the sole SQLite metadata writer.
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::MetadataStore;

pub const MAX_CONTEXT_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextDocument {
    pub scope_id: Uuid,
    /// Revision observed by the reader; successful saves return the next revision.
    pub revision: u64,
    pub instructions: String,
    pub role: String,
    pub memory: String,
}

impl ContextDocument {
    fn empty(scope_id: Uuid) -> Self {
        Self {
            scope_id,
            revision: 0,
            instructions: String::new(),
            role: String::new(),
            memory: String::new(),
        }
    }

    fn validate(&self) -> Result<()> {
        let bytes = self
            .instructions
            .len()
            .checked_add(self.role.len())
            .and_then(|size| size.checked_add(self.memory.len()))
            .context("context byte length overflow")?;
        ensure!(
            bytes <= MAX_CONTEXT_BYTES,
            "context exceeds 1 MiB text budget"
        );
        ensure!(
            !self.instructions.contains('\0')
                && !self.role.contains('\0')
                && !self.memory.contains('\0'),
            "context text contains NUL"
        );
        Ok(())
    }
}

pub(crate) fn migrate_context_schema(connection: &Connection) -> Result<()> {
    connection.execute_batch(
        "CREATE TABLE context_revisions (
            scope_id TEXT NOT NULL,
            revision INTEGER NOT NULL CHECK(revision>0),
            instructions TEXT NOT NULL,
            role TEXT NOT NULL,
            memory TEXT NOT NULL,
            PRIMARY KEY(scope_id, revision)
        ) WITHOUT ROWID;
        CREATE TABLE context_heads (
            scope_id TEXT PRIMARY KEY,
            revision INTEGER NOT NULL CHECK(revision>0),
            FOREIGN KEY(scope_id, revision)
                REFERENCES context_revisions(scope_id, revision)
        ) WITHOUT ROWID;",
    )?;
    Ok(())
}

impl MetadataStore {
    /// Read the current immutable context revision, or an empty revision zero.
    pub fn context(&self, scope_id: Uuid) -> Result<ContextDocument> {
        let row: Option<(i64, String, String, String)> = self
            .connection
            .query_row(
                "SELECT r.revision, r.instructions, r.role, r.memory
                 FROM context_heads h
                 JOIN context_revisions r
                   ON r.scope_id=h.scope_id AND r.revision=h.revision
                 WHERE h.scope_id=?1",
                [scope_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()?;
        let Some((revision, instructions, role, memory)) = row else {
            return Ok(ContextDocument::empty(scope_id));
        };
        ensure!(revision > 0, "invalid context revision");
        let document = ContextDocument {
            scope_id,
            revision: revision as u64,
            instructions,
            role,
            memory,
        };
        document.validate()?;
        Ok(document)
    }

    /// Append an immutable revision and advance the current head in one CAS transaction.
    pub fn save_context(
        &mut self,
        document: ContextDocument,
        expected_revision: u64,
    ) -> Result<ContextDocument> {
        document.validate()?;
        ensure!(
            document.revision == expected_revision,
            "context document revision differs from expected revision"
        );
        ensure!(
            expected_revision < i64::MAX as u64,
            "context revision exhausted"
        );
        let next_revision = expected_revision + 1;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current: Option<i64> = tx
            .query_row(
                "SELECT revision FROM context_heads WHERE scope_id=?1",
                [document.scope_id.to_string()],
                |row| row.get(0),
            )
            .optional()?;
        let actual = current.unwrap_or(0);
        ensure!(
            actual >= 0 && actual as u64 == expected_revision,
            "context revision conflict (expected {expected_revision}, found {actual})"
        );
        tx.execute(
            "INSERT INTO context_revisions(scope_id,revision,instructions,role,memory)
             VALUES(?1,?2,?3,?4,?5)",
            params![
                document.scope_id.to_string(),
                next_revision as i64,
                document.instructions,
                document.role,
                document.memory
            ],
        )?;
        if expected_revision == 0 {
            tx.execute(
                "INSERT INTO context_heads(scope_id,revision) VALUES(?1,?2)",
                params![document.scope_id.to_string(), next_revision as i64],
            )?;
        } else {
            let changed = tx.execute(
                "UPDATE context_heads SET revision=?2 WHERE scope_id=?1 AND revision=?3",
                params![
                    document.scope_id.to_string(),
                    next_revision as i64,
                    expected_revision as i64
                ],
            )?;
            ensure!(changed == 1, "context revision changed during save");
        }
        tx.commit()?;
        Ok(ContextDocument {
            revision: next_revision,
            ..document
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;
    use tempfile::tempdir;

    fn document(scope_id: Uuid, revision: u64, value: &str) -> ContextDocument {
        ContextDocument {
            scope_id,
            revision,
            instructions: format!("instructions {value}"),
            role: format!("role {value}"),
            memory: format!("memory {value}"),
        }
    }

    #[test]
    fn context_revisions_are_atomic_bounded_and_compare_and_swap() -> Result<()> {
        let _serial = crate::STORAGE_TEST_LOCK.lock().unwrap();
        let directory = tempdir()?;
        let path = directory.path().join("metadata.sqlite");
        let scope = Uuid::new_v4();
        let mut store = MetadataStore::open(&path)?;
        assert_eq!(store.context(scope)?, ContextDocument::empty(scope));

        let first = store.save_context(document(scope, 0, "one"), 0)?;
        assert_eq!(first.revision, 1);
        assert_eq!(store.context(scope)?, first);
        assert!(store.save_context(document(scope, 0, "stale"), 0).is_err());
        assert_eq!(store.context(scope)?, first);

        let second = store.save_context(document(scope, 1, "two"), 1)?;
        assert_eq!(second.revision, 2);
        assert_eq!(store.context(scope)?, second);
        let revisions: i64 = store.connection.query_row(
            "SELECT COUNT(*) FROM context_revisions WHERE scope_id=?1",
            [scope.to_string()],
            |row| row.get(0),
        )?;
        assert_eq!(revisions, 2, "stale CAS must not append a revision");
        drop(store);

        let reopened = MetadataStore::open(path)?;
        assert_eq!(reopened.context(scope)?, second);
        assert_eq!(reopened.context(Uuid::new_v4())?.revision, 0);
        Ok(())
    }

    #[test]
    fn aggregate_context_bound_rejects_without_advancing_head() -> Result<()> {
        let _serial = crate::STORAGE_TEST_LOCK.lock().unwrap();
        let directory = tempdir()?;
        let mut store = MetadataStore::open(directory.path().join("metadata.sqlite"))?;
        let scope = Uuid::new_v4();
        let oversized = ContextDocument {
            scope_id: scope,
            revision: 0,
            instructions: "x".repeat(MAX_CONTEXT_BYTES),
            role: "y".to_owned(),
            memory: String::new(),
        };
        assert!(store.save_context(oversized, 0).is_err());
        assert_eq!(store.context(scope)?, ContextDocument::empty(scope));
        Ok(())
    }

    #[test]
    fn schema_three_migration_keeps_a_valid_pre_v4_backup() -> Result<()> {
        let _serial = crate::STORAGE_TEST_LOCK.lock().unwrap();
        let directory = tempdir()?;
        let path = directory.path().join("metadata.sqlite");
        {
            let legacy = rusqlite::Connection::open(&path)?;
            legacy.execute_batch(
                "CREATE TABLE preserved(value TEXT NOT NULL);
                 INSERT INTO preserved(value) VALUES('schema-three-data');
                 PRAGMA user_version=3;",
            )?;
        }
        let mut store = MetadataStore::open(&path)?;
        let current: u32 = store
            .connection
            .pragma_query_value(None, "user_version", |row| row.get(0))?;
        assert_eq!(current, 4);
        let scope = Uuid::new_v4();
        assert_eq!(store.context(scope)?, ContextDocument::empty(scope));
        let saved = store.save_context(document(scope, 0, "after migration"), 0)?;
        assert_eq!(saved.revision, 1);
        assert_eq!(store.context(scope)?, saved);
        let backup_path = store.migration_backup_path().context("v3 backup absent")?;
        assert!(backup_path.to_string_lossy().contains(".pre-v4."));
        let backup = rusqlite::Connection::open_with_flags(
            backup_path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )?;
        let backup_version: u32 =
            backup.pragma_query_value(None, "user_version", |row| row.get(0))?;
        assert_eq!(backup_version, 3);
        let preserved: String =
            backup.query_row("SELECT value FROM preserved", [], |row| row.get(0))?;
        assert_eq!(preserved, "schema-three-data");
        Ok(())
    }
}

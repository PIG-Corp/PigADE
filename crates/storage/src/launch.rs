use anyhow::{Context, Result, bail, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use std::{
    fs::{File, OpenOptions},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    time::Duration,
};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessIdentity {
    pub boot_id: String,
    pub pid: u32,
    pub start_ticks: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LaunchState {
    IntentCommitted,
    HelperRegistered,
    ExecPermitted,
    Running,
    Unknown,
    Exited,
}
impl LaunchState {
    fn as_str(self) -> &'static str {
        match self {
            Self::IntentCommitted => "intent",
            Self::HelperRegistered => "helper",
            Self::ExecPermitted => "permitted",
            Self::Running => "running",
            Self::Unknown => "unknown",
            Self::Exited => "exited",
        }
    }
    fn parse(value: &str) -> Result<Self> {
        match value {
            "intent" => Ok(Self::IntentCommitted),
            "helper" => Ok(Self::HelperRegistered),
            "permitted" => Ok(Self::ExecPermitted),
            "running" => Ok(Self::Running),
            "unknown" => Ok(Self::Unknown),
            "exited" => Ok(Self::Exited),
            _ => bail!("unknown launch state {value}"),
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchRecord {
    pub session_id: Uuid,
    pub generation: Uuid,
    #[serde(default)]
    pub ordinal: u64,
    pub operation_id: Uuid,
    pub nonce: Uuid,
    pub state: LaunchState,
    pub identity: Option<ProcessIdentity>,
    pub reason: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BeginLaunchResult {
    pub record: LaunchRecord,
    pub created: bool,
}

/// Sole metadata writer. The lock is held across transactions and prevents a
/// second host version from opening this database as another writer.
pub struct MetadataStore {
    pub(crate) connection: Connection,
    _writer_lock: File,
    migration_backup: Option<PathBuf>,
}
impl MetadataStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let mut lock_path = path.as_os_str().to_os_string();
        lock_path.push(".writer.lock");
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(lock_path)?;
        super::lock_writer(&lock).context("metadata writer already active")?;
        // SQLite inherits restrictive permissions for its WAL and SHM files.
        let db_file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(path)?;
        db_file.sync_all()?;
        let mut connection = Connection::open(path)?;
        connection.busy_timeout(Duration::from_secs(2))?;
        let version: u32 = connection.pragma_query_value(None, "user_version", |r| r.get(0))?;
        ensure!(
            version <= 14,
            "database schema {version} is newer than this writer"
        );
        connection.pragma_update(None, "synchronous", "FULL")?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        connection.pragma_update(None, "trusted_schema", "OFF")?;
        // VACUUM INTO reads committed WAL pages through SQLite. Never copy a
        // database file while disregarding its WAL. The backup is durable before
        // any persistent pragma/schema change to an existing older database.
        let migration_backup = if version > 0 && version < 14 {
            Some(backup_before_migration(&connection, path, version, 14)?)
        } else {
            None
        };
        connection.pragma_update(None, "journal_mode", "WAL")?;
        if version < 14 {
            let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            if version == 0 {
                tx.execute_batch("CREATE TABLE launch_intents (
                    operation_id TEXT PRIMARY KEY, session_id TEXT NOT NULL,
                    generation TEXT NOT NULL UNIQUE, nonce TEXT NOT NULL,
                    state TEXT NOT NULL CHECK(state IN ('intent','helper','permitted','running','unknown','exited')),
                    identity TEXT, reason TEXT);
                  CREATE TABLE generation_locks (
                    session_id TEXT PRIMARY KEY, generation TEXT NOT NULL UNIQUE,
                    operation_id TEXT NOT NULL UNIQUE REFERENCES launch_intents(operation_id));")?;
            }
            if version < 2 {
                crate::catalog::migrate_catalog(&tx)?;
            }
            if version < 3 {
                crate::catalog::migrate_layout_document(&tx)?;
            }
            if version < 4 {
                crate::context::migrate_context_schema(&tx)?;
            }
            if version < 5 {
                crate::context::migrate_context_v5(&tx)?;
                crate::prepared_launch::migrate_prepared_launch_schema(&tx)?;
            }
            if version < 6 {
                crate::conversation::migrate_conversation_schema(&tx)?;
            }
            if version < 7 {
                migrate_launch_ordinal_schema(&tx)?;
            }
            if version < 8 {
                crate::catalog::migrate_layout_templates(&tx)?;
            }
            if version < 9 {
                crate::catalog::migrate_template_launches(&tx)?;
                crate::template_run::migrate_template_runs(&tx)?;
            }
            if version < 10 {
                crate::catalog::migrate_pane_labels(&tx)?;
            }
            if version < 11 {
                crate::catalog::migrate_agent_profiles(&tx)?;
            }
            if version < 12 {
                crate::prompt::migrate_prompts(&tx)?;
            }
            if version < 14 {
                crate::catalog::migrate_agent_profile_roster(&tx)?;
            }
            tx.pragma_update(None, "user_version", 14)?;
            tx.commit()?;
        }
        File::open(
            path.parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or_else(|| Path::new(".")),
        )?
        .sync_all()?;
        Ok(Self {
            connection,
            _writer_lock: lock,
            migration_backup,
        })
    }
    /// Fresh migration backup, if this open upgraded an older database. Existing
    /// backups are retained; later opens do not create redundant copies.
    pub fn migration_backup_path(&self) -> Option<&Path> {
        self.migration_backup.as_deref()
    }
    pub fn get_launch(&self, operation_id: Uuid) -> Result<Option<LaunchRecord>> {
        load_record(&self.connection, operation_id)
    }
    /// Read the exact durable state for a pane generation without loading a
    /// history page or unrelated launch records.
    pub fn launch_state_for_generation(
        &self,
        session: Uuid,
        generation: Uuid,
    ) -> Result<Option<LaunchState>> {
        let mut statement = self.connection.prepare(
            "SELECT state FROM launch_intents WHERE session_id=?1 AND generation=?2 LIMIT 2",
        )?;
        let mut rows = statement.query(params![session.to_string(), generation.to_string()])?;
        let Some(first) = rows.next()? else {
            return Ok(None);
        };
        let state = LaunchState::parse(&first.get::<_, String>(0)?)?;
        ensure!(
            rows.next()?.is_none(),
            "duplicate launch state for session generation"
        );
        Ok(Some(state))
    }
    /// Count every launch that is not durably reconciled as exited.
    pub fn unresolved_launch_count(&self) -> Result<usize> {
        let count: i64 = self.connection.query_row(
            "SELECT COUNT(*) FROM launch_intents WHERE state <> 'exited'",
            [],
            |row| row.get(0),
        )?;
        usize::try_from(count).context("unresolved launch count is out of range")
    }
    /// Bounded manifest read; exceeding the supported live registry is explicit.
    pub fn list_launches(&self) -> Result<Vec<LaunchRecord>> {
        let count: u64 =
            self.connection
                .query_row("SELECT COUNT(*) FROM launch_intents", [], |r| r.get(0))?;
        ensure!(
            count <= 100_000,
            "launch registry exceeds bounded manifest capacity"
        );
        let mut stmt = self
            .connection
            .prepare("SELECT operation_id FROM launch_intents ORDER BY rowid")?;
        let operations = stmt.query_map([], |r| r.get::<_, String>(0))?;
        let mut records = Vec::with_capacity(count as usize);
        for op in operations {
            records.push(
                load_record(&self.connection, Uuid::parse_str(&op?)?)?
                    .context("launch disappeared")?,
            );
        }
        Ok(records)
    }

    /// Read a bounded, newest-first page of one session's generation history.
    /// `before` is an exclusive ordinal cursor, independent of generation UUIDs.
    pub fn generation_records(
        &self,
        session: Uuid,
        before: Option<u64>,
        limit: u16,
    ) -> Result<Vec<LaunchRecord>> {
        ensure!(
            (1..=33).contains(&limit),
            "generation page limit must be 1..=33"
        );
        let before = before
            .map(i64::try_from)
            .transpose()
            .context("generation page cursor exceeds SQLite integer range")?;
        let mut statement = self.connection.prepare(
            "SELECT operation_id FROM launch_intents
             WHERE session_id=?1 AND (?2 IS NULL OR generation_ordinal<?2)
             ORDER BY generation_ordinal DESC LIMIT ?3",
        )?;
        let operations = statement.query_map(
            params![session.to_string(), before, i64::from(limit)],
            |row| row.get::<_, String>(0),
        )?;
        let mut records = Vec::with_capacity(limit as usize);
        for operation in operations {
            records.push(
                load_record(&self.connection, Uuid::parse_str(&operation?)?)?
                    .context("launch disappeared during generation page read")?,
            );
        }
        Ok(records)
    }

    /// A retry returns created=false, even for an unresolved intent. Only the
    /// call that receives created=true may proceed to creating its helper.
    pub fn begin_launch(
        &mut self,
        session_id: Uuid,
        generation: Uuid,
        operation_id: Uuid,
        nonce: Uuid,
    ) -> Result<BeginLaunchResult> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(record) = load_record(&tx, operation_id)? {
            ensure!(
                record.session_id == session_id
                    && record.generation == generation
                    && record.nonce == nonce,
                "operation ID reused with a different launch identity"
            );
            return Ok(BeginLaunchResult {
                record,
                created: false,
            });
        }
        let locked: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM generation_locks WHERE session_id=?1)",
            [session_id.to_string()],
            |r| r.get(0),
        )?;
        ensure!(
            !locked,
            "generation locked: existing launch must be reconciled before another launch"
        );
        let previous_ordinal: i64 = tx.query_row(
            "SELECT COALESCE(MAX(generation_ordinal),0) FROM launch_intents WHERE session_id=?1",
            [session_id.to_string()],
            |row| row.get(0),
        )?;
        ensure!(
            (0..i64::MAX).contains(&previous_ordinal),
            "generation ordinal exhausted or invalid"
        );
        let ordinal = (previous_ordinal + 1) as u64;
        tx.execute("INSERT INTO launch_intents(operation_id,session_id,generation,nonce,state,generation_ordinal) VALUES(?1,?2,?3,?4,'intent',?5)", params![operation_id.to_string(), session_id.to_string(), generation.to_string(), nonce.to_string(), ordinal as i64])?;
        tx.execute(
            "INSERT INTO generation_locks(session_id,generation,operation_id) VALUES(?1,?2,?3)",
            params![
                session_id.to_string(),
                generation.to_string(),
                operation_id.to_string()
            ],
        )?;
        tx.commit()?;
        Ok(BeginLaunchResult {
            record: LaunchRecord {
                session_id,
                generation,
                ordinal,
                operation_id,
                nonce,
                state: LaunchState::IntentCommitted,
                identity: None,
                reason: None,
            },
            created: true,
        })
    }
    pub fn register_helper(
        &mut self,
        operation_id: Uuid,
        identity: ProcessIdentity,
    ) -> Result<LaunchRecord> {
        ensure!(
            !identity.boot_id.is_empty()
                && identity.boot_id.len() <= 256
                && identity.pid > 0
                && identity.start_ticks > 0,
            "invalid process identity"
        );
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let record = load_record(&tx, operation_id)?.context("launch intent missing")?;
        if record.state == LaunchState::HelperRegistered {
            ensure!(
                record.identity.as_ref() == Some(&identity),
                "helper identity changed"
            );
            return Ok(record);
        }
        ensure!(
            record.state == LaunchState::IntentCommitted,
            "helper registration out of order"
        );
        tx.execute(
            "UPDATE launch_intents SET state='helper',identity=?2 WHERE operation_id=?1",
            params![operation_id.to_string(), serde_json::to_string(&identity)?],
        )?;
        let result = load_record(&tx, operation_id)?.unwrap();
        tx.commit()?;
        Ok(result)
    }
    pub fn permit_exec(&mut self, operation_id: Uuid) -> Result<LaunchRecord> {
        self.transition(
            operation_id,
            LaunchState::HelperRegistered,
            LaunchState::ExecPermitted,
        )
    }
    pub fn mark_running(&mut self, operation_id: Uuid) -> Result<LaunchRecord> {
        self.transition(
            operation_id,
            LaunchState::ExecPermitted,
            LaunchState::Running,
        )
    }
    pub fn mark_exited(&mut self, operation_id: Uuid) -> Result<LaunchRecord> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let record = load_record(&tx, operation_id)?.context("launch intent missing")?;
        ensure!(
            matches!(record.state, LaunchState::Running | LaunchState::Exited),
            "exit requires a confirmed running process; reconcile uncertain launch explicitly"
        );
        tx.execute(
            "UPDATE launch_intents SET state='exited' WHERE operation_id=?1",
            [operation_id.to_string()],
        )?;
        tx.execute(
            "DELETE FROM generation_locks WHERE operation_id=?1",
            [operation_id.to_string()],
        )?;
        let result = load_record(&tx, operation_id)?.unwrap();
        tx.commit()?;
        Ok(result)
    }
    pub fn mark_unknown(&mut self, operation_id: Uuid, reason: &str) -> Result<LaunchRecord> {
        ensure!(
            !reason.is_empty() && reason.len() <= 4096,
            "invalid unknown-outcome reason"
        );
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let record = load_record(&tx, operation_id)?.context("launch intent missing")?;
        ensure!(
            record.state != LaunchState::Exited,
            "cannot make exited launch uncertain"
        );
        tx.execute(
            "UPDATE launch_intents SET state='unknown',reason=?2 WHERE operation_id=?1",
            params![operation_id.to_string(), reason],
        )?;
        let result = load_record(&tx, operation_id)?.unwrap();
        tx.commit()?;
        Ok(result)
    }
    /// Explicit reconciliation boundary. The host must establish that the exact
    /// boot/PID/start identity is dead or never executed, including provider
    /// receipts where relevant. Missing a PID in an in-memory registry is not proof.
    /// Evidence is retained; this API never automatically retries the operation.
    pub fn resolve_unknown_as_exited(
        &mut self,
        operation_id: Uuid,
        evidence: &str,
    ) -> Result<LaunchRecord> {
        ensure!(
            !evidence.is_empty() && evidence.len() <= 4096,
            "reconciliation evidence required"
        );
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let record = load_record(&tx, operation_id)?.context("launch intent missing")?;
        ensure!(
            record.state == LaunchState::Unknown,
            "launch is not awaiting reconciliation"
        );
        tx.execute(
            "UPDATE launch_intents SET state='exited',reason=?2 WHERE operation_id=?1",
            params![operation_id.to_string(), evidence],
        )?;
        tx.execute(
            "DELETE FROM generation_locks WHERE operation_id=?1",
            [operation_id.to_string()],
        )?;
        let result = load_record(&tx, operation_id)?.unwrap();
        tx.commit()?;
        Ok(result)
    }
    fn transition(
        &mut self,
        operation_id: Uuid,
        from: LaunchState,
        to: LaunchState,
    ) -> Result<LaunchRecord> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let record = load_record(&tx, operation_id)?.context("launch intent missing")?;
        if record.state == to {
            return Ok(record);
        }
        ensure!(
            record.state == from,
            "invalid launch transition {:?} -> {:?}",
            record.state,
            to
        );
        ensure!(
            record.identity.is_some(),
            "exec requires a durable helper identity"
        );
        tx.execute(
            "UPDATE launch_intents SET state=?2 WHERE operation_id=?1",
            params![operation_id.to_string(), to.as_str()],
        )?;
        let result = load_record(&tx, operation_id)?.unwrap();
        tx.commit()?;
        Ok(result)
    }
}

/// Schema v7 gives every historical and new generation a stable, per-session
/// order based on its original durable intent insertion order.
fn migrate_launch_ordinal_schema(connection: &Connection) -> Result<()> {
    connection.execute_batch(
        "ALTER TABLE launch_intents
            ADD COLUMN generation_ordinal INTEGER NOT NULL DEFAULT 0;
        WITH ordered AS (
            SELECT operation_id,
                   ROW_NUMBER() OVER (PARTITION BY session_id ORDER BY rowid) AS ordinal
            FROM launch_intents
        )
        UPDATE launch_intents
        SET generation_ordinal=(
            SELECT ordinal FROM ordered
            WHERE ordered.operation_id=launch_intents.operation_id
        );
        CREATE UNIQUE INDEX launch_session_generation_ordinal
            ON launch_intents(session_id,generation_ordinal);",
    )?;
    Ok(())
}
/// SQLite documents VACUUM INTO as a consistent live-database snapshot:
/// https://www.sqlite.org/lang_vacuum.html#vacuum_with_an_into_clause
/// An empty private temp file is explicitly synced, renamed and directory-synced
/// before migration. A failed/interrupted backup never authorizes schema writes.
fn backup_before_migration(
    connection: &Connection,
    path: &Path,
    original_version: u32,
    target_version: u32,
) -> Result<PathBuf> {
    let parent = std::fs::canonicalize(
        path.parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new(".")),
    )?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .context("database filename requires UTF-8")?;
    let nonce = Uuid::new_v4();
    let destination = parent.join(format!(
        "{name}.pre-v{target_version}.{nonce}.backup.sqlite"
    ));
    let temporary = parent.join(format!(
        "{name}.pre-v{target_version}.{nonce}.backup.pending"
    ));
    let backup = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&temporary)?;
    connection.execute(
        "VACUUM main INTO ?1",
        [temporary.to_str().context("backup path requires UTF-8")?],
    )?;
    backup.sync_all()?;
    let reader =
        Connection::open_with_flags(&temporary, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let check: String = reader.pragma_query_value(None, "quick_check", |row| row.get(0))?;
    let version: u32 = reader.pragma_query_value(None, "user_version", |row| row.get(0))?;
    ensure!(
        check == "ok" && version == original_version,
        "migration backup validation failed"
    );
    drop(reader);
    drop(backup);
    std::fs::rename(&temporary, &destination)?;
    File::open(parent)?.sync_all()?;
    Ok(destination)
}

fn load_record(connection: &Connection, operation_id: Uuid) -> Result<Option<LaunchRecord>> {
    type LaunchRow = (
        String,
        String,
        i64,
        String,
        String,
        Option<String>,
        Option<String>,
    );
    let record: Option<LaunchRow> = connection.query_row("SELECT session_id,generation,generation_ordinal,nonce,state,identity,reason FROM launch_intents WHERE operation_id=?1", [operation_id.to_string()], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?))).optional()?;
    record
        .map(
            |(session, generation, ordinal, nonce, state, identity, reason)| {
                ensure!(ordinal > 0, "invalid stored generation ordinal");
                Ok(LaunchRecord {
                    session_id: Uuid::parse_str(&session)?,
                    generation: Uuid::parse_str(&generation)?,
                    ordinal: ordinal as u64,
                    operation_id,
                    nonce: Uuid::parse_str(&nonce)?,
                    state: LaunchState::parse(&state)?,
                    identity: identity.map(|s| serde_json::from_str(&s)).transpose()?,
                    reason,
                })
            },
        )
        .transpose()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn identity() -> ProcessIdentity {
        ProcessIdentity {
            boot_id: "test-boot".into(),
            pid: 42,
            start_ticks: 123,
        }
    }
    #[test]
    fn schema_v7_migration_adds_layout_templates_and_preserves_backup() -> Result<()> {
        let _serial = crate::STORAGE_TEST_LOCK.lock().unwrap();
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("state.sqlite");
        drop(MetadataStore::open(&path)?);
        {
            let connection = Connection::open(&path)?;
            connection.execute_batch(
                "DROP INDEX prepared_launch_generation_lookup;
                 DROP TABLE prompt_records; DROP TABLE agent_profile_roster; DROP TABLE agent_profiles; DROP TABLE pane_labels; DROP TABLE template_runs; DROP TABLE layout_templates;
                 PRAGMA user_version=7;",
            )?;
        }
        let store = MetadataStore::open(&path)?;
        assert_eq!(store.catalog()?.layout_templates, Some(Vec::new()));
        assert_eq!(store.catalog()?.template_launch_schema, Some(1));
        assert_eq!(
            store
                .connection
                .pragma_query_value::<u32, _>(None, "user_version", |row| row.get(0))?,
            14
        );
        let backup = store
            .migration_backup_path()
            .context("schema-7 backup absent")?;
        assert!(backup.to_string_lossy().contains(".pre-v14."));
        let backup =
            Connection::open_with_flags(backup, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        assert_eq!(
            backup.pragma_query_value::<u32, _>(None, "user_version", |row| row.get(0))?,
            7
        );
        assert_eq!(
            backup.query_row::<usize, _, _>(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='layout_templates'",
                [],
                |row| row.get(0)
            )?,
            0
        );
        Ok(())
    }
    #[test]
    fn schema_v8_migration_adds_launch_templates_and_run_storage_with_backup() -> Result<()> {
        let _serial = crate::STORAGE_TEST_LOCK.lock().unwrap();
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("state.sqlite");
        {
            let mut store = MetadataStore::open(&path)?;
            store.apply_catalog(crate::CatalogBatch {
                layout_templates: vec![crate::CatalogWrite {
                    value: crate::LayoutTemplate {
                        id: Uuid::new_v4(),
                        name: "legacy".into(),
                        geometry: pigade_layout::GeometryTemplate {
                            schema: 1,
                            root: Some(pigade_layout::TemplateNode::Pane),
                        },
                        launches: None,
                        revision: 0,
                    },
                    expected_revision: None,
                }],
                ..crate::CatalogBatch::default()
            })?;
        }
        {
            let connection = Connection::open(&path)?;
            connection.execute_batch(
                "ALTER TABLE layout_templates RENAME TO layout_templates_v9;
                 CREATE TABLE layout_templates (
                   id TEXT PRIMARY KEY,name TEXT NOT NULL UNIQUE,geometry_json TEXT NOT NULL,
                   revision INTEGER NOT NULL CHECK(revision>0));
                 INSERT INTO layout_templates(id,name,geometry_json,revision)
                   SELECT id,name,geometry_json,revision FROM layout_templates_v9;
                 DROP TABLE layout_templates_v9;
                 DROP TABLE template_runs;
                 DROP INDEX prepared_launch_generation_lookup;
                 DROP TABLE prompt_records; DROP TABLE agent_profile_roster; DROP TABLE agent_profiles; DROP TABLE pane_labels;
                 PRAGMA user_version=8;",
            )?;
        }
        let store = MetadataStore::open(&path)?;
        let catalog = store.catalog()?;
        assert_eq!(catalog.template_launch_schema, Some(1));
        assert_eq!(catalog.layout_templates.as_ref().unwrap().len(), 1);
        assert_eq!(catalog.layout_templates.as_ref().unwrap()[0].launches, None);
        assert_eq!(
            store
                .connection
                .pragma_query_value::<u32, _>(None, "user_version", |row| row.get(0))?,
            14
        );
        assert_eq!(
            store.connection.query_row::<usize, _, _>(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='template_runs'",
                [],
                |row| row.get(0),
            )?,
            1
        );
        let backup_path = store
            .migration_backup_path()
            .context("schema-8 backup absent")?;
        assert!(backup_path.to_string_lossy().contains(".pre-v14."));
        let backup =
            Connection::open_with_flags(backup_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        assert_eq!(
            backup.pragma_query_value::<u32, _>(None, "user_version", |row| row.get(0))?,
            8
        );
        Ok(())
    }
    #[test]
    fn schema_v9_migration_adds_pane_labels_and_preserves_backup() -> Result<()> {
        let _serial = crate::STORAGE_TEST_LOCK.lock().unwrap();
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("state.sqlite");
        drop(MetadataStore::open(&path)?);
        {
            let connection = Connection::open(&path)?;
            connection.execute_batch("DROP INDEX prepared_launch_generation_lookup; DROP TABLE prompt_records; DROP TABLE agent_profile_roster; DROP TABLE agent_profiles; DROP TABLE pane_labels; PRAGMA user_version=9;")?;
        }
        let store = MetadataStore::open(&path)?;
        assert_eq!(store.catalog()?.pane_labels, Some(Vec::new()));
        assert_eq!(
            store
                .connection
                .pragma_query_value::<u32, _>(None, "user_version", |row| row.get(0))?,
            14
        );
        let backup = store
            .migration_backup_path()
            .context("schema-9 backup absent")?;
        assert!(backup.to_string_lossy().contains(".pre-v14."));
        let backup =
            Connection::open_with_flags(backup, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        assert_eq!(
            backup.pragma_query_value::<u32, _>(None, "user_version", |row| row.get(0))?,
            9
        );
        assert_eq!(
            backup.query_row::<usize, _, _>(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='pane_labels'",
                [],
                |row| row.get(0)
            )?,
            0
        );
        Ok(())
    }
    #[test]
    fn idempotency_generation_lock_and_monotonic_protocol() -> Result<()> {
        // Isolate fork/exec crash probes: a child can briefly inherit unrelated flock descriptors.
        let _serial = crate::STORAGE_TEST_LOCK.lock().unwrap();
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("state.sqlite");
        let mut store = MetadataStore::open(&path)?;
        assert!(MetadataStore::open(&path).is_err());
        let (session, generation, op, nonce) = (
            Uuid::new_v4(),
            Uuid::from_u128(900),
            Uuid::new_v4(),
            Uuid::new_v4(),
        );
        let first = store.begin_launch(session, generation, op, nonce)?;
        assert!(first.created);
        assert_eq!(first.record.ordinal, 1);
        let mut legacy_wire = serde_json::to_value(&first.record)?;
        legacy_wire
            .as_object_mut()
            .context("serialized launch record must be an object")?
            .remove("ordinal");
        let legacy_record: LaunchRecord = serde_json::from_value(legacy_wire)?;
        assert_eq!(legacy_record.ordinal, 0);
        let retry = store.begin_launch(session, generation, op, nonce)?;
        assert!(!retry.created);
        assert_eq!(retry.record.ordinal, 1);
        assert!(
            store
                .begin_launch(session, Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4())
                .is_err()
        );
        assert!(
            store
                .begin_launch(session, generation, op, Uuid::new_v4())
                .is_err()
        );
        assert!(store.permit_exec(op).is_err());
        assert!(store.mark_running(op).is_err());
        store.register_helper(op, identity())?;
        store.register_helper(op, identity())?;
        assert!(
            store
                .register_helper(
                    op,
                    ProcessIdentity {
                        pid: 43,
                        ..identity()
                    }
                )
                .is_err()
        );
        store.permit_exec(op)?;
        store.permit_exec(op)?;
        store.mark_running(op)?;
        store.mark_exited(op)?;
        let second_generation = Uuid::from_u128(100);
        let second =
            store.begin_launch(session, second_generation, Uuid::new_v4(), Uuid::new_v4())?;
        assert!(second.created);
        assert_eq!(second.record.ordinal, 2);
        store.mark_unknown(second.record.operation_id, "uncertain")?;
        assert!(
            store
                .begin_launch(session, Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4())
                .is_err()
        );
        store.resolve_unknown_as_exited(second.record.operation_id, "verified absent")?;
        let third = store.begin_launch(session, Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4())?;
        assert_eq!(third.record.ordinal, 3);
        assert_eq!(store.list_launches()?.len(), 3);
        drop(store);
        let reopened = MetadataStore::open(path)?;
        let reopened_first = reopened.get_launch(op)?.unwrap();
        assert_eq!(reopened_first.state, LaunchState::Exited);
        assert_eq!(reopened_first.ordinal, 1);
        Ok(())
    }

    #[test]
    fn schema_v6_migration_backfills_per_session_in_rowid_order_and_keeps_backup() -> Result<()> {
        let _serial = crate::STORAGE_TEST_LOCK.lock().unwrap();
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("state.sqlite");
        let first_session = Uuid::new_v4();
        let other_session = Uuid::new_v4();
        let first_generation = Uuid::from_u128(900);
        let other_generation = Uuid::new_v4();
        let second_generation = Uuid::from_u128(100);
        let first_operation = Uuid::new_v4();
        let other_operation = Uuid::new_v4();
        let second_operation = Uuid::new_v4();
        let first_nonce = Uuid::new_v4();
        let other_nonce = Uuid::new_v4();
        let second_nonce = Uuid::new_v4();
        {
            let connection = Connection::open(&path)?;
            connection.execute_batch(
                "CREATE TABLE launch_intents (
                    operation_id TEXT PRIMARY KEY, session_id TEXT NOT NULL,
                    generation TEXT NOT NULL UNIQUE, nonce TEXT NOT NULL,
                    state TEXT NOT NULL CHECK(state IN ('intent','helper','permitted','running','unknown','exited')),
                    identity TEXT, reason TEXT
                );
                CREATE TABLE generation_locks (
                    session_id TEXT PRIMARY KEY, generation TEXT NOT NULL UNIQUE,
                    operation_id TEXT NOT NULL UNIQUE REFERENCES launch_intents(operation_id)
                );
                CREATE TABLE prepared_launches (
                    id TEXT PRIMARY KEY, payload TEXT NOT NULL,
                    phase TEXT NOT NULL CHECK(phase IN (
                        'prepared','attempting','delivered_unconfirmed','not_requested','unknown'
                    ))
                ) WITHOUT ROWID;
                PRAGMA user_version=6;",
            )?;
            for (operation, session, generation, nonce, state) in [
                (
                    first_operation,
                    first_session,
                    first_generation,
                    first_nonce,
                    "exited",
                ),
                (
                    other_operation,
                    other_session,
                    other_generation,
                    other_nonce,
                    "exited",
                ),
                (
                    second_operation,
                    first_session,
                    second_generation,
                    second_nonce,
                    "unknown",
                ),
            ] {
                connection.execute(
                    "INSERT INTO launch_intents(operation_id,session_id,generation,nonce,state)
                     VALUES(?1,?2,?3,?4,?5)",
                    params![
                        operation.to_string(),
                        session.to_string(),
                        generation.to_string(),
                        nonce.to_string(),
                        state,
                    ],
                )?;
            }
            connection.execute(
                "INSERT INTO generation_locks(session_id,generation,operation_id)
                 VALUES(?1,?2,?3)",
                params![
                    first_session.to_string(),
                    second_generation.to_string(),
                    second_operation.to_string(),
                ],
            )?;
        }

        let store = MetadataStore::open(&path)?;
        assert_eq!(store.get_launch(first_operation)?.unwrap().ordinal, 1);
        assert_eq!(store.get_launch(other_operation)?.unwrap().ordinal, 1);
        assert_eq!(store.get_launch(second_operation)?.unwrap().ordinal, 2);
        assert_eq!(
            store.get_launch(second_operation)?.unwrap().generation,
            second_generation
        );
        assert_eq!(
            store
                .connection
                .pragma_query_value::<u32, _>(None, "user_version", |row| row.get(0))?,
            14
        );
        assert_eq!(
            store.connection.query_row::<usize, _, _>(
                "SELECT COUNT(*) FROM generation_locks WHERE session_id=?1 AND operation_id=?2",
                params![first_session.to_string(), second_operation.to_string()],
                |row| row.get(0),
            )?,
            1
        );
        let backup_path = store.migration_backup_path().context("v6 backup absent")?;
        assert!(backup_path.to_string_lossy().contains(".pre-v14."));
        let backup =
            Connection::open_with_flags(backup_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        assert_eq!(
            backup.pragma_query_value::<u32, _>(None, "user_version", |row| row.get(0))?,
            6
        );
        assert_eq!(
            backup
                .query_row::<usize, _, _>("SELECT COUNT(*) FROM launch_intents", [], |row| row
                    .get(0))?,
            3
        );
        assert_eq!(
            backup.query_row::<usize, _, _>(
                "SELECT COUNT(*) FROM generation_locks",
                [],
                |row| row.get(0)
            )?,
            1
        );
        Ok(())
    }

    #[test]
    fn generation_records_pages_by_exclusive_ordinal_with_strict_bounds() -> Result<()> {
        let _serial = crate::STORAGE_TEST_LOCK.lock().unwrap();
        let directory = tempfile::tempdir()?;
        let mut store = MetadataStore::open(directory.path().join("state.sqlite"))?;
        let session = Uuid::new_v4();
        for offset in 1..=5u128 {
            let generation = Uuid::from_u128(1_000 - offset);
            let operation = Uuid::new_v4();
            store.begin_launch(session, generation, operation, Uuid::new_v4())?;
            store.register_helper(operation, identity())?;
            store.permit_exec(operation)?;
            store.mark_running(operation)?;
            store.mark_exited(operation)?;
        }
        let other_session = Uuid::new_v4();
        let other_operation = Uuid::new_v4();
        store.begin_launch(
            other_session,
            Uuid::new_v4(),
            other_operation,
            Uuid::new_v4(),
        )?;
        store.register_helper(other_operation, identity())?;
        store.permit_exec(other_operation)?;
        store.mark_running(other_operation)?;
        store.mark_exited(other_operation)?;

        let latest = store.generation_records(session, None, 3)?;
        assert_eq!(
            latest
                .iter()
                .map(|record| record.ordinal)
                .collect::<Vec<_>>(),
            [5, 4, 3]
        );
        assert_eq!(
            latest
                .iter()
                .map(|record| record.generation)
                .collect::<Vec<_>>(),
            [
                Uuid::from_u128(995),
                Uuid::from_u128(996),
                Uuid::from_u128(997)
            ]
        );
        let older = store.generation_records(session, Some(3), 33)?;
        assert_eq!(
            older
                .iter()
                .map(|record| record.ordinal)
                .collect::<Vec<_>>(),
            [2, 1]
        );
        assert!(store.generation_records(session, Some(1), 33)?.is_empty());
        assert!(store.generation_records(session, Some(0), 33)?.is_empty());
        assert!(store.generation_records(session, None, 0).is_err());
        assert!(store.generation_records(session, None, 34).is_err());
        assert!(
            store
                .generation_records(session, Some(u64::MAX), 33)
                .is_err()
        );
        assert!(
            store
                .generation_records(Uuid::new_v4(), None, 33)?
                .is_empty()
        );
        Ok(())
    }

    #[test]
    fn every_crash_boundary_keeps_lock_until_explicit_reconciliation() -> Result<()> {
        // Isolate fork/exec crash probes: a child can briefly inherit unrelated flock descriptors.
        let _serial = crate::STORAGE_TEST_LOCK.lock().unwrap();
        for phase in 0..4 {
            let dir = tempfile::tempdir()?;
            let path = dir.path().join("state.sqlite");
            let (session, generation, op, nonce) = (
                Uuid::new_v4(),
                Uuid::new_v4(),
                Uuid::new_v4(),
                Uuid::new_v4(),
            );
            let mut store = MetadataStore::open(&path)?;
            store.begin_launch(session, generation, op, nonce)?;
            if phase >= 1 {
                store.register_helper(op, identity())?;
            }
            if phase >= 2 {
                store.permit_exec(op)?;
            }
            if phase >= 3 {
                store.mark_running(op)?;
            }
            drop(store);
            let mut recovered = MetadataStore::open(&path)?;
            assert!(
                !recovered
                    .begin_launch(session, generation, op, nonce)?
                    .created
            );
            recovered.mark_unknown(op, "host disappeared at protocol boundary")?;
            drop(recovered);
            let mut recovered = MetadataStore::open(&path)?;
            assert_eq!(
                recovered.get_launch(op)?.unwrap().state,
                LaunchState::Unknown
            );
            assert!(
                recovered
                    .begin_launch(session, Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4())
                    .is_err()
            );
            assert!(recovered.permit_exec(op).is_err());
            assert!(recovered.mark_exited(op).is_err());
            recovered.resolve_unknown_as_exited(
                op,
                "verified exact boot/PID/start identity absent; no provider daemon",
            )?;
            assert!(
                recovered
                    .begin_launch(session, Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4())?
                    .created
            );
        }
        Ok(())
    }
    #[test]
    fn actual_database_wal_full_and_newer_schema_refusal() -> Result<()> {
        // Isolate fork/exec crash probes: a child can briefly inherit unrelated flock descriptors.
        let _serial = crate::STORAGE_TEST_LOCK.lock().unwrap();
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("state.sqlite");
        let store = MetadataStore::open(&path)?;
        let mode: String = store
            .connection
            .pragma_query_value(None, "journal_mode", |r| r.get(0))?;
        let synchronous: u32 = store
            .connection
            .pragma_query_value(None, "synchronous", |r| r.get(0))?;
        assert_eq!(mode, "wal");
        assert_eq!(synchronous, 2);
        store.connection.pragma_update(None, "user_version", 15)?;
        drop(store);
        assert!(MetadataStore::open(path).is_err());
        Ok(())
    }
    #[test]
    fn real_sigkill_keeps_unknown_launch_locked() -> Result<()> {
        // Isolate fork/exec crash probes: a child can briefly inherit unrelated flock descriptors.
        let _serial = crate::STORAGE_TEST_LOCK.lock().unwrap();
        const CHILD_PATH: &str = "PIGADE_SQLITE_CRASH_CHILD_PATH";
        if let Some(path) = std::env::var_os(CHILD_PATH) {
            let mut store = MetadataStore::open(path)?;
            let op = Uuid::from_u128(3);
            store.begin_launch(
                Uuid::from_u128(1),
                Uuid::from_u128(2),
                op,
                Uuid::from_u128(4),
            )?;
            store.register_helper(op, identity())?;
            store.permit_exec(op)?;
            // SAFETY: deliberately terminate only this isolated test child.
            unsafe {
                libc::raise(libc::SIGKILL);
            }
            unreachable!();
        }
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("state.sqlite");
        let result = std::process::Command::new(std::env::current_exe()?)
            .args([
                "--exact",
                "launch::tests::real_sigkill_keeps_unknown_launch_locked",
            ])
            .env(CHILD_PATH, &path)
            .output()?;
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(result.status.signal(), Some(libc::SIGKILL));
        let mut store = MetadataStore::open(path)?;
        assert_eq!(
            store.get_launch(Uuid::from_u128(3))?.unwrap().state,
            LaunchState::ExecPermitted
        );
        store.mark_unknown(Uuid::from_u128(3), "exec receipt lost")?;
        assert!(
            store
                .begin_launch(
                    Uuid::from_u128(1),
                    Uuid::new_v4(),
                    Uuid::new_v4(),
                    Uuid::new_v4()
                )
                .is_err()
        );
        Ok(())
    }
}

//! Versioned project/workspace context stored by the sole SQLite metadata writer.
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

use crate::MetadataStore;
use crate::{MemoryReview, MemorySelection};

pub const MAX_CONTEXT_BYTES: usize = 1024 * 1024;

type StoredContextRow = (i64, String, String, String, String, Option<i64>, String);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextDocument {
    pub scope_id: Uuid,
    /// Revision observed by the reader; successful saves return the next revision.
    pub revision: u64,
    pub instructions: String,
    pub role: String,
    pub memory: String,
    #[serde(default)]
    pub content_hash: String,
    #[serde(default)]
    pub saved_at_unix_ms: Option<u64>,
    #[serde(default)]
    pub author: String,
}

impl ContextDocument {
    fn empty(scope_id: Uuid) -> Self {
        Self {
            scope_id,
            revision: 0,
            instructions: String::new(),
            role: String::new(),
            memory: String::new(),
            content_hash: content_hash("", "", ""),
            saved_at_unix_ms: None,
            author: String::new(),
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffectiveContext {
    pub id: Uuid,
    pub created_at_unix_ms: u64,
    pub content_hash: String,
    pub blocks: Vec<ContextBlock>,
    pub text: String,
}

impl EffectiveContext {
    /// Verify the canonical content snapshot before it is persisted, displayed,
    /// or passed to a provider adapter.
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.blocks.len() <= 6,
            "effective context has too many blocks"
        );
        ensure!(
            self.blocks
                .iter()
                .all(|block| block.source.len() <= 64 && !block.source.contains('\0')),
            "effective context source metadata exceeds bounds"
        );
        let total = self.blocks.iter().try_fold(0usize, |sum, block| {
            ensure!(!block.text.contains('\0'), "effective context contains NUL");
            sum.checked_add(block.text.len())
                .context("effective context byte length overflow")
        })?;
        ensure!(
            total <= MAX_CONTEXT_BYTES,
            "effective context exceeds 1 MiB"
        );
        let joined = self
            .blocks
            .iter()
            .map(|block| block.text.as_str())
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n");
        ensure!(
            joined.len() <= MAX_CONTEXT_BYTES,
            "effective context exceeds 1 MiB"
        );
        ensure!(
            self.text == joined,
            "effective context text does not match blocks"
        );
        let canonical = serde_json::to_vec(&self.blocks)?;
        let digest = Sha256::digest(canonical);
        ensure!(
            self.content_hash == digest_hex(&digest),
            "effective context content hash mismatch"
        );
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextBlock {
    pub source: String,
    pub scope_id: Uuid,
    pub revision: u64,
    pub text: String,
}

fn content_hash(instructions: &str, role: &str, memory: &str) -> String {
    let mut hasher = Sha256::new();
    for part in [instructions, role, memory] {
        hasher.update((part.len() as u64).to_be_bytes());
        hasher.update(part.as_bytes());
    }
    let digest = hasher.finalize();
    digest_hex(&digest)
}

fn digest_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

fn now_unix_ms() -> Result<u64> {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock precedes Unix epoch")?
        .as_millis();
    u64::try_from(millis).context("Unix timestamp overflow")
}

fn role_instructions(role: &str) -> String {
    match role.trim().to_ascii_lowercase().as_str() {
        "implementer" => "Роль: реализуй задачу в назначенной области, сохраняй существующие изменения и проверь результат.\nRole: implement the assigned work, preserve existing changes, and verify the result.".to_owned(),
        "reviewer" => "Роль: ищи конкретные дефекты и подтверждай выводы кодом или тестами.\nRole: identify concrete defects and support findings with code or tests.".to_owned(),
        "researcher" => "Роль: собирай проверяемые факты, отмечай источники и отделяй выводы от предположений.\nRole: gather verifiable facts, identify sources, and distinguish inference from evidence.".to_owned(),
        "tester" => "Роль: проверяй поведение целевыми тестами и сохраняй фактические результаты.\nRole: validate behavior with targeted tests and report actual results.".to_owned(),
        _ => role.to_owned(),
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

/// Add provenance fields without inventing timestamps or hashes for old rows.
pub(crate) fn migrate_context_v5(connection: &Connection) -> Result<()> {
    connection.execute_batch(
        "ALTER TABLE context_revisions ADD COLUMN content_hash TEXT NOT NULL DEFAULT ''; \
         ALTER TABLE context_revisions ADD COLUMN saved_at_unix_ms INTEGER; \
         ALTER TABLE context_revisions ADD COLUMN author TEXT NOT NULL DEFAULT 'unknown';",
    )?;
    Ok(())
}

impl MetadataStore {
    /// Read the current immutable context revision, or an empty revision zero.
    pub fn context(&self, scope_id: Uuid) -> Result<ContextDocument> {
        let row: Option<StoredContextRow> = self
            .connection
            .query_row(
                "SELECT r.revision, r.instructions, r.role, r.memory,
                        r.content_hash, r.saved_at_unix_ms, r.author
                 FROM context_heads h
                 JOIN context_revisions r
                   ON r.scope_id=h.scope_id AND r.revision=h.revision
                 WHERE h.scope_id=?1",
                [scope_id.to_string()],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                    ))
                },
            )
            .optional()?;
        let Some((revision, instructions, role, memory, stored_hash, saved_at, author)) = row
        else {
            return Ok(ContextDocument::empty(scope_id));
        };
        ensure!(revision > 0, "invalid context revision");
        let calculated_hash = content_hash(&instructions, &role, &memory);
        if !stored_hash.is_empty() {
            ensure!(
                stored_hash == calculated_hash,
                "context content hash mismatch"
            );
        }
        let document = ContextDocument {
            scope_id,
            revision: revision as u64,
            instructions,
            role,
            memory,
            content_hash: calculated_hash,
            saved_at_unix_ms: saved_at
                .map(|value| u64::try_from(value).context("invalid context timestamp"))
                .transpose()?,
            author,
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
        let stored_hash = content_hash(&document.instructions, &document.role, &document.memory);
        let saved_at_unix_ms = now_unix_ms()?;
        tx.execute(
            "INSERT INTO context_revisions(
                scope_id,revision,instructions,role,memory,content_hash,saved_at_unix_ms,author
             ) VALUES(?1,?2,?3,?4,?5,?6,?7,'user')",
            params![
                document.scope_id.to_string(),
                next_revision as i64,
                &document.instructions,
                &document.role,
                &document.memory,
                stored_hash,
                saved_at_unix_ms as i64,
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
            content_hash: stored_hash,
            saved_at_unix_ms: Some(saved_at_unix_ms),
            author: "user".to_owned(),
            ..document
        })
    }

    /// Build an ordered immutable prompt snapshot from persisted project and
    /// workspace context plus explicitly supplied task/agent instructions.
    #[allow(clippy::too_many_arguments)]
    pub fn effective_context(
        &self,
        project: Uuid,
        workspace: Uuid,
        task: &str,
        workspace_revision: u64,
        role_override: &str,
        agent_instructions: &str,
        include_memory: bool,
    ) -> Result<EffectiveContext> {
        self.effective_context_with_memory_selection(
            project,
            workspace,
            task,
            workspace_revision,
            role_override,
            agent_instructions,
            include_memory,
            None,
        )
        .map(|(effective, _)| effective)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn effective_context_with_memory_selection(
        &self,
        project: Uuid,
        workspace: Uuid,
        task: &str,
        workspace_revision: u64,
        role_override: &str,
        agent_instructions: &str,
        include_memory: bool,
        selection: Option<&MemorySelection>,
    ) -> Result<(EffectiveContext, MemoryReview)> {
        let snapshot_id = Uuid::new_v4();
        let project_context = self.context(project)?;
        let workspace_context = self.context(workspace)?;
        let mut blocks = Vec::new();
        push_block(
            &mut blocks,
            "project_instructions",
            project,
            project_context.revision,
            &project_context.instructions,
        );
        push_block(
            &mut blocks,
            "workspace_instructions",
            workspace,
            workspace_context.revision,
            &workspace_context.instructions,
        );
        push_block(&mut blocks, "task", workspace, workspace_revision, task);

        let role = if role_override.is_empty() {
            workspace_context.role.as_str()
        } else {
            role_override
        };
        let expanded_role = role_instructions(role);
        push_block(
            &mut blocks,
            if role_override.is_empty() {
                "workspace_default_role"
            } else {
                "agent_role_override"
            },
            if role_override.is_empty() {
                workspace
            } else {
                snapshot_id
            },
            if role_override.is_empty() {
                workspace_context.revision
            } else {
                1
            },
            &expanded_role,
        );

        let memory_review =
            MemoryReview::from_document(&workspace_context, selection, include_memory)?;
        if include_memory && memory_review.selected_bytes > 0 {
            let memory_text = memory_review.selected_text(&workspace_context)?;
            push_block(
                &mut blocks,
                "workspace_memory",
                workspace,
                workspace_context.revision,
                &memory_text,
            );
        }
        push_block(
            &mut blocks,
            "agent_instructions",
            snapshot_id,
            1,
            agent_instructions,
        );

        let total = blocks.iter().try_fold(0usize, |sum, block| {
            sum.checked_add(block.text.len())
                .context("effective context byte length overflow")
        })?;
        ensure!(
            total <= MAX_CONTEXT_BYTES,
            "effective context exceeds 1 MiB"
        );
        ensure!(
            blocks.iter().all(|block| !block.text.contains('\0')),
            "effective context contains NUL"
        );
        let text = blocks
            .iter()
            .map(|block| block.text.as_str())
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n");
        ensure!(
            text.len() <= MAX_CONTEXT_BYTES,
            "effective context exceeds 1 MiB"
        );
        let canonical = serde_json::to_vec(&blocks)?;
        let digest = Sha256::digest(canonical);
        let effective = EffectiveContext {
            id: snapshot_id,
            created_at_unix_ms: now_unix_ms()?,
            content_hash: digest_hex(&digest),
            blocks,
            text,
        };
        effective.validate()?;
        Ok((effective, memory_review))
    }
}

fn push_block(
    blocks: &mut Vec<ContextBlock>,
    source: &str,
    scope_id: Uuid,
    revision: u64,
    text: &str,
) {
    if !text.is_empty() {
        blocks.push(ContextBlock {
            source: source.to_owned(),
            scope_id,
            revision,
            text: text.to_owned(),
        });
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
            content_hash: String::new(),
            saved_at_unix_ms: None,
            author: String::new(),
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
        assert_eq!(first.content_hash.len(), 64);
        assert!(first.saved_at_unix_ms.is_some());
        assert_eq!(first.author, "user");
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
        reopened.connection.execute(
            "UPDATE context_revisions SET content_hash='',saved_at_unix_ms=NULL,author='unknown'
             WHERE scope_id=?1 AND revision=2",
            [scope.to_string()],
        )?;
        let legacy = reopened.context(scope)?;
        assert_eq!(legacy.content_hash, second.content_hash);
        assert_eq!(legacy.saved_at_unix_ms, None);
        assert_eq!(legacy.author, "unknown");
        reopened.connection.execute(
            "UPDATE context_revisions SET content_hash='not-the-content-hash'
             WHERE scope_id=?1 AND revision=2",
            [scope.to_string()],
        )?;
        assert!(reopened.context(scope).is_err());
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
            content_hash: String::new(),
            saved_at_unix_ms: None,
            author: String::new(),
        };
        assert!(store.save_context(oversized, 0).is_err());
        assert_eq!(store.context(scope)?, ContextDocument::empty(scope));
        Ok(())
    }

    #[test]
    fn effective_context_preserves_source_order_and_selected_memory_bound() -> Result<()> {
        let _serial = crate::STORAGE_TEST_LOCK.lock().unwrap();
        let directory = tempdir()?;
        let mut store = MetadataStore::open(directory.path().join("metadata.sqlite"))?;
        let project = Uuid::new_v4();
        let workspace = Uuid::new_v4();
        let project_doc = ContextDocument {
            scope_id: project,
            revision: 0,
            instructions: "project rules".into(),
            role: String::new(),
            memory: String::new(),
            content_hash: String::new(),
            saved_at_unix_ms: None,
            author: String::new(),
        };
        let workspace_doc = ContextDocument {
            scope_id: workspace,
            revision: 0,
            instructions: "workspace rules".into(),
            role: "implementer".into(),
            memory: "selected memory".into(),
            content_hash: String::new(),
            saved_at_unix_ms: None,
            author: String::new(),
        };
        store.save_context(project_doc, 0)?;
        store.save_context(workspace_doc, 0)?;

        let effective = store.effective_context(
            project,
            workspace,
            "task request",
            7,
            "reviewer",
            "agent rules",
            true,
        )?;
        let sources: Vec<_> = effective
            .blocks
            .iter()
            .map(|block| block.source.as_str())
            .collect();
        assert_eq!(
            sources,
            [
                "project_instructions",
                "workspace_instructions",
                "task",
                "agent_role_override",
                "workspace_memory",
                "agent_instructions"
            ]
        );
        assert!(
            effective.blocks[3]
                .text
                .contains("Role: identify concrete defects")
        );
        assert!(
            effective
                .text
                .contains("project rules\n\nworkspace rules\n\ntask request")
        );
        assert_eq!(effective.content_hash.len(), 64);
        assert_eq!(effective.blocks[3].scope_id, effective.id);
        assert_eq!(effective.blocks[3].revision, 1);
        assert_eq!(effective.blocks[5].scope_id, effective.id);
        effective.validate()?;
        let mut altered_text = effective.clone();
        altered_text.text.push_str("tampered");
        assert!(altered_text.validate().is_err());
        let mut altered_block = effective.clone();
        altered_block.blocks[0].text.push_str("tampered");
        assert!(altered_block.validate().is_err());
        let mut altered_hash = effective.clone();
        let first_hash_digit = if altered_hash.content_hash.starts_with('0') {
            "1"
        } else {
            "0"
        };
        altered_hash
            .content_hash
            .replace_range(..1, first_hash_digit);
        assert!(altered_hash.validate().is_err());

        let too_large = ContextDocument {
            scope_id: workspace,
            revision: 1,
            instructions: "workspace rules".into(),
            role: "implementer".into(),
            memory: "m".repeat(32 * 1024 + 1),
            content_hash: String::new(),
            saved_at_unix_ms: None,
            author: String::new(),
        };
        store.save_context(too_large, 1)?;
        assert!(
            store
                .effective_context(project, workspace, "task", 8, "", "", true)
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn effective_context_includes_exact_selected_memory_slices_with_document_revision() -> Result<()>
    {
        let _serial = crate::STORAGE_TEST_LOCK.lock().unwrap();
        let directory = tempdir()?;
        let mut store = MetadataStore::open(directory.path().join("metadata.sqlite"))?;
        let project = Uuid::new_v4();
        let workspace = Uuid::new_v4();
        store.save_context(
            ContextDocument {
                scope_id: project,
                revision: 0,
                instructions: String::new(),
                role: String::new(),
                memory: String::new(),
                content_hash: String::new(),
                saved_at_unix_ms: None,
                author: String::new(),
            },
            0,
        )?;
        let saved = store.save_context(
            ContextDocument {
                scope_id: workspace,
                revision: 0,
                instructions: String::new(),
                role: String::new(),
                memory: "# Facts\nlarge\n## Decisions\nλ\n# Agreements\nlast\n".into(),
                content_hash: String::new(),
                saved_at_unix_ms: None,
                author: String::new(),
            },
            0,
        )?;
        let selection = MemorySelection {
            revision: saved.revision,
            content_hash: saved.content_hash.clone(),
            sections: vec![1],
        };
        let (effective, review) = store.effective_context_with_memory_selection(
            project,
            workspace,
            "task",
            1,
            "",
            "",
            true,
            Some(&selection),
        )?;
        assert_eq!(review.selected_text(&saved)?, "## Decisions\nλ\n");
        let block = effective
            .blocks
            .iter()
            .find(|block| block.source == "workspace_memory")
            .unwrap();
        assert_eq!(block.text, "## Decisions\nλ\n");
        assert_eq!(block.revision, saved.revision);
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
                 CREATE TABLE launch_intents (
                    operation_id TEXT PRIMARY KEY, session_id TEXT NOT NULL,
                    generation TEXT NOT NULL UNIQUE, nonce TEXT NOT NULL,
                    state TEXT NOT NULL CHECK(state IN ('intent','helper','permitted','running','unknown','exited')),
                    identity TEXT, reason TEXT
                 );
                 CREATE TABLE generation_locks (
                    session_id TEXT PRIMARY KEY, generation TEXT NOT NULL UNIQUE,
                    operation_id TEXT NOT NULL UNIQUE REFERENCES launch_intents(operation_id)
                 );
                 PRAGMA user_version=3;",
            )?;
        }
        let mut store = MetadataStore::open(&path)?;
        let current: u32 = store
            .connection
            .pragma_query_value(None, "user_version", |row| row.get(0))?;
        assert_eq!(current, 11);
        let scope = Uuid::new_v4();
        assert_eq!(store.context(scope)?, ContextDocument::empty(scope));
        let saved = store.save_context(document(scope, 0, "after migration"), 0)?;
        assert_eq!(saved.revision, 1);
        assert_eq!(store.context(scope)?, saved);
        let backup_path = store.migration_backup_path().context("v3 backup absent")?;
        assert!(backup_path.to_string_lossy().contains(".pre-v11."));
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

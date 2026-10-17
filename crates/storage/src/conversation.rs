//! Explicit, revisioned bindings between PigADE sessions and provider conversations.
use anyhow::{Context, Result, ensure};
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

const MAX_CONVERSATION_ID_BYTES: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConversationBinding {
    pub session_id: Uuid,
    pub source_generation: Uuid,
    pub plan_id: Uuid,
    pub context_revision: Uuid,
    pub provider: String,
    pub conversation_id: String,
    /// The revision observed by the writer. A successful save returns the next revision.
    pub revision: u64,
    pub bound_at_unix_ms: u64,
    pub author: String,
}

pub(crate) fn migrate_conversation_schema(connection: &rusqlite::Connection) -> Result<()> {
    connection.execute_batch(
        "CREATE TABLE conversation_bindings (
            session_id TEXT NOT NULL,
            revision INTEGER NOT NULL CHECK(revision>0),
            source_generation TEXT NOT NULL,
            plan_id TEXT NOT NULL,
            context_revision TEXT NOT NULL,
            provider TEXT NOT NULL CHECK(provider IN ('claude','codex','opencode')),
            conversation_id TEXT NOT NULL CHECK(length(CAST(conversation_id AS BLOB)) BETWEEN 1 AND 256),
            bound_at_unix_ms INTEGER NOT NULL CHECK(bound_at_unix_ms>=0),
            author TEXT NOT NULL CHECK(author='user'),
            PRIMARY KEY(session_id,revision)
        ) WITHOUT ROWID;
        CREATE TRIGGER conversation_bindings_no_update
        BEFORE UPDATE ON conversation_bindings
        BEGIN SELECT RAISE(ABORT, 'conversation binding history is immutable'); END;
        CREATE TRIGGER conversation_bindings_no_delete
        BEFORE DELETE ON conversation_bindings
        BEGIN SELECT RAISE(ABORT, 'conversation binding history is immutable'); END;",
    )?;
    Ok(())
}

impl ConversationBinding {
    fn validate(&self) -> Result<()> {
        ensure!(
            !self.session_id.is_nil()
                && !self.source_generation.is_nil()
                && !self.plan_id.is_nil()
                && !self.context_revision.is_nil(),
            "conversation binding contains a nil identity"
        );
        ensure!(
            matches!(self.provider.as_str(), "claude" | "codex" | "opencode"),
            "unsupported conversation provider"
        );
        ensure!(
            !self.conversation_id.is_empty()
                && self.conversation_id.len() <= MAX_CONVERSATION_ID_BYTES
                && self
                    .conversation_id
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_')),
            "invalid provider conversation ID"
        );
        ensure!(
            self.revision <= i64::MAX as u64,
            "conversation binding revision exhausted"
        );
        Ok(())
    }
}

impl crate::MetadataStore {
    /// Return the current explicitly selected provider conversation for a session.
    /// The persisted head is validated on every read before it can be resumed.
    pub fn conversation_binding(&self, session: Uuid) -> Result<Option<ConversationBinding>> {
        let row: Option<BindingRow> = self
            .connection
            .query_row(
                "SELECT session_id,source_generation,plan_id,context_revision,provider,
                        conversation_id,revision,bound_at_unix_ms,author
                 FROM conversation_bindings WHERE session_id=?1
                 ORDER BY revision DESC LIMIT 1",
                [session.to_string()],
                read_binding_row,
            )
            .optional()?;
        row.map(into_binding).transpose()
    }

    /// Append a user-confirmed binding if the caller's observed revision is current.
    /// A provider conversation can have only one current session head; rebinding
    /// this session advances its head and releases its previously bound ID.
    pub fn bind_conversation(
        &mut self,
        mut binding: ConversationBinding,
        expected_revision: u64,
    ) -> Result<ConversationBinding> {
        binding.validate()?;
        ensure!(
            binding.revision == expected_revision,
            "conversation binding revision differs from expected revision"
        );
        ensure!(
            expected_revision < i64::MAX as u64,
            "conversation binding revision exhausted"
        );
        let next_revision = expected_revision + 1;
        let bound_at_unix_ms = now_unix_ms()?;
        binding.revision = next_revision;
        binding.bound_at_unix_ms = bound_at_unix_ms;
        binding.author = "user".to_owned();

        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let actual: Option<i64> = tx
            .query_row(
                "SELECT revision FROM conversation_bindings WHERE session_id=?1
                 ORDER BY revision DESC LIMIT 1",
                [binding.session_id.to_string()],
                |row| row.get(0),
            )
            .optional()?;
        let actual = actual.unwrap_or(0);
        ensure!(
            actual >= 0 && actual as u64 == expected_revision,
            "conversation binding revision conflict (expected {expected_revision}, found {actual})"
        );
        let conflict: bool = tx.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM conversation_bindings current
                WHERE current.session_id<>?1
                  AND current.provider=?2 AND current.conversation_id=?3
                  AND current.revision=(
                      SELECT MAX(head.revision) FROM conversation_bindings head
                      WHERE head.session_id=current.session_id
                  )
             )",
            params![
                binding.session_id.to_string(),
                binding.provider,
                binding.conversation_id
            ],
            |row| row.get(0),
        )?;
        ensure!(
            !conflict,
            "provider conversation is already bound to another session"
        );
        tx.execute(
            "INSERT INTO conversation_bindings(
                session_id,revision,source_generation,plan_id,context_revision,
                provider,conversation_id,bound_at_unix_ms,author
             ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,'user')",
            params![
                binding.session_id.to_string(),
                next_revision as i64,
                binding.source_generation.to_string(),
                binding.plan_id.to_string(),
                binding.context_revision.to_string(),
                binding.provider,
                binding.conversation_id,
                bound_at_unix_ms as i64,
            ],
        )?;
        tx.commit()?;
        Ok(binding)
    }
}

type BindingRow = (
    String,
    String,
    String,
    String,
    String,
    String,
    i64,
    i64,
    String,
);

fn read_binding_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<BindingRow> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
        row.get(7)?,
        row.get(8)?,
    ))
}

fn into_binding(row: BindingRow) -> Result<ConversationBinding> {
    let (
        session,
        generation,
        plan,
        context,
        provider,
        conversation_id,
        revision,
        timestamp,
        author,
    ) = row;
    ensure!(
        revision > 0 && timestamp >= 0,
        "invalid stored conversation binding counters"
    );
    let binding = ConversationBinding {
        session_id: Uuid::parse_str(&session).context("invalid stored session ID")?,
        source_generation: Uuid::parse_str(&generation).context("invalid stored generation ID")?,
        plan_id: Uuid::parse_str(&plan).context("invalid stored plan ID")?,
        context_revision: Uuid::parse_str(&context)
            .context("invalid stored context revision ID")?,
        provider,
        conversation_id,
        revision: revision as u64,
        bound_at_unix_ms: timestamp as u64,
        author,
    };
    binding.validate()?;
    ensure!(
        binding.author == "user",
        "invalid stored conversation binding author"
    );
    Ok(binding)
}

fn now_unix_ms() -> Result<u64> {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock precedes Unix epoch")?
        .as_millis();
    u64::try_from(millis).context("Unix timestamp overflow")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ContextDocument, MetadataStore};
    use rusqlite::Connection;
    use tempfile::tempdir;

    fn binding(session_id: Uuid, revision: u64, conversation_id: &str) -> ConversationBinding {
        ConversationBinding {
            session_id,
            source_generation: Uuid::new_v4(),
            plan_id: Uuid::new_v4(),
            context_revision: Uuid::new_v4(),
            provider: "codex".into(),
            conversation_id: conversation_id.into(),
            revision,
            bound_at_unix_ms: 1,
            author: "untrusted caller value".into(),
        }
    }

    #[test]
    fn binding_is_validated_compare_and_swap_and_durable() -> Result<()> {
        let _serial = crate::STORAGE_TEST_LOCK.lock().unwrap();
        let directory = tempdir()?;
        let path = directory.path().join("metadata.sqlite");
        let session = Uuid::new_v4();
        let mut store = MetadataStore::open(&path)?;
        let before = now_unix_ms()?;
        let saved = store.bind_conversation(binding(session, 0, "Opaque_ID-42"), 0)?;
        let after = now_unix_ms()?;
        assert_eq!(saved.revision, 1);
        assert_eq!(saved.author, "user");
        assert!((before..=after).contains(&saved.bound_at_unix_ms));
        assert_eq!(store.conversation_binding(session)?, Some(saved.clone()));
        assert!(
            store
                .bind_conversation(binding(session, 0, "stale"), 0)
                .is_err()
        );

        let second = store.bind_conversation(binding(session, 1, "second"), 1)?;
        assert_eq!(second.revision, 2);
        assert_eq!(store.conversation_binding(session)?, Some(second.clone()));
        let history_count: usize = store.connection.query_row(
            "SELECT COUNT(*) FROM conversation_bindings WHERE session_id=?1",
            [session.to_string()],
            |row| row.get(0),
        )?;
        assert_eq!(history_count, 2);
        assert!(
            store
                .connection
                .execute(
                    "DELETE FROM conversation_bindings WHERE session_id=?1 AND revision=1",
                    params![session.to_string()],
                )
                .is_err()
        );
        drop(store);
        let reopened = MetadataStore::open(path)?;
        assert_eq!(reopened.conversation_binding(session)?, Some(second));

        let invalid_provider = ConversationBinding {
            provider: "shell".into(),
            ..binding(Uuid::new_v4(), 0, "id")
        };
        let mut reopened = reopened;
        assert!(reopened.bind_conversation(invalid_provider, 0).is_err());
        let invalid_id = binding(Uuid::new_v4(), 0, "../conversation");
        assert!(reopened.bind_conversation(invalid_id, 0).is_err());
        let nil_identity = ConversationBinding {
            plan_id: Uuid::nil(),
            ..binding(Uuid::new_v4(), 0, "id")
        };
        assert!(reopened.bind_conversation(nil_identity, 0).is_err());
        let exhausted = binding(Uuid::new_v4(), i64::MAX as u64, "id");
        exhausted.validate()?; // A final saved head remains readable.
        assert!(
            reopened
                .bind_conversation(exhausted, i64::MAX as u64)
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn current_conversation_head_is_unique_and_rebinding_releases_old_id() -> Result<()> {
        let _serial = crate::STORAGE_TEST_LOCK.lock().unwrap();
        let directory = tempdir()?;
        let mut store = MetadataStore::open(directory.path().join("metadata.sqlite"))?;
        let first_session = Uuid::new_v4();
        let second_session = Uuid::new_v4();
        store.bind_conversation(binding(first_session, 0, "same-conversation"), 0)?;
        assert!(
            store
                .bind_conversation(binding(second_session, 0, "same-conversation"), 0)
                .is_err()
        );
        store.bind_conversation(binding(first_session, 1, "new-conversation"), 1)?;
        let second = store.bind_conversation(binding(second_session, 0, "same-conversation"), 0)?;
        assert_eq!(second.conversation_id, "same-conversation");
        assert_eq!(
            store
                .conversation_binding(first_session)?
                .unwrap()
                .conversation_id,
            "new-conversation"
        );
        Ok(())
    }

    #[test]
    fn v5_upgrade_backups_and_preserves_catalog_launch_and_context_metadata() -> Result<()> {
        let _serial = crate::STORAGE_TEST_LOCK.lock().unwrap();
        let directory = tempdir()?;
        let path = directory.path().join("metadata.sqlite");
        let (session, generation, operation, nonce) = (
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
        );
        let (project, workspace, tab, scope) = (
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
        );
        {
            let mut store = MetadataStore::open(&path)?;
            store.connection.execute(
                "INSERT INTO projects(id,canonical_path,git_common_dir,name,revision) VALUES(?1,'/v5/project','/v5/project/.git','Preserved project',1)",
                [project.to_string()],
            )?;
            store.connection.execute(
                "INSERT INTO workspaces(id,project_id,task,cwd,mode,status,revision) VALUES(?1,?2,'task','/v5/worktree','worktree','ready',1)",
                params![workspace.to_string(), project.to_string()],
            )?;
            store.connection.execute(
                "INSERT INTO tabs(id,workspace_id,name,layout_json,revision) VALUES(?1,?2,'Preserved tab','null',1)",
                params![tab.to_string(), workspace.to_string()],
            )?;
            let saved_context = store.save_context(
                ContextDocument {
                    scope_id: scope,
                    revision: 0,
                    instructions: "v5 instructions".into(),
                    role: "reviewer".into(),
                    memory: "v5 memory".into(),
                    content_hash: String::new(),
                    saved_at_unix_ms: None,
                    author: String::new(),
                },
                0,
            )?;
            assert_eq!(saved_context.author, "user");
            assert!(saved_context.saved_at_unix_ms.is_some());
            store.begin_launch(session, generation, operation, nonce)?;
            drop(store);
            let connection = Connection::open(&path)?;
            connection.execute_batch(
                "DROP INDEX launch_session_generation_ordinal;
                 ALTER TABLE launch_intents DROP COLUMN generation_ordinal;
                 DROP TRIGGER conversation_bindings_no_update;
                 DROP TRIGGER conversation_bindings_no_delete;
                 DROP TABLE conversation_bindings;
                 DROP INDEX prepared_launch_generation_lookup;
                 DROP TABLE template_runs;
                 DROP TABLE layout_templates;
                 DROP TABLE pane_labels;
                 PRAGMA user_version=5;",
            )?;
        }

        let store = MetadataStore::open(&path)?;
        assert_eq!(
            store
                .connection
                .pragma_query_value::<u32, _>(None, "user_version", |row| row.get(0))?,
            10
        );
        assert_eq!(store.catalog()?.projects[0].name, "Preserved project");
        assert_eq!(store.catalog()?.workspaces[0].id, workspace);
        assert_eq!(store.catalog()?.tabs[0].id, tab);
        let context = store.context(scope)?;
        assert_eq!(context.instructions, "v5 instructions");
        assert_eq!(context.role, "reviewer");
        assert_eq!(context.memory, "v5 memory");
        assert_eq!(context.author, "user");
        assert!(context.saved_at_unix_ms.is_some());
        assert_eq!(context.content_hash.len(), 64);
        assert_eq!(store.get_launch(operation)?.unwrap().session_id, session);
        let backup_path = store.migration_backup_path().context("v5 backup absent")?;
        assert!(backup_path.to_string_lossy().contains(".pre-v10."));
        let backup =
            Connection::open_with_flags(backup_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        assert_eq!(
            backup.pragma_query_value::<u32, _>(None, "user_version", |row| row.get(0))?,
            5
        );
        assert_eq!(
            backup.query_row::<String, _, _>(
                "SELECT name FROM projects WHERE id=?1",
                [project.to_string()],
                |row| row.get(0)
            )?,
            "Preserved project"
        );
        assert_eq!(
            backup.query_row::<String, _, _>(
                "SELECT instructions FROM context_revisions WHERE scope_id=?1",
                [scope.to_string()],
                |row| row.get(0)
            )?,
            "v5 instructions"
        );
        let backup_context_metadata: (String, Option<i64>, String) = backup.query_row(
            "SELECT content_hash,saved_at_unix_ms,author FROM context_revisions WHERE scope_id=?1",
            [scope.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        assert_eq!(backup_context_metadata.0.len(), 64);
        assert!(backup_context_metadata.1.is_some());
        assert_eq!(backup_context_metadata.2, "user");
        assert_eq!(
            backup.query_row::<String, _, _>(
                "SELECT state FROM launch_intents WHERE operation_id=?1",
                [operation.to_string()],
                |row| row.get(0)
            )?,
            "intent"
        );
        Ok(())
    }
}

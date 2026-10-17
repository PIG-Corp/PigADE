//! Immutable launch snapshots and durable phase transitions.
use anyhow::{Result, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use uuid::Uuid;

const MAX_PREPARED_LAUNCH_BYTES: usize = 2 * 1024 * 1024;

pub(crate) fn migrate_prepared_launch_schema(connection: &Connection) -> Result<()> {
    connection.execute_batch(
        "CREATE TABLE prepared_launches (
            id TEXT PRIMARY KEY,
            payload TEXT NOT NULL,
            phase TEXT NOT NULL CHECK(phase IN (
                'prepared','attempting','delivered_unconfirmed','not_requested','unknown'
            ))
        ) WITHOUT ROWID;",
    )?;
    Ok(())
}

impl crate::MetadataStore {
    /// Persist a create-only launch payload before any external launch attempt.
    pub fn save_prepared_launch(&mut self, id: Uuid, payload: &str) -> Result<()> {
        ensure!(
            payload.len() <= MAX_PREPARED_LAUNCH_BYTES,
            "prepared launch payload exceeds 2 MiB"
        );
        serde_json::from_str::<serde_json::Value>(payload)
            .map_err(|error| anyhow::anyhow!("invalid prepared launch JSON: {error}"))?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "INSERT INTO prepared_launches(id,payload,phase) VALUES(?1,?2,'prepared')",
            params![id.to_string(), payload],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn prepared_launch(&self, id: Uuid) -> Result<Option<(String, String)>> {
        Ok(self
            .connection
            .query_row(
                "SELECT payload,phase FROM prepared_launches WHERE id=?1",
                [id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?)
    }

    /// Find a persisted launch snapshot by its immutable session generation.
    /// Treat duplicate matches as a catalog invariant violation instead of
    /// choosing one arbitrarily.
    pub fn prepared_launch_for_generation(
        &self,
        session: Uuid,
        generation: Uuid,
    ) -> Result<Option<(String, String)>> {
        let mut statement = self.connection.prepare(
            "SELECT payload,phase FROM prepared_launches
             WHERE json_extract(payload,'$.launch.session')=?1
               AND json_extract(payload,'$.launch.generation')=?2
             LIMIT 2",
        )?;
        let mut rows = statement.query(params![session.to_string(), generation.to_string()])?;
        let Some(first) = rows.next()? else {
            return Ok(None);
        };
        let matched = (first.get(0)?, first.get(1)?);
        ensure!(
            rows.next()?.is_none(),
            "duplicate prepared launch session generation"
        );
        Ok(Some(matched))
    }

    /// Read only the stable, user-facing fields from the exact immutable head
    /// snapshot. SQLite projects these JSON paths without copying launch args,
    /// environment, context, or payload text into the host.
    pub fn panel_launch_metadata(
        &self,
        session: Uuid,
        generation: Uuid,
    ) -> Result<Option<(Option<String>, String, bool)>> {
        let mut statement = self.connection.prepare(
            "SELECT NULLIF(json_extract(payload,'$.preview.provider'),''),
                    substr(COALESCE(
                      NULLIF(json_extract(payload,'$.preview.role_override'),''),
                      (SELECT json_extract(context_block.value,'$.text')
                       FROM json_each(prepared_launches.payload,'$.preview.context.blocks') AS context_block
                       WHERE json_extract(context_block.value,'$.source') IN
                         ('workspace_default_role','agent_role_override')
                       LIMIT 1),
                      ''
                    ),1,2048),
                    json_extract(payload,'$.preview.deliver_context')
             FROM prepared_launches
             WHERE json_extract(payload,'$.launch.session')=?1
               AND json_extract(payload,'$.launch.generation')=?2
             LIMIT 2",
        )?;
        let mut rows = statement.query(params![session.to_string(), generation.to_string()])?;
        let Some(first) = rows.next()? else {
            return Ok(None);
        };
        let metadata = (
            first.get::<_, Option<String>>(0)?,
            first.get::<_, Option<String>>(1)?.unwrap_or_default(),
            first.get::<_, Option<bool>>(2)?.unwrap_or(false),
        );
        ensure!(
            rows.next()?.is_none(),
            "duplicate prepared launch session generation"
        );
        ensure!(
            metadata
                .0
                .as_ref()
                .is_none_or(|provider| provider.len() <= 64)
                && metadata.1.len() <= 8 * 1024,
            "prepared panel metadata exceeds bounds"
        );
        Ok(Some(metadata))
    }

    /// Query-plan-visible check for the immutable session/generation index used
    /// by directory search. This avoids an accidental table scan regression.
    #[cfg(test)]
    pub(crate) fn panel_metadata_lookup_uses_index(&self) -> Result<bool> {
        let mut plan = self.connection.prepare(
            "EXPLAIN QUERY PLAN SELECT json_extract(payload,'$.preview.provider')
             FROM prepared_launches
             WHERE json_extract(payload,'$.launch.session')=?1
               AND json_extract(payload,'$.launch.generation')=?2 LIMIT 2",
        )?;
        let details = plan.query_map(["session", "generation"], |row| row.get::<_, String>(3))?;
        for detail in details {
            if detail?.contains("prepared_launch_generation_lookup") {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Atomically move a prepared payload into the attempt phase.
    pub fn begin_prepared_launch(&mut self, id: Uuid) -> Result<()> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = tx.execute(
            "UPDATE prepared_launches SET phase='attempting' WHERE id=?1 AND phase='prepared'",
            [id.to_string()],
        )?;
        ensure!(
            changed == 1,
            "prepared launch is missing or already attempted"
        );
        tx.commit()?;
        Ok(())
    }

    /// Record the result of the launch attempt. No phase implies application of
    /// any context revision; the payload remains an immutable input snapshot.
    pub fn finish_prepared_launch(&mut self, id: Uuid, phase: &str) -> Result<()> {
        ensure!(
            matches!(phase, "delivered_unconfirmed" | "unknown" | "not_requested"),
            "invalid prepared launch terminal phase"
        );
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = tx.execute(
            "UPDATE prepared_launches SET phase=?2 WHERE id=?1 AND phase='attempting'",
            params![id.to_string(), phase],
        )?;
        ensure!(changed == 1, "prepared launch phase transition is invalid");
        tx.commit()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn prepared_payload_is_create_only_and_phases_are_compare_and_swap() -> Result<()> {
        let _serial = crate::STORAGE_TEST_LOCK.lock().unwrap();
        let directory = tempdir()?;
        let mut store = crate::MetadataStore::open(directory.path().join("metadata.sqlite"))?;
        let id = Uuid::new_v4();
        let payload = r#"{"context_hash":"abc","context_revision":4,"args":["x"]}"#;
        store.save_prepared_launch(id, payload)?;
        assert_eq!(
            store.prepared_launch(id)?,
            Some((payload.to_owned(), "prepared".to_owned()))
        );
        assert!(store.save_prepared_launch(id, "{}").is_err());
        assert!(
            store
                .finish_prepared_launch(id, "delivered_unconfirmed")
                .is_err()
        );
        store.begin_prepared_launch(id)?;
        store.finish_prepared_launch(id, "delivered_unconfirmed")?;
        assert_eq!(
            store.prepared_launch(id)?,
            Some((payload.to_owned(), "delivered_unconfirmed".to_owned()))
        );
        assert!(store.begin_prepared_launch(id).is_err());
        assert!(store.finish_prepared_launch(id, "applied").is_err());
        let not_requested = Uuid::new_v4();
        store.save_prepared_launch(not_requested, "{}")?;
        store.begin_prepared_launch(not_requested)?;
        store.finish_prepared_launch(not_requested, "not_requested")?;
        assert_eq!(
            store
                .prepared_launch(not_requested)?
                .map(|(_, phase)| phase),
            Some("not_requested".to_owned())
        );

        let session = Uuid::new_v4();
        let generation_a = Uuid::new_v4();
        let generation_b = Uuid::new_v4();
        let payload_for = |generation: Uuid| {
            serde_json::json!({
                "launch": {
                    "session": session,
                    "generation": generation,
                    "provider": "local"
                }
            })
            .to_string()
        };
        let payload_a = payload_for(generation_a);
        let payload_b = payload_for(generation_b);
        store.save_prepared_launch(Uuid::new_v4(), &payload_a)?;
        store.save_prepared_launch(Uuid::new_v4(), &payload_b)?;
        assert_eq!(
            store.prepared_launch_for_generation(session, generation_a)?,
            Some((payload_a, "prepared".to_owned()))
        );
        assert_eq!(
            store.prepared_launch_for_generation(session, Uuid::new_v4())?,
            None
        );
        let panel_generation = Uuid::new_v4();
        let panel_payload = serde_json::json!({
            "launch": { "session": session, "generation": panel_generation },
            "preview": {
                "provider": "claude",
                "role_override": "reviewer",
                "deliver_context": true,
                "agent_instructions": "must not be read by directory lookup"
            },
            "large_other_field": "x".repeat(64 * 1024)
        })
        .to_string();
        store.save_prepared_launch(Uuid::new_v4(), &panel_payload)?;
        assert_eq!(
            store.panel_launch_metadata(session, panel_generation)?,
            Some((Some("claude".into()), "reviewer".into(), true))
        );
        assert!(store.panel_metadata_lookup_uses_index()?);
        let default_role_generation = Uuid::new_v4();
        let default_role_payload = serde_json::json!({
            "launch": { "session": session, "generation": default_role_generation },
            "preview": {
                "provider": "codex",
                "role_override": "",
                "deliver_context": false,
                "context": { "blocks": [
                    { "source": "workspace_default_role", "text": "workspace analyst role" }
                ] }
            }
        })
        .to_string();
        store.save_prepared_launch(Uuid::new_v4(), &default_role_payload)?;
        assert_eq!(
            store.panel_launch_metadata(session, default_role_generation)?,
            Some((Some("codex".into()), "workspace analyst role".into(), false))
        );
        Ok(())
    }
}

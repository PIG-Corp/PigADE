//! Bounded, revisioned prompt library records.
//!
//! Deleted prompt IDs remain as compact tombstones so they can never be
//! silently reused and repeated deletes can be recognized safely.

use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const MAX_PROMPTS: usize = 128;
pub const MAX_PROMPT_RECORDS: usize = 4096;
pub const MAX_PROMPT_TITLE_BYTES: usize = 256;
pub const MAX_PROMPT_BYTES: usize = 32 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Prompt {
    pub id: Uuid,
    pub title: String,
    pub content: String,
    pub revision: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptDelete {
    pub id: Uuid,
    pub expected_revision: u64,
}

pub(crate) fn migrate_prompts(connection: &Connection) -> Result<()> {
    connection.execute_batch(
        "CREATE TABLE prompt_records (
            id TEXT PRIMARY KEY,
            title TEXT NOT NULL,
            content TEXT NOT NULL,
            revision INTEGER NOT NULL CHECK(revision>0),
            deleted INTEGER NOT NULL CHECK(deleted IN (0,1)),
            CHECK(deleted=0 OR (title='' AND content=''))
        ) WITHOUT ROWID;",
    )?;
    Ok(())
}

pub fn validate_prompt(title: &str, content: &str) -> Result<()> {
    ensure!(
        !title.trim().is_empty()
            && title.trim() == title
            && title.len() <= MAX_PROMPT_TITLE_BYTES
            && !title.chars().any(char::is_control),
        "prompt title is empty, untrimmed, or outside bounds"
    );
    ensure!(
        !content.is_empty()
            && content.len() <= MAX_PROMPT_BYTES
            && !content
                .chars()
                .any(|ch| ch.is_control() && ch != '\n' && ch != '\t'),
        "prompt content is empty, contains unsupported controls, or exceeds bounds"
    );
    Ok(())
}

fn validate_record(prompt: &Prompt) -> Result<()> {
    ensure!(!prompt.id.is_nil(), "prompt ID cannot be nil");
    validate_prompt(&prompt.title, &prompt.content)?;
    ensure!(prompt.revision < i64::MAX as u64, "invalid prompt revision");
    Ok(())
}

pub(crate) fn write_prompt(
    tx: &Transaction<'_>,
    prompt: Prompt,
    expected: Option<u64>,
) -> Result<()> {
    validate_record(&prompt)?;
    ensure!(
        prompt.revision == expected.unwrap_or(0),
        "prompt revision must match expected revision"
    );
    let id = prompt.id.to_string();
    let current: Option<(u64, bool)> = tx
        .query_row(
            "SELECT revision,deleted FROM prompt_records WHERE id=?1",
            [&id],
            |row| Ok((row.get(0)?, row.get::<_, i64>(1)? != 0)),
        )
        .optional()?;
    let next = match (current, expected) {
        (None, None) => 1,
        (Some((revision, false)), Some(expected)) if revision == expected => {
            ensure!(
                expected > 0 && expected < i64::MAX as u64,
                "invalid prompt revision"
            );
            expected + 1
        }
        (Some((_, true)), _) => anyhow::bail!("deleted prompt IDs cannot be reused"),
        _ => anyhow::bail!("prompt record revision must match expected revision"),
    };
    if current.is_none() {
        let records: usize =
            tx.query_row("SELECT COUNT(*) FROM prompt_records", [], |row| row.get(0))?;
        ensure!(
            records < MAX_PROMPT_RECORDS,
            "prompt record capacity exhausted"
        );
        tx.execute(
            "INSERT INTO prompt_records(id,title,content,revision,deleted) VALUES(?1,?2,?3,?4,0)",
            params![id, prompt.title, prompt.content, next],
        )?;
    } else {
        let changed = tx.execute(
            "UPDATE prompt_records SET title=?2,content=?3,revision=?4 WHERE id=?1 AND deleted=0 AND revision=?5",
            params![id, prompt.title, prompt.content, next, expected],
        )?;
        ensure!(changed == 1, "prompt record revision changed during update");
    }
    Ok(())
}

pub(crate) fn delete_prompt(tx: &Transaction<'_>, delete: PromptDelete) -> Result<()> {
    ensure!(
        delete.expected_revision > 0 && delete.expected_revision < i64::MAX as u64,
        "invalid prompt revision"
    );
    let id = delete.id.to_string();
    let current: Option<(u64, bool)> = tx
        .query_row(
            "SELECT revision,deleted FROM prompt_records WHERE id=?1",
            [&id],
            |row| Ok((row.get(0)?, row.get::<_, i64>(1)? != 0)),
        )
        .optional()?;
    let Some((revision, deleted)) = current else {
        anyhow::bail!("prompt record does not exist")
    };
    if deleted {
        ensure!(
            revision == delete.expected_revision + 1,
            "prompt record revision must match expected revision"
        );
        return Ok(());
    }
    ensure!(
        revision == delete.expected_revision,
        "prompt record revision must match expected revision"
    );
    let changed = tx.execute(
        "UPDATE prompt_records SET title='',content='',revision=?2,deleted=1 WHERE id=?1 AND revision=?3 AND deleted=0",
        params![id, delete.expected_revision + 1, delete.expected_revision],
    )?;
    ensure!(changed == 1, "prompt record revision changed during delete");
    Ok(())
}

pub(crate) fn read_prompts(connection: &Connection) -> Result<Vec<Prompt>> {
    let total: usize =
        connection.query_row("SELECT COUNT(*) FROM prompt_records", [], |row| row.get(0))?;
    ensure!(
        total <= MAX_PROMPT_RECORDS,
        "prompt record capacity exceeded"
    );
    let active: usize = connection.query_row(
        "SELECT COUNT(*) FROM prompt_records WHERE deleted=0",
        [],
        |row| row.get(0),
    )?;
    ensure!(active <= MAX_PROMPTS, "too many active prompts");
    let mut prompts = Vec::with_capacity(active);
    let mut statement = connection.prepare(
        "SELECT id,title,content,revision FROM prompt_records WHERE deleted=0 ORDER BY id",
    )?;
    for row in statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, u64>(3)?,
        ))
    })? {
        let (id, title, content, revision) = row?;
        let prompt = Prompt {
            id: Uuid::parse_str(&id)?,
            title,
            content,
            revision,
        };
        validate_record(&prompt).context("invalid stored prompt")?;
        prompts.push(prompt);
    }
    Ok(prompts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CatalogBatch, CatalogWrite, MetadataStore};

    fn prompt(id: Uuid, revision: u64, content: &str) -> Prompt {
        Prompt {
            id,
            title: "Prompt title".into(),
            content: content.into(),
            revision,
        }
    }

    #[test]
    fn prompt_cas_survives_reopen_and_rejects_stale_drafts() -> Result<()> {
        let _serial = crate::STORAGE_TEST_LOCK.lock().unwrap();
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("state.sqlite");
        let id = Uuid::new_v4();
        {
            let mut store = MetadataStore::open(&path)?;
            let saved = store.apply_catalog(CatalogBatch {
                prompts: vec![CatalogWrite {
                    value: prompt(id, 0, "first\nline"),
                    expected_revision: None,
                }],
                ..CatalogBatch::default()
            })?;
            assert_eq!(saved.prompts.as_ref().unwrap()[0].revision, 1);
            let saved = store.apply_catalog(CatalogBatch {
                prompts: vec![CatalogWrite {
                    value: prompt(id, 1, "updated\tcontent"),
                    expected_revision: Some(1),
                }],
                ..CatalogBatch::default()
            })?;
            assert_eq!(saved.prompts.as_ref().unwrap()[0].revision, 2);
            assert!(
                store
                    .apply_catalog(CatalogBatch {
                        prompts: vec![CatalogWrite {
                            value: prompt(id, 1, "stale draft"),
                            expected_revision: Some(1),
                        }],
                        ..CatalogBatch::default()
                    })
                    .is_err()
            );
        }
        let store = MetadataStore::open(&path)?;
        let saved = store.catalog()?;
        assert_eq!(
            saved.prompts.unwrap(),
            vec![prompt(id, 2, "updated\tcontent")]
        );
        Ok(())
    }

    #[test]
    fn prompt_delete_is_revisioned_idempotent_and_never_reuses_id() -> Result<()> {
        let _serial = crate::STORAGE_TEST_LOCK.lock().unwrap();
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("state.sqlite");
        let id = Uuid::new_v4();
        let mut store = MetadataStore::open(&path)?;
        store.apply_catalog(CatalogBatch {
            prompts: vec![CatalogWrite {
                value: prompt(id, 0, "content"),
                expected_revision: None,
            }],
            ..CatalogBatch::default()
        })?;
        let delete = PromptDelete {
            id,
            expected_revision: 1,
        };
        assert!(
            store
                .apply_catalog(CatalogBatch {
                    remove_prompts: vec![PromptDelete {
                        id,
                        expected_revision: 0
                    }],
                    ..CatalogBatch::default()
                })
                .is_err()
        );
        for _ in 0..2 {
            let catalog = store.apply_catalog(CatalogBatch {
                remove_prompts: vec![delete],
                ..CatalogBatch::default()
            })?;
            assert!(catalog.prompts.unwrap().is_empty());
        }
        assert!(
            store
                .apply_catalog(CatalogBatch {
                    remove_prompts: vec![PromptDelete {
                        id,
                        expected_revision: 2
                    }],
                    ..CatalogBatch::default()
                })
                .is_err()
        );
        assert!(
            store
                .apply_catalog(CatalogBatch {
                    prompts: vec![CatalogWrite {
                        value: prompt(id, 0, "reuse"),
                        expected_revision: None,
                    }],
                    ..CatalogBatch::default()
                })
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn prompt_validation_and_batch_failure_are_bounded_and_atomic() -> Result<()> {
        let _serial = crate::STORAGE_TEST_LOCK.lock().unwrap();
        assert!(validate_prompt("ok", "line 1\nline 2\tend").is_ok());
        assert!(validate_prompt(" padded ", "content").is_err());
        assert!(validate_prompt("", "content").is_err());
        assert!(validate_prompt("title", "bad\rcontrol").is_err());
        assert!(validate_prompt("title", &"x".repeat(MAX_PROMPT_BYTES + 1)).is_err());

        let dir = tempfile::tempdir()?;
        let mut store = MetadataStore::open(dir.path().join("state.sqlite"))?;
        assert!(
            store
                .apply_catalog(CatalogBatch {
                    prompts: vec![CatalogWrite {
                        value: prompt(Uuid::nil(), 0, "nil id"),
                        expected_revision: None,
                    }],
                    ..CatalogBatch::default()
                })
                .is_err()
        );
        let first = Uuid::new_v4();
        let invalid = Uuid::new_v4();
        assert!(
            store
                .apply_catalog(CatalogBatch {
                    prompts: vec![
                        CatalogWrite {
                            value: prompt(first, 0, "valid"),
                            expected_revision: None
                        },
                        CatalogWrite {
                            value: prompt(invalid, 0, "bad\u{1}"),
                            expected_revision: None
                        },
                    ],
                    ..CatalogBatch::default()
                })
                .is_err()
        );
        assert!(store.catalog()?.prompts.unwrap().is_empty());
        Ok(())
    }

    #[test]
    fn prompt_active_and_tombstone_capacity_are_explicit() -> Result<()> {
        let _serial = crate::STORAGE_TEST_LOCK.lock().unwrap();
        let dir = tempfile::tempdir()?;
        let mut store = MetadataStore::open(dir.path().join("state.sqlite"))?;
        let writes = (0..MAX_PROMPTS)
            .map(|_| CatalogWrite {
                value: prompt(Uuid::new_v4(), 0, "bounded"),
                expected_revision: None,
            })
            .collect();
        store.apply_catalog(CatalogBatch {
            prompts: writes,
            ..CatalogBatch::default()
        })?;
        assert!(
            store
                .apply_catalog(CatalogBatch {
                    prompts: vec![CatalogWrite {
                        value: prompt(Uuid::new_v4(), 0, "one too many"),
                        expected_revision: None,
                    }],
                    ..CatalogBatch::default()
                })
                .is_err()
        );
        assert_eq!(store.catalog()?.prompts.unwrap().len(), MAX_PROMPTS);

        for index in 0..MAX_PROMPT_RECORDS - MAX_PROMPTS {
            store.connection.execute(
                "INSERT INTO prompt_records(id,title,content,revision,deleted) VALUES(?1,'','',1,1)",
                [Uuid::from_u128(index as u128 + 10_000).to_string()],
            )?;
        }
        assert!(
            store
                .apply_catalog(CatalogBatch {
                    prompts: vec![CatalogWrite {
                        value: prompt(Uuid::new_v4(), 0, "record cap"),
                        expected_revision: None,
                    }],
                    ..CatalogBatch::default()
                })
                .is_err()
        );
        assert_eq!(store.catalog()?.prompts.unwrap().len(), MAX_PROMPTS);
        Ok(())
    }

    #[test]
    fn catalog_defaults_prompt_capability_for_old_hosts() -> Result<()> {
        let mut value = serde_json::to_value(crate::Catalog {
            projects: vec![],
            workspaces: vec![],
            tabs: vec![],
            selection: crate::Selection::default(),
            layout: crate::LayoutDocument::default(),
            layout_templates: None,
            template_launch_schema: None,
            pane_labels: None,
            agent_profiles: None,
            agent_profile_roster: None,
            prompts: None,
            tab_close_schema: None,
            engine_options_schema: None,
            pane_tab_drop_schema: Some(1),
            pane_workspace_drop_schema: Some(1),
            history_grid_schema: Some(1),
            custom_cli_schema: Some(1),
        })?;
        value.as_object_mut().unwrap().remove("prompts");
        value
            .as_object_mut()
            .unwrap()
            .remove("engine_options_schema");
        let decoded: crate::Catalog = serde_json::from_value(value)?;
        assert_eq!(decoded.prompts, None);
        assert_eq!(decoded.engine_options_schema, None);
        Ok(())
    }
}

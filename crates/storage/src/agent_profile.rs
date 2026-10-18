//! Reusable, bounded agent launch settings.
//!
//! Profiles contain launch configuration only. Session identity, credentials,
//! working directories and context snapshots belong to their own records.

use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::LaunchSettings;

pub const MAX_AGENT_PROFILES: usize = 128;
pub const MAX_AGENT_PROFILE_SETTINGS_BYTES: usize = 128 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentProfile {
    pub id: Uuid,
    pub name: String,
    pub settings: LaunchSettings,
    pub revision: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentProfileDelete {
    pub id: Uuid,
    pub expected_revision: u64,
}

/// Agent profiles intentionally exclude shell launches. Their settings are
/// bounded by the same limits as template launch settings.
pub fn validate_agent_profile_settings(settings: &LaunchSettings) -> Result<()> {
    ensure!(
        matches!(
            settings.provider.as_str(),
            "claude" | "codex" | "opencode" | "custom"
        ),
        "unsupported agent profile provider"
    );
    settings.validate()
}

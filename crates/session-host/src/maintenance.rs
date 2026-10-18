//! Bounded host lifecycle status returned without copying terminal frames.
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Status {
    pub schema: u16,
    pub incarnation: Uuid,
    pub version: String,
    pub pid: u32,
    pub active_sessions: usize,
    pub unresolved_launches: usize,
    pub git_busy: bool,
    pub resume_busy: bool,
    pub can_shutdown: bool,
    pub blockers: Vec<String>,
}

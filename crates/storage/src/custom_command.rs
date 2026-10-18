//! Exact custom executable and argument vector for generic CLI launches.
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

pub const MAX_CUSTOM_EXECUTABLE_BYTES: usize = 4096;
pub const MAX_CUSTOM_ARGUMENTS: usize = 64;
pub const MAX_CUSTOM_ARGUMENT_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustomCommand {
    pub executable: String,
    pub args: Vec<String>,
}

impl CustomCommand {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            !self.executable.is_empty()
                && self.executable.len() <= MAX_CUSTOM_EXECUTABLE_BYTES
                && std::path::Path::new(&self.executable).is_absolute()
                && !self.executable.chars().any(char::is_control),
            "custom executable must be an absolute bounded path without control characters"
        );
        ensure!(
            self.args.len() <= MAX_CUSTOM_ARGUMENTS,
            "too many custom arguments"
        );
        let mut bytes = 0usize;
        for arg in &self.args {
            ensure!(!arg.contains('\0'), "custom arguments must not contain NUL");
            bytes = bytes
                .checked_add(arg.len())
                .context("custom argument size overflow")?;
        }
        ensure!(
            bytes <= MAX_CUSTOM_ARGUMENT_BYTES,
            "custom arguments exceed 16 KiB"
        );
        Ok(())
    }
}

use anyhow::Context;

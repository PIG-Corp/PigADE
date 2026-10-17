//! Bounded durable journals and the persistent launch state machine.
//!
//! A journal receipt is produced only after fsync. Callers may publish terminal
//! model changes only after obtaining it. Recovery never modifies source bytes.
pub mod archive;
mod catalog;
mod context;
mod journal;
mod launch;

pub use catalog::*;
pub use context::*;
pub use journal::*;
pub use launch::*;

use std::{fs::File, os::fd::AsRawFd};

/// A nonblocking exclusive advisory lock held for the lifetime of `file`.
fn lock_writer(file: &File) -> anyhow::Result<()> {
    // SAFETY: flock only reads the valid borrowed descriptor and does not retain it.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}

#[cfg(test)]
static STORAGE_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

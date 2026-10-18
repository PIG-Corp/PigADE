//! Generation-owned Unix PTY process-group shutdown.
//!
//! The caller must exclusively own the still-unreaped `portable-pty` child for
//! this identity. On Unix, portable-pty starts it with `setsid()`, and the
//! helper execs the CLI in place, so the child PID is also its original session
//! and process-group ID. Keeping the child unreaped pins that numeric identity
//! while signals are sent. This type never waits for or reaps the child.

use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use pigade_storage::ProcessIdentity;

const HANGUP_GRACE: Duration = Duration::from_millis(200);

#[derive(Debug, Clone, PartialEq, Eq)]
enum Phase {
    Ready,
    Grace { deadline: Instant },
    Complete,
    Failed(String),
    Disarmed,
}

/// Sends SIGHUP once, then SIGKILL after a short grace period.
///
/// Duplicate requests are idempotent. A failed signal attempt is terminal and
/// is never retried. The actor must call [`Self::disarm`] immediately after it
/// successfully reaps the child, including from degraded-state handling.
pub(crate) struct GroupStop {
    identity: ProcessIdentity,
    phase: Phase,
}

impl GroupStop {
    /// Creates a data-only stop controller for a previously registered child.
    pub(crate) fn new(identity: ProcessIdentity) -> Self {
        Self {
            identity,
            phase: Phase::Ready,
        }
    }

    /// Requests graceful termination once. Caller must still own the child and
    /// must not have reaped it.
    pub(crate) fn request(&mut self, now: Instant) -> Result<()> {
        match &self.phase {
            Phase::Ready => {}
            Phase::Failed(message) => return Err(anyhow!(message.clone())),
            Phase::Disarmed => {
                bail!("PTY child was already reaped; process-group stop is unavailable")
            }
            Phase::Grace { .. } | Phase::Complete => return Ok(()),
        }

        let Some(deadline) = now.checked_add(HANGUP_GRACE) else {
            return self.fail(anyhow!("PTY stop grace deadline overflow"));
        };
        match self.signal(libc::SIGHUP) {
            Ok(true) => self.phase = Phase::Grace { deadline },
            Ok(false) => self.phase = Phase::Complete,
            Err(error) => return self.fail(error),
        }
        Ok(())
    }

    /// Sends the one forced termination signal once the grace period expires.
    /// This method never sleeps or reaps the child.
    pub(crate) fn service(&mut self, now: Instant) -> Result<()> {
        let deadline = match &self.phase {
            Phase::Grace { deadline } => *deadline,
            Phase::Failed(message) => return Err(anyhow!(message.clone())),
            Phase::Ready | Phase::Complete | Phase::Disarmed => return Ok(()),
        };
        if now < deadline {
            return Ok(());
        }

        match self.signal(libc::SIGKILL) {
            Ok(_) => self.phase = Phase::Complete,
            Err(error) => return self.fail(error),
        }
        Ok(())
    }

    /// Time until the forced signal is due, if a graceful request is pending.
    pub(crate) fn remaining(&self, now: Instant) -> Option<Duration> {
        match &self.phase {
            Phase::Grace { deadline } => Some(deadline.saturating_duration_since(now)),
            _ => None,
        }
    }

    /// Whether the caller must defer reaping to preserve the signal target.
    pub(crate) fn blocks_reap(&self) -> bool {
        matches!(&self.phase, Phase::Grace { .. })
    }

    /// Permanently prevents further signals after the caller reaps the child.
    pub(crate) fn disarm(&mut self) {
        self.phase = Phase::Disarmed;
    }

    /// Returns `Ok(true)` when the signal was sent and `Ok(false)` when the
    /// group was already gone. Errors are latched by the caller as terminal.
    fn signal(&self, signal: libc::c_int) -> Result<bool> {
        let Some(pid) = self.validate_target()? else {
            return Ok(false);
        };
        // SAFETY: negative pid targets only the validated original process
        // group; the caller's unreaped-child ownership pins its identity.
        let result = unsafe { libc::kill(-pid, signal) };
        if result == 0 {
            return Ok(true);
        }
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ESRCH) {
            return Ok(false);
        }
        Err(error).context("signal owned PTY process group")
    }

    /// Revalidates the frozen identity and original PTY session/group directly
    /// before each signal. `None` means the target has already disappeared.
    fn validate_target(&self) -> Result<Option<libc::pid_t>> {
        // An unreaped Child pins its PID only when the parent does not
        // auto-reap SIGCHLD or delegate reaping to an unknown handler. The host
        // owns all waits for this child; it never changes this policy while
        // actors run. Refuse inherited non-default policies before any signal.
        let mut action = std::mem::MaybeUninit::<libc::sigaction>::uninit();
        // SAFETY: null installs nothing; sigaction initializes the output on success.
        if unsafe { libc::sigaction(libc::SIGCHLD, std::ptr::null(), action.as_mut_ptr()) } != 0 {
            return Err(std::io::Error::last_os_error()).context("inspect host SIGCHLD policy");
        }
        // SAFETY: successful sigaction initialized the output above.
        let action = unsafe { action.assume_init() };
        validate_reaping_policy(action.sa_sigaction, action.sa_flags)?;
        let raw_pid = self.identity.pid;
        if raw_pid < 2 {
            bail!("invalid PTY process identity PID");
        }
        let pid = libc::pid_t::try_from(raw_pid).context("PTY PID does not fit pid_t")?;
        // SAFETY: getpid/getpgrp/getsid(0) have no pointer arguments.
        let own_pid = unsafe { libc::getpid() };
        // SAFETY: getpid/getpgrp/getsid(0) have no pointer arguments.
        let own_group = unsafe { libc::getpgrp() };
        // SAFETY: getsid accepts zero to query the caller's own session.
        let own_session = unsafe { libc::getsid(0) };
        if own_group == -1 || own_session == -1 {
            return Err(std::io::Error::last_os_error())
                .context("inspect host process group/session");
        }
        if pid == own_pid {
            bail!("refusing to signal the session host itself");
        }

        match crate::identity(raw_pid) {
            Ok(current) if current == self.identity => {}
            Ok(_) => bail!("PTY process identity changed before group signal"),
            Err(error) => return Err(error).context("revalidate PTY process identity"),
        }

        // SAFETY: both calls use a positive, range-checked PID.
        let group = unsafe { libc::getpgid(pid) };
        if group == -1 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ESRCH) {
                return Ok(None);
            }
            return Err(error).context("inspect PTY process group");
        }
        // SAFETY: getsid uses a positive, range-checked PID.
        let session = unsafe { libc::getsid(pid) };
        if session == -1 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ESRCH) {
                return Ok(None);
            }
            return Err(error).context("inspect PTY process session");
        }
        if group != pid || session != pid || own_group == pid || own_session == pid {
            bail!("PTY process no longer owns an isolated original group and session");
        }
        Ok(Some(pid))
    }

    fn fail<T>(&mut self, error: anyhow::Error) -> Result<T> {
        let message = format!("{error:#}");
        self.phase = Phase::Failed(message.clone());
        Err(anyhow!(message))
    }
}

fn validate_reaping_policy(handler: libc::sighandler_t, flags: libc::c_int) -> Result<()> {
    if handler != libc::SIG_DFL || flags & libc::SA_NOCLDWAIT != 0 {
        bail!("host SIGCHLD policy cannot guarantee exclusive unreaped-child ownership");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn controller() -> GroupStop {
        GroupStop::new(ProcessIdentity {
            boot_id: "boot".into(),
            pid: 42,
            start_ticks: 7,
        })
    }

    #[test]
    fn auto_reaping_or_an_unknown_reaper_never_authorizes_numeric_group_signals() {
        assert!(validate_reaping_policy(libc::SIG_DFL, 0).is_ok());
        assert!(validate_reaping_policy(libc::SIG_DFL, libc::SA_RESTART).is_ok());
        assert!(validate_reaping_policy(libc::SIG_IGN, 0).is_err());
        assert!(validate_reaping_policy(libc::SIG_DFL, libc::SA_NOCLDWAIT).is_err());
        // A non-default handler could reap the child behind the actor's back.
        assert!(validate_reaping_policy(2, 0).is_err());
    }
    #[test]
    fn initial_controller_does_not_block_reaping_or_schedule_a_signal() {
        let stop = controller();
        assert!(!stop.blocks_reap());
        assert_eq!(stop.remaining(Instant::now()), None);
    }

    #[test]
    fn disarm_is_permanent_and_clears_pending_deadline() {
        let mut stop = controller();
        stop.phase = Phase::Grace {
            deadline: Instant::now() + HANGUP_GRACE,
        };
        stop.disarm();
        assert!(!stop.blocks_reap());
        assert_eq!(stop.remaining(Instant::now()), None);
        assert!(stop.request(Instant::now()).is_err());
        assert!(matches!(stop.phase, Phase::Disarmed));
        assert!(stop.service(Instant::now() + HANGUP_GRACE).is_ok());
    }

    #[test]
    fn host_identity_is_rejected_and_failure_is_not_retried() {
        let identity = crate::identity(std::process::id()).unwrap();
        let mut stop = GroupStop::new(identity);
        assert!(stop.request(Instant::now()).is_err());
        assert!(matches!(&stop.phase, Phase::Failed(_)));
        assert!(stop.request(Instant::now()).is_err());
        assert!(!stop.blocks_reap());
    }
}

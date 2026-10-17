//! Bounded, coalescing notification tokens for published host state.
//!
//! This module carries no terminal data. Callers should notify only after a
//! complete durable model update has been published. Waiters compare a token
//! and sleep under the same mutex used by `notify`, preventing lost wakeups.

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::{
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use uuid::Uuid;

pub const MAX_WAITERS: usize = 4;
pub const MIN_WAIT_TIMEOUT: Duration = Duration::from_millis(1);
pub const MAX_WAIT_TIMEOUT: Duration = Duration::from_millis(1000);

/// Opaque host-incarnation and publication revision. A restarted host cannot
/// accidentally accept a token from its previous process lifetime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Token {
    pub host_instance: Uuid,
    pub revision: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WaitResult {
    pub token: Token,
    pub changed: bool,
}

#[derive(Debug)]
struct State {
    host_instance: Uuid,
    revision: u64,
    active_waiters: usize,
    closed: bool,
}

#[derive(Debug)]
struct Inner {
    state: Mutex<State>,
    changed: Condvar,
    failure_reported: AtomicBool,
}

/// One host-local epoch source. Clones share the same bounded state.
#[derive(Debug, Clone)]
pub struct ChangeHub(Arc<Inner>);

impl ChangeHub {
    pub fn new() -> Self {
        Self(Arc::new(Inner {
            state: Mutex::new(State {
                host_instance: Uuid::new_v4(),
                revision: 0,
                active_waiters: 0,
                closed: false,
            }),
            changed: Condvar::new(),
            failure_reported: AtomicBool::new(false),
        }))
    }

    /// Return the current epoch. Poisoning or revision exhaustion fails closed.
    pub fn token(&self) -> Result<Token> {
        let state = self
            .0
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("change hub state poisoned"))?;
        ensure!(
            !state.closed,
            "change hub is closed after revision exhaustion"
        );
        Ok(Token {
            host_instance: state.host_instance,
            revision: state.revision,
        })
    }

    /// Advance the coalescing epoch. All current waiters wake; no per-event
    /// notification queue is retained.
    pub fn notify(&self) -> Result<Token> {
        let mut state = self
            .0
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("change hub state poisoned"))?;
        ensure!(
            !state.closed,
            "change hub is closed after revision exhaustion"
        );
        let Some(revision) = state.revision.checked_add(1) else {
            state.closed = true;
            drop(state);
            self.0.changed.notify_all();
            bail!("change hub revision exhausted; restart required");
        };
        state.revision = revision;
        let token = Token {
            host_instance: state.host_instance,
            revision,
        };
        drop(state);
        self.0.changed.notify_all();
        Ok(token)
    }

    /// Wait for a different host token or bounded timeout. Foreign-host tokens
    /// are immediately stale. No host/view lock should be held by the caller.
    pub fn wait(&self, after: Token, timeout: Duration) -> Result<WaitResult> {
        ensure!(
            (MIN_WAIT_TIMEOUT..=MAX_WAIT_TIMEOUT).contains(&timeout),
            "change wait timeout must be between 1 and 1000 ms"
        );
        let deadline = Instant::now()
            .checked_add(timeout)
            .context("change wait deadline overflow")?;

        // Declared before the mutex guard so unwinding drops the guard first,
        // then releases this reservation even if a panic poisons the mutex.
        let mut reservation = WaiterReservation {
            inner: &self.0,
            active: false,
        };
        let mut state = self
            .0
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("change hub state poisoned"))?;
        ensure!(
            !state.closed,
            "change hub is closed after revision exhaustion"
        );

        if token_changed(&state, after) {
            return Ok(WaitResult {
                token: current_token(&state),
                changed: true,
            });
        }
        ensure!(
            state.active_waiters < MAX_WAITERS,
            "change hub waiter limit reached"
        );
        state.active_waiters += 1;
        reservation.active = true;

        loop {
            if state.closed {
                drop(state);
                drop(reservation);
                bail!("change hub is closed after revision exhaustion");
            }
            if token_changed(&state, after) {
                let result = WaitResult {
                    token: current_token(&state),
                    changed: true,
                };
                drop(state);
                drop(reservation);
                return Ok(result);
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                let result = WaitResult {
                    token: current_token(&state),
                    changed: false,
                };
                drop(state);
                drop(reservation);
                return Ok(result);
            }
            let waited = self.0.changed.wait_timeout(state, remaining);
            match waited {
                Ok((next_state, _)) => state = next_state,
                Err(_) => {
                    // Poison is an explicit failure, never interpreted as a
                    // healthy timeout or a change notification.
                    drop(reservation);
                    bail!("change hub state poisoned while waiting");
                }
            }
        }
    }

    /// Run one diagnostic closure for the first persistent hub failure across
    /// all clones. Later failed polls/publications stay silent while preserving
    /// their error/fallback behavior.
    pub(crate) fn report_failure_once(&self, report: impl FnOnce()) -> bool {
        if self.0.failure_reported.swap(true, Ordering::Relaxed) {
            return false;
        }
        report();
        true
    }

    #[cfg(test)]
    pub(crate) fn close_for_test(&self) {
        let mut state = self.0.state.lock().unwrap();
        state.closed = true;
    }
}

impl Default for ChangeHub {
    fn default() -> Self {
        Self::new()
    }
}

fn token_changed(state: &State, after: Token) -> bool {
    after.host_instance != state.host_instance || after.revision != state.revision
}

fn current_token(state: &State) -> Token {
    Token {
        host_instance: state.host_instance,
        revision: state.revision,
    }
}

struct WaiterReservation<'a> {
    inner: &'a Inner,
    active: bool,
}

impl Drop for WaiterReservation<'_> {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        // Recover only to release the bounded reservation. The public operation
        // still reports poison as an error and never treats it as healthy state.
        let mut state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.active_waiters = state.active_waiters.saturating_sub(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        sync::{Barrier, mpsc},
        thread,
    };

    fn wait_for_waiters(hub: &ChangeHub, count: usize) {
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            if hub.0.state.lock().unwrap().active_waiters == count {
                return;
            }
            assert!(Instant::now() < deadline, "waiters did not register");
            thread::yield_now();
        }
    }

    #[test]
    fn notify_before_wait_is_observed_without_sleeping() -> Result<()> {
        let hub = ChangeHub::new();
        let before = hub.token()?;
        let after = hub.notify()?;
        let result = hub.wait(before, Duration::from_millis(100))?;
        assert_eq!(
            result,
            WaitResult {
                token: after,
                changed: true
            }
        );
        Ok(())
    }

    #[test]
    fn notify_after_wait_registration_wakes_waiter() -> Result<()> {
        let hub = ChangeHub::new();
        let before = hub.token()?;
        let waiter_hub = hub.clone();
        let (done_tx, done_rx) = mpsc::sync_channel(1);
        let started = Barrier::new(2);
        let waiter_started = &started;
        let waiter = thread::scope(|scope| {
            let handle = scope.spawn(move || {
                waiter_started.wait();
                done_tx
                    .send(waiter_hub.wait(before, Duration::from_millis(500)))
                    .unwrap();
            });
            started.wait();
            wait_for_waiters(&hub, 1);
            let notified = hub.notify()?;
            let result = done_rx.recv_timeout(Duration::from_secs(1))??;
            handle.join().unwrap();
            Ok::<_, anyhow::Error>((notified, result))
        })?;
        assert_eq!(
            waiter.1,
            WaitResult {
                token: waiter.0,
                changed: true
            }
        );
        assert_eq!(hub.0.state.lock().unwrap().active_waiters, 0);
        Ok(())
    }

    #[test]
    fn multiple_notifications_coalesce_to_latest_epoch() -> Result<()> {
        let hub = ChangeHub::new();
        let before = hub.token()?;
        for _ in 0..7 {
            hub.notify()?;
        }
        let current = hub.token()?;
        let result = hub.wait(before, Duration::from_millis(50))?;
        assert_eq!(
            result,
            WaitResult {
                token: current,
                changed: true
            }
        );
        assert_eq!(current.revision, before.revision + 7);
        Ok(())
    }

    #[test]
    fn timeout_and_foreign_host_token_are_explicit() -> Result<()> {
        let hub = ChangeHub::new();
        let token = hub.token()?;
        assert_eq!(
            hub.wait(token, Duration::from_millis(1))?,
            WaitResult {
                token,
                changed: false,
            }
        );
        let foreign = Token {
            host_instance: Uuid::new_v4(),
            revision: token.revision,
        };
        assert!(hub.wait(foreign, Duration::from_millis(100))?.changed);
        assert!(hub.wait(token, Duration::ZERO).is_err());
        assert!(hub.wait(token, Duration::from_millis(1001)).is_err());
        Ok(())
    }

    #[test]
    fn waiter_count_is_bounded_and_released_on_wake() -> Result<()> {
        let hub = ChangeHub::new();
        let token = hub.token()?;
        let started = Arc::new(Barrier::new(MAX_WAITERS + 1));
        let mut joins = Vec::new();
        for _ in 0..MAX_WAITERS {
            let hub = hub.clone();
            let started = started.clone();
            joins.push(thread::spawn(move || {
                started.wait();
                hub.wait(token, Duration::from_millis(900))
            }));
        }
        started.wait();
        wait_for_waiters(&hub, MAX_WAITERS);
        assert!(hub.wait(token, Duration::from_millis(100)).is_err());
        hub.notify()?;
        for join in joins {
            assert!(join.join().unwrap()?.changed);
        }
        assert_eq!(hub.0.state.lock().unwrap().active_waiters, 0);
        Ok(())
    }

    #[test]
    fn revision_overflow_closes_and_wakes_every_waiter() -> Result<()> {
        let hub = ChangeHub::new();
        let token = hub.token()?;
        {
            let mut state = hub.0.state.lock().unwrap();
            state.revision = u64::MAX;
        }
        let token = Token {
            revision: u64::MAX,
            ..token
        };
        let waiter_hub = hub.clone();
        let waiter = thread::spawn(move || waiter_hub.wait(token, Duration::from_millis(900)));
        wait_for_waiters(&hub, 1);
        assert!(hub.notify().is_err());
        assert!(waiter.join().unwrap().is_err());
        assert!(hub.token().is_err());
        assert!(hub.notify().is_err());
        Ok(())
    }

    #[test]
    fn poisoned_mutex_fails_closed_without_hanging() {
        let hub = ChangeHub::new();
        let poison_hub = hub.clone();
        let _ = thread::spawn(move || {
            let _state = poison_hub.0.state.lock().unwrap();
            panic!("intentional test poison");
        })
        .join();
        assert!(hub.token().is_err());
        assert!(hub.notify().is_err());
        assert!(
            hub.wait(
                Token {
                    host_instance: Uuid::nil(),
                    revision: 0
                },
                Duration::from_millis(10)
            )
            .is_err()
        );
    }

    #[test]
    fn failure_report_reservation_is_shared_by_all_hub_clones() {
        let hub = ChangeHub::new();
        let clone = hub.clone();
        let reports = std::sync::atomic::AtomicUsize::new(0);
        assert!(hub.report_failure_once(|| {
            reports.fetch_add(1, Ordering::Relaxed);
        }));
        assert!(!clone.report_failure_once(|| {
            reports.fetch_add(1, Ordering::Relaxed);
        }));
        assert!(!hub.report_failure_once(|| {
            reports.fetch_add(1, Ordering::Relaxed);
        }));
        assert_eq!(reports.load(Ordering::Relaxed), 1);
    }
}

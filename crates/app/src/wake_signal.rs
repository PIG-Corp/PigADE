//! A bounded, cloneable wake signal for coalescing state-change notifications.
//!
//! The signal retains one pending bit and at most one waiter Waker. It is
//! intended for one async consumer; simultaneous waiters are reported as busy.

use std::{
    future::{Future, poll_fn},
    pin::Pin,
    sync::{Arc, Mutex, TryLockError},
    task::{Context, Poll, Waker},
};

#[derive(Clone, Debug, Default)]
pub struct WakeSignal {
    inner: Arc<Inner>,
}

#[derive(Debug, Default)]
struct Inner {
    state: Mutex<State>,
}

#[derive(Debug, Default)]
struct State {
    pending: bool,
    next_waiter_id: u64,
    active_waiter_id: Option<u64>,
    waiter: Option<Waiter>,
}

#[derive(Debug)]
struct Waiter {
    id: u64,
    waker: Waker,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitError {
    /// The single-waiter contract was violated by polling a second waiter.
    AlreadyWaiting,
    /// The practically unreachable waiter identity counter was exhausted.
    WaiterIdExhausted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitOutcome {
    Notified,
    TimedOut,
    Failed(WaitError),
}

impl WakeSignal {
    /// Set the pending bit and wake the current waiter, if any.
    ///
    /// Notifications coalesce while the bit is set. The Waker is always called
    /// after releasing the mutex, so a synchronous executor may poll again
    /// without re-entering the signal lock.
    pub fn notify(&self) {
        let waker = {
            let mut state = self.lock();
            state.pending = true;
            state.waiter.take().map(|waiter| waiter.waker)
        };

        if let Some(waker) = waker {
            waker.wake();
        }
    }

    /// Wait for one coalesced notification.
    pub fn wait(&self) -> WaitFuture {
        WaitFuture {
            signal: self.clone(),
            waiter_id: None,
            complete: false,
        }
    }

    /// Wait until notified or until `timeout` completes.
    ///
    /// If both become ready in the same poll, the notification wins. The
    /// waiter unregisters itself when this future completes or is cancelled.
    pub async fn wait_or_timeout<F>(&self, timeout: F) -> WaitOutcome
    where
        F: Future<Output = ()>,
    {
        let mut waiter = std::pin::pin!(self.wait());
        let mut timeout = std::pin::pin!(timeout);

        poll_fn(|context| {
            match waiter.as_mut().poll(context) {
                Poll::Ready(Ok(())) => return Poll::Ready(WaitOutcome::Notified),
                Poll::Ready(Err(error)) => return Poll::Ready(WaitOutcome::Failed(error)),
                Poll::Pending => (),
            }

            if timeout.as_mut().poll(context).is_ready() {
                Poll::Ready(WaitOutcome::TimedOut)
            } else {
                Poll::Pending
            }
        })
        .await
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.inner
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }
}

pub struct WaitFuture {
    signal: WakeSignal,
    waiter_id: Option<u64>,
    complete: bool,
}

impl Future for WaitFuture {
    type Output = Result<(), WaitError>;

    fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        if self.complete {
            return Poll::Ready(Ok(()));
        }

        let signal = self.signal.clone();
        let mut state = signal.lock();
        if self.waiter_id.is_none() && state.active_waiter_id.is_some() {
            return Poll::Ready(Err(WaitError::AlreadyWaiting));
        }
        if state.pending {
            state.pending = false;
            if let Some(waiter_id) = self.waiter_id {
                if state.active_waiter_id == Some(waiter_id) {
                    state.active_waiter_id = None;
                }
                if state
                    .waiter
                    .as_ref()
                    .is_some_and(|waiter| waiter.id == waiter_id)
                {
                    state.waiter = None;
                }
            }
            drop(state);
            self.complete = true;
            return Poll::Ready(Ok(()));
        }

        let waiter_id = match self.waiter_id {
            Some(waiter_id) => waiter_id,
            None => {
                if state.waiter.is_some() {
                    return Poll::Ready(Err(WaitError::AlreadyWaiting));
                }
                let Some(waiter_id) = state.next_waiter_id.checked_add(1) else {
                    return Poll::Ready(Err(WaitError::WaiterIdExhausted));
                };
                state.next_waiter_id = waiter_id;
                state.active_waiter_id = Some(waiter_id);
                self.waiter_id = Some(waiter_id);
                waiter_id
            }
        };

        match state.waiter.as_mut() {
            Some(waiter) if waiter.id == waiter_id => {
                if !waiter.waker.will_wake(context.waker()) {
                    waiter.waker = context.waker().clone();
                }
            }
            Some(_) => return Poll::Ready(Err(WaitError::AlreadyWaiting)),
            None => {
                state.waiter = Some(Waiter {
                    id: waiter_id,
                    waker: context.waker().clone(),
                });
            }
        }

        Poll::Pending
    }
}

impl Drop for WaitFuture {
    fn drop(&mut self) {
        let Some(waiter_id) = self.waiter_id else {
            return;
        };
        let mut state = match self.signal.inner.state.try_lock() {
            Ok(state) => state,
            Err(TryLockError::Poisoned(poison)) => poison.into_inner(),
            Err(TryLockError::WouldBlock) => {
                // The only competing holder is a short notify/poll section.
                // Waiting here guarantees no Waker->task->signal cycle remains
                // after cancellation; no blocking work is done under the lock.
                self.signal.lock()
            }
        };
        if state.active_waiter_id == Some(waiter_id) {
            state.active_waiter_id = None;
        }
        if state
            .waiter
            .as_ref()
            .is_some_and(|waiter| waiter.id == waiter_id)
        {
            state.waiter = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        sync::{
            atomic::{AtomicUsize, Ordering},
            mpsc,
        },
        task::Wake,
        thread,
        time::Duration,
    };

    struct ThreadWaker(thread::Thread);

    impl Wake for ThreadWaker {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }

        fn wake_by_ref(self: &Arc<Self>) {
            self.0.unpark();
        }
    }

    fn block_on<F: Future>(future: F) -> F::Output {
        let waker = Waker::from(Arc::new(ThreadWaker(thread::current())));
        let mut context = Context::from_waker(&waker);
        let mut future = std::pin::pin!(future);
        loop {
            match future.as_mut().poll(&mut context) {
                Poll::Ready(result) => return result,
                Poll::Pending => thread::park(),
            }
        }
    }

    struct ReentrantWake {
        signal: WakeSignal,
        calls: AtomicUsize,
    }

    impl Wake for ReentrantWake {
        fn wake(self: Arc<Self>) {
            self.wake_by_ref();
        }

        fn wake_by_ref(self: &Arc<Self>) {
            // This lock-taking call deadlocks if notify invokes the Waker while
            // it still holds the signal mutex.
            let _ = self.signal.lock().pending;
            self.calls.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn poll_once<F: Future>(future: Pin<&mut F>, waker: &Waker) -> Poll<F::Output> {
        future.poll(&mut Context::from_waker(waker))
    }

    #[test]
    fn notification_before_wait_is_retained_once() {
        let signal = WakeSignal::default();
        signal.notify();
        assert_eq!(
            block_on(signal.wait_or_timeout(std::future::pending())),
            WaitOutcome::Notified
        );
        assert_eq!(
            block_on(signal.wait_or_timeout(std::future::ready(()))),
            WaitOutcome::TimedOut
        );
    }

    #[test]
    fn notification_after_arm_wakes_outside_the_lock() {
        let signal = WakeSignal::default();
        let reentrant = Arc::new(ReentrantWake {
            signal: signal.clone(),
            calls: AtomicUsize::new(0),
        });
        let waker = Waker::from(reentrant.clone());
        let mut waiter = Box::pin(signal.wait());
        assert!(poll_once(waiter.as_mut(), &waker).is_pending());
        signal.notify();
        assert_eq!(reentrant.calls.load(Ordering::SeqCst), 1);
        assert_eq!(poll_once(waiter.as_mut(), &waker), Poll::Ready(Ok(())));
    }

    #[test]
    fn burst_notifications_coalesce_to_one_pending_bit() {
        let signal = WakeSignal::default();
        for _ in 0..10_000 {
            signal.notify();
        }
        assert!(signal.lock().pending);
        assert_eq!(
            block_on(signal.wait_or_timeout(std::future::ready(()))),
            WaitOutcome::Notified
        );
        assert!(!signal.lock().pending);
        assert_eq!(
            block_on(signal.wait_or_timeout(std::future::ready(()))),
            WaitOutcome::TimedOut
        );
    }

    #[test]
    fn dropping_waiter_unregisters_waker_and_fresh_waiter_works() {
        let signal = WakeSignal::default();
        let waker = Waker::from(Arc::new(ThreadWaker(thread::current())));
        let mut cancelled = Box::pin(signal.wait());
        assert!(poll_once(cancelled.as_mut(), &waker).is_pending());
        assert!(signal.lock().waiter.is_some());
        signal.notify();
        assert!(signal.lock().pending);
        assert!(signal.lock().active_waiter_id.is_some());
        drop(cancelled);
        assert!(signal.lock().waiter.is_none());
        assert!(signal.lock().active_waiter_id.is_none());
        assert!(
            signal.lock().pending,
            "cancellation must preserve the event"
        );

        assert_eq!(
            block_on(signal.wait_or_timeout(std::future::pending())),
            WaitOutcome::Notified
        );
    }

    #[test]
    fn concurrent_waiter_is_explicitly_rejected() {
        let signal = WakeSignal::default();
        let waker = Waker::from(Arc::new(ThreadWaker(thread::current())));
        let mut first = Box::pin(signal.wait());
        let mut second = Box::pin(signal.wait());
        assert!(poll_once(first.as_mut(), &waker).is_pending());
        signal.notify();
        assert_eq!(
            poll_once(second.as_mut(), &waker),
            Poll::Ready(Err(WaitError::AlreadyWaiting))
        );
        assert_eq!(poll_once(first.as_mut(), &waker), Poll::Ready(Ok(())));
    }

    #[test]
    fn notify_racing_with_wait_registration_is_not_lost() {
        for _ in 0..100 {
            let signal = WakeSignal::default();
            let waiting_signal = signal.clone();
            let start = Arc::new(std::sync::Barrier::new(3));
            let waiter_start = start.clone();
            let notifier_start = start.clone();
            let (result_tx, result_rx) = mpsc::channel();
            let waiter = thread::spawn(move || {
                let waker = Waker::from(Arc::new(ThreadWaker(thread::current())));
                let mut context = Context::from_waker(&waker);
                let mut future = Box::pin(waiting_signal.wait());
                waiter_start.wait();
                loop {
                    match future.as_mut().poll(&mut context) {
                        Poll::Ready(result) => {
                            result_tx.send(result).unwrap();
                            break;
                        }
                        Poll::Pending => thread::park(),
                    }
                }
            });
            let notifying_signal = signal.clone();
            let notifier = thread::spawn(move || {
                notifier_start.wait();
                notifying_signal.notify();
            });
            start.wait();
            assert_eq!(
                result_rx.recv_timeout(Duration::from_secs(1)).unwrap(),
                Ok(())
            );
            notifier.join().unwrap();
            waiter.join().unwrap();
        }
    }
}

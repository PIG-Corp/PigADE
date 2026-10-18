//! Coalesced local read receipts; host output and CLI input never enter this path.
use crate::{
    CanvasApp, activity,
    product::{Dialog, DialogKind, ProductAction},
    wake_signal::WakeSignal,
};
use gpui::{Context, Window};
use std::{
    path::{Path, PathBuf},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

pub(crate) struct Writer {
    tx: mpsc::SyncSender<activity::Saved>,
    rx: mpsc::Receiver<(activity::Saved, Result<(), String>)>,
    confirmed: activity::Saved,
    inflight: Option<activity::Saved>,
    due: Instant,
    blocked: bool,
    pub initial: activity::Saved,
    pub initial_error: Option<String>,
}

impl Writer {
    pub fn new(socket: &Path, wake: WakeSignal) -> Self {
        Self::for_path(activity::path_for_socket(socket), wake)
    }

    fn for_path(path: Result<PathBuf, String>, wake: WakeSignal) -> Self {
        let loaded = path
            .as_ref()
            .map_err(Clone::clone)
            .and_then(|p| activity::load_from(p));
        let initial = loaded.clone().unwrap_or_default();
        let initial_error = loaded.err();
        let (tx, requests) = mpsc::sync_channel::<activity::Saved>(1);
        let (results, rx) = mpsc::sync_channel(1);
        thread::spawn(move || {
            while let Ok(saved) = requests.recv() {
                let result = path
                    .as_ref()
                    .map_err(Clone::clone)
                    .and_then(|path| activity::save_to(path, &saved));
                if results.send((saved, result)).is_err() {
                    return;
                }
                wake.notify();
            }
        });
        Self {
            tx,
            rx,
            confirmed: initial.clone(),
            inflight: None,
            due: Instant::now(),
            blocked: initial_error.is_some(),
            initial,
            initial_error,
        }
    }

    pub fn pending(&self, desired: &activity::Saved) -> bool {
        self.inflight.is_some() || self.confirmed != *desired
    }

    pub fn retry(&mut self) {
        self.blocked = false;
        self.due = Instant::now();
    }

    pub fn service(
        &mut self,
        desired: &activity::Saved,
        force: bool,
    ) -> Option<Result<(), String>> {
        let mut outcome = None;
        while let Ok((saved, result)) = self.rx.try_recv() {
            self.inflight = None;
            if result.is_ok() {
                self.confirmed = saved;
            } else {
                self.blocked = true;
            }
            outcome = Some(result);
        }
        if !self.blocked
            && self.inflight.is_none()
            && self.confirmed != *desired
            && (force || Instant::now() >= self.due)
        {
            match self.tx.try_send(desired.clone()) {
                Ok(()) => {
                    self.inflight = Some(desired.clone());
                    self.due = Instant::now() + Duration::from_secs(1);
                }
                Err(mpsc::TrySendError::Full(_)) => (),
                Err(mpsc::TrySendError::Disconnected(_)) => {
                    self.blocked = true;
                    outcome = Some(Err("Поток сохранения активности недоступен".into()));
                }
            }
        }
        outcome
    }
}

impl CanvasApp {
    pub(super) fn activity_action(&mut self, action: ProductAction, cx: &mut Context<Self>) {
        match action {
            ProductAction::OpenActivity => {
                self.product.dialog = Some(Dialog::new(DialogKind::Activity));
                self.product
                    .activity_scroll
                    .set_offset(gpui::point(gpui::px(0.), gpui::px(0.)));
            }
            ProductAction::MarkActivityRead(session, generation, sequence) => {
                if self.product.activity_supported {
                    self.product
                        .activity
                        .acknowledge(session, generation, sequence);
                }
            }
            ProductAction::MarkAllActivityRead => {
                if self.product.activity_supported {
                    self.product.activity.acknowledge_all();
                }
            }
            ProductAction::RetryActivitySave => self.activity_writer.retry(),
            _ => unreachable!("activity action routing"),
        }
        let count = self.product.activity_menu_entries().len();
        if let Some(dialog) = self
            .product
            .dialog
            .as_mut()
            .filter(|dialog| dialog.kind == DialogKind::Activity)
        {
            dialog.active_field = dialog.active_field.min(count.saturating_sub(1));
        }
        cx.notify();
    }

    pub(super) fn service_activity(&mut self, cx: &mut Context<Self>) {
        let mut changed = false;
        if self.activity_sample != self.state.activity {
            self.activity_sample = self.state.activity.clone();
            self.product.activity_feed_error = self.state.activity_error.clone();
            match self
                .state
                .activity
                .clone()
                .map(|snapshot| self.product.activity.observe(snapshot))
            {
                Some(Ok(_)) => self.product.activity_supported = true,
                Some(Err(error)) => {
                    self.product.activity_supported = false;
                    self.product.activity_feed_error =
                        Some(format!("Сводка активности отклонена: {error}"));
                }
                None => self.product.activity_supported = false,
            }
            changed = true;
        }
        if self.state.activity_error.is_some()
            && self.product.activity_feed_error != self.state.activity_error
        {
            self.product.activity_feed_error = self.state.activity_error.clone();
            changed = true;
        }
        if self.state.error.is_some() && self.product.activity_supported {
            self.product.activity_supported = false;
            changed = true;
        }
        let saved = self.product.activity.saved();
        if let Some(result) = self
            .activity_writer
            .service(&saved, self.close_flow.active())
        {
            self.product.activity_error = result.err();
            changed = true;
        }
        if changed {
            cx.notify();
        }
    }

    /// The canvas calls this after submitting the exact visible frames to its
    /// painter. Receiving a frame or selecting a tab alone is not a read.
    pub(super) fn activity_painted(
        &mut self,
        painted: &[crate::protocol::Known],
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        if !self.product.activity_supported
            || !window.is_window_active()
            || self.product.dialog.is_some()
            || self.close_flow.active()
            || self.state.error.is_some()
            || self.pane_drag.is_some()
            || self.resize_gesture.is_some()
        {
            return;
        }
        let mut changed = false;
        for frame in painted {
            let Some(view) =
                self.state.views.iter().find(|view| {
                    view.session == frame.session && view.generation == frame.generation
                })
            else {
                continue;
            };
            if self.history_target(view)
                || self.scrollback_display.as_ref().is_some_and(|history| {
                    history.session == frame.session && history.generation == frame.generation
                })
            {
                continue;
            }
            let (Ok(session), Ok(generation)) = (frame.session.parse(), frame.generation.parse())
            else {
                continue;
            };
            let matches_state = self.product.activity.snapshot().is_some_and(|snapshot| {
                snapshot.sessions.iter().any(|entry| {
                    entry.session == session
                        && entry.generation == generation
                        && entry.state == frame.state
                        && entry.storage_error == frame.storage_error.is_some()
                })
            });
            if matches_state {
                changed |= self
                    .product
                    .activity
                    .acknowledge(session, generation, frame.sequence);
            }
        }
        if changed {
            cx.notify();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn saved(bytes: u64, host: Uuid, session: Uuid, generation: Uuid) -> activity::Saved {
        activity::Saved {
            schema: 1,
            revision: 0,
            host_instance: Some(host),
            seen: vec![activity::Seen {
                session,
                generation,
                sequence: bytes,
                output_bytes: bytes,
                state: "running".into(),
                storage_error: false,
            }],
        }
    }

    fn settle(writer: &mut Writer, desired: &activity::Saved) -> Result<(), String> {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if let Some(result) = writer.service(desired, true) {
                result?;
            }
            if !writer.pending(desired) {
                return Ok(());
            }
            assert!(
                Instant::now() < deadline,
                "read receipt worker did not finish"
            );
            thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn async_receipts_coalesce_latest_intent_and_survive_reopen() {
        let dir = std::env::temp_dir().join(format!("pigade-activity-writer-{}", Uuid::new_v4()));
        let path = dir.join("reads.json");
        let mut writer = Writer::for_path(Ok(path.clone()), WakeSignal::default());
        let (host, session, generation) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let first = saved(1, host, session, generation);
        writer.service(&first, false);
        // Multiple newer observations need only the single latest intent.
        for bytes in 2..10 {
            writer.service(&saved(bytes, host, session, generation), false);
        }
        let last = saved(10, host, session, generation);
        settle(&mut writer, &last).unwrap();
        assert_eq!(activity::load_from(&path).unwrap(), last);
        let reopened = Writer::for_path(Ok(path.clone()), WakeSignal::default());
        assert_eq!(reopened.initial, last);
        assert!(!reopened.pending(&last));
        drop((writer, reopened));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn malformed_receipts_fail_visibly_without_overwriting_file_or_retrying() {
        let dir =
            std::env::temp_dir().join(format!("pigade-activity-writer-bad-{}", Uuid::new_v4()));
        std::fs::create_dir(&dir).unwrap();
        let path = dir.join("reads.json");
        std::fs::write(&path, b"malformed").unwrap();
        let mut writer = Writer::for_path(Ok(path.clone()), WakeSignal::default());
        assert!(writer.initial_error.is_some());
        let desired = saved(1, Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        assert!(writer.service(&desired, true).is_none());
        writer.retry();
        assert!(settle(&mut writer, &desired).is_err());
        assert!(
            writer.service(&desired, true).is_none(),
            "failed save must await an explicit retry"
        );
        assert_eq!(std::fs::read(path).unwrap(), b"malformed");
        drop(writer);
        std::fs::remove_dir_all(dir).unwrap();
    }
}

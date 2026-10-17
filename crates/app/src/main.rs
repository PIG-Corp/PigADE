//! PigADE native application. The session host owns ANSI parsing and CLI lifetime.
mod cell_style;
mod command_worker;
mod cursor;
mod font;
mod input;
mod mouse;
mod mouse_delivery;
mod palette;
mod protocol;
mod publication_trace;
mod selection;
mod wake_signal;
use command_worker::{
    CommandNotice, CommandTarget, Dispatch, RequestFailure, ResizeCache, ResizeKey, WorkerGate,
    confirmed_ended_state,
};
use gpui::{prelude::*, *};
use gpui_platform::application;
use mouse_delivery::{
    MouseDelivery, MouseSnapshot, Outcome as DeliveryOutcome, ReceiptEffect, RequestReceipt,
};
use palette::color;
use protocol::{
    Cell, CommandFailure, Poll, PollChanges, RecoveryDiagnostics, View, command, request,
};
use publication_trace::{Stage as TraceStage, Trace};
use serde_json::{Value, json};
use std::{
    ops::Range,
    path::PathBuf,
    sync::{Arc, Mutex, mpsc},
    thread,
    time::{Duration, Instant},
};
use wake_signal::{WaitOutcome, WakeSignal};

const HEADER: f32 = 32.;
const GAP: f32 = 6.;
#[derive(Clone, Default)]
struct State {
    revision: u64,
    views_revision: u64,
    views: Vec<View>,
    error: Option<String>,
    command_notices: Vec<CommandNotice>,
    recovery: Option<RecoveryDiagnostics>,
    poll_backpressured: bool,
}
impl State {
    fn fail(&mut self, error: String) {
        if self.error.is_none() {
            self.error = Some(error);
            self.revision = self.revision.wrapping_add(1);
        }
    }

    fn set_command_notices(&mut self, notices: Vec<CommandNotice>) {
        if self.command_notices != notices {
            self.command_notices = notices;
            self.revision = self.revision.wrapping_add(1);
        }
    }

    fn notice_for(&self, view: &View) -> Option<&CommandNotice> {
        self.command_notices.iter().find(|notice| {
            notice.target.session == view.session && notice.target.generation == view.generation
        })
    }
}

struct StateUpdate {
    revision: u64,
    views_revision: u64,
    views: Option<Vec<View>>,
    error: Option<String>,
    command_notices: Vec<CommandNotice>,
    recovery: Option<RecoveryDiagnostics>,
    poll_backpressured: bool,
}

fn state_update(current: &State, shared: &State) -> Option<StateUpdate> {
    (current.revision != shared.revision).then(|| StateUpdate {
        revision: shared.revision,
        views_revision: shared.views_revision,
        views: (current.views_revision != shared.views_revision).then(|| shared.views.clone()),
        error: shared.error.clone(),
        command_notices: shared.command_notices.clone(),
        recovery: shared.recovery.clone(),
        poll_backpressured: shared.poll_backpressured,
    })
}

fn apply_state_update(state: &mut State, update: StateUpdate) {
    state.revision = update.revision;
    state.error = update.error;
    state.command_notices = update.command_notices;
    state.recovery = update.recovery;
    state.poll_backpressured = update.poll_backpressured;
    if let Some(views) = update.views {
        state.views_revision = update.views_revision;
        state.views = views;
    }
}

fn composition_target_changed(state: &State, selected: usize, update: &StateUpdate) -> bool {
    if update.error.is_some() {
        return true;
    }
    let updated_target = update
        .views
        .as_ref()
        .and_then(|views| views.get(selected))
        .map(|view| (&view.session, &view.generation));
    let current_target = state
        .views
        .get(selected)
        .map(|view| (&view.session, &view.generation));
    if update.command_notices.iter().any(|notice| {
        updated_target.is_some_and(|(session, generation)| {
            notice.target.session == *session && notice.target.generation == *generation
        }) || current_target.is_some_and(|(session, generation)| {
            notice.target.session == *session && notice.target.generation == *generation
        })
    }) {
        return true;
    }
    let Some(views) = update.views.as_ref() else {
        return false;
    };
    match (state.views.get(selected), views.get(selected)) {
        (Some(old), Some(new)) => {
            old.session != new.session
                || old.generation != new.generation
                || new.state != "running"
                || new.storage_error.is_some()
                || input::protocol_error(new.frame.modes).is_some()
        }
        (None, None) => false,
        _ => true,
    }
}

fn cursor_target_changed(state: &State, selected: usize, update: &StateUpdate) -> bool {
    let Some(views) = &update.views else {
        return false;
    };
    match (state.views.get(selected), views.get(selected)) {
        (Some(old), Some(new)) => {
            old.session != new.session
                || old.generation != new.generation
                || old.frame.cursor != new.frame.cursor
                || old.frame.cursor_presentation != new.frame.cursor_presentation
                || old.state != new.state
        }
        (None, None) => false,
        _ => true,
    }
}

fn can_send_to_view(view: &View, state: &State, local_error: Option<&str>) -> bool {
    view.state == "running"
        && state.error.is_none()
        && state.notice_for(view).is_none()
        && local_error.is_none()
        && view.storage_error.is_none()
}

fn apply_poll(state: &mut State, reply: Poll) -> Result<PollChanges, String> {
    let changes = protocol::merge_poll(&mut state.views, &mut state.recovery, reply)?;
    if changes.views {
        state.views_revision = state.views_revision.wrapping_add(1);
    }
    if changes.views || changes.recovery {
        state.revision = state.revision.wrapping_add(1);
    }
    Ok(changes)
}

fn short_notice_part(value: &str, limit: usize) -> String {
    let mut chars = value.chars().map(|character| {
        if character.is_control() {
            ' '
        } else {
            character
        }
    });
    let mut part = chars.by_ref().take(limit).collect::<String>();
    if chars.next().is_some() {
        part.push('…');
    }
    part.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn recovery_warning(
    recovery: Option<&RecoveryDiagnostics>,
    selected_storage_error: Option<&str>,
) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(recovery) = recovery {
        if let Some(issue) = recovery.issues.first() {
            let journal = short_notice_part(&issue.journal, 32);
            let reason = short_notice_part(&issue.message, 72);
            let reason = if reason.is_empty() {
                "причина не указана"
            } else {
                &reason
            };
            parts.push(format!("Восстановление: {journal} — {reason}"));
            let extra =
                (recovery.issues.len().saturating_sub(1) as u64).saturating_add(recovery.omitted);
            if extra > 0 {
                parts.push(format!("ещё {extra} проблем"));
            }
        } else if recovery.omitted > 0 {
            parts.push(format!(
                "Восстановление: скрыто проблем {}",
                recovery.omitted
            ));
        }
    }
    if let Some(reason) = selected_storage_error {
        let reason = short_notice_part(reason, 72);
        let reason = if reason.is_empty() {
            "причина не указана"
        } else {
            &reason
        };
        parts.push(format!("История сессии: {reason}"));
    }
    (!parts.is_empty()).then(|| short_notice_part(&parts.join(" · "), 160))
}

enum IpcMessage {
    Request {
        id: u64,
        target: CommandTarget,
        command: Value,
    },
    AcknowledgeKnownRejection {
        target: CommandTarget,
        token: u64,
        cleanup: Option<mouse_delivery::Cleanup>,
    },
}

enum WorkerReceipt {
    Request(RequestReceipt),
    Acknowledgement {
        target: CommandTarget,
        token: u64,
        accepted: bool,
        cleanup_id: Option<u64>,
    },
}

fn command_target(command: &Value) -> Option<CommandTarget> {
    Some(CommandTarget {
        session: command.get("session")?.as_str()?.to_owned(),
        generation: command.get("generation")?.as_str()?.to_owned(),
    })
}

fn resize_key(command: &Value) -> Option<ResizeKey> {
    if command.get("command")?.as_str()? != "resize" {
        return None;
    }
    Some(ResizeKey {
        target: command_target(command)?,
        rows: command
            .get("geometry")?
            .get("rows")?
            .as_u64()?
            .try_into()
            .ok()?,
        cols: command
            .get("geometry")?
            .get("cols")?
            .as_u64()?
            .try_into()
            .ok()?,
    })
}

fn publish_worker_notices(shared: &Arc<Mutex<State>>, gate: &WorkerGate, wake: &WakeSignal) {
    let changed = {
        let mut state = shared.lock().unwrap();
        let revision = state.revision;
        state.set_command_notices(gate.notices());
        state.revision != revision
    };
    if changed {
        wake.notify();
    }
}

struct WorkerConnection {
    tx: mpsc::SyncSender<IpcMessage>,
    shared: Arc<Mutex<State>>,
    receipts: mpsc::Receiver<WorkerReceipt>,
    wake: WakeSignal,
    trace: Trace,
}

fn ipc(socket: PathBuf, trace: Trace) -> WorkerConnection {
    let (tx, rx) = mpsc::sync_channel(32);
    let (receipt_tx, receipt_rx) = mpsc::sync_channel(32);
    let shared = Arc::new(Mutex::new(State::default()));
    let output = shared.clone();
    let wake = WakeSignal::default();
    let worker_wake = wake.clone();
    let command_socket = socket.clone();
    let command_trace = trace.clone();
    thread::spawn(move || {
        let mut gate = WorkerGate::default();
        while let Ok(message) = rx.recv() {
            let ended = output
                .lock()
                .unwrap()
                .views
                .iter()
                .filter(|view| confirmed_ended_state(&view.state))
                .map(|view| CommandTarget {
                    session: view.session.clone(),
                    generation: view.generation.clone(),
                })
                .collect::<Vec<_>>();
            gate.remove_ended(ended.into_iter());
            publish_worker_notices(&output, &gate, &worker_wake);
            match message {
                IpcMessage::AcknowledgeKnownRejection {
                    target,
                    token,
                    cleanup,
                } => {
                    let cleanup_id = cleanup.as_ref().map(|cleanup| cleanup.id);
                    let accepted = gate.acknowledge(&target, token);
                    if accepted {
                        if let Some(cleanup) = cleanup {
                            let id = cleanup.id;
                            let command = json!({
                                "command": "input",
                                "session": target.session,
                                "generation": target.generation,
                                "bytes": cleanup.bytes,
                            });
                            let dispatch =
                                gate.dispatch_request_with_id(target.clone(), None, id, || {
                                    request(&command_socket, command)
                                        .map(|_| ())
                                        .map_err(|error| match error {
                                            CommandFailure::NotAdmitted { .. } => {
                                                RequestFailure::NotAdmitted(format!(
                                                    "Команда не принята: {error}"
                                                ))
                                            }
                                            CommandFailure::OutcomeUnknown(_) => {
                                                RequestFailure::OutcomeUnknown(format!(
                                                    "Исход команды неизвестен: {error}"
                                                ))
                                            }
                                        })
                                });
                            let outcome = delivery_outcome(&dispatch);
                            if dispatch != Dispatch::Executed {
                                publish_worker_notices(&output, &gate, &worker_wake);
                            }
                            if dispatch == Dispatch::CapacityExceeded {
                                let mut state = output.lock().unwrap();
                                state.fail("Слишком много заблокированных сессий команд".into());
                                state.set_command_notices(gate.notices());
                            }
                            if !send_worker_receipt(
                                &receipt_tx,
                                &worker_wake,
                                WorkerReceipt::Request(RequestReceipt {
                                    id,
                                    target: target.clone(),
                                    outcome,
                                }),
                            ) {
                                return;
                            }
                        }
                        publish_worker_notices(&output, &gate, &worker_wake);
                    }
                    if !send_worker_receipt(
                        &receipt_tx,
                        &worker_wake,
                        WorkerReceipt::Acknowledgement {
                            target,
                            token,
                            accepted,
                            cleanup_id,
                        },
                    ) {
                        return;
                    }
                }
                IpcMessage::Request {
                    id,
                    target,
                    command,
                } => {
                    let resize = resize_key(&command);
                    let dispatch =
                        gate.dispatch_request_with_id(target.clone(), resize, id, || {
                            command_trace.record(TraceStage::CommandDispatch, id, &[]);
                            let result = request(&command_socket, command);
                            command_trace.record(TraceStage::CommandReply, id, &[]);
                            result.map(|_| ()).map_err(|error| match error {
                                CommandFailure::NotAdmitted { .. } => RequestFailure::NotAdmitted(
                                    format!("Команда не принята: {error}"),
                                ),
                                CommandFailure::OutcomeUnknown(_) => {
                                    RequestFailure::OutcomeUnknown(format!(
                                        "Исход команды неизвестен: {error}"
                                    ))
                                }
                            })
                        });
                    if dispatch != Dispatch::Executed {
                        publish_worker_notices(&output, &gate, &worker_wake);
                    }
                    if dispatch == Dispatch::CapacityExceeded {
                        let mut state = output.lock().unwrap();
                        state.fail("Слишком много заблокированных сессий команд".into());
                        state.set_command_notices(gate.notices());
                    }
                    let outcome = delivery_outcome(&dispatch);
                    if !send_worker_receipt(
                        &receipt_tx,
                        &worker_wake,
                        WorkerReceipt::Request(RequestReceipt {
                            id,
                            target,
                            outcome,
                        }),
                    ) {
                        return;
                    }
                }
            }
        }
    });
    spawn_poll_worker(socket, Arc::downgrade(&shared), wake.clone(), trace.clone());
    WorkerConnection {
        tx,
        shared,
        receipts: receipt_rx,
        wake,
        trace,
    }
}

fn apply_poll_result(
    state: &mut State,
    result: Result<(Poll, Option<protocol::ChangeToken>), protocol::PollError>,
) -> Option<protocol::ChangeToken> {
    let backpressured = matches!(&result, Err(protocol::PollError::Busy));
    if state.poll_backpressured != backpressured {
        state.poll_backpressured = backpressured;
        state.revision = state.revision.wrapping_add(1);
    }
    match result {
        Ok((reply, token)) => match apply_poll(state, reply) {
            Ok(_) => token,
            Err(error) => {
                state.fail(error);
                None
            }
        },
        Err(protocol::PollError::Busy) => None,
        Err(protocol::PollError::Failed(error)) => {
            state.fail(error);
            None
        }
    }
}

fn trace_publication(trace: &Trace, stage: TraceStage, state: &State) {
    let mut sequences = [0; 4];
    let count = state.views.len().min(sequences.len());
    for (sequence, view) in sequences.iter_mut().zip(&state.views) {
        *sequence = view.sequence;
    }
    trace.record(stage, state.revision, &sequences[..count]);
}

fn spawn_poll_worker(
    socket: PathBuf,
    shared: std::sync::Weak<Mutex<State>>,
    wake: WakeSignal,
    trace: Trace,
) {
    thread::spawn(move || {
        let mut token = None;
        let mut next_poll = Instant::now();
        loop {
            let Some(shared) = shared.upgrade() else {
                return;
            };
            if let Some(after) = token.take() {
                // Wait on a separate connection/thread: neither input nor the
                // serialized host command dispatcher can wait behind this call.
                // A read-only wait failure falls back to bounded polling; the
                // subsequent validated Poll establishes current replica state.
                if protocol::wait_for_change(&socket, &after).is_err() {
                    thread::sleep(Duration::from_millis(50));
                }
            } else if next_poll > Instant::now() {
                thread::sleep(next_poll.saturating_duration_since(Instant::now()));
            }
            // Coalesce cell snapshots to at most60 polls/s during output bursts.
            // Every raw event remains in the host journal/history.
            if next_poll > Instant::now() {
                thread::sleep(next_poll.saturating_duration_since(Instant::now()));
            }
            let started = Instant::now();
            let known = shared
                .lock()
                .unwrap()
                .views
                .iter()
                .map(View::known)
                .collect::<Vec<_>>();
            let result = protocol::poll(&socket, &known);
            let changed = {
                let mut state = shared.lock().unwrap();
                let revision = state.revision;
                token = apply_poll_result(&mut state, result);
                if revision != state.revision {
                    trace_publication(&trace, TraceStage::PollApplied, &state);
                }
                revision != state.revision
            };
            if changed {
                wake.notify();
            }
            next_poll = started
                + if token.is_some() {
                    Duration::from_nanos(16_666_667)
                } else {
                    Duration::from_millis(50)
                };
        }
    });
}

fn send_worker_receipt(
    sender: &mpsc::SyncSender<WorkerReceipt>,
    wake: &WakeSignal,
    receipt: WorkerReceipt,
) -> bool {
    // Wake before a potentially blocking send, then again after admission: the
    // first drain may race ahead of the insertion. No state lock is held here.
    wake.notify();
    let sent = sender.send(receipt).is_ok();
    wake.notify();
    sent
}

fn delivery_outcome(dispatch: &Dispatch) -> DeliveryOutcome {
    match dispatch {
        Dispatch::Executed => DeliveryOutcome::Accepted,
        Dispatch::RejectedKnown => DeliveryOutcome::NotAdmitted,
        Dispatch::OutcomeUnknown => DeliveryOutcome::Unknown,
        Dispatch::Dropped | Dispatch::CapacityExceeded => DeliveryOutcome::Dropped,
    }
}

#[derive(Clone)]
struct PointerRoute {
    session: String,
    generation: String,
    encoder: mouse::Encoder,
    position: mouse::Position,
    wheel: mouse::WheelAccumulator,
}

impl PointerRoute {
    fn index(&self, state: &State) -> Option<usize> {
        state
            .views
            .iter()
            .position(|view| view.session == self.session && view.generation == self.generation)
    }
    fn event(
        &mut self,
        action: PointerAction,
        position: mouse::Position,
        modifiers: mouse::Modifiers,
        modes: u32,
    ) -> Option<Vec<u8>> {
        let mut reported_position = position;
        let bytes = match action {
            PointerAction::Press(button) => self.encoder.press(button, position, modifiers, modes),
            PointerAction::Release(button) => {
                let mut candidate = self.encoder.clone();
                if let Some(bytes) = candidate.release(button, position, modifiers, modes) {
                    self.encoder = candidate;
                    Some(bytes)
                } else {
                    // Legacy encodings cannot represent every host column.
                    // Close a previously reported press at its last encodable
                    // position instead of leaving the CLI's button held.
                    reported_position = self.position;
                    let mut fallback = self.encoder.clone();
                    if let Some(bytes) = fallback.release(button, self.position, modifiers, modes) {
                        self.encoder = fallback;
                        Some(bytes)
                    } else {
                        None
                    }
                }
            }
            PointerAction::Motion => self.encoder.motion(position, modifiers, modes),
        };
        if bytes.is_some() {
            self.position = reported_position;
        }
        bytes
    }
}

#[derive(Clone, Copy)]
enum PointerAction {
    Press(mouse::Button),
    Release(mouse::Button),
    Motion,
}

fn pointer_button(button: MouseButton) -> Option<mouse::Button> {
    match button {
        MouseButton::Left => Some(mouse::Button::Left),
        MouseButton::Middle => Some(mouse::Button::Middle),
        MouseButton::Right => Some(mouse::Button::Right),
        MouseButton::Navigate(_) => None,
    }
}

fn pointer_modifiers(modifiers: Modifiers) -> mouse::Modifiers {
    mouse::Modifiers {
        shift: modifiers.shift,
        alt: modifiers.alt,
        control: modifiers.control,
    }
}

struct CanvasApp {
    focus: FocusHandle,
    selected: usize,
    prefix: bool,
    state: State,
    tx: mpsc::SyncSender<IpcMessage>,
    receipts: mpsc::Receiver<WorkerReceipt>,
    shared: Arc<Mutex<State>>,
    trace: Trace,
    mouse_delivery: MouseDelivery,
    next_request_id: u64,
    panes: Vec<PaneGeometry>,
    text_input: input::TextInput,
    pointer: Option<PointerRoute>,
    selection: Option<selection::CellSelection>,
    selection_dragging: bool,
    clipboard_pending: bool,
    input_epoch: u64,
    cursor_epoch: Instant,
    cursor_phase_visible: bool,
    input_error: Option<String>,
    input_notice: Option<String>,
    resized: ResizeCache,
    _input_subscriptions: Vec<Subscription>,
    diagnostic_preedit_pending: bool,
    diagnostic_selection: bool,
}
impl CanvasApp {
    fn new(
        window: &mut Window,
        cx: &mut Context<Self>,
        connection: WorkerConnection,
        diagnostic_preedit_pending: bool,
        diagnostic_selection: bool,
    ) -> Self {
        let WorkerConnection {
            tx,
            shared,
            receipts,
            wake,
            trace,
        } = connection;
        let ui_trace = trace.clone();
        let focus = cx.focus_handle();
        let input_subscriptions = vec![
            cx.on_blur(&focus, window, |this, window, cx| {
                this.selection_dragging = false;
                this.input_epoch = this.input_epoch.wrapping_add(1);
                this.finish_pointer(cx);
                this.text_input.cancel();
                window.notify_text_input_changed(&this.focus);
                cx.notify();
            }),
            cx.observe_window_activation(window, |this, window, cx| {
                this.reset_cursor_blink(cx);
                cx.notify();
                if !window.is_window_active() {
                    this.selection_dragging = false;
                    this.input_epoch = this.input_epoch.wrapping_add(1);
                    this.finish_pointer(cx);
                    this.text_input.cancel();
                    window.notify_text_input_changed(&this.focus);
                    cx.notify();
                }
            }),
        ];
        cx.spawn(async move |weak, cx| {
            loop {
                if let WaitOutcome::Failed(_) = wake
                    .wait_or_timeout(cx.background_executor().timer(Duration::from_millis(50)))
                    .await
                {
                    // Preserve bounded periodic service if the single-consumer
                    // signal contract fails; never spin or discard receipts.
                    cx.background_executor()
                        .timer(Duration::from_millis(50))
                        .await;
                }
                if weak
                    .update_in(cx, |this, window, cx| {
                        // Drain worker receipts before state/revision work: a
                        // worker blocked on this bounded channel must not wait
                        // for a State revision to change.
                        let mut receipt_drained = false;
                        while let Ok(receipt) = this.receipts.try_recv() {
                            receipt_drained = true;
                            this.apply_worker_receipt(receipt);
                        }
                        if receipt_drained {
                            cx.notify();
                        }
                        let next = {
                            let shared = this.shared.lock().unwrap();
                            state_update(&this.state, &shared)
                        };
                        if let Some(next) = next {
                            this.resized.apply_notices(&next.command_notices);
                            if let Some(views) = next.views.as_ref() {
                                this.resized.retain_targets(views.iter().map(|view| {
                                    CommandTarget {
                                        session: view.session.clone(),
                                        generation: view.generation.clone(),
                                    }
                                }));
                            }
                            if cursor_target_changed(&this.state, this.selected, &next) {
                                this.reset_cursor_blink(cx);
                            }
                            if composition_target_changed(&this.state, this.selected, &next) {
                                this.input_epoch = this.input_epoch.wrapping_add(1);
                                this.text_input.cancel();
                                window.notify_text_input_changed(&this.focus);
                            }
                            let views_changed = next.views.is_some();
                            apply_state_update(&mut this.state, next);
                            trace_publication(&ui_trace, TraceStage::UiApplied, &this.state);
                            let ended_targets = this
                                .state
                                .views
                                .iter()
                                .filter(|view| confirmed_ended_state(&view.state))
                                .map(|view| CommandTarget {
                                    session: view.session.clone(),
                                    generation: view.generation.clone(),
                                })
                                .collect::<Vec<_>>();
                            for target in ended_targets {
                                this.mouse_delivery.remove_ended(&target);
                            }
                            this.reconcile_pointer();
                            if this.selection.as_ref().is_some_and(|selection| {
                                !this
                                    .state
                                    .views
                                    .iter()
                                    .any(|view| selection.is_current(view))
                            }) {
                                this.selection = None;
                                this.selection_dragging = false;
                            }
                            if views_changed {
                                this.selected =
                                    this.selected.min(this.state.views.len().saturating_sub(1));
                            }
                            if this.diagnostic_selection
                                && let Some(view) = this.state.views.get(this.selected)
                            {
                                this.selection = selection::CellSelection::new(
                                    view,
                                    selection::CellPosition {
                                        row: 1.min(
                                            view.frame.geometry.rows.saturating_sub(1) as usize
                                        ),
                                        column: 0,
                                    },
                                    selection::CellPosition {
                                        row: 3.min(
                                            view.frame.geometry.rows.saturating_sub(1) as usize
                                        ),
                                        column: 18.min(
                                            view.frame.geometry.cols.saturating_sub(1) as usize
                                        ),
                                    },
                                )
                                .ok();
                            }
                            if this.diagnostic_preedit_pending && !this.state.views.is_empty() {
                                this.text_input.key(false);
                                this.text_input.mark(None, "日本😀", Some(0..2));
                                this.diagnostic_preedit_pending = false;
                            }
                            cx.notify();
                        }
                        let blinking = this.state.error.is_none()
                            && this.state.views.get(this.selected).is_some_and(|view| {
                                view.state == "running"
                                    && cursor::plan(
                                        &view.frame,
                                        view.frame.geometry.rows,
                                        view.frame.geometry.cols,
                                        this.focus.is_focused(window) && window.is_window_active(),
                                    )
                                    .is_some_and(|plan| plan.blinking)
                            });
                        let visible = cursor::blink_visible(
                            this.cursor_epoch.elapsed(),
                            blinking,
                            cx.reduce_motion(),
                        );
                        if visible != this.cursor_phase_visible {
                            this.cursor_phase_visible = visible;
                            cx.notify();
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
        Self {
            focus,
            selected: 0,
            prefix: false,
            state: State::default(),
            tx,
            receipts,
            shared,
            trace,
            mouse_delivery: MouseDelivery::default(),
            next_request_id: 1,
            panes: Vec::new(),
            text_input: input::TextInput::default(),
            pointer: None,
            selection: None,
            selection_dragging: false,
            clipboard_pending: false,
            input_epoch: 0,
            cursor_epoch: Instant::now(),
            cursor_phase_visible: true,
            input_error: None,
            input_notice: None,
            resized: ResizeCache::default(),
            _input_subscriptions: input_subscriptions,
            diagnostic_preedit_pending,
            diagnostic_selection,
        }
    }
    fn reset_cursor_blink(&mut self, cx: &mut Context<Self>) {
        self.cursor_epoch = Instant::now();
        if !self.cursor_phase_visible {
            self.cursor_phase_visible = true;
            cx.notify();
        }
    }
    fn send(&mut self, bytes: Vec<u8>, cx: &mut Context<Self>) {
        self.send_to(self.selected, bytes, cx);
    }
    fn target_can_send(&self, view: &View) -> bool {
        can_send_to_view(view, &self.state, self.input_error.as_deref())
            && !self.mouse_delivery.awaiting_cleanup(&CommandTarget {
                session: view.session.clone(),
                generation: view.generation.clone(),
            })
    }

    fn queue_command(
        &mut self,
        target: CommandTarget,
        value: Value,
        mouse_after: Option<MouseSnapshot>,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.mouse_delivery.has_capacity() {
            self.input_error = Some("Очередь подтверждений команд заполнена".into());
            cx.notify();
            return false;
        }
        let Some(next) = self.next_request_id.checked_add(1) else {
            self.input_error = Some("Исчерпан счетчик запросов; перезапустите приложение".into());
            cx.notify();
            return false;
        };
        let id = self.next_request_id;
        self.next_request_id = next;
        if !self
            .mouse_delivery
            .stage(id, target.clone(), mouse_after.clone())
        {
            self.input_error = Some("Не удалось зарезервировать подтверждение команды".into());
            cx.notify();
            return false;
        }
        if self
            .tx
            .try_send(IpcMessage::Request {
                id,
                target,
                command: value,
            })
            .is_err()
        {
            self.mouse_delivery.cancel_stage(id);
            self.input_error = Some("Очередь ввода заполнена; ввод не принят".into());
            cx.notify();
            return false;
        }
        true
    }

    /// Admission to the local bounded queue only. The IPC worker never retries
    /// an ambiguous command; a failure freezes only the command's target.
    fn send_to(&mut self, index: usize, bytes: Vec<u8>, cx: &mut Context<Self>) -> bool {
        self.send_to_with_mouse(index, bytes, None, cx)
    }

    fn send_to_with_mouse(
        &mut self,
        index: usize,
        bytes: Vec<u8>,
        mouse_after: Option<MouseSnapshot>,
        cx: &mut Context<Self>,
    ) -> bool {
        if bytes.is_empty() {
            return true;
        }
        if bytes.len() > 16 * 1024 {
            self.input_error = Some("Ввод более 16 KiB не принят; разделите текст".into());
            cx.notify();
            return false;
        }
        let Some(view) = self.state.views.get(index) else {
            return false;
        };
        if !self.target_can_send(view) {
            return false;
        }
        let target = CommandTarget {
            session: view.session.clone(),
            generation: view.generation.clone(),
        };
        let mut value = command(view, "input");
        value["bytes"] = json!(bytes);
        self.queue_command(target, value, mouse_after, cx)
    }

    fn mouse_snapshot(route: &PointerRoute, view: &View) -> MouseSnapshot {
        MouseSnapshot {
            encoder: route.encoder.clone(),
            position: route.position,
            wheel: route.wheel,
            rows: view.frame.geometry.rows,
            cols: view.frame.geometry.cols,
            modes: view.frame.modes,
        }
    }

    fn apply_worker_receipt(&mut self, receipt: WorkerReceipt) {
        match receipt {
            WorkerReceipt::Request(receipt) => {
                if let ReceiptEffect::Rollback {
                    target, confirmed, ..
                } = self.mouse_delivery.request_receipt(receipt)
                    && self.pointer.as_ref().is_some_and(|route| {
                        route.session == target.session && route.generation == target.generation
                    })
                {
                    if let Some(snapshot) = confirmed {
                        if let Some(route) = self.pointer.as_mut() {
                            route.encoder = snapshot.encoder;
                            route.position = snapshot.position;
                            route.wheel = snapshot.wheel;
                        }
                    } else {
                        self.pointer = None;
                    }
                }
            }
            WorkerReceipt::Acknowledgement {
                target,
                token,
                accepted,
                cleanup_id,
            } => self
                .mouse_delivery
                .finish_ack(&target, token, cleanup_id, accepted),
        }
    }

    fn acknowledge_notice(&mut self, target: &CommandTarget, token: u64) -> bool {
        let Some(notice) = self.state.command_notices.iter().find(|notice| {
            notice.target == *target && notice.token == token && notice.acknowledgeable
        }) else {
            return false;
        };
        if !self
            .mouse_delivery
            .settled_through(target, notice.last_request_id)
        {
            return false;
        }
        let mut cleanup = None;
        if self.mouse_delivery.confirmed_pressed(target) {
            let Some(view) = self.state.views.iter().find(|view| {
                view.session == target.session
                    && view.generation == target.generation
                    && view.state == "running"
            }) else {
                self.input_notice = Some(
                    "Подтверждение отложено: нет актуальной геометрии для отпускания кнопки".into(),
                );
                return false;
            };
            let Some(next) = self.next_request_id.checked_add(1) else {
                self.input_error =
                    Some("Исчерпан счетчик запросов; перезапустите приложение".into());
                return false;
            };
            let id = self.next_request_id;
            self.next_request_id = next;
            let plan = match self.mouse_delivery.prepare_cleanup(
                target,
                id,
                view.frame.geometry.rows,
                view.frame.geometry.cols,
                view.frame.modes,
            ) {
                Ok(Some(plan)) => plan,
                Ok(None) => unreachable!("confirmed press must produce a cleanup plan"),
                Err(()) => {
                    self.input_notice = Some(
                        "Подтверждение отложено: текущий режим не кодирует отпускание кнопки"
                            .into(),
                    );
                    return false;
                }
            };
            if !self.mouse_delivery.reserve_cleanup(
                target.clone(),
                token,
                id,
                plan.mouse_after.clone(),
            ) {
                return false;
            }
            cleanup = Some(plan);
        }
        if self
            .tx
            .try_send(IpcMessage::AcknowledgeKnownRejection {
                target: target.clone(),
                token,
                cleanup: cleanup.clone(),
            })
            .is_err()
        {
            if let Some(cleanup) = cleanup {
                self.mouse_delivery.cancel_cleanup(target, cleanup.id);
            }
            self.input_error = Some("Очередь команд заполнена; подтверждение не принято".into());
            return false;
        }
        true
    }
    fn pointer_index(&self, route: &PointerRoute) -> Option<usize> {
        route
            .index(&self.state)
            .filter(|&index| self.target_can_send(&self.state.views[index]))
    }
    fn reconcile_pointer(&mut self) {
        // A stopped/replaced generation must never receive an old drag release.
        if self.pointer.as_ref().is_some_and(|route| {
            !self.state.views.iter().any(|view| {
                view.session == route.session
                    && view.generation == route.generation
                    && view.state == "running"
                    && view.storage_error.is_none()
            })
        }) {
            self.pointer = None;
        }
    }
    fn finish_pointer(&mut self, cx: &mut Context<Self>) {
        let Some(mut route) = self.pointer.clone() else {
            return;
        };
        if !route.encoder.has_pressed() {
            route.wheel.reset();
            self.pointer = None;
            return;
        }
        let Some(index) = self.pointer_index(&route) else {
            return;
        };
        let modes = self.state.views[index].frame.modes;
        let bytes = route
            .encoder
            .release_all(route.position, mouse::Modifiers::default(), modes);
        let snapshot = Self::mouse_snapshot(&route, &self.state.views[index]);
        match bytes {
            Some(bytes) => {
                if self.send_to_with_mouse(index, bytes, Some(snapshot), cx) {
                    self.pointer = None;
                }
            }
            None => {
                self.input_notice = Some(
                    "Кнопка мыши не отпущена: текущий режим не кодирует событие release".into(),
                );
                cx.notify();
            }
        }
    }
    fn pointer_hit(&self, point: Point<Pixels>) -> Option<(usize, mouse::Position)> {
        self.panes
            .iter()
            .zip(&self.state.views)
            .enumerate()
            .find_map(|(index, (pane, view))| {
                pane.pointer_position(point, &view.frame.geometry, false)
                    .map(|position| (index, position))
            })
    }
    fn pointer_event(
        &mut self,
        action: PointerAction,
        point: Point<Pixels>,
        modifiers: Modifiers,
        cx: &mut Context<Self>,
    ) {
        if self.prefix {
            return;
        }
        self.reconcile_pointer();
        let captured = self
            .pointer
            .as_ref()
            .filter(|route| route.encoder.has_pressed());
        let hit = if let Some(route) = captured {
            self.pointer_index(route).and_then(|index| {
                self.panes
                    .get(index)?
                    .pointer_position(point, &self.state.views[index].frame.geometry, true)
                    .map(|position| (index, position))
            })
        } else {
            self.pointer_hit(point)
        };
        let Some((index, position)) = hit else { return };
        let view = &self.state.views[index];
        if !self.target_can_send(view) {
            return;
        }
        let mut route = self
            .pointer
            .clone()
            .filter(|route| route.session == view.session && route.generation == view.generation)
            .unwrap_or_else(|| PointerRoute {
                session: view.session.clone(),
                generation: view.generation.clone(),
                encoder: mouse::Encoder::default(),
                position,
                wheel: mouse::WheelAccumulator::default(),
            });
        let modes = view.frame.modes;
        let modifiers = pointer_modifiers(modifiers);
        let bytes = route.event(action, position, modifiers, modes);
        // Rejected local admission cannot consume a button transition. A single
        // queued command binds bytes and state to the same process generation.
        let snapshot = Self::mouse_snapshot(&route, &self.state.views[index]);
        if bytes.is_none_or(|bytes| self.send_to_with_mouse(index, bytes, Some(snapshot), cx)) {
            self.pointer = Some(route);
        }
    }
    fn extend_selection(&mut self, point: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(selection) = self.selection.as_mut() else {
            return;
        };
        let Some((pane, view)) = self
            .panes
            .iter()
            .zip(&self.state.views)
            .find(|(_, view)| selection.is_current(view))
        else {
            self.selection_dragging = false;
            self.selection = None;
            return;
        };
        if let Some(position) = pane.pointer_position(point, &view.frame.geometry, true) {
            let _ = selection.set_focus(
                view,
                selection::CellPosition {
                    row: position.row,
                    column: position.column,
                },
            );
            cx.notify();
        }
    }
    fn copy_selection(&mut self, cx: &mut Context<Self>) {
        if self.clipboard_pending {
            self.input_epoch = self.input_epoch.wrapping_add(1);
        }
        let Some(selection) = &self.selection else {
            self.input_notice = Some("Нет выделения для копирования".into());
            cx.notify();
            return;
        };
        let Some(view) = self
            .state
            .views
            .iter()
            .find(|view| selection.is_current(view))
        else {
            return;
        };
        if let Ok(text) = selection.text(view) {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
            self.input_notice = Some("Выделение скопировано".into());
            cx.notify();
        }
    }
    fn select_visible(&mut self, cx: &mut Context<Self>) {
        if let Some(view) = self.state.views.get(self.selected) {
            self.selection = selection::CellSelection::new(
                view,
                selection::CellPosition { row: 0, column: 0 },
                selection::CellPosition {
                    row: view.frame.geometry.rows.saturating_sub(1) as usize,
                    column: view.frame.geometry.cols.saturating_sub(1) as usize,
                },
            )
            .ok();
            self.selection_dragging = false;
            cx.notify();
        }
    }
    fn pointer_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(index) = self
            .panes
            .iter()
            .position(|pane| pane.bounds.contains(&event.position))
        {
            self.reset_cursor_blink(cx);
            self.input_epoch = self.input_epoch.wrapping_add(1);
            if self.selected != index
                && !self
                    .pointer
                    .as_ref()
                    .is_some_and(|route| route.encoder.has_pressed())
            {
                self.selected = index;
                self.selection = None;
                self.selection_dragging = false;
                self.text_input.cancel();
                window.notify_text_input_changed(&self.focus);
            }
            self.focus.focus(window, cx);
            if event.button == MouseButton::Left
                && !self.prefix
                && !self
                    .pointer
                    .as_ref()
                    .is_some_and(|route| route.encoder.has_pressed())
                && let Some((hit, position)) = self.pointer_hit(event.position)
                && hit == index
            {
                let view = &self.state.views[index];
                if event.modifiers.shift
                    || view.frame.modes
                        & (mouse::MOUSE_REPORT_CLICK | mouse::MOUSE_DRAG | mouse::MOUSE_MOTION)
                        == 0
                {
                    let position = selection::CellPosition {
                        row: position.row,
                        column: position.column,
                    };
                    self.selection = selection::CellSelection::new(view, position, position).ok();
                    self.selection_dragging = self.selection.is_some();
                    self.text_input.cancel();
                    window.notify_text_input_changed(&self.focus);
                    cx.notify();
                    return;
                }
            }
            self.selection = None;
            self.selection_dragging = false;
            if let Some(button) = pointer_button(event.button) {
                self.pointer_event(
                    PointerAction::Press(button),
                    event.position,
                    event.modifiers,
                    cx,
                );
            }
            cx.notify();
        }
    }
    fn pointer_up(&mut self, event: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        if event.button == MouseButton::Left && self.selection_dragging {
            self.extend_selection(event.position, cx);
            self.selection_dragging = false;
            return;
        }
        if let Some(button) = pointer_button(event.button) {
            self.pointer_event(
                PointerAction::Release(button),
                event.position,
                event.modifiers,
                cx,
            );
        }
    }
    fn pointer_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.selection_dragging {
            if event.pressed_button == Some(MouseButton::Left) {
                self.extend_selection(event.position, cx);
            } else {
                self.selection_dragging = false;
            }
            return;
        }
        if event.pressed_button.is_none()
            && self
                .pointer
                .as_ref()
                .is_some_and(|route| route.encoder.has_pressed())
        {
            // Recover a release delivered outside this window without inventing
            // a press in the newly hovered pane.
            self.finish_pointer(cx);
        }
        self.pointer_event(PointerAction::Motion, event.position, event.modifiers, cx);
    }
    fn pointer_wheel(&mut self, event: &ScrollWheelEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.prefix || event.modifiers.shift {
            return;
        }
        let Some((index, position)) = self.pointer_hit(event.position) else {
            return;
        };
        let view = &self.state.views[index];
        if !self.target_can_send(view) {
            return;
        }
        if self.pointer.as_ref().is_some_and(|route| {
            route.encoder.has_pressed()
                && (route.session != view.session || route.generation != view.generation)
        }) {
            return;
        }
        let mut route = self
            .pointer
            .clone()
            .filter(|route| route.session == view.session && route.generation == view.generation)
            .unwrap_or_else(|| PointerRoute {
                session: view.session.clone(),
                generation: view.generation.clone(),
                encoder: mouse::Encoder::default(),
                position,
                wheel: mouse::WheelAccumulator::default(),
            });
        let lines = match event.delta {
            ScrollDelta::Lines(delta) => delta.y,
            ScrollDelta::Pixels(delta) => {
                f32::from(delta.y) / f32::from(self.panes[index].cell.height)
            }
        };
        let modes = view.frame.modes;
        if modes & (mouse::MOUSE_REPORT_CLICK | mouse::MOUSE_DRAG | mouse::MOUSE_MOTION) == 0 {
            return;
        }
        let modifiers = pointer_modifiers(event.modifiers);
        let mut bytes = Vec::new();
        for step in route.wheel.take_steps(lines) {
            if let Some(report) = route.encoder.wheel(step, position, modifiers, modes) {
                bytes.extend(report);
            }
        }
        if !bytes.is_empty() {
            route.position = position;
        }
        let snapshot = (!bytes.is_empty()).then(|| Self::mouse_snapshot(&route, view));
        if self.send_to_with_mouse(index, bytes, snapshot, cx) {
            self.pointer = Some(route);
            cx.stop_propagation();
        }
    }
    fn paste_text(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.reset_cursor_blink(cx);
        self.text_input.cancel();
        window.notify_text_input_changed(&self.focus);
        let modes = self
            .state
            .views
            .get(self.selected)
            .map_or(0, |view| view.frame.modes);
        match input::paste(text, modes) {
            Ok(bytes) => self.send(bytes, cx),
            Err(reason) => self.input_notice = Some(reason.into()),
        }
        cx.notify();
    }
    fn request_paste(&mut self, cx: &mut Context<Self>) {
        if self.clipboard_pending {
            return;
        }
        let Some(view) = self.state.views.get(self.selected) else {
            return;
        };
        if !self.target_can_send(view) {
            return;
        }
        let target = (view.session.clone(), view.generation.clone());
        let epoch = self.input_epoch;
        self.clipboard_pending = true;
        self.input_notice = None;
        let clipboard = cx.read_from_clipboard_async();
        cx.spawn(async move |weak, cx| {
            let result = clipboard.await;
            let _ = weak.update_in(cx, |this, window, cx| {
                this.clipboard_pending = false;
                if epoch != this.input_epoch
                    || !this.focus.is_focused(window)
                    || !window.is_window_active()
                    || !this.state.views.get(this.selected).is_some_and(|view| {
                        (&view.session, &view.generation) == (&target.0, &target.1)
                    })
                {
                    this.input_notice = Some("Вставка отменена: изменился фокус или панель".into());
                    cx.notify();
                    return;
                }
                match result {
                    Ok(Some(item)) => {
                        if let Some(text) = item.text() {
                            this.paste_text(&text, window, cx);
                        }
                    }
                    Ok(None) => (),
                    Err(_) => {
                        this.input_notice =
                            Some("Не удалось прочитать буфер обмена; вставка не выполнена".into());
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }
    fn error(&self) -> Option<String> {
        let notice = self
            .state
            .views
            .get(self.selected)
            .and_then(|view| self.state.notice_for(view));
        let command_error = notice.map(|notice| {
            let message = notice.display_message();
            match self.state.error.as_deref() {
                Some(error) if error != notice.message => format!("{error} · {message}"),
                _ => message,
            }
        });
        command_error
            .or_else(|| self.state.error.clone())
            .or_else(|| self.input_error.clone())
    }

    fn selected_notice(&self) -> Option<&CommandNotice> {
        self.state
            .views
            .get(self.selected)
            .and_then(|view| self.state.notice_for(view))
    }

    fn acknowledge_selected_rejection(&mut self) -> bool {
        let Some(notice) = self.selected_notice() else {
            return false;
        };
        if !notice.acknowledgeable
            || !self
                .mouse_delivery
                .settled_through(&notice.target, notice.last_request_id)
        {
            return false;
        }
        let target = notice.target.clone();
        let token = notice.token;
        self.acknowledge_notice(&target, token)
    }

    fn key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.reset_cursor_blink(cx);
        self.input_notice = None;
        if self.clipboard_pending {
            self.input_epoch = self.input_epoch.wrapping_add(1);
        }
        let k = &event.keystroke;
        if k.modifiers.control
            && k.modifiers.shift
            && !k.modifiers.alt
            && !k.modifiers.platform
            && k.key == "space"
        {
            self.finish_pointer(cx);
            self.selection_dragging = false;
            self.prefix = !self.prefix;
            self.text_input.cancel();
            window.notify_text_input_changed(&self.focus);
            cx.notify();
            cx.stop_propagation();
            return;
        }
        if self.prefix {
            self.prefix = false;
            if k.key == "c" {
                self.copy_selection(cx);
            }
            if k.key == "a" {
                self.select_visible(cx);
            }
            if k.key == "v" {
                self.request_paste(cx);
            }
            if let Ok(index) = k.key.parse::<usize>()
                && (1..=self.state.views.len()).contains(&index)
            {
                self.selected = index - 1;
            }
            // Only this explicit acknowledgement clears a local rejected-input error.
            // It never resends a failed or unknown command.
            if k.key == "escape" {
                let acknowledged =
                    self.selected_notice().is_none() || self.acknowledge_selected_rejection();
                if acknowledged {
                    self.input_error = None;
                }
                // A rejected local release never reached the worker. Close its
                // remembered press before accepting a fresh click. An unknown
                // IPC outcome stays blocked by state.error and is not retried.
                self.finish_pointer(cx);
                self.selection = None;
                self.selection_dragging = false;
            }
            self.text_input.cancel();
            window.notify_text_input_changed(&self.focus);
            cx.notify();
            cx.stop_propagation();
            return;
        }
        // An active composition owns ordinary editing/navigation until commit.
        if !self.text_input.preedit.is_empty() && !k.modifiers.control && !k.modifiers.alt {
            return;
        }
        let modes = self
            .state
            .views
            .get(self.selected)
            .map_or(0, |v| v.frame.modes);
        let encoded = input::encode(
            input::Key {
                name: &k.key,
                text: k.key_char.as_deref(),
                modifiers: input::Modifiers {
                    control: k.modifiers.control,
                    alt: k.modifiers.alt,
                    shift: k.modifiers.shift,
                    platform: k.modifiers.platform,
                },
            },
            modes,
        );
        match encoded {
            input::Encoded::Bytes(bytes) => {
                self.text_input.cancel();
                window.notify_text_input_changed(&self.focus);
                self.send(bytes, cx);
                cx.stop_propagation();
            }
            input::Encoded::PlatformText { meta } => self.text_input.key(meta),
            input::Encoded::Unsupported(reason) => {
                self.text_input.cancel();
                window.notify_text_input_changed(&self.focus);
                self.input_error = Some(reason.into());
                cx.notify();
                cx.stop_propagation();
            }
            input::Encoded::Ignore => {
                cx.stop_propagation();
            }
        }
    }
    fn geometry(
        &mut self,
        bounds: Bounds<Pixels>,
        cell: Size<Pixels>,
        scale: f32,
        cx: &mut Context<Self>,
    ) -> Vec<PaneGeometry> {
        self.panes = project_panels(bounds, self.state.views.len(), scale)
            .into_iter()
            .map(|bounds| PaneGeometry::new(bounds, cell, scale))
            .collect();
        let resizes = self
            .state
            .views
            .iter()
            .zip(&self.panes)
            .filter_map(|(view, pane)| {
                let (rows, cols) = (pane.rows, pane.cols);
                let resize = ResizeKey {
                    target: CommandTarget {
                        session: view.session.clone(),
                        generation: view.generation.clone(),
                    },
                    rows,
                    cols,
                };
                if self.state.error.is_none()
                    && self.input_error.is_none()
                    && view.state == "running"
                    && self.state.notice_for(view).is_none()
                    && !self.mouse_delivery.awaiting_cleanup(&resize.target)
                    && !self.resized.contains(&resize)
                {
                    let mut value = command(view, "resize");
                    value["geometry"] = json!({"rows":rows,"cols":cols});
                    Some((resize, value))
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        for (resize, value) in resizes {
            if self.queue_command(resize.target.clone(), value, None, cx) {
                self.resized.remember(resize);
            }
        }
        self.panes.clone()
    }
}

/// All coordinates, cell advances and viewport rounding share device pixels.
/// Font size itself is unchanged: no scale transform is applied to terminal text.
#[derive(Clone, Copy, Debug)]
struct PaneGeometry {
    bounds: Bounds<Pixels>,
    origin: Point<Pixels>,
    cell: Size<Pixels>,
    rows: u16,
    cols: u16,
}
fn valid_scale(scale: f32) -> f32 {
    if scale.is_finite() && scale > 0. {
        scale
    } else {
        1.
    }
}
impl PaneGeometry {
    fn new(bounds: Bounds<Pixels>, metrics: Size<Pixels>, scale: f32) -> Self {
        let scale = valid_scale(scale);
        let cw = (f32::from(metrics.width) * scale).ceil().max(1.);
        let ch = (f32::from(metrics.height) * scale).ceil().max(1.);
        let x = ((f32::from(bounds.left()) + 8.) * scale).ceil();
        let y = ((f32::from(bounds.top()) + HEADER + 8.) * scale).ceil();
        let right = ((f32::from(bounds.right()) - 8.) * scale).floor();
        let bottom = ((f32::from(bounds.bottom()) - 8.) * scale).floor();
        Self {
            bounds,
            origin: point(px(x / scale), px(y / scale)),
            cell: size(px(cw / scale), px(ch / scale)),
            rows: ((bottom - y).max(0.) / ch).floor().clamp(1., 128.) as u16,
            cols: ((right - x).max(0.) / cw).floor().clamp(2., 256.) as u16,
        }
    }
    /// Hit tests only visible, acknowledged cells. During resize the pending
    /// pane dimensions cannot invent coordinates outside the last host frame.
    fn pointer_position(
        self,
        point: Point<Pixels>,
        geometry: &protocol::Geometry,
        clamp: bool,
    ) -> Option<mouse::Position> {
        let rows = self.rows.min(geometry.rows) as usize;
        let cols = self.cols.min(geometry.cols) as usize;
        let x = f32::from(point.x - self.origin.x) / f32::from(self.cell.width);
        let y = f32::from(point.y - self.origin.y) / f32::from(self.cell.height);
        if rows == 0 || cols == 0 || !x.is_finite() || !y.is_finite() {
            return None;
        }
        if !clamp && (x < 0. || y < 0. || x >= cols as f32 || y >= rows as f32) {
            return None;
        }
        Some(mouse::Position {
            row: y.floor().clamp(0., (rows - 1) as f32) as usize,
            column: x.floor().clamp(0., (cols - 1) as f32) as usize,
        })
    }
}
fn project_panels(bounds: Bounds<Pixels>, count: usize, scale: f32) -> Vec<Bounds<Pixels>> {
    let count = count.clamp(1, 4);
    let scale = valid_scale(scale);
    let x = (f32::from(bounds.left()) * scale).ceil();
    let y = (f32::from(bounds.top()) * scale).ceil();
    let right = (f32::from(bounds.right()) * scale).floor().max(x);
    let bottom = (f32::from(bounds.bottom()) * scale).floor().max(y);
    let width = right - x;
    let height = bottom - y;
    let gap = (GAP * scale).ceil();
    let split_w = ((width - gap).max(0.) / 2.).floor();
    let split_h = ((height - gap).max(0.) / 2.).floor();
    (0..count)
        .map(|index| {
            let columns = if count == 1 || (count == 3 && index == 2) {
                1
            } else {
                2
            };
            let rows = if count <= 2 { 1 } else { 2 };
            let left = if columns == 2 && index % 2 == 1 {
                x + split_w + gap
            } else {
                x
            };
            let top = if rows == 2 && index >= 2 {
                y + split_h + gap
            } else {
                y
            };
            let pane_right = if columns == 2 && index % 2 == 0 {
                x + split_w
            } else {
                right
            };
            let pane_bottom = if rows == 2 && index < 2 {
                y + split_h
            } else {
                bottom
            };
            Bounds::new(
                point(px(left / scale), px(top / scale)),
                size(
                    px((pane_right - left).max(0.) / scale),
                    px((pane_bottom - top).max(0.) / scale),
                ),
            )
        })
        .collect()
}
impl EntityInputHandler for CanvasApp {
    fn accepts_text_input(&self, _: &mut Window, _: &mut Context<Self>) -> bool {
        !self.prefix
            && self.state.views.get(self.selected).is_some_and(|view| {
                self.target_can_send(view) && input::protocol_error(view.frame.modes).is_none()
            })
    }
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let (text, actual) = self.text_input.text_for_range(range);
        *actual_range = Some(actual);
        Some(text)
    }
    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.text_input.selection(),
            reversed: false,
        })
    }
    fn set_selected_text_range(
        &mut self,
        range: Range<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.text_input.select(range);
        window.notify_text_selection_changed(&self.focus);
        cx.notify();
    }
    fn text_length_utf16(&mut self, _: &mut Window, _: &mut Context<Self>) -> Option<usize> {
        Some(self.text_input.len_utf16())
    }
    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.text_input.marked()
    }
    fn unmark_text(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.text_input.cancel();
        window.notify_text_input_changed(&self.focus);
        cx.notify();
    }
    fn replace_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.reset_cursor_blink(cx);
        let modes = self
            .state
            .views
            .get(self.selected)
            .map_or(0, |v| v.frame.modes);
        if let Some(error) = input::protocol_error(modes) {
            self.text_input.cancel();
            self.input_error = Some(error.into());
        } else {
            let bytes = self.text_input.commit(range, text);
            self.send(bytes, cx);
        }
        window.notify_text_input_changed(&self.focus);
        cx.notify();
    }
    fn paste(&mut self, item: ClipboardItem, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = item.text() {
            self.paste_text(&text, window, cx);
        }
    }
    fn replace_and_mark_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        selection: Option<Range<usize>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.reset_cursor_blink(cx);
        self.text_input.mark(range, text, selection);
        window.notify_text_input_changed(&self.focus);
        cx.notify();
    }
    fn bounds_for_range(
        &mut self,
        range: Range<usize>,
        _: Bounds<Pixels>,
        window: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let pane = self.panes.get(self.selected)?;
        let view = self.state.views.get(self.selected)?;
        let area = preedit_area(*pane, view)?;
        if self.text_input.preedit.is_empty() {
            return Some(Bounds::new(area.origin, pane.cell));
        }
        let line = preedit_line(
            &self.text_input.preedit,
            &crate::font::terminal_font(),
            window,
        );
        let bytes = self.text_input.range_bytes(range);
        let left = line.x_for_index(bytes.start).min(area.size.width);
        let right = line.x_for_index(bytes.end).max(left).min(area.size.width);
        Some(Bounds::new(
            area.origin + point(left, px(0.)),
            size((right - left).max(px(1.)), pane.cell.height),
        ))
    }
    fn character_index_for_point(
        &mut self,
        _: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        None
    }
}
impl Render for CanvasApp {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity();
        let painter = entity.clone();
        let trace = self.trace.clone();
        let paint_revision = self.state.revision;
        let views = self.state.views.clone();
        let preedit = self.text_input.preedit.clone();
        let preedit_selection = self.text_input.selection_bytes();
        let selected = self.selected;
        let cursor_visible = self.cursor_phase_visible;
        let selection = self.selection.clone();
        let attached = self.state.error.is_none();
        let terminal_label = self.state.views.get(selected).map_or_else(
            || "Терминал: нет выбранной сессии".to_owned(),
            |view| format!("Терминал {} · {}", view.frame.title, view.state),
        );
        let status = self.error()
            .or_else(|| self.state.poll_backpressured.then(|| "Обновление терминала задержано".into()))
            .or_else(|| self.input_notice.clone()).unwrap_or_else(|| {
            if self.prefix {
                "Режим PigADE: 1–4 — фокус · A/C/V — выделить/копировать/вставить · Escape — выход"
                    .into()
            } else {
                format!(
                    "PigADE 0.0.1-dev · {} сессий · Ctrl+Shift+Space, 1–4 — фокус",
                    views.len()
                )
            }
        });
        let recovery_warning = recovery_warning(
            self.state.recovery.as_ref(),
            self.state
                .views
                .get(selected)
                .and_then(|view| view.storage_error.as_deref()),
        );
        div()
            .id("pigade-root")
            .accessibility_id("pigade-root")
            .role(Role::Application)
            .aria_label("PigADE")
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(0x17191d))
            .text_color(rgb(0xf2f4f7))
            .font_family("Inter")
            .text_size(px(13.))
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::key))
            .child(
                div()
                    .id("app-heading")
                    .role(Role::Heading)
                    .aria_level(1)
                    .aria_label("PigADE")
                    .h(px(40.))
                    .px(px(12.))
                    .flex()
                    .items_center()
                    .child("PigADE"),
            )
            .child(
                div()
                    .id("terminal-canvas")
                    .accessibility_id("terminal-canvas")
                    .role(Role::Terminal)
                    .aria_label(terminal_label)
                    .flex_1()
                    .min_h_0()
                    .child(
                        canvas(
                            move |bounds, window, cx| {
                                let font = crate::font::terminal_font();
                                let fontid = window.text_system().resolve_font(&font);
                                let width = window
                                    .text_system()
                                    .advance(fontid, px(crate::font::SIZE), 'M')
                                    .map(|s| s.width)
                                    .unwrap_or(px(9.));
                                let height = terminal_line_height(
                                    window.text_system().ascent(fontid, px(crate::font::SIZE)),
                                    window.text_system().descent(fontid, px(crate::font::SIZE)),
                                );
                                let cell = size(width, height);
                                let scale = window.scale_factor();
                                let panes = entity
                                    .update(cx, |this, cx| this.geometry(bounds, cell, scale, cx));
                                (font, panes)
                            },
                            move |bounds, (font, panes), window, cx| {
                                let mut sequences = [0; 4];
                                let count = views.len().min(sequences.len());
                                for (sequence, view) in sequences.iter_mut().zip(&views) {
                                    *sequence = view.sequence;
                                }
                                trace.record(
                                    TraceStage::Paint,
                                    paint_revision,
                                    &sequences[..count],
                                );
                                let focus = painter.read(cx).focus.clone();
                                window.handle_input(
                                    &focus,
                                    ElementInputHandler::new(bounds, painter.clone()),
                                    cx,
                                );
                                window.with_content_mask(Some(ContentMask { bounds }), |window| {
                                    for (index, pane) in panes.into_iter().enumerate() {
                                        paint_panel(
                                            pane,
                                            views.get(index),
                                            PanelState {
                                                selected: index == selected,
                                                attached,
                                                focused: index == selected
                                                    && attached
                                                    && views.get(index).is_some_and(|view| {
                                                        view.state == "running"
                                                    })
                                                    && focus.is_focused(window)
                                                    && window.is_window_active(),
                                                cursor_visible,
                                            },
                                            selection.as_ref(),
                                            &font,
                                            window,
                                            cx,
                                        );
                                        if index == selected && !preedit.is_empty() {
                                            paint_preedit(
                                                pane,
                                                views.get(index),
                                                &preedit,
                                                preedit_selection.clone(),
                                                &font,
                                                window,
                                                cx,
                                            );
                                        }
                                    }
                                });
                            },
                        )
                        .size_full(),
                    ),
            )
            .child({
                let mut footer = div().flex().flex_col();
                if let Some(warning) = recovery_warning {
                    footer = footer.child(
                        div()
                            .id("recovery-warning")
                            .accessibility_id("recovery-warning")
                            .role(Role::Alert)
                            .aria_label(warning.clone())
                            .h(px(22.))
                            .px(px(12.))
                            .flex()
                            .items_center()
                            .text_color(rgb(0xffd18a))
                            .child(warning),
                    );
                }
                let acknowledgement = self
                    .selected_notice()
                    .filter(|notice| {
                        notice.acknowledgeable
                            && self
                                .mouse_delivery
                                .settled_through(&notice.target, notice.last_request_id)
                    })
                    .map(|notice| (notice.target.clone(), notice.token));
                let mut status_row = div()
                    .id("session-status")
                    .role(Role::Status)
                    .aria_label(status.clone())
                    .h(px(24.))
                    .px(px(12.))
                    .flex()
                    .items_center()
                    .text_color(rgb(0xb0b8c5))
                    .child(div().flex_1().child(status));
                if let Some((target, token)) = acknowledgement {
                    let needs_release = self.mouse_delivery.confirmed_pressed(&target);
                    let label = if needs_release {
                        "Подтвердить и отпустить кнопку мыши"
                    } else {
                        "Подтвердить"
                    };
                    status_row = status_row.child(
                        div()
                            .id("acknowledge-command-rejection")
                            .role(Role::Button)
                            .aria_label(label)
                            .px(px(8.))
                            .child(label)
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _, _, cx| {
                                    if this.acknowledge_notice(&target, token) {
                                        this.input_error = None;
                                    }
                                    cx.notify();
                                    cx.stop_propagation();
                                }),
                            ),
                    );
                }
                footer.child(
                    status_row
                        .child(
                            div()
                                .id("copy-selection")
                                .role(Role::Button)
                                .aria_label("Копировать выделение")
                                .px(px(8.))
                                .child("Копировать")
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, _, _, cx| {
                                        this.copy_selection(cx);
                                        cx.stop_propagation();
                                    }),
                                ),
                        )
                        .child(
                            div()
                                .id("paste-clipboard")
                                .role(Role::Button)
                                .aria_label("Вставить из буфера обмена")
                                .px(px(8.))
                                .child("Вставить")
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, _, _, cx| {
                                        this.request_paste(cx);
                                        cx.stop_propagation();
                                    }),
                                ),
                        ),
                )
            })
            .on_mouse_down(MouseButton::Left, cx.listener(Self::pointer_down))
            .on_mouse_down(MouseButton::Middle, cx.listener(Self::pointer_down))
            .on_mouse_down(MouseButton::Right, cx.listener(Self::pointer_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::pointer_up))
            .on_mouse_up(MouseButton::Middle, cx.listener(Self::pointer_up))
            .on_mouse_up(MouseButton::Right, cx.listener(Self::pointer_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::pointer_up))
            .on_mouse_up_out(MouseButton::Middle, cx.listener(Self::pointer_up))
            .on_mouse_up_out(MouseButton::Right, cx.listener(Self::pointer_up))
            .on_mouse_move(cx.listener(Self::pointer_move))
            .on_scroll_wheel(cx.listener(Self::pointer_wheel))
    }
}
/// Font metric descent is negative on GPUI Linux and positive on some
/// other backends. ShapedLine::paint uses a positive extent below its baseline.
/// Keep the full ascent/descent extent plus padding before device rounding.
fn terminal_line_height(ascent: Pixels, descent: Pixels) -> Pixels {
    px(f32::from(ascent).max(0.) + f32::from(descent).abs() + 4.)
}

#[derive(Clone, Copy)]
enum CellPaint<'a> {
    Background(usize, &'a Cell),
    Foreground(usize, &'a Cell),
}
fn cell_paint_order(cells: &[Cell]) -> impl Iterator<Item = CellPaint<'_>> {
    cells
        .iter()
        .enumerate()
        .map(|(i, c)| CellPaint::Background(i, c))
        .chain(
            cells
                .iter()
                .enumerate()
                .filter(|(_, c)| cell_style::needs_foreground(c))
                .map(|(i, c)| CellPaint::Foreground(i, c)),
        )
}

/// Alacritty marks the leading cell of a width-two glyph with WIDE_CHAR.
/// Continuation cells are not separate grapheme origins.
fn glyph_span_cells(flags: u16) -> usize {
    if flags & (1 << 5) != 0 { 2 } else { 1 }
}

fn glyph_span_bounds(origin: Point<Pixels>, cell: Size<Pixels>, flags: u16) -> Bounds<Pixels> {
    Bounds::new(
        origin,
        size(cell.width * glyph_span_cells(flags) as f32, cell.height),
    )
}

fn text(
    text: String,
    origin: Point<Pixels>,
    font: &Font,
    color: Hsla,
    line_height: Pixels,
    window: &mut Window,
    cx: &mut App,
) {
    let len = text.len();
    if len == 0 {
        return;
    }
    let line = window.text_system().shape_line(
        text.into(),
        px(crate::font::SIZE),
        &[TextRun {
            len,
            font: font.clone(),
            color,
            background_color: None,
            underline: None,
            strikethrough: None,
        }],
        None,
    );
    let _ = line.paint(origin, line_height, TextAlign::Left, None, window, cx);
}
fn preedit_area(pane: PaneGeometry, view: &View) -> Option<Bounds<Pixels>> {
    let (row, col) = view.frame.cursor;
    if row < 0 || row as u16 >= pane.rows || col >= pane.cols as usize {
        return None;
    }
    Some(Bounds::new(
        pane.origin + point(pane.cell.width * col as f32, pane.cell.height * row as f32),
        size(
            pane.cell.width * (pane.cols as usize - col) as f32,
            pane.cell.height,
        ),
    ))
}

fn preedit_line(text: &str, font: &Font, window: &Window) -> ShapedLine {
    window.text_system().shape_line(
        text.to_owned().into(),
        px(crate::font::SIZE),
        &[TextRun {
            len: text.len(),
            font: font.clone(),
            color: rgb(0xf2f4f7).into(),
            background_color: None,
            underline: None,
            strikethrough: None,
        }],
        None,
    )
}

fn paint_preedit(
    pane: PaneGeometry,
    view: Option<&View>,
    preedit: &str,
    selection: Range<usize>,
    font: &Font,
    window: &mut Window,
    cx: &mut App,
) {
    let Some(area) = view.and_then(|view| preedit_area(pane, view)) else {
        return;
    };
    let line = preedit_line(preedit, font, window);
    // This is undelivered IME text. It overlays the acknowledged cursor only;
    // terminal cells and PTY geometry never change during composition.
    window.with_content_mask(Some(ContentMask { bounds: area }), |window| {
        let width = line.width().max(pane.cell.width).min(area.size.width);
        window.paint_quad(fill(
            Bounds::new(area.origin, size(width, area.size.height)),
            rgb(0x252d39),
        ));
        let start = line.x_for_index(selection.start);
        let end = line.x_for_index(selection.end);
        if end > start {
            window.paint_quad(fill(
                Bounds::new(
                    area.origin + point(start, px(0.)),
                    size(end - start, area.size.height),
                ),
                rgb(0x354c70),
            ));
        }
        let _ = line.paint(
            area.origin,
            pane.cell.height,
            TextAlign::Left,
            None,
            window,
            cx,
        );
        window.paint_quad(fill(
            Bounds::new(
                area.origin + point(px(0.), pane.cell.height - px(1.)),
                size(width, px(1.)),
            ),
            rgb(0x7dafff),
        ));
        window.paint_quad(fill(
            Bounds::new(
                area.origin + point(end, px(0.)),
                size(px(1.), pane.cell.height),
            ),
            rgb(0x7dafff),
        ));
    });
}

#[derive(Clone, Copy)]
struct PanelState {
    selected: bool,
    attached: bool,
    focused: bool,
    cursor_visible: bool,
}

fn paint_panel(
    pane: PaneGeometry,
    view: Option<&View>,
    PanelState {
        selected,
        attached,
        focused,
        cursor_visible,
    }: PanelState,
    selection: Option<&selection::CellSelection>,
    font: &Font,
    window: &mut Window,
    cx: &mut App,
) {
    let bounds = pane.bounds;
    let cell = pane.cell;
    window.paint_quad(fill(
        bounds,
        rgb(if selected { 0x7dafff } else { 0x373e49 }),
    ));
    let inner = Bounds::new(
        bounds.origin + point(px(1.), px(1.)),
        bounds.size - size(px(2.), px(2.)),
    );
    window.paint_quad(fill(inner, rgb(0x111317)));
    let title = view
        .map(|v| {
            format!(
                "{} · {} · seq {} · {}×{}",
                v.session.chars().take(8).collect::<String>(),
                if attached {
                    v.state.as_str()
                } else {
                    "История · связь потеряна"
                },
                v.sequence,
                v.frame.geometry.cols,
                v.frame.geometry.rows
            )
        })
        .unwrap_or_else(|| "Нет сессии — запустите через host CLI".into());
    text(
        title,
        bounds.origin + point(px(8.), px(5.)),
        font,
        rgb(0xb0b8c5).into(),
        px(HEADER - 8.),
        window,
        cx,
    );
    let Some(view) = view else {
        return;
    };
    let frame = &view.frame;
    let scale = valid_scale(window.scale_factor());
    let cell_width_px = (f32::from(cell.width) * scale).round() as u16;
    let cell_height_px = (f32::from(cell.height) * scale).round() as u16;
    let cursor = cursor::plan(frame, pane.rows, pane.cols, focused)
        .filter(|plan| !plan.blinking || cursor_visible);
    let cursor_color = frame
        .palette_overrides
        .binary_search_by_key(&258, |(index, _)| *index)
        .ok()
        .map(|index| {
            let rgb = frame.palette_overrides[index].1;
            u32::from(rgb[0]) << 16 | u32::from(rgb[1]) << 8 | u32::from(rgb[2])
        })
        .unwrap_or(if focused { 0x7dafff } else { 0x606875 });
    let viewport = Bounds::new(
        bounds.origin + point(px(1.), px(HEADER)),
        size(
            bounds.size.width - px(2.),
            bounds.size.height - px(HEADER + 1.),
        ),
    );
    window.with_content_mask(Some(ContentMask { bounds: viewport }), |window| {
        window.paint_quad(fill(
            viewport,
            rgb(color("Named(Background)", false, &frame.palette_overrides)),
        ));
        // Backgrounds must be complete before any cluster is painted. A
        // following spacer/row must never erase wide glyphs or descenders.
        for operation in cell_paint_order(&frame.cells) {
            let (i, c) = match operation {
                CellPaint::Background(i, c) | CellPaint::Foreground(i, c) => (i, c),
            };
            let x = i % frame.geometry.cols as usize;
            let y = i / frame.geometry.cols as usize;
            if x >= usize::from(pane.cols) || y >= usize::from(pane.rows) {
                continue;
            }
            let origin = pane.origin + point(cell.width * x as f32, cell.height * y as f32);
            let mut fg = color(&c.fg, true, &frame.palette_overrides);
            let mut bg = color(&c.bg, false, &frame.palette_overrides);
            if c.flags & 1 != 0 {
                std::mem::swap(&mut fg, &mut bg);
            }
            if selection.is_some_and(|selection| {
                selection.contains(view, selection::CellPosition { row: y, column: x })
            }) {
                bg = 0x354c70;
                fg = 0xf2f4f7;
            }
            (fg, bg) = cursor::cell_colors(cursor, (y, x), (fg, bg), cursor_color);
            match operation {
                CellPaint::Background(..) => {
                    window.paint_quad(fill(Bounds::new(origin, cell), rgb(bg)))
                }
                CellPaint::Foreground(..) => {
                    let span = glyph_span_cells(c.flags) as u16;
                    let columns = pane.cols.min(frame.geometry.cols).saturating_sub(x as u16);
                    let style = cell_style::plan(
                        c,
                        cell_width_px,
                        cell_height_px,
                        span,
                        cell_width_px.saturating_mul(span.min(columns)),
                    );
                    let span_bounds = if style.width_px != 0 {
                        Bounds::new(
                            origin,
                            size(px(f32::from(style.width_px) / scale), cell.height),
                        )
                    } else {
                        glyph_span_bounds(origin, cell, c.flags)
                    };
                    // All backgrounds precede foregrounds. Host-owned wide
                    // spans and decorations stay inside acknowledged full cells.
                    window.with_content_mask(
                        Some(ContentMask {
                            bounds: span_bounds,
                        }),
                        |window| {
                            if style.glyph_visible && c.text != " " {
                                let mut glyphfont = font.clone();
                                if c.flags & 2 != 0 {
                                    glyphfont.weight = FontWeight::BOLD;
                                }
                                if c.flags & 4 != 0 {
                                    glyphfont.style = FontStyle::Italic;
                                }
                                text(
                                    c.text.clone(),
                                    origin,
                                    &glyphfont,
                                    rgb(fg).into(),
                                    cell.height,
                                    window,
                                    cx,
                                );
                            }
                            let underline_color = style
                                .underline_color
                                .map(|value| color(value, true, &frame.palette_overrides))
                                .unwrap_or(fg);
                            for (line, stroke_color) in style
                                .underline
                                .into_iter()
                                .flatten()
                                .map(|line| (line, underline_color))
                                .chain(style.strike.map(|line| (line, fg)))
                            {
                                for stroke in line.segments() {
                                    window.paint_quad(fill(
                                        Bounds::new(
                                            origin
                                                + point(
                                                    px(f32::from(stroke.x_px) / scale),
                                                    px(f32::from(stroke.y_px) / scale),
                                                ),
                                            size(
                                                px(f32::from(stroke.width_px) / scale),
                                                px(f32::from(stroke.height_px) / scale),
                                            ),
                                        ),
                                        rgb(stroke_color),
                                    ));
                                }
                            }
                        },
                    );
                }
            }
        }
        if let Some(cursor) = cursor {
            let origin = pane.origin
                + point(
                    cell.width * f32::from(cursor.column),
                    cell.height * f32::from(cursor.row),
                );
            let width = cell.width * f32::from(cursor.span);
            let thin = px(1. / valid_scale(window.scale_factor()));
            let thick = (thin * 2.).min(cell.height);
            let rect = |offset, size| Some(Bounds::new(origin + offset, size));
            use protocol::CursorPresentationShape::*;
            let strokes = match cursor.shape {
                Underline => [
                    rect(point(px(0.), cell.height - thick), size(width, thick)),
                    None,
                    None,
                    None,
                ],
                Beam => [
                    rect(point(px(0.), px(0.)), size(thin.min(width), cell.height)),
                    None,
                    None,
                    None,
                ],
                HollowBlock => [
                    rect(point(px(0.), px(0.)), size(width, thin)),
                    rect(point(px(0.), cell.height - thin), size(width, thin)),
                    rect(point(px(0.), px(0.)), size(thin, cell.height)),
                    rect(point(width - thin, px(0.)), size(thin, cell.height)),
                ],
                Block | Hidden => [None; 4],
            };
            for bounds in strokes.into_iter().flatten() {
                window.paint_quad(fill(bounds, rgb(cursor_color)));
            }
        }
    });
}

fn main() {
    if std::env::args().nth(1).as_deref() == Some("--version") {
        println!(
            "PigADE native canvas {} (stage-0)",
            env!("CARGO_PKG_VERSION")
        );
        return;
    }
    let socket = std::env::args()
        .nth(1)
        .expect("usage: pigade-app /path/to/host.sock");
    let diagnostic_preedit = std::env::args()
        .skip(2)
        .any(|arg| arg == "--diagnostic-preedit");
    let diagnostic_selected = std::env::args()
        .skip(2)
        .find_map(|arg| {
            arg.strip_prefix("--diagnostic-selected=")
                .and_then(|index| index.parse::<usize>().ok())
        })
        .filter(|index| *index < 4)
        .unwrap_or(0);
    let reduced_motion = std::env::args()
        .skip(2)
        .any(|arg| arg == "--reduced-motion");
    let diagnostic_selection = std::env::args()
        .skip(2)
        .any(|arg| arg == "--diagnostic-selection");
    let trace = if std::env::args()
        .skip(2)
        .any(|arg| arg == "--diagnostic-publication-trace")
    {
        Trace::enabled().expect("publication diagnostic logger must start")
    } else {
        Trace::disabled()
    };
    let connection = ipc(socket.into(), trace);
    application().run(move |cx: &mut App| {
        cx.set_reduce_motion(reduced_motion);
        crate::font::install(cx).expect("bundled terminal font profile must load");
        cx.on_window_closed(|cx, _window_id| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        let bounds = Bounds::centered(None, size(px(1280.), px(800.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                app_id: Some("org.pigade.app".into()),
                ..Default::default()
            },
            move |window, cx| {
                let view = cx.new(|cx| {
                    CanvasApp::new(
                        window,
                        cx,
                        connection,
                        diagnostic_preedit,
                        diagnostic_selection,
                    )
                });
                view.update(cx, |this, cx| {
                    this.selected = diagnostic_selected;
                    cx.notify();
                });
                let focus = view.read(cx).focus.clone();
                focus.focus(window, cx);
                view
            },
        )
        .unwrap();
        cx.activate(true);
    });
}
#[cfg(test)]
mod tests {
    use super::command_worker::{CommandNotice, CommandTarget};
    use super::protocol::{Cell, RecoveryDiagnostics, RecoveryIssue, View};
    use super::{
        CellPaint, PaneGeometry, State, apply_poll, apply_state_update, can_send_to_view,
        cell_paint_order, color, composition_target_changed, glyph_span_bounds, glyph_span_cells,
        project_panels, recovery_warning, state_update, terminal_line_height,
    };
    use gpui::{Bounds, point, px, size};
    use serde_json::json;

    fn view(storage_error: Option<&str>) -> View {
        serde_json::from_value(json!({
            "session":"s",
            "generation":"g",
            "sequence":1,
            "state":"running",
            "recovery_path":"live",
            "pid":1,
            "storage_error":storage_error,
            "frame":{
                "geometry":{"rows":1,"cols":1},
                "cells":[{"text":"x","fg":"Named(Foreground)","bg":"Named(Background)","flags":0,"underline":null,"hyperlink":null}],
                "hyperlinks":[],"cursor":[0,0],"cursor_style":"Block","modes":0,
                "colors":"","history_lines":0,"sync_pending_bytes":0,"title":""
            }
        }))
        .unwrap()
    }

    #[test]
    fn scoped_command_notice_blocks_only_its_exact_session_generation() {
        let mut blocked = view(None);
        blocked.session = "noisy-session".into();
        blocked.generation = "generation-a".into();
        let mut healthy = view(None);
        healthy.session = "healthy-session".into();
        healthy.generation = "generation-b".into();
        let state = State {
            command_notices: vec![CommandNotice {
                token: 1,
                last_request_id: 0,
                target: CommandTarget {
                    session: blocked.session.clone(),
                    generation: blocked.generation.clone(),
                },
                message: "command rejected".into(),
                unsent: 0,
                rejected_resizes: Vec::new(),
                acknowledgeable: true,
            }],
            ..State::default()
        };
        assert!(!can_send_to_view(&blocked, &state, None));
        assert!(can_send_to_view(&healthy, &state, None));

        // A later generation under the same session ID is an independent PTY.
        blocked.generation = "generation-c".into();
        assert!(can_send_to_view(&blocked, &state, None));
    }

    #[test]
    fn pointer_hit_uses_acknowledged_visible_cells_and_clamps_captured_release() {
        let pane = PaneGeometry::new(
            Bounds::new(point(px(0.), px(0.)), size(px(300.), px(200.))),
            size(px(8.), px(16.)),
            1.25,
        );
        let geometry = super::protocol::Geometry { rows: 3, cols: 4 };
        assert_eq!(
            pane.pointer_position(pane.origin, &geometry, false),
            Some(super::mouse::Position { row: 0, column: 0 })
        );
        assert!(
            pane.pointer_position(point(px(10.), px(10.)), &geometry, false)
                .is_none(),
            "header is not a terminal cell"
        );
        let beyond_ack = pane.origin + point(pane.cell.width * 4., pane.cell.height);
        assert!(
            pane.pointer_position(beyond_ack, &geometry, false)
                .is_none()
        );
        assert_eq!(
            pane.pointer_position(point(px(900.), px(-50.)), &geometry, true),
            Some(super::mouse::Position { row: 0, column: 3 })
        );
        assert!(
            pane.pointer_position(point(px(f32::NAN), px(0.)), &geometry, true)
                .is_none()
        );
        assert!(
            pane.pointer_position(
                pane.origin,
                &super::protocol::Geometry { rows: 0, cols: 4 },
                true
            )
            .is_none()
        );
    }

    fn route() -> super::PointerRoute {
        super::PointerRoute {
            session: "s".into(),
            generation: "g".into(),
            encoder: super::mouse::Encoder::default(),
            position: super::mouse::Position { row: 0, column: 0 },
            wheel: super::mouse::WheelAccumulator::default(),
        }
    }

    #[test]
    fn pointer_capture_follows_identity_across_reordering_and_refuses_new_generation() {
        let mut other = view(None);
        other.session = "other".into();
        let mut state = State {
            views: vec![other, view(None)],
            ..State::default()
        };
        let route = route();
        assert_eq!(route.index(&state), Some(1));
        state.views.swap(0, 1);
        assert_eq!(route.index(&state), Some(0));
        state.views[0].generation = "replacement".into();
        assert_eq!(route.index(&state), None);
    }

    #[test]
    fn rejected_pointer_candidate_does_not_consume_press_or_release() {
        use super::{
            PointerAction,
            mouse::{Button, MOUSE_DRAG, Modifiers, Position, SGR_MOUSE},
        };
        let original = route();
        let mut candidate = original.clone();
        assert_eq!(
            candidate
                .event(
                    PointerAction::Press(Button::Left),
                    Position { row: 2, column: 3 },
                    Modifiers::default(),
                    MOUSE_DRAG | SGR_MOUSE
                )
                .unwrap(),
            b"\x1b[<0;4;3M"
        );
        assert!(!original.encoder.has_pressed());
        assert!(candidate.encoder.has_pressed());
        let accepted = candidate;
        let mut rejected_release = accepted.clone();
        rejected_release.event(
            PointerAction::Release(Button::Left),
            Position { row: 3, column: 4 },
            Modifiers::default(),
            MOUSE_DRAG | SGR_MOUSE,
        );
        assert!(accepted.encoder.has_pressed());
        assert!(!rejected_release.encoder.has_pressed());
    }

    #[test]
    fn unencodable_release_does_not_clear_confirmed_local_button_state() {
        use super::{
            PointerAction,
            mouse::{Button, MOUSE_DRAG, Modifiers, Position, SGR_MOUSE},
        };
        let mut route = route();
        route
            .event(
                PointerAction::Press(Button::Left),
                Position { row: 2, column: 3 },
                Modifiers::default(),
                MOUSE_DRAG | SGR_MOUSE,
            )
            .unwrap();
        assert!(
            route
                .event(
                    PointerAction::Release(Button::Left),
                    Position { row: 2, column: 3 },
                    Modifiers::default(),
                    0,
                )
                .is_none()
        );
        assert!(route.encoder.has_pressed());
    }

    #[test]
    fn legacy_drag_release_outside_encoding_range_uses_last_reported_position() {
        use super::{
            PointerAction,
            mouse::{Button, MOUSE_DRAG, Modifiers, Position},
        };
        let mut route = route();
        let last = Position { row: 0, column: 4 };
        route
            .event(
                PointerAction::Press(Button::Left),
                last,
                Modifiers::default(),
                MOUSE_DRAG,
            )
            .unwrap();
        let outside = Position {
            row: 0,
            column: 250,
        };
        assert!(
            route
                .event(
                    PointerAction::Motion,
                    outside,
                    Modifiers::default(),
                    MOUSE_DRAG
                )
                .is_none()
        );
        assert_eq!(route.position, last);
        assert_eq!(
            route
                .event(
                    PointerAction::Release(Button::Left),
                    outside,
                    Modifiers::default(),
                    MOUSE_DRAG
                )
                .unwrap(),
            b"\x1b[M#%!"
        );
        assert!(!route.encoder.has_pressed());
        assert_eq!(route.position, last);
    }

    #[test]
    fn cursor_blink_reset_tracks_its_target_not_unrelated_output() {
        let state = State {
            views: vec![view(None), view(None)],
            ..State::default()
        };
        let mut shared = state.clone();
        shared.revision += 1;
        shared.views_revision += 1;
        shared.views[1].sequence += 1;
        let mut update = state_update(&state, &shared).unwrap();
        assert!(!super::cursor_target_changed(&state, 0, &update));
        update.views.as_mut().unwrap()[0].frame.cursor = (0, 1);
        assert!(super::cursor_target_changed(&state, 0, &update));
        update.views.as_mut().unwrap()[0].frame.cursor = (0, 0);
        update.views.as_mut().unwrap()[0].generation = "new".into();
        assert!(super::cursor_target_changed(&state, 0, &update));
    }

    #[test]
    fn composition_is_cancelled_when_its_generation_or_input_availability_changes() {
        let state = State {
            views: vec![view(None)],
            ..State::default()
        };
        let mut shared = state.clone();
        shared.revision += 1;
        shared.views_revision += 1;
        shared.views[0].sequence += 1;
        let mut update = state_update(&state, &shared).unwrap();
        assert!(!composition_target_changed(&state, 0, &update));
        update.views.as_mut().unwrap()[0].generation = "new-generation".into();
        assert!(composition_target_changed(&state, 0, &update));
        update.views = Some(vec![view(Some("storage unavailable"))]);
        assert!(composition_target_changed(&state, 0, &update));
        let mut unsupported = view(None);
        unsupported.frame.modes |= 1 << 18;
        update.views = Some(vec![unsupported]);
        assert!(composition_target_changed(&state, 0, &update));
        update.views = Some(vec![]);
        assert!(composition_target_changed(&state, 0, &update));
        update.views = None;
        update.error = Some("disconnected".into());
        assert!(composition_target_changed(&state, 0, &update));
    }

    fn recovery() -> RecoveryDiagnostics {
        RecoveryDiagnostics {
            issues: vec![RecoveryIssue {
                journal: "session-7.journal".into(),
                message: "checkpoint cannot be read".into(),
            }],
            omitted: 0,
        }
    }

    #[test]
    fn recovery_notice_updates_revision_without_replacing_cached_cells() {
        let mut ui_state = State {
            revision: 1,
            views_revision: 1,
            views: vec![view(None)],
            ..State::default()
        };
        let mut shared = State {
            revision: 1,
            views_revision: 1,
            views: ui_state.views.clone(),
            ..State::default()
        };
        let cell_ptr = ui_state.views[0].frame.cells.as_ptr();
        let selected = shared.views[0].known();
        let changes = apply_poll(
            &mut shared,
            super::protocol::Poll {
                selected: vec![selected],
                views: vec![],
                recovery: Some(recovery()),
            },
        )
        .unwrap();
        assert!(!changes.views);
        assert!(changes.recovery);
        assert_eq!(shared.revision, 2);
        assert_eq!(shared.views_revision, 1);

        let update = state_update(&ui_state, &shared).unwrap();
        assert!(update.views.is_none());
        apply_state_update(&mut ui_state, update);
        assert_eq!(ui_state.views[0].frame.cells.as_ptr(), cell_ptr);
        assert_eq!(ui_state.recovery, Some(recovery()));
    }

    #[test]
    fn recovery_and_selected_history_warnings_are_visible_and_nonfatal() {
        let mut state = State {
            recovery: Some(recovery()),
            ..State::default()
        };
        let healthy = view(None);
        assert!(can_send_to_view(&healthy, &state, None));
        assert!(!can_send_to_view(
            &healthy,
            &state,
            Some("input not admitted")
        ));
        let warning = recovery_warning(state.recovery.as_ref(), None).unwrap();
        assert!(warning.contains("session-7.journal"));
        assert!(warning.contains("checkpoint cannot be read"));

        let unavailable_history = view(Some("cold history archive is unavailable"));
        assert!(!can_send_to_view(&unavailable_history, &state, None));
        let warning = recovery_warning(None, unavailable_history.storage_error.as_deref()).unwrap();
        assert!(warning.contains("cold history archive is unavailable"));
        assert!(can_send_to_view(&healthy, &state, None));

        state.error = Some("host connection lost".into());
        assert!(!can_send_to_view(&healthy, &state, None));
    }

    #[test]
    fn recovery_warning_saturates_untrusted_omission_counts() {
        let mut diagnostics = recovery();
        diagnostics.issues.push(diagnostics.issues[0].clone());
        diagnostics.omitted = u64::MAX;
        let warning = recovery_warning(Some(&diagnostics), None).unwrap();
        assert!(warning.contains(&u64::MAX.to_string()));
    }

    #[test]
    fn signed_font_descent_keeps_complete_extent_and_device_line_height() {
        let negative = terminal_line_height(px(12.), px(-4.));
        let positive = terminal_line_height(px(12.), px(4.));
        assert_eq!(negative, px(20.));
        assert_eq!(negative, positive);
        let panel = Bounds::new(point(px(0.), px(0.)), size(px(1280.), px(800.)));
        for scale in [1., 1.25, 1.5, 2.] {
            let pane = PaneGeometry::new(panel, size(px(8.4), negative), scale);
            // GPUI places the baseline at half the extra line space + ascent.
            let baseline = (f32::from(pane.cell.height) - 12. - 4.) / 2. + 12.;
            assert!(baseline - 12. >= 0.);
            assert!(baseline + 4. <= f32::from(pane.cell.height));
        }
    }
    #[test]
    fn glyph_clip_matches_host_allocated_cell_span() {
        let origin = point(px(4.), px(9.));
        let cell = size(px(8.5), px(19.));
        let single = glyph_span_bounds(origin, cell, 0);
        let wide = glyph_span_bounds(origin, cell, 1 << 5);
        assert_eq!(glyph_span_cells(0), 1);
        assert_eq!(glyph_span_cells(1 << 5), 2);
        assert_eq!(single.origin, origin);
        assert_eq!(single.size, cell);
        assert_eq!(wide.origin, origin);
        assert_eq!(wide.size, size(px(17.), px(19.)));
    }
    #[test]
    fn backgrounds_cannot_erase_wide_spans_or_next_row_descenders() {
        let cell = |text: &str, flags| Cell {
            text: text.into(),
            fg: "Named(Foreground)".into(),
            bg: "Named(Background)".into(),
            flags,
            underline: None,
            hyperlink: None,
        };
        // Two columns, three rows. A CJK cluster spans the first row;
        // the second-row g extends into the next row's background area.
        let cells = [
            cell("界", 32),
            cell(" ", 64),
            cell("g", 0),
            cell("e\u{301}", 0),
            cell(" ", 0),
            cell(" ", 0),
        ];
        let mut coverage = [false; 6];
        let mut glyph_started = false;
        let mut clusters = Vec::new();
        for operation in cell_paint_order(&cells) {
            match operation {
                CellPaint::Background(index, _) => {
                    assert!(!glyph_started);
                    coverage[index] = false;
                }
                CellPaint::Foreground(index, cell) => {
                    glyph_started = true;
                    clusters.push(cell.text.as_str());
                    coverage[index] = true;
                    if index == 0 {
                        coverage[1] = true;
                    } // wide continuation
                    if index == 2 {
                        coverage[4] = true;
                    } // descender coverage
                }
            }
        }
        assert!(coverage[1] && coverage[4]);
        assert_eq!(clusters, ["界", "g", "e\u{301}"]);
    }
    #[test]
    fn decorated_spaces_receive_foreground_without_revealing_hidden_cells() {
        let cell = |flags| Cell {
            text: " ".into(),
            fg: "Named(Foreground)".into(),
            bg: "Named(Background)".into(),
            flags,
            underline: None,
            hyperlink: None,
        };
        let cells = [
            cell(8),
            cell(512),
            cell(2048),
            cell(8 | 256),
            cell(8 | 64),
            cell(0),
        ];
        let foreground: Vec<_> = cell_paint_order(&cells)
            .filter_map(|op| match op {
                CellPaint::Foreground(index, _) => Some(index),
                _ => None,
            })
            .collect();
        assert_eq!(foreground, [0, 1, 2]);
    }
    #[test]
    fn pane_projection_uses_available_area_without_fake_panels() {
        let bounds = Bounds::new(point(px(0.), px(0.)), size(px(1280.), px(800.)));
        for count in 1..=4 {
            let panels = project_panels(bounds, count, 1.);
            assert_eq!(panels.len(), count);
            assert_eq!(
                panels[0].size.height,
                if count <= 2 { px(800.) } else { px(397.) }
            );
        }
        assert_eq!(project_panels(bounds, 1, 1.)[0], bounds);
        assert_eq!(project_panels(bounds, 3, 1.)[2].size.width, px(1280.));
    }
    #[test]
    fn font_metrics_and_fractional_scale_share_device_grid() {
        for scale in [1., 1.25, 1.5, 2.] {
            let bounds = Bounds::new(point(px(-679.3), px(342.2)), size(px(1280.7), px(800.5)));
            for count in 1..=4 {
                for panel in project_panels(bounds, count, scale) {
                    let pane = PaneGeometry::new(panel, size(px(8.4), px(18.2)), scale);
                    let integral = |logical: gpui::Pixels| {
                        let device = f32::from(logical) * scale;
                        (device - device.round()).abs() < 0.001
                    };
                    assert!(
                        integral(pane.cell.width)
                            && integral(pane.cell.height)
                            && integral(pane.origin.x)
                            && integral(pane.origin.y)
                    );
                    assert!(
                        pane.origin.x + pane.cell.width * pane.cols as f32
                            <= panel.right() - px(8.) + px(0.001)
                    );
                    assert!(
                        pane.origin.y + pane.cell.height * pane.rows as f32
                            <= panel.bottom() - px(8.) + px(0.001)
                    );
                    assert!(pane.rows > 0 && pane.cols > 0);
                }
            }
        }
    }
    #[test]
    fn tiny_canvas_preserves_nonzero_pty_size() {
        let bounds = Bounds::new(point(px(0.), px(0.)), size(px(1.), px(1.)));
        let pane = PaneGeometry::new(bounds, size(px(8.4), px(18.2)), 1.25);
        assert_eq!((pane.rows, pane.cols), (1, 2));
    }
    #[test]
    fn ansi_color_fields_are_display_only() {
        assert_eq!(color("Spec(Rgb { r: 1, g: 2, b: 3 })", true, &[]), 0x010203);
        assert_eq!(color("Indexed(196)", true, &[]), 0xff0000);
        assert_eq!(color("Named(BrightBlue)", true, &[]), 0x88c0ff);
    }
}

#[cfg(test)]
mod notification_transport_tests {
    use super::Trace;
    use super::{
        CommandTarget, DeliveryOutcome, IpcMessage, Poll, RequestReceipt, State, WorkerReceipt,
        apply_poll_result, ipc,
    };
    use serde_json::{Value, json};
    use std::{
        fs::{self, DirBuilder},
        io::{Read, Write},
        os::unix::{
            fs::{DirBuilderExt, PermissionsExt},
            net::UnixListener,
        },
        time::{SystemTime, UNIX_EPOCH},
    };
    use std::{
        path::PathBuf,
        sync::mpsc,
        thread,
        time::{Duration, Instant},
    };

    struct PrivateHost {
        root: PathBuf,
        release: Option<mpsc::Sender<()>>,
        thread: Option<thread::JoinHandle<Result<(), String>>>,
    }
    impl Drop for PrivateHost {
        fn drop(&mut self) {
            if let Some(release) = self.release.take() {
                let _ = release.send(());
            }
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn read_command(stream: &mut std::os::unix::net::UnixStream) -> Result<Value, String> {
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .map_err(|e| e.to_string())?;
        stream
            .set_write_timeout(Some(Duration::from_secs(2)))
            .map_err(|e| e.to_string())?;
        let mut length = [0; 4];
        stream.read_exact(&mut length).map_err(|e| e.to_string())?;
        let length = u32::from_le_bytes(length) as usize;
        if !(1..=4096).contains(&length) {
            return Err("test request bound".into());
        }
        let mut body = vec![0; length];
        stream.read_exact(&mut body).map_err(|e| e.to_string())?;
        serde_json::from_slice(&body).map_err(|e| e.to_string())
    }

    fn reply(stream: &mut std::os::unix::net::UnixStream, value: Value) -> Result<(), String> {
        let bytes = serde_json::to_vec(&value).map_err(|e| e.to_string())?;
        stream
            .write_all(&(bytes.len() as u32).to_le_bytes())
            .and_then(|_| stream.write_all(&bytes))
            .map_err(|e| e.to_string())
    }

    #[test]
    fn readonly_poll_backpressure_does_not_latch_global_input_error() {
        let mut state = State::default();
        apply_poll_result(&mut state, Err(super::protocol::PollError::Busy));
        assert!(state.poll_backpressured);
        assert!(state.error.is_none());
        let revision = state.revision;
        apply_poll_result(
            &mut state,
            Ok((
                Poll {
                    selected: vec![],
                    views: vec![],
                    recovery: None,
                },
                None,
            )),
        );
        assert!(!state.poll_backpressured);
        assert!(state.error.is_none());
        assert!(state.revision > revision);
    }

    #[test]
    fn blocked_change_wait_does_not_block_input_worker() -> Result<(), String> {
        let root = std::env::temp_dir().join(format!(
            "pg-notify-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        DirBuilder::new()
            .mode(0o700)
            .create(&root)
            .map_err(|e| e.to_string())?;
        let socket = root.join("host.sock");
        let listener = UnixListener::bind(&socket).map_err(|e| e.to_string())?;
        fs::set_permissions(&socket, fs::Permissions::from_mode(0o600))
            .map_err(|e| e.to_string())?;
        listener.set_nonblocking(true).map_err(|e| e.to_string())?;
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let server = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut release = Some(release_rx);
            let mut waiter = None;
            let mut received_input = false;
            for _ in 0..3 {
                let mut stream = loop {
                    match listener.accept() {
                        Ok((stream, _)) => break stream,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            if Instant::now() > deadline {
                                return Err("test host accept deadline".into());
                            }
                            thread::sleep(Duration::from_millis(1));
                        }
                        Err(error) => return Err(error.to_string()),
                    }
                };
                let request = read_command(&mut stream)?;
                let token =
                    json!({"host_instance":"33333333-3333-4333-8333-333333333333","revision":0});
                match request["command"]["command"].as_str() {
                    Some("poll") => reply(
                        &mut stream,
                        json!({"protocol":2,"views":[],"selected":[],"error":null,"change_token":token}),
                    )?,
                    Some("wait_for_change") => {
                        let release = release.take().ok_or("duplicate test waiter")?;
                        let started_tx = started_tx.clone();
                        waiter = Some(thread::spawn(move || {
                            started_tx.send(()).map_err(|e| e.to_string())?;
                            let _ = release.recv_timeout(Duration::from_secs(4));
                            reply(
                                &mut stream,
                                json!({"protocol":2,"views":[],"error":null,"change_token":token}),
                            )
                        }));
                    }
                    Some("input") => {
                        received_input = true;
                        reply(&mut stream, json!({"protocol":2,"views":[],"error":null}))?;
                    }
                    _ => return Err("unexpected test request".into()),
                }
            }
            if let Some(waiter) = waiter {
                waiter.join().map_err(|_| "test waiter panic")??;
            }
            if !received_input {
                return Err("input was never dispatched".into());
            }
            Ok(())
        });
        let mut host = PrivateHost {
            root,
            release: Some(release_tx),
            thread: Some(server),
        };
        let connection = ipc(socket, Trace::disabled());
        started_rx
            .recv_timeout(Duration::from_secs(2))
            .map_err(|e| format!("wait never started: {e}"))?;
        let target = CommandTarget {
            session: "11111111-1111-4111-8111-111111111111".into(),
            generation: "22222222-2222-4222-8222-222222222222".into(),
        };
        connection.tx.try_send(IpcMessage::Request {
            id: 1, target: target.clone(), command: json!({"command":"input","session":target.session,"generation":target.generation,"bytes":[120]}),
        }).map_err(|e| e.to_string())?;
        let receipt = connection
            .receipts
            .recv_timeout(Duration::from_secs(2))
            .map_err(|e| format!("input blocked behind change wait: {e}"))?;
        assert!(matches!(
            receipt,
            WorkerReceipt::Request(RequestReceipt {
                id: 1,
                outcome: DeliveryOutcome::Accepted,
                ..
            })
        ));
        drop(connection);
        host.release.take().unwrap().send(()).unwrap();
        host.thread
            .take()
            .unwrap()
            .join()
            .map_err(|_| "test host panic")??;
        Ok(())
    }
}

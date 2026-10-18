//! PigADE native application. The session host owns ANSI parsing and CLI lifetime.
mod appearance;
mod appearance_commands;
mod block_glyph;
mod catalog_flow;
mod cell_style;
mod chrome;
mod command_worker;
mod controls;
mod cursor;
mod edit_field;
mod font;
mod history_view;
mod input;
mod memory_choice;
mod mouse;
mod mouse_delivery;
mod navigation;
mod palette;
mod pointer_layout;
mod product;
mod profile_commands;
mod profile_flow;
mod prompt_commands;
mod prompt_flow;
mod protocol;
mod publication_trace;
mod selection;
mod session_control;
mod setup_flow;
mod startup;
mod tab_close;
mod template_commands;
mod template_flow;
mod text_edit;
mod tiling;
mod viewport;
mod wake_signal;
mod workspace_git;
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
    visible_panels: Option<Vec<protocol::PanelTarget>>,
    supports_generation_selection: bool,
}
impl State {
    fn set_visible_panels(&mut self, panels: Vec<protocol::PanelTarget>) {
        if self.visible_panels.as_ref() == Some(&panels) {
            return;
        }
        self.views.retain(|view| {
            panels.iter().any(|target| {
                target.session.to_string() == view.session
                    && self
                        .visible_panels
                        .as_ref()
                        .is_some_and(|old| old.contains(target))
                    && target
                        .generation
                        .is_none_or(|generation| generation.to_string() == view.generation)
            })
        });
        self.visible_panels = Some(panels);
        self.views_revision = self.views_revision.wrapping_add(1);
        self.revision = self.revision.wrapping_add(1);
    }
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

/// Only the displayed ACK button and release label depend on receipt bookkeeping.
fn acknowledgement_presentation(
    notice: Option<&CommandNotice>,
    delivery: &MouseDelivery,
) -> Option<(u64, bool)> {
    notice
        .filter(|notice| {
            notice.acknowledgeable
                && delivery.settled_through(&notice.target, notice.last_request_id)
        })
        .map(|notice| (notice.token, delivery.confirmed_pressed(&notice.target)))
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

#[derive(Debug, PartialEq, Eq)]
enum AdmissionFailure {
    Busy,
    Reservation,
    Disconnected,
    Exhausted,
}
impl AdmissionFailure {
    fn message(&self) -> &'static str {
        match self {
            Self::Busy => "Очередь ввода занята; этот ввод не принят. Повторите после освобождения",
            Self::Reservation => "Ввод не принят: нет места для подтверждения команды",
            Self::Disconnected => "Соединение с обработчиком ввода закрыто; переоткройте окно",
            Self::Exhausted => "Исчерпан счетчик запросов; перезапустите приложение",
        }
    }
    fn blocks_input(&self) -> bool {
        matches!(self, Self::Disconnected | Self::Exhausted)
    }
}

/// A local rejection proves no command was dispatched. It must not become a
/// permanent input gate. Host rejections and unknown outcomes remain separate.
fn admit_command(
    delivery: &mut MouseDelivery,
    next_id: &mut u64,
    tx: &mpsc::SyncSender<IpcMessage>,
    target: CommandTarget,
    command: Value,
    mouse_after: Option<MouseSnapshot>,
) -> Result<(), AdmissionFailure> {
    if !delivery.has_capacity() {
        return Err(AdmissionFailure::Busy);
    }
    let next = next_id.checked_add(1).ok_or(AdmissionFailure::Exhausted)?;
    let id = *next_id;
    *next_id = next;
    if !delivery.stage(id, target.clone(), mouse_after) {
        return Err(AdmissionFailure::Reservation);
    }
    match tx.try_send(IpcMessage::Request {
        id,
        target,
        command,
    }) {
        Ok(()) => Ok(()),
        Err(error) => {
            delivery.cancel_stage(id);
            Err(match error {
                mpsc::TrySendError::Full(_) => AdmissionFailure::Busy,
                mpsc::TrySendError::Disconnected(_) => AdmissionFailure::Disconnected,
            })
        }
    }
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

fn catalog_panes(catalog: &pigade_storage::Catalog) -> Vec<uuid::Uuid> {
    catalog_panes_for(catalog, false)
}

fn catalog_panes_for(catalog: &pigade_storage::Catalog, reveal_all: bool) -> Vec<uuid::Uuid> {
    catalog
        .selection
        .tab_id
        .and_then(|id| catalog.tabs.iter().find(|t| t.id == id))
        .and_then(|tab| serde_json::from_str::<pigade_layout::TabLayout>(&tab.layout_json).ok())
        .map(|layout| {
            if layout.focus_mode && !reveal_all {
                layout.focused.into_iter().collect()
            } else {
                layout.panes()
            }
        })
        .unwrap_or_default()
}

fn catalog_panel_targets(catalog: &pigade_storage::Catalog) -> Vec<protocol::PanelTarget> {
    catalog_panel_targets_for(catalog, false)
}

fn catalog_panel_targets_for(
    catalog: &pigade_storage::Catalog,
    reveal_all: bool,
) -> Vec<protocol::PanelTarget> {
    catalog_panes_for(catalog, reveal_all)
        .into_iter()
        .map(|session| protocol::PanelTarget {
            session,
            generation: catalog
                .selection
                .window
                .panel_generations
                .get(&session)
                .copied(),
        })
        .collect()
}

struct WorkerConnection {
    tx: mpsc::SyncSender<IpcMessage>,
    shared: Arc<Mutex<State>>,
    receipts: mpsc::Receiver<WorkerReceipt>,
    wake: WakeSignal,
    trace: Trace,
    product_tx: mpsc::SyncSender<Value>,
    product_rx: mpsc::Receiver<Result<protocol::ProductReply, protocol::ProductError>>,
    git_worker: workspace_git::Worker,
}

fn ipc(socket: PathBuf, trace: Trace) -> WorkerConnection {
    let (tx, rx) = mpsc::sync_channel(32);
    let (receipt_tx, receipt_rx) = mpsc::sync_channel(32);
    let shared = Arc::new(Mutex::new(State::default()));
    let output = shared.clone();
    let wake = WakeSignal::default();
    let worker_wake = wake.clone();
    let (product_tx, product_commands) = mpsc::sync_channel(8);
    let (product_results, product_rx) = mpsc::sync_channel(8);
    let git_worker = workspace_git::Worker::new(socket.clone(), wake.clone());
    let product_socket = socket.clone();
    let product_shared = shared.clone();
    let poll_gate = Arc::new(Mutex::new(()));
    let product_poll_gate = poll_gate.clone();
    let product_wake = wake.clone();
    thread::spawn(move || {
        let mut next = json!({"command":"catalog"});
        let mut routing_catalog: Option<pigade_storage::Catalog> = None;
        loop {
            // UI-only replica refresh: do not send a product command, mutate
            // metadata, or manufacture a reply that could cancel a live drag.
            // The existing bounded queue and poll gate serialize this with
            // prefetch/poll; input and launch do not wait on this worker.
            if next.get("_ui_refresh_panels") == Some(&Value::Bool(true)) {
                serialized_poll(
                    &product_shared,
                    &product_poll_gate,
                    false,
                    |known, visible, supports| {
                        fetch_replica(&product_socket, known, visible, supports)
                    },
                );
                product_wake.notify();
                let Ok(command) = product_commands.recv() else {
                    return;
                };
                next = command;
                continue;
            }
            let focus_only = next.pointer("/action/command/Focus/pane").is_some();
            let reply = protocol::product_request(&product_socket, next);
            if let Ok(reply) = &reply {
                if let Some(catalog) = &reply.catalog {
                    routing_catalog = Some(catalog.clone());
                }
                if let Some(selection) = &reply.selection
                    && let Some(catalog) = &mut routing_catalog
                    && selection.revision >= catalog.selection.revision
                {
                    catalog.selection = selection.clone();
                }
                if (reply.catalog.is_some() || reply.generation_selected)
                    && let Some(catalog) = &routing_catalog
                {
                    let panels = catalog_panel_targets(catalog);
                    {
                        let mut state = product_shared.lock().unwrap();
                        if let Some(supports) = reply.generation_selection {
                            state.supports_generation_selection = supports;
                        }
                        state.set_visible_panels(panels);
                    }
                    if !focus_only {
                        serialized_poll(
                            &product_shared,
                            &product_poll_gate,
                            true,
                            |known, visible, supports| {
                                fetch_replica(&product_socket, known, visible, supports)
                            },
                        );
                    }
                }
            }
            product_wake.notify();
            if product_results.send(reply).is_err() {
                return;
            }
            product_wake.notify();
            let Ok(command) = product_commands.recv() else {
                return;
            };
            next = command;
        }
    });
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
    spawn_poll_worker(
        socket,
        Arc::downgrade(&shared),
        poll_gate,
        wake.clone(),
        trace.clone(),
    );
    WorkerConnection {
        tx,
        shared,
        receipts: receipt_rx,
        wake,
        trace,
        product_tx,
        product_rx,
        git_worker,
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

// The periodic poller and product prefetch publish into one replica. Serialize
// capture → request → apply, so an older in-flight reply cannot overtake a newer
// frame. The UI-state lock is released throughout I/O, and input/launch/wait
// connections never take this gate. True host sequence regressions still fail.
fn serialized_poll(
    shared: &Mutex<State>,
    gate: &Mutex<()>,
    fresh: bool,
    fetch: impl FnOnce(
        &[protocol::Known],
        Option<&[protocol::PanelTarget]>,
        bool,
    ) -> Result<(Poll, Option<protocol::ChangeToken>), protocol::PollError>,
) -> (Option<protocol::ChangeToken>, bool) {
    let _poll = gate.lock().unwrap();
    let (known, visible, supports) = {
        let state = shared.lock().unwrap();
        (
            if fresh {
                Vec::new()
            } else {
                state.views.iter().map(View::known).collect()
            },
            state.visible_panels.clone(),
            state.supports_generation_selection,
        )
    };
    let result = fetch(&known, visible.as_deref(), supports);
    let mut state = shared.lock().unwrap();
    if state.visible_panels != visible {
        return (None, false);
    }
    let revision = state.revision;
    let token = apply_poll_result(&mut state, result);
    (token, revision != state.revision)
}

fn fetch_replica(
    socket: &std::path::Path,
    known: &[protocol::Known],
    visible: Option<&[protocol::PanelTarget]>,
    supports: bool,
) -> Result<(Poll, Option<protocol::ChangeToken>), protocol::PollError> {
    match visible {
        Some(panels) if supports => protocol::poll_panels(socket, known, panels),
        Some(panels) => protocol::poll_visible(
            socket,
            known,
            &panels.iter().map(|panel| panel.session).collect::<Vec<_>>(),
        ),
        None => protocol::poll(socket, known),
    }
}

fn spawn_poll_worker(
    socket: PathBuf,
    shared: std::sync::Weak<Mutex<State>>,
    poll_gate: Arc<Mutex<()>>,
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
            let (next_token, changed) =
                serialized_poll(&shared, &poll_gate, false, |known, visible, supports| {
                    fetch_replica(&socket, known, visible, supports)
                });
            token = next_token;
            if changed {
                trace_publication(&trace, TraceStage::PollApplied, &shared.lock().unwrap());
            }
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum LayoutMode {
    Swap,
    Resize,
    Pan,
}

struct CanvasScrollDrag {
    bar: viewport::Scrollbar,
    initial: pigade_storage::CanvasOffset,
    start: Point<Pixels>,
}

struct ResizeGesture {
    button: MouseButton,
    tab: uuid::Uuid,
    base: pigade_layout::TabLayout,
    split: uuid::Uuid,
    boundary: usize,
    axis: pigade_layout::Axis,
    start: Point<Pixels>,
    extent: f64,
}

struct PaneDrag {
    pane: uuid::Uuid,
    split: uuid::Uuid,
    tab: uuid::Uuid,
    base: pigade_layout::TabLayout,
    source: pigade_layout::Rect,
    position: Point<Pixels>,
    start: Point<Pixels>,
    moved: bool,
    travelled: bool,
    drop: Option<(pigade_layout::Command, pigade_layout::Rect)>,
}

struct DirectLaunch {
    tab: uuid::Uuid,
    request: uuid::Uuid,
    provider: String,
}

impl DirectLaunch {
    fn matches(&self, request: Option<uuid::Uuid>) -> bool {
        request == Some(self.request)
    }
    fn target_matches(
        &self,
        selected_tab: Option<uuid::Uuid>,
        tab: uuid::Uuid,
        provider: &str,
    ) -> bool {
        selected_tab == Some(self.tab) && tab == self.tab && provider == self.provider
    }
}

fn same_focus_placement(before: &pigade_storage::Catalog, after: &pigade_storage::Catalog) -> bool {
    if before.selection.project_id != after.selection.project_id
        || before.selection.workspace_id != after.selection.workspace_id
        || before.selection.tab_id != after.selection.tab_id
        || before.selection.window.panel_generations != after.selection.window.panel_generations
    {
        return false;
    }
    let layout = |catalog: &pigade_storage::Catalog| {
        catalog
            .tabs
            .iter()
            .find(|tab| Some(tab.id) == catalog.selection.tab_id)
            .and_then(|tab| serde_json::from_str::<pigade_layout::TabLayout>(&tab.layout_json).ok())
    };
    match (layout(before), layout(after)) {
        (Some(before), Some(after)) => {
            before.root == after.root
                && before.focus_mode == after.focus_mode
                && (!before.focus_mode || before.focused == after.focused)
        }
        _ => false,
    }
}

#[cfg(test)]
mod focus_placement_tests {
    use super::same_focus_placement;
    use pigade_layout::{Node, TabLayout};
    use pigade_storage::{Catalog, LayoutDocument, Selection, Tab};
    use uuid::Uuid;

    #[test]
    fn focus_ack_preserves_local_state_only_for_same_placement_and_generation() {
        let tab = Uuid::from_u128(1);
        let pane = Uuid::from_u128(2);
        let mut before = Catalog {
            projects: vec![],
            workspaces: vec![],
            tabs: vec![Tab {
                id: tab,
                workspace_id: Uuid::from_u128(3),
                name: "tab".into(),
                layout_json: serde_json::to_string(&TabLayout {
                    root: Some(Node::Pane { id: pane }),
                    focused: Some(pane),
                    focus_mode: false,
                })
                .unwrap(),
                active_pane: Some(pane),
                revision: 1,
            }],
            selection: Selection {
                tab_id: Some(tab),
                ..Default::default()
            },
            layout: LayoutDocument::default(),
            layout_templates: Some(vec![]),
            template_launch_schema: Some(1),
            pane_labels: Some(vec![]),
            agent_profiles: None,
            prompts: None,
            tab_close_schema: None,
            engine_options_schema: Some(1),
        };
        let mut after = before.clone();
        after.tabs[0].revision += 1;
        after.layout.revision += 1;
        assert!(same_focus_placement(&before, &after));
        let mut layout: TabLayout = serde_json::from_str(&after.tabs[0].layout_json).unwrap();
        layout.focused = None;
        after.tabs[0].layout_json = serde_json::to_string(&layout).unwrap();
        assert!(same_focus_placement(&before, &after));
        layout.focus_mode = true;
        after.tabs[0].layout_json = serde_json::to_string(&layout).unwrap();
        assert!(!same_focus_placement(&before, &after));
        after = before.clone();
        after
            .selection
            .window
            .panel_generations
            .insert(pane, Uuid::from_u128(4));
        assert!(!same_focus_placement(&before, &after));
        before.selection.tab_id = None;
        assert!(!same_focus_placement(&before, &after));
    }
}

struct CatalogEdit {
    return_dialog: Option<product::Dialog>,
    field_values: Option<Vec<String>>,
    request: uuid::Uuid,
    dialog: Option<uuid::Uuid>,
    return_to: Option<product::DialogKind>,
    notice: String,
}

struct CanvasApp {
    product: product::ProductUi,
    editor_layouts: std::collections::BTreeMap<usize, edit_field::FieldLayout>,
    editor_scroll: std::collections::BTreeMap<usize, Pixels>,
    editor_drag: Option<(uuid::Uuid, usize, usize)>,
    editor_preferred_x: Option<(uuid::Uuid, usize, Pixels)>,
    product_tx: mpsc::SyncSender<Value>,
    product_rx: mpsc::Receiver<Result<protocol::ProductReply, protocol::ProductError>>,
    git_worker: workspace_git::Worker,
    product_launch_unknown: bool,
    catalog_edit: Option<CatalogEdit>,
    prompt_reload: Option<uuid::Uuid>,
    profile_prepare: Option<profile_flow::Pending>,
    profile_reload: Option<(uuid::Uuid, uuid::Uuid)>,
    template_flow: template_flow::RunFlow,
    deferred_product_action: Option<product::ProductAction>,
    pending_launch: Option<(uuid::Uuid, uuid::Uuid)>,
    pending_launch_review: bool,
    pending_direct_launch: Option<DirectLaunch>,
    setup_flow: Option<setup_flow::SetupFlow>,
    picker_request: Option<uuid::Uuid>,
    providers_checked: bool,
    pending_context_inspection: bool,
    pending_conversation_inspection: bool,
    pending_generation_picker: bool,
    pending_context_editor: Option<uuid::Uuid>,
    editor_dialog: Option<uuid::Uuid>,
    layout_geometry: Option<pigade_layout::CanvasGeometry>,
    projection_input: Option<(Bounds<Pixels>, Size<Pixels>, f32)>,
    canvas_bounds: Option<Bounds<Pixels>>,
    canvas_bars: Vec<viewport::Scrollbar>,
    canvas_drag: Option<CanvasScrollDrag>,
    canvas_offsets: std::collections::BTreeMap<uuid::Uuid, pigade_storage::CanvasOffset>,
    canvas_dirty: std::collections::BTreeSet<uuid::Uuid>,
    canvas_save_at: Option<Instant>,
    canvas_saving: Option<(uuid::Uuid, pigade_storage::CanvasOffset)>,
    canvas_close_requested: bool,
    canvas_tab: Option<uuid::Uuid>,
    canvas_focus_mode: bool,
    canvas_focus_offset: pigade_storage::CanvasOffset,
    canvas_layout_revision: Option<u64>,
    canvas_reveal: Option<uuid::Uuid>,
    layout_mode: Option<LayoutMode>,
    layout_preview: Option<(uuid::Uuid, pigade_layout::TabLayout)>,
    resize_pending: Option<pigade_layout::Command>,
    resize_gesture: Option<ResizeGesture>,
    pane_drag: Option<PaneDrag>,
    focus_queue: pointer_layout::FocusQueue,
    hover_position: Option<Point<Pixels>>,
    navigation: navigation::Navigation,
    stop_control: session_control::StopControl,
    git_flow: workspace_git::Flow,
    deferred_placement: Option<pigade_layout::Command>,
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
            product_tx,
            product_rx,
            git_worker,
        } = connection;
        let ui_trace = trace.clone();
        let focus = cx.focus_handle();
        let close_target = cx.entity().downgrade();
        window.on_window_should_close(cx, move |_, cx| {
            close_target
                .update(cx, |this, cx| {
                    if this.canvas_dirty.is_empty() && this.canvas_saving.is_none() {
                        return true;
                    }
                    this.canvas_close_requested = true;
                    this.canvas_drag = None;
                    this.canvas_save_at = Some(Instant::now());
                    this.flush_canvas_offset(cx);
                    cx.notify();
                    false
                })
                .unwrap_or(true)
        });
        let input_subscriptions = vec![
            cx.on_blur(&focus, window, |this, window, cx| {
                this.cancel_pane_drag(cx);
                this.finish_resize(false, cx);
                this.canvas_drag = None;
                this.selection_dragging = false;
                this.editor_drag = None;
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
                    this.cancel_pane_drag(cx);
                    this.finish_resize(false, cx);
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
                        this.service_workspace_git(cx);
                        while let Ok(reply) = this.product_rx.try_recv() {
                            let stop_pending=this.stop_control.take_reply(match &reply {Ok(reply)=>reply.ui_request,Err(error)=>error.ui_request});
                            let mut navigation_pending=this.navigation.take_reply(match &reply {Ok(reply)=>reply.ui_request,Err(error)=>error.ui_request});
                            let template_pending=this.template_flow.take_reply(match &reply {Ok(reply)=>reply.ui_request,Err(error)=>error.ui_request});
                            let focus_reply = this.focus_queue.matches(match &reply { Ok(reply)=>reply.ui_request, Err(error)=>error.ui_request });
                            let provider_reply = match &reply {
                                Ok(reply) => reply.providers_request,
                                Err(error) => error.providers_request,
                            };
                            if !provider_reply { this.product.busy = false; }
                            match reply {
                                Ok(reply) => {
                                    let ui_request = reply.ui_request;
                                    if let Some(pending)=stop_pending { this.receive_stop(pending,reply.process_view,cx); }
                                    if let Some(providers) = reply.providers {
                                        this.product.providers = Some(providers);
                                        this.product.provider_error = None;
                                    }
                                    if reply.history_view_request {
                                        this.apply_history_reply(reply.history_view.clone(), cx);
                                    }
                                    if let Some(selection) = reply.selection {
                                        let generation_changed = this.product.catalog.as_ref().is_some_and(|catalog| catalog.selection.window.panel_generations != selection.window.panel_generations);
                                        if let Some(catalog) = &mut this.product.catalog
                                            && selection.revision >= catalog.selection.revision {
                                            catalog.selection = selection;
                                        }
                                        this.canvas_saving = None;
                                        if generation_changed || reply.generation_selected {
                                            this.sync_product_selection(window,cx);
                                        }
                                        if reply.generation_selected && this.product.dialog.as_ref().is_some_and(|dialog| dialog.kind == product::DialogKind::Generations) {
                                            this.product.dialog = None;
                                            this.product.notice = Some("Выбор поколения сохранён".into());
                                        }
                                        // A click/focus key can arrive during the
                                        // short background save. Persist that
                                        // local focus once the writer is free.
                                        this.persist_focus(cx);
                                    }
                                    if reply.launch_confirmed {
                                        this.product_launch_unknown = false;
                                        this.pending_launch = None;
                                    }
                                    if let Some(catalog) = reply.catalog {
                                        let close_reply=this.product.tab_close_sheet.as_ref().is_some_and(|sheet|sheet.request.is_some() && sheet.request==ui_request);
                                        let prompt_refresh=this.prompt_reload.is_some_and(|request|Some(request)==ui_request);
                                        if prompt_refresh { this.prompt_reload=None; }
                                        let profile_refresh=this.profile_reload.as_ref().is_some_and(|(request,_)|Some(*request)==ui_request);
                                        if profile_refresh { this.profile_reload=None; }
                                        let edit = if this.catalog_edit.as_ref().is_some_and(|edit|Some(edit.request)==ui_request) {this.catalog_edit.take()} else {None};
                                        let keep_local = (focus_reply || edit.is_some()) && this.product.catalog.as_ref().is_some_and(|previous| same_focus_placement(previous,&catalog));
                                        let same_dialog=edit.as_ref().is_some_and(|edit|edit.dialog == this.product.dialog.as_ref().map(|dialog|dialog.id));
                                        let current_dialog=this.product.dialog.clone();
                                        if focus_reply {
                                            let saved=catalog.selection.tab_id.and_then(|tab|catalog.tabs.iter().find(|value|value.id==tab).and_then(|value|serde_json::from_str::<pigade_layout::TabLayout>(&value.layout_json).ok()).and_then(|layout|layout.focused.map(|pane|pointer_layout::FocusIntent{tab,pane})));
                                            this.focus_queue.complete(reply.ui_request.expect("matching focus request"),saved);
                                        }
                                        this.product.catalog = Some(catalog);
                                        if !keep_local {
                                            this.product.notice = None;
                                            this.product.dialog = None;
                                            this.sync_product_selection(window, cx);
                                        }
                                        if let Some(edit)=edit {
                                            let is_prompt=current_dialog.as_ref().is_some_and(|dialog|dialog.kind==product::DialogKind::Prompt);
                                            let changed = same_dialog && edit.field_values.as_ref().is_some_and(|saved|current_dialog.as_ref().is_some_and(|dialog|dialog.fields.iter().map(|buffer|buffer.text().to_owned()).collect::<Vec<_>>()!=*saved));
                                            if same_dialog && is_prompt && let Some(editor)=&mut this.product.prompt_editor {
                                                editor.saved=this.product.catalog.as_ref().and_then(|catalog|catalog.prompts.as_ref()).and_then(|prompts|prompts.iter().find(|prompt|prompt.id==editor.id)).cloned();
                                            }
                                            if same_dialog && !is_prompt && edit.field_values.is_some() && let Some(editor)=&mut this.product.profile_editor {
                                                editor.saved=this.product.catalog.as_ref().and_then(|catalog|catalog.agent_profiles.as_ref()).and_then(|profiles|profiles.iter().find(|profile|profile.id==editor.id)).cloned();
                                            }
                                            this.product.notice=Some(if changed {"Сохранено; новые правки ещё не сохранены".into()} else {edit.notice});
                                            this.product.dialog=if same_dialog && !changed {edit.return_dialog.or_else(||edit.return_to.map(product::Dialog::new))} else {current_dialog};
                                        } else if template_pending.is_some() || navigation_pending.is_some() || profile_refresh || prompt_refresh || close_reply {
                                            this.product.dialog=current_dialog;
                                        }
                                        this.receive_tab_close(ui_request);
                                    }
                                    if let (Some(run),Some(pending))=(reply.template_run,template_pending) {
                                        this.receive_template_run(run,pending,cx);
                                    }
                                    if let (Some(page),Some(pending))=(reply.template_runs,template_pending) {
                                        this.receive_template_list(page,pending,cx);
                                    }
                                    if let Some(pending)=navigation_pending.take() {
                                        this.receive_navigation(pending,reply.panel_directory,cx);
                                    }
                                    if let Some(sessions) = reply.sessions {
                                        if this.pending_launch.is_some_and(|(session,generation)| sessions.iter().any(|view|
                                            view.session == session.to_string() && view.generation == generation.to_string()
                                            && matches!(view.state.as_str(),"running"|"exited"|"history"))) {
                                            this.product_launch_unknown = false;
                                            this.pending_launch = None;
                                            this.product.notice = Some("Предыдущая сессия найдена; её можно вернуть во вкладку".into());
                                        }
                                        this.product.replace_sessions(sessions.into_iter().map(Into::into).collect());
                                    }
                                    let profile_preview_ok=reply.launch_preview.as_ref().is_none_or(|preview|this.accept_profile_preview(ui_request,preview,reply.launch_confirmed,reply.launch_phase.as_deref()));
                                    if let Some(preview) = reply.launch_preview.filter(|_|profile_preview_ok) {
                                        let direct_request = this.pending_direct_launch.as_ref().is_some_and(|pending| pending.matches(ui_request));
                                        let direct_start = this.pending_direct_launch.as_ref().is_some_and(|pending|
                                            pending.matches(ui_request) && pending.target_matches(this.product.catalog.as_ref().and_then(|catalog| catalog.selection.tab_id), preview.tab_id, &preview.provider)
                                            && preview.resume.is_none() && !preview.deliver_context && !preview.include_memory
                                            && preview.role_override.is_empty() && preview.agent_instructions.is_empty()
                                            && preview.can_launch && reply.launch_phase.is_none());
                                        let direct_error = preview.unavailable_reason.clone().unwrap_or_else(|| "Запуск не подтверждён: параметры или выбранная вкладка изменились".into());
                                        if direct_request { this.pending_direct_launch = None; }
                                        this.product.launch_phase = reply.launch_phase;
                                        if reply.launch_confirmed {
                                            if this.product.dialog.as_ref().is_some_and(|dialog| dialog.kind == product::DialogKind::LaunchReview) {
                                                this.product.dialog = None;
                                            }
                                            this.product.notice = Some(if preview.resume.is_some() {
                                                "Запрошено продолжение конкретного диалога. Результат виден в CLI; прежняя история сохранена."
                                            } else if this.product.launch_phase.as_deref() == Some("delivered_unconfirmed") {
                                                "CLI запущен с контекстом. Применение моделью не подтверждено."
                                            } else { "CLI запущен без передачи контекста." }.into());
                                        } else if (this.product.launch_phase.is_some() && this.pending_context_inspection)
                                            || (this.product.launch_phase.is_none() && (this.pending_launch_review || this.product.dialog.as_ref().is_some_and(|dialog| dialog.kind == product::DialogKind::LaunchReview))) {
                                            let kind = if this.product.launch_phase.is_some() { if this.pending_conversation_inspection { product::DialogKind::Conversation } else { product::DialogKind::SessionContext } } else if preview.resume.is_some() { product::DialogKind::ResumeReview } else {product::DialogKind::LaunchReview};
                                            let mut dialog = this.product.dialog.as_ref().filter(|dialog| dialog.kind == kind).cloned().unwrap_or_else(|| product::Dialog::new(kind));
                                            if kind == product::DialogKind::LaunchReview && this.product.dialog.as_ref().is_none_or(|existing| existing.kind != kind) { dialog.fields = vec![preview.role_override.clone().into(),preview.agent_instructions.clone().into(),preview.memory.as_ref().map(|memory| memory_choice::format_sections(&memory.selected)).unwrap_or_default().into(),preview.engine.model.clone().unwrap_or_default().into(),preview.engine.reasoning_effort.clone().unwrap_or_default().into()]; }
                                            if kind == product::DialogKind::Conversation && this.product.dialog.as_ref().is_none_or(|existing| existing.kind != kind) { dialog.fields = vec![preview.conversation.as_ref().map(|binding| binding.conversation_id.clone()).unwrap_or_default().into()]; }
                                            if kind == product::DialogKind::Conversation && !matches!(preview.provider.as_str(),"codex"|"claude"|"opencode") { dialog.fields.clear(); }
                                            this.product.dialog = Some(dialog);
                                        }
                                        this.pending_launch_review = false;
                                        this.pending_context_inspection = false;
                                        this.pending_conversation_inspection = false;
                                        this.product.launch_preview = Some(preview);
                                        if direct_request {
                                            if direct_start {this.confirm_prepared_launch(cx);} else {this.product.notice=Some(direct_error);}
                                            // The ordinary creation path never exposes the advanced review.
                                            this.product.dialog = None;
                                        }
                                    }
                                    if let Some(generations) = reply.generations {
                                        if this.pending_generation_picker {
                                            this.product.generation_page = Some(generations);
                                            this.product.dialog = Some(product::Dialog::new(product::DialogKind::Generations));
                                        }
                                        this.pending_generation_picker = false;
                                    }
                                    if let Some(context) = reply.context {
                                        this.product.apply_context_reply(context.clone(),reply.context_saved,this.pending_context_editor == Some(context.scope_id));
                                        this.pending_context_editor = None;
                                    }
                                    this.advance_project_setup(ui_request,cx);
                                    if !this.providers_checked && this.setup_flow.is_none()
                                        && this.product.catalog.as_ref().is_some_and(|catalog|catalog.selection.tab_id.is_some()) {
                                        this.detect_providers(cx);
                                    }
                                }
                                Err(error) => {
                                    if let Some(pending)=stop_pending {this.stop_failed(pending,error.message,cx);continue;}
                                    this.tab_close_failed(error.ui_request);
                                    let template_error=error.message.clone();
                                    let navigation_error=error.message.clone();
                                    if focus_reply {
                                        this.focus_queue.cancel();
                                        this.focus_queue.complete(error.ui_request.expect("matching focus error"),None);
                                        this.deferred_placement=None;
                                        this.deferred_product_action=None;
                                    }
                                    if this.prompt_reload.is_some_and(|request|Some(request)==error.ui_request) {this.prompt_reload=None;}
                                    if this.profile_reload.as_ref().is_some_and(|(request,_)|Some(*request)==error.ui_request) {this.profile_reload=None;}
                                    if this.profile_prepare.as_ref().is_some_and(|pending|Some(pending.request)==error.ui_request) {this.profile_prepare=None;}
                                    if this.catalog_edit.as_ref().is_some_and(|edit|Some(edit.request)==error.ui_request) {this.catalog_edit=None;}
                                    if this.setup_flow.as_ref().is_some_and(|flow| flow.matches(error.ui_request)) { this.setup_flow = None; }
                                    if this.pending_direct_launch.as_ref().is_some_and(|pending| pending.matches(error.ui_request)) { this.pending_direct_launch=None; }
                                    if error.providers_request { this.product.provider_error=Some("Не удалось проверить CLI. Терминал можно открыть сразу; проверку агентов можно повторить.".into()); }
                                    if this.product.history_sheet.as_ref().is_some_and(|history| history.pending.is_some()) {
                                        this.apply_history_error(error.message.clone(), cx);
                                    }
                                    if let Some((tab, _)) = this.canvas_saving.take() {
                                        this.canvas_dirty.insert(tab);
                                        this.canvas_save_at = None;
                                    }
                                    this.pending_launch_review = false;
                                    this.pending_context_inspection = false;
                                    this.pending_conversation_inspection = false;
                                    this.pending_context_editor = None;
                                    this.pending_generation_picker = false;
                                    if error.rejected_before_launch {
                                        this.product_launch_unknown = false;
                                        this.pending_launch = None;
                                    }
                                    this.product.notice = Some(if error.providers_request {
                                        this.product.provider_error.clone().unwrap_or(error.message)
                                    } else { error.message });
                                    if let Some(pending)=template_pending {this.template_request_failed(pending,template_error,cx);}
                                    if let Some(pending)=navigation_pending {this.navigation_failed(pending,navigation_error,cx);}
                                }
                            }
                            cx.notify();
                        }
                        // Drain worker receipts before state/revision work: a
                        // worker blocked on this bounded channel must not wait
                        // for a State revision to change.
                        let previous_ack = acknowledgement_presentation(
                            this.selected_notice(),
                            &this.mouse_delivery,
                        );
                        while let Ok(receipt) = this.receipts.try_recv() {
                            this.apply_worker_receipt(receipt);
                        }
                        if previous_ack
                            != acknowledgement_presentation(
                                this.selected_notice(),
                                &this.mouse_delivery,
                            )
                        {
                            cx.notify();
                        }
                        if this.pane_drag.as_ref().is_some_and(|drag| drag.moved && drag.base.focus_mode) {
                            this.sync_panel_visibility();
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
                            this.filter_product_views();
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
                        this.sync_editor_scope(window,cx);
                        this.flush_focus(cx);
                        if !this.product.busy && !this.focus_queue.is_saving() && this.focus_queue.pending().is_none()
                            && let Some(action)=this.deferred_product_action.take() {
                            this.product_action(action,window,cx);
                        }
                        this.flush_canvas_offset(cx);
                        this.service_panel_search(cx);
                        this.service_stops(cx);
                        if this.canvas_close_requested && this.canvas_dirty.is_empty() && this.canvas_saving.is_none() {
                            window.remove_window();
                            return;
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
        let mut product = match controls::load() {
            Ok(command_modifier) => product::ProductUi {
                command_modifier,
                ..Default::default()
            },
            Err(error) => product::ProductUi {
                command_modifier: controls::Modifier::Disabled,
                notice: Some(format!("Не удалось загрузить клавишу управления: {error}")),
                ..Default::default()
            },
        };
        match appearance::load() {
            Ok(config) => product.appearance = config,
            Err(error) => {
                product.appearance_error =
                    Some(format!("Не удалось загрузить внешний вид: {error}"))
            }
        }
        product.motion_forced = std::env::args()
            .skip(2)
            .any(|arg| arg == "--reduced-motion");
        cx.set_reduce_motion(product.appearance.reduced_motion || product.motion_forced);
        Self {
            product,
            editor_layouts: Default::default(),
            editor_scroll: Default::default(),
            editor_drag: None,
            editor_preferred_x: None,
            product_tx,
            product_rx,
            git_worker,
            git_flow: Default::default(),
            product_launch_unknown: false,
            catalog_edit: None,
            prompt_reload: None,
            profile_prepare: None,
            profile_reload: None,
            template_flow: Default::default(),
            deferred_product_action: None,
            pending_launch: None,
            pending_launch_review: false,
            pending_direct_launch: None,
            setup_flow: None,
            picker_request: None,
            providers_checked: false,
            pending_context_inspection: false,
            pending_conversation_inspection: false,
            pending_generation_picker: false,
            pending_context_editor: None,
            editor_dialog: None,
            layout_geometry: None,
            projection_input: None,
            canvas_bounds: None,
            canvas_bars: Vec::new(),
            canvas_drag: None,
            canvas_offsets: Default::default(),
            canvas_dirty: Default::default(),
            canvas_save_at: None,
            canvas_saving: None,
            canvas_close_requested: false,
            canvas_tab: None,
            canvas_focus_mode: false,
            canvas_focus_offset: Default::default(),
            canvas_layout_revision: None,
            canvas_reveal: None,
            layout_mode: None,
            layout_preview: None,
            resize_pending: None,
            resize_gesture: None,
            pane_drag: None,
            focus_queue: Default::default(),
            hover_position: None,
            navigation: Default::default(),
            stop_control: Default::default(),
            deferred_placement: None,
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
    fn product_command(&mut self, command: Value, cx: &mut Context<Self>) {
        self.try_product_command(command, cx);
    }
    fn try_product_command(&mut self, command: Value, cx: &mut Context<Self>) -> bool {
        if self.product.busy {
            self.product.notice = Some("Дождитесь завершения текущей операции".into());
            cx.notify();
            return false;
        }
        let admitted = match self.product_tx.try_send(command) {
            Ok(()) => {
                self.product.busy = true;
                self.product.notice = None;
                true
            }
            Err(_) => {
                self.product.notice = Some("Операция не принята: очередь занята".into());
                false
            }
        };
        cx.notify();
        admitted
    }

    fn edit_catalog(
        &mut self,
        batch: pigade_storage::CatalogBatch,
        notice: &str,
        return_to: Option<product::DialogKind>,
        cx: &mut Context<Self>,
    ) {
        let request = uuid::Uuid::new_v4();
        if self.try_product_command(
            json!({"command":"catalog_apply","batch":batch,"_ui_request":request}),
            cx,
        ) {
            self.catalog_edit = Some(CatalogEdit {
                return_dialog: self
                    .product
                    .dialog
                    .as_ref()
                    .filter(|dialog| dialog.kind == product::DialogKind::Prompt)
                    .and_then(|_| {
                        self.product
                            .prompt_editor
                            .as_ref()
                            .and_then(|editor| editor.origin.clone())
                    })
                    .or_else(|| {
                        self.product
                            .dialog
                            .as_ref()
                            .filter(|dialog| dialog.kind == product::DialogKind::AgentProfile)
                            .and_then(|_| {
                                self.product
                                    .profile_editor
                                    .as_ref()
                                    .and_then(|editor| editor.origin.clone())
                            })
                    }),
                field_values: self
                    .product
                    .dialog
                    .as_ref()
                    .filter(|dialog| {
                        matches!(
                            dialog.kind,
                            product::DialogKind::AgentProfile | product::DialogKind::Prompt
                        )
                    })
                    .map(|dialog| {
                        dialog
                            .fields
                            .iter()
                            .map(|buffer| buffer.text().to_owned())
                            .collect()
                    }),
                request,
                dialog: self.product.dialog.as_ref().map(|dialog| dialog.id),
                return_to,
                notice: notice.into(),
            });
        }
    }

    fn begin_project_setup(&mut self, mut command: Value, cx: &mut Context<Self>) {
        if self.product.busy {
            return;
        }
        let request = uuid::Uuid::new_v4();
        command["_ui_request"] = json!(request);
        if self.try_product_command(command, cx) {
            self.setup_flow = Some(setup_flow::SetupFlow::new(request));
            self.product.notice = Some("Открываем проект…".into());
        }
    }

    fn advance_project_setup(&mut self, request: Option<uuid::Uuid>, cx: &mut Context<Self>) {
        let Some(mut flow) = self.setup_flow.take() else {
            return;
        };
        let Some(catalog) = self.product.catalog.as_ref() else {
            self.setup_flow = Some(flow);
            return;
        };
        match flow.accept(request, catalog) {
            Ok(setup_flow::Next::Ignore) => self.setup_flow = Some(flow),
            Ok(setup_flow::Next::Workspace { project, request }) => {
                if self.try_product_command(json!({"command":"create_workspace","project":project,"task":"Основной","worktree":false,"_ui_request":request}),cx) {
                    self.setup_flow = Some(flow);
                    self.product.notice = Some("Создаём основной воркспейс в папке проекта…".into());
                }
            }
            Ok(setup_flow::Next::Ready) => {
                self.product.notice = Some("Проект открыт. Выберите агента или откройте терминал.".into());
                self.detect_providers(cx);
            }
            Err(error) => self.product.notice = Some(error),
        }
    }

    fn detect_providers(&mut self, cx: &mut Context<Self>) {
        self.product.provider_error = None;
        match self
            .product_tx
            .try_send(json!({"command":"detect_providers"}))
        {
            Ok(()) => self.providers_checked = true,
            Err(_) => self.product.provider_error = Some(
                "Проверка CLI не началась: очередь занята. Откройте выбор агента, чтобы повторить."
                    .into(),
            ),
        }
        cx.notify();
    }

    fn pick_project_folder(&mut self, parent: bool, cx: &mut Context<Self>) {
        if self.picker_request.is_some() || self.product.busy {
            return;
        }
        let dialog = parent
            .then(|| self.product.dialog.as_ref().map(|dialog| dialog.id))
            .flatten();
        if parent && dialog.is_none() {
            return;
        }
        let token = uuid::Uuid::new_v4();
        self.picker_request = Some(token);
        let picker = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some(
                if parent {
                    "Выберите родительскую папку"
                } else {
                    "Откройте папку Git-проекта"
                }
                .into(),
            ),
        });
        cx.spawn(async move |weak, cx| {
            let result = picker.await;
            let _ = weak.update_in(cx, |this, window, cx| {
                if this.picker_request != Some(token) {
                    return;
                }
                this.picker_request = None;
                match result {
                    Ok(Ok(Some(paths))) if paths.len() == 1 => {
                        let path = &paths[0];
                        let Some(path) = path.to_str() else {
                            this.product.notice =
                                Some("Путь должен содержать корректный Unicode".into());
                            cx.notify();
                            return;
                        };
                        if parent {
                            if let Some(current) = &mut this.product.dialog
                                && Some(current.id) == dialog
                                && current.kind == product::DialogKind::CreateProject
                            {
                                current.fields[1] = path.into();
                                this.editor_layouts.remove(&1);
                            }
                        } else {
                            this.begin_project_setup(
                                json!({"command":"add_project","path":path}),
                                cx,
                            );
                        }
                    }
                    Ok(Ok(None)) => (),
                    Ok(Ok(Some(_))) => {
                        this.product.notice = Some("Выберите одну папку проекта".into())
                    }
                    _ => {
                        if !parent {
                            this.product.dialog =
                                Some(product::Dialog::new(product::DialogKind::AddProject));
                        }
                        this.product.notice = Some(
                            "Не удалось открыть системный выбор папки. Можно ввести путь вручную."
                                .into(),
                        );
                    }
                }
                this.sync_editor_scope(window, cx);
                cx.notify();
            });
        })
        .detach();
    }

    fn issue_history_query(
        &mut self,
        query: protocol::HistoryViewQuery,
        pending: history_view::Pending,
        cx: &mut Context<Self>,
    ) {
        if self.product.busy {
            self.product.notice = Some("Подождите завершения запроса истории".into());
            cx.notify();
            return;
        }
        let Some(history) = &mut self.product.history_sheet else {
            return;
        };
        history.pending = Some(pending);
        history.error = None;
        let command = serde_json::to_value(query)
            .map(|query| json!({"command":"history_view","query":query}));
        match command {
            Ok(command) => self.product_command(command, cx),
            Err(error) => {
                if let Some(history) = &mut self.product.history_sheet {
                    history.pending = None;
                    history.error = Some(format!("Запрос истории не собран: {error}"));
                }
                cx.notify();
            }
        }
    }

    fn open_history(&mut self, cx: &mut Context<Self>) {
        if self.product.busy {
            self.product.notice =
                Some("Дождитесь завершения операции перед открытием истории".into());
            return;
        }
        let Some(view) = self.state.views.get(self.selected) else {
            self.product.notice = Some("Сначала выберите терминальную панель".into());
            cx.notify();
            return;
        };
        let (Ok(session), Ok(generation)) = (
            view.session.parse::<uuid::Uuid>(),
            view.generation.parse::<uuid::Uuid>(),
        ) else {
            self.product.notice = Some("У выбранной панели нет точной идентичности".into());
            cx.notify();
            return;
        };
        let dialog = product::Dialog::new(product::DialogKind::History);
        let dialog_id = dialog.id;
        self.product.dialog = Some(dialog);
        self.product.history_sheet = Some(history_view::HistorySheet::new(
            dialog_id, session, generation,
        ));
        self.issue_history_query(
            protocol::HistoryViewQuery::Open {
                session,
                generation,
            },
            history_view::Pending::Open,
            cx,
        );
    }

    fn history_open_snapshot(&mut self, cx: &mut Context<Self>) {
        let Some(history) = &mut self.product.history_sheet else {
            return;
        };
        history.closed = false;
        history.status = "Обновление снимка истории…".into();
        history.page = None;
        history.back.clear();
        let session = history.session;
        let generation = history.generation;
        self.issue_history_query(
            protocol::HistoryViewQuery::Open {
                session,
                generation,
            },
            history_view::Pending::Open,
            cx,
        );
    }

    fn history_page(
        &mut self,
        cursor: protocol::HistoryCursor,
        remember: bool,
        cx: &mut Context<Self>,
    ) {
        if self.product.busy {
            return;
        }
        let Some(history) = &mut self.product.history_sheet else {
            return;
        };
        let Some(snapshot) = history.snapshot else {
            return;
        };
        if remember && let Some(page) = &history.page {
            history.remember_page(page.start);
        }
        self.issue_history_query(
            protocol::HistoryViewQuery::Page {
                snapshot,
                start: cursor,
            },
            history_view::Pending::Page { start: cursor },
            cx,
        );
    }

    fn history_search(
        &mut self,
        continuation: Option<protocol::HistoryCursor>,
        cx: &mut Context<Self>,
    ) {
        let text = self
            .product
            .dialog
            .as_ref()
            .filter(|dialog| dialog.kind == product::DialogKind::History)
            .and_then(|dialog| dialog.fields.first())
            .map(|field| field.text().to_owned())
            .unwrap_or_default();
        if text.is_empty() || text.len() > history_view::SEARCH_TEXT_LIMIT {
            if let Some(history) = &mut self.product.history_sheet {
                history.error = Some(if text.is_empty() {
                    "Введите текст для поиска".into()
                } else {
                    "Запрос поиска не может превышать 512 байт UTF-8".into()
                });
            }
            cx.notify();
            return;
        }
        let Some(snapshot) = self
            .product
            .history_sheet
            .as_ref()
            .and_then(|history| history.snapshot)
        else {
            return;
        };
        if let Some(history) = &mut self.product.history_sheet {
            history.status = "Поиск…".into();
            history.error = None;
            if let Some(dialog) = &mut self.product.dialog {
                dialog.active_field = 0;
            }
            history.match_at = None;
            if continuation.is_none() {
                history.search_next = None;
            }
        }
        self.issue_history_query(
            protocol::HistoryViewQuery::Search {
                snapshot,
                text: text.clone(),
                cursor: continuation,
            },
            history_view::Pending::Search {
                text,
                cursor: continuation,
            },
            cx,
        );
    }

    fn close_history(&mut self, refresh: bool, cx: &mut Context<Self>) {
        let Some(history) = &mut self.product.history_sheet else {
            self.product.dialog = None;
            return;
        };
        history.closed = !refresh;
        if !refresh {
            self.product.dialog = None;
        }
        if self.product.busy {
            if let Some(history) = &mut self.product.history_sheet {
                history.status = "Закрывается после текущего запроса…".into();
            }
            cx.notify();
            return;
        }
        let Some(snapshot) = history.snapshot else {
            if refresh {
                self.history_open_snapshot(cx);
            } else {
                self.product.history_sheet = None;
            }
            return;
        };
        self.issue_history_query(
            protocol::HistoryViewQuery::Close { snapshot },
            history_view::Pending::Close { refresh },
            cx,
        );
        cx.notify();
    }

    fn close_history_snapshot(&mut self, snapshot: uuid::Uuid, cx: &mut Context<Self>) {
        if let Some(history) = &mut self.product.history_sheet {
            history.pending = Some(history_view::Pending::Close { refresh: false });
        }
        self.product_command(
            json!({"command":"history_view","query":{"action":"close","snapshot":snapshot}}),
            cx,
        );
    }

    fn history_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let key = &event.keystroke;
        let control = key.modifiers.control || key.modifiers.platform;
        let shift = key.modifiers.shift;
        let Some(history) = self.product.history_sheet.as_ref() else {
            return;
        };
        let current = history.page.as_ref();
        let previous = history
            .back
            .last()
            .copied()
            .or_else(|| current.and_then(|page| page.previous));
        let next = current.and_then(|page| page.next);
        let start = current.map(|_| protocol::HistoryCursor { row: 0, offset: 0 });
        let end = current.map(|page| protocol::HistoryCursor {
            row: page.total_rows.saturating_sub(128),
            offset: 0,
        });
        match key.key.as_str() {
            "escape" => self.close_history(false, cx),
            "tab" => {
                if let Some(dialog) = &mut self.product.dialog {
                    dialog.fields[dialog.active_field].cancel_mark();
                    dialog.active_field = 0;
                }
            }
            "left" => {
                if let Some(buffer) = self.product.selection_editor_mut() {
                    buffer.navigate(
                        if control {
                            text_edit::Movement::WordLeft
                        } else {
                            text_edit::Movement::Left
                        },
                        shift,
                    );
                }
            }
            "right" => {
                if let Some(buffer) = self.product.selection_editor_mut() {
                    buffer.navigate(
                        if control {
                            text_edit::Movement::WordRight
                        } else {
                            text_edit::Movement::Right
                        },
                        shift,
                    );
                }
            }
            "home" => {
                if let Some(buffer) = self.product.selection_editor_mut() {
                    buffer.navigate(
                        if control {
                            text_edit::Movement::DocumentStart
                        } else {
                            text_edit::Movement::LineStart
                        },
                        shift,
                    );
                }
            }
            "end" => {
                if let Some(buffer) = self.product.selection_editor_mut() {
                    buffer.navigate(
                        if control {
                            text_edit::Movement::DocumentEnd
                        } else {
                            text_edit::Movement::LineEnd
                        },
                        shift,
                    );
                }
            }
            "up" => {
                if let Some(buffer) = self.product.selection_editor_mut() {
                    buffer.navigate(text_edit::Movement::Up, shift);
                }
            }
            "down" => {
                if let Some(buffer) = self.product.selection_editor_mut() {
                    buffer.navigate(text_edit::Movement::Down, shift);
                }
            }
            "a" if control => {
                if let Some(buffer) = self.product.selection_editor_mut() {
                    buffer.select_all();
                }
            }
            "c" if control => {
                let text = self
                    .product
                    .selection_editor_mut()
                    .map(|buffer| {
                        if buffer.selected_text().is_empty() {
                            buffer.text().to_owned()
                        } else {
                            buffer.selected_text().to_owned()
                        }
                    })
                    .unwrap_or_default();
                if !text.is_empty() {
                    cx.write_to_clipboard(ClipboardItem::new_string(text));
                }
            }
            "pageup" if control => {
                if let Some(cursor) = start {
                    self.history_page(cursor, true, cx);
                }
            }
            "pagedown" if control => {
                if let Some(cursor) = end {
                    self.history_page(cursor, true, cx);
                }
            }
            "pageup" if !self.product.busy => {
                if let Some(cursor) = previous {
                    if let Some(history) = &mut self.product.history_sheet {
                        history.back.pop();
                    }
                    self.history_page(cursor, false, cx);
                }
            }
            "pagedown" => {
                if let Some(cursor) = next {
                    self.history_page(cursor, true, cx);
                }
            }
            "v" | "x" | "z" | "y" if control => (),
            _ => (),
        }
        self.editor_changed(window, cx);
        cx.stop_propagation();
    }

    fn apply_history_reply(
        &mut self,
        reply: Option<protocol::HistoryViewReply>,
        cx: &mut Context<Self>,
    ) {
        let pending = self
            .product
            .history_sheet
            .as_mut()
            .and_then(|history| history.pending.take());
        let Some(pending) = pending else {
            if let Some(protocol::HistoryViewReply::Page { page }) = reply {
                if let Some(history) = &mut self.product.history_sheet {
                    history.status = "Пришёл устаревший ответ; снимок закрывается".into();
                }
                self.close_history_snapshot(page.snapshot, cx);
            }
            return;
        };
        let is_current = self.product.dialog.as_ref().is_some_and(|dialog| {
            dialog.kind == product::DialogKind::History
                && self
                    .product
                    .history_sheet
                    .as_ref()
                    .is_some_and(|history| dialog.id == history.dialog_id && !history.closed)
        });
        match reply {
            Some(protocol::HistoryViewReply::Page { page }) => {
                if !matches!(
                    &pending,
                    history_view::Pending::Open | history_view::Pending::Page { .. }
                ) {
                    if let Some(history) = &mut self.product.history_sheet {
                        history.error = Some("Неожиданный ответ страницы истории".into());
                    }
                    cx.notify();
                    return;
                }
                if let Err(error) = history_view::validate_page(&page) {
                    if let Some(history) = &mut self.product.history_sheet {
                        history.error = Some(error);
                    }
                    cx.notify();
                    return;
                }
                let Some(current_history) = self.product.history_sheet.as_ref() else {
                    return;
                };
                let (session, generation) = (current_history.session, current_history.generation);
                let expected_snapshot = current_history.snapshot;
                let expected_sequence = current_history.sequence;
                let valid_identity = page.session == session && page.generation == generation;
                let valid_snapshot = match &pending {
                    history_view::Pending::Open => expected_snapshot.is_none(),
                    _ => expected_snapshot == Some(page.snapshot),
                };
                let valid_cursor = match &pending {
                    history_view::Pending::Page { start } => page.start == *start,
                    _ => true,
                };
                let valid_sequence = matches!(&pending, history_view::Pending::Open)
                    || expected_sequence == Some(page.sequence);
                if !valid_identity || !valid_snapshot || !valid_cursor || !valid_sequence {
                    if let Some(history) = &mut self.product.history_sheet {
                        history.error =
                            Some("Ответ истории не совпадает с ожидаемым снимком".into());
                    }
                    self.close_history_snapshot(page.snapshot, cx);
                    cx.notify();
                    return;
                }
                if !is_current
                    || self
                        .product
                        .history_sheet
                        .as_ref()
                        .is_some_and(|h| h.closed)
                {
                    self.close_history_snapshot(page.snapshot, cx);
                    return;
                }
                let text = history_view::page_plain_text(&page);
                let query_len = self
                    .product
                    .dialog
                    .as_ref()
                    .and_then(|dialog| dialog.fields.first())
                    .map(|field| field.text().len())
                    .unwrap_or(0);
                let should_select_match = self
                    .product
                    .history_sheet
                    .as_ref()
                    .is_some_and(|history| history.match_at == Some(page.start));
                // A new page installs a fresh buffer, whose local revision may
                // equal the previous buffer's. Do not reuse shaped text from
                // another page or preserve its scroll/drag coordinates.
                self.editor_layouts.remove(&1);
                self.editor_scroll.remove(&1);
                self.editor_drag = None;
                self.editor_preferred_x = None;
                if let Some(dialog) = self
                    .product
                    .dialog
                    .as_mut()
                    .filter(|dialog| dialog.id == current_history.dialog_id)
                {
                    dialog.fields[1] = text.into();
                    dialog.active_field = 0;
                    if should_select_match {
                        let end = query_len.min(dialog.fields[1].text().len());
                        dialog.fields[1].select_bytes(0..end, false);
                        dialog.active_field = 1;
                    }
                }
                if let Some(history) = &mut self.product.history_sheet {
                    history.snapshot = Some(page.snapshot);
                    history.sequence = Some(page.sequence);
                    history.page = Some(page.clone());
                    let match_at = history.match_at;
                    history.status = if match_at == Some(page.start) {
                        format!("Совпадение показано · sequence {}", page.sequence)
                    } else if page.warnings.is_empty() {
                        format!("Снимок заморожен на sequence {}", page.sequence)
                    } else {
                        format!(
                            "Снимок sequence {} · предупреждений восстановления: {}",
                            page.sequence,
                            page.warnings.len()
                        )
                    };
                    history.error = None;
                }
                cx.notify();
            }
            Some(protocol::HistoryViewReply::Search { result }) => {
                let Some(history) = &mut self.product.history_sheet else {
                    return;
                };
                let text = match &pending {
                    history_view::Pending::Search { text, .. } => text,
                    _ => {
                        history.error = Some("Неожиданный ответ поиска истории".into());
                        cx.notify();
                        return;
                    }
                };
                let query_now = self
                    .product
                    .dialog
                    .as_ref()
                    .and_then(|dialog| dialog.fields.first())
                    .map(|field| field.text());
                let valid = history.snapshot == Some(result.snapshot)
                    && history.session == result.session
                    && history.generation == result.generation
                    && history.sequence == Some(result.sequence)
                    && result.scanned_bytes <= 256 * 1024
                    && result.found.is_none_or(|cursor| {
                        cursor.row
                            < history
                                .page
                                .as_ref()
                                .map_or(u64::MAX, |page| page.total_rows)
                            && cursor.offset <= 16 * 1024 * 1024
                    })
                    && result.next.is_none_or(|cursor| {
                        cursor.row
                            < history
                                .page
                                .as_ref()
                                .map_or(u64::MAX, |page| page.total_rows)
                            && cursor.offset <= 16 * 1024 * 1024
                    })
                    && match &pending {
                        history_view::Pending::Search { cursor, .. } => {
                            history.search_next == *cursor
                        }
                        _ => false,
                    }
                    && query_now == Some(text.as_str());
                if history.closed || !is_current {
                    let snapshot = result.snapshot;
                    self.close_history_snapshot(snapshot, cx);
                    return;
                }
                if !valid {
                    history.status = "Поиск отменён: запрос или снимок изменился".into();
                    cx.notify();
                    return;
                }
                history.search_next = result.next;
                if let Some(found) = result.found {
                    history.match_at = Some(found);
                    history.status = format!("Совпадение найдено: строка {}", found.row + 1);
                    self.history_page(found, true, cx);
                } else if let Some(next) = result.next {
                    history.status = format!(
                        "Поиск продолжается · проверено {} байт",
                        result.scanned_bytes
                    );
                    let query = text.clone();
                    let snapshot = result.snapshot;
                    self.issue_history_query(
                        protocol::HistoryViewQuery::Search {
                            snapshot,
                            text: query.clone(),
                            cursor: Some(next),
                        },
                        history_view::Pending::Search {
                            text: query,
                            cursor: Some(next),
                        },
                        cx,
                    );
                } else {
                    history.status =
                        format!("Совпадений нет · проверено {} байт", result.scanned_bytes);
                }
            }
            Some(protocol::HistoryViewReply::Closed { snapshot }) => {
                if !matches!(&pending, history_view::Pending::Close { .. }) {
                    if let Some(history) = &mut self.product.history_sheet {
                        history.error = Some("Неожиданный ответ закрытия снимка истории".into());
                    }
                    cx.notify();
                    return;
                }
                let refresh = matches!(pending, history_view::Pending::Close { refresh: true });
                if let Some(history) = &mut self.product.history_sheet
                    && history.snapshot == Some(snapshot)
                {
                    history.snapshot = None;
                    history.page = None;
                    history.back.clear();
                }
                if refresh {
                    self.history_open_snapshot(cx);
                } else {
                    self.product.history_sheet = None;
                }
            }
            None => {
                if let Some(history) = &mut self.product.history_sheet {
                    history.error = Some("Host returned no history response".into());
                    history.status = "История недоступна; можно обновить снимок".into();
                    history.pending = None;
                }
            }
        }
    }

    fn apply_history_error(&mut self, message: String, cx: &mut Context<Self>) {
        let message = history_view::display_error(&message);
        let pending = self
            .product
            .history_sheet
            .as_mut()
            .and_then(|history| history.pending.take());
        if matches!(
            pending,
            Some(history_view::Pending::Close { refresh: true })
        ) {
            if let Some(history) = &mut self.product.history_sheet {
                history.snapshot = None;
                history.page = None;
                history.error = Some(message);
            }
            self.history_open_snapshot(cx);
            return;
        }
        if matches!(
            pending,
            Some(history_view::Pending::Close { refresh: false })
        ) {
            self.product.history_sheet = None;
            cx.notify();
            return;
        }
        let closed = self
            .product
            .history_sheet
            .as_ref()
            .is_some_and(|history| history.closed);
        if closed {
            if let Some(snapshot) = self
                .product
                .history_sheet
                .as_ref()
                .and_then(|history| history.snapshot)
            {
                self.close_history_snapshot(snapshot, cx);
            } else {
                self.product.history_sheet = None;
            }
        } else if let Some(history) = &mut self.product.history_sheet {
            history.error = Some(message);
            history.status = "История недоступна; проверьте снимок или обновите его".into();
        }
        cx.notify();
    }

    fn canvas_offset(&self) -> pigade_storage::CanvasOffset {
        if self.canvas_focus_mode {
            return self.canvas_focus_offset;
        }
        self.canvas_tab
            .and_then(|tab| self.canvas_offsets.get(&tab).copied())
            .unwrap_or_default()
    }

    fn sync_editor_scope(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let scope = self.product.dialog.as_ref().map(|dialog| dialog.id);
        if scope != self.editor_dialog {
            self.editor_dialog = scope;
            self.editor_preferred_x = None;
            self.editor_layouts.clear();
            self.editor_scroll.clear();
            self.editor_drag = None;
            self.text_input.cancel();
            self.input_epoch = self.input_epoch.wrapping_add(1);
            self.finish_pointer(cx);
            window.notify_text_input_changed(&self.focus);
        }
    }

    fn editor_pointer_down(
        &mut self,
        index: usize,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(dialog) = &mut self.product.dialog else {
            return;
        };
        if index >= dialog.fields.len() {
            return;
        }
        if dialog.active_field != index {
            dialog.fields[dialog.active_field].cancel_mark();
        }
        dialog.active_field = index;
        self.editor_preferred_x = None;
        let buffer = &mut dialog.fields[index];
        let position = self
            .editor_layouts
            .get(&index)
            .filter(|layout| layout.dialog_id == dialog.id)
            .map_or(buffer.cursor(), |layout| {
                layout.index_for_point(event.position)
            });
        let selection = buffer.selection();
        let anchor = if event.modifiers.shift {
            if buffer.reversed() {
                selection.end
            } else {
                selection.start
            }
        } else {
            position
        };
        buffer.select_bytes(
            anchor.min(position)..anchor.max(position),
            position < anchor,
        );
        self.editor_drag = Some((dialog.id, index, anchor));
        self.focus.focus(window, cx);
        window.notify_text_input_changed(&self.focus);
        cx.stop_propagation();
        cx.notify();
    }

    fn editor_wheel(
        &mut self,
        index: usize,
        event: &ScrollWheelEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(layout) = self.editor_layouts.get(&index)
            && self
                .product
                .dialog
                .as_ref()
                .is_some_and(|dialog| dialog.id == layout.dialog_id)
        {
            let delta = match event.delta {
                ScrollDelta::Lines(value) => value.y * 24.,
                ScrollDelta::Pixels(value) => f32::from(value.y),
            };
            let current = self.editor_scroll.entry(index).or_default();
            *current = (*current - px(delta)).clamp(px(0.), layout.max_scroll());
            cx.stop_propagation();
            cx.notify();
        }
    }

    fn editor_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.service_panel_search(cx);
        let query = self.product.dialog.as_ref().and_then(|dialog| {
            (dialog.kind == product::DialogKind::History)
                .then(|| dialog.fields.first().map(|field| field.text().to_owned()))
                .flatten()
        });
        if let (Some(query), Some(history)) = (query, self.product.history_sheet.as_mut())
            && history.query_text != query
        {
            history.query_text = query;
            history.query_changed();
        }
        window.notify_text_input_changed(&self.focus);
        cx.notify();
    }

    fn set_canvas_offset(&mut self, offset: pigade_storage::CanvasOffset) {
        if offset.validate().is_err() || offset == self.canvas_offset() {
            return;
        }
        if self.canvas_focus_mode {
            self.canvas_focus_offset = offset;
        } else if let Some(tab) = self.canvas_tab {
            self.canvas_offsets.insert(tab, offset);
            self.canvas_dirty.insert(tab);
            self.canvas_save_at = Some(Instant::now() + Duration::from_millis(250));
        }
    }

    fn flush_canvas_offset(&mut self, cx: &mut Context<Self>) {
        if self.product.busy
            || self.pane_drag.is_some()
            || self.canvas_drag.is_some()
            || !self
                .canvas_save_at
                .is_some_and(|time| Instant::now() >= time)
        {
            return;
        }
        let Some(tab) = self.canvas_dirty.first().copied() else {
            return;
        };
        let Some(offset) = self.canvas_offsets.get(&tab).copied() else {
            return;
        };
        if self
            .product_tx
            .try_send(json!({"command":"set_canvas_offset","tab":tab,"offset":offset}))
            .is_ok()
        {
            self.canvas_dirty.remove(&tab);
            self.canvas_saving = Some((tab, offset));
            self.product.busy = true;
            cx.notify();
        }
    }

    fn pan_canvas(&mut self, dx: f64, dy: f64, cx: &mut Context<Self>) {
        if self.product.dialog.is_some()
            || self.resize_gesture.is_some()
            || self.pane_drag.is_some()
        {
            return;
        }
        let mut offset = self.canvas_offset();
        for bar in &self.canvas_bars {
            match bar.axis {
                pigade_layout::Axis::Horizontal => {
                    offset.x = (offset.x + dx).clamp(0., bar.max_offset)
                }
                pigade_layout::Axis::Vertical => {
                    offset.y = (offset.y + dy).clamp(0., bar.max_offset)
                }
            }
        }
        self.canvas_reveal = None;
        self.set_canvas_offset(offset);
        cx.notify();
    }

    fn tab_layout(&self) -> Option<(uuid::Uuid, pigade_layout::TabLayout)> {
        let catalog = self.product.catalog.as_ref()?;
        let id = catalog.selection.tab_id?;
        let tab = catalog.tabs.iter().find(|tab| tab.id == id)?;
        Some((
            id,
            self.layout_preview
                .as_ref()
                .filter(|(preview_tab, _)| *preview_tab == id)
                .map(|(_, layout)| layout.clone())
                .unwrap_or(serde_json::from_str(&tab.layout_json).ok()?),
        ))
    }

    fn selected_pane(&self) -> Option<uuid::Uuid> {
        self.state
            .views
            .get(self.selected)
            .and_then(|view| view.session.parse().ok())
    }

    fn preview_layout(
        &mut self,
        tab: uuid::Uuid,
        base: pigade_layout::TabLayout,
        command: pigade_layout::Command,
        cx: &mut Context<Self>,
    ) {
        let state = pigade_layout::LayoutState {
            tabs: [(tab, base)].into_iter().collect(),
        };
        if let Ok(model) = pigade_layout::LayoutModel::new(state)
            && let Ok(mut preview) = model.preview(command.clone())
            && let Some(layout) = preview.tabs.remove(&tab)
        {
            self.layout_preview = Some((tab, layout));
            self.resize_pending = Some(command);
            cx.notify();
        }
    }

    fn finish_resize(&mut self, commit: bool, cx: &mut Context<Self>) {
        self.layout_preview = None;
        if !commit
            && let Some(previous) = self
                .resize_gesture
                .as_ref()
                .and_then(|gesture| gesture.base.focused)
            && let Some(index) = self
                .state
                .views
                .iter()
                .position(|view| view.session == previous.to_string())
        {
            self.selected = index;
        }
        self.resize_gesture = None;
        self.layout_mode = None;
        if let Some(command) = self.resize_pending.take().filter(|_| commit) {
            self.layout_command(command, cx);
        }
        cx.notify();
    }

    fn resize_at(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(gesture) = &self.resize_gesture else {
            return;
        };
        let delta = match gesture.axis {
            pigade_layout::Axis::Horizontal => f64::from(f32::from(position.x - gesture.start.x)),
            pigade_layout::Axis::Vertical => f64::from(f32::from(position.y - gesture.start.y)),
        };
        if let Some(command) = tiling::resize_by_delta(
            &gesture.base,
            gesture.split,
            gesture.boundary,
            delta,
            gesture.extent,
        ) {
            self.preview_layout(gesture.tab, gesture.base.clone(), command, cx);
        }
    }

    fn begin_pane_drag(
        &mut self,
        pane: uuid::Uuid,
        position: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        let Some((tab, base)) = self.tab_layout() else {
            return;
        };
        let Some(bounds) = self
            .state
            .views
            .iter()
            .position(|view| view.session == pane.to_string())
            .and_then(|index| self.panes.get(index))
            .map(|pane| pane.bounds)
        else {
            return;
        };
        self.pane_drag = Some(PaneDrag {
            pane,
            split: uuid::Uuid::new_v4(),
            tab,
            base,
            source: pigade_layout::Rect {
                x: f64::from(f32::from(bounds.left())),
                y: f64::from(f32::from(bounds.top())),
                width: f64::from(f32::from(bounds.size.width)),
                height: f64::from(f32::from(bounds.size.height)),
            },
            position,
            start: position,
            moved: false,
            travelled: false,
            drop: None,
        });
        self.update_pane_drag_at(position, cx);
    }

    fn cancel_pane_drag(&mut self, cx: &mut Context<Self>) {
        if self.pane_drag.take().is_some() {
            self.layout_preview = None;
            self.sync_panel_visibility();
            self.filter_product_views();
            cx.notify();
        }
    }

    fn update_pane_drag(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        if event.pressed_button != Some(MouseButton::Left) {
            self.cancel_pane_drag(cx);
            return;
        }
        self.update_pane_drag_at(event.position, cx);
    }

    fn update_pane_drag_at(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(drag) = &mut self.pane_drag else {
            return;
        };
        drag.position = position;
        drag.travelled |= pointer_layout::travelled(
            f64::from(f32::from(drag.start.x)),
            f64::from(f32::from(drag.start.y)),
            f64::from(f32::from(position.x)),
            f64::from(f32::from(position.y)),
        );
        drag.moved |= drag.travelled;
        // Do not tear a pane out on a modifier click or hand jitter. Once the
        // threshold is crossed, project the detached remainder synchronously:
        // a coalesced move/up must use the same targets as the painted preview.
        if drag.moved && self.layout_preview.is_none() {
            self.layout_preview = pointer_layout::detached_layout(&drag.base, drag.pane)
                .map(|layout| (drag.tab, layout));
            self.sync_panel_visibility();
            if self.layout_preview.is_some()
                && let Some((bounds, cell, scale)) = self.projection_input
            {
                // Active pane drags suppress all PTY resize admission here.
                self.geometry(bounds, cell, scale, cx);
            }
        }
        let drag = self.pane_drag.as_mut().expect("active pane drag");
        drag.drop = None;
        if drag.moved
            && self
                .canvas_bounds
                .is_some_and(|bounds| bounds.contains(&position))
            && let Some(geometry) = &self.layout_geometry
        {
            let x = f64::from(f32::from(position.x));
            let y = f64::from(f32::from(position.y));
            if let Some(target) = pointer_layout::drop_target(&geometry.panes, drag.pane, x, y)
                && let Some(command) = pointer_layout::drop_command(
                    drag.pane,
                    drag.split,
                    target,
                    x.clamp(target.rect.x, target.rect.x + target.rect.width),
                    y.clamp(target.rect.y, target.rect.y + target.rect.height),
                )
            {
                let mut hint = target.rect;
                if let pigade_layout::Command::Move { side, .. } = command {
                    match side {
                        pigade_layout::Direction::Left => hint.width /= 2.,
                        pigade_layout::Direction::Right => {
                            hint.x += hint.width / 2.;
                            hint.width /= 2.;
                        }
                        pigade_layout::Direction::Up => hint.height /= 2.,
                        pigade_layout::Direction::Down => {
                            hint.y += hint.height / 2.;
                            hint.height /= 2.;
                        }
                    }
                }
                drag.drop = Some((command, hint));
            }
        }
        cx.stop_propagation();
        cx.notify();
    }

    fn focus_neighbor(
        &mut self,
        direction: pigade_layout::Direction,
        moving: bool,
        swapping: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(pane) = self.selected_pane() else {
            return;
        };
        let Some(target) = self
            .layout_geometry
            .as_ref()
            .and_then(|geometry| geometry.neighbor(pane, direction))
        else {
            return;
        };
        let command = if swapping {
            pigade_layout::Command::Swap {
                first: pane,
                second: target,
            }
        } else if moving {
            pigade_layout::Command::Move {
                pane,
                target,
                side: direction,
                split: uuid::Uuid::new_v4(),
            }
        } else {
            pigade_layout::Command::Focus { pane: target }
        };
        if !moving && !swapping {
            if let Some(index) = self
                .state
                .views
                .iter()
                .position(|view| view.session == target.to_string())
            {
                self.selected = index;
                self.canvas_reveal = Some(target);
            }
            self.text_input.cancel();
            window.notify_text_input_changed(&self.focus);
            self.persist_focus(cx);
            return;
        }
        self.layout_command(command, cx);
    }

    fn keyboard_resize(&mut self, direction: pigade_layout::Direction, cx: &mut Context<Self>) {
        use pigade_layout::{Axis, Direction};
        let Some(pane) = self.selected_pane() else {
            return;
        };
        let Some((tab, layout)) = self.tab_layout() else {
            return;
        };
        let axis = match direction {
            Direction::Left | Direction::Right => Axis::Horizontal,
            _ => Axis::Vertical,
        };
        let Some((split, boundary)) = tiling::nearest_split(&layout, pane, axis) else {
            return;
        };
        if let Some(pigade_layout::Command::ResizeBoundary {
            split: pending_split,
            boundary: pending_boundary,
            ..
        }) = self.resize_pending.as_ref()
            && (*pending_split != split || *pending_boundary != boundary)
        {
            return;
        }
        let Some(extent) = self
            .layout_geometry
            .as_ref()
            .and_then(|geometry| tiling::split_extent(geometry, &layout, split))
        else {
            return;
        };
        let delta = if matches!(direction, Direction::Left | Direction::Up) {
            -24.
        } else {
            24.
        };
        if let Some(command) = tiling::resize_by_delta(&layout, split, boundary, delta, extent) {
            self.preview_layout(tab, layout, command, cx);
        }
    }

    fn sync_panel_visibility(&mut self) {
        if let Some(catalog) = &self.product.catalog {
            let reveal_all = self.pane_drag.as_ref().is_some_and(|drag| drag.moved);
            let panels = catalog_panel_targets_for(catalog, reveal_all);
            let changed = {
                let mut shared = self.shared.lock().unwrap();
                let changed = shared.visible_panels.as_ref() != Some(&panels);
                if changed {
                    shared.set_visible_panels(panels);
                }
                changed
            };
            if changed {
                // A quiet PTY can leave the periodic reader in a 1s host wait.
                // Refresh the newly visible replicas without that idle delay.
                let _ = self.product_tx.try_send(json!({"_ui_refresh_panels":true}));
            }
        }
    }

    fn filter_product_views(&mut self) {
        if let Some(catalog) = &self.product.catalog {
            let dragged = self
                .pane_drag
                .as_ref()
                .filter(|drag| drag.moved)
                .map(|drag| drag.pane);
            let ids = catalog_panes_for(catalog, dragged.is_some());
            self.state.views = ids
                .iter()
                .filter_map(|id| {
                    self.state
                        .views
                        .iter()
                        .find(|view| view.session == id.to_string())
                        .cloned()
                })
                .collect();
            self.selected = dragged
                .and_then(|id| {
                    self.state
                        .views
                        .iter()
                        .position(|view| view.session == id.to_string())
                })
                .unwrap_or_else(|| self.selected.min(self.state.views.len().saturating_sub(1)));
        }
    }

    fn sync_product_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.focus_queue.cancel();
        self.deferred_placement = None;
        self.layout_preview = None;
        self.resize_pending = None;
        self.resize_gesture = None;
        self.layout_mode = None;
        self.pane_drag = None;
        self.canvas_drag = None;
        let Some(catalog) = &self.product.catalog else {
            return;
        };
        let previous_target = self
            .state
            .views
            .get(self.selected)
            .map(|view| (view.session.clone(), view.generation.clone()));
        let ids = catalog_panes(catalog);
        {
            let mut shared = self.shared.lock().unwrap();
            shared.set_visible_panels(catalog_panel_targets(catalog));
            shared
                .views
                .retain(|v| ids.iter().any(|id| v.session == id.to_string()));
            shared.views_revision = shared.views_revision.wrapping_add(1);
            shared.revision = shared.revision.wrapping_add(1);
            self.state.views = shared.views.clone();
        }
        self.filter_product_views();
        if let Some((_, layout)) = self.tab_layout() {
            self.selected = layout
                .focused
                .and_then(|id| {
                    self.state
                        .views
                        .iter()
                        .position(|v| v.session == id.to_string())
                })
                .unwrap_or(0);
        }
        if previous_target
            != self
                .state
                .views
                .get(self.selected)
                .map(|view| (view.session.clone(), view.generation.clone()))
        {
            self.finish_pointer(cx);
            self.selection = None;
            self.selection_dragging = false;
            self.text_input.cancel();
            self.input_epoch = self.input_epoch.wrapping_add(1);
            window.notify_text_input_changed(&self.focus);
        }
        cx.notify();
    }

    fn persist_focus(&mut self, cx: &mut Context<Self>) {
        if let Some(pane) = self.selected_pane()
            && let Some((tab, _)) = self.tab_layout()
        {
            self.focus_queue
                .choose(pointer_layout::FocusIntent { tab, pane });
        }
        cx.notify();
    }

    fn flush_focus(&mut self, cx: &mut Context<Self>) {
        if self.product.busy
            || self.focus_queue.is_saving()
            || self.pane_drag.is_some()
            || self.resize_gesture.is_some()
        {
            return;
        }
        let Some(intent) = self.focus_queue.pending() else {
            if let Some(command) = self.deferred_placement.take() {
                self.layout_command(command, cx);
            }
            return;
        };
        let Some((tab, layout)) = self.tab_layout() else {
            self.focus_queue.cancel();
            return;
        };
        if tab != intent.tab
            || !layout.panes().contains(&intent.pane)
            || layout.focused == Some(intent.pane)
        {
            self.focus_queue.cancel();
            return;
        }
        let request = uuid::Uuid::new_v4();
        let command = json!({"command":"layout_edit","expected_revision":self.product.catalog.as_ref().expect("tab requires catalog").layout.revision,
            "action":{"action":"apply","command":{"Focus":{"pane":intent.pane}}},"_ui_request":request});
        if self.try_product_command(command, cx) {
            self.focus_queue.start(request);
        }
    }

    fn layout_command(&mut self, command: pigade_layout::Command, cx: &mut Context<Self>) {
        // Placement (especially focus-mode toggle) must observe the latest
        // hovered focus, including one not yet dispatched to the host.
        if self.focus_queue.is_saving() || self.focus_queue.pending().is_some() {
            if self.deferred_placement.is_none() {
                self.deferred_placement = Some(command);
            } else {
                self.product.notice =
                    Some("Операция размещения уже ожидает сохранения фокуса".into());
            }
            cx.notify();
            return;
        }
        let Some(catalog) = &self.product.catalog else {
            return;
        };
        self.product_command(
            json!({"command":"layout_edit","expected_revision":catalog.layout.revision,
            "action":{"action":"apply","command":command}}),
            cx,
        );
    }

    fn product_action(
        &mut self,
        action: product::ProductAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use product::{Dialog, DialogKind, ProductAction as A};
        self.focus.focus(window, cx);
        // A real hover may have just queued a focus CAS. Keep one explicit
        // user action and build its metadata batch only after that ACK.
        if !matches!(
            action,
            A::CloseDialog | A::FocusPane(_) | A::ToggleSidebar | A::ToggleArchived
        ) {
            if !self.product.busy && self.focus_queue.pending().is_some() {
                self.flush_focus(cx);
            }
            if self.product.busy && self.focus_queue.is_saving() {
                if self.deferred_product_action.is_none() {
                    self.deferred_product_action = Some(action);
                } else {
                    self.product.notice = Some("Действие уже ожидает сохранения фокуса".into());
                }
                cx.notify();
                return;
            }
        }
        if matches!(
            action,
            A::SubmitDialog
                | A::ConfirmCloseTab
                | A::ConfirmPreparedLaunch
                | A::RefreshLaunchPreview
                | A::ToggleMemory
                | A::ToggleMemorySection(_)
                | A::ReloadMemory
                | A::ToggleContextDelivery
                | A::OpenPrompts
                | A::SaveDraftAsPrompt
                | A::SaveLaunchAsProfile
                | A::SetProfileProvider(_)
                | A::ToggleProfileContext
                | A::SetTemplateRole(_)
                | A::EditTemplateSlot(_)
                | A::EditRecipe
        ) && self
            .product
            .dialog
            .as_ref()
            .is_some_and(|dialog| dialog.fields.iter().any(|buffer| buffer.marked().is_some()))
        {
            self.product.notice = Some("Завершите ввод IME перед сохранением или запуском".into());
            cx.notify();
            return;
        }
        if matches!(
            action,
            A::CloseDialog
                | A::OpenAppearance
                | A::OpenCloseTab
                | A::CloseManagedTab
                | A::OpenWorkspaceGit
                | A::OpenControls
                | A::OpenPalette
                | A::OpenPaneMenu
                | A::OpenLauncher
                | A::AddProject
                | A::OpenProjectPath
                | A::CreateProject
                | A::CreateWorkspace
                | A::CreateWorkspaceFor(_)
                | A::CreateTab
                | A::ManageSelected(_)
                | A::ManageProject(_)
                | A::ManageWorkspace(_)
                | A::ManageTab(_)
                | A::OpenAgentProfiles
                | A::OpenLayouts
                | A::SaveLayout
                | A::ChoosePreset(_)
                | A::ConfigureTemplate(_)
                | A::NewTabFromTemplate(_)
                | A::OpenTemplateRuns
                | A::OpenTransfer
                | A::OpenSessions
        ) {
            self.pending_launch_review = false;
            self.profile_prepare = None;
            self.pending_direct_launch = None;
            self.pending_context_inspection = false;
            self.pending_conversation_inspection = false;
            self.pending_generation_picker = false;
            self.pending_context_editor = None;
        }
        match action {
            A::OpenAppearance
            | A::SetTheme(_)
            | A::ToggleReducedMotion
            | A::ReloadAppearance
            | A::AdjustTerminalFont(_)
            | A::ResetTerminalFont => self.appearance_action(action, window, cx),
            A::OpenCloseTab | A::CloseManagedTab | A::ConfirmCloseTab | A::InspectCloseTab => {
                self.tab_close_action(action, cx)
            }
            A::OpenPrompts
            | A::ReloadPrompts
            | A::NewPrompt
            | A::EditPrompt(_)
            | A::DeletePrompt(_)
            | A::UsePrompt(_)
            | A::CopyPrompt(_)
            | A::SaveDraftAsPrompt => self.prompt_action(action, cx),
            A::OpenAgentProfiles
            | A::ReloadAgentProfiles
            | A::NewAgentProfile
            | A::EditAgentProfile(_)
            | A::LaunchAgentProfile(_)
            | A::DeleteAgentProfile(_)
            | A::SetProfileProvider(_)
            | A::ToggleProfileContext
            | A::SaveLaunchAsProfile => self.profile_action(action, cx),
            A::OpenWorkspaceGit | A::RefreshWorkspaceGit => self.workspace_git_action(action, cx),
            A::OpenStop | A::StopSession { .. } | A::ConfirmStop | A::InspectStop => {
                self.session_control_action(action, cx)
            }
            A::RenamePane
            | A::ActivatePane { .. }
            | A::MorePanelResults
            | A::FirstPanelResults
            | A::ReloadPanelResults => self.navigation_action(action, cx),
            A::ChoosePreset(_)
            | A::ConfigureTemplate(_)
            | A::NewTabFromTemplate(_)
            | A::EditRecipe
            | A::EditTemplateSlot(_)
            | A::SetTemplateProvider(_)
            | A::SetTemplateRole(_)
            | A::ToggleTemplateContext
            | A::ClearTemplateLaunches
            | A::ConfirmTemplateRun
            | A::RefreshTemplateRun
            | A::CancelTemplateRun
            | A::InspectTemplateRunSlot(_)
            | A::ReturnTemplateRun
            | A::OpenTemplateRuns
            | A::LoadTemplateRuns(_)
            | A::SelectTemplateRun(_) => self.template_action(action, window, cx),
            A::ManageSelected(kind) => {
                let action = self
                    .product
                    .catalog
                    .as_ref()
                    .and_then(|catalog| match kind {
                        catalog_flow::EntityKind::Project => {
                            catalog.selection.project_id.map(A::ManageProject)
                        }
                        catalog_flow::EntityKind::Workspace => {
                            catalog.selection.workspace_id.map(A::ManageWorkspace)
                        }
                        catalog_flow::EntityKind::Tab => catalog.selection.tab_id.map(A::ManageTab),
                    });
                if let Some(action) = action {
                    self.product_action(action, window, cx);
                } else {
                    self.product.notice =
                        Some("Сначала выберите проект, воркспейс или вкладку".into());
                }
            }
            A::ManageProject(id) | A::ManageWorkspace(id) | A::ManageTab(id) => {
                self.product.management_target =
                    self.product
                        .catalog
                        .as_ref()
                        .and_then(|catalog| match action {
                            A::ManageProject(_) => catalog
                                .projects
                                .iter()
                                .find(|value| value.id == id)
                                .cloned()
                                .map(catalog_flow::ManagedEntity::Project),
                            A::ManageWorkspace(_) => catalog
                                .workspaces
                                .iter()
                                .find(|value| value.id == id)
                                .cloned()
                                .map(catalog_flow::ManagedEntity::Workspace),
                            _ => catalog
                                .tabs
                                .iter()
                                .find(|value| value.id == id)
                                .cloned()
                                .map(catalog_flow::ManagedEntity::Tab),
                        });
                if self.product.management_target.is_some() {
                    self.product.notice = None;
                    self.product.dialog = Some(Dialog::new(DialogKind::Manage));
                }
            }
            A::RenameTarget => {
                if let Some(target) = &self.product.management_target {
                    let mut dialog = Dialog::new(DialogKind::Rename);
                    dialog.fields[0] = target.name().into();
                    dialog.fields[0].select_all();
                    self.product.dialog = Some(dialog);
                }
            }
            A::ArchiveTarget => {
                if let (Some(target), Some(catalog)) =
                    (&self.product.management_target, &self.product.catalog)
                {
                    match target.archive(catalog) {
                        Ok(batch) => self.edit_catalog(
                            batch,
                            "Воркспейс обновлён. CLI и файлы сохранены.",
                            None,
                            cx,
                        ),
                        Err(error) => self.product.notice = Some(error),
                    }
                }
            }
            A::ToggleArchived => self.product.show_archived = !self.product.show_archived,
            A::RefreshCatalog => self.product_command(json!({"command":"catalog"}), cx),
            A::OpenLayouts => {
                self.product.layout_scroll.scroll_to_item(0);
                self.product.notice = None;
                self.product.dialog = Some(Dialog::new(DialogKind::Layouts));
            }
            A::SaveLayout => {
                if self
                    .product
                    .catalog
                    .as_ref()
                    .and_then(|catalog| catalog.layout_templates.as_ref())
                    .is_none()
                {
                    self.product.notice =
                        Some("Текущий session host не поддерживает сохранение шаблонов".into());
                } else if let Some((tab, layout)) = self.tab_layout() {
                    if layout.panes().is_empty() {
                        self.product.notice =
                            Some("Откройте хотя бы одну терминальную панель".into());
                    } else {
                        match pigade_layout::GeometryTemplate::from_tab(&layout) {
                            Ok(draft) => {
                                self.product.template_draft = Some(draft);
                                let name = self
                                    .product
                                    .catalog
                                    .as_ref()
                                    .and_then(|catalog| {
                                        catalog.tabs.iter().find(|value| value.id == tab)
                                    })
                                    .map(|tab| tab.name.clone())
                                    .unwrap_or_else(|| "Раскладка".into());
                                let mut dialog = Dialog::new(DialogKind::SaveLayout);
                                dialog.fields[0] = name.into();
                                dialog.fields[0].select_all();
                                self.product.notice = None;
                                self.product.dialog = Some(dialog);
                            }
                            Err(error) => self.product.notice = Some(error.to_string()),
                        }
                    }
                } else {
                    self.product.notice = Some("Сначала выберите вкладку".into());
                }
            }
            A::ApplyLayout(id) => {
                let template = self
                    .product
                    .catalog
                    .as_ref()
                    .and_then(|catalog| catalog.layout_templates.as_ref())
                    .and_then(|templates| templates.iter().find(|value| value.id == id))
                    .cloned();
                if let (Some(template), Some((tab, layout))) = (template, self.tab_layout()) {
                    if template.geometry.pane_count() != layout.panes().len() {
                        self.product.notice = Some(format!(
                            "Для этой раскладки нужно {} панелей; открыто {}",
                            template.geometry.pane_count(),
                            layout.panes().len()
                        ));
                    } else {
                        self.layout_command(
                            pigade_layout::Command::ApplyTemplate {
                                tab,
                                template: template.geometry,
                            },
                            cx,
                        );
                    }
                }
            }
            A::DeleteLayout(id) => {
                self.product.template_target = self
                    .product
                    .catalog
                    .as_ref()
                    .and_then(|catalog| catalog.layout_templates.as_ref())
                    .and_then(|templates| templates.iter().find(|value| value.id == id))
                    .cloned();
                if self.product.template_target.is_some() {
                    self.product.dialog = Some(Dialog::new(DialogKind::DeleteLayout));
                }
            }
            A::OpenControls => {
                self.prefix = false;
                self.cancel_pane_drag(cx);
                self.finish_resize(false, cx);
                self.finish_pointer(cx);
                self.product.dialog = Some(Dialog::new(DialogKind::Controls));
            }
            A::SetCommandModifier(modifier) => {
                if self.product.controls_saving {
                    return;
                }
                self.product.controls_saving = true;
                self.product.notice = None;
                let save = cx.background_spawn(async move { controls::save(modifier) });
                cx.spawn(async move |weak, cx| {
                    let result = save.await;
                    let _ = weak.update_in(cx, |this, window, cx| {
                        this.product.controls_saving = false;
                        match result {
                            Ok(()) => {
                                this.finish_pointer(cx);
                                this.product.command_modifier = modifier;
                                this.product.notice = Some(format!(
                                    "Клавиша управления сохранена: {}",
                                    modifier.label()
                                ));
                                this.text_input.cancel();
                                window.notify_text_input_changed(&this.focus);
                            }
                            Err(error) => {
                                this.product.notice = Some(format!(
                                    "Не удалось подтвердить сохранение клавиши: {error}"
                                ))
                            }
                        }
                        cx.notify();
                    });
                })
                .detach();
            }
            A::ToggleSidebar => {
                self.product.sidebar_override = Some(!self.product.sidebar_compact);
            }
            A::FocusPane(id) => {
                if self.product.busy {
                    return;
                }
                if let Some(index) = self
                    .state
                    .views
                    .iter()
                    .position(|view| view.session == id.to_string())
                {
                    if index != self.selected {
                        self.finish_pointer(cx);
                        self.selection = None;
                        self.selection_dragging = false;
                        self.text_input.cancel();
                        self.input_epoch = self.input_epoch.wrapping_add(1);
                        window.notify_text_input_changed(&self.focus);
                    }
                    self.selected = index;
                    self.persist_focus(cx);
                    self.canvas_reveal = Some(id);
                }
            }
            A::OpenHistory => self.open_history(cx),
            A::HistoryStart => {
                if self
                    .product
                    .history_sheet
                    .as_ref()
                    .and_then(|h| h.page.as_ref())
                    .is_some()
                {
                    self.history_page(protocol::HistoryCursor { row: 0, offset: 0 }, true, cx);
                }
            }
            A::HistoryPrevious => {
                let cursor = self.product.history_sheet.as_mut().and_then(|history| {
                    history
                        .back
                        .pop()
                        .or_else(|| history.page.as_ref().and_then(|page| page.previous))
                });
                if let Some(cursor) = cursor {
                    self.history_page(cursor, false, cx);
                }
            }
            A::HistoryNext => {
                if let Some(cursor) = self
                    .product
                    .history_sheet
                    .as_ref()
                    .and_then(|history| history.page.as_ref())
                    .and_then(|page| page.next)
                {
                    self.history_page(cursor, true, cx);
                }
            }
            A::HistoryEnd => {
                if let Some(total) = self
                    .product
                    .history_sheet
                    .as_ref()
                    .and_then(|history| history.page.as_ref())
                    .map(|page| page.total_rows)
                {
                    self.history_page(
                        protocol::HistoryCursor {
                            row: total.saturating_sub(128),
                            offset: 0,
                        },
                        true,
                        cx,
                    );
                }
            }
            A::HistoryRefresh => self.close_history(true, cx),
            A::HistorySearch => self.history_search(None, cx),
            A::HistoryNextMatch => {
                let cursor = self
                    .product
                    .history_sheet
                    .as_ref()
                    .and_then(|history| history.search_next);
                if let Some(cursor) = cursor {
                    self.history_search(Some(cursor), cx);
                } else if let Some(history) = &mut self.product.history_sheet {
                    history.error = Some("Больше совпадений нет".into());
                    cx.notify();
                }
            }
            A::HistoryCopyPage => {
                if let Some(dialog) = self
                    .product
                    .dialog
                    .as_ref()
                    .filter(|dialog| dialog.kind == product::DialogKind::History)
                {
                    let text = dialog
                        .fields
                        .get(1)
                        .map(|field| field.text().to_owned())
                        .unwrap_or_default();
                    if !text.is_empty() {
                        cx.write_to_clipboard(ClipboardItem::new_string(text));
                    }
                }
            }
            A::OpenGenerations | A::LoadGenerations(_) => {
                if self.product.busy {
                    return;
                }
                let session = if matches!(action, A::LoadGenerations(_)) {
                    self.product
                        .generation_page
                        .as_ref()
                        .map(|page| page.session)
                } else {
                    self.selected_pane()
                        .or_else(|| self.tab_layout().and_then(|(_, layout)| layout.focused))
                };
                if let Some(session) = session {
                    let before = if let A::LoadGenerations(before) = action {
                        before
                    } else {
                        None
                    };
                    self.pending_generation_picker = true;
                    self.product_command(
                        json!({"command":"generations","session":session,"before":before}),
                        cx,
                    );
                } else {
                    self.product.notice = Some("Сначала выберите терминальную панель".into());
                }
            }
            A::SelectGeneration(generation) => {
                if !self.product.busy
                    && let Some(page) = &self.product.generation_page
                {
                    self.product_command(json!({"command":"select_generation","session":page.session,"generation":generation}),cx);
                }
            }
            A::PanCanvas => {
                self.product.dialog = None;
                self.finish_pointer(cx);
                self.layout_mode = Some(LayoutMode::Pan);
            }
            A::RevealFocusedPane => {
                self.product.dialog = None;
                self.canvas_reveal = self.selected_pane();
            }
            A::AddProject => self.pick_project_folder(false, cx),
            A::OpenProjectPath => self.product.dialog = Some(Dialog::new(DialogKind::AddProject)),
            A::CreateProject => {
                let mut dialog = Dialog::new(DialogKind::CreateProject);
                let home = std::env::var_os("HOME")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| PathBuf::from("/tmp"));
                let projects = home.join("Projects");
                dialog.fields[1] = if projects.is_dir() { projects } else { home }
                    .to_string_lossy()
                    .into_owned()
                    .into();
                self.product.dialog = Some(dialog);
            }
            A::ChooseProjectParent => self.pick_project_folder(true, cx),
            A::CreateWorkspace | A::CreateWorkspaceFor(_) => {
                let project = if let A::CreateWorkspaceFor(project) = action {
                    Some(project)
                } else {
                    self.product
                        .catalog
                        .as_ref()
                        .and_then(|catalog| catalog.selection.project_id)
                };
                if let Some(project) = project {
                    self.product.workspace_project = Some(project);
                    self.product.workspace_isolated = true;
                    self.product.dialog = Some(Dialog::new(DialogKind::CreateWorkspace));
                } else {
                    self.product.notice = Some("Сначала откройте папку проекта".into());
                }
            }
            A::WorkspaceIsolation(isolated) => self.product.workspace_isolated = isolated,
            A::OpenLauncher => {
                self.product.dialog = Some(Dialog::new(DialogKind::Launcher));
                self.detect_providers(cx);
            }
            A::ToggleLaunchDetails => self.product.launch_details = !self.product.launch_details,
            A::OpenPaneMenu => {
                self.product.palette_index = 0;
                self.product.dialog = Some(Dialog::new(DialogKind::PaneMenu));
            }
            A::CreateTab => {
                if self
                    .product
                    .catalog
                    .as_ref()
                    .is_some_and(|catalog| catalog.selection.workspace_id.is_some())
                {
                    self.product.dialog = Some(Dialog::new(DialogKind::NewTab));
                } else {
                    self.product.notice = Some("Сначала создайте или выберите воркспейс".into());
                }
            }
            A::OpenSessions | A::RefreshSessions => {
                if action == A::OpenSessions
                    || !self
                        .product
                        .dialog
                        .as_ref()
                        .is_some_and(|dialog| dialog.kind == DialogKind::Sessions)
                {
                    self.product.dialog = Some(Dialog::new(DialogKind::Sessions));
                    self.product.layout_scroll.scroll_to_item(0);
                }
                self.product_command(json!({"command":"list"}), cx);
            }
            A::AttachSession(pane) => {
                if let Some((tab, layout)) = self.tab_layout() {
                    self.product.dialog = None;
                    self.layout_command(
                        pigade_layout::Command::Insert {
                            tab,
                            pane,
                            target: self.selected_pane().or(layout.focused),
                            side: pigade_layout::Direction::Right,
                            split: uuid::Uuid::new_v4(),
                        },
                        cx,
                    );
                } else {
                    self.product.notice = Some("Сначала выберите вкладку".into());
                }
            }
            A::OpenPalette => {
                self.product.notice = None;
                self.product.palette_index = 0;
                self.product.palette_scroll.scroll_to_item(0);
                self.product.dialog = Some(Dialog::new(DialogKind::Palette));
                self.service_panel_search(cx);
            }
            A::OpenTransfer => self.product.dialog = Some(Dialog::new(DialogKind::Transfer)),
            A::TransferTo(to_tab) => {
                if let Some(pane) = self.selected_pane() {
                    self.product.dialog = None;
                    self.layout_command(
                        pigade_layout::Command::Transfer {
                            pane,
                            to_tab,
                            target: None,
                            side: pigade_layout::Direction::Right,
                            split: uuid::Uuid::new_v4(),
                        },
                        cx,
                    );
                }
            }
            A::CloseDialog => {
                self.template_flow.pause();
                self.product.run_automatic = false;
                self.deferred_product_action = None;
                self.setup_flow = None;
                self.picker_request = None;
                self.product.workspace_project = None;
                if self.close_prompt_dialog() {
                    self.text_input.cancel();
                } else if self
                    .product
                    .dialog
                    .as_ref()
                    .is_some_and(|dialog| dialog.kind == DialogKind::History)
                {
                    self.close_history(false, cx);
                } else {
                    let profile_child = self.product.dialog.as_ref().is_some_and(|dialog| {
                        matches!(
                            dialog.kind,
                            DialogKind::AgentProfile | DialogKind::DeleteAgentProfile
                        )
                    });
                    let origin = self
                        .product
                        .dialog
                        .as_ref()
                        .filter(|dialog| dialog.kind == DialogKind::AgentProfile)
                        .and_then(|_| {
                            self.product
                                .profile_editor
                                .as_ref()
                                .and_then(|editor| editor.origin.clone())
                        });
                    self.product.dialog = origin
                        .or_else(|| profile_child.then(|| Dialog::new(DialogKind::AgentProfiles)));
                    self.text_input.cancel();
                }
            }
            A::EditProjectContext => {
                if let Some(scope) = self
                    .product
                    .catalog
                    .as_ref()
                    .and_then(|catalog| catalog.selection.project_id)
                    && !self.product.busy
                {
                    self.pending_context_editor = Some(scope);
                    self.product_command(json!({"command":"get_context","scope_id":scope}), cx);
                }
            }
            A::PrepareResume => {
                if self.product.busy || self.product_launch_unknown {
                    self.product.notice = Some(
                        "Сначала завершите текущую операцию или сверьте неизвестный запуск".into(),
                    );
                } else if let Some(session) = self.selected_pane() {
                    self.product.dialog = None;
                    self.pending_launch_review = true;
                    self.product_command(json!({"command":"prepare_resume","session":session}), cx);
                } else {
                    self.product.notice =
                        Some("Сначала выберите панель с привязанным диалогом".into());
                }
            }
            A::InspectSessionContext | A::InspectConversation => {
                if self.product.busy {
                    return;
                }
                self.pending_context_inspection = true;
                self.pending_conversation_inspection = action == A::InspectConversation;
                if let Some(view) = self.state.views.get(self.selected) {
                    self.product_command(json!({"command":"get_session_context","session":view.session,"generation":view.generation}),cx);
                } else {
                    self.pending_context_inspection = false;
                    self.pending_conversation_inspection = false;
                    self.product.notice = Some("Сначала выберите терминальную панель".into());
                }
            }
            A::ToggleMemory | A::ToggleMemorySection(_) => {
                if let Some(memory) = self
                    .product
                    .launch_preview
                    .as_ref()
                    .and_then(|preview| preview.memory.as_ref())
                    && let Some(dialog) = self
                        .product
                        .dialog
                        .as_mut()
                        .filter(|dialog| dialog.kind == DialogKind::LaunchReview)
                    && let Some(field) = dialog.fields.get_mut(2)
                {
                    match memory_choice::parse_sections(field.text(), memory) {
                        Ok(mut selected) => {
                            if let A::ToggleMemorySection(index) = action {
                                if let Some(position) =
                                    selected.iter().position(|value| *value == index)
                                {
                                    selected.remove(position);
                                } else if memory.sections.get(index).is_some() {
                                    selected.push(index);
                                    selected.sort_unstable();
                                }
                            } else if selected.is_empty() {
                                selected = memory
                                    .sections
                                    .iter()
                                    .map(|section| section.index)
                                    .collect();
                            } else {
                                selected.clear();
                            }
                            let text = memory_choice::format_sections(&selected);
                            let _ = field.replace(Some(0..field.len_utf16()), &text, false);
                            self.product.notice = Some(
                                "Выбор памяти изменён. Обновите предпросмотр перед запуском."
                                    .into(),
                            );
                            self.editor_changed(window, cx);
                        }
                        Err(error) => self.product.notice = Some(error),
                    }
                } else {
                    self.product.notice =
                        Some("Выбор разделов недоступен: обновите session host".into());
                }
            }
            A::RefreshLaunchPreview | A::ToggleContextDelivery | A::ReloadMemory => {
                if self.product.busy {
                    return;
                }
                if action == A::ReloadMemory
                    && let Some(dialog) = self
                        .product
                        .dialog
                        .as_mut()
                        .filter(|dialog| dialog.kind == DialogKind::LaunchReview)
                    && let Some(field) = dialog.fields.get_mut(2)
                {
                    let _ = field.replace(Some(0..field.len_utf16()), "", false);
                    self.editor_changed(window, cx);
                }
                if self.product_launch_unknown {
                    self.product.notice =
                        Some("Сначала сверьте исход предыдущего запуска через «Сессии»".into());
                } else if let Some(preview) = &self.product.launch_preview {
                    let fields = self
                        .product
                        .dialog
                        .as_ref()
                        .filter(|dialog| dialog.kind == DialogKind::LaunchReview)
                        .map(|dialog| {
                            dialog
                                .fields
                                .iter()
                                .map(|field| field.text().to_owned())
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default();
                    if preview.memory.is_none()
                        && action != A::ReloadMemory
                        && fields.get(2).is_some_and(|field| !field.trim().is_empty())
                    {
                        self.product.notice = Some("Session host не поддерживает выбор разделов. Обновите host; выбранный текст не будет отправлен автоматически.".into());
                        cx.notify();
                        return;
                    }
                    let memory_selection = match preview
                        .memory
                        .as_ref()
                        .filter(|_| action != A::ReloadMemory)
                        .map(|memory| {
                            memory_choice::selection(
                                fields.get(2).map(String::as_str).unwrap_or(""),
                                memory,
                            )
                        })
                        .transpose()
                    {
                        Ok(selection) => selection,
                        Err(error) => {
                            self.product.notice = Some(error);
                            cx.notify();
                            return;
                        }
                    };
                    let include_memory = action != A::ReloadMemory
                        && memory_selection
                            .as_ref()
                            .map(|selection| !selection.sections.is_empty())
                            .unwrap_or(preview.include_memory);
                    let deliver_context = if action == A::ToggleContextDelivery {
                        !preview.deliver_context
                    } else {
                        preview.deliver_context
                    };
                    let engine = match pigade_storage::EngineOptions::from_fields(
                        fields.get(3).map(String::as_str).unwrap_or(""),
                        fields.get(4).map(String::as_str).unwrap_or(""),
                    ) {
                        Ok(engine) => engine,
                        Err(error) => {
                            self.product.notice = Some(error.to_string());
                            cx.notify();
                            return;
                        }
                    };
                    if !engine.is_empty() && !self.product.engine_options_supported() {
                        self.product.notice=Some("Session host не поддерживает параметры модели. Обновите host; значения не будут отброшены молча.".into());
                        cx.notify();
                        return;
                    }
                    self.product_command(json!({"command":"prepare_launch","request":{"tab":preview.tab_id,"provider":preview.provider,"side":preview.side,"target":preview.target,
                        "role_override":fields.first().cloned().unwrap_or_default(),"agent_instructions":fields.get(1).cloned().unwrap_or_default(),"include_memory":include_memory,"memory_selection":memory_selection,"deliver_context":deliver_context,"engine":engine}}),cx);
                }
            }
            A::ConfirmPreparedLaunch => self.confirm_prepared_launch(cx),
            A::EditContext => {
                if let Some(scope) = self
                    .product
                    .catalog
                    .as_ref()
                    .and_then(|c| c.selection.workspace_id)
                    && !self.product.busy
                {
                    self.pending_context_editor = Some(scope);
                    self.product_command(json!({"command":"get_context","scope_id":scope}), cx);
                }
            }
            A::SelectProject(id) => {
                if let Some(catalog) = &self.product.catalog {
                    let mut selection = catalog.selection.clone();
                    selection.project_id = Some(id);
                    selection.workspace_id = catalog_flow::first_active_workspace(catalog, id)
                        .map(|workspace| workspace.id);
                    selection.tab_id = catalog
                        .tabs
                        .iter()
                        .find(|tab| Some(tab.workspace_id) == selection.workspace_id)
                        .map(|tab| tab.id);
                    let revision = selection.revision;
                    self.product_command(json!({"command":"catalog_apply","batch":{"projects":[],"workspaces":[],"tabs":[],"selection":{"value":selection,"expected_revision":revision},"layout":null}}),cx);
                }
            }
            A::SelectWorkspace(id) => {
                if let Some(catalog) = &self.product.catalog
                    && let Some(workspace) = catalog.workspaces.iter().find(|w| w.id == id)
                {
                    let mut selection = catalog.selection.clone();
                    selection.project_id = Some(workspace.project_id);
                    selection.workspace_id = Some(id);
                    selection.tab_id = catalog
                        .tabs
                        .iter()
                        .find(|t| t.workspace_id == id)
                        .map(|t| t.id);
                    let revision = selection.revision;
                    self.product_command(json!({"command":"catalog_apply","batch":{"projects":[],"workspaces":[],"tabs":[],"selection":{"value":selection,"expected_revision":revision},"layout":null}}),cx);
                }
            }
            A::SelectTab(id) => {
                if let Some(catalog) = &self.product.catalog
                    && let Some(tab) = catalog.tabs.iter().find(|t| t.id == id)
                {
                    let mut selection = catalog.selection.clone();
                    selection.tab_id = Some(id);
                    selection.workspace_id = Some(tab.workspace_id);
                    selection.project_id = catalog
                        .workspaces
                        .iter()
                        .find(|w| w.id == tab.workspace_id)
                        .map(|w| w.project_id);
                    let revision = selection.revision;
                    self.product_command(json!({"command":"catalog_apply","batch":{"projects":[],"workspaces":[],"tabs":[],"selection":{"value":selection,"expected_revision":revision},"layout":null}}),cx);
                }
            }
            A::SubmitDialog => {
                let Some(dialog) = self.product.dialog.as_ref() else {
                    return;
                };
                let kind = dialog.kind;
                let fields = dialog
                    .fields
                    .iter()
                    .map(|field| field.text().to_owned())
                    .collect::<Vec<_>>();
                let field = |index| fields.get(index).map(String::as_str).unwrap_or("");
                if kind == DialogKind::Appearance {
                    self.product.dialog = None;
                    return;
                }
                if kind == DialogKind::CloseTab {
                    self.tab_close_action(A::ConfirmCloseTab, cx);
                    return;
                }
                if self.submit_prompt_dialog(kind, &fields, cx) {
                    self.sync_editor_scope(window, cx);
                    cx.notify();
                    return;
                }
                if self.submit_profile_dialog(kind, &fields, cx) {
                    self.sync_editor_scope(window, cx);
                    cx.notify();
                    return;
                }
                if self.submit_template_dialog(kind, &fields, cx) {
                    self.sync_editor_scope(window, cx);
                    cx.notify();
                    return;
                }
                match kind {
                    DialogKind::Appearance
                    | DialogKind::CloseTab
                    | DialogKind::Prompts
                    | DialogKind::Prompt
                    | DialogKind::DeletePrompt
                    | DialogKind::AgentProfiles
                    | DialogKind::AgentProfile
                    | DialogKind::DeleteAgentProfile => {
                        unreachable!("profile dialog handled above")
                    }
                    DialogKind::Recipe
                    | DialogKind::RecipeSlot
                    | DialogKind::NewTemplateTab
                    | DialogKind::TemplateRun
                    | DialogKind::TemplateRunContext
                    | DialogKind::TemplateRuns => unreachable!("template dialog handled above"),
                    DialogKind::Rename => {
                        if let Some(target) = &self.product.management_target {
                            match target.rename(field(0)) {
                                Ok(batch) => {
                                    self.edit_catalog(batch, "Название сохранено", None, cx)
                                }
                                Err(error) => self.product.notice = Some(error),
                            }
                        }
                    }
                    DialogKind::SaveLayout => {
                        if let Some(geometry) = &self.product.template_draft {
                            let name = field(0).trim();
                            if name.is_empty()
                                || name.len() > 256
                                || name.chars().any(char::is_control)
                            {
                                self.product.notice =
                                    Some("Введите название до 256 байт UTF-8".into());
                            } else {
                                let batch = pigade_storage::CatalogBatch {
                                    layout_templates: vec![pigade_storage::CatalogWrite {
                                        value: pigade_storage::LayoutTemplate {
                                            id: uuid::Uuid::new_v4(),
                                            name: name.into(),
                                            geometry: geometry.clone(),
                                            launches: None,
                                            revision: 0,
                                        },
                                        expected_revision: None,
                                    }],
                                    ..Default::default()
                                };
                                self.edit_catalog(
                                    batch,
                                    "Раскладка сохранена",
                                    Some(DialogKind::Layouts),
                                    cx,
                                );
                            }
                        }
                    }
                    DialogKind::DeleteLayout => {
                        if let Some(template) = &self.product.template_target {
                            let batch = pigade_storage::CatalogBatch {
                                remove_layout_templates: vec![
                                    pigade_storage::LayoutTemplateDelete {
                                        id: template.id,
                                        expected_revision: template.revision,
                                    },
                                ],
                                ..Default::default()
                            };
                            self.edit_catalog(
                                batch,
                                "Раскладка удалена",
                                Some(DialogKind::Layouts),
                                cx,
                            );
                        }
                    }
                    DialogKind::Manage | DialogKind::Layouts => self.product.dialog = None,
                    DialogKind::AddProject => self
                        .begin_project_setup(json!({"command":"add_project","path":field(0)}), cx),
                    DialogKind::CreateProject => {
                        self.begin_project_setup(json!({"command":"create_project","name":field(0).trim(),"parent":field(1)}),cx);
                    }
                    DialogKind::CreateWorkspace => {
                        if let Some(project) = self.product.workspace_project.or_else(|| {
                            self.product
                                .catalog
                                .as_ref()
                                .and_then(|c| c.selection.project_id)
                        }) {
                            self.product_command(json!({"command":"create_workspace","project":project,"task":if field(0).trim().is_empty() {"Новая задача"} else {field(0)},"worktree":self.product.workspace_isolated}),cx);
                        } else {
                            self.product.notice =
                                Some("Сначала добавьте или выберите проект".into());
                        }
                    }
                    DialogKind::NewTab => {
                        if let Some(catalog) = &self.product.catalog
                            && let Some(workspace) = catalog.selection.workspace_id
                        {
                            let id = uuid::Uuid::new_v4();
                            let mut selection = catalog.selection.clone();
                            selection.tab_id = Some(id);
                            self.product_command(json!({"command":"catalog_apply","batch":{"projects":[],"workspaces":[],"tabs":[{"value":{"id":id,"workspace_id":workspace,"name":if field(0).trim().is_empty(){"Development"}else{field(0)},"layout_json":"null","active_pane":null,"revision":0},"expected_revision":null}],"selection":{"expected_revision":selection.revision,"value":selection},"layout":null}}),cx);
                        }
                    }
                    DialogKind::ProjectContext => {
                        if let Some(context) = &self.product.context {
                            self.product_command(json!({"command":"save_context","expected_revision":context.revision,"document":{"scope_id":context.scope_id,"revision":context.revision,"instructions":field(0),"role":context.role,"memory":context.memory}}),cx);
                        }
                    }
                    DialogKind::Context => {
                        if let Some(context) = &self.product.context {
                            self.product_command(json!({"command":"save_context","expected_revision":context.revision,"document":{"scope_id":context.scope_id,"revision":context.revision,"instructions":field(0),"role":field(1),"memory":field(2)}}),cx);
                        }
                    }
                    DialogKind::Generations => {
                        let selected = field(0).trim();
                        let action = if selected.eq_ignore_ascii_case("текущее")
                            || selected.eq_ignore_ascii_case("current")
                        {
                            Some(A::SelectGeneration(None))
                        } else {
                            selected
                                .parse::<usize>()
                                .ok()
                                .and_then(|index| index.checked_sub(1))
                                .and_then(|index| {
                                    self.product.generation_page.as_ref()?.entries.get(index)
                                })
                                .filter(|entry| entry.has_frame)
                                .map(|entry| A::SelectGeneration(Some(entry.generation)))
                        };
                        if let Some(action) = action {
                            self.product_action(action, window, cx);
                        } else {
                            self.product.notice =
                                Some("Выберите доступную строку или введите «текущее»".into());
                        }
                    }
                    DialogKind::Conversation => {
                        if !self.product.busy
                            && let Some(preview) = &self.product.launch_preview
                            && matches!(preview.provider.as_str(), "codex" | "claude" | "opencode")
                        {
                            self.pending_context_inspection = true;
                            self.pending_conversation_inspection = true;
                            self.product_command(json!({"command":"bind_conversation","session":preview.session,"generation":preview.generation,"conversation_id":field(0).trim(),"expected_revision":preview.conversation.as_ref().map(|binding| binding.revision).unwrap_or(0)}),cx);
                        }
                    }
                    DialogKind::RenamePane => self.submit_pane_name(cx),
                    DialogKind::WorkspaceGit => {
                        self.workspace_git_action(A::RefreshWorkspaceGit, cx)
                    }
                    DialogKind::StopSession => self.confirm_stop(cx),
                    DialogKind::Palette | DialogKind::PaneMenu => {
                        if let Some(action) = self.product.palette_action() {
                            self.choose_palette(action, window, cx);
                        }
                    }
                    DialogKind::LaunchReview | DialogKind::ResumeReview => {
                        self.confirm_prepared_launch(cx)
                    }
                    DialogKind::History => self.history_search(None, cx),
                    DialogKind::Launcher | DialogKind::Controls => self.product.dialog = None,
                    DialogKind::SessionContext | DialogKind::Transfer | DialogKind::Sessions => (),
                }
            }
            A::Launch(provider) => {
                self.launch_panel(provider, pigade_layout::Direction::Right, true, cx)
            }
            A::ReviewLaunch(provider) => {
                self.launch_panel(provider, pigade_layout::Direction::Right, false, cx)
            }
            A::Layout(name) => {
                let Some((tab, layout)) = self.tab_layout() else {
                    return;
                };
                match name.as_str() {
                    "hide" => {
                        if let Some(pane) = self.selected_pane() {
                            self.layout_command(pigade_layout::Command::ClosePane { pane }, cx);
                        }
                    }
                    "split_right" => {
                        self.launch_panel("shell".into(), pigade_layout::Direction::Right, true, cx)
                    }
                    "split_down" => {
                        self.launch_panel("shell".into(), pigade_layout::Direction::Down, true, cx)
                    }
                    "focus" => {
                        self.layout_command(pigade_layout::Command::ToggleFocusMode { tab }, cx)
                    }
                    "undo" | "redo" => {
                        if let Some(catalog) = &self.product.catalog {
                            self.product_command(json!({"command":"layout_edit","expected_revision":catalog.layout.revision,"action":{"action":name}}),cx);
                        }
                    }
                    "equalize" => {
                        if let Some(split) = self
                            .selected_pane()
                            .and_then(|pane| tiling::parent_split(&layout, pane))
                        {
                            self.layout_command(pigade_layout::Command::Equalize { split }, cx);
                        }
                    }
                    _ => (),
                }
            }
        }
        self.sync_editor_scope(window, cx);
        cx.notify();
    }

    fn launch_panel(
        &mut self,
        provider: String,
        side: pigade_layout::Direction,
        direct: bool,
        cx: &mut Context<Self>,
    ) {
        if self.product_launch_unknown {
            self.product.notice = Some(
                "Предыдущий запуск не подтверждён. Откройте «Сессии» для сверки; повторный запуск заблокирован.".into(),
            );
            return;
        }
        let Some((tab, layout)) = self.tab_layout() else {
            self.product.notice = Some("Создайте воркспейс и вкладку перед запуском".into());
            return;
        };
        if self.product.busy {
            return;
        }
        self.pending_launch_review = true;
        let request = uuid::Uuid::new_v4();
        let command = json!({"command":"prepare_launch","_ui_request":request,"request":{"tab":tab,"provider":provider,"side":side,
            "target":self.selected_pane().or(layout.focused),"role_override":"","agent_instructions":"","include_memory":false,"deliver_context":!direct}});
        if self.try_product_command(command, cx) && direct {
            self.pending_direct_launch = Some(DirectLaunch {
                tab,
                request,
                provider,
            });
        }
        cx.notify();
    }

    fn confirm_prepared_launch(&mut self, cx: &mut Context<Self>) {
        if self.product.busy || self.product_launch_unknown {
            return;
        }
        let Some(preview) = &self.product.launch_preview else {
            return;
        };
        let Some(dialog) = self.product.dialog.as_ref().filter(|dialog| {
            matches!(
                dialog.kind,
                product::DialogKind::LaunchReview | product::DialogKind::ResumeReview
            )
        }) else {
            return;
        };
        if !preview.can_launch
            || self.product.launch_phase.is_some()
            || (preview.resume.is_none()
                && (dialog.fields.first().map(text_edit::EditBuffer::text)
                    != Some(preview.role_override.as_str())
                    || dialog.fields.get(1).map(text_edit::EditBuffer::text)
                        != Some(preview.agent_instructions.as_str())
                    || !pigade_storage::EngineOptions::from_fields(
                        dialog
                            .fields
                            .get(3)
                            .map(text_edit::EditBuffer::text)
                            .unwrap_or(""),
                        dialog
                            .fields
                            .get(4)
                            .map(text_edit::EditBuffer::text)
                            .unwrap_or(""),
                    )
                    .is_ok_and(|engine| engine == preview.engine)
                    || !memory_choice::matches_preview(
                        dialog
                            .fields
                            .get(2)
                            .map(text_edit::EditBuffer::text)
                            .unwrap_or(""),
                        preview.memory.as_ref(),
                        preview.include_memory,
                    )))
            || (preview.resume.is_some() && dialog.kind != product::DialogKind::ResumeReview)
        {
            self.product.notice =
                Some("Обновите просмотр после изменения параметров запуска".into());
            cx.notify();
            return;
        }
        match self
            .product_tx
            .try_send(json!({"command":"start_prepared","plan_id":preview.plan_id}))
        {
            Ok(()) => {
                self.product_launch_unknown = true;
                self.pending_launch = Some((preview.session, preview.generation));
                self.product.busy = true;
                self.product.notice = None;
            }
            Err(_) => self.product.notice = Some("Запуск не отправлен: очередь недоступна".into()),
        }
        cx.notify();
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

    fn held_modifier(&self, modifiers: Modifiers) -> bool {
        self.product.command_modifier.matches(input::Modifiers {
            control: modifiers.control,
            alt: modifiers.alt,
            shift: modifiers.shift,
            platform: modifiers.platform,
        }) && !modifiers.shift
    }

    fn control_action(
        &mut self,
        action: controls::Action,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use controls::Action as A;
        let direction = |value| match value {
            controls::Direction::Left => pigade_layout::Direction::Left,
            controls::Direction::Right => pigade_layout::Direction::Right,
            controls::Direction::Up => pigade_layout::Direction::Up,
            controls::Direction::Down => pigade_layout::Direction::Down,
        };
        self.finish_pointer(cx);
        self.selection_dragging = false;
        self.text_input.cancel();
        window.notify_text_input_changed(&self.focus);
        match action {
            A::Close => {
                self.product_action(product::ProductAction::Layout("hide".into()), window, cx)
            }
            A::FocusMode => {
                self.product_action(product::ProductAction::Layout("focus".into()), window, cx)
            }
            A::Focus(value) => self.focus_neighbor(direction(value), false, false, window, cx),
            A::Swap(value) => self.focus_neighbor(direction(value), false, true, window, cx),
            A::FocusLeft => {
                self.focus_neighbor(pigade_layout::Direction::Left, false, false, window, cx)
            }
            A::FocusRight => {
                self.focus_neighbor(pigade_layout::Direction::Right, false, false, window, cx)
            }
            A::SelectTab(number) | A::TransferToTab(number) => {
                let tab = self
                    .product
                    .catalog
                    .as_ref()
                    .and_then(|catalog| {
                        catalog
                            .tabs
                            .iter()
                            .filter(|tab| Some(tab.workspace_id) == catalog.selection.workspace_id)
                            .nth(usize::from(number - 1))
                    })
                    .map(|tab| tab.id);
                if let Some(tab) = tab {
                    self.product_action(
                        if matches!(action, A::SelectTab(_)) {
                            product::ProductAction::SelectTab(tab)
                        } else {
                            product::ProductAction::TransferTo(tab)
                        },
                        window,
                        cx,
                    );
                }
            }
            A::NextTab | A::PreviousTab => {
                if let Some(catalog) = &self.product.catalog {
                    let tabs = catalog
                        .tabs
                        .iter()
                        .filter(|tab| Some(tab.workspace_id) == catalog.selection.workspace_id)
                        .map(|tab| tab.id)
                        .collect::<Vec<_>>();
                    if let Some(current) = tabs
                        .iter()
                        .position(|tab| Some(*tab) == catalog.selection.tab_id)
                    {
                        let index = if action == A::NextTab {
                            (current + 1) % tabs.len()
                        } else {
                            (current + tabs.len() - 1) % tabs.len()
                        };
                        self.product_action(
                            product::ProductAction::SelectTab(tabs[index]),
                            window,
                            cx,
                        );
                    }
                }
            }
            A::LaunchShell => {
                self.product_action(product::ProductAction::Launch("shell".into()), window, cx)
            }
            A::ResizeLeft | A::ResizeRight => {
                self.keyboard_resize(
                    if action == A::ResizeLeft {
                        pigade_layout::Direction::Left
                    } else {
                        pigade_layout::Direction::Right
                    },
                    cx,
                );
                self.finish_resize(true, cx);
            }
        }
        cx.notify();
    }

    fn queue_command(
        &mut self,
        target: CommandTarget,
        value: Value,
        mouse_after: Option<MouseSnapshot>,
        cx: &mut Context<Self>,
    ) -> bool {
        if let Err(error) = admit_command(
            &mut self.mouse_delivery,
            &mut self.next_request_id,
            &self.tx,
            target,
            value,
            mouse_after,
        ) {
            if error.blocks_input() {
                self.input_error = Some(error.message().into());
            } else {
                self.input_notice = Some(error.message().into());
            }
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
            self.input_notice = Some("Ввод более 16 KiB не принят; разделите текст".into());
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
        match self.tx.try_send(IpcMessage::AcknowledgeKnownRejection {
            target: target.clone(),
            token,
            cleanup: cleanup.clone(),
        }) {
            Ok(()) => true,
            Err(error) => {
                if let Some(cleanup) = cleanup {
                    self.mouse_delivery.cancel_cleanup(target, cleanup.id);
                }
                match error {
                    mpsc::TrySendError::Full(_) => {
                        self.input_notice =
                            Some("Очередь команд занята; подтверждение не принято".into())
                    }
                    mpsc::TrySendError::Disconnected(_) => {
                        self.input_error = Some(AdmissionFailure::Disconnected.message().into())
                    }
                }
                false
            }
        }
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
        if self
            .canvas_bounds
            .is_some_and(|bounds| !bounds.contains(&point))
        {
            return None;
        }
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
        // Hover/drag samples may be superseded before admission. Reserve queue
        // slots for keyboard and button transitions; never discard queued bytes.
        if matches!(action, PointerAction::Motion) && !self.mouse_delivery.has_motion_capacity() {
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
        if self.product.dialog.is_some() {
            return;
        }
        if event.button == MouseButton::Left
            && let Some(bar) = self
                .canvas_bars
                .iter()
                .find(|bar| layout_rect_contains(bar.track, event.position))
                .copied()
        {
            self.finish_pointer(cx);
            self.selection_dragging = false;
            let initial = self.canvas_offset();
            if layout_rect_contains(bar.thumb, event.position) {
                self.canvas_drag = Some(CanvasScrollDrag {
                    bar,
                    initial,
                    start: event.position,
                });
            } else {
                let delta = match bar.axis {
                    pigade_layout::Axis::Horizontal => {
                        f64::from(f32::from(event.position.x))
                            - (bar.thumb.x + bar.thumb.width / 2.)
                    }
                    pigade_layout::Axis::Vertical => {
                        f64::from(f32::from(event.position.y))
                            - (bar.thumb.y + bar.thumb.height / 2.)
                    }
                };
                self.set_canvas_offset(viewport::dragged_offset(bar, initial, delta));
            }
            self.text_input.cancel();
            window.notify_text_input_changed(&self.focus);
            cx.stop_propagation();
            cx.notify();
            return;
        }
        if self
            .canvas_bounds
            .is_some_and(|bounds| !bounds.contains(&event.position))
        {
            return;
        }
        if self.held_modifier(event.modifiers)
            && matches!(event.button, MouseButton::Left | MouseButton::Right)
        {
            if self.product.busy && !self.focus_queue.is_saving() {
                self.input_notice =
                    Some("Раскладка обновляется; повторите жест после завершения операции".into());
                cx.notify();
            }
            if (!self.product.busy || self.focus_queue.is_saving())
                && let Some(index) = self
                    .panes
                    .iter()
                    .position(|pane| pane.bounds.contains(&event.position))
            {
                self.finish_pointer(cx);
                if self
                    .pointer
                    .as_ref()
                    .is_some_and(|route| route.encoder.has_pressed())
                {
                    cx.stop_propagation();
                    return;
                }
                self.selected = index;
                self.input_epoch = self.input_epoch.wrapping_add(1);
                self.selection = None;
                self.selection_dragging = false;
                self.text_input.cancel();
                self.focus.focus(window, cx);
                window.notify_text_input_changed(&self.focus);
                if let Some(pane) = self.selected_pane() {
                    if event.button == MouseButton::Left {
                        self.begin_pane_drag(pane, event.position, cx);
                    } else if let Some((tab, base)) = self.tab_layout()
                        && let Some(geometry) = &self.layout_geometry
                        && let Some((divider, extent)) = tiling::pointer_boundary(
                            &base,
                            geometry,
                            pane,
                            f64::from(f32::from(event.position.x)),
                            f64::from(f32::from(event.position.y)),
                        )
                    {
                        self.resize_gesture = Some(ResizeGesture {
                            button: MouseButton::Right,
                            tab,
                            base,
                            split: divider.split,
                            boundary: divider.boundary,
                            axis: divider.axis,
                            start: event.position,
                            extent,
                        });
                    }
                }
                cx.notify();
            }
            cx.stop_propagation();
            return;
        }
        if event.button == MouseButton::Left
            && !self.product.busy
            && let Some(geometry) = &self.layout_geometry
            && let Some(divider) = geometry
                .dividers
                .iter()
                .find(|divider| {
                    let r = divider.rect;
                    let x = f64::from(f32::from(event.position.x));
                    let y = f64::from(f32::from(event.position.y));
                    x >= r.x - 2.
                        && x <= r.x + r.width + 2.
                        && y >= r.y - 2.
                        && y <= r.y + r.height + 2.
                })
                .copied()
            && let Some((tab, base)) = self.tab_layout()
            && let Some(extent) = tiling::split_extent(geometry, &base, divider.split)
        {
            self.finish_pointer(cx);
            self.selection_dragging = false;
            self.resize_gesture = Some(ResizeGesture {
                button: MouseButton::Left,
                tab,
                base,
                split: divider.split,
                boundary: divider.boundary,
                axis: divider.axis,
                start: event.position,
                extent,
            });
            cx.stop_propagation();
            return;
        }
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
                && !self.product.busy
                && let Some((_, action, _)) = pane_header_buttons(self.panes[index])
                    .into_iter()
                    .find(|(_, _, bounds)| bounds.contains(&event.position))
            {
                let action = if action == "menu" {
                    product::ProductAction::OpenPaneMenu
                } else {
                    product::ProductAction::Layout(action.into())
                };
                self.product_action(action, window, cx);
                cx.stop_propagation();
                return;
            }
            if event.button == MouseButton::Left
                && (!self.product.busy || self.focus_queue.is_saving())
                && event.position.y < self.panes[index].bounds.top() + px(HEADER)
                && let Some(pane) = self.selected_pane()
            {
                self.finish_pointer(cx);
                self.begin_pane_drag(pane, event.position, cx);
                cx.stop_propagation();
                cx.notify();
                return;
            }
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
        if self.editor_drag.take().is_some() {
            cx.stop_propagation();
            return;
        }
        if self.product.dialog.is_some() {
            return;
        }
        if event.button == MouseButton::Left && self.canvas_drag.take().is_some() {
            cx.stop_propagation();
            cx.notify();
            return;
        }
        if event.button == MouseButton::Left && self.pane_drag.is_some() {
            // A compositor can coalesce the last motion until after mouse-up.
            // Commit against the release position, never a stale preview.
            self.update_pane_drag_at(event.position, cx);
        }
        if event.button == MouseButton::Left
            && let Some(drag) = self.pane_drag.take()
        {
            self.layout_preview = None;
            self.sync_panel_visibility();
            self.filter_product_views();
            if let Some((command, _)) = drag.drop.filter(|_| drag.travelled) {
                self.layout_command(command, cx);
            } else {
                self.persist_focus(cx);
            }
            cx.stop_propagation();
            cx.notify();
            return;
        }
        if self
            .resize_gesture
            .as_ref()
            .is_some_and(|gesture| event.button == gesture.button)
        {
            self.resize_at(event.position, cx);
            self.finish_resize(true, cx);
            cx.stop_propagation();
            return;
        }
        if event.button == MouseButton::Left && self.selection_dragging {
            self.extend_selection(event.position, cx);
            self.selection_dragging = false;
            self.persist_focus(cx);
            return;
        }
        self.persist_focus(cx);
        if let Some(button) = pointer_button(event.button) {
            self.pointer_event(
                PointerAction::Release(button),
                event.position,
                event.modifiers,
                cx,
            );
        }
    }
    fn pointer_move(
        &mut self,
        event: &MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Only real pointer movement changes hover focus. Repaint/layout changes
        // beneath a stationary cursor must not undo a keyboard focus command.
        let moved = self.hover_position.replace(event.position) != Some(event.position);
        if let Some((id, index, anchor)) = self.editor_drag {
            if event.pressed_button != Some(MouseButton::Left) {
                self.editor_drag = None;
            } else if let Some(dialog) = &mut self.product.dialog
                && dialog.id == id
                && let Some(layout) = self
                    .editor_layouts
                    .get(&index)
                    .filter(|layout| layout.dialog_id == id)
                && let Some(buffer) = dialog.fields.get_mut(index)
            {
                let position = layout.index_for_point(event.position);
                buffer.select_bytes(
                    anchor.min(position)..anchor.max(position),
                    position < anchor,
                );
            }
            cx.stop_propagation();
            cx.notify();
            return;
        }
        if self.product.dialog.is_some() {
            return;
        }
        if let Some(drag) = &self.canvas_drag {
            if event.pressed_button == Some(MouseButton::Left) {
                let delta = match drag.bar.axis {
                    pigade_layout::Axis::Horizontal => {
                        f64::from(f32::from(event.position.x - drag.start.x))
                    }
                    pigade_layout::Axis::Vertical => {
                        f64::from(f32::from(event.position.y - drag.start.y))
                    }
                };
                self.set_canvas_offset(viewport::dragged_offset(drag.bar, drag.initial, delta));
            } else {
                self.canvas_drag = None;
            }
            cx.stop_propagation();
            cx.notify();
            return;
        }
        if self.pane_drag.is_some() {
            self.update_pane_drag(event, cx);
            return;
        }
        if let Some(gesture) = &self.resize_gesture {
            if event.pressed_button != Some(gesture.button) {
                self.finish_resize(false, cx);
                return;
            }
            self.resize_at(event.position, cx);
            cx.stop_propagation();
            return;
        }
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
        if moved
            && event.pressed_button.is_none()
            && window.is_window_active()
            && !self.prefix
            && self.layout_mode.is_none()
            && self.text_input.preedit.is_empty()
            && self
                .canvas_bounds
                .is_some_and(|bounds| bounds.contains(&event.position))
            && let Some(index) = self
                .panes
                .iter()
                .position(|pane| pane.bounds.contains(&event.position))
            && index != self.selected
            && self.state.views.get(index).is_some()
        {
            self.finish_pointer(cx);
            self.selected = index;
            self.selection = None;
            self.text_input.cancel();
            self.input_epoch = self.input_epoch.wrapping_add(1);
            window.notify_text_input_changed(&self.focus);
            self.focus.focus(window, cx);
            self.reset_cursor_blink(cx);
            self.persist_focus(cx);
        }
        self.pointer_event(PointerAction::Motion, event.position, event.modifiers, cx);
    }
    fn pointer_wheel(
        &mut self,
        event: &ScrollWheelEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.product.dialog.is_some() {
            return;
        }
        let over_bar = self
            .canvas_bars
            .iter()
            .find(|bar| layout_rect_contains(bar.track, event.position))
            .map(|bar| bar.axis);
        let over_canvas = self
            .canvas_bounds
            .is_some_and(|bounds| bounds.contains(&event.position));
        if (over_canvas && (self.prefix || event.modifiers.shift)) || over_bar.is_some() {
            let (mut dx, mut dy) = match event.delta {
                ScrollDelta::Lines(delta) => (f64::from(delta.x) * 40., f64::from(delta.y) * 40.),
                ScrollDelta::Pixels(delta) => {
                    (f64::from(f32::from(delta.x)), f64::from(f32::from(delta.y)))
                }
            };
            if (event.modifiers.alt || over_bar == Some(pigade_layout::Axis::Horizontal))
                && dx == 0.
            {
                dx = dy;
                dy = 0.;
            }
            self.finish_pointer(cx);
            self.text_input.cancel();
            window.notify_text_input_changed(&self.focus);
            self.pan_canvas(-dx, -dy, cx);
            cx.stop_propagation();
            return;
        }
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
        if self.product.dialog.is_some()
            || self.pane_drag.is_some()
            || self.resize_gesture.is_some()
        {
            return;
        }
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
        if self.input_notice.take().is_some() {
            cx.notify();
        }
        if self.clipboard_pending {
            self.input_epoch = self.input_epoch.wrapping_add(1);
        }
        let k = &event.keystroke;
        if k.key == "escape" && self.deferred_product_action.take().is_some() {
            self.product.notice = Some("Ожидающее действие отменено".into());
            cx.notify();
            cx.stop_propagation();
            return;
        }
        if self
            .product
            .dialog
            .as_ref()
            .is_some_and(|dialog| dialog.kind == product::DialogKind::WorkspaceGit)
            && matches!(k.key.as_str(), "pageup" | "pagedown" | "home" | "end")
        {
            let handle = &self.product.git_scroll;
            let mut offset = handle.offset();
            let end = -handle.max_offset().y;
            offset.y = match k.key.as_str() {
                "home" => px(0.),
                "end" => end,
                "pageup" => (offset.y + px(280.)).min(px(0.)),
                _ => (offset.y - px(280.)).max(end),
            };
            handle.set_offset(offset);
            cx.notify();
            cx.stop_propagation();
            return;
        }
        if self.product.dialog.as_ref().is_some_and(|dialog| {
            matches!(
                dialog.kind,
                product::DialogKind::Manage
                    | product::DialogKind::WorkspaceGit
                    | product::DialogKind::StopSession
                    | product::DialogKind::Sessions
                    | product::DialogKind::Prompts
                    | product::DialogKind::AgentProfiles
                    | product::DialogKind::Layouts
                    | product::DialogKind::Recipe
                    | product::DialogKind::TemplateRun
                    | product::DialogKind::TemplateRunContext
                    | product::DialogKind::TemplateRuns
            )
        }) {
            if event.is_held && matches!(k.key.as_str(), "enter" | "delete") {
                cx.stop_propagation();
                return;
            }
            let entries = self.product.menu_entries();
            let count = entries.len();
            let selected = self
                .product
                .dialog
                .as_ref()
                .map_or(0, |dialog| dialog.active_field)
                .min(count.saturating_sub(1));
            let action =
                match k.key.as_str() {
                    "enter"
                        if (k.modifiers.control || k.modifiers.platform)
                            && self.product.dialog.as_ref().is_some_and(|dialog| {
                                dialog.kind == product::DialogKind::Recipe
                            }) =>
                    {
                        Some(product::ProductAction::SubmitDialog)
                    }
                    "enter" if k.modifiers.control || k.modifiers.platform => {
                        entries.get(selected).map(|(_, action)| match action {
                            product::ProductAction::ApplyLayout(id) => {
                                product::ProductAction::NewTabFromTemplate(*id)
                            }
                            _ => action.clone(),
                        })
                    }
                    "r" if (k.modifiers.control || k.modifiers.platform)
                        && self.product.dialog.as_ref().is_some_and(|dialog| {
                            matches!(
                                dialog.kind,
                                product::DialogKind::Prompts | product::DialogKind::AgentProfiles
                            )
                        }) =>
                    {
                        Some(
                            if self
                                .product
                                .dialog
                                .as_ref()
                                .is_some_and(|dialog| dialog.kind == product::DialogKind::Prompts)
                            {
                                product::ProductAction::ReloadPrompts
                            } else {
                                product::ProductAction::ReloadAgentProfiles
                            },
                        )
                    }
                    "e" if k.modifiers.control || k.modifiers.platform => {
                        entries.get(selected).and_then(|(_, action)| match action {
                            product::ProductAction::UsePrompt(id) => {
                                Some(product::ProductAction::EditPrompt(*id))
                            }
                            product::ProductAction::LaunchAgentProfile(id) => {
                                Some(product::ProductAction::EditAgentProfile(*id))
                            }
                            product::ProductAction::ApplyLayout(id) => {
                                Some(product::ProductAction::ConfigureTemplate(*id))
                            }
                            _ => None,
                        })
                    }
                    "escape"
                        if self.product.dialog.as_ref().is_some_and(|dialog| {
                            dialog.kind == product::DialogKind::TemplateRunContext
                        }) =>
                    {
                        Some(product::ProductAction::ReturnTemplateRun)
                    }
                    "escape" => Some(product::ProductAction::CloseDialog),
                    "enter" => entries.get(selected).map(|(_, action)| action.clone()),
                    "delete"
                        if self.product.dialog.as_ref().is_some_and(|dialog| {
                            matches!(
                                dialog.kind,
                                product::DialogKind::Prompts | product::DialogKind::AgentProfiles
                            )
                        }) =>
                    {
                        entries.get(selected).and_then(|(_, action)| match action {
                            product::ProductAction::UsePrompt(id) => {
                                Some(product::ProductAction::DeletePrompt(*id))
                            }
                            product::ProductAction::LaunchAgentProfile(id) => {
                                Some(product::ProductAction::DeleteAgentProfile(*id))
                            }
                            _ => None,
                        })
                    }
                    "delete" if k.modifiers.shift => {
                        entries.get(selected).and_then(|(_, action)| match action {
                            product::ProductAction::ApplyLayout(id) => {
                                Some(product::ProductAction::DeleteLayout(*id))
                            }
                            _ => None,
                        })
                    }
                    _ => None,
                };
            if let Some(action) = action {
                self.product_action(action, window, cx);
            } else if count > 0 && matches!(k.key.as_str(), "up" | "down" | "tab") {
                let previous = k.key == "up" || k.key == "tab" && k.modifiers.shift;
                if let Some(dialog) = &mut self.product.dialog {
                    dialog.active_field = (selected + if previous { count - 1 } else { 1 }) % count;
                    let kind = dialog.kind;
                    let selected = dialog.active_field;
                    if kind == product::DialogKind::Sessions {
                        let row = self.product.session_action_row(selected);
                        self.product.layout_scroll.scroll_to_item(row);
                    } else if matches!(
                        kind,
                        product::DialogKind::Layouts
                            | product::DialogKind::AgentProfiles
                            | product::DialogKind::Prompts
                    ) {
                        self.product.layout_scroll.scroll_to_item(selected);
                    }
                }
                cx.notify();
            }
            cx.stop_propagation();
            return;
        }
        if self
            .product
            .dialog
            .as_ref()
            .is_some_and(|dialog| dialog.kind == product::DialogKind::Controls)
        {
            if k.key == "escape" || k.key == "enter" {
                self.product_action(product::ProductAction::CloseDialog, window, cx);
            } else if !k.modifiers.control
                && !k.modifiers.alt
                && !k.modifiers.platform
                && !event.is_held
                && let Ok(number) = k.key.parse::<usize>()
                && let Some(modifier) = number
                    .checked_sub(1)
                    .and_then(|index| controls::Modifier::ALL.get(index))
            {
                self.product_action(
                    product::ProductAction::SetCommandModifier(*modifier),
                    window,
                    cx,
                );
            }
            cx.stop_propagation();
            return;
        }
        if self.canvas_close_requested && k.key == "escape" {
            if k.modifiers.control && k.modifiers.shift {
                window.remove_window();
            } else {
                self.canvas_close_requested = false;
            }
            cx.notify();
            cx.stop_propagation();
            return;
        }
        if k.key == "escape"
            && let Some(drag) = self.canvas_drag.take()
        {
            self.set_canvas_offset(drag.initial);
            cx.notify();
            cx.stop_propagation();
            return;
        }
        if k.key == "escape" && (self.resize_gesture.is_some() || self.pane_drag.is_some()) {
            self.cancel_pane_drag(cx);
            self.finish_resize(false, cx);
            cx.stop_propagation();
            return;
        }
        if self.pane_drag.is_some() || self.resize_gesture.is_some() {
            cx.stop_propagation();
            return;
        }
        if self
            .product
            .dialog
            .as_ref()
            .is_some_and(|dialog| dialog.kind == product::DialogKind::History)
        {
            let control = k.modifiers.control || k.modifiers.platform;
            let action = match k.key.as_str() {
                "f3" => Some(if k.modifiers.shift {
                    product::ProductAction::HistorySearch
                } else {
                    product::ProductAction::HistoryNextMatch
                }),
                "r" if control => Some(product::ProductAction::HistoryRefresh),
                "c" if control && k.modifiers.shift => {
                    Some(product::ProductAction::HistoryCopyPage)
                }
                "pageup" if control => Some(product::ProductAction::HistoryStart),
                "pagedown" if control => Some(product::ProductAction::HistoryEnd),
                _ => None,
            };
            if let Some(action) = action {
                self.product_action(action, window, cx);
                cx.stop_propagation();
                return;
            }
            if control && k.key == "f" {
                if let Some(dialog) = &mut self.product.dialog {
                    dialog.fields[dialog.active_field].cancel_mark();
                    dialog.active_field = 0;
                    dialog.fields[0].select_all();
                }
                self.editor_changed(window, cx);
                cx.stop_propagation();
                return;
            }
        }
        if self.product.dialog.as_ref().is_some_and(|dialog| {
            dialog.kind == product::DialogKind::History
                && self
                    .product
                    .dialog
                    .as_ref()
                    .is_some_and(|dialog| dialog.active_field == 1)
        }) {
            self.history_key(event, window, cx);
            return;
        }
        if self
            .product
            .dialog
            .as_ref()
            .is_some_and(|dialog| dialog.kind == product::DialogKind::Appearance)
        {
            let action = match k.key.as_str() {
                "1" if !k.modifiers.control && !k.modifiers.alt && !k.modifiers.platform => {
                    Some(product::ProductAction::SetTheme(appearance::Theme::Dark))
                }
                "2" if !k.modifiers.control && !k.modifiers.alt && !k.modifiers.platform => {
                    Some(product::ProductAction::SetTheme(appearance::Theme::Light))
                }
                "r" if k.modifiers.control || k.modifiers.platform => {
                    Some(product::ProductAction::ReloadAppearance)
                }
                "r" if !k.modifiers.alt => Some(product::ProductAction::ToggleReducedMotion),
                "+" | "=" if !k.modifiers.control && !k.modifiers.alt && !k.modifiers.platform => {
                    Some(product::ProductAction::AdjustTerminalFont(1))
                }
                "-" if !k.modifiers.control && !k.modifiers.alt && !k.modifiers.platform => {
                    Some(product::ProductAction::AdjustTerminalFont(-1))
                }
                "0" if !k.modifiers.control && !k.modifiers.alt && !k.modifiers.platform => {
                    Some(product::ProductAction::ResetTerminalFont)
                }
                "escape" | "enter" => Some(product::ProductAction::CloseDialog),
                _ => None,
            };
            if !event.is_held
                && let Some(action) = action
            {
                self.product_action(action, window, cx);
            }
            cx.stop_propagation();
            return;
        }
        if (k.modifiers.control || k.modifiers.platform)
            && k.key == "r"
            && self
                .product
                .dialog
                .as_ref()
                .is_some_and(|dialog| dialog.kind == product::DialogKind::CloseTab)
        {
            self.product_action(product::ProductAction::InspectCloseTab, window, cx);
            cx.stop_propagation();
            return;
        }
        if k.modifiers.control
            && k.modifiers.shift
            && self
                .product
                .dialog
                .as_ref()
                .is_some_and(prompt_flow::accepts_prompt)
        {
            let action = match k.key.as_str() {
                "p" => Some(product::ProductAction::OpenPrompts),
                "b" => Some(product::ProductAction::SaveDraftAsPrompt),
                _ => None,
            };
            if let Some(action) = action {
                self.product_action(action, window, cx);
                cx.stop_propagation();
                return;
            }
        }
        if self
            .product
            .dialog
            .as_ref()
            .is_some_and(|dialog| dialog.kind == product::DialogKind::AgentProfile)
            && (k.modifiers.control || k.modifiers.platform)
        {
            let action = match k.key.as_str() {
                "1" => Some(product::ProductAction::SetProfileProvider("claude".into())),
                "2" => Some(product::ProductAction::SetProfileProvider("codex".into())),
                "3" => Some(product::ProductAction::SetProfileProvider(
                    "opencode".into(),
                )),
                "m" => Some(product::ProductAction::ToggleProfileContext),
                _ => None,
            };
            if let Some(action) = action {
                self.product_action(action, window, cx);
                cx.stop_propagation();
                return;
            }
        }
        if self.product.dialog.is_some() {
            if k.modifiers.control
                && self
                    .product
                    .dialog
                    .as_ref()
                    .is_some_and(|dialog| dialog.kind == product::DialogKind::LaunchReview)
            {
                let action = match k.key.as_str() {
                    "s" if k.modifiers.shift => Some(product::ProductAction::SaveLaunchAsProfile),
                    "r" => Some(if k.modifiers.shift {
                        product::ProductAction::ReloadMemory
                    } else {
                        product::ProductAction::RefreshLaunchPreview
                    }),
                    "m" => Some(product::ProductAction::ToggleMemory),
                    "d" => Some(product::ProductAction::ToggleContextDelivery),
                    _ => None,
                };
                if let Some(action) = action {
                    self.product_action(action, window, cx);
                    cx.stop_propagation();
                    return;
                }
            }
            use text_edit::Movement as M;
            let control = k.modifiers.control || k.modifiers.platform;
            if control && k.key == "v" {
                if self.product.dialog.as_ref().is_some_and(|dialog| {
                    dialog.kind == product::DialogKind::History && dialog.active_field == 1
                }) {
                    cx.stop_propagation();
                    return;
                }
                let scope = self.product.dialog.as_ref().and_then(|dialog| {
                    dialog.fields.get(dialog.active_field).map(|buffer| {
                        (
                            dialog.id,
                            dialog.active_field,
                            buffer.revision(),
                            buffer.selection(),
                        )
                    })
                });
                let clipboard = cx.read_from_clipboard_async();
                cx.spawn(async move |weak, cx| {
                    if let Ok(Some(item)) = clipboard.await {
                        let _ = weak.update_in(cx, |this, window, cx| {
                            if this.product.dialog.as_ref().and_then(|dialog| {
                                dialog.fields.get(dialog.active_field).map(|buffer| {
                                    (
                                        dialog.id,
                                        dialog.active_field,
                                        buffer.revision(),
                                        buffer.selection(),
                                    )
                                })
                            }) == scope
                                && let Some(text) = item.text()
                            {
                                this.product.insert_text(&text);
                                this.editor_changed(window, cx);
                            }
                        });
                    }
                })
                .detach();
                cx.stop_propagation();
                return;
            }
            if control && matches!(k.key.as_str(), "c" | "x") {
                if let Some(buffer) = self.product.active_editor_mut() {
                    let text = buffer.selected_text().to_owned();
                    if !text.is_empty() {
                        cx.write_to_clipboard(ClipboardItem::new_string(text));
                        if k.key == "x" {
                            buffer.delete(false);
                        }
                    }
                }
                self.editor_changed(window, cx);
                cx.stop_propagation();
                return;
            }
            if self
                .product
                .active_editor()
                .is_some_and(|buffer| buffer.marked().is_some())
            {
                if k.key == "escape" {
                    if let Some(buffer) = self.product.active_editor_mut() {
                        buffer.cancel_mark();
                    }
                    self.editor_changed(window, cx);
                    cx.stop_propagation();
                }
                return;
            }
            if self.product.dialog.as_ref().is_some_and(|dialog| {
                matches!(
                    dialog.kind,
                    product::DialogKind::Palette | product::DialogKind::PaneMenu
                )
            }) && !control
                && !k.modifiers.alt
                && matches!(
                    k.key.as_str(),
                    "up" | "down" | "pageup" | "pagedown" | "tab"
                )
            {
                let count = self.product.palette_entries().len();
                if count > 0 {
                    let backward = matches!(k.key.as_str(), "up" | "pageup")
                        || k.key == "tab" && k.modifiers.shift;
                    let step = if matches!(k.key.as_str(), "pageup" | "pagedown") {
                        8
                    } else {
                        1
                    };
                    self.product.palette_index = if backward {
                        self.product.palette_index.saturating_sub(step)
                    } else {
                        (self.product.palette_index + step).min(count - 1)
                    };
                    self.product
                        .palette_scroll
                        .scroll_to_item(self.product.palette_index);
                    cx.notify();
                }
                cx.stop_propagation();
                return;
            }
            if matches!(k.key.as_str(), "up" | "down" | "pageup" | "pagedown") {
                if let Some(dialog) = &mut self.product.dialog
                    && let Some(layout) = self
                        .editor_layouts
                        .get(&dialog.active_field)
                        .filter(|layout| layout.dialog_id == dialog.id)
                    && let Some(buffer) = dialog.fields.get_mut(dialog.active_field)
                    && let Some(caret) = layout.bounds_for_range(buffer.cursor()..buffer.cursor())
                {
                    let x = self
                        .editor_preferred_x
                        .filter(|(id, index, _)| *id == dialog.id && *index == dialog.active_field)
                        .map_or(caret.left(), |(_, _, x)| x);
                    self.editor_preferred_x = Some((dialog.id, dialog.active_field, x));
                    let distance = if matches!(k.key.as_str(), "pageup" | "pagedown") {
                        layout.bounds.size.height
                    } else {
                        caret.size.height
                    };
                    let y = caret.top()
                        + caret.size.height / 2.
                        + if matches!(k.key.as_str(), "up" | "pageup") {
                            -distance
                        } else {
                            distance
                        };
                    let target = layout.index_for_point(point(x, y));
                    let selected = buffer.selection();
                    let anchor = if k.modifiers.shift {
                        if buffer.reversed() {
                            selected.end
                        } else {
                            selected.start
                        }
                    } else {
                        target
                    };
                    buffer.select_bytes(anchor.min(target)..anchor.max(target), target < anchor);
                    self.editor_changed(window, cx);
                    cx.stop_propagation();
                    return;
                }
            } else {
                self.editor_preferred_x = None;
            }
            let movement = match k.key.as_str() {
                "left" => Some(if control { M::WordLeft } else { M::Left }),
                "right" => Some(if control { M::WordRight } else { M::Right }),
                "up" => Some(M::Up),
                "down" => Some(M::Down),
                "home" => Some(if control {
                    M::DocumentStart
                } else {
                    M::LineStart
                }),
                "end" => Some(if control { M::DocumentEnd } else { M::LineEnd }),
                _ => None,
            };
            if let Some(movement) = movement {
                if let Some(buffer) = self.product.active_editor_mut() {
                    buffer.navigate(movement, k.modifiers.shift);
                }
            } else if control && k.key == "a" {
                if let Some(buffer) = self.product.active_editor_mut() {
                    buffer.select_all();
                }
            } else if control && matches!(k.key.as_str(), "z" | "y") {
                if let Some(buffer) = self.product.active_editor_mut() {
                    if k.key == "y" || k.modifiers.shift {
                        buffer.redo();
                    } else {
                        buffer.undo();
                    }
                }
            } else {
                match k.key.as_str() {
                    "escape" => {
                        self.product_action(product::ProductAction::CloseDialog, window, cx)
                    }
                    "backspace" => {
                        if let Some(buffer) = self.product.active_editor_mut() {
                            buffer.backspace(control);
                        }
                    }
                    "delete" => {
                        if let Some(buffer) = self.product.active_editor_mut() {
                            buffer.delete(control);
                        }
                    }
                    "tab" => {
                        if let Some(dialog) = &mut self.product.dialog {
                            let count = dialog.fields.len();
                            if count > 0 {
                                dialog.fields[dialog.active_field].unmark();
                                dialog.active_field = (dialog.active_field
                                    + if k.modifiers.shift { count - 1 } else { 1 })
                                    % count;
                            }
                        }
                    }
                    "enter" if !control && self.product.active_multiline() => {
                        self.product.insert_text("\n")
                    }
                    "enter" if k.modifiers.shift => self.product.insert_text("\n"),
                    "enter" => {
                        self.product_action(product::ProductAction::SubmitDialog, window, cx)
                    }
                    "s" if control => {
                        self.product_action(product::ProductAction::SubmitDialog, window, cx)
                    }
                    _ => return,
                }
            }
            self.editor_changed(window, cx);
            cx.stop_propagation();
            return;
        }
        let direction = match k.key.as_str() {
            "h" | "left" => Some(pigade_layout::Direction::Left),
            "j" | "down" => Some(pigade_layout::Direction::Down),
            "k" | "up" => Some(pigade_layout::Direction::Up),
            "l" | "right" => Some(pigade_layout::Direction::Right),
            _ => None,
        };
        if let Some(mode) = self.layout_mode {
            if mode == LayoutMode::Pan {
                match k.key.as_str() {
                    "escape" => self.layout_mode = None,
                    "enter" => {
                        self.canvas_reveal = self.selected_pane();
                        self.layout_mode = None;
                    }
                    "home" => self.set_canvas_offset(Default::default()),
                    "end" => self.pan_canvas(1e9, 1e9, cx),
                    "pageup" => self.pan_canvas(0., -300., cx),
                    "pagedown" => self.pan_canvas(0., 300., cx),
                    _ => {
                        if let Some(direction) = direction {
                            let step = if k.modifiers.shift { 240. } else { 60. };
                            let (dx, dy) = match direction {
                                pigade_layout::Direction::Left => (-step, 0.),
                                pigade_layout::Direction::Right => (step, 0.),
                                pigade_layout::Direction::Up => (0., -step),
                                pigade_layout::Direction::Down => (0., step),
                            };
                            self.pan_canvas(dx, dy, cx);
                        }
                    }
                }
                cx.notify();
                cx.stop_propagation();
                return;
            }
            if k.key == "escape" {
                self.finish_resize(false, cx);
            } else if mode == LayoutMode::Resize {
                if k.key == "enter" {
                    self.finish_resize(true, cx);
                } else if let Some(direction) = direction {
                    self.keyboard_resize(direction, cx);
                }
            } else {
                self.layout_mode = None;
                if let Some(direction) = direction {
                    self.focus_neighbor(direction, false, true, window, cx);
                }
            }
            cx.stop_propagation();
            return;
        }
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
            if let Some(direction) = direction {
                self.focus_neighbor(direction, k.modifiers.shift, false, window, cx);
                cx.stop_propagation();
                return;
            }
            if matches!(k.key.as_str(), "w" | "r" | "g") {
                self.layout_mode = Some(if k.key == "w" {
                    LayoutMode::Swap
                } else if k.key == "g" {
                    LayoutMode::Pan
                } else {
                    LayoutMode::Resize
                });
                cx.notify();
                cx.stop_propagation();
                return;
            }
            let action = match k.key.as_str() {
                "p" => Some(product::ProductAction::OpenPalette),
                "e" => Some(if k.modifiers.shift {
                    product::ProductAction::EditProjectContext
                } else {
                    product::ProductAction::EditContext
                }),
                "o" => Some(if k.modifiers.shift {
                    product::ProductAction::AddProject
                } else {
                    product::ProductAction::OpenGenerations
                }),
                "n" => Some(if k.modifiers.shift {
                    product::ProductAction::CreateProject
                } else {
                    product::ProductAction::CreateWorkspace
                }),
                "i" => Some(product::ProductAction::OpenHistory),
                "b" => Some(product::ProductAction::InspectConversation),
                "m" => Some(product::ProductAction::OpenTransfer),
                "t" => Some(product::ProductAction::CreateTab),
                "d" => Some(product::ProductAction::OpenLayouts),
                "enter" => Some(if k.modifiers.shift {
                    product::ProductAction::ReviewLaunch("shell".into())
                } else {
                    product::ProductAction::Launch("shell".into())
                }),
                "s" => Some(product::ProductAction::Layout("split_down".into())),
                "v" => Some(product::ProductAction::Layout("split_right".into())),
                "f" => Some(product::ProductAction::Layout("focus".into())),
                "=" => Some(product::ProductAction::Layout("equalize".into())),
                "z" => Some(product::ProductAction::Layout(
                    if k.modifiers.shift { "redo" } else { "undo" }.into(),
                )),
                _ => None,
            };
            if let Some(action) = action {
                self.product_action(action, window, cx);
                cx.stop_propagation();
                return;
            }
            if k.key == "c" {
                self.copy_selection(cx);
            }
            if k.key == "a" {
                self.select_visible(cx);
            }
            if let Ok(index) = k.key.parse::<usize>()
                && (1..=9).contains(&index)
                && let Some(catalog) = &self.product.catalog
                && let Some(tab) = catalog
                    .tabs
                    .iter()
                    .filter(|tab| Some(tab.workspace_id) == catalog.selection.workspace_id)
                    .nth(index - 1)
            {
                self.product_action(product::ProductAction::SelectTab(tab.id), window, cx);
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
        if !event.prefer_character_input
            && let Some(action) = controls::classify(
                self.product.command_modifier,
                &k.key,
                input::Modifiers {
                    control: k.modifiers.control,
                    alt: k.modifiers.alt,
                    shift: k.modifiers.shift,
                    platform: k.modifiers.platform,
                },
            )
        {
            let repeating = matches!(
                action,
                controls::Action::Focus(_)
                    | controls::Action::Swap(_)
                    | controls::Action::FocusLeft
                    | controls::Action::FocusRight
                    | controls::Action::ResizeLeft
                    | controls::Action::ResizeRight
            );
            if !event.is_held || repeating {
                self.control_action(action, window, cx);
            }
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
        self.projection_input = Some((bounds, cell, scale));
        self.canvas_bars.clear();
        self.canvas_bounds = Some(bounds);
        let projected = self.tab_layout().and_then(|(tab, layout)| {
            let focus_mode = self
                .pane_drag
                .as_ref()
                .filter(|drag| drag.moved)
                .map_or(layout.focus_mode, |drag| drag.base.focus_mode);
            let focused = layout.focused;
            let state = pigade_layout::LayoutState {
                tabs: [(tab, layout)].into_iter().collect(),
            };
            let outer = pigade_layout::Rect {
                x: f64::from(f32::from(bounds.origin.x)),
                y: f64::from(f32::from(bounds.origin.y)),
                width: f64::from(f32::from(bounds.size.width)).max(1.),
                height: f64::from(f32::from(bounds.size.height)).max(1.),
            };
            let scale = f64::from(valid_scale(scale));
            let minimum = pigade_layout::Size {
                width: (f64::from(f32::from(cell.width)) * scale).ceil() / scale * 40.
                    + 16.
                    + 2. / scale,
                height: (f64::from(f32::from(cell.height)) * scale).ceil() / scale * 8.
                    + f64::from(HEADER)
                    + 16.
                    + 2. / scale,
            };
            let mut content = outer;
            let mut geometry = state.project(tab, content, minimum, f64::from(GAP)).ok()?;
            let mut horizontal = false;
            let mut vertical = false;
            // Reserving one scrollbar can make the other necessary. At most
            // two additions reach a stable viewport; no tree mutation occurs.
            for _ in 0..3 {
                let next_horizontal = horizontal || geometry.bounds.width > content.width;
                let next_vertical = vertical || geometry.bounds.height > content.height;
                if next_horizontal == horizontal && next_vertical == vertical {
                    break;
                }
                horizontal = next_horizontal;
                vertical = next_vertical;
                content.width = (outer.width - if vertical { 24. } else { 0. }).max(1.);
                content.height = (outer.height - if horizontal { 24. } else { 0. }).max(1.);
                geometry = state.project(tab, content, minimum, f64::from(GAP)).ok()?;
            }
            let revision = self
                .product
                .catalog
                .as_ref()
                .map(|catalog| catalog.layout.revision);
            let same_mode = self.canvas_tab == Some(tab) && self.canvas_focus_mode == focus_mode;
            let reveal = self.canvas_reveal.take().or_else(|| {
                (same_mode
                    && self.canvas_layout_revision.is_some()
                    && self.canvas_layout_revision != revision)
                    .then_some(focused)
                    .flatten()
            });
            if !same_mode {
                self.canvas_focus_offset = Default::default();
            }
            self.canvas_tab = Some(tab);
            self.canvas_focus_mode = focus_mode;
            self.canvas_layout_revision = revision;
            let saved = self
                .product
                .catalog
                .as_ref()
                .and_then(|catalog| catalog.selection.window.canvas_offsets.get(&tab))
                .copied()
                .unwrap_or_default();
            self.canvas_offsets.entry(tab).or_insert(saved);
            let extent = geometry.bounds;
            let (geometry, offset) =
                viewport::project(geometry, content, self.canvas_offset(), scale, reveal);
            if !self.pane_drag.as_ref().is_some_and(|drag| drag.moved) {
                self.set_canvas_offset(offset);
            }
            self.canvas_bounds = Some(layout_bounds(content));
            self.canvas_bars = [
                viewport::scrollbar(
                    content,
                    extent.width,
                    offset.x,
                    pigade_layout::Axis::Horizontal,
                ),
                viewport::scrollbar(
                    content,
                    extent.height,
                    offset.y,
                    pigade_layout::Axis::Vertical,
                ),
            ]
            .into_iter()
            .flatten()
            .collect();
            Some(geometry)
        });
        self.layout_geometry = projected.clone();
        self.panes = if let Some(projected) = projected {
            self.state
                .views
                .iter()
                .map(|view| {
                    let floating = self
                        .pane_drag
                        .as_ref()
                        .filter(|drag| drag.moved && drag.pane.to_string() == view.session)
                        .and_then(|drag| {
                            pointer_layout::floating_rect(
                                drag.source,
                                f64::from(f32::from(drag.start.x)),
                                f64::from(f32::from(drag.start.y)),
                                f64::from(f32::from(drag.position.x)),
                                f64::from(f32::from(drag.position.y)),
                            )
                        });
                    match floating {
                        Some(rect) => PaneGeometry::new(layout_bounds(rect), cell, scale),
                        None => {
                            PaneGeometry::from_projection(&projected, &view.session, cell, scale)
                        }
                    }
                })
                .collect()
        } else if self.product.catalog.is_some() {
            Vec::new()
        } else {
            project_panels(bounds, self.state.views.len(), scale)
                .into_iter()
                .map(|bounds| PaneGeometry::new(bounds, cell, scale))
                .collect()
        };
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
                if self
                    .canvas_bounds
                    .is_none_or(|bounds| pixel_bounds_intersect(bounds, pane.bounds))
                    && !self.pane_drag.as_ref().is_some_and(|drag| drag.moved)
                    && self.state.error.is_none()
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

fn layout_bounds(rect: pigade_layout::Rect) -> Bounds<Pixels> {
    Bounds::new(
        point(px(rect.x as f32), px(rect.y as f32)),
        size(px(rect.width as f32), px(rect.height as f32)),
    )
}

fn layout_rect_contains(rect: pigade_layout::Rect, position: Point<Pixels>) -> bool {
    layout_bounds(rect).contains(&position)
}

fn pixel_bounds_intersect(a: Bounds<Pixels>, b: Bounds<Pixels>) -> bool {
    a.size.width > px(0.)
        && a.size.height > px(0.)
        && b.size.width > px(0.)
        && b.size.height > px(0.)
        && a.left() < b.right()
        && a.right() > b.left()
        && a.top() < b.bottom()
        && a.bottom() > b.top()
}

fn paint_canvas_scrollbar(bar: viewport::Scrollbar, colors: chrome::Colors, window: &mut Window) {
    window.paint_quad(fill(layout_bounds(bar.track), rgb(colors.canvas_track)));
    let mut thumb = bar.thumb;
    match bar.axis {
        pigade_layout::Axis::Horizontal => {
            thumb.y += 9.;
            thumb.height = 6.;
        }
        pigade_layout::Axis::Vertical => {
            thumb.x += 9.;
            thumb.width = 6.;
        }
    }
    window.paint_quad(fill(layout_bounds(thumb), rgb(colors.scroll_thumb)));
}
fn valid_scale(scale: f32) -> f32 {
    if scale.is_finite() && scale > 0. {
        scale
    } else {
        1.
    }
}
impl PaneGeometry {
    /// Preserve one slot per view even when focus mode hides every other pane.
    /// Compressing this vector would route hits and resizes to a different CLI.
    fn from_projection(
        geometry: &pigade_layout::CanvasGeometry,
        session: &str,
        metrics: Size<Pixels>,
        scale: f32,
    ) -> Self {
        let bounds = geometry
            .panes
            .iter()
            .find(|pane| pane.pane.to_string() == session)
            .map(|pane| layout_bounds(pane.rect))
            .unwrap_or_default();
        Self::new(bounds, metrics, scale)
    }

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
        if self.bounds.size.width <= px(0.) || self.bounds.size.height <= px(0.) {
            return None;
        }
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
        if self.product.dialog.is_some() {
            return !self.product.dialog.as_ref().is_some_and(|dialog| {
                dialog.kind == product::DialogKind::History && dialog.active_field == 1
            }) && self.product.active_editor().is_some();
        }
        (!self.prefix
            && self.layout_mode.is_none()
            && self.pane_drag.is_none()
            && self.resize_gesture.is_none())
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
        if self.product.dialog.is_some() {
            let (text, actual) = self.product.active_editor()?.text_for_utf16(range);
            *actual_range = Some(actual);
            return Some(text);
        }
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
        if self.product.dialog.is_some() {
            let buffer = self.product.active_editor()?;
            return Some(UTF16Selection {
                range: buffer.selected_utf16(),
                reversed: buffer.reversed(),
            });
        }
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
        if self.product.dialog.is_some() {
            if let Some(buffer) = self.product.selection_editor_mut() {
                buffer.select_utf16(range);
            }
        } else {
            self.text_input.select(range);
        }
        window.notify_text_selection_changed(&self.focus);
        cx.notify();
    }
    fn text_length_utf16(&mut self, _: &mut Window, _: &mut Context<Self>) -> Option<usize> {
        if self.product.dialog.is_some() {
            return self
                .product
                .active_editor()
                .map(text_edit::EditBuffer::len_utf16);
        }
        Some(self.text_input.len_utf16())
    }
    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        if self.product.dialog.is_some() {
            let buffer = self.product.active_editor()?;
            let range = buffer.marked()?;
            return Some(
                text_edit::utf16_at_byte(buffer.text(), range.start)
                    ..text_edit::utf16_at_byte(buffer.text(), range.end),
            );
        }
        self.text_input.marked()
    }
    fn unmark_text(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.product.dialog.is_some() {
            if let Some(buffer) = self.product.active_editor_mut() {
                buffer.unmark();
            }
            self.editor_changed(window, cx);
            return;
        }
        let had_preedit = !self.text_input.preedit.is_empty();
        self.text_input.cancel();
        window.notify_text_input_changed(&self.focus);
        if had_preedit {
            cx.notify();
        }
    }
    fn replace_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.product.dialog.as_ref().is_some_and(|dialog| {
            dialog.kind == product::DialogKind::History && dialog.active_field == 1
        }) {
            self.text_input.cancel();
            window.notify_text_input_changed(&self.focus);
            cx.notify();
            return;
        }
        if self.product.dialog.is_some() {
            let multiline = self.product.active_multiline();
            if let Some(buffer) = self.product.active_editor_mut()
                && let Err(error) = buffer.replace(range, text, multiline)
            {
                self.product.notice = Some(format!("Текст не изменён: {error}"));
            }
            self.text_input.cancel();
            self.editor_changed(window, cx);
            return;
        }
        if self.pane_drag.is_some() || self.resize_gesture.is_some() {
            self.text_input.cancel();
            window.notify_text_input_changed(&self.focus);
            return;
        }
        self.reset_cursor_blink(cx);
        let had_preedit = !self.text_input.preedit.is_empty();
        let modes = self
            .state
            .views
            .get(self.selected)
            .map_or(0, |v| v.frame.modes);
        if let Some(error) = input::protocol_error(modes) {
            self.text_input.cancel();
            self.input_error = Some(error.into());
            cx.notify();
        } else {
            let bytes = self.text_input.commit(range, text);
            self.send(bytes, cx);
        }
        window.notify_text_input_changed(&self.focus);
        if had_preedit {
            cx.notify();
        }
    }
    fn paste(&mut self, item: ClipboardItem, window: &mut Window, cx: &mut Context<Self>) {
        if self.product.dialog.as_ref().is_some_and(|dialog| {
            dialog.kind == product::DialogKind::History && dialog.active_field == 1
        }) {
            cx.notify();
            return;
        }
        if let Some(text) = item.text() {
            if self.product.dialog.is_some() {
                self.product.insert_text(&text);
                self.editor_changed(window, cx);
            } else {
                self.paste_text(&text, window, cx);
            }
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
        if self.product.dialog.as_ref().is_some_and(|dialog| {
            dialog.kind == product::DialogKind::History && dialog.active_field == 1
        }) {
            self.text_input.cancel();
            window.notify_text_input_changed(&self.focus);
            cx.notify();
            return;
        }
        if self.product.dialog.is_some() {
            let multiline = self.product.active_multiline();
            if let Some(buffer) = self.product.active_editor_mut() {
                let replacement = range.unwrap_or_else(|| {
                    buffer
                        .marked()
                        .map(|range| {
                            text_edit::utf16_at_byte(buffer.text(), range.start)
                                ..text_edit::utf16_at_byte(buffer.text(), range.end)
                        })
                        .unwrap_or_else(|| buffer.selected_utf16())
                });
                if let Err(error) = buffer.mark(replacement, text, selection, multiline) {
                    self.product.notice = Some(format!("Текст не изменён: {error}"));
                }
            }
            self.editor_changed(window, cx);
            return;
        }
        if self.pane_drag.is_some() || self.resize_gesture.is_some() {
            self.text_input.cancel();
            window.notify_text_input_changed(&self.focus);
            return;
        }
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
        if let Some(dialog) = &self.product.dialog {
            let buffer = dialog.fields.get(dialog.active_field)?;
            let byte_range = text_edit::byte_range_from_utf16(buffer.text(), &range);
            return self
                .editor_layouts
                .get(&dialog.active_field)
                .filter(|layout| layout.dialog_id == dialog.id)?
                .bounds_for_range(byte_range);
        }
        let pane = self.panes.get(self.selected)?;
        let view = self.state.views.get(self.selected)?;
        let area = preedit_area(*pane, view)?;
        if self.text_input.preedit.is_empty() {
            return Some(Bounds::new(area.origin, pane.cell));
        }
        let line = preedit_line(
            &self.text_input.preedit,
            &crate::font::TerminalFont {
                face: crate::font::terminal_font(),
                size: px(f32::from(self.product.appearance.terminal_font_size)),
            },
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
        point: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        let dialog = self.product.dialog.as_ref()?;
        let buffer = dialog.fields.get(dialog.active_field)?;
        let index = self
            .editor_layouts
            .get(&dialog.active_field)
            .filter(|layout| layout.dialog_id == dialog.id)?
            .index_for_point(point);
        Some(text_edit::utf16_at_byte(buffer.text(), index))
    }
}
impl Render for CanvasApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let width = f32::from(window.viewport_size().width);
        self.product.compact = width < 900.;
        self.product.sidebar_compact = self.product.sidebar_override.unwrap_or(width < 1100.);
        let active_pane = self.selected_pane();
        let header = self.product.header(cx);
        let sidebar = self.product.sidebar(&self.state.views, active_pane, cx);
        let has_workspace = self
            .product
            .catalog
            .as_ref()
            .is_some_and(|catalog| catalog.selection.workspace_id.is_some());
        let tabbar = self.product.tabbar(cx).into_any_element();
        let toolbar = self.product.toolbar(cx).into_any_element();
        let overlay = self.product.overlay(&self.editor_layouts, cx);
        let empty_state = self
            .state
            .views
            .is_empty()
            .then(|| self.product.empty_state(cx))
            .flatten();
        let entity = cx.entity();
        let colors = chrome::colors(self.product.appearance.theme);
        let terminal_font_size = px(f32::from(self.product.appearance.terminal_font_size));
        let painter = entity.clone();
        let trace = self.trace.clone();
        let paint_revision = self.state.revision;
        let views = self.state.views.clone();
        let pane_titles = views
            .iter()
            .map(|view| self.product.pane_title(view).to_owned())
            .collect::<Vec<_>>();
        let preedit = self.text_input.preedit.clone();
        let preedit_selection = self.text_input.selection_bytes();
        let selected = self.selected;
        let cursor_visible = self.cursor_phase_visible;
        let selection = self.selection.clone();
        let drop_preview = self
            .pane_drag
            .as_ref()
            .and_then(|drag| drag.drop.as_ref().map(|(_, rect)| *rect));
        let dragged_index = self
            .pane_drag
            .as_ref()
            .filter(|drag| drag.moved)
            .and_then(|drag| {
                views
                    .iter()
                    .position(|view| view.session == drag.pane.to_string())
            });
        let attached = self.state.error.is_none();
        let terminal_label = self.state.views.get(selected).map_or_else(
            || "Терминал: нет выбранной сессии".to_owned(),
            |view| {
                format!(
                    "Терминал {} · {}",
                    self.product.pane_title(view),
                    view.state
                )
            },
        );
        let status = self.error()
            .or_else(|| self.state.poll_backpressured.then(|| "Обновление терминала задержано".into()))
            .or_else(|| self.input_notice.clone()).unwrap_or_else(|| {
            if self.prefix {
                "PigADE: H/J/K/L — фокус · Shift — переместить · W — обмен · R — размер · 1–9 — вкладка · P — команды"
                    .into()
            } else {
                format!(
                    "PigADE 0.0.1-dev · {} панелей · Ctrl+Shift+Space, P — команды",
                    views.len()
                )
            }
        });
        let invalid_drop = self
            .pane_drag
            .as_ref()
            .is_some_and(|drag| drag.moved && drag.travelled && drag.drop.is_none());
        let status = match self.pane_drag.as_ref().filter(|drag| drag.moved) {
            Some(drag) if !drag.travelled => {
                "Перетащите панель в нужное место · Escape — отменить".to_owned()
            }
            Some(drag) if drag.drop.is_some() => {
                "Отпустите для перемещения панели · Escape — отменить".to_owned()
            }
            Some(_) => "Здесь нельзя разместить панель · Отпустите для отмены · Escape — отменить"
                .to_owned(),
            None => status,
        };
        let status = match self.layout_mode {
            Some(LayoutMode::Resize) => {
                "Размер: стрелки — пропорции · Enter — сохранить · Escape — отменить".to_owned()
            }
            Some(LayoutMode::Swap) => {
                "Обмен панелей: H/J/K/L или стрелки · Escape — отменить".to_owned()
            }
            Some(LayoutMode::Pan) => {
                "Canvas: стрелки — прокрутка · Home — начало · Enter — к панели · Escape — выход"
                    .to_owned()
            }
            None => status,
        };
        let status = if self.canvas_saving.is_some() || !self.canvas_dirty.is_empty() {
            format!("{status} · Положение canvas не сохранено")
        } else if !self.canvas_bars.is_empty() && self.layout_mode.is_none() {
            format!("{status} · Shift+колесо — canvas · Alt — горизонтально")
        } else {
            status
        };
        let recovery_warning = recovery_warning(
            self.state.recovery.as_ref(),
            self.state
                .views
                .get(selected)
                .and_then(|view| view.storage_error.as_deref()),
        );
        div()
            .id("pigade-root")
            .relative()
            .accessibility_id("pigade-root")
            .role(Role::Application)
            .aria_label("PigADE")
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(colors.base))
            .text_color(rgb(colors.text))
            .font_family("Inter")
            .text_size(px(13.))
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::key))
            .child(header)
            .child(
                div().flex().flex_1().min_h_0().child(sidebar).child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w_0()
                        .when(has_workspace, |column| {
                            column.child(
                                div()
                                    .h(px(36.))
                                    .flex_shrink_0()
                                    .flex()
                                    .items_center()
                                    .bg(rgb(colors.base))
                                    .border_b_1()
                                    .border_color(rgb(colors.border))
                                    .child(tabbar)
                                    .child(toolbar),
                            )
                        })
                        .child(
                            div()
                                .id("terminal-canvas")
                                .relative()
                                .accessibility_id("terminal-canvas")
                                .role(Role::Terminal)
                                .aria_label(terminal_label)
                                .flex_1()
                                .min_h_0()
                                .child(
                                    canvas(
                                        move |bounds, window, cx| {
                                            let font = crate::font::TerminalFont {
                                                face: crate::font::terminal_font(),
                                                size: terminal_font_size,
                                            };
                                            let fontid =
                                                window.text_system().resolve_font(&font.face);
                                            let width = window
                                                .text_system()
                                                .advance(fontid, font.size, 'M')
                                                .map(|s| s.width)
                                                .unwrap_or(terminal_font_size * (9. / 14.));
                                            let height = terminal_line_height(
                                                window.text_system().ascent(fontid, font.size),
                                                window.text_system().descent(fontid, font.size),
                                            );
                                            let cell = size(width, height);
                                            let scale = window.scale_factor();
                                            let (panes, content, bars) =
                                                entity.update(cx, |this, cx| {
                                                    let panes =
                                                        this.geometry(bounds, cell, scale, cx);
                                                    (
                                                        panes,
                                                        this.canvas_bounds.unwrap_or(bounds),
                                                        this.canvas_bars.clone(),
                                                    )
                                                });
                                            (font, panes, content, bars)
                                        },
                                        move |bounds, (font, panes, content, bars), window, cx| {
                                            let mut sequences = [0; 4];
                                            let count = views.len().min(sequences.len());
                                            for (sequence, view) in sequences.iter_mut().zip(&views)
                                            {
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
                                            window.with_content_mask(
                                                Some(ContentMask { bounds: content }),
                                                |window| {
                                                    let order = (0..panes.len())
                                                        .filter(|index| {
                                                            Some(*index) != dragged_index
                                                        })
                                                        .chain(dragged_index);
                                                    for index in order {
                                                        let Some(pane) = panes.get(index).copied()
                                                        else {
                                                            continue;
                                                        };
                                                        if Some(index) == dragged_index
                                                            && let Some(rect) = drop_preview
                                                        {
                                                            window.paint_quad(fill(
                                                                layout_bounds(rect),
                                                                rgb(colors.accent).opacity(0.267),
                                                            ));
                                                        }
                                                        if !pixel_bounds_intersect(
                                                            content,
                                                            pane.bounds,
                                                        ) {
                                                            continue;
                                                        }
                                                        paint_panel(
                                                            pane,
                                                            views.get(index),
                                                            PanelState {
                                                                title: pane_titles
                                                                    .get(index)
                                                                    .map(String::as_str)
                                                                    .unwrap_or("Терминал"),
                                                                selected: index == selected,
                                                                attached,
                                                                focused: index == selected
                                                                    && attached
                                                                    && views
                                                                        .get(index)
                                                                        .is_some_and(|view| {
                                                                            view.state == "running"
                                                                        })
                                                                    && focus.is_focused(window)
                                                                    && window.is_window_active(),
                                                                cursor_visible,
                                                                colors: if Some(index)
                                                                    == dragged_index
                                                                    && invalid_drop
                                                                {
                                                                    chrome::Colors {
                                                                        accent: colors.status_amber,
                                                                        ..colors
                                                                    }
                                                                } else {
                                                                    colors
                                                                },
                                                            },
                                                            selection.as_ref(),
                                                            &font,
                                                            window,
                                                            cx,
                                                        );
                                                        if index == selected && !preedit.is_empty()
                                                        {
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
                                                    if dragged_index.is_none()
                                                        && let Some(rect) = drop_preview
                                                    {
                                                        window.paint_quad(fill(
                                                            layout_bounds(rect),
                                                            rgb(colors.accent).opacity(0.267),
                                                        ));
                                                    }
                                                },
                                            );
                                            window.with_content_mask(
                                                Some(ContentMask { bounds }),
                                                |window| {
                                                    for bar in bars {
                                                        paint_canvas_scrollbar(bar, colors, window);
                                                    }
                                                },
                                            );
                                        },
                                    )
                                    .size_full(),
                                )
                                .children(
                                    empty_state
                                        .map(|element| div().absolute().inset_0().child(element)),
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
                                        .text_color(rgb(colors.status_amber))
                                        .child(warning),
                                );
                            }
                            let acknowledgement = self.selected_notice().and_then(|notice| {
                                acknowledgement_presentation(Some(notice), &self.mouse_delivery)
                                    .map(|(token, needs_release)| {
                                        (notice.target.clone(), token, needs_release)
                                    })
                            });
                            let mut status_row = div()
                                .id("session-status")
                                .role(Role::Status)
                                .aria_label(status.clone())
                                .h(px(24.))
                                .px(px(12.))
                                .flex()
                                .items_center()
                                .text_color(rgb(colors.muted))
                                .text_size(px(10.))
                                .border_t_1()
                                .border_color(rgb(colors.border))
                                .child(div().flex_1().min_w_0().truncate().child(status));
                            if self.canvas_close_requested {
                                status_row = status_row.child(
                                    div()
                                        .id("close-without-canvas-position")
                                        .role(Role::Button)
                                        .aria_label("Закрыть без сохранения положения canvas")
                                        .px(px(8.))
                                        .h(px(24.))
                                        .child("Закрыть без сохранения · Ctrl+Shift+Esc")
                                        .on_mouse_down(
                                            MouseButton::Left,
                                            cx.listener(|_, _, window, cx| {
                                                window.remove_window();
                                                cx.stop_propagation();
                                            }),
                                        ),
                                );
                            }
                            if let Some((target, token, needs_release)) = acknowledgement {
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
                        }),
                ),
            )
            .children(overlay)
            .when(
                self.pane_drag.as_ref().is_some_and(|drag| drag.moved),
                |element| element.cursor(CursorStyle::ClosedHand),
            )
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
    font: &crate::font::TerminalFont,
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
        font.size,
        &[TextRun {
            len,
            font: font.face.clone(),
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

fn preedit_line(text: &str, font: &crate::font::TerminalFont, window: &Window) -> ShapedLine {
    window.text_system().shape_line(
        text.to_owned().into(),
        font.size,
        &[TextRun {
            len: text.len(),
            font: font.face.clone(),
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
    font: &crate::font::TerminalFont,
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
struct PanelState<'a> {
    title: &'a str,
    selected: bool,
    attached: bool,
    focused: bool,
    cursor_visible: bool,
    colors: chrome::Colors,
}

fn pane_header_bounds(pane: PaneGeometry) -> Bounds<Pixels> {
    let width = (pane.bounds.size.width - px(16.)).min(px(520.)).max(px(0.));
    Bounds::new(
        point(
            pane.bounds.left() + (pane.bounds.size.width - width) / 2.,
            pane.bounds.top() + px(4.),
        ),
        size(width, px(24.)),
    )
}

fn pane_header_buttons(pane: PaneGeometry) -> [(&'static str, &'static str, Bounds<Pixels>); 4] {
    let header = pane_header_bounds(pane);
    [
        ("split-right", "split_right"),
        ("split-down", "split_down"),
        ("focus", "focus"),
        ("more", "menu"),
    ]
    .map(|(icon, action)| {
        let index = match action {
            "split_right" => 0.,
            "split_down" => 1.,
            "focus" => 2.,
            _ => 3.,
        };
        (
            icon,
            action,
            Bounds::new(
                point(header.right() - px(98.) + px(index * 24.), header.top()),
                size(px(24.), px(24.)),
            ),
        )
    })
}

fn paint_panel(
    pane: PaneGeometry,
    view: Option<&View>,
    PanelState {
        title,
        selected,
        attached,
        focused,
        cursor_visible,
        colors,
    }: PanelState<'_>,
    selection: Option<&selection::CellSelection>,
    font: &crate::font::TerminalFont,
    window: &mut Window,
    cx: &mut App,
) {
    let bounds = pane.bounds;
    let cell = pane.cell;
    let border = if selected {
        colors.accent
    } else {
        colors.border
    };
    window.paint_quad(quad(
        bounds,
        px(7.),
        rgb(0x111317),
        px(1.),
        rgb(border),
        BorderStyle::Solid,
    ));
    let header = pane_header_bounds(pane);
    window.paint_quad(quad(
        header,
        px(12.),
        rgb(colors.raised),
        px(1.),
        rgb(colors.border),
        BorderStyle::Solid,
    ));
    let status = view
        .map(|view| {
            if !attached {
                "История · нет связи"
            } else {
                match view.state.as_str() {
                    "running" => "Работает",
                    "exited" => "Завершён",
                    "degraded" => "Ошибка",
                    "launch_outcome_unknown" => "Исход неизвестен",
                    _ => "История",
                }
            }
        })
        .unwrap_or("Модель недоступна");
    let title = format!("{title}   {status}");
    let title_bounds = Bounds::new(
        header.origin + point(px(28.), px(1.)),
        size((header.size.width - px(130.)).max(px(0.)), px(22.)),
    );
    let dot = Bounds::new(
        header.origin + point(px(12.), px(10.)),
        size(px(5.), px(5.)),
    );
    window.paint_quad(quad(
        dot,
        px(3.),
        rgb(
            if attached && view.is_some_and(|view| view.state == "running") {
                colors.status_green
            } else {
                colors.status_amber
            },
        ),
        px(0.),
        rgb(colors.border),
        BorderStyle::Solid,
    ));
    window.with_content_mask(
        Some(ContentMask {
            bounds: title_bounds,
        }),
        |window| {
            let font = Font {
                family: "Inter".into(),
                ..Default::default()
            };
            let line = window.text_system().shape_line(
                title.clone().into(),
                px(12.),
                &[TextRun {
                    len: title.len(),
                    font,
                    color: rgb(colors.text).into(),
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                }],
                None,
            );
            let _ = line.paint(
                title_bounds.origin,
                px(22.),
                TextAlign::Left,
                None,
                window,
                cx,
            );
        },
    );
    for (name, _, bounds) in pane_header_buttons(pane) {
        let icon_bounds = Bounds::new(
            bounds.origin + point(px(5.), px(5.)),
            size(px(14.), px(14.)),
        );
        let _ = window.paint_svg(
            icon_bounds,
            format!("icons/{name}.svg").into(),
            None,
            TransformationMatrix::default(),
            rgb(colors.muted).into(),
            cx,
        );
    }
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
                                let mut characters = c.text.chars();
                                let block = characters.next().and_then(|character| {
                                    characters
                                        .next()
                                        .is_none()
                                        .then(|| {
                                            block_glyph::plan(
                                                character,
                                                cell_width_px.saturating_mul(span.min(columns)),
                                                cell_height_px,
                                            )
                                        })
                                        .flatten()
                                });
                                if let Some(block) = block {
                                    for rect in block.rects() {
                                        window.paint_quad(fill(
                                            Bounds::new(
                                                origin
                                                    + point(
                                                        px(f32::from(rect.x_px) / scale),
                                                        px(f32::from(rect.y_px) / scale),
                                                    ),
                                                size(
                                                    px(f32::from(rect.width_px) / scale),
                                                    px(f32::from(rect.height_px) / scale),
                                                ),
                                            ),
                                            rgb(fg),
                                        ));
                                    }
                                } else {
                                    let mut glyphfont = font.face.clone();
                                    if c.flags & 2 != 0 {
                                        glyphfont.weight = FontWeight::BOLD;
                                    }
                                    if c.flags & 4 != 0 {
                                        glyphfont.style = FontStyle::Italic;
                                    }
                                    text(
                                        c.text.clone(),
                                        origin,
                                        &crate::font::TerminalFont {
                                            face: glyphfont,
                                            size: font.size,
                                        },
                                        rgb(fg).into(),
                                        cell.height,
                                        window,
                                        cx,
                                    );
                                }
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
        println!("PigADE {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    let explicit_socket = std::env::args().nth(1);
    if explicit_socket.as_deref() == Some("--help") {
        println!(
            "PigADE {}\nUsage: pigade-app [HOST_SOCKET]\nWithout a socket, starts or reconnects to the local session host.",
            env!("CARGO_PKG_VERSION")
        );
        return;
    }
    let socket = match startup::resolve_socket(explicit_socket.map(PathBuf::from)) {
        Ok(socket) => socket,
        Err(error) => {
            eprintln!("PigADE: {error}");
            std::process::exit(2);
        }
    };
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
    let connection = ipc(socket, trace);
    application()
        .with_assets(chrome::Assets)
        .run(move |cx: &mut App| {
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
                    window.set_window_title("PigADE · dev 0.0.1");
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
    fn direct_launch_binds_exact_request_provider_and_still_selected_tab() {
        let tab = uuid::Uuid::new_v4();
        let request = uuid::Uuid::new_v4();
        for provider in ["shell", "codex", "claude", "opencode"] {
            let pending = super::DirectLaunch {
                tab,
                request,
                provider: provider.into(),
            };
            assert!(pending.matches(Some(request)));
            assert!(!pending.matches(None));
            assert!(!pending.matches(Some(uuid::Uuid::new_v4())));
            assert!(pending.target_matches(Some(tab), tab, provider));
            assert!(!pending.target_matches(None, tab, provider));
            assert!(!pending.target_matches(Some(uuid::Uuid::new_v4()), tab, provider));
            assert!(!pending.target_matches(Some(tab), uuid::Uuid::new_v4(), provider));
            assert!(!pending.target_matches(Some(tab), tab, "other-provider"));
        }
    }

    #[test]
    fn periodic_poll_and_product_prefetch_cannot_overtake_each_other() {
        use std::sync::{Arc, Mutex, mpsc};
        use std::time::Duration;
        for first_is_prefetch in [false, true] {
            let shared = Arc::new(Mutex::new(State {
                views: vec![view(None)],
                ..Default::default()
            }));
            let gate = Arc::new(Mutex::new(()));
            let (entered, first_entered) = mpsc::channel();
            let (release, released) = mpsc::channel();
            let (attempt, attempted) = mpsc::channel();
            let (second_enter, second_entered) = mpsc::channel();
            let first_shared = shared.clone();
            let first_gate = gate.clone();
            let first = std::thread::spawn(move || {
                super::serialized_poll(
                    &first_shared,
                    &first_gate,
                    first_is_prefetch,
                    |known, _, _| {
                        assert_eq!(known.len(), usize::from(!first_is_prefetch));
                        entered.send(()).unwrap();
                        released.recv_timeout(Duration::from_secs(2)).unwrap();
                        let mut next = view(None);
                        next.sequence = 2;
                        Ok((
                            super::Poll {
                                selected: vec![next.known()],
                                views: vec![next],
                                recovery: None,
                            },
                            None,
                        ))
                    },
                )
            });
            first_entered.recv_timeout(Duration::from_secs(2)).unwrap();
            let second_shared = shared.clone();
            let second_gate = gate.clone();
            let second = std::thread::spawn(move || {
                attempt.send(()).unwrap();
                super::serialized_poll(
                    &second_shared,
                    &second_gate,
                    !first_is_prefetch,
                    |known, _, _| {
                        second_enter.send(()).unwrap();
                        // The first response was merged before the second capture;
                        // no shared-state lock is held while fetching a response.
                        assert_eq!(second_shared.lock().unwrap().views[0].sequence, 2);
                        if first_is_prefetch {
                            assert_eq!(known[0].sequence, 2);
                        } else {
                            assert!(known.is_empty());
                        }
                        let mut next = view(None);
                        next.sequence = 3;
                        Ok((
                            super::Poll {
                                selected: vec![next.known()],
                                views: vec![next],
                                recovery: None,
                            },
                            None,
                        ))
                    },
                )
            });
            attempted.recv_timeout(Duration::from_secs(2)).unwrap();
            assert!(
                second_entered
                    .recv_timeout(Duration::from_millis(20))
                    .is_err()
            );
            release.send(()).unwrap();
            assert!(first.join().unwrap().1);
            assert!(second.join().unwrap().1);
            let state = shared.lock().unwrap();
            assert_eq!(state.views[0].sequence, 3);
            assert!(state.error.is_none());
        }
    }

    #[test]
    fn scrolling_keeps_terminal_rows_columns_and_pointer_cells_at_fractional_scales() {
        for scale in [1., 1.25, 1.5, 2.] {
            let metrics = size(px(8.21), px(19.13));
            let width = (f32::from(metrics.width) * scale).ceil() / scale * 40. + 16. + 2. / scale;
            let height = (f32::from(metrics.height) * scale).ceil() / scale * 8.
                + super::HEADER
                + 16.
                + 2. / scale;
            let original = PaneGeometry::new(
                Bounds::new(point(px(0.3), px(17.7)), size(px(width), px(height))),
                metrics,
                scale,
            );
            // A scrollbar moves surfaces by whole device pixels, so even at
            // fractional scale it cannot resize a grid or change its cells.
            let shift = point(px(73. / scale), px(51. / scale));
            let moved = PaneGeometry::new(
                Bounds::new(original.bounds.origin - shift, original.bounds.size),
                metrics,
                scale,
            );
            assert_eq!((original.rows, original.cols), (moved.rows, moved.cols));
            assert!(original.rows >= 8 && original.cols >= 40);
            let geometry = super::protocol::Geometry {
                rows: original.rows,
                cols: original.cols,
            };
            let inside =
                original.origin + point(original.cell.width * 3.5, original.cell.height * 2.5);
            assert_eq!(
                original.pointer_position(inside, &geometry, false),
                moved.pointer_position(inside - shift, &geometry, false)
            );
        }
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
    fn focus_mode_preserves_view_slots_and_routes_hits_to_the_exact_visible_pane() {
        use pigade_layout::{Axis, LayoutState, Node, Rect, Size, TabLayout};
        let ids: Vec<_> = (1..=3).map(uuid::Uuid::from_u128).collect();
        let tab = uuid::Uuid::from_u128(10);
        let viewport = Rect {
            x: 100.,
            y: 100.,
            width: 800.,
            height: 600.,
        };
        for focused in &ids {
            let state = LayoutState {
                tabs: [(
                    tab,
                    TabLayout {
                        root: Some(Node::Split {
                            id: uuid::Uuid::from_u128(11),
                            axis: Axis::Horizontal,
                            weights: vec![1. / 3.; 3],
                            children: ids.iter().copied().map(Node::pane).collect(),
                        }),
                        focused: Some(*focused),
                        focus_mode: true,
                    },
                )]
                .into_iter()
                .collect(),
            };
            let projected = state
                .project(
                    tab,
                    viewport,
                    Size {
                        width: 100.,
                        height: 100.,
                    },
                    6.,
                )
                .unwrap();
            let panes: Vec<_> = ids
                .iter()
                .map(|id| {
                    PaneGeometry::from_projection(
                        &projected,
                        &id.to_string(),
                        size(px(8.), px(16.)),
                        1.25,
                    )
                })
                .collect();
            assert_eq!(panes.len(), ids.len());
            let cell_point = point(px(120.), px(160.));
            let geometry = super::protocol::Geometry { rows: 24, cols: 80 };
            let hits: Vec<_> = panes
                .iter()
                .enumerate()
                .filter_map(|(index, pane)| {
                    pane.pointer_position(cell_point, &geometry, false)
                        .map(|_| ids[index])
                })
                .collect();
            assert_eq!(hits, vec![*focused]);
            for (id, pane) in ids.iter().zip(&panes) {
                assert_eq!(
                    super::pixel_bounds_intersect(super::layout_bounds(viewport), pane.bounds),
                    id == focused
                );
                if id != focused {
                    assert!(
                        pane.pointer_position(cell_point, &geometry, true).is_none(),
                        "hidden geometry must not capture input or resize a different generation"
                    );
                }
            }
        }
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
    fn generation_choice_drops_only_changed_pane_before_loading_new_cells() {
        use super::protocol::PanelTarget;
        use uuid::Uuid;
        let session = Uuid::from_u128(10);
        let old_generation = Uuid::from_u128(999);
        let other_session = Uuid::from_u128(20);
        let old_target = PanelTarget {
            session,
            generation: Some(old_generation),
        };
        let following = PanelTarget {
            session,
            generation: None,
        };
        let other_target = PanelTarget {
            session: other_session,
            generation: None,
        };
        let mut historical = view(None);
        historical.session = session.to_string();
        historical.generation = old_generation.to_string();
        historical.state = "history".into();
        historical.pid = None;
        let mut other = view(None);
        other.session = other_session.to_string();
        let mut ui = State {
            views: vec![historical, other],
            visible_panels: Some(vec![old_target, other_target.clone()]),
            ..Default::default()
        };
        let mut shared = ui.clone();
        let other_cells = shared.views[1].frame.cells.as_ptr();
        shared.set_visible_panels(vec![following, other_target]);
        assert_eq!(shared.views.len(), 1);
        assert_eq!(shared.views[0].session, other_session.to_string());
        assert_eq!(shared.views[0].frame.cells.as_ptr(), other_cells);
        let update = state_update(&ui, &shared).unwrap();
        assert!(composition_target_changed(&ui, 0, &update));
        apply_state_update(&mut ui, update);
        assert!(
            !ui.views
                .iter()
                .any(|view| view.session == session.to_string())
        );
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
mod admission_tests {
    use super::{
        AdmissionFailure, CommandTarget, DeliveryOutcome, IpcMessage, MouseDelivery,
        RequestReceipt, admit_command, mouse_delivery,
    };
    use serde_json::json;
    use std::sync::mpsc;

    fn target() -> CommandTarget {
        CommandTarget {
            session: "session".into(),
            generation: "generation".into(),
        }
    }

    #[test]
    fn receipt_capacity_recovers_without_retrying_rejected_input() {
        let (tx, rx) = mpsc::sync_channel(mouse_delivery::MAX_PENDING);
        let mut delivery = MouseDelivery::default();
        let mut next = 1;
        for _ in 0..mouse_delivery::MAX_PENDING {
            admit_command(
                &mut delivery,
                &mut next,
                &tx,
                target(),
                json!({"bytes": [97]}),
                None,
            )
            .unwrap();
        }
        let error = admit_command(
            &mut delivery,
            &mut next,
            &tx,
            target(),
            json!({"bytes": [98]}),
            None,
        )
        .unwrap_err();
        assert_eq!(error, AdmissionFailure::Busy);
        assert!(!error.blocks_input());
        assert_eq!(next, 33);
        for id in 1..=32 {
            let IpcMessage::Request {
                id: received,
                target,
                command,
            } = rx.try_recv().unwrap()
            else {
                panic!("unexpected acknowledgement")
            };
            assert_eq!(received, id);
            assert_eq!(command["bytes"], json!([97]));
            delivery.request_receipt(RequestReceipt {
                id,
                target,
                outcome: DeliveryOutcome::Accepted,
            });
        }
        admit_command(
            &mut delivery,
            &mut next,
            &tx,
            target(),
            json!({"bytes": [99]}),
            None,
        )
        .unwrap();
        let IpcMessage::Request { id, command, .. } = rx.try_recv().unwrap() else {
            panic!("unexpected acknowledgement")
        };
        assert_eq!(id, 33);
        assert_eq!(command["bytes"], json!([99]));
        assert!(
            rx.try_recv().is_err(),
            "rejected bytes must never be retried"
        );
    }

    #[test]
    fn transport_backpressure_cancels_only_its_reservation_and_recovers() {
        let (tx, rx) = mpsc::sync_channel(1);
        let mut delivery = MouseDelivery::default();
        let mut next = 1;
        admit_command(&mut delivery, &mut next, &tx, target(), json!({}), None).unwrap();
        let failure =
            admit_command(&mut delivery, &mut next, &tx, target(), json!({}), None).unwrap_err();
        assert_eq!(failure, AdmissionFailure::Busy);
        assert!(!failure.blocks_input());
        assert!(!delivery.settled_through(&target(), 1));
        let _ = rx.try_recv().unwrap();
        delivery.request_receipt(RequestReceipt {
            id: 1,
            target: target(),
            outcome: DeliveryOutcome::Accepted,
        });
        assert!(delivery.settled_through(&target(), 2));
        admit_command(&mut delivery, &mut next, &tx, target(), json!({}), None).unwrap();
        drop(rx);
        assert_eq!(
            admit_command(&mut delivery, &mut next, &tx, target(), json!({}), None),
            Err(AdmissionFailure::Disconnected)
        );
        assert!(AdmissionFailure::Disconnected.blocks_input());
        assert!(AdmissionFailure::Exhausted.blocks_input());
    }

    #[test]
    fn motion_pressure_leaves_eight_slots_for_keys_and_button_transitions() {
        let mut delivery = MouseDelivery::default();
        for id in 1..=24 {
            assert!(delivery.has_motion_capacity());
            assert!(delivery.stage(id, target(), None));
        }
        assert!(!delivery.has_motion_capacity());
        for id in 25..=32 {
            assert!(delivery.has_capacity());
            assert!(delivery.stage(id, target(), None));
        }
        assert!(!delivery.has_capacity());
        delivery.request_receipt(RequestReceipt {
            id: 1,
            target: target(),
            outcome: DeliveryOutcome::Accepted,
        });
        assert!(delivery.has_capacity());
        assert!(!delivery.has_motion_capacity());
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
    fn blocked_change_wait_does_not_block_input_or_visible_panel_refresh() -> Result<(), String> {
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
        let (refreshed_tx, refreshed_rx) = mpsc::channel();
        let server = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut release = Some(release_rx);
            let mut waiter = None;
            let mut received_input = false;
            let mut polls = 0;
            for _ in 0..5 {
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
                    Some("catalog") => {
                        reply(&mut stream, json!({"protocol":2,"views":[],"error":null}))?
                    }
                    Some("poll") => {
                        reply(
                            &mut stream,
                            json!({"protocol":2,"views":[],"selected":[],"error":null,"change_token":token}),
                        )?;
                        polls += 1;
                        if polls == 2 {
                            refreshed_tx.send(()).map_err(|e| e.to_string())?;
                        }
                    }
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
        // Drain the initial catalog receipt; the local refresh must not emit a
        // product reply or send its internal marker to the host.
        connection
            .product_rx
            .recv_timeout(Duration::from_secs(2))
            .map_err(|e| e.to_string())?
            .map_err(|e| format!("initial catalog: {}", e.message))?;
        connection
            .product_tx
            .try_send(json!({"_ui_refresh_panels":true}))
            .map_err(|e| e.to_string())?;
        refreshed_rx
            .recv_timeout(Duration::from_secs(2))
            .map_err(|e| format!("newly visible panels blocked behind idle wait: {e}"))?;
        assert!(matches!(
            connection.product_rx.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
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

#[cfg(test)]
mod acknowledgement_presentation_tests {
    use super::acknowledgement_presentation;
    use crate::{
        command_worker::{CommandNotice, CommandTarget},
        mouse,
        mouse_delivery::{MouseDelivery, MouseSnapshot, Outcome, RequestReceipt},
    };

    fn target(name: &str) -> CommandTarget {
        CommandTarget {
            session: name.into(),
            generation: "generation".into(),
        }
    }
    fn notice(target: CommandTarget, last: u64) -> CommandNotice {
        CommandNotice {
            target,
            last_request_id: last,
            token: 7,
            message: "rejected".into(),
            unsent: 0,
            rejected_resizes: vec![],
            acknowledgeable: true,
        }
    }
    fn receipt(id: u64, target: &CommandTarget, outcome: Outcome) -> RequestReceipt {
        RequestReceipt {
            id,
            target: target.clone(),
            outcome,
        }
    }
    #[test]
    fn known_rejection_button_waits_for_each_receipt_and_ignores_other_targets() {
        let a = target("a");
        let b = target("b");
        let notice = notice(a.clone(), 2);
        let mut delivery = MouseDelivery::default();
        assert!(delivery.stage(1, a.clone(), None));
        assert!(delivery.stage(2, a.clone(), None));
        assert!(delivery.stage(3, b.clone(), None));
        assert_eq!(acknowledgement_presentation(Some(&notice), &delivery), None);
        delivery.request_receipt(receipt(1, &a, Outcome::NotAdmitted));
        assert_eq!(acknowledgement_presentation(Some(&notice), &delivery), None);
        delivery.request_receipt(receipt(2, &a, Outcome::Dropped));
        assert_eq!(
            acknowledgement_presentation(Some(&notice), &delivery),
            Some((7, false))
        );
        delivery.request_receipt(receipt(3, &b, Outcome::Accepted));
        assert_eq!(
            acknowledgement_presentation(Some(&notice), &delivery),
            Some((7, false))
        );
        let mut unknown = notice.clone();
        unknown.acknowledgeable = false;
        assert_eq!(
            acknowledgement_presentation(Some(&unknown), &delivery),
            None
        );
        assert_eq!(acknowledgement_presentation(None, &delivery), None);
    }
    #[test]
    fn rejected_release_keeps_confirmed_press_visible_until_a_new_release_is_admitted() {
        let a = target("a");
        let mut delivery = MouseDelivery::default();
        let mut encoder = mouse::Encoder::default();
        encoder
            .press(
                mouse::Button::Left,
                mouse::Position { row: 0, column: 0 },
                mouse::Modifiers::default(),
                mouse::MOUSE_REPORT_CLICK | mouse::SGR_MOUSE,
            )
            .unwrap();
        let snapshot = MouseSnapshot {
            encoder,
            position: mouse::Position { row: 0, column: 0 },
            wheel: mouse::WheelAccumulator::default(),
            rows: 10,
            cols: 20,
            modes: mouse::MOUSE_REPORT_CLICK | mouse::SGR_MOUSE,
        };
        assert!(delivery.stage(1, a.clone(), Some(snapshot.clone())));
        delivery.request_receipt(receipt(1, &a, Outcome::Accepted));
        let mut released = snapshot;
        released
            .encoder
            .release_all(
                released.position,
                mouse::Modifiers::default(),
                released.modes,
            )
            .unwrap();
        assert!(delivery.stage(2, a.clone(), Some(released.clone())));
        delivery.request_receipt(receipt(2, &a, Outcome::NotAdmitted));
        let notice = notice(a.clone(), 2);
        assert_eq!(
            acknowledgement_presentation(Some(&notice), &delivery),
            Some((7, true))
        );
        assert!(delivery.stage(3, a.clone(), Some(released)));
        delivery.request_receipt(receipt(3, &a, Outcome::Accepted));
        assert_eq!(
            acknowledgement_presentation(Some(&notice), &delivery),
            Some((7, false))
        );
    }
}

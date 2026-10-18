//! Bounded same-user IPC framing workers. Host/SQLite mutations remain on the
//! receiver's single thread; a client withholding bytes does not block it.
use std::{
    fs,
    os::unix::{
        fs::{FileTypeExt, MetadataExt, PermissionsExt},
        net::{UnixListener, UnixStream},
    },
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
    thread,
    time::Duration,
};

use anyhow::{Context, Result, ensure};

use crate::{
    Command, CommandRejection, PROTOCOL, Request, Response, changes::ChangeHub, encode, peer_pid,
    receive_request, send, send_encoded,
};

const CONNECTION_LIMIT: usize = 16;
const JOB_LIMIT: usize = 16;
const IO_TIMEOUT: Duration = Duration::from_secs(2);
const REPLY_TIMEOUT: Duration = Duration::from_secs(15);

pub(crate) struct Job {
    pub request: Request,
    pub reply: SyncSender<Response>,
    pub(crate) resume_probe: Option<crate::resume_probe::Completion>,
    pub(crate) _resume_activity: Option<crate::resume_probe::Activity>,
    pub(crate) delivery: Option<Receiver<()>>,
}

/// Serializes command admission with the actor's shutdown barrier.
#[derive(Clone, Default)]
pub(crate) struct Admission(Arc<Mutex<bool>>);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AdmissionError {
    Full,
    Sealed,
    Disconnected,
}

impl Admission {
    pub(crate) fn seal(&self) {
        *self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = true;
    }

    fn submit(&self, jobs: &SyncSender<Job>, job: Job) -> std::result::Result<(), AdmissionError> {
        let sealed = self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if *sealed {
            return Err(AdmissionError::Sealed);
        }
        jobs.try_send(job).map_err(|error| match error {
            mpsc::TrySendError::Full(_) => AdmissionError::Full,
            mpsc::TrySendError::Disconnected(_) => AdmissionError::Disconnected,
        })
    }

    pub(crate) fn seal_and_reject_queued(&self, jobs: &Receiver<Job>) -> Vec<Receiver<()>> {
        self.seal();
        let mut deliveries = Vec::new();
        while let Ok(queued) = jobs.try_recv() {
            let _ = queued.reply.try_send(error_response(
                "host is shutting down; command was not applied",
                Some(CommandRejection::HostShuttingDown),
            ));
            if let Some(delivery) = queued.delivery {
                deliveries.push(delivery);
            }
        }
        deliveries
    }
}

/// Caller must hold the host ownership lock and have validated its private state
/// directory. The worker admission count also bounds simultaneous frame buffers.
pub(crate) fn bind(
    socket: &Path,
    changes: ChangeHub,
) -> Result<(Receiver<Job>, SyncSender<Job>, Admission)> {
    if let Ok(metadata) = fs::symlink_metadata(socket) {
        // A stale socket is safe to unlink only under the caller's ownership lock.
        // SAFETY: geteuid has no preconditions.
        ensure!(
            metadata.file_type().is_socket() && metadata.uid() == unsafe { libc::geteuid() },
            "refusing to replace a non-socket or foreign IPC endpoint"
        );
        fs::remove_file(socket)?;
    }
    let listener = UnixListener::bind(socket).context("bind private host endpoint")?;
    fs::set_permissions(socket, fs::Permissions::from_mode(0o600))?;
    let (sender, receiver) = mpsc::sync_channel(JOB_LIMIT);
    let internal_sender = sender.clone();
    let admission = Admission::default();
    let worker_admission = admission.clone();
    let active = Arc::new(AtomicUsize::new(0));
    thread::Builder::new()
        .name("pigade-ipc-accept".into())
        .spawn(move || {
            for connection in listener.incoming() {
                let stream = match connection {
                    Ok(stream) => stream,
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(error) => {
                        eprintln!("PigADE IPC accept failed: {error}");
                        break;
                    }
                };
                if peer_pid(&stream).is_err() {
                    continue;
                }
                if active
                    .fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                        (count < CONNECTION_LIMIT).then_some(count + 1)
                    })
                    .is_err()
                {
                    // Excess peers are closed; never spawn an overflow worker.
                    continue;
                }
                let slot = ConnectionSlot(active.clone());
                let sender = sender.clone();
                let changes = changes.clone();
                let admission = worker_admission.clone();
                // If spawn fails, dropping its closure also releases the slot.
                let _ = thread::Builder::new()
                    .name("pigade-ipc-client".into())
                    .spawn(move || {
                        let _slot = slot;
                        let _ = process_connection(stream, sender, changes, admission);
                    });
            }
        })?;
    Ok((receiver, internal_sender, admission))
}

struct ConnectionSlot(Arc<AtomicUsize>);
impl Drop for ConnectionSlot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

pub(crate) fn error_response(
    error: impl Into<String>,
    rejected_before_admission: Option<CommandRejection>,
) -> Response {
    Response {
        protocol: PROTOCOL,
        views: Vec::new(),
        error: Some(error.into()),
        rejected_before_admission,
        catalog: None,
        selection: None,
        context: None,
        launch_preview: None,
        launch_phase: None,
        catalog_notice: None,
        recovery: None,
        selected: None,
        history_chunk: None,
        change_token: None,
        activity: None,
        activity_error: None,
        generations: None,
        generation_selection: None,
        history_view: None,
        providers: None,
        template_run: None,
        template_runs: None,
        panel_directory: None,
        workspace_git_snapshot: None,
        workspace_git_diff: None,
        workspace_git_index: None,
        workspace_git_commit_plan: None,
        workspace_git_commit: None,
        host_status: None,
    }
}

fn reply_or_unknown(result: std::result::Result<Response, mpsc::RecvTimeoutError>) -> Response {
    match result {
        Ok(response) => response,
        // The request may have been committed; callers must reconcile rather
        // than treating this as a safe retry signal.
        Err(mpsc::RecvTimeoutError::Timeout) => error_response(
            "host response timed out; operation outcome may be unknown",
            None,
        ),
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            error_response("host response lost; operation outcome may be unknown", None)
        }
    }
}

fn encode_reply(response: Response) -> Result<Vec<u8>> {
    let encoded = encode(&response);
    // The worker retains only the bounded encoded buffer during a slow socket
    // write, rather than also keeping deep copies of terminal cell frames.
    drop(response);
    match encoded {
        Ok(bytes) => Ok(bytes),
        Err(error) => encode(&error_response(
            format!("host response encoding failed: {error}; operation outcome may be unknown"),
            None,
        )),
    }
}

fn process_connection(
    mut stream: UnixStream,
    jobs: SyncSender<Job>,
    changes: ChangeHub,
    admission: Admission,
) -> Result<()> {
    // Validate here too so tests and any future caller cannot bypass peer auth.
    peer_pid(&stream)?;
    stream.set_read_timeout(Some(IO_TIMEOUT))?;
    stream.set_write_timeout(Some(IO_TIMEOUT))?;
    let request: Request = match receive_request(&mut stream) {
        Ok(request) => request,
        Err(_) => {
            // Do not echo malformed user payloads into diagnostics.
            send(
                &mut stream,
                &error_response("invalid request framing or payload", None),
            )?;
            return Ok(());
        }
    };
    if request.protocol != PROTOCOL {
        send(
            &mut stream,
            &error_response("unsupported IPC protocol", None),
        )?;
        return Ok(());
    }
    if let Command::WaitForChange { after, timeout_ms } = &request.command {
        let response = match changes.wait(*after, Duration::from_millis(u64::from(*timeout_ms))) {
            Ok(result) => Response {
                protocol: PROTOCOL,
                views: Vec::new(),
                error: None,
                rejected_before_admission: None,
                catalog: None,
                selection: None,
                context: None,
                launch_preview: None,
                launch_phase: None,
                catalog_notice: None,
                recovery: None,
                selected: None,
                history_chunk: None,
                change_token: Some(result.token),
                activity: None,
                activity_error: None,
                generations: None,
                generation_selection: None,
                history_view: None,
                providers: None,
                template_run: None,
                template_runs: None,
                panel_directory: None,
                workspace_git_snapshot: None,
                workspace_git_diff: None,
                workspace_git_index: None,
                workspace_git_commit_plan: None,
                workspace_git_commit: None,
                host_status: None,
            },
            Err(error) => error_response(format!("change wait unavailable: {error:#}"), None),
        };
        send(&mut stream, &response)?;
        return Ok(());
    }
    let (reply, receiver) = mpsc::sync_channel(1);
    let (delivered, delivery) = mpsc::sync_channel(1);
    match admission.submit(
        &jobs,
        Job {
            request,
            reply,
            resume_probe: None,
            _resume_activity: None,
            delivery: Some(delivery),
        },
    ) {
        Ok(()) => (),
        Err(AdmissionError::Full) => {
            send(
                &mut stream,
                &error_response(
                    "host command queue full; command was not admitted",
                    Some(CommandRejection::QueueFull),
                ),
            )?;
            return Ok(());
        }
        Err(AdmissionError::Sealed | AdmissionError::Disconnected) => {
            send(
                &mut stream,
                &error_response(
                    "host is shutting down; command was not applied",
                    Some(CommandRejection::HostShuttingDown),
                ),
            )?;
            return Ok(());
        }
    }
    // Timeout does not cancel or retry a possibly committed command. Its
    // operation ID remains the caller's reconciliation/idempotency boundary.
    let response = reply_or_unknown(receiver.recv_timeout(REPLY_TIMEOUT));
    let bytes = encode_reply(response)?;
    // Encoding fallback occurs before writing the frame header. A partial I/O
    // failure must never append another reply to an already-started frame.
    send_encoded(&mut stream, &bytes)?;
    let _ = delivered.try_send(());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Command, receive};

    #[test]
    fn decoded_job_runs_on_receiver_and_replies_without_extra_writer() -> Result<()> {
        let (mut client, server) = UnixStream::pair()?;
        client.set_read_timeout(Some(IO_TIMEOUT))?;
        let (jobs, receiver) = mpsc::sync_channel(1);
        let worker = thread::spawn(move || {
            process_connection(server, jobs, ChangeHub::new(), Admission::default())
        });
        send(
            &mut client,
            &Request {
                protocol: PROTOCOL,
                command: Command::List,
            },
        )?;
        let job = receiver.recv_timeout(IO_TIMEOUT)?;
        assert!(matches!(job.request.command, Command::List));
        job.reply.send(Response {
            protocol: PROTOCOL,
            views: Vec::new(),
            error: None,
            rejected_before_admission: None,
            catalog: None,
            selection: None,
            context: None,
            launch_preview: None,
            launch_phase: None,
            catalog_notice: None,
            recovery: None,
            selected: None,
            history_chunk: None,
            change_token: None,
            activity: None,
            activity_error: None,
            generations: None,
            generation_selection: None,
            history_view: None,
            providers: None,
            template_run: None,
            template_runs: None,
            panel_directory: None,
            workspace_git_snapshot: None,
            workspace_git_diff: None,
            workspace_git_index: None,
            workspace_git_commit_plan: None,
            workspace_git_commit: None,
            host_status: None,
        })?;
        assert!(receive::<Response>(&mut client)?.error.is_none());
        worker.join().unwrap()?;
        Ok(())
    }

    #[test]
    fn wire_fields_cannot_supply_internal_resume_verification() -> Result<()> {
        let (mut client, server) = UnixStream::pair()?;
        client.set_read_timeout(Some(IO_TIMEOUT))?;
        let (jobs, receiver) = mpsc::sync_channel(1);
        let worker = thread::spawn(move || {
            process_connection(server, jobs, ChangeHub::new(), Admission::default())
        });
        send(
            &mut client,
            &serde_json::json!({"protocol":PROTOCOL,
            "command":{"command":"prepare_resume","session":uuid::Uuid::new_v4(),"resume_probe":{"verified":true}},
            "resume_probe":{"verified":true,"directory":"/untrusted"}}),
        )?;
        let job = receiver.recv_timeout(IO_TIMEOUT)?;
        assert!(job.resume_probe.is_none());
        assert!(matches!(job.request.command, Command::PrepareResume { .. }));
        job.reply
            .send(error_response("spy rejected before execution", None))
            .ok()
            .unwrap();
        assert_eq!(
            receive::<Response>(&mut client)?.error.as_deref(),
            Some("spy rejected before execution")
        );
        worker.join().unwrap()?;
        Ok(())
    }

    #[test]
    fn incompatible_protocol_is_rejected_before_dispatch() -> Result<()> {
        let (mut client, server) = UnixStream::pair()?;
        client.set_read_timeout(Some(IO_TIMEOUT))?;
        let (jobs, receiver) = mpsc::sync_channel(1);
        let worker = thread::spawn(move || {
            process_connection(server, jobs, ChangeHub::new(), Admission::default())
        });
        send(
            &mut client,
            &Request {
                protocol: PROTOCOL + 1,
                command: Command::List,
            },
        )?;
        let response: Response = receive(&mut client)?;
        assert!(response.error.unwrap().contains("protocol"));
        worker.join().unwrap()?;
        assert!(receiver.try_recv().is_err());
        Ok(())
    }

    #[test]
    fn full_job_queue_is_explicit_and_never_waits_for_capacity() -> Result<()> {
        let (mut client, server) = UnixStream::pair()?;
        client.set_read_timeout(Some(IO_TIMEOUT))?;
        let (jobs, receiver) = mpsc::sync_channel(1);
        let (reply, _response) = mpsc::sync_channel(1);
        jobs.send(Job {
            request: Request {
                protocol: PROTOCOL,
                command: Command::List,
            },
            reply,
            resume_probe: None,
            _resume_activity: None,
            delivery: None,
        })
        .ok()
        .unwrap();
        let worker = thread::spawn(move || {
            process_connection(server, jobs, ChangeHub::new(), Admission::default())
        });
        send(
            &mut client,
            &Request {
                protocol: PROTOCOL,
                command: Command::List,
            },
        )?;
        let response: Response = receive(&mut client)?;
        assert!(response.error.unwrap().contains("queue full"));
        assert_eq!(
            response.rejected_before_admission,
            Some(CommandRejection::QueueFull)
        );
        worker.join().unwrap()?;
        assert!(receiver.try_recv().is_ok());
        assert!(receiver.try_recv().is_err());
        Ok(())
    }

    #[test]
    fn sealed_actor_refuses_queued_mutation_and_waits_for_delivery() -> Result<()> {
        let (jobs, receiver) = mpsc::sync_channel(2);
        let (reply, response) = mpsc::sync_channel(1);
        let (delivered, delivery) = mpsc::sync_channel(1);
        jobs.try_send(Job {
            request: Request {
                protocol: PROTOCOL,
                command: Command::Input {
                    session: uuid::Uuid::new_v4(),
                    generation: uuid::Uuid::new_v4(),
                    bytes: b"must not be applied".to_vec(),
                },
            },
            reply,
            resume_probe: None,
            _resume_activity: None,
            delivery: Some(delivery),
        })
        .unwrap();
        let worker = thread::spawn(move || {
            let response = response.recv_timeout(IO_TIMEOUT)?;
            assert_eq!(
                response.rejected_before_admission,
                Some(CommandRejection::HostShuttingDown)
            );
            assert!(
                response
                    .error
                    .as_deref()
                    .is_some_and(|error| error.contains("not applied"))
            );
            delivered.send(()).unwrap();
            Ok::<_, mpsc::RecvTimeoutError>(())
        });

        let admission = Admission::default();
        let receipts = admission.seal_and_reject_queued(&receiver);
        assert_eq!(receipts.len(), 1);
        receipts[0].recv_timeout(IO_TIMEOUT)?;
        worker.join().unwrap()?;
        assert!(
            receiver.try_recv().is_err(),
            "queued command reached dispatch"
        );
        let (reply, _response) = mpsc::sync_channel(1);
        let result = admission.submit(
            &jobs,
            Job {
                request: Request {
                    protocol: PROTOCOL,
                    command: Command::List,
                },
                reply,
                resume_probe: None,
                _resume_activity: None,
                delivery: None,
            },
        );
        assert_eq!(result, Err(AdmissionError::Sealed));
        Ok(())
    }

    #[test]
    fn ambiguous_reply_timeout_never_claims_pre_admission_rejection() {
        let response = reply_or_unknown(Err(mpsc::RecvTimeoutError::Timeout));
        assert!(response.error.unwrap().contains("outcome may be unknown"));
        assert_eq!(response.rejected_before_admission, None);
    }

    #[test]
    fn oversized_dispatched_reply_returns_explicit_unknown_outcome_without_retry() -> Result<()> {
        let (mut client, server) = UnixStream::pair()?;
        client.set_read_timeout(Some(IO_TIMEOUT))?;
        let (jobs, receiver) = mpsc::sync_channel(1);
        let worker = thread::spawn(move || {
            process_connection(server, jobs, ChangeHub::new(), Admission::default())
        });
        send(
            &mut client,
            &Request {
                protocol: PROTOCOL,
                command: Command::Input {
                    session: uuid::Uuid::new_v4(),
                    generation: uuid::Uuid::new_v4(),
                    bytes: b"synthetic input".to_vec(),
                },
            },
        )?;
        let job = receiver.recv_timeout(IO_TIMEOUT)?;
        assert!(matches!(job.request.command, Command::Input { .. }));
        let mut oversized = error_response("test result", None);
        oversized.catalog_notice = Some("x".repeat(crate::IPC_LIMIT));
        job.reply.send(oversized)?;
        let response: Response = receive(&mut client)?;
        let error = response.error.context("encoding failure was hidden")?;
        assert!(error.contains("IPC payload too large"));
        assert!(error.contains("outcome may be unknown"));
        assert_eq!(response.rejected_before_admission, None);
        assert!(response.views.is_empty());
        worker.join().unwrap()?;
        assert!(
            receiver.try_recv().is_err(),
            "dispatched command was retried"
        );
        Ok(())
    }

    #[test]
    fn change_wait_bypasses_dispatch_queue_and_does_not_block_other_commands() -> Result<()> {
        let changes = ChangeHub::new();
        let initial = changes.token()?;
        let (jobs, receiver) = mpsc::sync_channel(2);

        let (mut wait_client, wait_server) = UnixStream::pair()?;
        wait_client.set_read_timeout(Some(IO_TIMEOUT))?;
        let wait_jobs = jobs.clone();
        let wait_changes = changes.clone();
        let wait_worker = thread::spawn(move || {
            process_connection(wait_server, wait_jobs, wait_changes, Admission::default())
        });
        send(
            &mut wait_client,
            &Request {
                protocol: PROTOCOL,
                command: Command::WaitForChange {
                    after: initial,
                    timeout_ms: 900,
                },
            },
        )?;

        let (mut command_client, command_server) = UnixStream::pair()?;
        command_client.set_read_timeout(Some(IO_TIMEOUT))?;
        let command_changes = changes.clone();
        let command_worker = thread::spawn(move || {
            process_connection(command_server, jobs, command_changes, Admission::default())
        });
        send(
            &mut command_client,
            &Request {
                protocol: PROTOCOL,
                command: Command::List,
            },
        )?;

        let job = receiver.recv_timeout(IO_TIMEOUT)?;
        assert!(matches!(job.request.command, Command::List));
        assert!(
            receiver.try_recv().is_err(),
            "wait must never enter Host jobs"
        );
        job.reply.send(Response {
            protocol: PROTOCOL,
            views: Vec::new(),
            error: None,
            rejected_before_admission: None,
            catalog: None,
            selection: None,
            context: None,
            launch_preview: None,
            launch_phase: None,
            catalog_notice: None,
            recovery: None,
            selected: None,
            history_chunk: None,
            change_token: None,
            activity: None,
            activity_error: None,
            generations: None,
            generation_selection: None,
            history_view: None,
            providers: None,
            template_run: None,
            template_runs: None,
            panel_directory: None,
            workspace_git_snapshot: None,
            workspace_git_diff: None,
            workspace_git_index: None,
            workspace_git_commit_plan: None,
            workspace_git_commit: None,
            host_status: None,
        })?;
        assert!(receive::<Response>(&mut command_client)?.error.is_none());
        command_worker.join().unwrap()?;

        let changed = changes.notify()?;
        let response: Response = receive(&mut wait_client)?;
        assert!(response.views.is_empty());
        assert!(response.error.is_none());
        assert_eq!(response.change_token, Some(changed));
        wait_worker.join().unwrap()?;
        Ok(())
    }
}

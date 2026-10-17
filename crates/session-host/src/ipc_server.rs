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
        Arc,
        atomic::{AtomicUsize, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
    thread,
    time::Duration,
};

use anyhow::{Context, Result, ensure};

use crate::{
    CommandRejection, PROTOCOL, Request, Response, encode, peer_pid, receive_request, send,
    send_encoded,
};

const CONNECTION_LIMIT: usize = 16;
const JOB_LIMIT: usize = 16;
const IO_TIMEOUT: Duration = Duration::from_secs(2);
const REPLY_TIMEOUT: Duration = Duration::from_secs(15);

pub struct Job {
    pub request: Request,
    pub reply: SyncSender<Response>,
}

/// Caller must hold the host ownership lock and have validated its private state
/// directory. The worker admission count also bounds simultaneous frame buffers.
pub fn bind(socket: &Path) -> Result<Receiver<Job>> {
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
                // If spawn fails, dropping its closure also releases the slot.
                let _ = thread::Builder::new()
                    .name("pigade-ipc-client".into())
                    .spawn(move || {
                        let _slot = slot;
                        let _ = process_connection(stream, sender);
                    });
            }
        })?;
    Ok(receiver)
}

struct ConnectionSlot(Arc<AtomicUsize>);
impl Drop for ConnectionSlot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

fn error_response(
    error: impl Into<String>,
    rejected_before_admission: Option<CommandRejection>,
) -> Response {
    Response {
        protocol: PROTOCOL,
        views: Vec::new(),
        error: Some(error.into()),
        rejected_before_admission,
        catalog: None,
        catalog_notice: None,
        recovery: None,
        selected: None,
        history_chunk: None,
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

fn process_connection(mut stream: UnixStream, jobs: SyncSender<Job>) -> Result<()> {
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
    let (reply, receiver) = mpsc::sync_channel(1);
    match jobs.try_send(Job { request, reply }) {
        Ok(()) => (),
        Err(mpsc::TrySendError::Full(_)) => {
            send(
                &mut stream,
                &error_response(
                    "host command queue full; command was not admitted",
                    Some(CommandRejection::QueueFull),
                ),
            )?;
            return Ok(());
        }
        Err(mpsc::TrySendError::Disconnected(_)) => {
            send(
                &mut stream,
                &error_response("host command handler unavailable", None),
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
        let worker = thread::spawn(move || process_connection(server, jobs));
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
            catalog_notice: None,
            recovery: None,
            selected: None,
            history_chunk: None,
        })?;
        assert!(receive::<Response>(&mut client)?.error.is_none());
        worker.join().unwrap()?;
        Ok(())
    }

    #[test]
    fn incompatible_protocol_is_rejected_before_dispatch() -> Result<()> {
        let (mut client, server) = UnixStream::pair()?;
        client.set_read_timeout(Some(IO_TIMEOUT))?;
        let (jobs, receiver) = mpsc::sync_channel(1);
        let worker = thread::spawn(move || process_connection(server, jobs));
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
        })
        .ok()
        .unwrap();
        let worker = thread::spawn(move || process_connection(server, jobs));
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
        let worker = thread::spawn(move || process_connection(server, jobs));
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
}

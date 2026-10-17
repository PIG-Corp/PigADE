//! Bounded cold-history I/O runs away from the host's command/SQLite thread.
use crate::{CommandRejection, PROTOCOL, Response, history};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex, mpsc};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Query {
    pub session: Uuid,
    pub generation: Uuid,
    pub expected_sequence: u64,
    pub ordinal: u64,
    pub offset: u64,
    pub max_bytes: usize,
}
impl Query {
    pub(crate) fn validate(&self) -> Result<()> {
        ensure!(
            (1..=history::MAX_RECORD_CHUNK_BYTES).contains(&self.max_bytes),
            "history chunk size outside allowed range"
        );
        Ok(())
    }
}

struct Task {
    source: Arc<history::History>,
    query: Query,
    reply: mpsc::SyncSender<Response>,
}

pub(crate) struct Reader {
    sender: mpsc::SyncSender<Task>,
}
impl Reader {
    pub(crate) fn new() -> Result<Self> {
        let (sender, receiver) = mpsc::sync_channel::<Task>(4);
        let receiver = Arc::new(Mutex::new(receiver));
        for index in 0..2 {
            let receiver = receiver.clone();
            std::thread::Builder::new()
                .name(format!("pigade-history-read-{index}"))
                .spawn(move || {
                    loop {
                        let task = receiver.lock().expect("history reader queue").recv();
                        let Ok(task) = task else { break };
                        let q = task.query;
                        let result = task.source.record_chunk(
                            q.expected_sequence,
                            q.ordinal,
                            q.offset,
                            q.max_bytes,
                        );
                        let _ = task.reply.try_send(response(result, None));
                    }
                })?;
        }
        Ok(Self { sender })
    }

    pub(crate) fn submit(
        &self,
        source: Arc<history::History>,
        query: Query,
        reply: mpsc::SyncSender<Response>,
    ) {
        let task = Task {
            source,
            query,
            reply,
        };
        match self.sender.try_send(task) {
            Ok(()) => (),
            Err(mpsc::TrySendError::Full(task)) => {
                let _ = task.reply.try_send(response(
                    Err(CommandRejection::QueueFull.into()),
                    Some(CommandRejection::QueueFull),
                ));
            }
            Err(mpsc::TrySendError::Disconnected(task)) => {
                let _ = task.reply.try_send(response(
                    Err(anyhow::anyhow!("history reader unavailable")),
                    None,
                ));
            }
        }
    }
}

pub(crate) fn response(
    result: Result<history::RecordChunk>,
    rejection: Option<CommandRejection>,
) -> Response {
    let (history_chunk, error) = match result {
        Ok(chunk) => (Some(chunk), None),
        Err(error) => (None, Some(format!("{error:#}"))),
    };
    Response {
        protocol: PROTOCOL,
        views: Vec::new(),
        error,
        rejected_before_admission: rejection,
        catalog: None,
        selection: None,
        context: None,
        launch_preview: None,
        launch_phase: None,
        catalog_notice: None,
        recovery: None,
        selected: None,
        history_chunk,
        change_token: None,
    }
}

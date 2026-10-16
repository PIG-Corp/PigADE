//! Host protocol 2 display-only DTOs. No terminal parser/core dependency.
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    io::{Read, Write},
    os::unix::{fs::MetadataExt, net::UnixStream},
    path::Path,
    time::Duration,
};

pub const LIMIT: usize = 8 * 1024 * 1024;
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct Geometry {
    pub rows: u16,
    pub cols: u16,
}
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct Cell {
    pub text: String,
    pub fg: String,
    pub bg: String,
    pub flags: u16,
    pub underline: Option<String>,
    pub hyperlink: Option<u32>,
}
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct Frame {
    pub geometry: Geometry,
    pub cells: Vec<Cell>,
    pub hyperlinks: Vec<(String, String)>,
    pub cursor: (i32, usize),
    pub cursor_style: String,
    pub modes: u32,
    pub colors: String,
    pub history_lines: usize,
    pub sync_pending_bytes: usize,
    pub title: String,
}
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct View {
    pub session: String,
    pub generation: String,
    pub sequence: u64,
    pub state: String,
    pub recovery_path: String,
    pub pid: Option<u32>,
    pub storage_error: Option<String>,
    pub frame: Frame,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct Known {
    pub session: String,
    pub generation: String,
    pub sequence: u64,
    pub state: String,
    pub pid: Option<u32>,
    pub storage_error: Option<String>,
}
impl View {
    pub fn known(&self) -> Known {
        Known {
            session: self.session.clone(),
            generation: self.generation.clone(),
            sequence: self.sequence,
            state: self.state.clone(),
            pid: self.pid,
            storage_error: self.storage_error.clone(),
        }
    }
}
#[derive(Deserialize)]
struct Response {
    protocol: u32,
    views: Vec<View>,
    error: Option<String>,
    #[serde(default)]
    rejected_before_admission: Option<String>,
    #[serde(default)]
    selected: Option<Vec<Known>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CommandFailure {
    NotAdmitted { code: String, message: String },
    OutcomeUnknown(String),
}

impl std::fmt::Display for CommandFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotAdmitted { message, .. } | Self::OutcomeUnknown(message) => {
                f.write_str(message)
            }
        }
    }
}

pub fn request(socket: &Path, command: Value) -> Result<Vec<View>, CommandFailure> {
    let response = request_response(socket, command).map_err(CommandFailure::OutcomeUnknown)?;
    command_result(response)
}

fn command_result(response: Response) -> Result<Vec<View>, CommandFailure> {
    if let Some(message) = response.error {
        return if response.rejected_before_admission.as_deref() == Some("queue_full") {
            Err(CommandFailure::NotAdmitted {
                code: "queue_full".into(),
                message,
            })
        } else {
            Err(CommandFailure::OutcomeUnknown(message))
        };
    }
    Ok(response.views)
}

fn request_response(socket: &Path, command: Value) -> Result<Response, String> {
    // Host and UI run as the same user. Reject group/world-accessible endpoints.
    let metadata = socket.metadata().map_err(|e| e.to_string())?;
    let own_uid = std::fs::metadata("/proc/self")
        .map_err(|e| e.to_string())?
        .uid();
    if metadata.uid() != own_uid || metadata.mode() & 0o077 != 0 {
        return Err("Host endpoint must be private (0700/0600)".into());
    }
    let mut stream = UnixStream::connect(socket).map_err(|e| e.to_string())?;
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .map_err(|e| e.to_string())?;
    stream
        .set_write_timeout(Some(Duration::from_secs(2)))
        .map_err(|e| e.to_string())?;
    let payload =
        serde_json::to_vec(&json!({"protocol":2,"command":command})).map_err(|e| e.to_string())?;
    if payload.len() > LIMIT {
        return Err("request exceeds limit".into());
    }
    stream
        .write_all(&(payload.len() as u32).to_le_bytes())
        .and_then(|_| stream.write_all(&payload))
        .map_err(|e| e.to_string())?;
    let mut length = [0; 4];
    stream.read_exact(&mut length).map_err(|e| e.to_string())?;
    let length = u32::from_le_bytes(length) as usize;
    if length == 0 || length > LIMIT {
        return Err("invalid response length".into());
    }
    let mut payload = vec![0; length];
    stream.read_exact(&mut payload).map_err(|e| e.to_string())?;
    let response: Response = serde_json::from_slice(&payload).map_err(|e| e.to_string())?;
    if response.protocol != 2 {
        return Err("incompatible host protocol".into());
    }
    if response.error.is_some() {
        return Ok(response);
    }
    for view in &response.views {
        let geometry = &view.frame.geometry;
        if geometry.rows == 0
            || geometry.cols == 0
            || geometry.rows as usize * geometry.cols as usize > 32768
            || view.frame.cells.len() != geometry.rows as usize * geometry.cols as usize
        {
            return Err("invalid cell frame geometry".into());
        }
        if view.frame.cells.iter().any(|c| {
            c.hyperlink
                .is_some_and(|i| i as usize >= view.frame.hyperlinks.len())
        }) {
            return Err("invalid hyperlink pool index".into());
        }
    }
    Ok(response)
}

pub struct Poll {
    pub selected: Vec<Known>,
    pub views: Vec<View>,
}
pub fn poll(socket: &Path, known: &[Known]) -> Result<Poll, String> {
    let reply = request_response(socket, json!({"command":"poll", "known":known}))?;
    if let Some(error) = reply.error {
        return Err(error);
    }
    Ok(Poll {
        selected: reply
            .selected
            .ok_or("Host does not support conditional cell polling")?,
        views: reply.views,
    })
}

/// Validate the whole replica update before moving any cached frame. Unchanged
/// cells stay in the cache; removals and new generations follow the host list.
pub fn merge_poll(current: &mut Vec<View>, reply: Poll) -> Result<bool, String> {
    if reply.selected.len() > 4 || reply.views.len() > 4 {
        return Err("Too many selected terminal frames".into());
    }
    let key = |known: &Known| (known.session.clone(), known.generation.clone());
    let mut selected = std::collections::HashSet::new();
    for info in &reply.selected {
        if !selected.insert(key(info)) {
            return Err("Duplicate selected terminal generation".into());
        }
        let changed = reply
            .views
            .iter()
            .filter(|v| key(&v.known()) == key(info))
            .collect::<Vec<_>>();
        if changed.len() > 1 {
            return Err("Duplicate changed terminal generation".into());
        }
        let previous = current.iter().find(|v| key(&v.known()) == key(info));
        let view = changed
            .first()
            .copied()
            .or(previous)
            .ok_or("Missing terminal replica")?;
        if view.known() != *info {
            return Err("Terminal metadata/frame watermark mismatch".into());
        }
        if previous.is_some_and(|old| old.sequence > view.sequence) {
            return Err("Host sequence regression".into());
        }
    }
    if reply
        .views
        .iter()
        .any(|v| !selected.contains(&key(&v.known())))
    {
        return Err("Unselected terminal frame".into());
    }
    let changed = !reply.views.is_empty()
        || current.len() != reply.selected.len()
        || current
            .iter()
            .zip(&reply.selected)
            .any(|(v, info)| v.known() != *info);
    if !changed {
        return Ok(false);
    }
    let mut old = std::mem::take(current);
    let mut updates = reply.views;
    for info in reply.selected {
        if let Some(index) = updates.iter().position(|v| key(&v.known()) == key(&info)) {
            current.push(updates.swap_remove(index));
        } else {
            let index = old
                .iter()
                .position(|v| key(&v.known()) == key(&info))
                .expect("validated cached replica");
            current.push(old.swap_remove(index));
        }
    }
    Ok(true)
}

pub fn command(view: &View, name: &str) -> Value {
    json!({"command":name,"session":view.session,"generation":view.generation})
}

#[cfg(test)]
mod tests {
    use super::*;

    fn error_response(error: &str, rejection: Option<&str>) -> Response {
        Response {
            protocol: 2,
            views: Vec::new(),
            error: Some(error.into()),
            rejected_before_admission: rejection.map(str::to_owned),
            selected: None,
        }
    }

    #[test]
    fn explicit_queue_full_marker_is_typed_as_not_admitted() {
        assert_eq!(
            command_result(error_response("queue full", Some("queue_full"))),
            Err(CommandFailure::NotAdmitted {
                code: "queue_full".into(),
                message: "queue full".into(),
            })
        );
    }

    #[test]
    fn unmarked_and_unknown_rejections_remain_outcome_unknown() {
        for rejection in [None, Some("future_code")] {
            assert_eq!(
                command_result(error_response("request may have been applied", rejection)),
                Err(CommandFailure::OutcomeUnknown(
                    "request may have been applied".into()
                ))
            );
        }
    }

    fn fixture() -> View {
        let value = json!({"session":"s","generation":"g","sequence":1,"state":"running","recovery_path":"live","pid":1,"storage_error":null,"frame":{"geometry":{"rows":1,"cols":1},"cells":[{"text":"界","fg":"Named(Foreground)","bg":"Named(Background)","flags":32,"underline":null,"hyperlink":0}],"hyperlinks":[["id","https://example.org"]],"cursor":[0,0],"cursor_style":"Block","modes":0,"colors":"","history_lines":0,"sync_pending_bytes":0,"title":""}});
        serde_json::from_value(value).unwrap()
    }
    #[test]
    fn request_is_generation_scoped() {
        let view = fixture();
        assert_eq!(
            command(&view, "snapshot"),
            json!({"command":"snapshot","session":"s","generation":"g"})
        );
        assert_eq!(view.frame.cells[0].text, "界");
    }
    #[test]
    fn unchanged_poll_reuses_the_existing_cell_allocation() {
        let mut cached = vec![fixture()];
        let cells = cached[0].frame.cells.as_ptr();
        let selected = vec![cached[0].known()];
        assert!(
            !merge_poll(
                &mut cached,
                Poll {
                    selected,
                    views: vec![]
                }
            )
            .unwrap()
        );
        assert_eq!(cells, cached[0].frame.cells.as_ptr());
    }
    #[test]
    fn lifecycle_changes_without_output_replace_the_replica() {
        let mut cached = vec![fixture()];
        let mut next = cached[0].clone();
        next.state = "degraded".into();
        next.storage_error = Some("disk full".into());
        assert!(
            merge_poll(
                &mut cached,
                Poll {
                    selected: vec![next.known()],
                    views: vec![next.clone()]
                }
            )
            .unwrap()
        );
        assert_eq!(cached, vec![next]);
    }
    #[test]
    fn new_generation_can_start_at_a_lower_sequence() {
        let mut cached = vec![fixture()];
        let mut next = fixture();
        next.generation = "new".into();
        next.sequence = 0;
        assert!(
            merge_poll(
                &mut cached,
                Poll {
                    selected: vec![next.known()],
                    views: vec![next.clone()]
                }
            )
            .unwrap()
        );
        assert_eq!(cached, vec![next]);
        assert!(
            merge_poll(
                &mut cached,
                Poll {
                    selected: vec![],
                    views: vec![]
                }
            )
            .unwrap()
        );
        assert!(cached.is_empty());
    }
    #[test]
    fn invalid_poll_is_atomic_and_keeps_the_last_good_replica() {
        let expected = vec![fixture()];
        let mut regression = fixture();
        regression.sequence = 0;
        let invalid = vec![
            Poll {
                selected: vec![regression.known()],
                views: vec![regression],
            },
            Poll {
                selected: vec![fixture().known(), fixture().known()],
                views: vec![],
            },
            Poll {
                selected: vec![],
                views: vec![fixture()],
            },
            Poll {
                selected: vec![fixture().known()],
                views: vec![fixture(), fixture()],
            },
            Poll {
                selected: vec![Known {
                    sequence: 2,
                    ..fixture().known()
                }],
                views: vec![],
            },
        ];
        for reply in invalid {
            let mut cached = expected.clone();
            assert!(merge_poll(&mut cached, reply).is_err());
            assert_eq!(cached, expected);
        }
    }
}

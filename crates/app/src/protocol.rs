//! Host protocol 2 display-only DTOs. No terminal parser/core dependency.
use serde::Deserialize;
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
#[derive(Deserialize)]
struct Response {
    protocol: u32,
    views: Vec<View>,
    error: Option<String>,
}

pub fn request(socket: &Path, command: Value) -> Result<Vec<View>, String> {
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
    if let Some(error) = response.error {
        return Err(error);
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
    Ok(response.views)
}

pub fn command(view: &View, name: &str) -> Value {
    json!({"command":name,"session":view.session,"generation":view.generation})
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn request_is_generation_scoped() {
        let value = json!({"session":"s","generation":"g","sequence":1,"state":"running","recovery_path":"live","pid":1,"storage_error":null,"frame":{"geometry":{"rows":1,"cols":1},"cells":[{"text":"界","fg":"Named(Foreground)","bg":"Named(Background)","flags":32,"underline":null,"hyperlink":0}],"hyperlinks":[["id","https://example.org"]],"cursor":[0,0],"cursor_style":"Block","modes":0,"colors":"","history_lines":0,"sync_pending_bytes":0,"title":""}});
        let view: View = serde_json::from_value(value).unwrap();
        assert_eq!(
            command(&view, "snapshot"),
            json!({"command":"snapshot","session":"s","generation":"g"})
        );
        assert_eq!(view.frame.cells[0].text, "界");
    }
}

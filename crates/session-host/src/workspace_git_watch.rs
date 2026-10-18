//! Bounded, read-only inotify coverage for a workspace Git tree.
#[cfg(target_os = "linux")]
use std::collections::{BTreeSet, HashMap};
#[cfg(target_os = "linux")]
use std::ffi::CString;
#[cfg(target_os = "linux")]
use std::fs::{self, File};
#[cfg(target_os = "linux")]
use std::io;
#[cfg(target_os = "linux")]
use std::os::fd::{AsRawFd, FromRawFd};
#[cfg(target_os = "linux")]
use std::os::unix::ffi::OsStrExt;
#[cfg(target_os = "linux")]
use std::path::Path;
use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;
#[cfg(target_os = "linux")]
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[cfg(target_os = "linux")]
const MAX_WATCHES: usize = 4;
#[cfg(target_os = "linux")]
const MAX_DIRS_PER_WATCH: usize = 1024;
#[cfg(target_os = "linux")]
const WATCH_TTL: Duration = Duration::from_secs(15);
#[cfg(target_os = "linux")]
const COMMAND_TIMEOUT: Duration = Duration::from_secs(3);
#[cfg(target_os = "linux")]
const FILE_LIST_LIMIT: usize = 2 * 1024 * 1024;
#[cfg(target_os = "linux")]
const EVENT_BUDGET: usize = 64 * 1024;
#[cfg(target_os = "linux")]
const QUIET_PERIOD: Duration = Duration::from_millis(350);
#[cfg(target_os = "linux")]
const REBUILD_DELAY: Duration = Duration::from_millis(100);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkspaceGitWatch {
    pub workspace_id: Uuid,
    pub workspace_revision: u64,
    pub project_id: Uuid,
    pub cwd: PathBuf,
    pub epoch: Uuid,
    pub sequence: u64,
    pub complete: bool,
    pub rebuilding: bool,
    pub settled: bool,
    pub notice: Option<String>,
}

struct Request {
    target: super::Target,
    reply: mpsc::SyncSender<crate::Response>,
}

pub(crate) struct WatchReader {
    sender: mpsc::SyncSender<Request>,
}

impl WatchReader {
    pub(crate) fn new() -> Result<Self> {
        let (sender, receiver) = mpsc::sync_channel::<Request>(1);
        thread::Builder::new()
            .name("pigade-workspace-git-watch".into())
            .spawn(move || worker(receiver))
            .context("cannot start workspace Git watch worker")?;
        Ok(Self { sender })
    }

    pub(crate) fn submit(&self, target: super::Target, reply: mpsc::SyncSender<crate::Response>) {
        let request = Request { target, reply };
        match self.sender.try_send(request) {
            Ok(()) => {}
            Err(mpsc::TrySendError::Full(request)) => {
                let mut response = super::response(None, None);
                response.workspace_git_watch = Some(unavailable(
                    &request.target,
                    "Git watch request queue is full",
                ));
                let _ = request.reply.try_send(response);
            }
            Err(mpsc::TrySendError::Disconnected(request)) => {
                let mut response = super::response(None, None);
                response.workspace_git_watch = Some(unavailable(
                    &request.target,
                    "Git watch worker is unavailable",
                ));
                let _ = request.reply.try_send(response);
            }
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn worker(receiver: mpsc::Receiver<Request>) {
    while let Ok(request) = receiver.recv() {
        let mut response = super::response(None, None);
        response.workspace_git_watch = Some(unavailable(
            &request.target,
            "inotify is available only on Linux",
        ));
        let _ = request.reply.try_send(response);
    }
}

#[cfg(target_os = "linux")]
fn worker(receiver: mpsc::Receiver<Request>) {
    let mut watches: Vec<Scope> = Vec::new();
    let mut cursor = 0usize;
    loop {
        match receiver.recv_timeout(Duration::from_millis(200)) {
            Ok(request) => {
                let now = Instant::now();
                prune_expired(&mut watches, now);
                let same = watches
                    .iter()
                    .position(|scope| scope.matches(&request.target));
                let index = if let Some(index) = same {
                    watches[index].last_used = now;
                    index
                } else {
                    if watches.len() == MAX_WATCHES {
                        evict_oldest_slot(&mut watches, &mut cursor);
                    }
                    let scope = Scope::install(request.target.clone());
                    watches.push(scope);
                    watches.len() - 1
                };
                let status = watches[index].snapshot();
                let mut response = super::response(None, None);
                response.workspace_git_watch = Some(status);
                let _ = request.reply.try_send(response);
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        let now = Instant::now();
        prune_expired(&mut watches, now);
        if watches.is_empty() {
            cursor = 0;
            continue;
        }
        let start = cursor % watches.len();
        for step in 0..watches.len() {
            let index = (start + step) % watches.len();
            watches[index].tick();
        }
        cursor = (start + 1) % watches.len();
    }
}

#[cfg(target_os = "linux")]
fn prune_expired(watches: &mut Vec<Scope>, now: Instant) {
    watches.retain(|scope| now.duration_since(scope.last_used) < WATCH_TTL);
}

#[cfg(target_os = "linux")]
fn evict_oldest_slot(watches: &mut Vec<Scope>, cursor: &mut usize) {
    if !watches.is_empty() {
        let evict = *cursor % watches.len();
        watches.remove(evict);
        *cursor = 0;
    }
}

fn unavailable(target: &super::Target, notice: &str) -> WorkspaceGitWatch {
    WorkspaceGitWatch {
        workspace_id: target.workspace_id,
        workspace_revision: target.workspace_revision,
        project_id: target.project_id,
        cwd: target.cwd.clone(),
        epoch: Uuid::new_v4(),
        sequence: 0,
        complete: false,
        rebuilding: false,
        settled: false,
        notice: Some(super::bounded_notice(notice)),
    }
}

#[cfg(target_os = "linux")]
struct Scope {
    target: super::Target,
    fd: Option<File>,
    epoch: Uuid,
    sequence: u64,
    dirs: HashMap<i32, PathBuf>,
    git_dirs: BTreeSet<PathBuf>,
    complete: bool,
    rebuilding: bool,
    settled: bool,
    notice: Option<String>,
    last_event: Option<Instant>,
    rebuild_at: Option<Instant>,
    last_used: Instant,
}

#[cfg(target_os = "linux")]
impl Scope {
    fn matches(&self, target: &super::Target) -> bool {
        self.target.workspace_id == target.workspace_id
            && self.target.workspace_revision == target.workspace_revision
            && self.target.project_id == target.project_id
            && self.target.cwd == target.cwd
            && self.target.git_common_dir == target.git_common_dir
    }

    fn install(target: super::Target) -> Self {
        let fd = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
        let mut scope = if fd >= 0 {
            // SAFETY: inotify_init1 returns a fresh owned descriptor.
            Self {
                target,
                fd: Some(unsafe { File::from_raw_fd(fd) }),
                epoch: Uuid::new_v4(),
                sequence: 0,
                dirs: HashMap::new(),
                git_dirs: BTreeSet::new(),
                complete: false,
                rebuilding: false,
                settled: false,
                notice: None,
                last_event: None,
                rebuild_at: None,
                last_used: Instant::now(),
            }
        } else {
            let reason = format!("cannot initialize inotify: {}", io::Error::last_os_error());
            Self {
                target,
                fd: None,
                epoch: Uuid::new_v4(),
                sequence: 0,
                dirs: HashMap::new(),
                git_dirs: BTreeSet::new(),
                complete: false,
                rebuilding: false,
                settled: false,
                notice: Some(reason),
                last_event: None,
                rebuild_at: None,
                last_used: Instant::now(),
            }
        };
        if scope.fd.is_some() {
            scope.rebuild();
        }
        scope
    }

    fn snapshot(&self) -> WorkspaceGitWatch {
        WorkspaceGitWatch {
            workspace_id: self.target.workspace_id,
            workspace_revision: self.target.workspace_revision,
            project_id: self.target.project_id,
            cwd: self.target.cwd.clone(),
            epoch: self.epoch,
            sequence: self.sequence,
            complete: self.complete,
            rebuilding: self.rebuilding,
            settled: self.settled,
            notice: self.notice.as_ref().map(super::bounded_notice),
        }
    }

    fn tick(&mut self) {
        let Some(fd) = self.fd.as_ref().map(AsRawFd::as_raw_fd) else {
            return;
        };
        let mut buf = [0u8; EVENT_BUDGET];
        let mut batch = Vec::new();
        let mut read_budget = 4usize;
        while read_budget > 0 {
            read_budget -= 1;
            let count = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
            if count < 0 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::WouldBlock {
                    break;
                }
                if error.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                self.invalidate(format!("inotify read failed: {error}"));
                return;
            }
            if count == 0 {
                break;
            }
            let parsed = parse_events(&buf[..count as usize]);
            match parsed {
                Ok(events) => batch.extend(events),
                Err(message) => {
                    self.invalidate(message);
                    return;
                }
            }
        }
        if !batch.is_empty() {
            self.process_event_batch(&batch);
        }
        if self.rebuild_at.is_some_and(|at| Instant::now() >= at) {
            self.rebuild();
        }
        if self.complete
            && self
                .last_event
                .is_some_and(|at| Instant::now().duration_since(at) >= QUIET_PERIOD)
        {
            self.settled = true;
        }
    }

    fn process_event_batch(&mut self, events: &[Event]) {
        self.sequence = self.sequence.saturating_add(1);
        self.last_event = Some(Instant::now());
        self.settled = false;
        let lost = events.iter().any(event_is_lost);
        if lost {
            self.invalidate("inotify reported queue overflow or watch loss".into());
        } else if events
            .iter()
            .any(|event| needs_rebuild(event, &self.dirs, &self.git_dirs))
        {
            self.pending_rescan();
        }
    }

    fn invalidate(&mut self, reason: String) {
        self.complete = false;
        self.rebuilding = true;
        self.settled = false;
        self.notice = Some(reason);
        self.sequence = self.sequence.saturating_add(1);
        self.last_event = Some(Instant::now());
        self.rebuild_at = Some(Instant::now() + REBUILD_DELAY);
    }

    fn rebuild(&mut self) {
        self.rebuild_at = None;
        let new_fd = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
        if new_fd < 0 {
            self.install_failed(format!(
                "cannot create replacement inotify queue: {}",
                io::Error::last_os_error()
            ));
            return;
        }
        // Install the workspace root first so it reports new top-level
        // directories while the bounded Git inventory is being prepared.
        let new_fd = unsafe { File::from_raw_fd(new_fd) };
        let root = match fs::canonicalize(&self.target.cwd) {
            Ok(root) => root,
            Err(error) => {
                self.install_failed(format!("workspace root unavailable: {error}"));
                return;
            }
        };
        let mut new_watches = HashMap::new();
        match add_watch_to_map(new_fd.as_raw_fd(), &root, &mut new_watches, false) {
            Ok(true) => {}
            Ok(false) => {
                self.install_failed("workspace root disappeared during watch installation".into());
                return;
            }
            Err(error) => {
                self.install_failed(format!("cannot watch workspace root: {error:#}"));
                return;
            }
        }
        if let Err(error) =
            super::validate_git_identity(&self.target, Instant::now() + COMMAND_TIMEOUT)
        {
            self.install_failed(format!("Git watch identity validation failed: {error:#}"));
            return;
        }
        let output = super::run_git_capture(
            &root,
            &[
                "ls-files",
                "--cached",
                "--others",
                "--exclude-standard",
                "-z",
            ],
            FILE_LIST_LIMIT,
            Instant::now() + COMMAND_TIMEOUT,
            "git ls-files watch paths",
        );
        let (paths, truncated) = match output {
            Ok((bytes, truncated)) => (bytes, truncated),
            Err(error) => {
                self.install_failed(format!("cannot enumerate Git paths: {error:#}"));
                return;
            }
        };
        let mut dirs = BTreeSet::new();
        dirs.insert(root.clone());
        for entry in paths
            .split(|byte| *byte == 0)
            .filter(|entry| !entry.is_empty())
        {
            let relative = PathBuf::from(std::ffi::OsStr::from_bytes(entry));
            if relative.is_absolute()
                || relative.components().any(|c| {
                    matches!(
                        c,
                        std::path::Component::ParentDir | std::path::Component::RootDir
                    )
                })
            {
                self.install_failed("Git returned an unsafe path while installing watches".into());
                return;
            }
            let mut path = root.join(relative);
            while path.pop() {
                dirs.insert(path.clone());
                if path == root {
                    break;
                }
            }
        }
        let worktree_dirs = dirs.clone();
        let git_dir_output = super::run_git_capture(
            &root,
            &["rev-parse", "--absolute-git-dir"],
            16 * 1024,
            Instant::now() + COMMAND_TIMEOUT,
            "git rev-parse absolute git dir",
        );
        let mut git_dirs = BTreeSet::new();
        match git_dir_output {
            Ok((bytes, false)) => {
                let git_dir = PathBuf::from(super::remove_git_line_ending(
                    &String::from_utf8_lossy(&bytes),
                ));
                let common_dir = self.target.git_common_dir.clone();
                git_dirs.insert(git_dir.clone());
                git_dirs.insert(common_dir.clone());
                add_metadata_dirs(&mut dirs, &git_dir);
                add_metadata_dirs(&mut dirs, &common_dir);
                for metadata in [&git_dir, &common_dir] {
                    match add_optional_directory(&mut dirs, &metadata.join("info")) {
                        Ok(()) => {}
                        Err(error) => {
                            self.install_failed(format!("Git info directory is unsafe: {error:#}"));
                            return;
                        }
                    }
                    if !add_directory_tree(&mut dirs, &metadata.join("refs"), MAX_DIRS_PER_WATCH) {
                        self.install_failed(
                            "Git refs watch coverage exceeded its directory limit".into(),
                        );
                        return;
                    }
                }
            }
            Ok((_, true)) => {
                self.install_failed("Git metadata path output exceeded its bound".into());
                return;
            }
            Err(error) => {
                self.install_failed(format!("Git metadata path unavailable: {error:#}"));
                return;
            }
        }
        if dirs.len() > MAX_DIRS_PER_WATCH || truncated {
            self.install_failed(
                "Git watch coverage is incomplete: directory or path-list limit reached".into(),
            );
            return;
        }
        for path in dirs {
            if new_watches.values().any(|watched| watched == &path) {
                continue;
            }
            let missing_ok = path != root && worktree_dirs.contains(&path);
            match add_watch_to_map(new_fd.as_raw_fd(), &path, &mut new_watches, missing_ok) {
                Ok(true) => {}
                Ok(false) => continue,
                Err(error) => {
                    self.install_failed(format!("Git watch rejected a directory: {error:#}"));
                    return;
                }
            }
        }
        if let Err(error) =
            super::validate_git_identity(&self.target, Instant::now() + COMMAND_TIMEOUT)
        {
            self.install_failed(format!(
                "Git watch identity changed during install: {error:#}"
            ));
            return;
        }
        let verify_paths = super::run_git_capture(
            &root,
            &[
                "ls-files",
                "--cached",
                "--others",
                "--exclude-standard",
                "-z",
            ],
            FILE_LIST_LIMIT,
            Instant::now() + COMMAND_TIMEOUT,
            "git ls-files watch verification",
        );
        match verify_paths {
            Ok((current, false)) if current == paths => {}
            Ok(_) => {
                self.pending_rescan();
                return;
            }
            Err(error) => {
                self.install_failed(format!("cannot verify Git watch paths: {error:#}"));
                return;
            }
        }
        let (events, overflow) = match drain_events(new_fd.as_raw_fd()) {
            Ok(events) => events,
            Err(error) => {
                self.install_failed(format!("cannot read replacement inotify queue: {error}"));
                return;
            }
        };
        if overflow {
            self.invalidate("inotify queue overflow while installing coverage".into());
            return;
        }
        if events.iter().any(event_is_lost) {
            self.invalidate("inotify reported watch loss while installing coverage".into());
            return;
        }
        if !events.is_empty() {
            self.sequence = self.sequence.saturating_add(1);
            self.last_event = Some(Instant::now());
            self.settled = false;
            if events
                .iter()
                .any(|event| needs_rebuild(event, &new_watches, &git_dirs))
            {
                self.pending_rescan();
                return;
            }
        }
        self.fd = Some(new_fd);
        self.dirs = new_watches;
        self.git_dirs = git_dirs;
        self.complete = true;
        self.rebuilding = false;
        self.settled = false;
        self.notice = None;
        self.last_event = Some(Instant::now());
    }

    fn pending_rescan(&mut self) {
        self.complete = false;
        self.rebuilding = true;
        self.notice = Some("Git watch coverage rebuild pending".into());
        self.settled = false;
        self.last_event = Some(Instant::now());
        self.rebuild_at = Some(Instant::now() + REBUILD_DELAY);
    }

    fn install_failed(&mut self, reason: String) {
        self.complete = false;
        self.rebuilding = false;
        self.settled = false;
        self.notice = Some(reason);
        self.rebuild_at = None;
    }
}

#[cfg(target_os = "linux")]
fn add_metadata_dirs(dirs: &mut BTreeSet<PathBuf>, path: &Path) {
    dirs.insert(path.to_path_buf());
    if let Some(parent) = path.parent() {
        dirs.insert(parent.to_path_buf());
    }
}

#[cfg(target_os = "linux")]
fn add_optional_directory(dirs: &mut BTreeSet<PathBuf>, path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("cannot inspect {}", path.display())),
        Ok(metadata) if metadata.file_type().is_symlink() => {
            bail!("{} has a symlink component", path.display())
        }
        Ok(metadata) if metadata.file_type().is_dir() => {
            safe_directory_chain(path)?;
            dirs.insert(path.to_path_buf());
            Ok(())
        }
        Ok(_) => bail!("{} is not a directory", path.display()),
    }
}

#[cfg(target_os = "linux")]
fn add_watch_to_map(
    fd: i32,
    path: &Path,
    watches: &mut HashMap<i32, PathBuf>,
    missing_ok: bool,
) -> Result<bool> {
    if directory_chain_missing(path)? {
        if missing_ok {
            return Ok(false);
        }
        bail!("required watch directory {} is missing", path.display());
    }
    let descriptor = match add_watch(fd, path) {
        Ok(descriptor) => descriptor,
        Err(_error) if missing_ok && is_missing(path) => return Ok(false),
        Err(error) => return Err(error),
    };
    if directory_chain_missing(path)? {
        bail!(
            "watch directory {} disappeared during installation",
            path.display()
        );
    }
    if let Some(existing) = watches.get(&descriptor) {
        if existing != path {
            bail!("two directory paths resolved to one inotify watch descriptor");
        }
    } else {
        watches.insert(descriptor, path.to_path_buf());
    }
    Ok(true)
}

#[cfg(target_os = "linux")]
fn is_missing(path: &Path) -> bool {
    fs::symlink_metadata(path).is_err_and(|error| error.kind() == io::ErrorKind::NotFound)
}

#[cfg(target_os = "linux")]
fn drain_events(fd: i32) -> std::result::Result<(Vec<Event>, bool), String> {
    let mut buf = [0u8; EVENT_BUDGET];
    let mut all = Vec::new();
    for _ in 0..4 {
        let count = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
        if count < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::WouldBlock {
                break;
            }
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(format!("inotify read failed: {error}"));
        }
        if count == 0 {
            break;
        }
        all.extend(parse_events(&buf[..count as usize])?);
    }
    let overflow = all
        .iter()
        .any(|event| event.mask & libc::IN_Q_OVERFLOW != 0);
    Ok((all, overflow))
}

#[cfg(target_os = "linux")]
fn add_directory_tree(dirs: &mut BTreeSet<PathBuf>, root: &Path, limit: usize) -> bool {
    let mut todo = vec![root.to_path_buf()];
    let mut complete = true;
    while let Some(path) = todo.pop() {
        if dirs.len() >= limit {
            complete = false;
            continue;
        }
        match fs::symlink_metadata(&path) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(_) => {
                complete = false;
                continue;
            }
            Ok(metadata) if metadata.file_type().is_symlink() => {
                complete = false;
                continue;
            }
            Ok(metadata) if !metadata.file_type().is_dir() => continue,
            Ok(_) => {}
        }
        if !dirs.insert(path.clone()) {
            continue;
        }
        let entries = match fs::read_dir(&path) {
            Ok(entries) => entries,
            Err(_) => {
                complete = false;
                continue;
            }
        };
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(_) => {
                    complete = false;
                    continue;
                }
            };
            match entry.file_type() {
                Ok(kind) if kind.is_dir() => todo.push(entry.path()),
                Ok(kind) if kind.is_symlink() => complete = false,
                Ok(_) => {}
                Err(_) => complete = false,
            }
        }
    }
    complete
}

#[cfg(target_os = "linux")]
fn safe_directory_chain(path: &Path) -> Result<()> {
    if directory_chain_missing(path)? {
        bail!("watch directory {} is missing", path.display());
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn directory_chain_missing(path: &Path) -> Result<bool> {
    if !path.is_absolute() {
        bail!("watch path is not absolute");
    }
    let mut current = PathBuf::from("/");
    for component in path.components().skip(1) {
        current.push(component.as_os_str());
        let metadata = match fs::symlink_metadata(&current) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(true),
            Err(error) => {
                return Err(error).with_context(|| format!("cannot inspect {}", current.display()));
            }
        };
        if metadata.file_type().is_symlink() {
            bail!("{} has a symlink component", current.display());
        }
        if !metadata.file_type().is_dir() {
            bail!("{} is not a directory", current.display());
        }
    }
    Ok(false)
}

#[cfg(target_os = "linux")]
fn add_watch(fd: i32, path: &Path) -> Result<i32> {
    let path = CString::new(path.as_os_str().as_bytes()).context("watch path contains NUL")?;
    let mask = libc::IN_ATTRIB
        | libc::IN_CLOSE_WRITE
        | libc::IN_CREATE
        | libc::IN_DELETE
        | libc::IN_DELETE_SELF
        | libc::IN_MODIFY
        | libc::IN_MOVED_FROM
        | libc::IN_MOVED_TO
        | libc::IN_MOVE_SELF
        | libc::IN_UNMOUNT
        | libc::IN_ONLYDIR
        | libc::IN_DONT_FOLLOW;
    let wd = unsafe { libc::inotify_add_watch(fd, path.as_ptr(), mask) };
    if wd < 0 {
        bail!("cannot watch directory: {}", io::Error::last_os_error());
    }
    Ok(wd)
}

#[cfg(target_os = "linux")]
struct Event {
    wd: i32,
    mask: u32,
    name: Vec<u8>,
}

#[cfg(target_os = "linux")]
fn parse_events(bytes: &[u8]) -> std::result::Result<Vec<Event>, String> {
    let header = std::mem::size_of::<libc::inotify_event>();
    let mut offset = 0usize;
    let mut events = Vec::new();
    while offset < bytes.len() {
        if bytes.len() - offset < header {
            return Err("truncated inotify event header".into());
        }
        let wd = i32::from_ne_bytes(bytes[offset..offset + 4].try_into().unwrap());
        let mask = u32::from_ne_bytes(bytes[offset + 4..offset + 8].try_into().unwrap());
        let len = u32::from_ne_bytes(bytes[offset + 12..offset + 16].try_into().unwrap()) as usize;
        let end = offset
            .checked_add(header)
            .and_then(|v| v.checked_add(len))
            .ok_or("inotify event length overflow")?;
        if end > bytes.len() {
            return Err("truncated inotify event name".into());
        }
        let name_end = bytes[offset + header..end]
            .iter()
            .position(|byte| *byte == 0)
            .map_or(end, |length| offset + header + length);
        events.push(Event {
            wd,
            mask,
            name: bytes[offset + header..name_end].to_vec(),
        });
        offset = end;
    }
    Ok(events)
}

#[cfg(target_os = "linux")]
fn needs_rebuild(
    event: &Event,
    dirs: &HashMap<i32, PathBuf>,
    git_dirs: &BTreeSet<PathBuf>,
) -> bool {
    let topology = libc::IN_CREATE | libc::IN_DELETE | libc::IN_MOVED_FROM | libc::IN_MOVED_TO;
    if event.mask & libc::IN_ISDIR != 0 && event.mask & topology != 0 {
        return true;
    }
    if event.name.is_empty() {
        return false;
    }
    if event.name.as_slice() == b".gitignore" || event.name.as_slice() == b".git" {
        return event.mask
            & (libc::IN_CREATE
                | libc::IN_DELETE
                | libc::IN_MOVED_FROM
                | libc::IN_MOVED_TO
                | libc::IN_CLOSE_WRITE
                | libc::IN_MODIFY
                | libc::IN_ATTRIB)
            != 0;
    }
    let Some(parent) = dirs.get(&event.wd) else {
        return false;
    };
    let path = parent.join(std::ffi::OsStr::from_bytes(&event.name));
    let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
        return false;
    };
    // Index membership can add a previously ignored subtree without any
    // watched directory event. Exact metadata roots also support separate
    // Git directories whose basename is not `.git`.
    let metadata_name = git_dirs.iter().any(|git_dir| {
        (parent == git_dir && matches!(name, "index" | "config"))
            || (parent == &git_dir.join("info") && name == "exclude")
    });
    metadata_name
        && event.mask
            & (libc::IN_CREATE
                | libc::IN_DELETE
                | libc::IN_MOVED_FROM
                | libc::IN_MOVED_TO
                | libc::IN_CLOSE_WRITE
                | libc::IN_MODIFY
                | libc::IN_ATTRIB)
            != 0
}

#[cfg(target_os = "linux")]
fn event_is_lost(event: &Event) -> bool {
    event.mask
        & (libc::IN_Q_OVERFLOW
            | libc::IN_DELETE_SELF
            | libc::IN_MOVE_SELF
            | libc::IN_UNMOUNT
            | libc::IN_IGNORED)
        != 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "linux")]
    struct GitFixture {
        root: PathBuf,
    }

    #[cfg(target_os = "linux")]
    impl GitFixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!("pigade-git-watch-{}", Uuid::new_v4()));
            let fixture = Self { root };
            fs::create_dir_all(&fixture.root).unwrap();
            fixture.git(&fixture.root, &["init", "-q"]);
            fixture.git(&fixture.root, &["config", "user.name", "Watch Test"]);
            fixture.git(
                &fixture.root,
                &["config", "user.email", "watch@example.invalid"],
            );
            fs::create_dir_all(fixture.root.join("cache")).unwrap();
            fs::create_dir_all(fixture.root.join("ignored-cache")).unwrap();
            fs::write(fixture.root.join(".gitignore"), "cache/\nignored-cache/\n").unwrap();
            fs::write(
                fixture.root.join("cache/tracked.txt"),
                "tracked ignored content\n",
            )
            .unwrap();
            fs::write(
                fixture.root.join("ignored-cache/untracked.txt"),
                "ignored\n",
            )
            .unwrap();
            fixture.git(&fixture.root, &["add", ".gitignore"]);
            fixture.git(&fixture.root, &["add", "-f", "cache/tracked.txt"]);
            fixture.git(&fixture.root, &["commit", "-q", "-m", "initial"]);
            fixture
        }

        fn git(&self, cwd: &Path, args: &[&str]) -> Vec<u8> {
            let output = std::process::Command::new("git")
                .arg("-C")
                .arg(cwd)
                .args(args)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .output()
                .expect("run git test command");
            assert!(
                output.status.success(),
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            output.stdout
        }

        fn target(&self, cwd: &Path) -> super::super::Target {
            let root = fs::canonicalize(&self.root).unwrap();
            super::super::Target {
                workspace_id: Uuid::new_v4(),
                workspace_revision: 1,
                project_id: Uuid::new_v4(),
                cwd: fs::canonicalize(cwd).unwrap(),
                project_root: root.clone(),
                git_common_dir: fs::canonicalize(root.join(".git")).unwrap(),
            }
        }
    }

    #[cfg(target_os = "linux")]
    impl Drop for GitFixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[cfg(target_os = "linux")]
    fn settled_change(scope: &mut Scope, previous: u64) -> WorkspaceGitWatch {
        let deadline = Instant::now() + Duration::from_secs(12);
        while Instant::now() < deadline {
            scope.tick();
            let status = scope.snapshot();
            if status.sequence > previous && status.complete && status.settled {
                return status;
            }
            thread::sleep(Duration::from_millis(20));
        }
        panic!(
            "watch did not settle after a filesystem change: {:?}",
            scope.snapshot()
        );
    }

    #[cfg(target_os = "linux")]
    fn wait_settled(scope: &mut Scope) {
        let deadline = Instant::now() + Duration::from_secs(12);
        while Instant::now() < deadline {
            scope.tick();
            if scope.snapshot().complete && scope.snapshot().settled {
                return;
            }
            thread::sleep(Duration::from_millis(20));
        }
        panic!("watch did not settle: {:?}", scope.snapshot());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn event_parser_accepts_variable_names_and_rejects_truncation() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&3i32.to_ne_bytes());
        bytes.extend_from_slice(&libc::IN_CREATE.to_ne_bytes());
        bytes.extend_from_slice(&0u32.to_ne_bytes());
        bytes.extend_from_slice(&8u32.to_ne_bytes());
        bytes.extend_from_slice(b"name\0\0\0\0");
        assert_eq!(super::parse_events(&bytes).unwrap().len(), 1);
        assert!(super::parse_events(&bytes[..17]).is_err());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn event_parser_handles_unaligned_following_header_and_overflow() {
        fn event(bytes: &mut Vec<u8>, wd: i32, mask: u32, name: &[u8]) {
            bytes.extend_from_slice(&wd.to_ne_bytes());
            bytes.extend_from_slice(&mask.to_ne_bytes());
            bytes.extend_from_slice(&0u32.to_ne_bytes());
            bytes.extend_from_slice(&(name.len() as u32).to_ne_bytes());
            bytes.extend_from_slice(name);
        }
        let mut bytes = Vec::new();
        event(&mut bytes, 1, libc::IN_CREATE, b"x");
        event(&mut bytes, -1, libc::IN_Q_OVERFLOW, b"");
        let parsed = super::parse_events(&bytes).unwrap();
        assert_eq!(parsed.len(), 2);
        assert_ne!(parsed[1].mask & libc::IN_Q_OVERFLOW, 0);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn metadata_directory_walk_reports_limit() {
        let root = std::env::temp_dir().join(format!("pigade-watch-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("refs/heads/nested")).unwrap();
        let mut dirs = std::collections::BTreeSet::new();
        assert!(!super::add_directory_tree(&mut dirs, &root.join("refs"), 2));
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn git_watch_tracks_writes_atomic_replacement_and_new_directories() {
        let fixture = GitFixture::new();
        let target = fixture.target(&fixture.root);
        let mut scope = Scope::install(target);
        assert!(
            scope.complete,
            "initial scope incomplete: {:?}",
            scope.notice
        );
        let epoch = scope.epoch;
        assert!(
            scope
                .dirs
                .values()
                .any(|path| path == &fixture.root.join("cache"))
        );
        assert!(
            !scope
                .dirs
                .values()
                .any(|path| path == &fixture.root.join("ignored-cache"))
        );
        assert!(
            scope
                .dirs
                .values()
                .all(|path| path.starts_with(&fixture.root))
        );
        wait_settled(&mut scope);

        let sequence = scope.sequence;
        fs::write(
            fixture.root.join("cache/tracked.txt"),
            "written through watch\n",
        )
        .unwrap();
        settled_change(&mut scope, sequence);

        let sequence = scope.sequence;
        fs::write(fixture.root.join("cache/replacement.tmp"), "replacement\n").unwrap();
        fs::rename(
            fixture.root.join("cache/replacement.tmp"),
            fixture.root.join("cache/tracked.txt"),
        )
        .unwrap();
        settled_change(&mut scope, sequence);

        let sequence = scope.sequence;
        fs::create_dir_all(fixture.root.join("new-parent/new-child")).unwrap();
        fs::write(fixture.root.join("new-parent/new-child/file.txt"), "new\n").unwrap();
        settled_change(&mut scope, sequence);
        assert!(
            scope
                .dirs
                .values()
                .any(|path| path == &fixture.root.join("new-parent/new-child"))
        );

        let sequence = scope.sequence;
        fs::write(
            fixture.root.join("new-parent/new-child/file.txt"),
            "updated\n",
        )
        .unwrap();
        settled_change(&mut scope, sequence);
        assert_eq!(scope.epoch, epoch);
        assert!(scope.complete);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn deleted_tracked_directory_is_skipped_and_recreation_is_watched() {
        let fixture = GitFixture::new();
        let mut scope = Scope::install(fixture.target(&fixture.root));
        assert!(
            scope.complete,
            "initial scope incomplete: {:?}",
            scope.notice
        );
        wait_settled(&mut scope);

        let sequence = scope.sequence;
        fs::remove_dir_all(fixture.root.join("cache")).unwrap();
        settled_change(&mut scope, sequence);
        assert!(
            scope.complete,
            "scope incomplete after deletion: {:?}",
            scope.notice
        );
        assert!(
            !scope
                .dirs
                .values()
                .any(|path| path == &fixture.root.join("cache"))
        );

        fs::create_dir_all(fixture.root.join("cache")).unwrap();
        fs::write(fixture.root.join("cache/tracked.txt"), "recreated\n").unwrap();
        let sequence = scope.sequence;
        settled_change(&mut scope, sequence);
        assert!(
            scope.complete,
            "scope incomplete after recreation: {:?}",
            scope.notice
        );
        assert!(
            scope
                .dirs
                .values()
                .any(|path| path == &fixture.root.join("cache"))
        );

        let sequence = scope.sequence;
        fs::write(
            fixture.root.join("cache/tracked.txt"),
            "observed after recreate\n",
        )
        .unwrap();
        settled_change(&mut scope, sequence);
        assert!(scope.complete);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linked_worktree_watches_head_index_and_common_refs() {
        let fixture = GitFixture::new();
        let linked = fixture
            .root
            .with_file_name(format!("linked-{}", Uuid::new_v4()));
        fixture.git(
            &fixture.root,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "linked-watch",
                linked.to_str().unwrap(),
            ],
        );
        let target = fixture.target(&linked);
        let worktree_git_dir = PathBuf::from(
            String::from_utf8(fixture.git(&linked, &["rev-parse", "--absolute-git-dir"]))
                .unwrap()
                .trim(),
        );
        let common_refs = fixture.root.join(".git/refs/heads");
        let mut scope = Scope::install(target);
        assert!(
            scope.complete,
            "linked scope incomplete: {:?}",
            scope.notice
        );
        assert!(scope.dirs.values().any(|path| path == &worktree_git_dir));
        assert!(scope.dirs.values().any(|path| path == &common_refs));
        wait_settled(&mut scope);

        let sequence = scope.sequence;
        fixture.git(&linked, &["checkout", "-q", "-b", "watch-head-change"]);
        settled_change(&mut scope, sequence);

        let sequence = scope.sequence;
        fs::write(linked.join("staged.txt"), "index update\n").unwrap();
        fixture.git(&linked, &["add", "staged.txt"]);
        settled_change(&mut scope, sequence);

        let sequence = scope.sequence;
        fixture.git(&linked, &["commit", "-q", "-m", "update refs"]);
        settled_change(&mut scope, sequence);
        let _ = fs::remove_dir_all(linked);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn installing_and_draining_watch_does_not_mutate_worktree_or_index() {
        let fixture = GitFixture::new();
        let file = fixture.root.join("cache/tracked.txt");
        let before_file = fs::read(&file).unwrap();
        let index = fixture.root.join(".git/index");
        let before_index = fs::read(&index).unwrap();
        let mut scope = Scope::install(fixture.target(&fixture.root));
        assert!(scope.complete, "scope incomplete: {:?}", scope.notice);
        wait_settled(&mut scope);
        assert_eq!(fs::read(file).unwrap(), before_file);
        assert_eq!(fs::read(index).unwrap(), before_index);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn overflow_invalidates_scope_and_symlinked_ancestor_is_rejected() {
        let fixture = GitFixture::new();
        let mut scope = Scope::install(fixture.target(&fixture.root));
        assert!(scope.complete);
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&(-1i32).to_ne_bytes());
        bytes.extend_from_slice(&libc::IN_Q_OVERFLOW.to_ne_bytes());
        bytes.extend_from_slice(&0u32.to_ne_bytes());
        bytes.extend_from_slice(&0u32.to_ne_bytes());
        let events = parse_events(&bytes).unwrap();
        scope.process_event_batch(&events);
        assert!(!scope.complete);
        assert!(scope.rebuilding);
        assert!(scope.notice.as_deref().unwrap().contains("overflow"));

        let outside = fixture.root.join("actual-parent");
        fs::create_dir_all(outside.join("leaf")).unwrap();
        let link = fixture.root.join("linked-parent");
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        assert!(safe_directory_chain(&link.join("leaf")).is_err());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn index_membership_adds_existing_ignored_subtree_to_coverage() {
        let fixture = GitFixture::new();
        let mut scope = Scope::install(fixture.target(&fixture.root));
        wait_settled(&mut scope);
        let ignored = fixture.root.join("ignored-cache");
        assert!(!scope.dirs.values().any(|path| path == &ignored));
        let before = scope.sequence;
        fixture.git(&fixture.root, &["add", "-f", "ignored-cache/untracked.txt"]);
        settled_change(&mut scope, before);
        assert!(scope.dirs.values().any(|path| path == &ignored));
        let before = scope.sequence;
        fs::write(ignored.join("untracked.txt"), "now tracked and watched\n").unwrap();
        settled_change(&mut scope, before);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn ordinary_files_do_not_request_rebuild_but_git_metadata_does() {
        let fixture = GitFixture::new();
        let scope = Scope::install(fixture.target(&fixture.root));
        let root_wd = *scope
            .dirs
            .iter()
            .find(|(_, path)| *path == &fixture.root)
            .unwrap()
            .0;
        assert!(!needs_rebuild(
            &Event {
                wd: root_wd,
                mask: libc::IN_CLOSE_WRITE,
                name: b"file.txt".to_vec()
            },
            &scope.dirs,
            &scope.git_dirs,
        ));
        assert!(needs_rebuild(
            &Event {
                wd: root_wd,
                mask: libc::IN_CLOSE_WRITE,
                name: b".gitignore".to_vec()
            },
            &scope.dirs,
            &scope.git_dirs,
        ));
        let git_dir = fixture.root.join(".git");
        let index_wd = *scope
            .dirs
            .iter()
            .find(|(_, path)| *path == &git_dir)
            .unwrap()
            .0;
        assert!(needs_rebuild(
            &Event {
                wd: index_wd,
                mask: libc::IN_CLOSE_WRITE,
                name: b"index".to_vec()
            },
            &scope.dirs,
            &scope.git_dirs,
        ));
        assert!(!needs_rebuild(
            &Event {
                wd: index_wd,
                mask: libc::IN_CLOSE_WRITE,
                name: b"HEAD".to_vec()
            },
            &scope.dirs,
            &scope.git_dirs,
        ));
        assert!(!needs_rebuild(
            &Event {
                wd: root_wd,
                mask: libc::IN_CLOSE_WRITE,
                name: b"index.lock".to_vec()
            },
            &scope.dirs,
            &scope.git_dirs,
        ));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn watcher_cache_is_limited_to_four_scopes_and_expires_idle_entries() {
        fn empty_scope(id: Uuid, last_used: Instant) -> Scope {
            Scope {
                target: super::super::Target {
                    workspace_id: id,
                    workspace_revision: 1,
                    project_id: Uuid::new_v4(),
                    cwd: PathBuf::from("/tmp"),
                    project_root: PathBuf::from("/tmp"),
                    git_common_dir: PathBuf::from("/tmp/.git"),
                },
                fd: None,
                epoch: Uuid::new_v4(),
                sequence: 0,
                dirs: HashMap::new(),
                git_dirs: BTreeSet::new(),
                complete: false,
                rebuilding: false,
                settled: false,
                notice: None,
                last_event: None,
                rebuild_at: None,
                last_used,
            }
        }
        let now = Instant::now();
        let mut scopes: Vec<_> = (0..MAX_WATCHES)
            .map(|index| empty_scope(Uuid::from_u128(index as u128 + 1), now))
            .collect();
        let mut cursor = 0;
        evict_oldest_slot(&mut scopes, &mut cursor);
        assert_eq!(scopes.len(), MAX_WATCHES - 1);
        scopes.push(empty_scope(Uuid::new_v4(), now));
        assert_eq!(scopes.len(), MAX_WATCHES);
        scopes[0].last_used = now - WATCH_TTL - Duration::from_millis(1);
        prune_expired(&mut scopes, now);
        assert_eq!(scopes.len(), MAX_WATCHES - 1);
    }
}

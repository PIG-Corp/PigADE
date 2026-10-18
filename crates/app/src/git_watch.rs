//! Selected-workspace Git watch and bounded snapshot refresh service.
//!
//! All host requests run on one worker. This service never reads the workspace
//! or invokes Git locally.
use crate::{WakeSignal, protocol};
use pigade_storage::{Catalog, WorkspaceStatus};
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};
use uuid::Uuid;

const WATCH_INTERVAL: Duration = Duration::from_secs(1);

#[derive(Clone, Debug, PartialEq, Eq)]
struct Target {
    workspace: Uuid,
    revision: u64,
    project: Uuid,
    cwd: PathBuf,
}

impl Target {
    fn selected(catalog: &Catalog) -> Option<Self> {
        let ws = catalog.workspaces.iter().find(|ws| {
            Some(ws.id) == catalog.selection.workspace_id && ws.status == WorkspaceStatus::Ready
        })?;
        Some(Self {
            workspace: ws.id,
            revision: ws.revision,
            project: ws.project_id,
            cwd: ws.cwd.clone(),
        })
    }
    fn current(&self, catalog: Option<&Catalog>) -> bool {
        catalog.and_then(Self::selected).as_ref() == Some(self)
    }
    fn matches_snapshot(&self, snapshot: &protocol::WorkspaceGitSnapshot) -> bool {
        snapshot.workspace_id == self.workspace
            && snapshot.workspace_revision == self.revision
            && snapshot.project_id == self.project
            && snapshot.cwd == self.cwd
    }
}

#[derive(Debug)]
enum Work {
    Watch { target: Target, request: Uuid },
    Snapshot { target: Target, request: Uuid },
}
impl Work {
    fn request(&self) -> Value {
        match self {
            Self::Watch { target, request } => {
                json!({"command":"workspace_git_watch","workspace":target.workspace,"expected_revision":target.revision,"_ui_request":request})
            }
            Self::Snapshot { target, request } => {
                json!({"command":"workspace_git_snapshot","workspace":target.workspace,"expected_revision":target.revision,"_ui_request":request})
            }
        }
    }
    fn kind(&self) -> Kind {
        match self {
            Self::Watch { .. } => Kind::Watch,
            Self::Snapshot { .. } => Kind::Snapshot,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Watch,
    Snapshot,
}
#[derive(Debug)]
struct Pending {
    target: Target,
    request: Uuid,
    kind: Kind,
    stamp: Option<(Uuid, u64)>,
}
struct ResultMessage {
    request: Uuid,
    kind: Kind,
    result: Result<protocol::ProductReply, protocol::ProductError>,
}

pub(super) struct Watch {
    tx: mpsc::SyncSender<Work>,
    rx: mpsc::Receiver<ResultMessage>,
    target: Option<Target>,
    pending: Option<Pending>,
    disabled: bool,
    last_poll: Instant,
    observed: Option<(Uuid, u64)>,
    dirty: bool,
    snapshot: Option<protocol::WorkspaceGitSnapshot>,
    stale: bool,
    notice: Option<String>,
    changed: bool,
    view: Option<View>,
    snapshot_blocked: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct View {
    pub workspace: Uuid,
    pub revision: u64,
    pub project: Uuid,
    pub cwd: PathBuf,
    pub branch: Option<String>,
    pub detached: bool,
    pub changed: usize,
    pub untracked: usize,
    pub truncated: bool,
    pub stale: bool,
    pub notice: Option<String>,
    pub loading: bool,
    pub unavailable: bool,
    pub stamp: Option<(Uuid, u64)>,
}

impl View {
    /// Plain, bounded toolbar text. GPUI renders it as text, never markup.
    pub(super) fn label(&self, compact: bool) -> String {
        if self.loading {
            return "Git: загрузка…".into();
        }
        if self.unavailable {
            return "Git: недоступно".into();
        }
        let branch = if self.detached {
            "без ветки".into()
        } else {
            self.branch
                .as_deref()
                .map(|value| safe_text(value, 12))
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| "ветка?".into())
        };
        let detail = if self.stale && self.truncated {
            format!(
                "{} изм. · {} новых · устарело · неполно",
                self.changed, self.untracked
            )
        } else if self.stale {
            format!(
                "{} изм. · {} новых · устарело",
                self.changed, self.untracked
            )
        } else if self.truncated {
            format!(
                "≥{} изм. · {} новых · неполно",
                self.changed, self.untracked
            )
        } else if self.changed == 0 && self.untracked == 0 {
            "чисто".into()
        } else {
            format!("{} изм. · {} новых", self.changed, self.untracked)
        };
        let label = format!("Git: {branch} · {detail}");
        if compact {
            if self.changed == 0 && self.untracked == 0 && !self.stale && !self.truncated {
                format!("Git: {branch} · чисто")
            } else {
                format!(
                    "Git: {branch} · {}{} изм. +{} нов.{}{}",
                    if self.truncated { "≥" } else { "" },
                    self.changed,
                    self.untracked,
                    if self.stale {
                        " · устарело"
                    } else {
                        ""
                    },
                    if self.truncated {
                        " · неполно"
                    } else {
                        ""
                    },
                )
            }
        } else {
            label
        }
    }

    pub(super) fn stamp(&self) -> Option<(Uuid, u64)> {
        self.stamp
    }
}

fn safe_text(value: &str, max_chars: usize) -> String {
    value
        .chars()
        .filter(|ch| !ch.is_control())
        .take(max_chars)
        .collect()
}

impl Watch {
    pub(super) fn new(socket: PathBuf, wake: WakeSignal) -> Self {
        let (tx, requests) = mpsc::sync_channel::<Work>(1);
        let (results, rx) = mpsc::sync_channel::<ResultMessage>(1);
        thread::spawn(move || {
            while let Ok(work) = requests.recv() {
                let result = protocol::product_request(&socket, work.request());
                let message = ResultMessage {
                    request: match &work {
                        Work::Watch { request, .. } | Work::Snapshot { request, .. } => *request,
                    },
                    kind: work.kind(),
                    result,
                };
                if results.send(message).is_err() {
                    break;
                }
                wake.notify();
            }
        });
        Self {
            tx,
            rx,
            target: None,
            pending: None,
            disabled: false,
            last_poll: Instant::now(),
            observed: None,
            dirty: true,
            snapshot: None,
            stale: true,
            notice: None,
            changed: false,
            view: None,
            snapshot_blocked: false,
        }
    }

    pub(super) fn service(&mut self, catalog: Option<&Catalog>, active: bool) -> bool {
        self.changed = false;
        let selected = catalog.and_then(Target::selected);
        if selected != self.target {
            self.target = selected;
            self.pending = None;
            self.disabled = false;
            self.observed = None;
            self.dirty = true;
            self.snapshot = None;
            self.stale = true;
            self.notice = None;
            self.snapshot_blocked = false;
            self.last_poll = Instant::now() - WATCH_INTERVAL;
            self.changed = true;
        }
        let Some(target) = self.target.clone() else {
            self.rebuild_view();
            return self.changed;
        };
        while let Ok(message) = self.rx.try_recv() {
            let Some(pending) = self.pending.as_ref() else {
                continue;
            };
            if pending.request != message.request
                || pending.kind != message.kind
                || pending.target != target
                || !target.current(catalog)
            {
                continue;
            }
            let Some(pending) = self.pending.take() else {
                continue;
            };
            match (message.kind, message.result) {
                (Kind::Watch, Ok(reply)) => {
                    if let Some(watch) = reply.workspace_git_watch {
                        if watch.workspace_id != target.workspace
                            || watch.workspace_revision != target.revision
                            || watch.project_id != target.project
                            || watch.cwd != target.cwd
                        {
                            self.disable("Git watch returned another workspace");
                        } else {
                            let stamp = (watch.epoch, watch.sequence);
                            if self.observed.is_some_and(|previous| previous != stamp) {
                                self.dirty = true;
                                self.stale = self.snapshot.is_some();
                                self.snapshot_blocked = false;
                            }
                            self.observed = Some(stamp);
                            if !watch.complete && !watch.rebuilding {
                                self.disable(
                                    watch
                                        .notice
                                        .as_deref()
                                        .unwrap_or("Git watch coverage is incomplete"),
                                );
                            } else if !watch.complete || watch.rebuilding || !watch.settled {
                                self.dirty = true;
                                if self.snapshot.is_some() {
                                    self.stale = true;
                                }
                            } else if self.dirty && active && !self.snapshot_blocked {
                                self.send_snapshot(target.clone());
                            }
                            if let Some(notice) = watch.notice {
                                self.notice = Some(safe_text(&notice, 48));
                            }
                        }
                    } else {
                        self.disable("This session host does not support Git watch");
                    }
                    self.changed = true;
                }
                (Kind::Watch, Err(error)) => {
                    self.disable(&error.message);
                    self.changed = true;
                }
                (Kind::Snapshot, Ok(reply)) => {
                    if pending.stamp == self.observed
                        && let Some(snapshot) = reply
                            .workspace_git_snapshot
                            .filter(|s| target.matches_snapshot(s))
                    {
                        self.snapshot = Some(snapshot);
                        self.dirty = false;
                        self.stale = self.disabled;
                        if !self.disabled {
                            self.notice = None;
                        }
                        self.snapshot_blocked = false;
                    } else {
                        self.notice = Some("Git snapshot did not match the selected workspace or watch state changed".into());
                        self.stale = true;
                        self.dirty = true;
                        self.snapshot_blocked = true;
                    }
                    self.changed = true;
                }
                (Kind::Snapshot, Err(error)) => {
                    self.notice = Some(safe_text(&error.message, 48));
                    self.stale = true;
                    self.dirty = true;
                    self.snapshot_blocked = true;
                    self.changed = true;
                }
            }
        }
        if active
            && self.pending.is_none()
            && !self.disabled
            && Instant::now().duration_since(self.last_poll) >= WATCH_INTERVAL
        {
            self.send_watch(target);
        } else if active
            && self.pending.is_none()
            && self.disabled
            && self.snapshot.is_none()
            && self.dirty
            && !self.snapshot_blocked
        {
            // Still obtain one explicit snapshot when watch is unavailable so old hosts show useful, clearly dated data.
            self.send_snapshot(target);
        }
        self.rebuild_view();
        self.changed
    }

    fn send_watch(&mut self, target: Target) {
        self.last_poll = Instant::now();
        let work = Work::Watch {
            target: target.clone(),
            request: Uuid::new_v4(),
        };
        self.submit(work, None);
    }
    fn send_snapshot(&mut self, target: Target) {
        let stamp = self.observed;
        let work = Work::Snapshot {
            target: target.clone(),
            request: Uuid::new_v4(),
        };
        self.submit(work, stamp);
    }
    fn submit(&mut self, work: Work, stamp: Option<(Uuid, u64)>) {
        let pending = Pending {
            target: match &work {
                Work::Watch { target, .. } | Work::Snapshot { target, .. } => target.clone(),
            },
            request: match &work {
                Work::Watch { request, .. } | Work::Snapshot { request, .. } => *request,
            },
            kind: work.kind(),
            stamp,
        };
        match self.tx.try_send(work) {
            Ok(()) => self.pending = Some(pending),
            Err(mpsc::TrySendError::Disconnected(_)) => {
                self.disable("Git watch worker is unavailable")
            }
            Err(mpsc::TrySendError::Full(_)) => (),
        }
    }
    fn disable(&mut self, notice: &str) {
        self.disabled = true;
        self.notice = Some(safe_text(notice, 48));
        self.stale = true;
    }

    fn rebuild_view(&mut self) {
        let Some(target) = self.target.as_ref() else {
            self.view = None;
            return;
        };
        let snapshot = self.snapshot.as_ref();
        let changed = snapshot.map_or(0, |s| {
            s.entries
                .iter()
                .filter(|entry| {
                    !entry.untracked && (entry.index_status != '.' || entry.worktree_status != '.')
                })
                .count()
        });
        let untracked = snapshot.map_or(0, |s| {
            s.entries.iter().filter(|entry| entry.untracked).count()
        });
        self.view = Some(View {
            workspace: target.workspace,
            revision: target.revision,
            project: target.project,
            cwd: target.cwd.clone(),
            branch: snapshot.and_then(|s| s.branch.clone()),
            detached: snapshot.is_some_and(|s| s.detached),
            changed,
            untracked,
            truncated: snapshot.is_some_and(|s| s.truncated),
            stale: self.stale,
            notice: self.notice.clone(),
            loading: snapshot.is_none() && !self.disabled,
            unavailable: self.disabled || self.snapshot_blocked,
            stamp: self.observed,
        });
    }

    pub(super) fn view(&self) -> Option<&View> {
        self.view.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pigade_storage::{Catalog, Project, Workspace, WorkspaceMode};

    fn catalog(selected: u128) -> Catalog {
        let mut catalog: Catalog = serde_json::from_value(json!({"projects":[],"workspaces":[],"tabs":[],
            "selection":{"project_id":null,"workspace_id":null,"tab_id":null,"window":pigade_storage::WindowState::default(),"revision":1},
            "layout":{"json":"null","revision":1}})).unwrap();
        let project = Uuid::from_u128(1);
        catalog.projects.push(Project {
            id: project,
            canonical_path: "/work".into(),
            git_common_dir: "/work/.git".into(),
            name: "Work".into(),
            roots: vec![],
            revision: 1,
        });
        for id in [10u128, 11] {
            catalog.workspaces.push(Workspace {
                id: Uuid::from_u128(id),
                project_id: project,
                task: format!("task{id}"),
                cwd: format!("/work/task{id}").into(),
                mode: WorkspaceMode::SharedCheckout,
                status: WorkspaceStatus::Ready,
                error: None,
                context_revision: None,
                revision: 7,
            });
        }
        catalog.selection.workspace_id = Some(Uuid::from_u128(selected));
        catalog
    }

    fn harness() -> (Watch, mpsc::Receiver<Work>, mpsc::SyncSender<ResultMessage>) {
        let (tx, requests) = mpsc::sync_channel(1);
        let (results, rx) = mpsc::sync_channel(1);
        let watch = Watch {
            tx,
            rx,
            target: None,
            pending: None,
            disabled: false,
            last_poll: Instant::now(),
            observed: None,
            dirty: true,
            snapshot: None,
            stale: true,
            notice: None,
            changed: false,
            view: None,
            snapshot_blocked: false,
        };
        (watch, requests, results)
    }

    fn reply(
        watch: Option<protocol::WorkspaceGitWatch>,
        snapshot: Option<protocol::WorkspaceGitSnapshot>,
    ) -> protocol::ProductReply {
        protocol::ProductReply {
            ui_request: None,
            providers_local: false,
            providers: None,
            providers_request: false,
            generation_selection: None,
            generation_selected: false,
            generations: None,
            context_saved: false,
            launch_preview: None,
            launch_phase: None,
            sessions: None,
            process_view: None,
            launch_confirmed: false,
            catalog: None,
            selection: None,
            context: None,
            history_view: None,
            history_view_request: false,
            template_run: None,
            template_runs: None,
            panel_directory: None,
            workspace_git_snapshot: snapshot,
            workspace_git_watch: watch,
            workspace_git_diff: None,
            workspace_git_index: None,
            workspace_sessions: None,
            workspace_git_index_batch_plan: None,
            workspace_git_index_batch: None,
            workspace_git_commit_plan: None,
            workspace_git_commit: None,
        }
    }

    fn watch_dto(
        target: &Target,
        epoch: Uuid,
        sequence: u64,
        complete: bool,
        settled: bool,
        rebuilding: bool,
    ) -> protocol::WorkspaceGitWatch {
        protocol::WorkspaceGitWatch {
            workspace_id: target.workspace,
            workspace_revision: target.revision,
            project_id: target.project,
            cwd: target.cwd.clone(),
            epoch,
            sequence,
            complete,
            settled,
            rebuilding,
            notice: (!complete).then(|| "coverage unavailable".into()),
        }
    }

    fn snapshot(target: &Target) -> protocol::WorkspaceGitSnapshot {
        protocol::WorkspaceGitSnapshot {
            workspace_id: target.workspace,
            workspace_revision: target.revision,
            project_id: target.project,
            cwd: target.cwd.clone(),
            branch: Some("topic".into()),
            head: None,
            detached: false,
            entries: vec![],
            truncated: false,
            index_operation: None,
            index_operation_unavailable_reason: None,
            index_batch_schema: None,
            index_batch_operation: None,
            index_batch_unavailable_reason: None,
            commit_schema: None,
            commit_operation: None,
            commit_unavailable_reason: None,
        }
    }

    fn snapshot_with_changes(target: &Target) -> protocol::WorkspaceGitSnapshot {
        let mut snapshot = snapshot(target);
        snapshot.entries = vec![
            protocol::WorkspaceGitEntry {
                path: "tracked.txt".into(),
                original_path: None,
                index_status: 'M',
                worktree_status: '.',
                untracked: false,
                unmerged: false,
            },
            protocol::WorkspaceGitEntry {
                path: "new.txt".into(),
                original_path: None,
                index_status: '?',
                worktree_status: '?',
                untracked: true,
                unmerged: false,
            },
        ];
        snapshot
    }

    fn queue(
        watch: &mut Watch,
        request: Uuid,
        kind: Kind,
        result: Result<protocol::ProductReply, protocol::ProductError>,
        sender: &mpsc::SyncSender<ResultMessage>,
    ) {
        if kind == Kind::Watch {
            watch.last_poll = Instant::now();
        }
        watch.pending = Some(Pending {
            target: watch.target.clone().unwrap(),
            request,
            kind,
            stamp: watch.observed,
        });
        let sent = sender
            .send(ResultMessage {
                request,
                kind,
                result,
            })
            .is_ok();
        assert!(sent);
    }

    fn error(message: &str) -> protocol::ProductError {
        protocol::ProductError {
            ui_request: None,
            providers_request: false,
            message: message.into(),
            rejected_before_launch: false,
        }
    }

    #[test]
    fn watch_request_is_workspace_revision_bound() {
        let target = Target {
            workspace: Uuid::new_v4(),
            revision: 19,
            project: Uuid::new_v4(),
            cwd: PathBuf::from("/work/project"),
        };
        let request = Uuid::new_v4();
        let payload = Work::Watch {
            target: target.clone(),
            request,
        }
        .request();
        assert_eq!(payload["command"], "workspace_git_watch");
        assert_eq!(payload["workspace"], target.workspace.to_string());
        assert_eq!(payload["expected_revision"], 19);
        assert_eq!(payload["_ui_request"], request.to_string());
        assert!(
            Work::Snapshot { target, request }.request()["command"] == "workspace_git_snapshot"
        );
    }

    #[test]
    fn toolbar_text_is_short_safe_and_marks_stale_or_partial_state() {
        let view = View {
            workspace: Uuid::new_v4(),
            revision: 1,
            project: Uuid::new_v4(),
            cwd: PathBuf::from("/work/project"),
            branch: Some("topic\u{1b}[31m".into()),
            detached: false,
            changed: 2,
            untracked: 1,
            truncated: true,
            stale: false,
            notice: None,
            loading: false,
            unavailable: false,
            stamp: Some((Uuid::new_v4(), 2)),
        };
        assert_eq!(
            view.label(true),
            "Git: topic[31m · ≥2 изм. +1 нов. · неполно"
        );
        assert!(view.label(false).contains("2 изм. · 1 новых"));
        let mut stale = view;
        stale.stale = true;
        assert!(stale.label(true).contains("устарело"));
        assert!(stale.label(true).starts_with("Git:"));
    }

    #[test]
    fn loading_and_unsupported_host_are_not_presented_as_clean() {
        let mut view = View {
            workspace: Uuid::new_v4(),
            revision: 1,
            project: Uuid::new_v4(),
            cwd: PathBuf::from("/work/project"),
            branch: None,
            detached: false,
            changed: 0,
            untracked: 0,
            truncated: false,
            stale: true,
            notice: None,
            loading: true,
            unavailable: false,
            stamp: None,
        };
        assert!(view.label(false).contains("загрузка"));
        view.loading = false;
        view.unavailable = true;
        assert!(view.label(false).contains("недоступно"));
    }

    #[test]
    fn scope_switch_ignores_old_reply_without_consuming_new_pending_request() {
        let (mut watch, requests, results) = harness();
        let first = catalog(10);
        watch.service(Some(&first), true);
        let old = match requests.try_recv().unwrap() {
            Work::Watch { request, .. } => request,
            _ => panic!(),
        };
        let second = catalog(11);
        watch.service(Some(&second), true);
        let new = match requests.try_recv().unwrap() {
            Work::Watch { request, .. } => request,
            _ => panic!(),
        };
        let sent = results
            .send(ResultMessage {
                request: old,
                kind: Kind::Watch,
                result: Err(error("old scope reply")),
            })
            .is_ok();
        assert!(sent);
        watch.service(Some(&second), false);
        assert_eq!(watch.pending.as_ref().unwrap().request, new);
        assert!(!watch.disabled);
    }

    #[test]
    fn inactive_window_suspends_new_requests_until_resumed() {
        let (mut watch, requests, _) = harness();
        let catalog = catalog(10);
        watch.service(Some(&catalog), false);
        assert!(watch.pending.is_none());
        assert!(requests.try_recv().is_err());
        watch.service(Some(&catalog), true);
        assert!(matches!(requests.try_recv().unwrap(), Work::Watch { .. }));
    }

    #[test]
    fn watch_waits_for_settled_metadata_and_tracks_epoch_sequence() {
        let (mut watch, requests, results) = harness();
        let catalog = catalog(10);
        watch.service(Some(&catalog), true);
        let target = watch.target.clone().unwrap();
        let first = match requests.try_recv().unwrap() {
            Work::Watch { request, .. } => request,
            _ => panic!(),
        };
        let epoch = Uuid::new_v4();
        queue(
            &mut watch,
            first,
            Kind::Watch,
            Ok(reply(
                Some(watch_dto(&target, epoch, 1, true, false, false)),
                None,
            )),
            &results,
        );
        watch.service(Some(&catalog), true);
        assert!(watch.pending.is_none());
        assert!(requests.try_recv().is_err());
        assert_eq!(watch.observed, Some((epoch, 1)));
        let second = Uuid::new_v4();
        queue(
            &mut watch,
            second,
            Kind::Watch,
            Ok(reply(
                Some(watch_dto(&target, epoch, 1, true, true, false)),
                None,
            )),
            &results,
        );
        watch.service(Some(&catalog), true);
        assert!(matches!(
            requests.try_recv().unwrap(),
            Work::Snapshot { .. }
        ));
    }

    #[test]
    fn snapshot_counts_tracked_changes_and_untracked_files_separately() {
        let (mut watch, requests, results) = harness();
        let catalog = catalog(10);
        watch.service(Some(&catalog), false);
        let target = watch.target.clone().unwrap();
        watch.observed = Some((Uuid::new_v4(), 3));
        let snapshot_request = Uuid::new_v4();
        queue(
            &mut watch,
            snapshot_request,
            Kind::Snapshot,
            Ok(reply(None, Some(snapshot_with_changes(&target)))),
            &results,
        );
        watch.service(Some(&catalog), false);
        let view = watch.view().unwrap();
        assert_eq!(view.changed, 1);
        assert_eq!(view.untracked, 1);
        assert_eq!(view.workspace, target.workspace);
        assert_eq!(view.revision, target.revision);
        assert_eq!(view.project, target.project);
        assert_eq!(view.cwd, target.cwd);
        assert!(view.label(false).contains("1 изм. · 1 новых"));
        assert!(
            requests.try_recv().is_err(),
            "inactive completion sends no follow-up request"
        );
    }

    #[test]
    fn changed_epoch_marks_cached_summary_stale_and_snapshot_errors_wait_for_new_stamp() {
        let (mut watch, requests, results) = harness();
        let catalog = catalog(10);
        watch.service(Some(&catalog), true);
        let target = watch.target.clone().unwrap();
        watch.snapshot = Some(snapshot(&target));
        watch.observed = Some((Uuid::new_v4(), 4));
        let req = match requests.try_recv().unwrap() {
            Work::Watch { request, .. } => request,
            _ => panic!(),
        };
        let epoch = Uuid::new_v4();
        queue(
            &mut watch,
            req,
            Kind::Watch,
            Ok(reply(
                Some(watch_dto(&target, epoch, 1, true, false, false)),
                None,
            )),
            &results,
        );
        watch.service(Some(&catalog), true);
        assert!(watch.stale);
        assert_eq!(watch.view().unwrap().stamp(), Some((epoch, 1)));
        let snap_request = Uuid::new_v4();
        queue(
            &mut watch,
            snap_request,
            Kind::Snapshot,
            Err(error("snapshot unavailable")),
            &results,
        );
        watch.service(Some(&catalog), false);
        assert!(watch.snapshot_blocked);
        assert!(watch.stale);
        assert!(watch.view().unwrap().unavailable);
        watch.service(Some(&catalog), false);
        assert!(watch.pending.is_none());
        let unchanged = Uuid::new_v4();
        queue(
            &mut watch,
            unchanged,
            Kind::Watch,
            Ok(reply(
                Some(watch_dto(&target, epoch, 1, true, true, false)),
                None,
            )),
            &results,
        );
        watch.service(Some(&catalog), true);
        assert!(
            requests.try_recv().is_err(),
            "unchanged stamp must not retry a failed snapshot"
        );
        let next = Uuid::new_v4();
        queue(
            &mut watch,
            next,
            Kind::Watch,
            Ok(reply(
                Some(watch_dto(&target, epoch, 2, true, true, false)),
                None,
            )),
            &results,
        );
        watch.service(Some(&catalog), true);
        assert!(
            !watch.snapshot_blocked,
            "new watch stamp permits another snapshot"
        );
        assert!(matches!(
            requests.try_recv().unwrap(),
            Work::Snapshot { .. }
        ));
    }

    #[test]
    fn legacy_and_partial_coverage_stay_unavailable_and_do_not_claim_clean() {
        let (mut watch, requests, results) = harness();
        let catalog = catalog(10);
        watch.service(Some(&catalog), true);
        let req = match requests.try_recv().unwrap() {
            Work::Watch { request, .. } => request,
            _ => panic!(),
        };
        queue(
            &mut watch,
            req,
            Kind::Watch,
            Ok(reply(None, None)),
            &results,
        );
        watch.service(Some(&catalog), true);
        assert!(watch.disabled);
        assert!(watch.view().unwrap().label(true).contains("недоступно"));
        assert!(matches!(
            requests.try_recv().unwrap(),
            Work::Snapshot { .. }
        ));
        let (mut watch, requests, results) = harness();
        watch.service(Some(&catalog), true);
        let req = match requests.try_recv().unwrap() {
            Work::Watch { request, .. } => request,
            _ => panic!(),
        };
        let target = watch.target.clone().unwrap();
        queue(
            &mut watch,
            req,
            Kind::Watch,
            Ok(reply(
                Some(watch_dto(&target, Uuid::new_v4(), 1, false, false, true)),
                None,
            )),
            &results,
        );
        watch.service(Some(&catalog), true);
        assert!(!watch.disabled, "in-progress coverage rebuild is temporary");
        assert!(
            requests.try_recv().is_err(),
            "do not snapshot before settled state"
        );
        watch.service(Some(&catalog), false);
        assert!(
            requests.try_recv().is_err(),
            "inactive window sends no requests"
        );

        let (mut watch, requests, results) = harness();
        watch.service(Some(&catalog), true);
        let req = match requests.try_recv().unwrap() {
            Work::Watch { request, .. } => request,
            _ => panic!(),
        };
        let target = watch.target.clone().unwrap();
        queue(
            &mut watch,
            req,
            Kind::Watch,
            Ok(reply(
                Some(watch_dto(&target, Uuid::new_v4(), 1, false, false, false)),
                None,
            )),
            &results,
        );
        watch.service(Some(&catalog), true);
        assert!(
            watch.disabled,
            "permanent incomplete coverage disables this scope"
        );
        assert!(watch.view().unwrap().stale);
        assert!(matches!(
            requests.try_recv().unwrap(),
            Work::Snapshot { .. }
        ));
    }
}

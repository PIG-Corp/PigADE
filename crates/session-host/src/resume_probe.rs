//! Internal-only async verification for exact OpenCode resume. Completion jobs
//! never cross the serialized IPC boundary and cannot authorize another target.
use crate::{Command, Host, adapters, ipc_server::Job, panel_launch, prepared};
use anyhow::{Context, Result, ensure};
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    thread,
};
use uuid::Uuid;

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Target {
    session: Uuid,
    plan_id: Uuid,
    start: bool,
    binding: pigade_storage::ConversationBinding,
    head: Uuid,
    executable: PathBuf,
    cwd: PathBuf,
    cwd_identity: DirectoryIdentity,
    cli_version: String,
}

#[derive(Clone, PartialEq, Eq)]
struct DirectoryIdentity {
    canonical: PathBuf,
    device: u64,
    inode: u64,
}

fn directory_identity(path: &std::path::Path) -> Result<DirectoryIdentity> {
    use std::os::unix::fs::MetadataExt;
    let canonical = std::fs::canonicalize(path).context("Рабочая папка недоступна")?;
    let metadata = std::fs::metadata(&canonical)?;
    ensure!(metadata.is_dir(), "Рабочая папка больше не является папкой");
    Ok(DirectoryIdentity {
        canonical,
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

pub(crate) struct Permit {
    target: Target,
    info: adapters::AdapterInfo,
    identity: prepared::BinaryIdentity,
}

pub(crate) struct Completion {
    target: Target,
    result: Result<Permit, String>,
}
struct Task {
    job: Job,
    target: Target,
}

pub(crate) struct Reader {
    sender: mpsc::SyncSender<Task>,
    active: Arc<AtomicUsize>,
}

pub(crate) struct Activity(Arc<AtomicUsize>);
impl Activity {
    fn begin(active: Arc<AtomicUsize>) -> Self {
        active.fetch_add(1, Ordering::AcqRel);
        Self(active)
    }
}
impl Drop for Activity {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

impl Reader {
    pub(crate) fn new(actor: mpsc::SyncSender<Job>) -> Result<Self> {
        let (sender, receiver) = mpsc::sync_channel::<Task>(1);
        let active = Arc::new(AtomicUsize::new(0));
        thread::Builder::new()
            .name("pigade-resume-metadata".into())
            .spawn(move || {
                while let Ok(mut task) = receiver.recv() {
                    let result = verify(task.target.clone()).map_err(|error| format!("{error:#}"));
                    task.job.resume_probe = Some(Completion {
                        target: task.target,
                        result,
                    });
                    if actor.send(task.job).is_err() {
                        break;
                    }
                }
            })?;
        Ok(Self { sender, active })
    }
    pub(crate) fn busy(&self) -> bool {
        self.active.load(Ordering::Acquire) != 0
    }
    pub(crate) fn submit(&self, job: Job, target: Target) {
        let mut job = job;
        job._resume_activity = Some(Activity::begin(self.active.clone()));
        let task = Task { job, target };
        if let Err(error) = self.sender.try_send(task) {
            let task = match error {
                mpsc::TrySendError::Full(task) | mpsc::TrySendError::Disconnected(task) => task,
            };
            // This request never reaches dispatch; no launch can follow it.
            let response = crate::ipc_server::error_response(
                "Проверка диалога занята или недоступна. Повторите явное действие после завершения текущего.",
                Some(crate::CommandRejection::LaunchPreflight),
            );
            let _ = task.job.reply.try_send(response);
        }
    }
}

pub(crate) fn target(host: &mut Host, command: &Command) -> Result<Option<Target>> {
    let (session, plan_id, start) = match command {
        Command::PrepareWorkspaceResume {
            workspace,
            expected_revision,
            session,
            expected_head,
            expected_binding_revision,
        } => {
            crate::workspace_sessions::validate_resume_scope(
                host,
                *workspace,
                *expected_revision,
                *session,
                *expected_head,
                *expected_binding_revision,
            )?;
            let Some(binding) = host.metadata.conversation_binding(*session)? else {
                return Ok(None);
            };
            (*session, binding.plan_id, false)
        }
        Command::PrepareResume { session } => {
            let Some(binding) = host.metadata.conversation_binding(*session)? else {
                return Ok(None);
            };
            (*session, binding.plan_id, false)
        }
        Command::StartPrepared { plan_id } => {
            let Some((payload, phase)) = host.metadata.prepared_launch(*plan_id)? else {
                return Ok(None);
            };
            // The ordinary durable phase/unknown-outcome checks retain priority.
            if phase != "prepared" || host.metadata.get_launch(*plan_id)?.is_some() {
                return Ok(None);
            }
            let plan: prepared::StoredPlan = serde_json::from_str(&payload)?;
            if plan.preview.resume.is_none() {
                return Ok(None);
            }
            (plan.launch.session, *plan_id, true)
        }
        _ => return Ok(None),
    };
    let Some((payload, _)) = host.metadata.prepared_launch(plan_id)? else {
        return Ok(None);
    };
    let plan: prepared::StoredPlan = serde_json::from_str(&payload)?;
    if plan.preview.provider != "opencode" {
        return Ok(None);
    }
    let Some(binding) = host.metadata.conversation_binding(session)? else {
        return Ok(None);
    };
    host.reconcile_exits()?;
    let head = prepared::exited_head(host, session)?;
    if start
        && plan
            .preview
            .resume
            .as_ref()
            .is_some_and(|source| source.binding != binding || source.previous_head != head)
    {
        // Preserve the existing cheap stale-binding/head rejection before any
        // external metadata request, and before a single-use permit is needed.
        return Ok(None);
    }
    let cwd_identity = directory_identity(&plan.launch.cwd)?;
    Ok(Some(Target {
        session,
        plan_id,
        start,
        binding,
        head,
        executable: plan.launch.executable,
        cwd: plan.launch.cwd,
        cwd_identity,
        cli_version: plan.preview.cli_version,
    }))
}

fn verify(target: Target) -> Result<Permit> {
    let identity = prepared::binary_identity(&target.executable)?;
    let info = adapters::inspect("opencode", &target.executable)?;
    ensure!(
        info.cli_version == target.cli_version
            && info.cli_version == "1.19.3"
            && info.capabilities.native_resume,
        "Нативное продолжение не подтверждено для установленной версии/конфигурации OpenCode"
    );
    crate::opencode_resume::verify(
        &target.executable,
        &target.cwd,
        &target.binding.conversation_id,
    )?;
    ensure!(
        target.cwd_identity == directory_identity(&target.cwd)?,
        "Рабочая папка изменилась во время проверки диалога"
    );
    ensure!(
        identity == prepared::binary_identity(&target.executable)?,
        "CLI изменился во время проверки диалога"
    );
    Ok(Permit {
        target,
        info,
        identity,
    })
}

pub(crate) fn accept(host: &mut Host, command: &Command, completion: Completion) -> Result<Permit> {
    let current =
        target(host, command)?.context("Состояние запуска изменилось во время проверки диалога")?;
    ensure!(
        current == completion.target,
        "Привязка, поколение или папка изменились во время проверки диалога"
    );
    let permit = completion.result.map_err(anyhow::Error::msg)?;
    ensure!(
        permit.identity == prepared::binary_identity(&permit.target.executable)?,
        "CLI изменился после проверки диалога"
    );
    Ok(permit)
}

impl Permit {
    pub(crate) fn adapter(
        &self,
        session: Uuid,
        binding: &pigade_storage::ConversationBinding,
        executable: &std::path::Path,
        cwd: &std::path::Path,
    ) -> Result<adapters::AdapterInfo> {
        ensure!(
            self.target.session == session
                && &self.target.binding == binding
                && self.target.executable == executable
                && self.target.cwd == cwd,
            "Проверка диалога относится к другому запуску"
        );
        ensure!(
            self.target.cwd_identity == directory_identity(cwd)?,
            "Рабочая папка изменилась после проверки диалога"
        );
        Ok(self.info.clone())
    }
}

/// Only a still-unattempted plan can receive a known pre-admission rejection.
/// An overlapping/earlier attempt must retain the existing unknown-outcome lock.
pub(crate) fn rejection(host: &Host, command: &Command, error: anyhow::Error) -> anyhow::Error {
    if let Command::StartPrepared { plan_id } = command {
        let unattempted = host
            .metadata
            .prepared_launch(*plan_id)
            .ok()
            .flatten()
            .is_some_and(|(_, phase)| phase == "prepared")
            && matches!(host.metadata.get_launch(*plan_id), Ok(None));
        if !unattempted {
            return error;
        }
    }
    panel_launch::mark_preflight::<()>(Err(error)).unwrap_err()
}

#[cfg(test)]
mod tests {
    use super::directory_identity;
    use std::os::unix::fs::symlink;

    #[test]
    fn changed_directory_and_retargeted_symlink_invalidate_the_frozen_identity() {
        let root = tempfile::tempdir().unwrap();
        let first = root.path().join("first");
        let second = root.path().join("second");
        let alias = root.path().join("alias");
        std::fs::create_dir(&first).unwrap();
        std::fs::create_dir(&second).unwrap();
        symlink(&first, &alias).unwrap();
        let frozen = directory_identity(&alias).unwrap();
        assert!(frozen == directory_identity(&first).unwrap());
        std::fs::remove_file(&alias).unwrap();
        symlink(&second, &alias).unwrap();
        assert!(frozen != directory_identity(&alias).unwrap());
        std::fs::rename(&first, root.path().join("old")).unwrap();
        std::fs::create_dir(&first).unwrap();
        assert!(frozen != directory_identity(&first).unwrap());
    }
}

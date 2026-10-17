//! Reviewed context is frozen before the durable launch protocol begins.
use std::{os::unix::fs::MetadataExt, path::Path};

use anyhow::{Context, Result, ensure};
use pigade_layout::{Command as LayoutCommand, Direction, LayoutHistory, LayoutModel};
use pigade_storage::{Catalog, EffectiveContext, MemoryReview, MemorySelection, WorkspaceStatus};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{Host, Launch, adapters, catalog, panel_launch};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PrepareRequest {
    pub tab: Uuid,
    pub provider: String,
    pub side: Direction,
    pub target: Option<Uuid>,
    #[serde(default)]
    pub role_override: String,
    #[serde(default)]
    pub agent_instructions: String,
    #[serde(default)]
    pub include_memory: bool,
    #[serde(default)]
    pub memory_selection: Option<MemorySelection>,
    pub deliver_context: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LaunchPreview {
    pub plan_id: Uuid,
    pub tab_id: Uuid,
    pub side: Direction,
    pub target: Option<Uuid>,
    pub session: Uuid,
    pub generation: Uuid,
    pub provider: String,
    pub cli_version: String,
    pub adapter_version: u32,
    pub delivery_method: String,
    pub context: EffectiveContext,
    pub capabilities: adapters::Capabilities,
    pub include_memory: bool,
    #[serde(default)]
    pub memory: Option<MemoryReview>,
    pub role_override: String,
    pub agent_instructions: String,
    pub deliver_context: bool,
    pub can_launch: bool,
    pub unavailable_reason: Option<String>,
    pub warnings: Vec<String>,
}

#[derive(Serialize, Deserialize, PartialEq, Eq)]
struct BinaryIdentity {
    device: u64,
    inode: u64,
    length: u64,
    mtime: i64,
    mtime_ns: i64,
    ctime: i64,
    ctime_ns: i64,
}

fn binary_identity(path: &Path) -> Result<BinaryIdentity> {
    let metadata = path.metadata().context("CLI executable is unavailable")?;
    ensure!(metadata.is_file(), "CLI executable is not a regular file");
    Ok(BinaryIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
        length: metadata.len(),
        mtime: metadata.mtime(),
        mtime_ns: metadata.mtime_nsec(),
        ctime: metadata.ctime(),
        ctime_ns: metadata.ctime_nsec(),
    })
}

#[derive(Serialize, Deserialize)]
struct StoredPlan {
    preview: LaunchPreview,
    launch: Launch,
    workspace: Uuid,
    executable_identity: BinaryIdentity,
    placement: LayoutCommand,
    requested_revision: Uuid,
    applied_revision: Option<Uuid>,
}

pub(crate) fn prepare(host: &mut Host, request: PrepareRequest) -> Result<LaunchPreview> {
    panel_launch::mark_preflight((|| {
        let catalog = catalog::read_metadata(&host.metadata)?;
        let tab = catalog
            .tabs
            .iter()
            .find(|tab| tab.id == request.tab)
            .context("target tab is missing")?;
        let workspace = catalog
            .workspaces
            .iter()
            .find(|workspace| workspace.id == tab.workspace_id)
            .context("workspace is missing")?;
        ensure!(
            workspace.status == WorkspaceStatus::Ready && workspace.cwd.is_dir(),
            "workspace is not ready"
        );
        let executable = panel_launch::resolve_provider(&request.provider)?;
        let identity = binary_identity(&executable)?;
        let info = adapters::inspect(&request.provider, &executable)?;
        ensure!(
            identity == binary_identity(&executable)?,
            "CLI changed during inspection; review again"
        );
        let (context, memory) = host.metadata.effective_context_with_memory_selection(
            workspace.project_id,
            workspace.id,
            &workspace.task,
            workspace.revision,
            &request.role_override,
            &request.agent_instructions,
            request.include_memory,
            request.memory_selection.as_ref(),
        )?;
        let session = Uuid::new_v4();
        let generation = Uuid::new_v4();
        let plan_id = Uuid::new_v4();
        let placement = LayoutCommand::Insert {
            tab: tab.id,
            pane: session,
            target: request.target,
            side: request.side,
            split: Uuid::new_v4(),
        };
        let model = LayoutModel::from_history(serde_json::from_str::<LayoutHistory>(
            &catalog.layout.json,
        )?)?;
        model.preview(placement.clone())?;
        let mut unavailable_reason = None;
        let mut arguments = (Vec::new(), Default::default());
        let needs_transport = request.deliver_context && !context.text.is_empty();
        if needs_transport {
            if info.capabilities.startup_context {
                match adapters::context_arguments(&info, &context.text) {
                    Ok(result) => arguments = result,
                    Err(error) => unavailable_reason = Some(error.to_string()),
                }
            } else {
                unavailable_reason = info.unsupported_reason.clone().or_else(|| Some("Для этой версии CLI доставка контекста не подтверждена. Можно явно выбрать запуск без контекста.".into()));
            }
        }
        let mut warnings = if needs_transport && unavailable_reason.is_none() {
            vec!["Контекст передаётся через параметры запуска. Подтверждения получения и применения моделью нет.".into(),
                "Инструкции роли не изменяют права, sandbox или политику CLI.".into()]
        } else {
            Vec::new()
        };
        if needs_transport && request.provider == "codex" && unavailable_reason.is_none() {
            warnings.push("Для этого запуска значение developer_instructions задаётся сохранённым снимком. Пользовательские файлы конфигурации не меняются.".into());
        }
        let preview = LaunchPreview {
            plan_id,
            tab_id: tab.id,
            side: request.side,
            target: request.target,
            session,
            generation,
            provider: request.provider,
            cli_version: info.cli_version,
            adapter_version: info.adapter_version,
            delivery_method: if needs_transport {
                info.delivery_method
            } else {
                "none".into()
            },
            context,
            capabilities: info.capabilities,
            include_memory: request.include_memory,
            memory: Some(memory),
            role_override: request.role_override,
            agent_instructions: request.agent_instructions,
            deliver_context: request.deliver_context,
            can_launch: unavailable_reason.is_none(),
            unavailable_reason,
            warnings,
        };
        let plan = StoredPlan {
            requested_revision: preview.context.id,
            applied_revision: None,
            preview: preview.clone(),
            launch: Launch {
                session,
                generation,
                operation: plan_id,
                executable,
                args: arguments.0,
                env: arguments.1,
                cwd: workspace.cwd.clone(),
                geometry: pigade_terminal::Geometry::new(36, 120)?,
            },
            workspace: workspace.id,
            executable_identity: identity,
            placement,
        };
        host.metadata
            .save_prepared_launch(plan_id, &serde_json::to_string(&plan)?)?;
        Ok(preview)
    })())
}

pub(crate) fn start(host: &mut Host, plan_id: Uuid) -> Result<(Catalog, LaunchPreview)> {
    let (payload, phase) = host
        .metadata
        .prepared_launch(plan_id)?
        .context("reviewed launch plan is missing")?;
    // An attempted operation must be reconciled, even if it has no frame yet.
    // Never turn that uncertainty into a pre-admission rejection or another spawn.
    ensure!(
        phase == "prepared",
        "launch was already attempted ({phase}); inspect sessions instead of retrying"
    );
    ensure!(
        host.metadata.get_launch(plan_id)?.is_none(),
        "operation already has a launch record; reconcile it before doing anything else"
    );
    let plan: StoredPlan = serde_json::from_str(&payload)?;
    let (catalog, _) = panel_launch::mark_preflight((|| -> Result<_> {
        ensure!(
            plan.preview.plan_id == plan_id && plan.launch.operation == plan_id,
            "prepared launch identity mismatch"
        );
        validate_plan(&plan)?;
        ensure!(
            plan.preview.can_launch,
            "reviewed context cannot be delivered; review the launch again"
        );
        ensure!(
            binary_identity(&plan.launch.executable)? == plan.executable_identity,
            "CLI executable changed since review"
        );
        let catalog = catalog::read_metadata(&host.metadata)?;
        let workspace = catalog
            .workspaces
            .iter()
            .find(|workspace| workspace.id == plan.workspace)
            .context("workspace was removed")?;
        ensure!(
            workspace.status == WorkspaceStatus::Ready
                && workspace.cwd == plan.launch.cwd
                && workspace.cwd.is_dir(),
            "workspace changed since review"
        );
        let tab = catalog
            .tabs
            .iter()
            .find(|tab| tab.id == plan.preview.tab_id)
            .context("target tab was removed")?;
        ensure!(
            tab.workspace_id == plan.workspace,
            "target tab moved since review"
        );
        let model = LayoutModel::from_history(serde_json::from_str::<LayoutHistory>(
            &catalog.layout.json,
        )?)?;
        model.preview(plan.placement.clone())?;
        Ok((catalog, ()))
    })())?;
    host.metadata.begin_prepared_launch(plan_id)?;
    let launched = host.launch(plan.launch.clone());
    let views = match launched {
        Ok(views) => views,
        Err(error) => {
            // An error updating this marker cannot authorize a retry either.
            let _ = host.metadata.finish_prepared_launch(plan_id, "unknown");
            return Err(error.context("launch outcome is unknown; inspect sessions"));
        }
    };
    ensure!(
        views
            .iter()
            .any(|view| view.session == plan.launch.session
                && view.generation == plan.launch.generation),
        "launch returned no matching generation; reconcile"
    );
    let delivered = plan.preview.deliver_context && !plan.preview.context.text.is_empty();
    host.metadata.finish_prepared_launch(
        plan_id,
        if delivered {
            "delivered_unconfirmed"
        } else {
            "not_requested"
        },
    )?;
    let catalog = catalog::layout_edit(
        &mut host.metadata,
        catalog.layout.revision,
        crate::LayoutAction::Apply {
            command: plan.placement,
        },
    )
    .context(
        "process started but placement failed; return the existing session through Sessions",
    )?;
    Ok((catalog, plan.preview))
}

pub(crate) fn session_context(
    host: &Host,
    session: Uuid,
    generation: Uuid,
) -> Result<(LaunchPreview, String)> {
    let (payload, phase) = host
        .metadata
        .prepared_launch_for_generation(session, generation)?
        .context("this generation has no PigADE context snapshot")?;
    let plan: StoredPlan = serde_json::from_str(&payload)?;
    validate_plan(&plan)?;
    ensure!(
        plan.preview.session == session && plan.preview.generation == generation,
        "saved context generation mismatch"
    );
    Ok((plan.preview, phase))
}

/// Validate the reviewed snapshot and startup parameters together before any
/// attempt is admitted. A damaged saved plan cannot silently launch other text.
fn validate_plan(plan: &StoredPlan) -> Result<()> {
    plan.preview.context.validate()?;
    let memory_block = plan
        .preview
        .context
        .blocks
        .iter()
        .find(|block| block.source == "workspace_memory");
    if let Some(memory) = &plan.preview.memory {
        ensure!(
            memory.selected.windows(2).all(|pair| pair[0] < pair[1]),
            "saved memory selection indexes are invalid"
        );
        ensure!(
            memory
                .selected
                .iter()
                .all(|index| *index < memory.sections.len()),
            "saved memory selection index is invalid"
        );
        let selected_bytes = memory.selected.iter().try_fold(0usize, |sum, index| {
            sum.checked_add(memory.sections[*index].bytes)
                .context("saved memory byte count overflow")
        })?;
        ensure!(
            selected_bytes == memory.selected_bytes
                && selected_bytes <= pigade_storage::MAX_SELECTED_MEMORY_BYTES,
            "saved memory selection byte count is invalid"
        );
        if plan.preview.include_memory && memory.selected_bytes > 0 {
            let block = memory_block.context("included memory block is missing")?;
            ensure!(
                block.revision == memory.revision && block.text.len() == memory.selected_bytes,
                "saved memory block differs from its review"
            );
        } else {
            ensure!(
                memory_block.is_none(),
                "memory block exists when memory was not included"
            );
        }
    } else {
        // Older frozen previews did not carry section metadata.
        ensure!(
            memory_block.is_none_or(|block| {
                plan.preview.include_memory
                    && block.text.len() <= pigade_storage::MAX_SELECTED_MEMORY_BYTES
            }),
            "legacy saved memory block is invalid"
        );
    }
    ensure!(
        plan.preview.session == plan.launch.session
            && plan.preview.generation == plan.launch.generation
            && plan.preview.plan_id == plan.launch.operation
            && plan.requested_revision == plan.preview.context.id
            && plan.applied_revision.is_none(),
        "saved context identity mismatch"
    );
    ensure!(
        matches!(&plan.placement,
        LayoutCommand::Insert {tab,pane,target,side,..}
            if *tab == plan.preview.tab_id && *pane == plan.preview.session
                && *target == plan.preview.target && *side == plan.preview.side),
        "saved placement differs from reviewed destination"
    );
    let expected = if plan.preview.deliver_context && !plan.preview.context.text.is_empty() {
        let info = adapters::AdapterInfo {
            provider: plan.preview.provider.clone(),
            cli_version: plan.preview.cli_version.clone(),
            adapter_version: plan.preview.adapter_version,
            capabilities: plan.preview.capabilities.clone(),
            delivery_method: plan.preview.delivery_method.clone(),
            unsupported_reason: None,
        };
        if !plan.preview.can_launch {
            return Ok(());
        }
        adapters::context_arguments(&info, &plan.preview.context.text)?
    } else {
        (Vec::new(), Default::default())
    };
    ensure!(
        plan.launch.args == expected.0 && plan.launch.env == expected.1,
        "saved startup parameters differ from the reviewed context"
    );
    Ok(())
}

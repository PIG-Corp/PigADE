//! Frozen, explicitly advanced multi-process launches from saved geometry templates.
use anyhow::{Context, Result, ensure};
use pigade_layout::{
    Command as LayoutCommand, Direction, GeometryTemplate, LayoutModel, Node, TemplateNode,
};
use pigade_storage::{
    Catalog, CatalogBatch, CatalogWrite, LaunchSettings, StoredTemplateRun, TemplateRunPage,
    WorkspaceStatus,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    Host, catalog,
    prepared::{self, LaunchPreview, PrepareRequest, StoredPlan},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TemplateRunRequest {
    pub operation: Uuid,
    pub workspace: Uuid,
    pub name: String,
    pub source: TemplateRunSource,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "snake_case")]
pub enum TemplateRunSource {
    Saved {
        id: Uuid,
        revision: u64,
    },
    Custom {
        geometry: GeometryTemplate,
        launches: Vec<LaunchSettings>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TemplateRunView {
    pub id: Uuid,
    pub tab_id: Uuid,
    pub workspace_id: Uuid,
    pub name: String,
    pub revision: u64,
    pub status: String,
    pub can_advance: bool,
    pub error: Option<String>,
    pub slots: Vec<TemplateRunSlotView>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TemplateRunSlotView {
    pub preview: LaunchPreview,
    pub phase: String,
    pub placed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RunPayload {
    schema: u16,
    geometry: GeometryTemplate,
    plan_ids: Vec<Uuid>,
    progress: usize,
}

fn pane_count(node: &TemplateNode) -> usize {
    match node {
        TemplateNode::Pane => 1,
        TemplateNode::Split { children, .. } => children.iter().map(pane_count).sum(),
    }
}

fn virtual_catalog(catalog: &Catalog, tab_id: Uuid, workspace_id: Uuid) -> Result<Catalog> {
    let mut result = catalog.clone();
    let mut model = catalog::load_layout(catalog)?;
    model.add_tab_metadata(tab_id)?;
    let tab = pigade_storage::Tab {
        id: tab_id,
        workspace_id,
        name: "Template run".into(),
        layout_json: serde_json::to_string(&pigade_layout::TabLayout::default())?,
        active_pane: None,
        revision: 0,
    };
    result.tabs.push(tab);
    result.layout.json = serde_json::to_string(&model.export_history())?;
    Ok(result)
}

fn save_run(
    host: &mut Host,
    mut value: StoredTemplateRun,
    expected: Option<u64>,
) -> Result<StoredTemplateRun> {
    value.revision = expected.unwrap_or(0);
    host.metadata.save_template_run(CatalogWrite {
        value,
        expected_revision: expected,
    })
}

pub(crate) fn prepare(host: &mut Host, request: TemplateRunRequest) -> Result<TemplateRunView> {
    ensure!(
        !request.operation.is_nil(),
        "template run operation ID is nil"
    );
    ensure!(
        !request.name.trim().is_empty() && request.name.len() <= 256,
        "template run name is invalid"
    );
    ensure!(
        host.metadata.template_run(request.operation)?.is_none(),
        "template run operation already exists"
    );
    let catalog = catalog::read_metadata(&host.metadata)?;
    ensure!(
        catalog.template_launch_schema == Some(1),
        "host cannot run templates with launch settings"
    );
    let workspace = catalog
        .workspaces
        .iter()
        .find(|w| w.id == request.workspace)
        .context("workspace is missing")?;
    ensure!(
        workspace.status == WorkspaceStatus::Ready,
        "workspace is not ready"
    );
    let (geometry, launches, template_id, name) = match request.source {
        TemplateRunSource::Saved { id, revision } => {
            let template = catalog
                .layout_templates
                .as_ref()
                .context("template library is unavailable")?
                .iter()
                .find(|t| t.id == id)
                .context("template was removed")?;
            ensure!(
                template.revision == revision,
                "template changed; review it again"
            );
            (
                template.geometry.clone(),
                template
                    .launches
                    .clone()
                    .context("template has no launch settings")?,
                Some(id),
                request.name.clone(),
            )
        }
        TemplateRunSource::Custom { geometry, launches } => {
            (geometry, launches, None, request.name.clone())
        }
    };
    geometry.validate()?;
    let panes = geometry.root.as_ref().map(pane_count).unwrap_or_default();
    ensure!(
        (1..=32).contains(&panes),
        "template must contain 1..=32 panes"
    );
    ensure!(
        launches.len() == panes,
        "launch slot count must exactly match template pane count"
    );
    let tab_id = Uuid::new_v4();
    let mut virtual_state = virtual_catalog(&catalog, tab_id, request.workspace)?;
    let mut plan_ids = Vec::with_capacity(launches.len());
    let mut stored = Vec::with_capacity(launches.len());
    let mut prior_session = None;
    for settings in launches {
        ensure!(
            matches!(
                settings.provider.as_str(),
                "shell" | "claude" | "codex" | "opencode" | "custom"
            ),
            "unsupported provider"
        );
        let mut plan = crate::panel_launch::mark_preflight(prepared::build_plan(
            host,
            &virtual_state,
            PrepareRequest {
                tab: tab_id,
                provider: settings.provider,
                side: Direction::Right,
                target: prior_session,
                role_override: settings.role,
                agent_instructions: settings.instructions,
                include_memory: false,
                memory_selection: None,
                deliver_context: settings.deliver_context,
                engine: settings.engine,
                custom_command: settings.custom_command,
            },
        ))?;
        ensure!(
            plan.preview.can_launch,
            "{}",
            plan.preview
                .unavailable_reason
                .as_deref()
                .unwrap_or("launch preflight failed")
        );
        plan.template_run = Some(request.operation);
        let command = plan
            .placement
            .clone()
            .context("template placement missing")?;
        let mut model = catalog::load_layout(&virtual_state)?;
        model.apply(command)?;
        virtual_state.layout.json = serde_json::to_string(&model.export_history())?;
        let projection = model
            .state()
            .tabs
            .get(&tab_id)
            .context("virtual tab missing")?;
        virtual_state
            .tabs
            .last_mut()
            .context("virtual tab missing")?
            .layout_json = serde_json::to_string(projection)?;
        virtual_state.tabs.last_mut().unwrap().active_pane = projection.focused;
        prior_session = Some(plan.launch.session);
        plan_ids.push(plan.preview.plan_id);
        stored.push(plan);
    }
    let mut payload = RunPayload {
        schema: 1,
        geometry,
        plan_ids,
        progress: 0,
    };
    let payload_json = serde_json::to_string(&payload)?;
    let plans_json = stored
        .iter()
        .map(serde_json::to_string)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    ensure!(
        payload_json.len() + plans_json.iter().map(String::len).sum::<usize>() <= 2 * 1024 * 1024,
        "template run plan exceeds 2 MiB"
    );
    for (plan, id) in stored.iter().zip(&payload.plan_ids) {
        host.metadata
            .save_prepared_launch(*id, &serde_json::to_string(plan)?)?;
    }
    payload.progress = 0;
    let run = save_run(
        host,
        StoredTemplateRun {
            id: request.operation,
            workspace_id: request.workspace,
            tab_id,
            template_id,
            name,
            slots: stored.len() as u16,
            status: "prepared".into(),
            payload: serde_json::to_string(&payload)?,
            revision: 0,
        },
        None,
    )?;
    view(host, run)
}

fn decode(host: &Host, run: &StoredTemplateRun) -> Result<RunPayload> {
    let payload: RunPayload = serde_json::from_str(&run.payload)?;
    ensure!(
        payload.schema == 1 && payload.plan_ids.len() == run.slots as usize,
        "unsupported or invalid template run payload"
    );
    payload.geometry.validate()?;
    ensure!(
        payload.geometry.pane_count() == payload.plan_ids.len()
            && payload.progress <= payload.plan_ids.len(),
        "template run payload is inconsistent"
    );
    let unique: std::collections::BTreeSet<_> = payload.plan_ids.iter().copied().collect();
    ensure!(
        unique.len() == payload.plan_ids.len(),
        "template run repeats a frozen plan ID"
    );
    let mut sessions = std::collections::BTreeSet::new();
    let mut generations = std::collections::BTreeSet::new();
    for (slot, plan_id) in payload.plan_ids.iter().enumerate() {
        let (stored, _) = plan(host, *plan_id)?;
        ensure!(
            stored.template_run == Some(run.id)
                && stored.workspace == run.workspace_id
                && stored.preview.plan_id == *plan_id
                && stored.launch.operation == *plan_id
                && stored.preview.tab_id == run.tab_id
                && stored.launch.session == stored.preview.session
                && stored.launch.generation == stored.preview.generation
                && stored.preview.session != Uuid::nil()
                && stored.preview.generation != Uuid::nil()
                && sessions.insert(stored.launch.session)
                && generations.insert(stored.launch.generation),
            "frozen template slot {slot} is not bound to this run"
        );
        let expected_target = slot
            .checked_sub(1)
            .map(|previous| {
                plan(host, payload.plan_ids[previous]).map(|(plan, _)| plan.launch.session)
            })
            .transpose()?;
        ensure!(
            matches!(stored.placement, Some(LayoutCommand::Insert { tab, pane, target, side: Direction::Right, .. }) if tab == run.tab_id && pane == stored.launch.session && target == expected_target),
            "frozen template slot {slot} has an invalid placement binding"
        );
    }
    Ok(payload)
}

fn plan(host: &Host, id: Uuid) -> Result<(StoredPlan, String)> {
    let (json, phase) = host
        .metadata
        .prepared_launch(id)?
        .context("frozen launch plan is missing")?;
    let plan = serde_json::from_str(&json)?;
    prepared::validate_stored_plan(&plan)?;
    Ok((plan, phase))
}

fn expected_prefix(
    host: &Host,
    run: &StoredTemplateRun,
    payload: &RunPayload,
    count: usize,
) -> Result<Option<Node>> {
    let mut model = LayoutModel::default();
    model.add_tab_metadata(run.tab_id)?;
    for id in payload.plan_ids.iter().take(count) {
        let (plan, _) = plan(host, *id)?;
        model.apply(plan.placement.context("template slot has no placement")?)?;
    }
    Ok(model.state().tabs[&run.tab_id].root.clone())
}

fn check_prefix(
    host: &Host,
    run: &StoredTemplateRun,
    payload: &RunPayload,
    count: usize,
) -> Result<()> {
    let catalog = catalog::read_metadata(&host.metadata)?;
    let tab = catalog
        .tabs
        .iter()
        .find(|tab| tab.id == run.tab_id)
        .context("template run tab is missing")?;
    ensure!(
        tab.workspace_id == run.workspace_id,
        "template run tab changed workspace"
    );
    let model = catalog::load_layout(&catalog)?;
    let actual = &model.state().tabs[&run.tab_id];
    ensure!(!actual.focus_mode, "template run tab focus mode changed");
    ensure!(
        actual.root == expected_prefix(host, run, payload, count)?,
        "template run tab geometry changed; explicit continuation is blocked"
    );
    Ok(())
}

fn view(host: &Host, run: StoredTemplateRun) -> Result<TemplateRunView> {
    let payload = decode(host, &run)?;
    let catalog = catalog::read_metadata(&host.metadata)?;
    let model = catalog::load_layout(&catalog)?;
    let current = model.state().tabs.get(&run.tab_id);
    let mut slots = Vec::new();
    for id in &payload.plan_ids {
        let (p, phase) = plan(host, *id)?;
        let placed = current.is_some_and(|tab| {
            tab.root
                .as_ref()
                .is_some_and(|root| root.panes().contains(&p.launch.session))
        });
        slots.push(TemplateRunSlotView {
            preview: p.preview,
            phase,
            placed,
        });
    }
    let mut can_advance = run.status == "prepared" || run.status == "running";
    let mut blocked_reason = None;
    let mut next_phase = None;
    if can_advance {
        if let Some(next_id) = payload.plan_ids.get(payload.progress) {
            let (_, phase) = plan(host, *next_id)?;
            next_phase = Some(phase.clone());
            if phase != "prepared"
                && !matches!(phase.as_str(), "not_requested" | "delivered_unconfirmed")
            {
                can_advance = false;
                blocked_reason = Some(format!(
                    "Launch outcome is {phase}; automatic retry is prohibited."
                ));
            }
            if phase == "prepared" && host.metadata.get_launch(*next_id)?.is_some() {
                can_advance = false;
            }
            if phase != "prepared"
                && !host.metadata.get_launch(*next_id)?.is_some_and(|record| {
                    matches!(
                        record.state,
                        pigade_storage::LaunchState::Running | pigade_storage::LaunchState::Exited
                    )
                })
            {
                can_advance = false;
                blocked_reason = Some(
                    "Launch record is missing or unknown; automatic retry is prohibited.".into(),
                );
            }
        } else {
            can_advance = false;
            blocked_reason = Some("Every slot has already been advanced.".into());
        }
        for id in &payload.plan_ids {
            let preview = plan(host, *id)?.0.preview;
            if !preview.can_launch {
                can_advance = false;
                blocked_reason.get_or_insert_with(|| {
                    preview
                        .unavailable_reason
                        .unwrap_or_else(|| "A frozen launch slot is unavailable.".into())
                });
            }
        }
        if run.status == "running" {
            let model_ok = if catalog.tabs.iter().any(|tab| tab.id == run.tab_id) {
                if let Some(layout) = model.state().tabs.get(&run.tab_id) {
                    let expected = expected_prefix(host, &run, &payload, payload.progress)?;
                    let recovered = if next_phase.as_deref() != Some("prepared")
                        && payload.progress < payload.plan_ids.len()
                    {
                        expected_prefix(host, &run, &payload, payload.progress + 1)?
                    } else {
                        expected.clone()
                    };
                    !layout.focus_mode && (layout.root == expected || layout.root == recovered)
                } else {
                    false
                }
            } else {
                false
            };
            if !model_ok {
                can_advance = false;
                blocked_reason =
                    Some("Run tab geometry or focus mode changed; continuation is blocked.".into());
            }
        }
    }
    Ok(TemplateRunView {
        id: run.id,
        tab_id: run.tab_id,
        workspace_id: run.workspace_id,
        name: run.name,
        revision: run.revision,
        status: run.status,
        can_advance,
        error: blocked_reason,
        slots,
    })
}

pub(crate) fn get(host: &mut Host, id: Uuid) -> Result<TemplateRunView> {
    host.reconcile_exits()?;
    let run = host
        .metadata
        .template_run(id)?
        .context("template run not found")?;
    view(host, run)
}

pub(crate) fn list(host: &mut Host, before: Option<u64>) -> Result<TemplateRunPage> {
    host.reconcile_exits()?;
    host.metadata.template_runs(before)
}

pub(crate) fn cancel(host: &mut Host, id: Uuid, expected: u64) -> Result<TemplateRunView> {
    let mut run = host
        .metadata
        .template_run(id)?
        .context("template run not found")?;
    ensure!(run.revision == expected, "template run revision conflict");
    ensure!(
        run.status == "prepared" || run.status == "running",
        "template run is already terminal"
    );
    run.status = "cancelled".into();
    let run = save_run(host, run, Some(expected))?;
    view(host, run)
}

pub(crate) fn advance(host: &mut Host, id: Uuid, expected: u64) -> Result<TemplateRunView> {
    let mut run = host
        .metadata
        .template_run(id)?
        .context("template run not found")?;
    ensure!(run.revision == expected, "template run revision conflict");
    ensure!(
        run.status == "prepared" || run.status == "running",
        "template run is not advanceable"
    );
    let mut payload = decode(host, &run)?;
    for plan_id in &payload.plan_ids {
        let (frozen, _) = plan(host, *plan_id)?;
        ensure!(
            frozen.preview.can_launch,
            "a frozen template slot is not launchable"
        );
    }
    ensure!(
        payload.progress < payload.plan_ids.len(),
        "all template slots are already launched"
    );
    let (p, phase) = plan(host, payload.plan_ids[payload.progress])?;
    if phase != "prepared" {
        ensure!(
            matches!(phase.as_str(), "not_requested" | "delivered_unconfirmed"),
            "launch slot outcome is {phase}; automatic retry is prohibited"
        );
        let record = host
            .metadata
            .get_launch(p.launch.operation)?
            .context("completed launch phase has no durable launch record")?;
        ensure!(
            matches!(
                record.state,
                pigade_storage::LaunchState::Running | pigade_storage::LaunchState::Exited
            ),
            "launch record is unknown; automatic retry is prohibited"
        );
        let catalog = catalog::read_metadata(&host.metadata)?;
        let model = catalog::load_layout(&catalog)?;
        let actual = model
            .state()
            .tabs
            .get(&run.tab_id)
            .context("template run tab is missing")?;
        ensure!(!actual.focus_mode, "template run tab focus mode changed");
        let already_inserted =
            actual.root == expected_prefix(host, &run, &payload, payload.progress + 1)?;
        if !already_inserted {
            check_prefix(host, &run, &payload, payload.progress)?;
        }
        let placed = actual
            .root
            .as_ref()
            .is_some_and(|root| root.panes().contains(&p.launch.session));
        if !placed {
            let revision = catalog.layout.revision;
            catalog::layout_edit(
                &mut host.metadata,
                revision,
                crate::LayoutAction::Apply {
                    command: p.placement.clone().context("frozen placement missing")?,
                },
            )?;
        }
        payload.progress += 1;
        run.payload = serde_json::to_string(&payload)?;
        if payload.progress == payload.plan_ids.len() {
            run.status = "complete".into();
            let revision = catalog::read_metadata(&host.metadata)?.layout.revision;
            let expected_run_revision = run.revision;
            let saved = catalog::layout_edit_with_run(
                &mut host.metadata,
                revision,
                LayoutCommand::ApplyTemplate {
                    tab: run.tab_id,
                    template: payload.geometry,
                },
                run,
                expected_run_revision,
            )?;
            return view(host, saved);
        }
        let saved = save_run(host, run, Some(expected))?;
        return view(host, saved);
    }
    ensure!(
        host.metadata.get_launch(p.launch.operation)?.is_none(),
        "launch slot has a launch record; reconcile before continuing"
    );
    let current = catalog::read_metadata(&host.metadata)?;
    let tab = current.tabs.iter().find(|tab| tab.id == run.tab_id);
    if tab.is_none() {
        ensure!(
            payload.progress == 0 && run.status == "prepared",
            "run tab disappeared after launch began"
        );
        let mut model = catalog::load_layout(&current)?;
        model.add_tab_metadata(run.tab_id)?;
        let project_id = current
            .workspaces
            .iter()
            .find(|w| w.id == run.workspace_id)
            .context("run workspace is missing")?
            .project_id;
        let tab = pigade_storage::Tab {
            id: run.tab_id,
            workspace_id: run.workspace_id,
            name: run.name.clone(),
            layout_json: serde_json::to_string(&pigade_layout::TabLayout::default())?,
            active_pane: None,
            revision: 0,
        };
        let mut selection = current.selection.clone();
        selection.project_id = Some(project_id);
        selection.workspace_id = Some(run.workspace_id);
        selection.tab_id = Some(run.tab_id);
        let batch = CatalogBatch {
            tabs: vec![CatalogWrite {
                value: tab,
                expected_revision: None,
            }],
            selection: Some(CatalogWrite {
                value: selection,
                expected_revision: Some(current.selection.revision),
            }),
            layout: Some(catalog::history_write(&model, &current.layout)?),
            ..CatalogBatch::default()
        };
        run.status = "running".into();
        run.payload = serde_json::to_string(&payload)?;
        host.metadata.apply_catalog_with_template_run(
            batch,
            CatalogWrite {
                value: run.clone(),
                expected_revision: Some(expected),
            },
        )?;
        run.revision += 1;
    } else {
        ensure!(
            tab.is_some_and(|existing| existing.workspace_id == run.workspace_id),
            "run tab changed workspace"
        );
        check_prefix(host, &run, &payload, payload.progress)?;
        run.status = "running".into();
        run.revision = expected;
        run = save_run(host, run, Some(expected))?;
    }
    let (_catalog, _preview) = prepared::start_for_template(host, run.id, p.preview.plan_id)?;
    payload.progress += 1;
    run.payload = serde_json::to_string(&payload)?;
    if payload.progress == payload.plan_ids.len() {
        let revision = catalog::read_metadata(&host.metadata)?.layout.revision;
        run.status = "complete".into();
        let expected_run_revision = run.revision;
        let saved = catalog::layout_edit_with_run(
            &mut host.metadata,
            revision,
            LayoutCommand::ApplyTemplate {
                tab: run.tab_id,
                template: payload.geometry,
            },
            run,
            expected_run_revision,
        )?;
        return view(host, saved);
    }
    let expected_run_revision = run.revision;
    let run = save_run(host, run, Some(expected_run_revision))?;
    view(host, run)
}

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
    #[serde(default)]
    pub engine: pigade_storage::EngineOptions,
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
    #[serde(default)]
    pub executable: Option<std::path::PathBuf>,
    #[serde(default)]
    pub cwd: Option<std::path::PathBuf>,
    #[serde(default)]
    pub conversation: Option<pigade_storage::ConversationBinding>,
    #[serde(default)]
    pub resume: Option<ResumeSource>,
    #[serde(default)]
    pub profile: Option<ProfileSource>,
    #[serde(default)]
    pub engine: pigade_storage::EngineOptions,
}

/// Immutable provenance; later library edits do not alter this launch.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProfileSource {
    pub id: Uuid,
    pub name: String,
    pub revision: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResumeSource {
    pub binding: pigade_storage::ConversationBinding,
    pub previous_head: Uuid,
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct BinaryIdentity {
    device: u64,
    inode: u64,
    length: u64,
    mtime: i64,
    mtime_ns: i64,
    ctime: i64,
    ctime_ns: i64,
}

pub(crate) fn binary_identity(path: &Path) -> Result<BinaryIdentity> {
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

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct StoredPlan {
    pub(crate) preview: LaunchPreview,
    pub(crate) launch: Launch,
    pub(crate) workspace: Uuid,
    pub(crate) executable_identity: BinaryIdentity,
    pub(crate) placement: Option<LayoutCommand>,
    pub(crate) requested_revision: Uuid,
    pub(crate) applied_revision: Option<Uuid>,
    /// Template runs own their frozen plans until that explicit run advances.
    #[serde(default)]
    pub(crate) template_run: Option<Uuid>,
}

pub(crate) fn prepare(host: &mut Host, request: PrepareRequest) -> Result<LaunchPreview> {
    panel_launch::mark_preflight((|| {
        let catalog = catalog::read_metadata(&host.metadata)?;
        let plan = build_plan(host, &catalog, request)?;
        let preview = plan.preview.clone();
        host.metadata
            .save_prepared_launch(preview.plan_id, &serde_json::to_string(&plan)?)?;
        Ok(preview)
    })())
}

pub(crate) fn prepare_profile(
    host: &mut Host,
    tab: Uuid,
    profile: Uuid,
    expected_revision: u64,
    side: Direction,
    target: Option<Uuid>,
) -> Result<LaunchPreview> {
    panel_launch::mark_preflight((|| {
        let catalog = catalog::read_metadata(&host.metadata)?;
        let saved = catalog
            .agent_profiles
            .as_ref()
            .and_then(|profiles| profiles.iter().find(|value| value.id == profile))
            .context("Профиль удалён или недоступен; обновите библиотеку")?;
        ensure!(
            saved.revision == expected_revision,
            "Профиль изменён; обновите библиотеку перед запуском"
        );
        pigade_storage::validate_agent_profile_settings(&saved.settings)?;
        let request = PrepareRequest {
            tab,
            provider: saved.settings.provider.clone(),
            side,
            target,
            role_override: saved.settings.role.clone(),
            agent_instructions: saved.settings.instructions.clone(),
            include_memory: false,
            memory_selection: None,
            deliver_context: saved.settings.deliver_context,
            engine: saved.settings.engine.clone(),
        };
        let mut plan = build_plan(host, &catalog, request)?;
        plan.preview.profile = Some(ProfileSource {
            id: saved.id,
            name: saved.name.clone(),
            revision: saved.revision,
        });
        let preview = plan.preview.clone();
        host.metadata
            .save_prepared_launch(preview.plan_id, &serde_json::to_string(&plan)?)?;
        Ok(preview)
    })())
}

/// Freeze one launch using the supplied catalog snapshot. Template runs pass a
/// virtual catalog whose tab and placement history contain the fixed prior
/// insertions; this function still performs the same CLI/context preflight.
pub(crate) fn build_plan(
    host: &mut Host,
    catalog: &Catalog,
    request: PrepareRequest,
) -> Result<StoredPlan> {
    request.engine.validate()?;
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
    let model =
        LayoutModel::from_history(serde_json::from_str::<LayoutHistory>(&catalog.layout.json)?)?;
    model.preview(placement.clone())?;
    let engine_args = adapters::engine_arguments(&info, &request.engine);
    let mut unavailable_reason = engine_args.as_ref().err().map(ToString::to_string);
    let mut arguments = (engine_args.unwrap_or_default(), Default::default());
    let needs_transport = request.deliver_context && !context.text.is_empty();
    if needs_transport {
        if info.capabilities.startup_context {
            match adapters::context_arguments(&info, &context.text) {
                Ok(result) => {
                    arguments.0.extend(result.0);
                    arguments.1 = result.1;
                }
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
    if !request.engine.is_empty() {
        warnings.push("Выбранные модель и reasoning передаются CLI как параметры запуска. Доступ аккаунта и совместимость проверяет CLI; PigADE не подменяет модель при ошибке.".into());
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
        executable: Some(executable.clone()),
        cwd: Some(workspace.cwd.clone()),
        conversation: None,
        resume: None,
        profile: None,
        engine: request.engine,
    };
    Ok(StoredPlan {
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
        placement: Some(placement),
        template_run: None,
    })
}

pub(crate) fn validate_stored_plan(plan: &StoredPlan) -> Result<()> {
    validate_plan(plan)
}

/// Raw launch commands must not impersonate a frozen template-owned generation.
pub(crate) fn guard_direct_launch(
    host: &Host,
    operation: Uuid,
    session: Uuid,
    generation: Uuid,
) -> Result<()> {
    if let Some((payload, _)) = host.metadata.prepared_launch(operation)? {
        let plan: StoredPlan = serde_json::from_str(&payload)?;
        ensure!(
            plan.template_run.is_none(),
            "launch identity is reserved by a template run"
        );
    }
    if let Some((payload, _)) = host
        .metadata
        .prepared_launch_for_generation(session, generation)?
    {
        let plan: StoredPlan = serde_json::from_str(&payload)?;
        ensure!(
            plan.template_run.is_none(),
            "session generation is reserved by a template run"
        );
    }
    Ok(())
}

pub(crate) fn start(host: &mut Host, plan_id: Uuid) -> Result<(Catalog, LaunchPreview)> {
    start_owned(host, plan_id, None)
}

pub(crate) fn start_for_template(
    host: &mut Host,
    run_id: Uuid,
    plan_id: Uuid,
) -> Result<(Catalog, LaunchPreview)> {
    start_owned(host, plan_id, Some(run_id))
}

fn start_owned(
    host: &mut Host,
    plan_id: Uuid,
    expected_template_run: Option<Uuid>,
) -> Result<(Catalog, LaunchPreview)> {
    host.reconcile_exits()?;
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
    ensure!(
        plan.template_run == expected_template_run,
        "prepared launch is owned by a template run or has an unexpected owner"
    );
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
        if let Some(resume) = &plan.preview.resume {
            validate_resume_source(host, &plan, resume)?;
            ensure!(
                plan.launch.cwd.is_dir(),
                "Исходная рабочая папка недоступна; resume не будет запущен в другой папке"
            );
            let model = LayoutModel::from_history(serde_json::from_str::<LayoutHistory>(
                &catalog.layout.json,
            )?)?;
            ensure!(
                model.state().pane_tab(plan.launch.session) == Some(plan.preview.tab_id),
                "Панель перемещена; заново откройте просмотр продолжения"
            );
        } else {
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
            model.preview(
                plan.placement
                    .clone()
                    .context("startup placement missing")?,
            )?;
        }
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
    let catalog = if let Some(placement) = plan.placement {
        catalog::layout_edit(
            &mut host.metadata,
            catalog.layout.revision,
            crate::LayoutAction::Apply { command: placement },
        )
        .context(
            "process started but placement failed; return the existing session through Sessions",
        )?
    } else {
        // Confirming resume explicitly follows the new generation. This does
        // not touch the layout tree or its undo history.
        crate::generations::select(host, plan.launch.session, None).context(
            "resume started but generation selection was not saved; inspect Generations",
        )?;
        catalog::read_metadata(&host.metadata)?
    };
    Ok((catalog, plan.preview))
}

pub(crate) fn exited_head(host: &Host, session: Uuid) -> Result<Uuid> {
    let records = host.metadata.generation_records(session, None, 1)?;
    let head = records
        .first()
        .context("У сессии нет подтверждённого запуска")?;
    ensure!(
        head.state == pigade_storage::LaunchState::Exited,
        "Предыдущий процесс работает или исход запуска неизвестен; продолжение заблокировано"
    );
    Ok(head.generation)
}

fn validate_resume_source(host: &Host, plan: &StoredPlan, resume: &ResumeSource) -> Result<()> {
    ensure!(
        host.metadata
            .conversation_binding(plan.launch.session)?
            .as_ref()
            == Some(&resume.binding),
        "Привязка диалога изменилась после просмотра; проверьте продолжение заново"
    );
    ensure!(
        exited_head(host, plan.launch.session)? == resume.previous_head,
        "После просмотра появилось другое поколение; проверьте продолжение заново"
    );
    let (payload, phase) = host
        .metadata
        .prepared_launch(resume.binding.plan_id)?
        .context("Исходный снимок контекста недоступен")?;
    let source: StoredPlan = serde_json::from_str(&payload)?;
    validate_plan(&source)?;
    ensure!(
        phase != "prepared" && host.metadata.get_launch(resume.binding.plan_id)?.is_some(),
        "Исходный запуск не подтверждён"
    );
    ensure!(
        source.launch.session == plan.launch.session
            && source.launch.generation == resume.binding.source_generation
            && source.preview.context == plan.preview.context
            && source.launch.executable == plan.launch.executable
            && source.launch.cwd == plan.launch.cwd
            && source.preview.provider == plan.preview.provider
            && source.preview.cli_version == plan.preview.cli_version
            && source.preview.deliver_context == plan.preview.deliver_context,
        "Продолжение отличается от исходной беседы, папки или ревизии контекста"
    );
    ensure!(
        plan.preview.capabilities.native_resume,
        "Нативное продолжение не подтверждено для этой версии CLI"
    );
    if plan.preview.provider == "opencode" {
        host.resume_verified
            .as_ref()
            .context("Требуется свежая проверка папки диалога OpenCode")?
            .adapter(
                plan.launch.session,
                &resume.binding,
                &plan.launch.executable,
                &plan.launch.cwd,
            )?;
    }
    Ok(())
}

pub(crate) fn prepare_resume(host: &mut Host, session: Uuid) -> Result<LaunchPreview> {
    host.reconcile_exits()?;
    panel_launch::mark_preflight((|| {
        let head = exited_head(host, session)?;
        let binding = host
            .metadata
            .conversation_binding(session)?
            .context("Сначала сохраните точный ID в «Диалог CLI»")?;
        let (payload, _) = host
            .metadata
            .prepared_launch(binding.plan_id)?
            .context("Исходный снимок контекста недоступен")?;
        let mut plan: StoredPlan = serde_json::from_str(&payload)?;
        validate_plan(&plan)?;
        ensure!(
            plan.launch.session == session
                && plan.launch.generation == binding.source_generation
                && plan.preview.context.id == binding.context_revision
                && plan.preview.provider == binding.provider,
            "Привязка не соответствует исходной сессии"
        );
        ensure!(
            plan.launch.cwd.is_dir(),
            "Исходная рабочая папка недоступна"
        );
        let identity = binary_identity(&plan.launch.executable)?;
        let info = if plan.preview.provider == "opencode" {
            host.resume_verified
                .as_ref()
                .context("Сначала требуется проверка папки диалога OpenCode")?
                .adapter(session, &binding, &plan.launch.executable, &plan.launch.cwd)?
        } else {
            adapters::inspect(&plan.preview.provider, &plan.launch.executable)?
        };
        ensure!(
            identity == binary_identity(&plan.launch.executable)?,
            "CLI изменился во время проверки"
        );
        ensure!(
            info.cli_version == plan.preview.cli_version,
            "Версия CLI изменилась; продолжение с этим адаптером не подтверждено"
        );
        let catalog = catalog::read_metadata(&host.metadata)?;
        let model = LayoutModel::from_history(serde_json::from_str::<LayoutHistory>(
            &catalog.layout.json,
        )?)?;
        let tab_id = model
            .state()
            .pane_tab(session)
            .context("Сначала разместите сессию во вкладке")?;
        plan.workspace = catalog
            .tabs
            .iter()
            .find(|tab| tab.id == tab_id)
            .context("Вкладка недоступна")?
            .workspace_id;
        let args = adapters::resume_arguments(
            &info,
            &binding.conversation_id,
            &plan.launch.cwd,
            (plan.preview.deliver_context && !plan.preview.context.text.is_empty())
                .then_some(plan.preview.context.text.as_str()),
        )?;
        plan.preview.plan_id = Uuid::new_v4();
        plan.preview.generation = Uuid::new_v4();
        plan.preview.tab_id = tab_id;
        plan.preview.target = Some(session);
        plan.preview.capabilities = info.capabilities;
        plan.preview.adapter_version = info.adapter_version;
        plan.preview.can_launch = plan.preview.capabilities.native_resume;
        plan.preview.unavailable_reason = (!plan.preview.can_launch)
            .then(|| "Нативное продолжение ещё не подтверждено для этой версии CLI".into());
        plan.preview.delivery_method = if plan.preview.deliver_context {
            "native resume по точному ID; сохранённый контекст"
        } else {
            "native resume по точному ID; контекст PigADE не передаётся"
        }
        .into();
        plan.preview.conversation = Some(binding.clone());
        // Native conversation settings may have been changed inside the CLI;
        // an original launch request is not evidence of their current value.
        plan.preview.engine = Default::default();
        plan.preview.resume = Some(ResumeSource {
            binding,
            previous_head: head,
        });
        plan.preview.warnings = vec![
            "Будет запущен новый процесс в той же панели. Предыдущее поколение останется доступно в истории.".into(),
            "Последнее задание не отправляется повторно. Продолжение подтверждается самим CLI; новая беседа при ошибке не создаётся.".into(),
            "Модель и reasoning исходного запуска не применяются повторно: настройки продолженного диалога определяет CLI.".into(),
            if plan.preview.deliver_context {
                "Используется сохранённая ревизия контекста; текущие правки проекта и воркспейса не подставляются."
            } else {
                "Контекст PigADE не передаётся. Историю и настройки выбранного диалога восстанавливает CLI."
            }.into(),
        ];
        plan.launch.generation = plan.preview.generation;
        plan.launch.operation = plan.preview.plan_id;
        plan.launch.args = args.0;
        plan.launch.env = args.1;
        plan.executable_identity = identity;
        plan.placement = None;
        // Explicit PrepareResume is a fresh user action over the validated
        // source generation; it must not inherit a cancelled template owner.
        plan.template_run = None;
        validate_plan(&plan)?;
        host.metadata
            .save_prepared_launch(plan.preview.plan_id, &serde_json::to_string(&plan)?)?;
        Ok(plan.preview)
    })())
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
    let mut preview = plan.preview;
    preview.executable = Some(plan.launch.executable);
    preview.cwd = Some(plan.launch.cwd);
    preview.conversation = host.metadata.conversation_binding(session)?;
    ensure!(
        preview.conversation.as_ref().is_none_or(
            |binding| binding.session_id == session && binding.provider == preview.provider
        ),
        "saved conversation owner differs from its session"
    );
    Ok((preview, phase))
}

/// Bind only the selected logical session and a user-supplied exact ID. This
/// stores provenance; it does not spawn, send input or certify native resume.
pub(crate) fn bind_conversation(
    host: &mut Host,
    session: Uuid,
    generation: Uuid,
    conversation_id: &str,
    expected_revision: u64,
) -> Result<(LaunchPreview, String)> {
    host.reconcile_exits()?;
    let (mut preview, phase) = session_context(host, session, generation)?;
    if preview
        .conversation
        .as_ref()
        .is_some_and(|binding| binding.conversation_id != conversation_id)
    {
        // Do not release a provider-ID reservation while a live/uncertain
        // generation may still control that dialogue under its frozen plan.
        exited_head(host, session)
            .context("Остановите текущий процесс перед сменой привязанного диалога")?;
    }
    let record = host
        .metadata
        .get_launch(preview.plan_id)?
        .context("Сессия ещё не запускалась; привязка диалога недоступна")?;
    ensure!(
        record.session_id == session && record.generation == generation,
        "conversation launch generation differs from its source"
    );
    match preview.provider.as_str() {
        "codex" | "claude" => {
            let id = Uuid::parse_str(conversation_id).context("Нужен точный UUID диалога CLI")?;
            ensure!(
                !id.is_nil() && id.to_string() == conversation_id,
                "Используйте полный UUID диалога в стандартной записи"
            );
        }
        "opencode" => ensure!(
            adapters::is_opencode_session_id(conversation_id),
            "Нужен точный ID OpenCode вида ses_…"
        ),
        _ => anyhow::bail!("Привязка диалога недоступна для shell"),
    }
    let binding = host.metadata.bind_conversation(
        pigade_storage::ConversationBinding {
            session_id: session,
            source_generation: generation,
            plan_id: preview.plan_id,
            context_revision: preview.context.id,
            provider: preview.provider.clone(),
            conversation_id: conversation_id.to_owned(),
            revision: expected_revision,
            bound_at_unix_ms: 0,
            author: String::new(),
        },
        expected_revision,
    )?;
    preview.conversation = Some(binding);
    Ok((preview, phase))
}

/// Validate the reviewed snapshot and startup parameters together before any
/// attempt is admitted. A damaged saved plan cannot silently launch other text.
fn validate_plan(plan: &StoredPlan) -> Result<()> {
    plan.preview.context.validate()?;
    ensure!(
        plan.preview
            .executable
            .as_ref()
            .is_none_or(|path| *path == plan.launch.executable)
            && plan
                .preview
                .cwd
                .as_ref()
                .is_none_or(|path| *path == plan.launch.cwd),
        "saved launch provenance differs from its review"
    );
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
        (plan.preview.resume.is_some() && plan.placement.is_none())
            || matches!(&plan.placement,
        Some(LayoutCommand::Insert {tab,pane,target,side,..})
            if *tab == plan.preview.tab_id && *pane == plan.preview.session
                && *target == plan.preview.target && *side == plan.preview.side),
        "saved placement differs from reviewed destination"
    );
    plan.preview.engine.validate()?;
    let info = adapters::AdapterInfo {
        provider: plan.preview.provider.clone(),
        cli_version: plan.preview.cli_version.clone(),
        adapter_version: plan.preview.adapter_version,
        capabilities: plan.preview.capabilities.clone(),
        delivery_method: plan.preview.delivery_method.clone(),
        unsupported_reason: None,
    };
    let expected = if let Some(resume) = &plan.preview.resume {
        ensure!(
            plan.preview.engine.is_empty(),
            "resume must not reapply original model or reasoning settings"
        );
        ensure!(
            resume.binding.session_id == plan.launch.session
                && resume.binding.provider == plan.preview.provider
                && resume.binding.context_revision == plan.preview.context.id,
            "saved resume binding differs from its frozen context"
        );
        adapters::resume_arguments(
            &info,
            &resume.binding.conversation_id,
            &plan.launch.cwd,
            (plan.preview.deliver_context && !plan.preview.context.text.is_empty())
                .then_some(plan.preview.context.text.as_str()),
        )?
    } else {
        if !plan.preview.can_launch {
            return Ok(());
        }
        let mut args = adapters::engine_arguments(&info, &plan.preview.engine)?;
        let env = if plan.preview.deliver_context && !plan.preview.context.text.is_empty() {
            let (context_args, env) =
                adapters::context_arguments(&info, &plan.preview.context.text)?;
            args.extend(context_args);
            env
        } else {
            Default::default()
        };
        (args, env)
    };
    ensure!(
        plan.launch.args == expected.0 && plan.launch.env == expected.1,
        "saved startup parameters differ from the reviewed context"
    );
    Ok(())
}

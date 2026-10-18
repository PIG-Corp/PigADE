//! Pure state model for bounded whole-index Stage All / Unstage All operations.
//! Mutations are ticketed, submitted once, and retained as uncertain on loss.

use crate::{
    protocol::{
        GitIndexAction, GitIndexPhase, ProductError, ProductReply, WorkspaceGitEntry,
        WorkspaceGitIndexBatchPlan as Plan, WorkspaceGitIndexBatchResult as ResultReceipt,
        WorkspaceGitSnapshot,
    },
    workspace_git::Target,
};
use pigade_storage::Catalog;
use serde_json::{Value, json};
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Sheet {
    pub dialog: Uuid,
    pub target: Target,
    pub action: GitIndexAction,
    pub plan: Option<Plan>,
    pub error: Option<String>,
    pub page: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Operation {
    pub target: Target,
    pub state: ResultReceipt,
    pub applying: bool,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct Model {
    pub sheet: Option<Sheet>,
    pub operation: Option<Operation>,
    pending: Option<Pending>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Kind {
    Prepare,
    Apply { plan: Plan },
    Inspect,
    Acknowledge,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Pending {
    request: Uuid,
    dialog: Uuid,
    target: Target,
    kind: Kind,
    previous_sheet: Option<Sheet>,
    previous_operation: Option<Operation>,
}

impl Model {
    pub(crate) fn prepare(
        &mut self,
        target: Target,
        dialog: Uuid,
        action: GitIndexAction,
    ) -> Result<Value, String> {
        if self.pending.is_some() || self.unresolved() {
            return Err("Пакетная операция Git ещё не разрешена".into());
        }
        let previous_sheet = self.sheet.replace(Sheet {
            dialog,
            target: target.clone(),
            action,
            plan: None,
            error: None,
            page: 0,
        });
        let request = Uuid::new_v4();
        self.pending = Some(Pending {
            request,
            dialog,
            target: target.clone(),
            kind: Kind::Prepare,
            previous_sheet,
            previous_operation: self.operation.clone(),
        });
        Ok(with_request(
            json!({
                "command":"workspace_git_index_batch_prepare",
                "workspace":target.workspace,
                "expected_revision":target.revision,
                "action":action_name(action),
            }),
            request,
        ))
    }

    pub(crate) fn confirm(
        &mut self,
        catalog: Option<&Catalog>,
        dialog: Uuid,
    ) -> Result<Value, String> {
        if self.pending.is_some() {
            return Err("Дождитесь ответа session host".into());
        }
        if self.unresolved() {
            return Err("Сначала проверьте или подтвердите исход предыдущей операции Git".into());
        }
        let sheet = self
            .sheet
            .as_ref()
            .filter(|sheet| sheet.dialog == dialog)
            .ok_or_else(|| "Предпросмотр пакетного действия Git закрыт".to_owned())?;
        let plan = sheet
            .plan
            .clone()
            .ok_or_else(|| "Сначала подготовьте пакетный предпросмотр Git".to_owned())?;
        if !sheet.target.current(catalog)
            || !plan_matches(&plan, &sheet.target, sheet.action)
            || plan.expires_unix_ms <= unix_ms()
        {
            return Err("Предпросмотр Git устарел или больше не соответствует воркспейсу".into());
        }
        if self
            .operation
            .as_ref()
            .is_some_and(|operation| operation.state.ticket == plan.ticket)
        {
            return Err("Этот пакетный билет уже был отправлен и не может быть повторён".into());
        }

        let previous_operation = self.operation.clone();
        self.operation = Some(Operation {
            target: sheet.target.clone(),
            state: receipt_from_plan(&plan, GitIndexPhase::Unknown, None, false),
            applying: true,
        });
        let target = sheet.target.clone();
        let request = Uuid::new_v4();
        self.pending = Some(Pending {
            request,
            dialog,
            target: target.clone(),
            kind: Kind::Apply { plan: plan.clone() },
            previous_sheet: None,
            previous_operation,
        });
        Ok(with_request(
            json!({
                "command":"workspace_git_index_batch",
                "workspace":target.workspace,
                "expected_revision":target.revision,
                "ticket":plan.ticket,
                "inspect":false,
            }),
            request,
        ))
    }

    pub(crate) fn inspect(&mut self, dialog: Uuid) -> Result<Value, String> {
        self.request_existing(dialog, false)
    }

    pub(crate) fn acknowledge(&mut self, dialog: Uuid) -> Result<Value, String> {
        self.request_existing(dialog, true)
    }

    fn request_existing(&mut self, dialog: Uuid, acknowledge: bool) -> Result<Value, String> {
        if self.pending.is_some() {
            return Err("Дождитесь ответа session host".into());
        }
        let operation = self
            .operation
            .as_ref()
            .filter(|operation| !operation.applying)
            .filter(|operation| {
                matches!(
                    operation.state.phase,
                    GitIndexPhase::Unknown | GitIndexPhase::Unavailable
                )
            })
            .filter(|operation| !operation.state.acknowledged)
            .ok_or_else(|| "Нет неопределённой пакетной операции Git для проверки".to_owned())?;
        let ticket = operation.state.ticket;
        let target = operation.target.clone();
        let request = Uuid::new_v4();
        let kind = if acknowledge {
            Kind::Acknowledge
        } else {
            Kind::Inspect
        };
        self.pending = Some(Pending {
            request,
            dialog,
            target: target.clone(),
            kind,
            previous_sheet: None,
            previous_operation: self.operation.clone(),
        });
        let command = if acknowledge {
            json!({
                "command":"workspace_git_index_batch_acknowledge",
                "workspace":target.workspace,
                "expected_revision":target.revision,
                "ticket":ticket,
            })
        } else {
            json!({
                "command":"workspace_git_index_batch",
                "workspace":target.workspace,
                "expected_revision":target.revision,
                "ticket":ticket,
                "inspect":true,
            })
        };
        Ok(with_request(command, request))
    }

    pub(crate) fn matches_request(&self, request: Option<Uuid>) -> bool {
        self.pending
            .as_ref()
            .is_some_and(|pending| request == Some(pending.request))
    }

    pub(crate) fn receive(
        &mut self,
        reply: Result<ProductReply, ProductError>,
        live_dialog: Option<Uuid>,
        catalog: Option<&Catalog>,
    ) -> bool {
        let Some(pending) = self.pending.take() else {
            return false;
        };
        let request = match &reply {
            Ok(reply) => reply.ui_request,
            Err(error) => error.ui_request,
        };
        if request != Some(pending.request) {
            self.pending = Some(pending);
            return false;
        }

        match pending.kind {
            Kind::Prepare => {
                if live_dialog != Some(pending.dialog) {
                    return true;
                }
                let Some(sheet) = self.sheet.as_mut().filter(|sheet| {
                    sheet.dialog == pending.dialog && sheet.target == pending.target
                }) else {
                    return true;
                };
                match reply {
                    Ok(reply) => match reply.workspace_git_index_batch_plan {
                        Some(plan)
                            if plan_matches(&plan, &pending.target, sheet.action)
                                && pending.target.current(catalog) =>
                        {
                            sheet.plan = Some(plan);
                            sheet.error = None;
                        }
                        Some(_) => {
                            sheet.plan = None;
                            sheet.error = Some(
                                "План относится к другому действию или устаревшему воркспейсу"
                                    .into(),
                            );
                        }
                        None => {
                            sheet.plan = None;
                            sheet.error = Some("Session host не вернул план Git".into());
                        }
                    },
                    Err(error) => {
                        sheet.plan = None;
                        sheet.error = Some(bounded_message(error.message));
                    }
                }
            }
            Kind::Apply { plan } => {
                let Some(operation) = self.operation.as_mut().filter(|operation| {
                    operation.target == pending.target
                        && operation.state.ticket == plan.ticket
                        && operation.applying
                }) else {
                    return true;
                };
                match reply {
                    Ok(reply) => {
                        let result = reply.workspace_git_index_batch;
                        operation.applying = false;
                        operation.state = match result {
                            Some(result)
                                if receipt_matches_plan(&result, &plan, &pending.target) =>
                            {
                                if result.phase == GitIndexPhase::Unavailable {
                                    ResultReceipt {
                                        action: plan.action,
                                        paths: plan.paths.clone(),
                                        ..result
                                    }
                                } else {
                                    result
                                }
                            }
                            _ => uncertain_from_plan(
                                &plan,
                                "Квитанция пакетной операции не совпадает с предпросмотром",
                            ),
                        };
                    }
                    Err(error) => {
                        operation.applying = false;
                        operation.state = uncertain_from_plan(&plan, &error.message);
                    }
                }
            }
            kind @ (Kind::Inspect | Kind::Acknowledge) => {
                let Some(operation) = self
                    .operation
                    .as_mut()
                    .filter(|operation| operation.target == pending.target && !operation.applying)
                else {
                    return true;
                };
                let previous = operation.state.clone();
                match reply {
                    Ok(reply) => {
                        let Some(result) = reply.workspace_git_index_batch else {
                            operation.state = uncertain_from_result(
                                &previous,
                                "Session host не вернул квитанцию пакетной операции",
                            );
                            return true;
                        };
                        if result.ticket != previous.ticket
                            || !same_receipt_scope(&result, &pending.target)
                        {
                            operation.state = uncertain_from_result(
                                &previous,
                                "Квитанция относится к другой пакетной операции",
                            );
                        } else if result.phase == GitIndexPhase::Unavailable {
                            // Unavailable responses may not know the original action/path list.
                            // Preserve the frozen identity so this response cannot bless it.
                            let explicit_ack = matches!(&kind, Kind::Acknowledge)
                                && result.acknowledged
                                && result.action == previous.action
                                && result.paths == previous.paths;
                            operation.state = ResultReceipt {
                                phase: GitIndexPhase::Unavailable,
                                acknowledged: explicit_ack,
                                message: result.message,
                                ..previous
                            };
                        } else if result.action != previous.action || result.paths != previous.paths
                        {
                            operation.state = uncertain_from_result(
                                &previous,
                                "Квитанция не совпадает с действием и путями предпросмотра",
                            );
                        } else {
                            operation.state = ResultReceipt {
                                acknowledged: matches!(&kind, Kind::Acknowledge)
                                    && result.acknowledged,
                                ..result
                            };
                        }
                    }
                    Err(error) => {
                        operation.state = uncertain_from_result(&previous, &error.message);
                    }
                }
            }
        }
        true
    }

    /// Revert only work known not to have entered the host queue.
    pub(crate) fn abort_unsent(&mut self) -> bool {
        let Some(pending) = self.pending.take() else {
            return false;
        };
        if matches!(&pending.kind, Kind::Apply { .. }) {
            self.operation = pending.previous_operation;
        }
        if matches!(&pending.kind, Kind::Prepare) {
            self.sheet = pending.previous_sheet;
        }
        true
    }

    pub(crate) fn request_pending(&self) -> bool {
        self.pending.is_some()
    }

    pub(crate) fn unresolved(&self) -> bool {
        self.operation.as_ref().is_some_and(|operation| {
            operation.applying
                || match operation.state.phase {
                    GitIndexPhase::Unknown => !operation.state.acknowledged,
                    GitIndexPhase::Unavailable => !operation.state.acknowledged,
                    GitIndexPhase::Applied | GitIndexPhase::Rejected => false,
                }
        })
    }

    /// Restore a durable operation exposed by a new host snapshot after restart.
    pub(crate) fn observe_snapshot(
        &mut self,
        target: &Target,
        snapshot: &WorkspaceGitSnapshot,
    ) -> bool {
        if self.pending.is_some()
            || self
                .operation
                .as_ref()
                .is_some_and(|operation| operation.applying)
            || snapshot.index_batch_schema != Some(1)
            || snapshot.workspace_id != target.workspace
            || snapshot.workspace_revision != target.revision
            || snapshot.project_id != target.project
            || snapshot.cwd != target.cwd
        {
            return false;
        }
        let Some(result) = snapshot.index_batch_operation.clone() else {
            return false;
        };
        if result.workspace_id != target.workspace
            || result.workspace_revision == 0
            || result.workspace_revision > snapshot.workspace_revision
            || result.project_id != target.project
            || result.cwd != target.cwd
            || result.ticket.is_nil()
            || result.paths.len() > 256
            || result.paths.iter().any(|path| !valid_path(path))
            || result
                .paths
                .iter()
                .collect::<std::collections::HashSet<_>>()
                .len()
                != result.paths.len()
        {
            return false;
        }
        let mut receipt_target = target.clone();
        receipt_target.revision = result.workspace_revision;
        if !same_receipt_scope(&result, &receipt_target) {
            return false;
        }
        self.operation = Some(Operation {
            target: receipt_target,
            state: result,
            applying: false,
        });
        true
    }
}

fn plan_matches(plan: &Plan, target: &Target, action: GitIndexAction) -> bool {
    let affected = batch_affected_paths(&plan.entries, plan.action);
    plan.ticket != Uuid::nil()
        && plan.workspace_id == target.workspace
        && plan.workspace_revision == target.revision
        && plan.project_id == target.project
        && plan.cwd == target.cwd
        && plan.action == action
        && !plan.paths.is_empty()
        && plan.paths.len() <= 256
        && plan.entries.len() <= 128
        && !plan.entries.is_empty()
        && plan.entries.iter().all(|entry| {
            valid_path(&entry.path)
                && entry.original_path.as_deref().is_none_or(valid_path)
                && !entry.unmerged
                && entry.index_status != 'S'
                && entry.worktree_status != 'S'
                && match action {
                    GitIndexAction::Stage => entry.untracked || entry.worktree_status != '.',
                    GitIndexAction::Unstage => !entry.untracked && entry.index_status != '.',
                }
        })
        && plan.file_bytes <= 8 * 1024 * 1024
        && plan.paths.iter().all(|path| valid_path(path))
        && plan
            .paths
            .iter()
            .collect::<std::collections::HashSet<_>>()
            .len()
            == plan.paths.len()
        && affected.len() == plan.paths.len()
        && plan
            .paths
            .iter()
            .all(|path| affected.contains(&path.as_str()))
}

fn batch_affected_paths(
    entries: &[WorkspaceGitEntry],
    action: GitIndexAction,
) -> std::collections::HashSet<&str> {
    let mut affected = std::collections::HashSet::new();
    for entry in entries {
        affected.insert(entry.path.as_str());
        let include_original = match action {
            GitIndexAction::Stage => entry.worktree_status == 'R',
            GitIndexAction::Unstage => matches!(entry.index_status, 'R' | 'C'),
        };
        if include_original && let Some(original) = &entry.original_path {
            affected.insert(original.as_str());
        }
    }
    affected
}

fn receipt_matches_plan(receipt: &ResultReceipt, plan: &Plan, target: &Target) -> bool {
    receipt.ticket == plan.ticket
        && same_receipt_scope(receipt, target)
        && (receipt.phase == GitIndexPhase::Unavailable
            || (receipt.action == plan.action && receipt.paths == plan.paths))
}

fn same_receipt_scope(receipt: &ResultReceipt, target: &Target) -> bool {
    receipt.workspace_id == target.workspace
        && receipt.workspace_revision == target.revision
        && receipt.project_id == target.project
        && receipt.cwd == target.cwd
}

fn receipt_from_plan(
    plan: &Plan,
    phase: GitIndexPhase,
    message: Option<String>,
    acknowledged: bool,
) -> ResultReceipt {
    ResultReceipt {
        ticket: plan.ticket,
        workspace_id: plan.workspace_id,
        workspace_revision: plan.workspace_revision,
        project_id: plan.project_id,
        cwd: plan.cwd.clone(),
        action: plan.action,
        paths: plan.paths.clone(),
        phase,
        acknowledged,
        message,
    }
}

fn uncertain_from_plan(plan: &Plan, message: &str) -> ResultReceipt {
    receipt_from_plan(
        plan,
        GitIndexPhase::Unknown,
        Some(bounded_message(message.to_owned())),
        false,
    )
}

fn uncertain_from_result(previous: &ResultReceipt, message: &str) -> ResultReceipt {
    ResultReceipt {
        phase: GitIndexPhase::Unknown,
        acknowledged: false,
        message: Some(bounded_message(message.to_owned())),
        ..previous.clone()
    }
}

fn action_name(action: GitIndexAction) -> &'static str {
    match action {
        GitIndexAction::Stage => "stage",
        GitIndexAction::Unstage => "unstage",
    }
}

fn valid_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 32768
        && !path.starts_with('-')
        && !path.starts_with('/')
        && !path.chars().any(char::is_control)
        && !path
            .split('/')
            .any(|component| component.is_empty() || matches!(component, "." | ".."))
        && std::path::Path::new(path)
            .components()
            .all(|component| matches!(component, std::path::Component::Normal(_)))
}

fn with_request(mut command: Value, request: Uuid) -> Value {
    command["_ui_request"] = json!(request);
    command
}

fn bounded_message(message: String) -> String {
    message.chars().take(512).collect()
}

fn unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use pigade_storage::{
        Catalog, LayoutDocument, Project, Selection, WindowState, Workspace, WorkspaceMode,
        WorkspaceStatus,
    };
    use std::path::PathBuf;

    fn target() -> Target {
        Target {
            workspace: Uuid::from_u128(11),
            revision: 7,
            project: Uuid::from_u128(12),
            cwd: PathBuf::from("/worktree"),
            name: "Task".into(),
        }
    }

    fn catalog(target: &Target) -> Catalog {
        Catalog {
            projects: vec![Project {
                id: target.project,
                canonical_path: "/project".into(),
                git_common_dir: "/project/.git".into(),
                name: "Project".into(),
                roots: Vec::new(),
                revision: 1,
            }],
            workspaces: vec![Workspace {
                id: target.workspace,
                project_id: target.project,
                task: target.name.clone(),
                cwd: target.cwd.clone(),
                mode: WorkspaceMode::Worktree,
                status: WorkspaceStatus::Ready,
                error: None,
                context_revision: None,
                revision: target.revision,
            }],
            tabs: Vec::new(),
            selection: Selection {
                project_id: Some(target.project),
                workspace_id: Some(target.workspace),
                tab_id: None,
                window: WindowState::default(),
                revision: 1,
            },
            layout: LayoutDocument {
                json: "null".into(),
                revision: 1,
            },
            layout_templates: None,
            template_launch_schema: None,
            pane_labels: None,
            agent_profiles: None,
            agent_profile_roster: None,
            prompts: None,
            tab_close_schema: None,
            engine_options_schema: None,
            pane_tab_drop_schema: None,
            pane_workspace_drop_schema: None,
            workspace_recovery_schema: None,
            history_grid_schema: None,
            custom_cli_schema: None,
        }
    }

    fn plan(target: &Target, action: GitIndexAction, expires: u64) -> Plan {
        Plan {
            ticket: Uuid::from_u128(13),
            workspace_id: target.workspace,
            workspace_revision: target.revision,
            project_id: target.project,
            cwd: target.cwd.clone(),
            action,
            paths: vec!["src/a.rs".into(), "src/renamed-old.rs".into()],
            entries: vec![WorkspaceGitEntry {
                path: "src/a.rs".into(),
                original_path: Some("src/renamed-old.rs".into()),
                index_status: match action {
                    GitIndexAction::Stage => '.',
                    GitIndexAction::Unstage => 'R',
                },
                worktree_status: match action {
                    GitIndexAction::Stage => 'R',
                    GitIndexAction::Unstage => '.',
                },
                untracked: false,
                unmerged: false,
            }],
            file_bytes: 17,
            expires_unix_ms: expires,
        }
    }

    fn reply(request: Uuid, plan: Option<Plan>, result: Option<ResultReceipt>) -> ProductReply {
        ProductReply {
            ui_request: Some(request),
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
            workspace_git_snapshot: None,
            workspace_git_watch: None,
            workspace_git_diff: None,
            workspace_git_index: None,
            workspace_git_commit_plan: None,
            workspace_git_commit: None,
            workspace_git_index_batch_plan: plan,
            workspace_git_index_batch: result,
        }
    }

    fn error(request: Uuid, message: &str) -> ProductError {
        ProductError {
            ui_request: Some(request),
            providers_request: false,
            message: message.into(),
            rejected_before_launch: false,
        }
    }

    fn prepare_with_plan(model: &mut Model, target: &Target, dialog: Uuid, action: GitIndexAction) {
        let command = model.prepare(target.clone(), dialog, action).unwrap();
        let request = command["_ui_request"].as_str().unwrap().parse().unwrap();
        assert!(model.matches_request(Some(request)));
        assert!(model.receive(
            Ok(reply(
                request,
                Some(plan(target, action, unix_ms() + 10_000)),
                None
            )),
            Some(dialog),
            Some(&catalog(target)),
        ));
    }

    #[test]
    fn plan_is_frozen_and_stale_or_expired_previews_cannot_be_applied() {
        let target = target();
        let dialog = Uuid::from_u128(20);
        let mut model = Model::default();
        prepare_with_plan(&mut model, &target, dialog, GitIndexAction::Stage);

        let mut stale_catalog = catalog(&target);
        stale_catalog.workspaces[0].revision += 1;
        assert!(model.confirm(Some(&stale_catalog), dialog).is_err());
        let sheet = model.sheet.as_mut().unwrap();
        sheet.plan.as_mut().unwrap().expires_unix_ms = unix_ms().saturating_sub(1);
        assert!(model.confirm(Some(&catalog(&target)), dialog).is_err());
        let sheet = model.sheet.as_mut().unwrap();
        sheet.plan = Some(plan(&target, GitIndexAction::Unstage, unix_ms() + 10_000));
        assert!(model.confirm(Some(&catalog(&target)), dialog).is_err());
    }

    #[test]
    fn stage_rename_destination_edit_omits_original_from_affected_paths() {
        let target = target();
        let mut stage = plan(&target, GitIndexAction::Stage, unix_ms() + 5_000);
        stage.paths = vec!["src/new.rs".into()];
        stage.entries[0].path = "src/new.rs".into();
        stage.entries[0].original_path = Some("src/old.rs".into());
        stage.entries[0].index_status = 'R';
        stage.entries[0].worktree_status = 'M';
        assert!(plan_matches(&stage, &target, GitIndexAction::Stage));

        let mut unstage = stage;
        unstage.action = GitIndexAction::Unstage;
        unstage.paths.push("src/old.rs".into());
        assert!(plan_matches(&unstage, &target, GitIndexAction::Unstage));
    }

    #[test]
    fn lost_apply_reply_is_unknown_and_never_resubmits_the_ticket() {
        let target = target();
        let dialog = Uuid::from_u128(21);
        let mut model = Model::default();
        prepare_with_plan(&mut model, &target, dialog, GitIndexAction::Stage);
        let command = model.confirm(Some(&catalog(&target)), dialog).unwrap();
        let request = command["_ui_request"].as_str().unwrap().parse().unwrap();
        let ticket = model.operation.as_ref().unwrap().state.ticket;
        assert_eq!(command["ticket"], ticket.to_string());
        assert!(model.receive(Err(error(request, "connection lost")), None, None));
        let operation = model.operation.as_ref().unwrap();
        assert_eq!(operation.state.phase, GitIndexPhase::Unknown);
        assert!(model.unresolved());
        assert!(model.confirm(Some(&catalog(&target)), dialog).is_err());

        let inspect = model.inspect(dialog).unwrap();
        assert_eq!(inspect["ticket"], ticket.to_string());
        assert_eq!(inspect["inspect"], true);
        assert!(!model.receive(
            Ok(reply(request, None, None)),
            Some(dialog),
            Some(&catalog(&target)),
        ));
        let inspect_request = inspect["_ui_request"].as_str().unwrap().parse().unwrap();
        let applied = model.operation.as_ref().unwrap().state.clone();
        let mut receipt = applied;
        receipt.phase = GitIndexPhase::Applied;
        receipt.message = None;
        assert!(model.receive(
            Ok(reply(inspect_request, None, Some(receipt))),
            Some(dialog),
            Some(&catalog(&target)),
        ));
        assert_eq!(
            model.operation.as_ref().unwrap().state.phase,
            GitIndexPhase::Applied
        );
        assert!(!model.unresolved());
    }

    #[test]
    fn unavailable_outcome_requires_explicit_acknowledgement_without_becoming_applied() {
        let target = target();
        let dialog = Uuid::from_u128(22);
        let mut model = Model::default();
        prepare_with_plan(&mut model, &target, dialog, GitIndexAction::Unstage);
        let apply = model.confirm(Some(&catalog(&target)), dialog).unwrap();
        let apply_request = apply["_ui_request"].as_str().unwrap().parse().unwrap();
        let mut unavailable = model.operation.as_ref().unwrap().state.clone();
        unavailable.phase = GitIndexPhase::Unavailable;
        unavailable.action = GitIndexAction::Stage;
        unavailable.paths.clear();
        unavailable.message = Some("receipt missing".into());
        assert!(model.receive(
            Ok(reply(apply_request, None, Some(unavailable))),
            Some(dialog),
            Some(&catalog(&target)),
        ));
        assert_eq!(
            model.operation.as_ref().unwrap().state.phase,
            GitIndexPhase::Unavailable
        );
        assert_eq!(
            model.operation.as_ref().unwrap().state.action,
            GitIndexAction::Unstage
        );
        assert!(model.unresolved());

        let ack = model.acknowledge(dialog).unwrap();
        assert_eq!(
            ack["ticket"],
            model.operation.as_ref().unwrap().state.ticket.to_string()
        );
        let ack_request = ack["_ui_request"].as_str().unwrap().parse().unwrap();
        let mut missing_receipt = model.operation.as_ref().unwrap().state.clone();
        missing_receipt.action = GitIndexAction::Stage;
        missing_receipt.paths.clear();
        assert!(model.receive(
            Ok(reply(ack_request, None, Some(missing_receipt))),
            Some(dialog),
            None,
        ));
        assert_eq!(
            model.operation.as_ref().unwrap().state.phase,
            GitIndexPhase::Unavailable
        );
        assert!(model.unresolved());
    }

    #[test]
    fn trusted_explicit_ack_releases_unavailable_state_without_claiming_success() {
        let target = target();
        let dialog = Uuid::from_u128(26);
        let mut model = Model::default();
        prepare_with_plan(&mut model, &target, dialog, GitIndexAction::Stage);
        let apply = model.confirm(Some(&catalog(&target)), dialog).unwrap();
        let apply_request = apply["_ui_request"].as_str().unwrap().parse().unwrap();
        let mut unavailable = model.operation.as_ref().unwrap().state.clone();
        unavailable.phase = GitIndexPhase::Unavailable;
        unavailable.message = Some("durable outcome unavailable".into());
        assert!(model.receive(
            Ok(reply(apply_request, None, Some(unavailable))),
            Some(dialog),
            None,
        ));
        assert!(model.unresolved());

        let ack = model.acknowledge(dialog).unwrap();
        let ack_request = ack["_ui_request"].as_str().unwrap().parse().unwrap();
        let mut acknowledged = model.operation.as_ref().unwrap().state.clone();
        acknowledged.acknowledged = true;
        assert!(model.receive(
            Ok(reply(ack_request, None, Some(acknowledged))),
            Some(dialog),
            None,
        ));
        let operation = model.operation.as_ref().unwrap();
        assert_eq!(operation.state.phase, GitIndexPhase::Unavailable);
        assert!(operation.state.acknowledged);
        assert!(!model.unresolved());
    }

    #[test]
    fn inspect_cannot_acknowledge_or_release_an_unavailable_outcome() {
        let target = target();
        let dialog = Uuid::from_u128(27);
        let mut model = Model::default();
        prepare_with_plan(&mut model, &target, dialog, GitIndexAction::Stage);
        let apply = model.confirm(Some(&catalog(&target)), dialog).unwrap();
        let apply_request = apply["_ui_request"].as_str().unwrap().parse().unwrap();
        let mut unavailable = model.operation.as_ref().unwrap().state.clone();
        unavailable.phase = GitIndexPhase::Unavailable;
        assert!(model.receive(
            Ok(reply(apply_request, None, Some(unavailable))),
            Some(dialog),
            None,
        ));

        let inspect = model.inspect(dialog).unwrap();
        let inspect_request = inspect["_ui_request"].as_str().unwrap().parse().unwrap();
        let mut forged_ack = model.operation.as_ref().unwrap().state.clone();
        forged_ack.acknowledged = true;
        assert!(model.receive(
            Ok(reply(inspect_request, None, Some(forged_ack))),
            Some(dialog),
            None,
        ));
        assert_eq!(
            model.operation.as_ref().unwrap().state.phase,
            GitIndexPhase::Unavailable
        );
        assert!(!model.operation.as_ref().unwrap().state.acknowledged);
        assert!(model.unresolved());
    }

    #[test]
    fn late_prepare_for_closed_or_replaced_dialog_does_not_install_plan() {
        let target = target();
        let dialog = Uuid::from_u128(23);
        let mut model = Model::default();
        let command = model
            .prepare(target.clone(), dialog, GitIndexAction::Stage)
            .unwrap();
        let request = command["_ui_request"].as_str().unwrap().parse().unwrap();
        assert!(model.receive(
            Ok(reply(
                request,
                Some(plan(&target, GitIndexAction::Stage, unix_ms() + 5000)),
                None
            )),
            None,
            Some(&catalog(&target)),
        ));
        assert!(model.sheet.as_ref().unwrap().plan.is_none());
        assert!(!model.request_pending());
    }

    #[test]
    fn wrong_paths_or_scope_never_bless_a_batch_receipt() {
        let target = target();
        let dialog = Uuid::from_u128(24);
        let mut model = Model::default();
        prepare_with_plan(&mut model, &target, dialog, GitIndexAction::Stage);
        let command = model.confirm(Some(&catalog(&target)), dialog).unwrap();
        let request = command["_ui_request"].as_str().unwrap().parse().unwrap();
        let mut wrong = model.operation.as_ref().unwrap().state.clone();
        wrong.phase = GitIndexPhase::Applied;
        wrong.paths = vec!["src/other.rs".into()];
        assert!(model.receive(Ok(reply(request, None, Some(wrong))), Some(dialog), None,));
        assert_eq!(
            model.operation.as_ref().unwrap().state.phase,
            GitIndexPhase::Unknown
        );
        assert!(model.unresolved());
    }

    #[test]
    fn wrong_scope_receipts_and_transport_failures_remain_unknown_with_frozen_identity() {
        for mismatch in 0..5 {
            let target = target();
            let dialog = Uuid::from_u128(28);
            let mut model = Model::default();
            prepare_with_plan(&mut model, &target, dialog, GitIndexAction::Stage);
            let command = model.confirm(Some(&catalog(&target)), dialog).unwrap();
            let request = command["_ui_request"].as_str().unwrap().parse().unwrap();
            let mut wrong = model.operation.as_ref().unwrap().state.clone();
            wrong.phase = GitIndexPhase::Applied;
            match mismatch {
                0 => wrong.ticket = Uuid::new_v4(),
                1 => wrong.workspace_id = Uuid::new_v4(),
                2 => wrong.workspace_revision += 1,
                3 => wrong.project_id = Uuid::new_v4(),
                _ => wrong.cwd = "/another/worktree".into(),
            }
            assert!(model.receive(Ok(reply(request, None, Some(wrong))), Some(dialog), None,));
            let operation = model.operation.as_ref().unwrap();
            assert_eq!(operation.state.phase, GitIndexPhase::Unknown);
            assert_eq!(operation.state.ticket, Uuid::from_u128(13));
            assert_eq!(operation.state.workspace_id, target.workspace);
            assert!(model.unresolved());
        }

        let target = target();
        let dialog = Uuid::from_u128(29);
        let mut model = Model::default();
        prepare_with_plan(&mut model, &target, dialog, GitIndexAction::Unstage);
        let command = model.confirm(Some(&catalog(&target)), dialog).unwrap();
        let request = command["_ui_request"].as_str().unwrap().parse().unwrap();
        assert!(model.receive(
            Err(error(request, "connection lost after dispatch")),
            None,
            None
        ));
        let operation = model.operation.as_ref().unwrap();
        assert_eq!(operation.state.phase, GitIndexPhase::Unknown);
        assert_eq!(operation.state.action, GitIndexAction::Unstage);
        assert!(model.unresolved());
    }

    #[test]
    fn known_unsent_apply_rolls_back_to_the_same_preview_and_old_operation() {
        let target = target();
        let dialog = Uuid::from_u128(25);
        let mut model = Model::default();
        prepare_with_plan(&mut model, &target, dialog, GitIndexAction::Stage);
        let saved_plan = model.sheet.as_ref().unwrap().plan.clone();
        assert!(model.confirm(Some(&catalog(&target)), dialog).is_ok());
        assert!(model.operation.as_ref().unwrap().applying);
        assert!(model.abort_unsent());
        assert!(model.operation.is_none());
        assert_eq!(model.sheet.as_ref().unwrap().plan, saved_plan);
        assert!(!model.unresolved());
    }

    #[test]
    fn snapshot_restores_same_scope_durable_operation_and_ignores_legacy_or_other_scope() {
        let mut target = target();
        target.revision += 1;
        let mut receipt_target = target.clone();
        receipt_target.revision -= 1;
        let mut result = receipt_from_plan(
            &plan(&receipt_target, GitIndexAction::Stage, unix_ms() + 1_000),
            GitIndexPhase::Unknown,
            Some("uncertain".into()),
            false,
        );
        let snapshot = WorkspaceGitSnapshot {
            workspace_id: target.workspace,
            workspace_revision: target.revision,
            project_id: target.project,
            cwd: target.cwd.clone(),
            branch: None,
            head: None,
            detached: false,
            entries: Vec::new(),
            truncated: false,
            index_operation: None,
            index_operation_unavailable_reason: None,
            commit_schema: None,
            commit_operation: None,
            commit_unavailable_reason: None,
            index_batch_schema: Some(1),
            index_batch_operation: Some(result.clone()),
            index_batch_unavailable_reason: None,
        };
        let mut model = Model::default();
        assert!(model.observe_snapshot(&target, &snapshot));
        assert_eq!(
            model.operation.as_ref().unwrap().target.revision,
            receipt_target.revision
        );
        assert!(model.unresolved());
        result.workspace_revision = snapshot.workspace_revision + 1;
        let mut other = snapshot.clone();
        other.index_batch_operation = Some(result);
        assert!(!model.observe_snapshot(&target, &other));
        let mut foreign = snapshot.clone();
        let mut receipt = foreign.index_batch_operation.clone().unwrap();
        receipt.project_id = Uuid::new_v4();
        foreign.index_batch_operation = Some(receipt);
        assert!(!model.observe_snapshot(&target, &foreign));
        other.index_batch_schema = None;
        assert!(!model.observe_snapshot(&target, &other));
    }

    #[test]
    fn legacy_snapshot_optional_batch_fields_deserialize_absent() {
        let snapshot: WorkspaceGitSnapshot = serde_json::from_value(json!({
            "workspace_id": Uuid::from_u128(1), "workspace_revision": 2,
            "project_id": Uuid::from_u128(3), "cwd":"/worktree", "branch":null,
            "head":null, "detached":false, "entries":[], "truncated":false
        }))
        .unwrap();
        assert_eq!(snapshot.index_batch_schema, None);
        assert_eq!(snapshot.index_batch_operation, None);
    }
}

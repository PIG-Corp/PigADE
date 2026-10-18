//! Explicit, generation-bound process stopping. Reconciliation only reads state;
//! a lost acknowledgement never causes another Stop request.
use crate::{
    CanvasApp,
    product::{Dialog, DialogKind, ProductAction},
    protocol,
};
use gpui::Context;
use serde_json::json;
use std::time::{Duration, Instant};
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Target {
    pub session: Uuid,
    pub generation: Uuid,
    pub title: String,
    pub stop_scope: Option<String>,
}

impl Target {
    fn matches(&self, session: &str, generation: &str) -> bool {
        session.parse::<Uuid>().ok() == Some(self.session)
            && generation.parse::<Uuid>().ok() == Some(self.generation)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Phase {
    Draft,
    Sending,
    Waiting,
    Unknown,
    Complete,
    Unavailable,
}

impl Phase {
    pub(crate) fn message(self) -> &'static str {
        match self {
            Self::Draft => "Работа CLI будет прервана. Панель и сохранённая история останутся.",
            Self::Sending => "Отправляем запрос остановки…",
            Self::Waiting => "Запрос принят. Ожидаем подтверждённого завершения CLI…",
            Self::Unknown => {
                "Ответ не получен. Проверяем состояние; запрос остановки не повторяется."
            }
            Self::Complete => "CLI завершён. Панель и сохранённая история доступны.",
            Self::Unavailable => {
                "Host не подтверждает завершение CLI. Сохранённые данные остаются доступны."
            }
        }
    }
}

#[derive(Clone)]
pub(crate) struct StopSheet {
    pub dialog: Uuid,
    pub target: Target,
    pub phase: Phase,
    pub error: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Stop,
    Inspect,
}

#[derive(Clone)]
pub(crate) struct Pending {
    id: Uuid,
    target: Target,
    kind: Kind,
}

struct Operation {
    target: Target,
    phase: Phase,
    error: Option<String>,
    request: Option<Pending>,
    due: Instant,
}

#[derive(Default)]
pub(crate) struct StopControl {
    operations: Vec<Operation>,
}

impl StopControl {
    fn find(&self, target: &Target) -> Option<&Operation> {
        self.operations.iter().find(|operation| {
            operation.target.session == target.session
                && operation.target.generation == target.generation
        })
    }
    fn find_mut(&mut self, target: &Target) -> Option<&mut Operation> {
        self.operations.iter_mut().find(|operation| {
            operation.target.session == target.session
                && operation.target.generation == target.generation
        })
    }
    pub(crate) fn take_reply(&mut self, id: Option<Uuid>) -> Option<Pending> {
        let id = id?;
        self.operations
            .iter_mut()
            .find(|operation| {
                operation
                    .request
                    .as_ref()
                    .is_some_and(|request| request.id == id)
            })?
            .request
            .take()
    }
    fn observe(
        &mut self,
        session: &str,
        generation: &str,
        state: &str,
        stop_error: Option<&str>,
    ) -> Option<(Target, Phase)> {
        let operation = self
            .operations
            .iter_mut()
            .find(|operation| operation.target.matches(session, generation))?;
        let phase = match state {
            "exited" => Phase::Complete,
            _ if stop_error.is_some() && operation.phase != Phase::Complete => Phase::Unavailable,
            "history" | "historical" | "history_unavailable" | "degraded"
                if operation.phase != Phase::Complete =>
            {
                Phase::Unavailable
            }
            _ => return None,
        };
        let error = stop_error
            .filter(|_| phase != Phase::Complete)
            .map(|message| {
                message
                    .chars()
                    .filter(|ch| !ch.is_control())
                    .take(1024)
                    .collect::<String>()
            });
        if operation.phase == phase && operation.error == error {
            return None;
        }
        operation.error = error;
        operation.phase = phase;
        Some((operation.target.clone(), phase))
    }
}

pub(crate) fn scope_message(scope: Option<&str>) -> &'static str {
    if scope == Some("pty_process_group_v1") {
        "Область остановки — группа процессов этого терминала. Процессы в других группах или сессиях могут продолжить работу."
    } else {
        "Область остановки host не подтверждена. Дочерние процессы CLI могут продолжить работу."
    }
}

pub(crate) fn can_stop(state: &str, pid: Option<u32>) -> bool {
    matches!(state, "running" | "degraded") && pid.is_some_and(|pid| pid > 0)
}

impl CanvasApp {
    pub(crate) fn session_control_action(&mut self, action: ProductAction, cx: &mut Context<Self>) {
        match action {
            ProductAction::OpenStop => {
                let Some(view) = self.state.views.get(self.selected) else {
                    return;
                };
                let (Ok(session), Ok(generation)) = (
                    view.session.parse::<Uuid>(),
                    view.generation.parse::<Uuid>(),
                ) else {
                    return;
                };
                if session.is_nil() || generation.is_nil() {
                    return;
                }
                let target = Target {
                    session,
                    generation,
                    title: self.product.pane_title(view).chars().take(160).collect(),
                    stop_scope: view.stop_scope.clone(),
                };
                let stoppable = can_stop(&view.state, view.pid);
                self.open_stop(target, stoppable, cx);
            }
            ProductAction::StopSession {
                session,
                generation,
            } => {
                let Some(summary) = self.product.sessions.iter().find(|summary| {
                    summary.session.parse::<Uuid>().ok() == Some(session)
                        && summary.generation.parse::<Uuid>().ok() == Some(generation)
                }) else {
                    return;
                };
                let title = self
                    .product
                    .display_title(&summary.session, &summary.title)
                    .chars()
                    .take(160)
                    .collect();
                let stoppable = can_stop(&summary.state, summary.pid);
                self.open_stop(
                    Target {
                        session,
                        generation,
                        title,
                        stop_scope: summary.stop_scope.clone(),
                    },
                    stoppable,
                    cx,
                );
            }
            ProductAction::ConfirmStop => self.confirm_stop(cx),
            ProductAction::InspectStop => {
                if let Some(target) = self
                    .product
                    .stop_sheet
                    .as_ref()
                    .map(|sheet| sheet.target.clone())
                {
                    self.request_stop_state(target, cx);
                }
            }
            _ => (),
        }
    }

    fn open_stop(&mut self, target: Target, stoppable: bool, cx: &mut Context<Self>) {
        let phase = self
            .stop_control
            .find(&target)
            .map(|operation| operation.phase);
        if !stoppable && phase.is_none() {
            self.product.notice =
                Some("В выбранном поколении нет доступного для остановки CLI".into());
            cx.notify();
            return;
        }
        let error = self
            .stop_control
            .find(&target)
            .and_then(|operation| operation.error.clone())
            .or_else(|| {
                self.state
                    .views
                    .iter()
                    .find(|view| target.matches(&view.session, &view.generation))
                    .and_then(|view| view.stop_error.clone())
            })
            .or_else(|| {
                self.product
                    .sessions
                    .iter()
                    .find(|view| target.matches(&view.session, &view.generation))
                    .and_then(|view| view.stop_error.clone())
            });
        // Reopened UI may already know a failed host stop without a local
        // request. Remember only its observation so Inspect can read it; never
        // synthesize or resend a Stop from restored metadata.
        if let Some(error) = &error
            && self.stop_control.find(&target).is_none()
        {
            if self.stop_control.operations.len() >= 32 {
                self.product.notice = Some(
                    "Ожидается подтверждение 32 остановок. Проверьте состояние сессий.".into(),
                );
                cx.notify();
                return;
            }
            self.stop_control.operations.push(Operation {
                target: target.clone(),
                phase: Phase::Unavailable,
                error: Some(error.clone()),
                request: None,
                due: Instant::now(),
            });
        }
        let dialog = Dialog::new(DialogKind::StopSession);
        self.product.stop_sheet = Some(StopSheet {
            dialog: dialog.id,
            target,
            phase: phase.unwrap_or(if error.is_some() {
                Phase::Unavailable
            } else {
                Phase::Draft
            }),
            error,
        });
        self.product.dialog = Some(dialog);
        self.product.notice = None;
        cx.notify();
    }

    pub(crate) fn confirm_stop(&mut self, cx: &mut Context<Self>) {
        let Some(sheet) = self
            .product
            .stop_sheet
            .as_ref()
            .filter(|sheet| {
                self.product.dialog.as_ref().is_some_and(|dialog| {
                    dialog.id == sheet.dialog && dialog.kind == DialogKind::StopSession
                }) && sheet.phase == Phase::Draft
            })
            .cloned()
        else {
            return;
        };
        if self.stop_control.find(&sheet.target).is_some() {
            return;
        }
        if self.stop_control.operations.len() >= 32 {
            self.product.notice =
                Some("Ожидается подтверждение 32 остановок. Проверьте состояние сессий.".into());
            return;
        }
        let pending = Pending {
            id: Uuid::new_v4(),
            target: sheet.target.clone(),
            kind: Kind::Stop,
        };
        if self.try_product_command(
            json!({"command":"stop","session":pending.target.session,
            "generation":pending.target.generation,"_ui_request":pending.id}),
            cx,
        ) {
            self.stop_control.operations.push(Operation {
                target: sheet.target.clone(),
                phase: Phase::Sending,
                error: None,
                request: Some(pending),
                due: Instant::now() + Duration::from_secs(1),
            });
            self.present_stop_phase(&sheet.target, Phase::Sending, cx);
        }
    }

    fn present_stop_phase(&mut self, target: &Target, phase: Phase, cx: &mut Context<Self>) {
        let error = self
            .stop_control
            .find(target)
            .and_then(|operation| operation.error.clone());
        if let Some(sheet) = &mut self.product.stop_sheet
            && sheet.target.session == target.session
            && sheet.target.generation == target.generation
        {
            if sheet.phase != phase
                && let Some(dialog) = &mut self.product.dialog
                && dialog.id == sheet.dialog
            {
                dialog.active_field = 0;
            }
            sheet.phase = phase;
            sheet.error = error;
        }
        if phase == Phase::Complete {
            for summary in &mut self.product.sessions {
                if target.matches(&summary.session, &summary.generation) {
                    summary.state = "exited".into();
                }
            }
            self.product.notice =
                Some(format!("{}: CLI завершён; история сохранена", target.title));
        }
        cx.notify();
    }

    pub(crate) fn receive_stop(
        &mut self,
        pending: Pending,
        view: Option<protocol::View>,
        cx: &mut Context<Self>,
    ) {
        let Some(view) =
            view.filter(|view| pending.target.matches(&view.session, &view.generation))
        else {
            self.stop_failed(
                pending,
                "Ответ остановки относится к другой сессии".into(),
                cx,
            );
            return;
        };
        if let Some(operation) = self.stop_control.find_mut(&pending.target) {
            if pending.kind == Kind::Stop && operation.phase == Phase::Sending {
                operation.phase = Phase::Waiting;
            }
            operation.due = Instant::now()
                + Duration::from_secs(if pending.kind == Kind::Stop { 1 } else { 5 });
        }
        if let Some((target, phase)) = self.stop_control.observe(
            &view.session,
            &view.generation,
            &view.state,
            view.stop_error.as_deref(),
        ) {
            self.present_stop_phase(&target, phase, cx);
        } else if let Some(operation) = self.stop_control.find(&pending.target) {
            self.present_stop_phase(&pending.target, operation.phase, cx);
        }
    }

    pub(crate) fn stop_failed(
        &mut self,
        pending: Pending,
        message: String,
        cx: &mut Context<Self>,
    ) {
        if let Some(operation) = self.stop_control.find_mut(&pending.target) {
            if operation.phase != Phase::Complete && pending.kind == Kind::Stop {
                operation.phase = Phase::Unknown;
            }
            operation.due = Instant::now() + Duration::from_secs(5);
            let phase = operation.phase;
            self.present_stop_phase(&pending.target, phase, cx);
            if phase != Phase::Complete
                && self
                    .product
                    .dialog
                    .as_ref()
                    .is_some_and(|dialog| dialog.kind == DialogKind::StopSession)
                && self.product.stop_sheet.as_ref().is_some_and(|sheet| {
                    sheet.target.session == pending.target.session
                        && sheet.target.generation == pending.target.generation
                })
            {
                self.product.notice = Some(message);
            }
        }
    }

    pub(crate) fn service_stops(&mut self, cx: &mut Context<Self>) {
        if self.stop_control.operations.is_empty() {
            return;
        }
        let observed: Vec<_> = self
            .state
            .views
            .iter()
            .filter_map(|view| {
                self.stop_control.observe(
                    &view.session,
                    &view.generation,
                    &view.state,
                    view.stop_error.as_deref(),
                )
            })
            .collect();
        for (target, phase) in observed {
            self.present_stop_phase(&target, phase, cx);
        }
        self.stop_control
            .operations
            .retain(|operation| operation.phase != Phase::Complete || operation.request.is_some());
        if self.product.busy {
            return;
        }
        let now = Instant::now();
        let Some(operation) = self.stop_control.operations.iter().find(|operation| {
            operation.request.is_none()
                && operation.phase != Phase::Unavailable
                && operation.due <= now
        }) else {
            return;
        };
        self.request_stop_state(operation.target.clone(), cx);
    }

    fn request_stop_state(&mut self, target: Target, cx: &mut Context<Self>) {
        if self
            .stop_control
            .find(&target)
            .is_none_or(|operation| operation.request.is_some())
        {
            return;
        }
        let pending = Pending {
            id: Uuid::new_v4(),
            target,
            kind: Kind::Inspect,
        };
        if self.try_product_command(
            json!({"command":"snapshot","session":pending.target.session,
            "generation":pending.target.generation,"_ui_request":pending.id}),
            cx,
        ) && let Some(operation) = self.stop_control.find_mut(&pending.target)
        {
            operation.request = Some(pending);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn target(generation: u128) -> Target {
        Target {
            session: Uuid::from_u128(1),
            generation: Uuid::from_u128(generation),
            title: "Review".into(),
            stop_scope: None,
        }
    }
    fn control() -> StopControl {
        StopControl {
            operations: vec![Operation {
                target: target(2),
                phase: Phase::Unknown,
                error: None,
                request: Some(Pending {
                    id: Uuid::from_u128(3),
                    target: target(2),
                    kind: Kind::Stop,
                }),
                due: Instant::now(),
            }],
        }
    }
    #[test]
    fn another_generation_cannot_confirm_a_stopped_process() {
        let mut control = control();
        assert!(
            control
                .observe(
                    &Uuid::from_u128(1).to_string(),
                    &Uuid::from_u128(4).to_string(),
                    "exited",
                    None
                )
                .is_none()
        );
        assert_eq!(control.operations[0].phase, Phase::Unknown);
        let update = control
            .observe(
                &Uuid::from_u128(1).to_string(),
                &Uuid::from_u128(2).to_string(),
                "exited",
                None,
            )
            .unwrap();
        assert_eq!(update.1, Phase::Complete);
    }
    #[test]
    fn history_does_not_prove_the_process_exited() {
        let mut control = control();
        let update = control
            .observe(
                &Uuid::from_u128(1).to_string(),
                &Uuid::from_u128(2).to_string(),
                "history",
                None,
            )
            .unwrap();
        assert_eq!(update.1, Phase::Unavailable);
        control.operations[0].phase = Phase::Waiting;
        let update = control
            .observe(
                &Uuid::from_u128(1).to_string(),
                &Uuid::from_u128(2).to_string(),
                "degraded",
                None,
            )
            .unwrap();
        assert_eq!(update.1, Phase::Unavailable);
    }
    #[test]
    fn stop_failure_preserves_terminal_liveness_and_never_confirms_another_generation() {
        let mut control = control();
        let session = Uuid::from_u128(1).to_string();
        let generation = Uuid::from_u128(2).to_string();
        assert!(
            control
                .observe(
                    &session,
                    &Uuid::from_u128(4).to_string(),
                    "running",
                    Some("refused")
                )
                .is_none()
        );
        let update = control
            .observe(&session, &generation, "running", Some("identity changed"))
            .unwrap();
        assert_eq!(update.1, Phase::Unavailable);
        assert_eq!(
            control.operations[0].error.as_deref(),
            Some("identity changed")
        );
        assert!(
            control
                .observe(&session, &generation, "running", Some("identity changed"))
                .is_none()
        );
        assert_eq!(
            control
                .observe(&session, &generation, "exited", None)
                .unwrap()
                .1,
            Phase::Complete
        );
        assert!(
            control
                .observe(&session, &generation, "running", Some("late error"))
                .is_none()
        );
        assert_eq!(control.operations[0].phase, Phase::Complete);
    }
    #[test]
    fn only_the_advertised_group_contract_changes_confirmation_scope() {
        assert!(scope_message(Some("pty_process_group_v1")).contains("группа процессов"));
        assert_eq!(scope_message(None), scope_message(Some("future_unknown")));
        assert!(scope_message(None).contains("не подтверждена"));
    }
    #[test]
    fn only_the_matching_receipt_is_consumed_once() {
        let mut control = control();
        assert!(control.take_reply(Some(Uuid::from_u128(9))).is_none());
        assert!(control.take_reply(Some(Uuid::from_u128(3))).is_some());
        assert!(control.take_reply(Some(Uuid::from_u128(3))).is_none());
        assert_eq!(control.operations[0].phase, Phase::Unknown);
        assert!(!can_stop("history", Some(42)));
        assert!(!can_stop("running", None));
        assert!(can_stop("degraded", Some(42)));
    }
}

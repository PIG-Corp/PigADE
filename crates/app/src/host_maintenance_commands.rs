//! Explicit local host inspection and idle-only replacement.
use super::*;
use host_maintenance::{Operation, Outcome};
use product::{Dialog, DialogKind, ProductAction as A};

impl CanvasApp {
    pub(super) fn host_maintenance_action(
        &mut self,
        action: A,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if action == A::OpenHostMaintenance {
            self.prefix = false;
            self.product.dialog = Some(Dialog::new(DialogKind::HostMaintenance));
        } else if !self
            .product
            .dialog
            .as_ref()
            .is_some_and(|dialog| dialog.kind == DialogKind::HostMaintenance)
        {
            return;
        }
        if self.product.maintenance.pending.is_some() {
            cx.notify();
            return;
        }
        let operation = match action {
            A::OpenHostMaintenance | A::InspectHost => Operation::Inspect,
            A::UpdateHost if self.product.maintenance.can_update() => Operation::Update,
            A::ReconcileHost
                if self.product.maintenance.managed
                    && self.product.maintenance.uncertain.is_some() =>
            {
                Operation::Reconcile
            }
            _ => return,
        };
        if operation == Operation::Update
            && (self.product.busy
                || self.product_launch_unknown
                || self.pending_launch.is_some()
                || self.focus_queue.pending().is_some()
                || self.focus_queue.is_saving()
                || self.git_flow.request_pending()
                || !self.git_flow.unresolved_resources().is_empty())
        {
            self.product.maintenance.error = Some(
                "Дождитесь результата текущей операции PigADE перед обновлением службы".into(),
            );
            cx.notify();
            return;
        }
        let expected = if operation == Operation::Reconcile {
            self.product.maintenance.uncertain.clone()
        } else {
            self.product.maintenance.inspection.clone()
        };
        if operation == Operation::Update {
            self.product.maintenance.uncertain = expected.clone();
        }
        if operation == Operation::Inspect {
            self.product.maintenance.inspection = None;
        }
        let request = uuid::Uuid::new_v4();
        self.product.maintenance.pending = Some((request, operation));
        self.product.maintenance.error = None;
        self.product.maintenance.message = None;
        let socket = self.host_socket.clone();
        let task = cx.background_spawn(async move {
            host_maintenance::perform(&socket, operation, expected.as_ref())
        });
        cx.spawn_in(window, async move |weak, cx| {
            let result = task.await;
            let _ = weak.update_in(cx, |this, _, cx| {
                if this.product.maintenance.pending != Some((request, operation)) { return; }
                this.product.maintenance.pending = None;
                let message = match result {
                    Ok(Outcome::Inspected(inspection)) => {
                        this.product.maintenance.inspection = Some(inspection);
                        "Состояние службы проверено".to_owned()
                    }
                    Ok(Outcome::Restarted(inspection)) => {
                        let same_binary = inspection.same_binary;
                        this.product.maintenance.inspection = Some(inspection);
                        this.product.maintenance.uncertain = None;
                        this.providers_checked = false;
                        this.product.providers = None;
                        this.product.provider_error = None;
                        this.product.providers_local = false;
                        this.product_command(json!({"command":"catalog"}), cx);
                        if same_binary { "Служба обновлена. Сохранённые панели доступны; CLI не запускались".into() }
                        else { "Служба подключена, но работает другая сборка. Проверьте состояние".into() }
                    }
                    Ok(Outcome::NotStopped {message, definitive}) => {
                        if definitive {
                            this.product.maintenance.uncertain = None;
                            this.product.maintenance.inspection = None;
                        }
                        let safe: String = message.chars().filter(|c| !c.is_control()).take(256).collect();
                        if definitive { format!("Обновление не выполнено: {safe}. Обновите состояние") }
                        else { "Завершение службы не подтверждено. «Проверить исход» только сверит процесс; повторной остановки не будет".into() }
                    }
                    Err(error) => {
                        let safe: String = error.chars().filter(|c| !c.is_control()).take(256).collect();
                        let message = format!("Не удалось подтвердить состояние службы: {safe}");
                        this.product.maintenance.error = Some(message.clone());
                        message
                    }
                };
                this.product.maintenance.message = Some(message.clone());
                if !this.product.dialog.as_ref().is_some_and(|dialog| dialog.kind == DialogKind::HostMaintenance) {
                    this.product.notice = Some(message);
                }
                cx.notify();
            });
        }).detach();
        cx.notify();
    }
}

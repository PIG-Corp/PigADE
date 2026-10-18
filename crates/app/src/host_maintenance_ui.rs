//! Build identity and explicit idle-only host replacement, within local settings.
use super::*;
use crate::host_maintenance::Operation;

impl ProductUi {
    pub(super) fn host_maintenance_fields(
        &self,
        mut fields: Div,
        cx: &mut Context<CanvasApp>,
    ) -> Div {
        let model = &self.maintenance;
        let colors = self.colors();
        fields = fields.child(
            div()
                .text_size(px(12.))
                .child(format!("Интерфейс · {}", env!("CARGO_PKG_VERSION"))),
        );
        if let Some(value) = &model.inspection {
            fields =
                fields
                    .child(div().text_size(px(14.)).child(if value.same_binary {
                        "Служба использует установленную сборку"
                    } else {
                        "Служба работает на другой сборке"
                    }))
                    .child(
                        div()
                            .text_size(px(11.))
                            .text_color(colors.muted)
                            .child(format!(
                                "Host · {} · PID {}",
                                value.status.as_ref().map_or(
                                    "старая версия протокола",
                                    |status| status.version.as_str()
                                ),
                                value.peer.pid
                            )),
                    )
                    .child(div().text_size(px(12.)).child(match value.active_sessions {
                        Some(count) => format!("Работающих сессий: {count}"),
                        None => "Число работающих сессий не подтверждено".into(),
                    }));
            if value.legacy {
                fields = fields.child(div().text_size(px(12.)).whitespace_normal().child(
                    "Эта версия host не поддерживает безопасное обновление из интерфейса. Работающие CLI сохраняются. Новые функции могут быть недоступны до плановой замены службы."));
            } else if let Some(status) = &value.status {
                for (show, text) in [
                    (status.active_sessions > 0, "Сначала завершите CLI внутри их терминалов. Закрытие панели оставляет процесс работающим.".to_owned()),
                    (status.unresolved_launches > 0 && status.active_sessions == 0, format!("Незавершённых запусков: {}. Их исход нужно выяснить до обновления.", status.unresolved_launches)),
                    (status.git_busy, "Ожидается завершение операции Git.".into()),
                    (status.resume_busy, "Проверяется продолжение диалога CLI.".into()),
                    (!status.can_shutdown && status.active_sessions == 0 && status.unresolved_launches == 0 && !status.git_busy && !status.resume_busy,
                        "Служба не подтвердила готовность к обновлению. Проверьте ошибки хранения в сессиях.".into()),
                ] {
                    if show { fields = fields.child(div().text_size(px(12.)).whitespace_normal().child(text)); }
                }
            }
        }
        if !model.managed {
            fields = fields.child(div().text_size(px(12.)).text_color(colors.muted).whitespace_normal()
                .child("Окно подключено к явно заданному адресу. Управление этой службой остаётся у запустившего её приложения."));
        }
        if let Some((_, operation)) = model.pending {
            fields = fields.child(
                div()
                    .id("host-maintenance-progress")
                    .role(Role::Status)
                    .text_size(px(12.))
                    .child(match operation {
                        Operation::Inspect => "Проверяем службу…",
                        Operation::Update => {
                            "Проверяем отсутствие работы и подключаем новую службу…"
                        }
                        Operation::Reconcile => "Сверяем исход предыдущей попытки…",
                    }),
            );
        }
        if let Some(message) = model.error.as_ref().or(model.message.as_ref()) {
            fields = fields.child(
                div()
                    .id("host-maintenance-result")
                    .role(Role::Status)
                    .text_size(px(12.))
                    .text_color(colors.accent)
                    .whitespace_normal()
                    .child(message.clone()),
            );
        }
        if model.pending.is_none() {
            let mut actions = div().flex().flex_col().gap_2();
            actions = actions.child(self.action_control(
                "host-maintenance-inspect".into(),
                "Обновить состояние · Ctrl+R".into(),
                ProductAction::InspectHost,
                ControlStyle {
                    role: Role::Button,
                    selected: false,
                    compact: true,
                },
                cx,
            ));
            if model.uncertain.is_some() && model.managed {
                actions = actions.child(self.action_control(
                    "host-maintenance-reconcile".into(),
                    "Проверить исход · Ctrl+Enter".into(),
                    ProductAction::ReconcileHost,
                    ControlStyle {
                        role: Role::Button,
                        selected: true,
                        compact: true,
                    },
                    cx,
                ));
            } else if model.can_update() {
                actions = actions.child(self.action_control(
                    "host-maintenance-update".into(),
                    "Обновить службу · Ctrl+Enter".into(),
                    ProductAction::UpdateHost,
                    ControlStyle {
                        role: Role::Button,
                        selected: true,
                        compact: true,
                    },
                    cx,
                ));
            }
            fields = fields.child(actions);
        }
        fields.child(div().text_size(px(11.)).text_color(colors.muted).whitespace_normal().child(
            "Обновление выполняется только по вашему действию и без работающих CLI. Проекты, раскладки и история сохраняются; агенты и предыдущие задания автоматически не запускаются."))
    }
}

//! Workspace Git snapshot, diff and explicit index-action controls.
use super::*;

#[cfg(test)]
use crate::workspace_git::diff_line_count;
use crate::workspace_git::{DIFF_PAGE_LINES, MAX_DIFF_LINES, diff_page_count};

fn safe_path(value: &str, limit: usize) -> String {
    let mut result = String::new();
    let mut chars = value.chars();
    for ch in chars.by_ref().take(limit) {
        if ch.is_control() {
            result.extend(ch.escape_default());
        } else {
            result.push(ch);
        }
    }
    if chars.next().is_some() {
        result.push('…');
    }
    if result.is_empty() {
        "∅".into()
    } else {
        result
    }
}

fn status_word(status: char) -> &'static str {
    match status {
        ' ' | '.' => "без изменений",
        'M' => "изменено",
        'A' => "добавлено",
        'D' => "удалено",
        'R' => "переименовано",
        'C' => "скопировано",
        'T' => "тип изменён",
        'U' => "конфликт",
        '?' => "неотслеживается",
        _ => "изменение",
    }
}

fn grouped_entries(
    entries: &[protocol::WorkspaceGitEntry],
) -> [Vec<&protocol::WorkspaceGitEntry>; 4] {
    let mut groups: [Vec<&protocol::WorkspaceGitEntry>; 4] = std::array::from_fn(|_| Vec::new());
    for entry in entries {
        if entry.unmerged {
            groups[3].push(entry);
            continue;
        }
        if entry.untracked {
            groups[2].push(entry);
            continue;
        }
        if !matches!(entry.index_status, ' ' | '.' | '?') {
            groups[0].push(entry);
        }
        if !matches!(entry.worktree_status, ' ' | '.' | '?') {
            groups[1].push(entry);
        }
    }
    groups
}

pub(super) fn ordered_entries(
    snapshot: &protocol::WorkspaceGitSnapshot,
) -> Vec<(protocol::GitDiffSide, &protocol::WorkspaceGitEntry)> {
    let [staged, unstaged, untracked, conflicts] = grouped_entries(&snapshot.entries);
    staged
        .into_iter()
        .map(|entry| (protocol::GitDiffSide::Index, entry))
        .chain(
            unstaged
                .into_iter()
                .map(|entry| (protocol::GitDiffSide::Worktree, entry)),
        )
        .chain(
            untracked
                .into_iter()
                .map(|entry| (protocol::GitDiffSide::Untracked, entry)),
        )
        .chain(
            conflicts
                .into_iter()
                .map(|entry| (protocol::GitDiffSide::Conflict, entry)),
        )
        .collect()
}

fn diff_side_label(side: protocol::GitDiffSide) -> &'static str {
    match side {
        protocol::GitDiffSide::Index => "Индекс",
        protocol::GitDiffSide::Worktree => "Рабочая папка",
        protocol::GitDiffSide::Untracked => "Новый файл",
        protocol::GitDiffSide::Conflict => "Конфликт",
    }
}

fn diff_comparison_label(side: protocol::GitDiffSide) -> &'static str {
    match side {
        protocol::GitDiffSide::Index => "HEAD → индекс",
        protocol::GitDiffSide::Worktree => "Индекс → рабочая папка",
        protocol::GitDiffSide::Untracked => "Пустой текст → файл",
        protocol::GitDiffSide::Conflict => "Пустой текст → рабочая копия конфликта",
    }
}

fn safe_diff_line(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for ch in value.chars() {
        if ch.is_control() && ch != '\t' {
            escaped.extend(ch.escape_default());
        } else {
            escaped.push(ch);
        }
    }
    escaped
}

fn visible_diff_line(value: &str) -> String {
    let mut visible = String::with_capacity(value.len());
    for ch in safe_diff_line(value).chars() {
        match ch {
            ' ' => visible.push('\u{00a0}'),
            '\t' => visible.push_str("\u{00a0}\u{00a0}\u{00a0}\u{00a0}"),
            _ => visible.push(ch),
        }
    }
    visible
}

fn diff_page(text: &str, page: usize) -> Vec<String> {
    text.lines()
        .skip(page.saturating_mul(DIFF_PAGE_LINES))
        .take(DIFF_PAGE_LINES)
        .map(safe_diff_line)
        .collect()
}

fn git_entry_row(
    entry: &protocol::WorkspaceGitEntry,
    side: protocol::GitDiffSide,
    colors: ProductColors,
    ordinal: usize,
    selected: bool,
    cx: &mut Context<CanvasApp>,
) -> Stateful<Div> {
    let path = entry.path.clone();
    let mut title = safe_path(&entry.path, 180);
    if let Some(original) = &entry.original_path {
        title.push_str("  ←  ");
        title.push_str(&safe_path(original, 180));
    }
    let status = if entry.unmerged {
        "Конфликт".to_owned()
    } else if entry.untracked {
        "Неотслеживаемый".to_owned()
    } else {
        format!(
            "Индекс: {} · Рабочая папка: {}",
            status_word(entry.index_status),
            status_word(entry.worktree_status)
        )
    };
    div()
        .id(format!("workspace-git-entry-{ordinal}"))
        .role(Role::Button)
        .aria_label(format!("{title}; {status}"))
        .aria_selected(selected)
        .w_full()
        .flex()
        .items_center()
        .justify_between()
        .gap_3()
        .rounded(px(5.))
        .bg(if selected {
            colors.selected
        } else {
            colors.base
        })
        .px(px(9.))
        .py(px(6.))
        .child(
            div()
                .text_size(px(12.))
                .text_color(colors.text)
                .truncate()
                .child(title),
        )
        .child(
            div()
                .text_size(px(10.))
                .text_color(colors.muted)
                .truncate()
                .child(status),
        )
        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
            this.product_action(ProductAction::GitSelect(ordinal), window, cx);
            this.product_action(ProductAction::GitOpen(path.clone(), side), window, cx);
        }))
}

impl ProductUi {
    fn git_watch_notice(&self, sheet: &crate::workspace_git::GitSheet) -> Option<String> {
        let view = self.git_watch_view.as_ref().filter(|view| {
            view.workspace == sheet.target.workspace
                && view.revision == sheet.target.revision
                && view.project == sheet.target.project
                && view.cwd == sheet.target.cwd
        })?;
        if view.stale || view.stamp() != sheet.watch_stamp {
            Some("В репозитории обнаружены изменения или актуальность не подтверждена. Ниже сохранённый снимок; Ctrl+R обновит его. Проверяемое действие не повторяется автоматически.".into())
        } else if view.unavailable || view.truncated {
            Some("Автообновление Git недоступно или охватывает не весь репозиторий. Обновляйте снимок вручную (Ctrl+R).".into())
        } else {
            None
        }
    }

    fn git_watch_banner(&self, sheet: &crate::workspace_git::GitSheet) -> Option<AnyElement> {
        self.git_watch_notice(sheet).map(|notice| {
            div()
                .id("workspace-git-watch-notice")
                .role(Role::Status)
                .text_size(px(11.))
                .text_color(self.colors().status_amber)
                .whitespace_normal()
                .child(notice)
                .into_any_element()
        })
    }

    fn git_index_banner(
        &self,
        notice: &crate::workspace_git::IndexNotice,
        cx: &mut Context<CanvasApp>,
    ) -> AnyElement {
        let colors = self.colors();
        let (label, detail) = match notice.state.phase {
            crate::workspace_git::IndexPhase::Ready => (
                "Индекс готов",
                notice
                    .state
                    .message
                    .as_deref()
                    .unwrap_or("Операция ещё не запущена."),
            ),
            crate::workspace_git::IndexPhase::Applying => {
                ("Изменяем индекс…", "Дождитесь результата операции.")
            }
            crate::workspace_git::IndexPhase::Applied => {
                ("Индекс обновлён", "Изменение индекса подтверждено.")
            }
            crate::workspace_git::IndexPhase::Rejected => (
                "Операция с индексом отклонена",
                notice
                    .state
                    .message
                    .as_deref()
                    .unwrap_or("Снимок оставлен без изменений."),
            ),
            crate::workspace_git::IndexPhase::Unknown => (
                "Результат операции неизвестен",
                notice
                    .state
                    .message
                    .as_deref()
                    .unwrap_or("Проверьте результат перед новой операцией."),
            ),
            crate::workspace_git::IndexPhase::Unavailable => (
                "Операция с индексом недоступна",
                notice
                    .state
                    .message
                    .as_deref()
                    .unwrap_or("Обновите снимок Git."),
            ),
        };
        let mut banner = div()
            .id("workspace-git-index-banner")
            .role(Role::Group)
            .aria_label("Результат операции с индексом")
            .w_full()
            .flex()
            .flex_col()
            .gap(px(4.))
            .rounded(px(6.))
            .border_1()
            .border_color(colors.border)
            .bg(colors.base)
            .p(px(8.))
            .child(
                div()
                    .id("workspace-git-index-banner-label")
                    .role(Role::Status)
                    .text_size(px(11.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(
                        if matches!(
                            notice.state.phase,
                            crate::workspace_git::IndexPhase::Rejected
                                | crate::workspace_git::IndexPhase::Unknown
                                | crate::workspace_git::IndexPhase::Unavailable
                        ) {
                            colors.accent
                        } else {
                            colors.text
                        },
                    )
                    .child(label),
            )
            .child(
                div()
                    .text_size(px(10.))
                    .text_color(colors.muted)
                    .whitespace_normal()
                    .child(format!(
                        "{} · {} · {}",
                        safe_path(&notice.path, 140),
                        safe_path(&notice.workspace, 100),
                        safe_path(detail, 220)
                    )),
            );
        if matches!(
            notice.state.phase,
            crate::workspace_git::IndexPhase::Unknown
        ) {
            let inspect = self.action_control(
                "workspace-git-index-banner-inspect".into(),
                "Проверить результат".into(),
                ProductAction::GitInspectIndex,
                ControlStyle {
                    role: Role::Button,
                    selected: false,
                    compact: true,
                },
                cx,
            );
            banner = banner.child(inspect);
        }
        if matches!(
            notice.state.phase,
            crate::workspace_git::IndexPhase::Unknown
                | crate::workspace_git::IndexPhase::Unavailable
        ) {
            banner = banner.child(self.action_control(
                "workspace-git-index-banner-dismiss".into(),
                "Понятно, обновить".into(),
                ProductAction::GitDismissIndexOutcome,
                ControlStyle {
                    role: Role::Button,
                    selected: false,
                    compact: true,
                },
                cx,
            ));
        }
        banner.into_any_element()
    }

    pub(crate) fn git_entry_count(&self) -> usize {
        self.git_sheet
            .as_ref()
            .and_then(|sheet| sheet.snapshot.as_ref())
            .map_or(0, |snapshot| ordered_entries(snapshot).len())
    }

    pub(crate) fn git_selected_target(&self) -> Option<(String, protocol::GitDiffSide)> {
        let sheet = self.git_sheet.as_ref()?;
        let snapshot = sheet.snapshot.as_ref()?;
        ordered_entries(snapshot)
            .get(sheet.selected)
            .map(|(side, entry)| (entry.path.clone(), *side))
    }

    pub(crate) fn git_selected_scroll_index(&self) -> usize {
        let Some(sheet) = self.git_sheet.as_ref() else {
            return 0;
        };
        let Some(snapshot) = sheet.snapshot.as_ref() else {
            return if sheet.loading || sheet.error.is_some() {
                1
            } else {
                0
            };
        };
        let [staged, unstaged, untracked, conflicts] = grouped_entries(&snapshot.entries);
        let groups = [staged, unstaged, untracked, conflicts];
        let mut offset = 1; // snapshot branch heading
        if sheet.last_index.is_some() {
            offset += 1;
        }
        if snapshot.index_operation_unavailable_reason.is_some() {
            offset += 1;
        }
        if sheet.loading {
            offset += 1;
        }
        if sheet.error.is_some() {
            offset += 1;
        }
        let mut selected = sheet.selected;
        for group in groups {
            if group.is_empty() {
                continue;
            }
            offset += 1; // group heading
            if selected < group.len() {
                return offset + selected;
            }
            selected -= group.len();
            offset += group.len();
        }
        offset
    }

    pub(super) fn git_overlay(&self, cx: &mut Context<CanvasApp>) -> AnyElement {
        let colors = self.colors();
        let Some(sheet) = &self.git_sheet else {
            return div()
                .id("workspace-git-unavailable")
                .role(Role::Dialog)
                .child("Снимок Git недоступен. Закройте окно и откройте его снова.")
                .into_any_element();
        };
        if !self.dialog.as_ref().is_some_and(|dialog| {
            dialog.kind == DialogKind::WorkspaceGit && dialog.id == sheet.dialog
        }) {
            return div()
                .id("workspace-git-stale")
                .role(Role::Dialog)
                .child("Этот снимок Git уже не относится к открытому окну. Закройте его и откройте снова.")
                .into_any_element();
        }
        if let Some(diff) = &sheet.diff {
            return self.git_diff_overlay(sheet, diff, cx);
        }

        let selected = |index| {
            self.dialog.as_ref().is_some_and(|dialog| {
                dialog.kind == DialogKind::WorkspaceGit
                    && dialog.id == sheet.dialog
                    && dialog.active_field == index
            })
        };
        let refresh_label = if sheet.loading {
            "Обновление…"
        } else {
            "Обновить снимок"
        };
        let refresh =
            if sheet.last_index.as_ref().is_some_and(|notice| {
                notice.state.phase == crate::workspace_git::IndexPhase::Applying
            }) {
                div()
                    .id("workspace-git-refresh-pending")
                    .role(Role::Status)
                    .text_size(px(10.))
                    .text_color(colors.muted)
                    .child("Обновление доступно после завершения операции")
                    .into_any_element()
            } else {
                self.action_control(
                    "workspace-git-refresh".into(),
                    refresh_label.into(),
                    ProductAction::RefreshWorkspaceGit,
                    ControlStyle {
                        role: Role::Button,
                        selected: selected(0),
                        compact: true,
                    },
                    cx,
                )
            };
        let close = self.action_control(
            "workspace-git-close".into(),
            "Закрыть".into(),
            ProductAction::CloseDialog,
            ControlStyle {
                role: Role::Button,
                selected: selected(1),
                compact: true,
            },
            cx,
        );
        let can_commit = sheet.can_prepare_commit();
        let commit = can_commit.then(|| {
            self.action_control(
                "workspace-git-commit".into(),
                "Создать коммит…".into(),
                ProductAction::GitPrepareCommit,
                ControlStyle {
                    role: Role::Button,
                    selected: selected(2),
                    compact: true,
                },
                cx,
            )
        });

        let mut batch_buttons = Vec::new();
        for (action, id, label) in [
            (
                protocol::GitIndexAction::Stage,
                "git-stage-all",
                "Подготовить всё…",
            ),
            (
                protocol::GitIndexAction::Unstage,
                "git-unstage-all",
                "Снять всё…",
            ),
        ] {
            if sheet.can_prepare_batch(action) && !self.git_batch.unresolved() {
                let action = ProductAction::GitPrepareBatch(action);
                batch_buttons.push(self.action_control(
                    id.into(),
                    label.into(),
                    action.clone(),
                    ControlStyle {
                        role: Role::Button,
                        selected: sheet.focused_action() == Some(action),
                        compact: true,
                    },
                    cx,
                ));
            }
        }
        if self.git_batch.operation.is_some() {
            batch_buttons.push(self.action_control(
                "git-batch-outcome".into(),
                "Пакет: результат".into(),
                ProductAction::GitShowBatchOutcome,
                ControlStyle {
                    role: Role::Button,
                    selected: false,
                    compact: true,
                },
                cx,
            ));
        }
        let mut body = div()
            .id("workspace-git-entry-list")
            .role(Role::List)
            .aria_label("Изменения в рабочей папке")
            .flex()
            .flex_col()
            .gap_1()
            .mt(px(8.))
            .min_h(px(0.))
            .max_h(px(440.))
            .flex_shrink(1.)
            .overflow_y_scroll()
            .track_scroll(&self.git_scroll);

        body = body.children(self.git_watch_banner(sheet));
        if let Some(notice) = &sheet.last_index {
            body = body.child(self.git_index_banner(notice, cx));
        }
        if let Some(result) = &sheet.commit_operation {
            let (label, detail) = match result.phase {
                protocol::GitCommitPhase::Running => (
                    "Коммит выполняется…",
                    "Ожидаем сохранённую квитанцию host-а.".to_owned(),
                ),
                protocol::GitCommitPhase::Applied => (
                    "Коммит создан",
                    format!(
                        "OID {}",
                        result.commit.as_deref().unwrap_or("не предоставлен")
                    ),
                ),
                protocol::GitCommitPhase::Failed | protocol::GitCommitPhase::Rejected => (
                    "Коммит не создан",
                    result
                        .message
                        .clone()
                        .unwrap_or_else(|| "Операция завершилась ошибкой.".into()),
                ),
                protocol::GitCommitPhase::Unknown => (
                    "Исход неизвестен",
                    result.message.clone().unwrap_or_else(|| {
                        "Проверьте сохранённый результат. Новый коммит не отправляется.".into()
                    }),
                ),
                protocol::GitCommitPhase::Unavailable => (
                    "Результат недоступен",
                    result
                        .message
                        .clone()
                        .unwrap_or_else(|| "Подтвердите, что вы просмотрели это состояние.".into()),
                ),
            };
            let mut banner = div()
                .id("workspace-git-commit-banner")
                .role(Role::Status)
                .w_full()
                .flex()
                .flex_col()
                .gap(px(4.))
                .rounded(px(6.))
                .border_1()
                .border_color(colors.border)
                .bg(colors.base)
                .p(px(8.))
                .child(
                    div()
                        .text_size(px(11.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(colors.accent)
                        .child(label),
                )
                .child(
                    div()
                        .text_size(px(10.))
                        .text_color(colors.muted)
                        .whitespace_normal()
                        .child(safe_path(&detail, 400)),
                );
            if result.phase == protocol::GitCommitPhase::Unknown {
                banner = banner.child(self.action_control(
                    "workspace-git-commit-inspect".into(),
                    "Проверить квитанцию".into(),
                    ProductAction::GitInspectCommit,
                    ControlStyle {
                        role: Role::Button,
                        selected: false,
                        compact: true,
                    },
                    cx,
                ));
            }
            if matches!(
                result.phase,
                protocol::GitCommitPhase::Unknown | protocol::GitCommitPhase::Unavailable
            ) {
                banner = banner.child(self.action_control(
                    "workspace-git-commit-ack".into(),
                    "Просмотрено".into(),
                    ProductAction::GitAcknowledgeCommit,
                    ControlStyle {
                        role: Role::Button,
                        selected: false,
                        compact: true,
                    },
                    cx,
                ));
            }
            body = body.child(banner);
        }
        if let Some(reason) = sheet
            .snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.index_operation_unavailable_reason.as_deref())
        {
            body = body.child(
                div()
                    .id("workspace-git-index-store-unavailable")
                    .role(Role::Status)
                    .text_size(px(11.))
                    .text_color(colors.muted)
                    .whitespace_normal()
                    .child(format!(
                        "Изменение индекса недоступно: {}",
                        safe_path(reason, 400)
                    )),
            );
        }
        if let Some(reason) = sheet
            .snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.index_batch_unavailable_reason.as_deref())
        {
            body = body.child(
                div()
                    .id("workspace-git-batch-unavailable")
                    .role(Role::Status)
                    .text_size(px(11.))
                    .text_color(colors.muted)
                    .whitespace_normal()
                    .child(format!(
                        "Пакетные действия недоступны: {}",
                        safe_path(reason, 400)
                    )),
            );
        }
        if let Some(reason) = sheet
            .snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.commit_unavailable_reason.as_deref())
        {
            body = body.child(
                div()
                    .id("workspace-git-commit-unavailable")
                    .role(Role::Status)
                    .text_size(px(11.))
                    .text_color(colors.muted)
                    .whitespace_normal()
                    .child(format!(
                        "Создание коммита недоступно: {}",
                        safe_path(reason, 400)
                    )),
            );
        }

        if sheet.loading {
            body = body.child(
                div()
                    .id("workspace-git-loading")
                    .role(Role::Status)
                    .text_size(px(11.))
                    .text_color(colors.accent)
                    .child(if sheet.snapshot.is_some() {
                        "Обновляем. Показанный ниже результат — предыдущий снимок."
                    } else {
                        "Читаем состояние Git…"
                    }),
            );
        }
        if let Some(error) = &sheet.error {
            let detail = safe_path(error, 300);
            body = body.child(
                div()
                    .id("workspace-git-error")
                    .role(Role::Status)
                    .text_size(px(11.))
                    .text_color(colors.accent)
                    .whitespace_normal()
                    .child(format!(
                        "{}{}",
                        if sheet.snapshot.is_some() {
                            "Не удалось обновить; ниже предыдущий снимок. "
                        } else {
                            "Не удалось получить снимок. "
                        },
                        detail
                    )),
            );
        }

        if let Some(snapshot) = &sheet.snapshot {
            let branch = if snapshot.detached {
                "Отключённая HEAD".to_owned()
            } else if let Some(branch) = snapshot.branch.as_deref() {
                if snapshot.head.is_none() && !snapshot.truncated {
                    format!("{} · Первый коммит ещё не создан", safe_path(branch, 120))
                } else {
                    safe_path(branch, 120)
                }
            } else {
                "Ветка неизвестна".into()
            };
            let commit = snapshot
                .head
                .as_deref()
                .map(|head| format!(" · {}", safe_path(head, 12)))
                .unwrap_or_default();
            body = body.child(
                div()
                    .id("workspace-git-branch")
                    .role(Role::Status)
                    .text_size(px(11.))
                    .text_color(colors.text)
                    .child(format!("{branch}{commit}")),
            );

            let [staged, unstaged, untracked, conflicts] = grouped_entries(&snapshot.entries);
            let has_changes = !(staged.is_empty()
                && unstaged.is_empty()
                && untracked.is_empty()
                && conflicts.is_empty());

            let mut ordinal = 0;
            for (group_index, (label, group)) in [
                ("В индексе", staged),
                ("Изменено в рабочей папке", unstaged),
                ("Неотслеживаемые", untracked),
                ("Конфликты", conflicts),
            ]
            .into_iter()
            .enumerate()
            {
                if group.is_empty() {
                    continue;
                }
                body = body.child(
                    div()
                        .id(format!("workspace-git-group-{group_index}"))
                        .role(Role::Heading)
                        .aria_level(3)
                        .mt(px(7.))
                        .text_size(px(11.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(colors.accent)
                        .child(format!("{label} · {}", group.len())),
                );
                for entry in group {
                    let side = match group_index {
                        0 => protocol::GitDiffSide::Index,
                        1 => protocol::GitDiffSide::Worktree,
                        2 => protocol::GitDiffSide::Untracked,
                        _ => protocol::GitDiffSide::Conflict,
                    };
                    let row_index = ordinal;
                    body = body.child(git_entry_row(
                        entry,
                        side,
                        colors,
                        row_index,
                        row_index == sheet.selected,
                        cx,
                    ));
                    ordinal += 1;
                }
            }

            if snapshot.truncated {
                body = body.child(
                    div()
                        .id("workspace-git-truncated")
                        .role(Role::Status)
                        .mt(px(8.))
                        .text_size(px(11.))
                        .text_color(colors.accent)
                        .whitespace_normal()
                        .child(
                            "Список ограничен. Часть файлов не показана; полный статус по этому снимку неизвестен.",
                        ),
                );
            } else if !has_changes {
                body = body.child(
                    div()
                        .id("workspace-git-clean")
                        .role(Role::Status)
                        .mt(px(8.))
                        .text_size(px(12.))
                        .text_color(colors.muted)
                        .child("Нет обнаруженных изменений в этом снимке."),
                );
            }
        } else if !sheet.loading && sheet.error.is_none() {
            body = body.child(
                div()
                    .id("workspace-git-empty")
                    .role(Role::Status)
                    .text_size(px(11.))
                    .text_color(colors.muted)
                    .child("Снимок Git пока не получен."),
            );
        }

        if let Some(notice) = &self.notice {
            body = body.child(
                div()
                    .id("workspace-git-notice")
                    .role(Role::Status)
                    .text_size(px(11.))
                    .text_color(colors.status_amber)
                    .whitespace_normal()
                    .child(safe_path(notice, 600)),
            );
        }

        let snapshot_meta = sheet
            .snapshot
            .as_ref()
            .map(|snapshot| {
                let cwd = snapshot.cwd.to_string_lossy();
                format!("Рабочая папка: {}", safe_path(cwd.as_ref(), 220))
            })
            .unwrap_or_else(|| {
                let cwd = sheet.target.cwd.to_string_lossy();
                format!("Рабочая папка: {}", safe_path(cwd.as_ref(), 220))
            });

        div()
            .id("workspace-git-overlay")
            .accessibility_id("workspace-git-overlay")
            .absolute()
            .inset_0()
            .flex()
            .items_start()
            .justify_center()
            .overflow_y_scroll()
            .py(px(20.))
            .bg(rgb(0x090b0e).opacity(0.72))
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                this.product_action(ProductAction::CloseDialog, window, cx);
            }))
            .child(
                div()
                    .id("workspace-git-dialog")
                    .role(Role::Dialog)
                    .aria_label("Состояние Git воркспейса")
                    .w_full()
                    .max_w(px(660.))
                    .max_h(gpui::relative(1.))
                    .mx(px(16.))
                    .flex()
                    .flex_col()
                    .rounded(px(12.))
                    .border_1()
                    .border_color(colors.border)
                    .bg(colors.raised)
                    .p(px(16.))
                    .text_color(colors.text)
                    .on_click(cx.listener(|_, _: &ClickEvent, _, cx| cx.stop_propagation()))
                    .child(
                        div()
                            .id("workspace-git-heading")
                            .role(Role::Heading)
                            .aria_level(2)
                            .text_size(px(16.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(format!("Git · {}", safe_path(&sheet.target.name, 120))),
                    )
                    .child(
                        div()
                            .id("workspace-git-snapshot-caption")
                            .role(Role::Status)
                            .mt(px(3.))
                            .text_size(px(11.))
                            .text_color(colors.muted)
                            .whitespace_normal()
                            .child("Снимок по запросу · PgUp/PgDn — прокрутка"),
                    )
                    .child(
                        div()
                            .id("workspace-git-path")
                            .text_size(px(10.))
                            .text_color(colors.muted)
                            .truncate()
                            .child(snapshot_meta),
                    )
                    .child(body)
                    .child(
                        div()
                            .id("workspace-git-actions")
                            .flex()
                            .flex_wrap()
                            .justify_end()
                            .gap_2()
                            .mt(px(10.))
                            .child(refresh)
                            .children(commit)
                            .children(batch_buttons)
                            .child(close),
                    ),
            )
            .into_any_element()
    }

    fn git_diff_overlay(
        &self,
        sheet: &crate::workspace_git::GitSheet,
        diff_sheet: &crate::workspace_git::DiffSheet,
        cx: &mut Context<CanvasApp>,
    ) -> AnyElement {
        let colors = self.colors();
        let mut body = div()
            .id("workspace-git-diff-body")
            .role(Role::Document)
            .aria_label("Текстовое сравнение снимка Git")
            .w_full()
            .max_h(px(420.))
            .min_h(px(100.))
            .overflow_y_scroll()
            .overflow_x_scroll()
            .track_scroll(&self.git_scroll)
            .font_family(crate::font::PRIMARY_FAMILY)
            .flex()
            .items_start()
            .flex_col()
            .gap(px(1.))
            .bg(colors.base)
            .border_1()
            .border_color(colors.border)
            .rounded(px(6.))
            .p(px(8.));

        body = body.children(self.git_watch_banner(sheet));

        if diff_sheet.loading {
            body = body.child(
                div()
                    .id("workspace-git-diff-loading")
                    .role(Role::Status)
                    .text_size(px(11.))
                    .text_color(colors.accent)
                    .child(if diff_sheet.result.is_some() {
                        "Обновляем сравнение. Ниже показан предыдущий ответ."
                    } else {
                        "Читаем сравнение снимка…"
                    }),
            );
        }
        if let Some(error) = &diff_sheet.error {
            body = body.child(
                div()
                    .id("workspace-git-diff-error")
                    .role(Role::Status)
                    .text_size(px(11.))
                    .text_color(colors.accent)
                    .whitespace_normal()
                    .child(format!(
                        "{}{}",
                        if diff_sheet.result.is_some() {
                            "Не удалось обновить; показан предыдущий ответ. "
                        } else {
                            "Не удалось получить сравнение. "
                        },
                        safe_path(error, 300)
                    )),
            );
        }

        if let Some(result) = &diff_sheet.result {
            let pages = diff_page_count(&result.text);
            let page = diff_sheet.page.min(pages - 1);
            if result.binary {
                body = body.child(
                    div()
                        .id("workspace-git-diff-binary")
                        .role(Role::Status)
                        .text_size(px(12.))
                        .text_color(colors.muted)
                        .child("Двоичный файл или текст не в UTF-8. Сравнение недоступно."),
                );
            } else if result.text.is_empty() {
                body = body.child(
                    div()
                        .id("workspace-git-diff-empty")
                        .role(Role::Status)
                        .text_size(px(12.))
                        .text_color(colors.muted)
                        .child("В этом снимке нет текстового сравнения для файла."),
                );
            } else {
                if result.truncated
                    || result.text.lines().take(MAX_DIFF_LINES + 1).count() > MAX_DIFF_LINES
                {
                    body = body.child(
                        div()
                            .id("workspace-git-diff-truncated")
                            .role(Role::Status)
                            .text_size(px(11.))
                            .text_color(colors.accent)
                            .child("Ответ сравнения ограничен; показан только доступный текст."),
                    );
                }
                let start = page.saturating_mul(DIFF_PAGE_LINES);
                for (index, line) in diff_page(&result.text, page).into_iter().enumerate() {
                    let color = if line.starts_with("+++") || line.starts_with("---") {
                        colors.muted
                    } else if line.starts_with('+') {
                        rgb(chrome::Colors::for_theme(self.appearance.theme).status_green).into()
                    } else if line.starts_with('-') {
                        rgb(match self.appearance.theme {
                            crate::appearance::Theme::Dark => 0xf28b82,
                            crate::appearance::Theme::Light => 0xa03232,
                        })
                        .into()
                    } else if line.starts_with("@@") || line.starts_with("diff ") {
                        colors.accent
                    } else {
                        colors.text
                    };
                    body = body.child(
                        div()
                            .id(format!("workspace-git-diff-line-{}", start + index))
                            .role(Role::ListItem)
                            .aria_label(line.clone())
                            .min_w_full()
                            .flex_shrink_0()
                            .text_size(px(11.))
                            .text_color(color)
                            .whitespace_nowrap()
                            .child(visible_diff_line(&line)),
                    );
                }
            }
        } else if !diff_sheet.loading && diff_sheet.error.is_none() {
            body = body.child(
                div()
                    .id("workspace-git-diff-empty-response")
                    .role(Role::Status)
                    .text_size(px(11.))
                    .text_color(colors.muted)
                    .child("Сравнение ещё не получено."),
            );
        }

        let path = diff_sheet
            .result
            .as_ref()
            .map(|result| result.path.as_str())
            .unwrap_or(diff_sheet.path.as_str());
        let side = diff_sheet
            .result
            .as_ref()
            .map(|result| result.side)
            .unwrap_or(diff_sheet.side);
        let header = format!("{} · {}", diff_side_label(side), safe_path(path, 300));
        let comparison = diff_comparison_label(side);
        let conflict_note = (side == protocol::GitDiffSide::Conflict).then_some(
            "Рабочая копия конфликта; сравнение с пустым текстом, не трёхсторонний merge.",
        );
        let back = self.action_control(
            "workspace-git-diff-back".into(),
            "К списку".into(),
            ProductAction::GitBack,
            ControlStyle {
                role: Role::Button,
                selected: false,
                compact: true,
            },
            cx,
        );
        let index_applying =
            diff_sheet.index_state.as_ref().is_some_and(|state| {
                matches!(state.phase, crate::workspace_git::IndexPhase::Applying)
            }) || sheet.last_index.as_ref().is_some_and(|notice| {
                notice.state.phase == crate::workspace_git::IndexPhase::Applying
            });
        let refresh = if index_applying {
            div()
                .id("workspace-git-diff-refresh-pending")
                .role(Role::Status)
                .text_size(px(10.))
                .text_color(colors.muted)
                .child("Обновление доступно после завершения операции")
                .into_any_element()
        } else {
            self.action_control(
                "workspace-git-diff-refresh".into(),
                if diff_sheet.index_state.as_ref().is_some_and(|state| {
                    matches!(state.phase, crate::workspace_git::IndexPhase::Applied)
                }) {
                    "Обновить после операции".into()
                } else {
                    "Обновить сравнение".into()
                },
                ProductAction::GitRefreshDiff,
                ControlStyle {
                    role: Role::Button,
                    selected: false,
                    compact: true,
                },
                cx,
            )
        };
        let copy = self.action_control(
            "workspace-git-diff-copy".into(),
            "Копировать снимок".into(),
            ProductAction::GitCopyDiff,
            ControlStyle {
                role: Role::Button,
                selected: false,
                compact: true,
            },
            cx,
        );
        let close = self.action_control(
            "workspace-git-diff-close".into(),
            "Закрыть".into(),
            ProductAction::CloseDialog,
            ControlStyle {
                role: Role::Button,
                selected: false,
                compact: true,
            },
            cx,
        );
        let index_state = diff_sheet.index_state.as_ref();
        let ticket_matches = diff_sheet
            .result
            .as_ref()
            .and_then(|result| result.index_ticket.as_ref())
            .zip(index_state.map(|state| &state.ticket))
            .is_some_and(|(result, state)| {
                let action_matches = match diff_sheet.side {
                    protocol::GitDiffSide::Index => {
                        result.action == protocol::GitIndexAction::Unstage
                    }
                    protocol::GitDiffSide::Worktree | protocol::GitDiffSide::Untracked => {
                        result.action == protocol::GitIndexAction::Stage
                    }
                    protocol::GitDiffSide::Conflict => false,
                };
                result == state && action_matches
            });
        let index_notice = if let Some(state) = index_state.filter(|state| {
            !sheet
                .last_index
                .as_ref()
                .is_some_and(|notice| notice.state.ticket == state.ticket)
        }) {
            let (label, message) = match &state.phase {
                crate::workspace_git::IndexPhase::Ready => (
                    "workspace-git-index-ready",
                    state
                        .message
                        .as_deref()
                        .unwrap_or("Действие для этого просмотра ещё не выполнено."),
                ),
                crate::workspace_git::IndexPhase::Applying => {
                    ("workspace-git-index-applying", "Изменяем индекс…")
                }
                crate::workspace_git::IndexPhase::Applied => (
                    "workspace-git-index-applied",
                    "Индекс обновлён. Снимок сравнения устарел; обновите его.",
                ),
                crate::workspace_git::IndexPhase::Rejected => (
                    "workspace-git-index-rejected",
                    state
                        .message
                        .as_deref()
                        .unwrap_or("Операция с индексом отклонена. Обновите сравнение."),
                ),
                crate::workspace_git::IndexPhase::Unknown => (
                    "workspace-git-index-unknown",
                    state
                        .message
                        .as_deref()
                        .unwrap_or("Результат операции неизвестен. Проверьте его перед повтором."),
                ),
                crate::workspace_git::IndexPhase::Unavailable => (
                    "workspace-git-index-unavailable",
                    state
                        .message
                        .as_deref()
                        .unwrap_or("Операция с индексом недоступна для этого снимка."),
                ),
            };
            Some((label, message.to_owned()))
        } else if index_state.is_some() {
            // The persistent banner above already owns this ticket's outcome.
            // Do not also claim that the host failed to provide a ticket.
            None
        } else {
            Some((
                "workspace-git-index-unavailable",
                diff_sheet
                    .result
                    .as_ref()
                    .and_then(|result| result.index_unavailable_reason.as_deref())
                    .unwrap_or("Этот host не предоставил безопасный билет для изменения индекса.")
                    .to_owned(),
            ))
        };
        let index_control = match index_state.map(|state| &state.phase) {
            Some(crate::workspace_git::IndexPhase::Ready)
                if ticket_matches
                    && !diff_sheet.loading
                    && diff_sheet.error.is_none()
                    && diff_sheet.side != protocol::GitDiffSide::Conflict
                    && !sheet.commit_operation.as_ref().is_some_and(|operation| {
                        matches!(
                            operation.phase,
                            protocol::GitCommitPhase::Running
                                | protocol::GitCommitPhase::Unknown
                                | protocol::GitCommitPhase::Unavailable
                        )
                    })
                    && !sheet.last_index.as_ref().is_some_and(|notice| {
                        matches!(
                            notice.state.phase,
                            crate::workspace_git::IndexPhase::Applying
                                | crate::workspace_git::IndexPhase::Unknown
                                | crate::workspace_git::IndexPhase::Unavailable
                        )
                    }) =>
            {
                Some(self.action_control(
                    "workspace-git-index-apply".into(),
                    match diff_sheet.side {
                        protocol::GitDiffSide::Index => "Убрать из индекса".into(),
                        protocol::GitDiffSide::Worktree | protocol::GitDiffSide::Untracked => {
                            "Подготовить файл".into()
                        }
                        protocol::GitDiffSide::Conflict => unreachable!(),
                    },
                    ProductAction::GitApplyIndex,
                    ControlStyle {
                        role: Role::Button,
                        selected: false,
                        compact: true,
                    },
                    cx,
                ))
            }
            Some(crate::workspace_git::IndexPhase::Unknown)
                if !sheet.last_index.as_ref().is_some_and(|notice| {
                    index_state.is_some_and(|state| notice.state.ticket == state.ticket)
                }) =>
            {
                Some(self.action_control(
                    "workspace-git-index-inspect".into(),
                    "Проверить результат".into(),
                    ProductAction::GitInspectIndex,
                    ControlStyle {
                        role: Role::Button,
                        selected: false,
                        compact: true,
                    },
                    cx,
                ))
            }
            _ => None,
        };
        let index_status = index_notice.map(|(id, message)| {
            div()
                .id(id)
                .role(Role::Status)
                .text_size(px(11.))
                .text_color(
                    if index_state.is_some_and(|state| {
                        matches!(
                            &state.phase,
                            crate::workspace_git::IndexPhase::Rejected
                                | crate::workspace_git::IndexPhase::Unknown
                        )
                    }) {
                        colors.accent
                    } else {
                        colors.muted
                    },
                )
                .whitespace_normal()
                .child(message)
                .into_any_element()
        });
        let pages = diff_sheet
            .result
            .as_ref()
            .map_or(1, |result| diff_page_count(&result.text));
        let page = diff_sheet.page.min(pages - 1);
        let previous = self.action_control(
            "workspace-git-diff-previous".into(),
            "Назад".into(),
            ProductAction::GitDiffPage(false),
            ControlStyle {
                role: Role::Button,
                selected: false,
                compact: true,
            },
            cx,
        );
        let next = self.action_control(
            "workspace-git-diff-next".into(),
            "Дальше".into(),
            ProductAction::GitDiffPage(true),
            ControlStyle {
                role: Role::Button,
                selected: false,
                compact: true,
            },
            cx,
        );
        div()
            .id("workspace-git-diff-overlay")
            .absolute()
            .inset_0()
            .flex()
            .items_start()
            .justify_center()
            .overflow_y_scroll()
            .py(px(20.))
            .bg(rgb(0x090b0e).opacity(0.72))
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                this.product_action(ProductAction::CloseDialog, window, cx);
            }))
            .child(
                div()
                    .id("workspace-git-diff-dialog")
                    .role(Role::Dialog)
                    .aria_label("Сравнение снимка Git")
                    .w_full()
                    .max_w(px(760.))
                    .max_h(gpui::relative(1.))
                    .mx(px(16.))
                    .flex()
                    .flex_col()
                    .gap_2()
                    .rounded(px(12.))
                    .border_1()
                    .border_color(colors.border)
                    .bg(colors.raised)
                    .p(px(16.))
                    .text_color(colors.text)
                    .on_click(cx.listener(|_, _: &ClickEvent, _, cx| cx.stop_propagation()))
                    .child(
                        div()
                            .id("workspace-git-diff-heading")
                            .role(Role::Heading)
                            .aria_level(2)
                            .text_size(px(16.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(format!("Git · {}", safe_path(&sheet.target.name, 100))),
                    )
                    .child(
                        div()
                            .id("workspace-git-diff-caption")
                            .role(Role::Status)
                            .text_size(px(11.))
                            .text_color(colors.muted)
                            .child("Ctrl+S — индекс · Ctrl+R — обновить / проверить · Ctrl+Shift+R — подтвердить просмотр исхода"),
                    )
                    .child(
                        div()
                            .id("workspace-git-diff-path")
                            .role(Role::Status)
                            .text_size(px(12.))
                            .text_color(colors.text)
                            .child(header),
                    )
                    .child(
                        div()
                            .id("workspace-git-diff-comparison")
                            .role(Role::Status)
                            .text_size(px(10.))
                            .text_color(colors.muted)
                            .child(comparison),
                    )
                    .when_some(sheet.last_index.as_ref(), |view, notice| {
                        view.child(self.git_index_banner(notice, cx))
                    })
                    .when_some(sheet.commit_operation.as_ref(), |view, result| {
                        let (label,detail)=match result.phase {
                            protocol::GitCommitPhase::Running=>("Коммит выполняется…","Ожидаем сохранённую квитанцию host-а.".to_owned()),
                            protocol::GitCommitPhase::Applied=>("Коммит создан",format!("OID {}",result.commit.as_deref().unwrap_or("не предоставлен"))),
                            protocol::GitCommitPhase::Failed|protocol::GitCommitPhase::Rejected=>("Коммит не создан",result.message.clone().unwrap_or_else(||"Операция завершилась ошибкой.".into())),
                            protocol::GitCommitPhase::Unknown=>("Исход неизвестен",result.message.clone().unwrap_or_else(||"Проверьте сохранённый результат. Новый коммит не отправляется.".into())),
                            protocol::GitCommitPhase::Unavailable=>("Результат недоступен",result.message.clone().unwrap_or_else(||"Подтвердите, что вы просмотрели это состояние.".into())),
                        };
                        let mut banner=div().id("workspace-git-commit-diff-banner").role(Role::Status).flex().flex_col().gap(px(4.)).rounded(px(6.)).border_1().border_color(colors.border).bg(colors.base).p(px(8.))
                            .child(div().text_size(px(11.)).font_weight(FontWeight::SEMIBOLD).text_color(colors.accent).child(label))
                            .child(div().text_size(px(10.)).text_color(colors.muted).whitespace_normal().child(safe_path(&detail,400)));
                        if result.phase==protocol::GitCommitPhase::Unknown { banner=banner.child(self.action_control("workspace-git-commit-diff-inspect".into(),"Проверить квитанцию".into(),ProductAction::GitInspectCommit,ControlStyle{role:Role::Button,selected:false,compact:true},cx)); }
                        if matches!(result.phase,protocol::GitCommitPhase::Unknown|protocol::GitCommitPhase::Unavailable) { banner=banner.child(self.action_control("workspace-git-commit-diff-ack".into(),"Просмотрено".into(),ProductAction::GitAcknowledgeCommit,ControlStyle{role:Role::Button,selected:false,compact:true},cx)); }
                        view.child(banner)
                    })
                    .when_some(conflict_note, |view, text| {
                        view.child(
                            div()
                                .id("workspace-git-conflict-note")
                                .role(Role::Status)
                                .text_size(px(11.))
                                .text_color(colors.accent)
                                .whitespace_normal()
                                .child(text),
                        )
                    })
                    .when_some(index_status, |view, status| view.child(status))
                    .child(body)
                    .child(div().id("workspace-git-diff-pages").flex().items_center().justify_between()
                        .child(if page > 0 && !diff_sheet.loading {
                            previous
                        } else {
                            div().id("workspace-git-diff-previous-disabled").role(Role::Status)
                                .text_size(px(11.)).text_color(colors.muted).child("Назад").into_any_element()
                        })
                        .child(div().id("workspace-git-diff-page-number").role(Role::Status).text_size(px(11.)).text_color(colors.muted)
                            .child(format!("Страница {} из {}", page + 1, pages)))
                        .child(if page + 1 < pages && !diff_sheet.loading {
                            next
                        } else {
                            div().id("workspace-git-diff-next-disabled").role(Role::Status)
                                .text_size(px(11.)).text_color(colors.muted).child("Дальше").into_any_element()
                        }))
                    .child(
                        div()
                            .id("workspace-git-diff-actions")
                            .flex()
                            .flex_wrap()
                            .justify_end()
                            .gap_2()
                            .children([back, refresh, copy, close])
                            .when_some(index_control, |actions, control| {
                                actions.child(control)
                            }),
                    ),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(
        path: &str,
        index_status: char,
        worktree_status: char,
        untracked: bool,
        unmerged: bool,
    ) -> protocol::WorkspaceGitEntry {
        protocol::WorkspaceGitEntry {
            path: path.into(),
            original_path: None,
            index_status,
            worktree_status,
            untracked,
            unmerged,
        }
    }

    #[test]
    fn status_groups_respect_porcelain_xy_and_exclusive_states() {
        let entries = vec![
            entry("staged.rs", 'M', '.', false, false),
            entry("unstaged.rs", '.', 'M', false, false),
            entry("both.rs", 'M', 'M', false, false),
            entry("new/", '?', '?', true, false),
            entry("conflict.rs", 'U', 'U', false, true),
        ];
        let [staged, unstaged, untracked, conflicts] = grouped_entries(&entries);
        let paths = |group: Vec<&protocol::WorkspaceGitEntry>| {
            group
                .into_iter()
                .map(|entry| entry.path.clone())
                .collect::<Vec<_>>()
        };

        assert_eq!(paths(staged), vec!["staged.rs", "both.rs"]);
        assert_eq!(paths(unstaged), vec!["unstaged.rs", "both.rs"]);
        assert_eq!(paths(untracked), vec!["new/"]);
        assert_eq!(paths(conflicts), vec!["conflict.rs"]);
        assert_eq!(status_word('.'), "без изменений");
        assert_eq!(status_word('T'), "тип изменён");
    }

    #[test]
    fn ordered_targets_keep_working_tree_versions_and_exclusive_groups() {
        let snapshot = protocol::WorkspaceGitSnapshot {
            workspace_id: uuid::Uuid::from_u128(1),
            workspace_revision: 2,
            project_id: uuid::Uuid::from_u128(3),
            cwd: "/tmp/project".into(),
            branch: Some("main".into()),
            head: None,
            detached: false,
            entries: vec![
                entry("both.rs", 'M', 'M', false, false),
                entry("staged.rs", 'A', ' ', false, false),
                entry("new.txt", '?', '?', true, false),
                entry("conflict.rs", 'U', 'U', false, true),
            ],
            truncated: false,
            index_operation: None,
            index_operation_unavailable_reason: None,
            index_batch_schema: None,
            index_batch_operation: None,
            index_batch_unavailable_reason: None,
            commit_schema: None,
            commit_operation: None,
            commit_unavailable_reason: None,
        };
        let rows = ordered_entries(&snapshot);
        assert_eq!(
            rows.iter()
                .map(|(side, entry)| (*side, entry.path.as_str()))
                .collect::<Vec<_>>(),
            vec![
                (protocol::GitDiffSide::Index, "both.rs"),
                (protocol::GitDiffSide::Index, "staged.rs"),
                (protocol::GitDiffSide::Worktree, "both.rs"),
                (protocol::GitDiffSide::Untracked, "new.txt"),
                (protocol::GitDiffSide::Conflict, "conflict.rs"),
            ]
        );
    }

    #[test]
    fn diff_view_pages_are_bounded_and_escape_terminal_controls() {
        let text = (0..401)
            .map(|line| {
                if line == 7 {
                    "\u{1b}[31mred".to_owned()
                } else {
                    format!("line-{line}")
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(diff_line_count(&text), 401);
        assert_eq!(diff_page(&text, 0).len(), DIFF_PAGE_LINES);
        assert_eq!(diff_page(&text, 1).len(), DIFF_PAGE_LINES);
        assert_eq!(diff_page(&text, 2), vec!["line-400"]);
        assert!(diff_page(&text, 3).is_empty());
        assert_eq!(diff_page(&text, 0)[7], "\\u{1b}[31mred");
        assert_eq!(
            visible_diff_line("  \tvalue"),
            "\u{00a0}\u{00a0}\u{00a0}\u{00a0}\u{00a0}\u{00a0}value"
        );
        assert_eq!(
            diff_line_count(&"x\n".repeat(MAX_DIFF_LINES + 4)),
            MAX_DIFF_LINES
        );
    }
}

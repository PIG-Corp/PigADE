//! Read-only workspace Git snapshot overlay.
use super::*;

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

fn git_entry_row(
    entry: &protocol::WorkspaceGitEntry,
    colors: ProductColors,
    ordinal: usize,
) -> Stateful<Div> {
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
        .role(Role::ListItem)
        .aria_label(format!("{title}; {status}"))
        .w_full()
        .flex()
        .items_center()
        .justify_between()
        .gap_3()
        .rounded(px(5.))
        .bg(colors.base)
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
}

impl ProductUi {
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
        let refresh = self.action_control(
            "workspace-git-refresh".into(),
            refresh_label.into(),
            ProductAction::RefreshWorkspaceGit,
            ControlStyle {
                role: Role::Button,
                selected: selected(0),
                compact: true,
            },
            cx,
        );
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
            let has_untracked = !untracked.is_empty();
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
                    body = body.child(git_entry_row(entry, colors, ordinal));
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
            if has_untracked {
                body = body.child(
                    div()
                        .id("workspace-git-untracked-note")
                        .role(Role::Status)
                        .text_size(px(10.))
                        .text_color(colors.muted)
                        .child("Неотслеживаемые каталоги могут отображаться одной записью."),
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
                            .justify_end()
                            .gap_2()
                            .mt(px(10.))
                            .child(refresh)
                            .child(close),
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
}

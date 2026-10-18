//! Read-only workspace Git snapshot overlay.
use super::*;

const DIFF_PAGE_LINES: usize = 200;
const MAX_DIFF_LINES: usize = 2000;

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

fn diff_line_count(text: &str) -> usize {
    text.lines()
        .take(MAX_DIFF_LINES + 1)
        .count()
        .min(MAX_DIFF_LINES)
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
            let total_lines = diff_line_count(&result.text);
            let pages = total_lines.div_ceil(DIFF_PAGE_LINES).max(1);
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
        let refresh = self.action_control(
            "workspace-git-diff-refresh".into(),
            "Обновить сравнение".into(),
            ProductAction::GitRefreshDiff,
            ControlStyle {
                role: Role::Button,
                selected: false,
                compact: true,
            },
            cx,
        );
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
        let lines = diff_sheet
            .result
            .as_ref()
            .map(|result| diff_line_count(&result.text))
            .unwrap_or(0);
        let pages = lines.div_ceil(DIFF_PAGE_LINES).max(1);
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
                            .child("Сравнение относится к последнему полученному снимку, а не к живому содержимому."),
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
                            .children([back, refresh, copy, close]),
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

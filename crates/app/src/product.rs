//! Native PigADE product chrome. The host owns commands and durable state;
//! this module renders the current catalog and emits typed user actions.

use crate::edit_field::FieldLayout;
use gpui::{
    AnyElement, ClickEvent, Context, Div, FontWeight, Hsla, IntoElement, ParentElement, Role,
    Stateful, StatefulInteractiveElement, Styled, div, prelude::InteractiveElement, px, rgb,
};
use pigade_storage::{Catalog, ContextDocument, Tab, Workspace, WorkspaceStatus};
use std::collections::{BTreeMap, HashSet};
use uuid::Uuid;

use super::{CanvasApp, protocol, text_edit::EditBuffer};

const SIDEBAR_WIDTH: f32 = 240.0;
const ROW_HEIGHT: f32 = 30.0;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum ProductAction {
    AddProject,
    CreateWorkspace,
    CreateTab,
    OpenTransfer,
    TransferTo(Uuid),
    OpenSessions,
    RefreshSessions,
    AttachSession(Uuid),
    RefreshLaunchPreview,
    ConfirmPreparedLaunch,
    ToggleMemory,
    ToggleContextDelivery,
    InspectSessionContext,
    PanCanvas,
    RevealFocusedPane,
    SelectProject(Uuid),
    SelectWorkspace(Uuid),
    SelectTab(Uuid),
    Launch(String),
    Layout(String),
    EditContext,
    EditProjectContext,
    OpenPalette,
    CloseDialog,
    SubmitDialog,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum DialogKind {
    AddProject,
    CreateWorkspace,
    NewTab,
    Palette,
    Context,
    ProjectContext,
    LaunchReview,
    SessionContext,
    Transfer,
    Sessions,
}

impl DialogKind {
    fn title(self) -> &'static str {
        match self {
            Self::AddProject => "Добавить проект",
            Self::CreateWorkspace => "Новый воркспейс",
            Self::NewTab => "Новая вкладка",
            Self::Palette => "Команды PigADE",
            Self::Context => "Контекст воркспейса",
            Self::ProjectContext => "Контекст проекта",
            Self::LaunchReview => "Проверка запуска",
            Self::SessionContext => "Контекст выбранной сессии",
            Self::Transfer => "Перенести панель",
            Self::Sessions => "Неразмещённые сессии",
        }
    }

    fn fields(self) -> &'static [&'static str] {
        match self {
            Self::AddProject => &["Путь к Git-проекту"],
            Self::CreateWorkspace => &["Задача", "Режим: worktree / shared"],
            Self::NewTab => &["Название вкладки"],
            Self::Palette => &["Поиск команды"],
            Self::Context => &["Инструкции", "Роль", "Память"],
            Self::ProjectContext => &["Инструкции проекта"],
            Self::LaunchReview => &["Роль для запуска", "Дополнительные инструкции агенту"],
            Self::SessionContext => &[],
            Self::Transfer => &[],
            Self::Sessions => &[],
        }
    }

    fn submit_label(self) -> &'static str {
        match self {
            Self::AddProject => "Добавить проект",
            Self::CreateWorkspace => "Создать воркспейс",
            Self::NewTab => "Создать вкладку",
            Self::Palette => "Закрыть",
            Self::Context => "Сохранить ревизию",
            Self::ProjectContext => "Сохранить ревизию",
            Self::LaunchReview => "Подтвердить и запустить",
            Self::SessionContext => "Закрыть",
            Self::Transfer => "Закрыть",
            Self::Sessions => "Закрыть",
        }
    }
}

#[derive(Clone, Debug)]
pub(super) struct Dialog {
    pub(super) kind: DialogKind,
    pub(super) id: Uuid,
    pub(super) fields: Vec<EditBuffer>,
    pub(super) active_field: usize,
}

impl Dialog {
    pub(super) fn new(kind: DialogKind) -> Self {
        Self {
            kind,
            id: Uuid::new_v4(),
            fields: match kind {
                DialogKind::CreateWorkspace => vec![EditBuffer::default(), "worktree".into()],
                _ => vec![EditBuffer::default(); kind.fields().len()],
            },
            active_field: 0,
        }
    }
}

#[derive(Default)]
pub(super) struct ProductUi {
    pub(super) catalog: Option<Catalog>,
    pub(super) context: Option<ContextDocument>,
    pub(super) sessions: Vec<SessionSummary>,
    pub(super) launch_preview: Option<protocol::LaunchPreview>,
    pub(super) launch_phase: Option<String>,
    pub(super) notice: Option<String>,
    pub(super) busy: bool,
    pub(super) dialog: Option<Dialog>,
}

pub(super) struct SessionSummary {
    session: String,
    title: String,
    state: String,
}

impl From<protocol::View> for SessionSummary {
    fn from(view: protocol::View) -> Self {
        Self {
            session: view.session,
            title: view.frame.title,
            state: view.state,
        }
    }
}

#[derive(Clone, Copy)]
struct ProductColors {
    base: Hsla,
    sidebar: Hsla,
    raised: Hsla,
    hover: Hsla,
    selected: Hsla,
    border: Hsla,
    text: Hsla,
    muted: Hsla,
    accent: Hsla,
}

#[derive(Clone, Copy)]
struct ControlStyle {
    role: Role,
    selected: bool,
    compact: bool,
}

impl ProductColors {
    fn dark() -> Self {
        Self {
            base: rgb(0x17191d).into(),
            sidebar: rgb(0x1d2025).into(),
            raised: rgb(0x252930).into(),
            hover: rgb(0x2b3038).into(),
            selected: rgb(0x293548).into(),
            border: rgb(0x373e49).into(),
            text: rgb(0xf2f4f7).into(),
            muted: rgb(0xb0b8c5).into(),
            accent: rgb(0x7dafff).into(),
        }
    }
}

fn status_label(status: WorkspaceStatus) -> Option<&'static str> {
    match status {
        WorkspaceStatus::Ready => None,
        WorkspaceStatus::Preparing => Some("Подготовка"),
        WorkspaceStatus::Failed => Some("Ошибка"),
        WorkspaceStatus::Archived => Some("Архив"),
        WorkspaceStatus::MissingPath => Some("Папка недоступна"),
    }
}

fn workspace_name(workspace: &Workspace) -> String {
    let name = if workspace.task.trim().is_empty() {
        "Без названия".to_owned()
    } else {
        workspace.task.clone()
    };
    if workspace.status == WorkspaceStatus::Failed
        && let Some(reason) = workspace
            .error
            .as_deref()
            .filter(|reason| !reason.is_empty())
    {
        return format!("{name} · {reason}");
    }
    status_label(workspace.status).map_or(name.clone(), |status| format!("{name} · {status}"))
}

fn selected_workspace(catalog: &Catalog) -> Option<Uuid> {
    catalog.selection.workspace_id
}

fn selected_tab(catalog: &Catalog) -> Option<Uuid> {
    catalog.selection.tab_id
}

fn placed_session_ids(catalog: Option<&Catalog>) -> HashSet<Uuid> {
    let mut ids = HashSet::new();
    for tab in catalog.into_iter().flat_map(|catalog| &catalog.tabs) {
        if let Ok(layout) = serde_json::from_str::<pigade_layout::TabLayout>(&tab.layout_json) {
            ids.extend(layout.panes());
        }
    }
    ids
}

fn session_state_label(state: &str) -> &'static str {
    match state {
        "running" => "Работает",
        "history" | "historical" => "История",
        "exited" => "Завершён",
        "degraded" => "Ошибка терминала",
        _ => "Исход неизвестен",
    }
}

fn capability_label(available: bool) -> &'static str {
    if available {
        "доступно"
    } else {
        "недоступно"
    }
}

fn palette_commands() -> Vec<(&'static str, ProductAction)> {
    vec![
        ("Добавить проект", ProductAction::AddProject),
        ("Новый воркспейс", ProductAction::CreateWorkspace),
        ("Контекст воркспейса · E", ProductAction::EditContext),
        (
            "Контекст проекта · Shift+E",
            ProductAction::EditProjectContext,
        ),
        ("Прокрутить canvas · G", ProductAction::PanCanvas),
        (
            "Показать выбранную панель",
            ProductAction::RevealFocusedPane,
        ),
        (
            "Контекст выбранной сессии",
            ProductAction::InspectSessionContext,
        ),
        ("Перенести панель", ProductAction::OpenTransfer),
        ("Сессии", ProductAction::OpenSessions),
        ("Новая вкладка", ProductAction::CreateTab),
        ("Открыть терминал", ProductAction::Launch("shell".into())),
        ("Запустить Claude", ProductAction::Launch("claude".into())),
        ("Запустить Codex", ProductAction::Launch("codex".into())),
        (
            "Запустить OpenCode",
            ProductAction::Launch("opencode".into()),
        ),
        (
            "Разделить вправо",
            ProductAction::Layout("split_right".into()),
        ),
        ("Разделить вниз", ProductAction::Layout("split_down".into())),
        ("Режим фокуса", ProductAction::Layout("focus".into())),
        ("Выровнять панели", ProductAction::Layout("equalize".into())),
        ("Отменить раскладку", ProductAction::Layout("undo".into())),
        ("Повторить раскладку", ProductAction::Layout("redo".into())),
        ("Скрыть панель", ProductAction::Layout("hide".into())),
    ]
}

fn context_lines(value: &str) -> Div {
    value.split('\n').fold(
        div().w_full().flex().flex_col().items_start(),
        |lines, line| {
            lines.child(
                div()
                    .w_full()
                    .whitespace_normal()
                    .child(if line.is_empty() {
                        " ".to_owned()
                    } else {
                        line.to_owned()
                    }),
            )
        },
    )
}

fn effective_context_blocks(
    context: &pigade_storage::EffectiveContext,
    id: &str,
    accessible_name: &str,
    max_height: f32,
    catalog: Option<&Catalog>,
) -> Stateful<Div> {
    let colors = ProductColors::dark();
    let mut blocks = div()
        .id(id.to_owned())
        .role(Role::List)
        .aria_label(accessible_name)
        .flex()
        .flex_col()
        .gap_2()
        .max_h(px(max_height))
        .overflow_y_scroll();
    if context.blocks.is_empty() {
        return blocks.child(
            div()
                .id(format!("{id}-empty"))
                .text_size(px(12.))
                .text_color(colors.muted)
                .child("Эффективный контекст пуст."),
        );
    }

    for (index, block) in context.blocks.iter().enumerate() {
        let source = match block.source.as_str() {
            "project_instructions" => "Инструкции проекта",
            "workspace_instructions" => "Инструкции воркспейса",
            "task" => "Задача",
            "workspace_default_role" | "role" => "Роль воркспейса",
            "agent_role_override" => "Роль этой сессии",
            "workspace_memory" => "Выбранная память",
            "agent_instructions" => "Инструкции этой сессии",
            other => other,
        };
        let scope = if block.scope_id == context.id {
            "Эта сессия".to_owned()
        } else {
            catalog
                .and_then(|catalog| {
                    catalog
                        .projects
                        .iter()
                        .find(|project| project.id == block.scope_id)
                        .map(|project| project.name.clone())
                        .or_else(|| {
                            catalog
                                .workspaces
                                .iter()
                                .find(|workspace| workspace.id == block.scope_id)
                                .map(workspace_name)
                        })
                })
                .unwrap_or_else(|| "Сохранённый источник".into())
        };
        let heading = format!("{source} · {scope} · ревизия {}", block.revision);
        blocks = blocks.child(
            div()
                .id(format!("{id}-block-{index}"))
                .role(Role::Group)
                .aria_label(heading.clone())
                .flex()
                .flex_col()
                .gap_1()
                .rounded(px(7.))
                .border_1()
                .border_color(colors.border)
                .bg(colors.base)
                .p(px(10.))
                .child(
                    div()
                        .text_size(px(11.))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(colors.accent)
                        .child(heading),
                )
                .child(
                    div()
                        .text_size(px(12.))
                        .text_color(colors.text)
                        .child(context_lines(&block.text)),
                ),
        );
    }
    blocks
}

fn launch_phase_label(phase: Option<&str>) -> &'static str {
    match phase {
        Some("prepared") => "Подготовлен, запуск ещё не подтверждён",
        Some("attempting") => "Попытка начата; подтверждения нет",
        Some("unknown") => "Исход запуска неизвестен",
        Some("delivered_unconfirmed") => "Параметры запуска переданы; применение не подтверждено",
        Some("not_requested") => "Контекст не запрашивался",
        _ => "Фаза запуска неизвестна",
    }
}

pub(super) fn dialog_field_multiline(kind: DialogKind, index: usize) -> bool {
    match kind {
        DialogKind::Context => index != 1,
        DialogKind::ProjectContext => true,
        DialogKind::LaunchReview => index == 1,
        _ => false,
    }
}

impl ProductUi {
    pub(super) fn palette_action(&self) -> Option<ProductAction> {
        let dialog = self.dialog.as_ref()?;
        if dialog.kind != DialogKind::Palette {
            return None;
        }
        let query = dialog.fields.first().map(EditBuffer::text).unwrap_or("");
        if query.trim().is_empty() {
            return None;
        }
        palette_commands()
            .into_iter()
            .find(|(label, _)| label.to_lowercase().contains(&query.to_lowercase()))
            .map(|(_, action)| action)
    }

    pub(super) fn apply_context_reply(
        &mut self,
        context: ContextDocument,
        saved: bool,
        requested: bool,
    ) {
        if !saved && !requested {
            return;
        }
        let project = self
            .catalog
            .as_ref()
            .is_some_and(|catalog| catalog.projects.iter().any(|p| p.id == context.scope_id));
        if !saved {
            let mut dialog = Dialog::new(if project {
                DialogKind::ProjectContext
            } else {
                DialogKind::Context
            });
            dialog.fields = if project {
                vec![context.instructions.clone().into()]
            } else {
                vec![
                    context.instructions.clone().into(),
                    context.role.clone().into(),
                    context.memory.clone().into(),
                ]
            };
            self.dialog = Some(dialog);
        } else {
            let dirty = self.dialog.as_ref().is_some_and(|dialog| {
                matches!(
                    dialog.kind,
                    DialogKind::Context | DialogKind::ProjectContext
                ) && (dialog
                    .fields
                    .first()
                    .is_some_and(|field| field.text() != context.instructions)
                    || (dialog.kind == DialogKind::Context
                        && (dialog
                            .fields
                            .get(1)
                            .is_some_and(|field| field.text() != context.role)
                            || dialog
                                .fields
                                .get(2)
                                .is_some_and(|field| field.text() != context.memory))))
            });
            self.notice = Some(format!(
                "Ревизия {} сохранена. {}Работающие сессии не изменены.",
                context.revision,
                if dirty {
                    "В редакторе есть новые несохранённые изменения. "
                } else {
                    ""
                }
            ));
        }
        self.context = Some(context);
    }

    pub(super) fn active_editor(&self) -> Option<&EditBuffer> {
        let dialog = self.dialog.as_ref()?;
        dialog.fields.get(dialog.active_field)
    }

    pub(super) fn active_editor_mut(&mut self) -> Option<&mut EditBuffer> {
        let dialog = self.dialog.as_mut()?;
        dialog.fields.get_mut(dialog.active_field)
    }

    pub(super) fn active_multiline(&self) -> bool {
        self.dialog
            .as_ref()
            .is_some_and(|dialog| dialog_field_multiline(dialog.kind, dialog.active_field))
    }

    pub(super) fn insert_text(&mut self, text: &str) {
        let multiline = self.active_multiline();
        if let Some(field) = self.active_editor_mut()
            && let Err(error) = field.replace(None, text, multiline)
        {
            self.notice = Some(format!("Текст не изменён: {error}"));
        }
    }

    pub(super) fn sidebar(&self, cx: &mut Context<CanvasApp>) -> impl IntoElement {
        let colors = ProductColors::dark();
        let mut project_tree = div()
            .id("product-project-tree")
            .role(Role::List)
            .aria_label("Проекты и воркспейсы")
            .flex()
            .flex_col()
            .gap_1()
            .overflow_y_scroll();

        if let Some(catalog) = &self.catalog {
            let active_workspace = selected_workspace(catalog);
            for project in &catalog.projects {
                project_tree = project_tree.child(self.action_control(
                    format!("project-{}", project.id),
                    project.name.clone(),
                    ProductAction::SelectProject(project.id),
                    ControlStyle {
                        role: Role::TreeItem,
                        selected: catalog.selection.project_id == Some(project.id),
                        compact: false,
                    },
                    cx,
                ));

                let mut has_workspace = false;
                for workspace in catalog
                    .workspaces
                    .iter()
                    .filter(|workspace| workspace.project_id == project.id)
                {
                    has_workspace = true;
                    project_tree = project_tree.child(self.action_control(
                        format!("workspace-{}", workspace.id),
                        workspace_name(workspace),
                        ProductAction::SelectWorkspace(workspace.id),
                        ControlStyle {
                            role: Role::TreeItem,
                            selected: active_workspace == Some(workspace.id),
                            compact: false,
                        },
                        cx,
                    ));
                }
                if !has_workspace {
                    project_tree = project_tree.child(
                        div()
                            .id(format!("project-empty-{}", project.id))
                            .pl(px(24.))
                            .py(px(3.))
                            .text_size(px(11.))
                            .text_color(colors.muted)
                            .child("Нет воркспейсов"),
                    );
                }
            }
            if catalog.projects.is_empty() {
                project_tree = project_tree.child(
                    div()
                        .id("empty-project-catalog")
                        .px(px(8.))
                        .py(px(10.))
                        .text_color(colors.muted)
                        .child("Добавьте Git-проект, чтобы начать работу."),
                );
            }
        } else {
            project_tree = project_tree.child(
                div()
                    .id("catalog-loading")
                    .px(px(8.))
                    .py(px(10.))
                    .text_color(colors.muted)
                    .child("Загрузка каталога…"),
            );
        }

        let mut controls = div().flex().flex_col().gap_1();
        controls = controls.child(self.action_control(
            "add-project".into(),
            "+  Добавить проект".into(),
            ProductAction::AddProject,
            ControlStyle {
                role: Role::Button,
                selected: false,
                compact: false,
            },
            cx,
        ));
        if self
            .catalog
            .as_ref()
            .is_some_and(|catalog| catalog.selection.project_id.is_some())
        {
            controls = controls.child(self.action_control_with_accessible_name(
                "edit-project-context".into(),
                "Контекст проекта".into(),
                "Просмотреть контекст выбранного проекта".into(),
                ProductAction::EditProjectContext,
                ControlStyle {
                    role: Role::Button,
                    selected: false,
                    compact: false,
                },
                cx,
            ));
        }
        controls = controls.child(self.action_control(
            "new-workspace".into(),
            "+  Новый воркспейс".into(),
            ProductAction::CreateWorkspace,
            ControlStyle {
                role: Role::Button,
                selected: false,
                compact: false,
            },
            cx,
        ));

        let mut sidebar = div()
            .id("product-sidebar")
            .accessibility_id("product-sidebar")
            .role(Role::Group)
            .aria_label("Проекты")
            .w(px(SIDEBAR_WIDTH))
            .min_w(px(180.))
            .max_w(px(360.))
            .h_full()
            .flex()
            .flex_col()
            .bg(colors.sidebar)
            .border_r_1()
            .border_color(colors.border)
            .px(px(10.))
            .py(px(12.));
        sidebar = sidebar.child(
            div()
                .h(px(28.))
                .px(px(8.))
                .flex()
                .items_center()
                .text_size(px(11.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(colors.muted)
                .child("ПРОЕКТЫ"),
        );
        sidebar = sidebar.child(project_tree.flex_1().min_h_0());
        sidebar = sidebar.child(div().h(px(1.)).my(px(8.)).bg(colors.border));
        sidebar = sidebar.child(controls);
        if self.busy || self.notice.is_some() {
            let message = self
                .notice
                .clone()
                .unwrap_or_else(|| "Идёт операция…".to_owned());
            sidebar = sidebar.child(
                div()
                    .id("product-notice")
                    .role(Role::Status)
                    .aria_label(message.clone())
                    .mt(px(10.))
                    .px(px(8.))
                    .py(px(6.))
                    .text_size(px(11.))
                    .text_color(colors.muted)
                    .child(message),
            );
        }
        sidebar
    }

    pub(super) fn tabbar(&self, cx: &mut Context<CanvasApp>) -> impl IntoElement {
        let colors = ProductColors::dark();
        let catalog = self.catalog.as_ref();
        let workspace_id = catalog.and_then(selected_workspace);
        let active_tab = catalog.and_then(selected_tab);
        let mut tabs = div()
            .id("product-tabbar")
            .accessibility_id("product-tabbar")
            .role(Role::TabList)
            .aria_label("Вкладки воркспейса")
            .h(px(32.))
            .w_full()
            .flex()
            .items_center()
            .gap_1()
            .overflow_x_scroll()
            .bg(colors.base)
            .border_b_1()
            .border_color(colors.border)
            .px(px(10.));

        if let (Some(catalog), Some(workspace_id)) = (catalog, workspace_id) {
            for tab in catalog
                .tabs
                .iter()
                .filter(|tab| tab.workspace_id == workspace_id)
            {
                tabs = tabs.child(self.tab_control(tab, active_tab == Some(tab.id), cx));
            }
        } else {
            tabs = tabs.child(
                div()
                    .px(px(8.))
                    .text_size(px(12.))
                    .text_color(colors.muted)
                    .child("Выберите воркспейс"),
            );
        }

        tabs.child(self.action_control(
            "new-tab".into(),
            "+".into(),
            ProductAction::CreateTab,
            ControlStyle {
                role: Role::Button,
                selected: false,
                compact: true,
            },
            cx,
        ))
    }

    pub(super) fn toolbar(&self, cx: &mut Context<CanvasApp>) -> impl IntoElement {
        let colors = ProductColors::dark();
        let mut toolbar = div()
            .id("product-toolbar")
            .accessibility_id("product-toolbar")
            .role(Role::Toolbar)
            .aria_label("Команды воркспейса")
            .h(px(40.))
            .w_full()
            .flex()
            .items_center()
            .gap_1()
            .bg(colors.base)
            .border_b_1()
            .border_color(colors.border)
            .px(px(10.));

        for (provider, label, accessible) in [
            ("shell", "Shell", "Открыть терминал"),
            ("claude", "Claude", "Запустить Claude"),
            ("codex", "Codex", "Запустить Codex"),
            ("opencode", "OpenCode", "Запустить OpenCode"),
        ] {
            toolbar = toolbar.child(self.action_control_with_accessible_name(
                format!("launch-{provider}"),
                label.into(),
                accessible.into(),
                ProductAction::Launch(provider.into()),
                ControlStyle {
                    role: Role::Button,
                    selected: false,
                    compact: true,
                },
                cx,
            ));
        }

        toolbar = toolbar.child(self.action_control_with_accessible_name(
            "edit-context".into(),
            "Контекст".into(),
            "Редактировать контекст воркспейса".into(),
            ProductAction::EditContext,
            ControlStyle {
                role: Role::Button,
                selected: false,
                compact: true,
            },
            cx,
        ));

        toolbar = toolbar.child(self.action_control_with_accessible_name(
            "transfer-tab".into(),
            "Перенос".into(),
            "Перенести выбранную вкладку".into(),
            ProductAction::OpenTransfer,
            ControlStyle {
                role: Role::Button,
                selected: false,
                compact: true,
            },
            cx,
        ));

        toolbar = toolbar.child(self.action_control_with_accessible_name(
            "open-sessions".into(),
            "Сессии".into(),
            "Показать неразмещённые сессии".into(),
            ProductAction::OpenSessions,
            ControlStyle {
                role: Role::Button,
                selected: false,
                compact: true,
            },
            cx,
        ));

        toolbar = toolbar.child(div().w(px(1.)).h(px(20.)).mx(px(4.)).bg(colors.border));
        for (key, label, accessible) in [
            ("split_right", "Справа", "Разделить вправо"),
            ("split_down", "Вниз", "Разделить вниз"),
            ("focus", "Фокус", "Режим фокуса"),
            ("equalize", "Ровно", "Выровнять панели"),
            ("undo", "Назад", "Отменить раскладку"),
            ("redo", "Вперёд", "Повторить раскладку"),
        ] {
            toolbar = toolbar.child(self.action_control_with_accessible_name(
                format!("layout-{key}"),
                label.into(),
                accessible.into(),
                ProductAction::Layout(key.into()),
                ControlStyle {
                    role: Role::Button,
                    selected: false,
                    compact: true,
                },
                cx,
            ));
        }

        toolbar
            .child(div().flex_1())
            .child(self.action_control_with_accessible_name(
                "command-palette".into(),
                "Команды".into(),
                "Поиск команд (Ctrl+Shift+Space, затем P)".into(),
                ProductAction::OpenPalette,
                ControlStyle {
                    role: Role::Button,
                    selected: false,
                    compact: true,
                },
                cx,
            ))
    }

    pub(super) fn overlay(
        &self,
        layouts: &BTreeMap<usize, FieldLayout>,
        cx: &mut Context<CanvasApp>,
    ) -> Option<AnyElement> {
        let dialog = self.dialog.as_ref()?;
        Some(self.dialog_overlay(dialog, layouts, cx))
    }

    pub(super) fn empty_state(&self, cx: &mut Context<CanvasApp>) -> Option<AnyElement> {
        let catalog = self.catalog.as_ref()?;
        let (title, detail, action, label) = if catalog.projects.is_empty() {
            (
                "Начните с Git-проекта",
                "Проект объединяет воркспейсы, вкладки и рабочие панели.",
                ProductAction::AddProject,
                "Добавить проект",
            )
        } else if catalog.selection.workspace_id.is_none() {
            if let Some(workspace) = catalog.workspaces.first() {
                (
                    "Выберите воркспейс",
                    "Выбор доступен в списке проектов слева.",
                    ProductAction::SelectWorkspace(workspace.id),
                    "Открыть первый воркспейс",
                )
            } else if catalog.selection.project_id.is_some() {
                (
                    "Создайте воркспейс",
                    "Воркспейс хранит задачу, рабочую копию и вкладки.",
                    ProductAction::CreateWorkspace,
                    "Новый воркспейс",
                )
            } else {
                (
                    "Выберите воркспейс",
                    "Добавьте проект и создайте в нём воркспейс.",
                    ProductAction::AddProject,
                    "Добавить проект",
                )
            }
        } else if catalog.selection.tab_id.is_none() {
            (
                "Создайте вкладку",
                "Вкладка хранит раскладку терминальных панелей.",
                ProductAction::CreateTab,
                "Новая вкладка",
            )
        } else {
            let tab = catalog
                .tabs
                .iter()
                .find(|tab| Some(tab.id) == catalog.selection.tab_id)?;
            let layout = serde_json::from_str::<pigade_layout::TabLayout>(&tab.layout_json).ok()?;
            if !layout.panes().is_empty() {
                return None;
            }
            (
                "Добавьте терминальную панель",
                "Команда откроется в выбранной вкладке.",
                ProductAction::Launch("shell".into()),
                "Открыть терминал",
            )
        };

        let colors = ProductColors::dark();
        let card = div()
            .id("product-empty-state-card")
            .role(Role::Group)
            .aria_label(title)
            .w_full()
            .max_w(px(460.))
            .mx(px(24.))
            .flex()
            .flex_col()
            .items_start()
            .rounded(px(12.))
            .border_1()
            .border_color(colors.border)
            .bg(colors.raised)
            .p(px(24.))
            .child(
                div()
                    .id("product-empty-state-title")
                    .role(Role::Heading)
                    .aria_level(2)
                    .text_size(px(18.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(colors.text)
                    .child(title),
            )
            .child(
                div()
                    .mt(px(8.))
                    .mb(px(16.))
                    .text_size(px(13.))
                    .text_color(colors.muted)
                    .child(detail),
            )
            .child(self.action_control(
                "product-empty-state-action".into(),
                label.into(),
                action,
                ControlStyle {
                    role: Role::Button,
                    selected: true,
                    compact: true,
                },
                cx,
            ));

        Some(
            div()
                .id("product-empty-state")
                .accessibility_id("product-empty-state")
                .w_full()
                .h_full()
                .flex()
                .items_center()
                .justify_center()
                .bg(colors.base)
                .child(card)
                .into_any_element(),
        )
    }

    fn tab_control(&self, tab: &Tab, selected: bool, cx: &mut Context<CanvasApp>) -> AnyElement {
        self.action_control(
            format!("tab-{}", tab.id),
            tab.name.clone(),
            ProductAction::SelectTab(tab.id),
            ControlStyle {
                role: Role::Tab,
                selected,
                compact: false,
            },
            cx,
        )
    }

    fn action_control(
        &self,
        id: String,
        label: String,
        action: ProductAction,
        style: ControlStyle,
        cx: &mut Context<CanvasApp>,
    ) -> AnyElement {
        self.action_control_with_accessible_name(id, label.clone(), label, action, style, cx)
    }

    fn action_control_with_accessible_name(
        &self,
        id: String,
        label: String,
        accessible_name: String,
        action: ProductAction,
        style: ControlStyle,
        cx: &mut Context<CanvasApp>,
    ) -> AnyElement {
        let colors = ProductColors::dark();
        let foreground = if style.selected {
            colors.text
        } else {
            colors.muted
        };
        let background = if style.selected {
            colors.selected
        } else {
            colors.raised
        };
        div()
            .id(id)
            .role(style.role)
            .aria_label(accessible_name)
            .aria_selected(style.selected)
            .h(px(if style.compact { 28. } else { ROW_HEIGHT }))
            .px(px(if style.compact { 9. } else { 10. }))
            .flex()
            .items_center()
            .rounded(px(6.))
            .bg(background)
            .text_size(px(12.))
            .truncate()
            .font_weight(if style.selected {
                FontWeight::MEDIUM
            } else {
                FontWeight::NORMAL
            })
            .text_color(foreground)
            .hover(move |style| style.bg(colors.hover))
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                this.product_action(action.clone(), window, cx);
                cx.stop_propagation();
            }))
            .child(label)
            .into_any_element()
    }

    fn dialog_overlay(
        &self,
        dialog: &Dialog,
        layouts: &BTreeMap<usize, FieldLayout>,
        cx: &mut Context<CanvasApp>,
    ) -> AnyElement {
        let colors = ProductColors::dark();
        if dialog.kind == DialogKind::LaunchReview {
            return self.launch_review_overlay(dialog, layouts, cx);
        }
        if dialog.kind == DialogKind::SessionContext {
            return self.session_context_overlay(cx);
        }
        if dialog.kind == DialogKind::Palette {
            return self.palette_overlay(dialog, layouts, cx);
        }
        if dialog.kind == DialogKind::Transfer {
            return self.transfer_overlay(cx);
        }
        if dialog.kind == DialogKind::Sessions {
            return self.sessions_overlay(cx);
        }

        let mut fields = div().flex().flex_col().gap_2().mt(px(16.));
        for (index, label) in dialog.kind.fields().iter().enumerate() {
            let active = index == dialog.active_field;
            let multiline = dialog_field_multiline(dialog.kind, index);
            let buffer = &dialog.fields[index];
            let field = div()
                .id(format!("product-dialog-field-{index}"))
                .role(Role::TextInput)
                .aria_label(*label)
                .w_full()
                .h(px(if multiline { 120. } else { 42. }))
                .px(px(12.))
                .py(px(6.))
                .rounded(px(7.))
                .border_1()
                .border_color(if active { colors.accent } else { colors.border })
                .bg(colors.base)
                .text_color(colors.text)
                .overflow_hidden()
                .child(crate::edit_field::content(
                    buffer,
                    crate::edit_field::FieldOptions {
                        cache: layouts.get(&index),
                        dialog_id: dialog.id,
                        index,
                        active,
                        multiline,
                        placeholder: label,
                        accent: colors.accent,
                        muted: colors.muted,
                    },
                    cx,
                ))
                .on_mouse_down(
                    gpui::MouseButton::Left,
                    cx.listener(move |this, event, window, cx| {
                        this.editor_pointer_down(index, event, window, cx);
                    }),
                )
                .on_scroll_wheel(cx.listener(move |this, event, window, cx| {
                    this.editor_wheel(index, event, window, cx);
                }));
            fields = fields.child(field);
        }

        if matches!(
            dialog.kind,
            DialogKind::Context | DialogKind::ProjectContext
        ) && let Some(context) = &self.context
        {
            let revision = if context.revision == 0 {
                "Ещё не сохранено".into()
            } else {
                format!(
                    "Сохранённая ревизия v{} · hash {}…",
                    context.revision,
                    context.content_hash.chars().take(12).collect::<String>()
                )
            };
            fields = fields.child(
                div()
                    .id("context-revision-summary")
                    .role(Role::Status)
                    .text_size(px(11.))
                    .text_color(colors.muted)
                    .child(revision),
            );
        }
        if let Some(notice) = &self.notice {
            fields = fields.child(
                div()
                    .id("product-dialog-notice")
                    .role(Role::Status)
                    .aria_label(notice.clone())
                    .text_size(px(12.))
                    .text_color(colors.accent)
                    .whitespace_normal()
                    .child(notice.clone()),
            );
        }
        if matches!(
            dialog.kind,
            DialogKind::Context | DialogKind::ProjectContext
        ) {
            fields = fields.child(
                div()
                    .id("context-revision-note")
                    .role(Role::Status)
                    .text_size(px(11.))
                    .text_color(colors.muted)
                    .child("Enter — новая строка · Ctrl+Enter — сохранить · Ctrl+Z — undo. Работающие сессии не изменяются."),
            );
        }

        let submit = self.action_control(
            "product-dialog-submit".into(),
            dialog.kind.submit_label().into(),
            if dialog.kind == DialogKind::Palette {
                ProductAction::CloseDialog
            } else {
                ProductAction::SubmitDialog
            },
            ControlStyle {
                role: Role::Button,
                selected: true,
                compact: true,
            },
            cx,
        );
        let close = self.action_control(
            "product-dialog-close".into(),
            "Отмена".into(),
            ProductAction::CloseDialog,
            ControlStyle {
                role: Role::Button,
                selected: false,
                compact: true,
            },
            cx,
        );

        div()
            .id("product-dialog-overlay")
            .accessibility_id("product-dialog-overlay")
            .absolute()
            .inset_0()
            .flex()
            .items_start()
            .justify_center()
            .overflow_y_scroll()
            .py(px(24.))
            .bg(rgb(0x090b0e).opacity(0.72))
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                this.product_action(ProductAction::CloseDialog, window, cx);
            }))
            .child(
                div()
                    .id("product-dialog")
                    .role(Role::Dialog)
                    .aria_label(dialog.kind.title())
                    .w_full()
                    .max_w(px(440.))
                    .mx(px(16.))
                    .flex()
                    .flex_col()
                    .rounded(px(12.))
                    .border_1()
                    .border_color(colors.border)
                    .bg(colors.raised)
                    .px(px(20.))
                    .py(px(18.))
                    .text_color(colors.text)
                    .on_click(cx.listener(|_, _: &ClickEvent, _, cx| cx.stop_propagation()))
                    .child(
                        div()
                            .id("product-dialog-title")
                            .role(Role::Heading)
                            .aria_level(2)
                            .text_size(px(16.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(dialog.kind.title()),
                    )
                    .child(fields)
                    .child(
                        div()
                            .flex()
                            .justify_end()
                            .gap_2()
                            .mt(px(18.))
                            .child(close)
                            .child(submit),
                    ),
            )
            .into_any_element()
    }

    fn launch_review_overlay(
        &self,
        dialog: &Dialog,
        layouts: &BTreeMap<usize, FieldLayout>,
        cx: &mut Context<CanvasApp>,
    ) -> AnyElement {
        let colors = ProductColors::dark();
        let preview = self.launch_preview.as_ref();
        let role_override = dialog.fields.first().map(EditBuffer::text).unwrap_or("");
        let agent_instructions = dialog.fields.get(1).map(EditBuffer::text).unwrap_or("");
        let fields_match = preview.is_some_and(|preview| {
            preview.role_override == role_override
                && preview.agent_instructions == agent_instructions
        });
        let can_confirm = preview.is_some_and(|preview| preview.can_launch)
            && fields_match
            && self.launch_phase.is_none()
            && !self.busy;

        let mut metadata = div()
            .id("launch-review-metadata")
            .flex()
            .flex_col()
            .gap_1()
            .text_size(px(12.))
            .text_color(colors.muted);
        let mut context_blocks = div()
            .id("launch-review-context-blocks")
            .role(Role::List)
            .aria_label("Блоки эффективного контекста")
            .flex()
            .flex_col()
            .gap_2()
            .max_h(px(240.))
            .overflow_y_scroll();
        let mut warnings = div().id("launch-review-warnings").flex().flex_col().gap_1();
        let mut status = None;
        let mut preview_controls = div()
            .id("launch-review-controls")
            .flex()
            .flex_wrap()
            .items_center()
            .gap_2()
            .mt(px(12.));

        if let Some(preview) = preview {
            let target = self
                .catalog
                .as_ref()
                .and_then(|catalog| {
                    let tab = catalog.tabs.iter().find(|tab| tab.id == preview.tab_id)?;
                    let workspace = catalog
                        .workspaces
                        .iter()
                        .find(|workspace| workspace.id == tab.workspace_id)?;
                    Some(format!("{} · {}", workspace_name(workspace), tab.name))
                })
                .unwrap_or_else(|| preview.tab_id.to_string());
            metadata = metadata
                .child(format!(
                    "{} · CLI {} · адаптер v{}",
                    preview.provider, preview.cli_version, preview.adapter_version
                ))
                .child(format!("Цель: {target} · план {}", preview.plan_id))
                .child(match preview.target {
                    Some(pane) => format!(
                        "Размещение: {} от панели {}",
                        match preview.side {
                            pigade_layout::Direction::Left => "слева",
                            pigade_layout::Direction::Right => "справа",
                            pigade_layout::Direction::Up => "сверху",
                            pigade_layout::Direction::Down => "снизу",
                        },
                        &pane.to_string()[..8]
                    ),
                    None => "Размещение: первая панель во вкладке".into(),
                })
                .child(format!("Способ доставки: {}", preview.delivery_method))
                .child(format!(
                    "Эффективный контекст · сборка {} · {}",
                    preview.context.id, preview.context.content_hash
                ))
                .child(format!(
                    "Нативное продолжение: {} · ID беседы: {}",
                    capability_label(preview.capabilities.native_resume),
                    capability_label(preview.capabilities.conversation_id)
                ));
            context_blocks = effective_context_blocks(
                &preview.context,
                "launch-review-context-blocks",
                "Блоки эффективного контекста",
                240.,
                self.catalog.as_ref(),
            );

            let memory_label = if preview.include_memory {
                "Память включена"
            } else {
                "Память выключена"
            };
            preview_controls = preview_controls.child(self.action_control(
                "launch-review-memory".into(),
                memory_label.into(),
                ProductAction::ToggleMemory,
                ControlStyle {
                    role: Role::Button,
                    selected: preview.include_memory,
                    compact: true,
                },
                cx,
            ));

            if preview.capabilities.startup_context || preview.deliver_context {
                let delivery_label = if preview.deliver_context {
                    "Запуск без контекста"
                } else {
                    "Передать контекст"
                };
                preview_controls = preview_controls.child(self.action_control(
                    "launch-review-delivery".into(),
                    delivery_label.into(),
                    ProductAction::ToggleContextDelivery,
                    ControlStyle {
                        role: Role::Button,
                        selected: preview.deliver_context,
                        compact: true,
                    },
                    cx,
                ));
            } else {
                let reason = preview
                    .unavailable_reason
                    .as_deref()
                    .unwrap_or("Этот адаптер не поддерживает передачу контекста при запуске.");
                status = Some(format!("Контекст не будет передан: {reason}"));
            }

            warnings = warnings.child(
                div()
                    .id("launch-review-unconfirmed")
                    .role(Role::Status)
                    .text_size(px(12.))
                    .text_color(colors.accent)
                    .child("Предпросмотр не подтверждает, что CLI применил контекст."),
            );
            if preview.capabilities.context_receipt {
                warnings = warnings.child(
                    div()
                        .text_size(px(11.))
                        .text_color(colors.muted)
                        .child("Для этого адаптера доступно подтверждение доставки; применение всё равно не считается подтверждённым этим экраном."),
                );
            }
            if let Some(reason) = &preview.unavailable_reason {
                status = Some(reason.clone());
            }
            for (index, warning) in preview.warnings.iter().enumerate() {
                warnings = warnings.child(
                    div()
                        .id(format!("launch-review-warning-{index}"))
                        .role(Role::Status)
                        .text_size(px(11.))
                        .text_color(colors.muted)
                        .child(warning.clone()),
                );
            }
            if !fields_match {
                status = Some("Поля изменены. Обновите предпросмотр перед запуском.".into());
            } else if !preview.can_launch {
                status = Some(
                    preview
                        .unavailable_reason
                        .clone()
                        .unwrap_or_else(|| "Запуск сейчас недоступен.".into()),
                );
            }
        } else {
            metadata = metadata.child("Предпросмотр запуска ещё не загружен.");
            context_blocks = context_blocks.child(
                div()
                    .id("launch-context-not-loaded")
                    .text_size(px(12.))
                    .text_color(colors.muted)
                    .child("Обновите предпросмотр, чтобы проверить эффективный контекст и параметры запуска."),
            );
            status = Some("Запуск недоступен без актуального предпросмотра.".into());
        }

        let fields = [
            (0usize, "Роль для запуска"),
            (1usize, "Дополнительные инструкции агенту"),
        ]
        .into_iter()
        .fold(
            div().flex().flex_col().gap_2().mt(px(12.)),
            |container, (index, label)| {
                let active = index == dialog.active_field;
                container.child(
                    div()
                        .id(format!("launch-review-field-{index}"))
                        .role(Role::TextInput)
                        .aria_label(label)
                        .w_full()
                        .h(px(if index == 1 { 96. } else { 40. }))
                        .px(px(10.))
                        .py(px(6.))
                        .rounded(px(7.))
                        .border_1()
                        .border_color(if active { colors.accent } else { colors.border })
                        .bg(colors.base)
                        .text_color(colors.text)
                        .overflow_hidden()
                        .child(crate::edit_field::content(
                            &dialog.fields[index],
                            crate::edit_field::FieldOptions {
                                cache: layouts.get(&index),
                                dialog_id: dialog.id,
                                index,
                                active,
                                multiline: index == 1,
                                placeholder: label,
                                accent: colors.accent,
                                muted: colors.muted,
                            },
                            cx,
                        ))
                        .on_mouse_down(
                            gpui::MouseButton::Left,
                            cx.listener(move |this, event, window, cx| {
                                this.editor_pointer_down(index, event, window, cx)
                            }),
                        )
                        .on_scroll_wheel(cx.listener(move |this, event, window, cx| {
                            this.editor_wheel(index, event, window, cx)
                        })),
                )
            },
        );

        if let Some(notice) = &self.notice {
            warnings = warnings.child(
                div()
                    .id("launch-review-notice")
                    .role(Role::Status)
                    .aria_label(notice.clone())
                    .text_size(px(12.))
                    .text_color(colors.accent)
                    .whitespace_normal()
                    .child(notice.clone()),
            );
        }
        if let Some(message) = status {
            warnings = warnings.child(
                div()
                    .id("launch-review-status")
                    .role(Role::Status)
                    .mt(px(8.))
                    .text_size(px(12.))
                    .text_color(colors.muted)
                    .child(message),
            );
        }

        warnings = warnings.child(div().text_size(px(11.)).text_color(colors.muted).child(
            "Ctrl+R — обновить · Ctrl+M — память · Ctrl+D — контекст · Ctrl+Enter — запуск",
        ));
        let refresh = self.action_control(
            "launch-review-refresh".into(),
            "Обновить предпросмотр".into(),
            ProductAction::RefreshLaunchPreview,
            ControlStyle {
                role: Role::Button,
                selected: false,
                compact: true,
            },
            cx,
        );
        let confirm = if can_confirm {
            self.action_control(
                "launch-review-confirm".into(),
                "Подтвердить и запустить".into(),
                ProductAction::ConfirmPreparedLaunch,
                ControlStyle {
                    role: Role::Button,
                    selected: true,
                    compact: true,
                },
                cx,
            )
        } else {
            div()
                .id("launch-review-confirm-unavailable")
                .role(Role::Status)
                .aria_label("Запуск недоступен")
                .h(px(28.))
                .px(px(9.))
                .flex()
                .items_center()
                .rounded(px(6.))
                .bg(colors.base)
                .text_size(px(12.))
                .text_color(colors.muted)
                .child("Подтвердить и запустить")
                .into_any_element()
        };
        let close = self.action_control(
            "launch-review-close".into(),
            "Отмена".into(),
            ProductAction::CloseDialog,
            ControlStyle {
                role: Role::Button,
                selected: false,
                compact: true,
            },
            cx,
        );

        div()
            .id("launch-review-overlay")
            .accessibility_id("launch-review-overlay")
            .absolute()
            .inset_0()
            .flex()
            .items_start()
            .justify_center()
            .overflow_y_scroll()
            .py(px(18.))
            .bg(rgb(0x090b0e).opacity(0.72))
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                this.product_action(ProductAction::CloseDialog, window, cx);
            }))
            .child(
                div()
                    .id("launch-review-sheet")
                    .role(Role::Dialog)
                    .aria_label(DialogKind::LaunchReview.title())
                    .w_full()
                    .max_w(px(760.))
                    .mx(px(16.))
                    .flex()
                    .flex_col()
                    .rounded(px(12.))
                    .border_1()
                    .border_color(colors.border)
                    .bg(colors.raised)
                    .p(px(18.))
                    .on_click(cx.listener(|_, _: &ClickEvent, _, cx| cx.stop_propagation()))
                    .child(
                        div()
                            .id("launch-review-title")
                            .role(Role::Heading)
                            .aria_level(2)
                            .text_size(px(17.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(colors.text)
                            .child("Проверьте запуск до старта"),
                    )
                    .child(metadata)
                    .child(preview_controls)
                    .child(fields)
                    .child(
                        div()
                            .id("launch-review-context-heading")
                            .role(Role::Heading)
                            .aria_level(3)
                            .mt(px(14.))
                            .mb(px(6.))
                            .text_size(px(13.))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(colors.text)
                            .child("Эффективный контекст · блоки по порядку"),
                    )
                    .child(context_blocks)
                    .child(warnings)
                    .child(
                        div()
                            .flex()
                            .justify_end()
                            .gap_2()
                            .mt(px(16.))
                            .child(close)
                            .child(refresh)
                            .child(confirm),
                    ),
            )
            .into_any_element()
    }

    fn session_context_overlay(&self, cx: &mut Context<CanvasApp>) -> AnyElement {
        let colors = ProductColors::dark();
        let mut content = div().flex().flex_col().gap_2();
        if let Some(preview) = &self.launch_preview {
            content = content
                .child(format!(
                    "{} · CLI {} · адаптер v{}",
                    preview.provider, preview.cli_version, preview.adapter_version
                ))
                .child(format!("Фаза: {}", launch_phase_label(self.launch_phase.as_deref())))
                .child(format!("Способ доставки: {}", preview.delivery_method))
                .child(format!(
                    "Сохранённый контекст · сборка {} · {}",
                    preview.context.id, preview.context.content_hash
                ))
                .child(format!(
                    "Сессия {} · поколение {}",
                    preview.session, preview.generation
                ))
                .child(
                    div()
                        .id("session-context-readonly-note")
                        .role(Role::Status)
                        .text_size(px(11.))
                        .text_color(colors.accent)
                        .child("Это сохранённый контекст запуска; редактирование не меняет активную сессию."),
                )
                .child(effective_context_blocks(
                    &preview.context,
                    "session-context-blocks",
                    "Сохранённые блоки контекста сессии",
                    420.,
                    self.catalog.as_ref(),
                ));
        } else {
            content = content.child(
                div()
                    .id("session-context-unavailable")
                    .role(Role::Status)
                    .text_color(colors.muted)
                    .child("Для выбранной сессии сохранённый контекст недоступен."),
            );
        }

        let close = self.action_control(
            "session-context-close".into(),
            "Закрыть".into(),
            ProductAction::CloseDialog,
            ControlStyle {
                role: Role::Button,
                selected: false,
                compact: true,
            },
            cx,
        );
        div()
            .id("session-context-overlay")
            .accessibility_id("session-context-overlay")
            .absolute()
            .inset_0()
            .flex()
            .items_start()
            .justify_center()
            .overflow_y_scroll()
            .py(px(18.))
            .bg(rgb(0x090b0e).opacity(0.72))
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                this.product_action(ProductAction::CloseDialog, window, cx);
            }))
            .child(
                div()
                    .id("session-context-sheet")
                    .role(Role::Dialog)
                    .aria_label(DialogKind::SessionContext.title())
                    .w_full()
                    .max_w(px(760.))
                    .mx(px(16.))
                    .flex()
                    .flex_col()
                    .gap_2()
                    .rounded(px(12.))
                    .border_1()
                    .border_color(colors.border)
                    .bg(colors.raised)
                    .p(px(18.))
                    .text_size(px(12.))
                    .text_color(colors.muted)
                    .on_click(cx.listener(|_, _: &ClickEvent, _, cx| cx.stop_propagation()))
                    .child(
                        div()
                            .id("session-context-title")
                            .role(Role::Heading)
                            .aria_level(2)
                            .text_size(px(17.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(colors.text)
                            .child("Сохранённый контекст сессии"),
                    )
                    .child(content)
                    .child(div().flex().justify_end().mt(px(8.)).child(close)),
            )
            .into_any_element()
    }

    fn transfer_overlay(&self, cx: &mut Context<CanvasApp>) -> AnyElement {
        let colors = ProductColors::dark();
        let current_tab = self.catalog.as_ref().and_then(selected_tab);
        let source_label = self
            .catalog
            .as_ref()
            .and_then(|catalog| {
                let tab = catalog
                    .tabs
                    .iter()
                    .find(|tab| Some(tab.id) == current_tab)?;
                let workspace = catalog
                    .workspaces
                    .iter()
                    .find(|workspace| workspace.id == tab.workspace_id)?;
                Some(format!("Из: {} · {}", workspace_name(workspace), tab.name))
            })
            .unwrap_or_else(|| "Сначала выберите вкладку для переноса.".into());
        let mut targets = div()
            .id("transfer-target-list")
            .role(Role::List)
            .aria_label("Целевые вкладки")
            .flex()
            .flex_col()
            .gap_1()
            .mt(px(14.))
            .max_h(px(360.))
            .overflow_y_scroll();

        let mut target_count = 0;
        if let Some(catalog) = &self.catalog
            && current_tab.is_some()
        {
            for tab in catalog
                .tabs
                .iter()
                .filter(|tab| Some(tab.id) != current_tab)
            {
                let workspace_label = catalog
                    .workspaces
                    .iter()
                    .find(|workspace| workspace.id == tab.workspace_id)
                    .map(workspace_name)
                    .unwrap_or_else(|| "Воркспейс недоступен".into());
                targets = targets.child(self.action_control_with_accessible_name(
                    format!("transfer-target-{}", tab.id),
                    format!("{workspace_label} · {}", tab.name),
                    format!(
                        "Перенести в воркспейс {workspace_label}, вкладка {}",
                        tab.name
                    ),
                    ProductAction::TransferTo(tab.id),
                    ControlStyle {
                        role: Role::Button,
                        selected: false,
                        compact: false,
                    },
                    cx,
                ));
                target_count += 1;
            }
        }
        if target_count == 0 {
            let message = if current_tab.is_none() {
                "Сначала выберите вкладку для переноса."
            } else {
                "Нет других вкладок, куда можно перенести эту вкладку."
            };
            targets = targets.child(
                div()
                    .id("transfer-targets-empty")
                    .px(px(10.))
                    .py(px(12.))
                    .text_color(colors.muted)
                    .child(message),
            );
        }

        let close = self.action_control(
            "transfer-dialog-close".into(),
            "Отмена".into(),
            ProductAction::CloseDialog,
            ControlStyle {
                role: Role::Button,
                selected: false,
                compact: true,
            },
            cx,
        );
        div()
            .id("transfer-dialog-overlay")
            .accessibility_id("transfer-dialog-overlay")
            .absolute()
            .inset_0()
            .flex()
            .items_start()
            .justify_center()
            .overflow_y_scroll()
            .py(px(24.))
            .bg(rgb(0x090b0e).opacity(0.72))
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                this.product_action(ProductAction::CloseDialog, window, cx);
            }))
            .child(
                div()
                    .id("transfer-dialog")
                    .role(Role::Dialog)
                    .aria_label("Перенести панель")
                    .w_full()
                    .max_w(px(480.))
                    .mx(px(16.))
                    .flex()
                    .flex_col()
                    .rounded(px(12.))
                    .border_1()
                    .border_color(colors.border)
                    .bg(colors.raised)
                    .px(px(20.))
                    .py(px(18.))
                    .text_color(colors.text)
                    .on_click(cx.listener(|_, _: &ClickEvent, _, cx| cx.stop_propagation()))
                    .child(
                        div()
                            .id("transfer-dialog-title")
                            .role(Role::Heading)
                            .aria_level(2)
                            .text_size(px(16.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Перенести панель"),
                    )
                    .child(
                        div()
                            .mt(px(6.))
                            .text_size(px(12.))
                            .text_color(colors.muted)
                            .child(source_label),
                    )
                    .child(targets)
                    .child(div().flex().justify_end().mt(px(12.)).child(close)),
            )
            .into_any_element()
    }

    fn sessions_overlay(&self, cx: &mut Context<CanvasApp>) -> AnyElement {
        let colors = ProductColors::dark();
        let placed = placed_session_ids(self.catalog.as_ref());
        let mut seen = HashSet::new();
        let mut entries = div()
            .id("unplaced-session-list")
            .role(Role::List)
            .aria_label("Неразмещённые сессии")
            .flex()
            .flex_col()
            .gap_1()
            .mt(px(14.))
            .max_h(px(360.))
            .overflow_y_scroll();
        let has_tab = self.catalog.as_ref().and_then(selected_tab).is_some();
        let mut count = 0;

        for view in &self.sessions {
            let Ok(session_id) = Uuid::parse_str(&view.session) else {
                continue;
            };
            if placed.contains(&session_id) || !seen.insert(session_id) {
                continue;
            }
            count += 1;
            let short_id = session_id.simple().to_string()[..8].to_owned();
            let title = if view.title.trim().is_empty() {
                "Без названия"
            } else {
                view.title.as_str()
            };
            let state = session_state_label(&view.state);
            let attach = if has_tab {
                self.action_control_with_accessible_name(
                    format!("attach-session-{session_id}"),
                    "Разместить".into(),
                    format!("Разместить сессию {short_id} в выбранной вкладке"),
                    ProductAction::AttachSession(session_id),
                    ControlStyle {
                        role: Role::Button,
                        selected: true,
                        compact: true,
                    },
                    cx,
                )
            } else {
                div()
                    .id(format!("attach-session-unavailable-{session_id}"))
                    .text_size(px(11.))
                    .text_color(colors.muted)
                    .child("Выберите вкладку")
                    .into_any_element()
            };
            entries = entries.child(
                div()
                    .id(format!("session-entry-{session_id}"))
                    .role(Role::ListItem)
                    .aria_label(format!("{title}; {state}; {short_id}"))
                    .w_full()
                    .flex()
                    .items_center()
                    .gap_2()
                    .rounded(px(7.))
                    .border_1()
                    .border_color(colors.border)
                    .bg(colors.base)
                    .px(px(10.))
                    .py(px(8.))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .text_size(px(12.))
                                    .text_color(colors.text)
                                    .truncate()
                                    .child(title.to_owned()),
                            )
                            .child(
                                div()
                                    .mt(px(2.))
                                    .text_size(px(10.))
                                    .text_color(colors.muted)
                                    .child(format!("{state} · {short_id}")),
                            ),
                    )
                    .child(attach),
            );
        }

        if count == 0 {
            entries = entries.child(
                div()
                    .id("sessions-all-placed")
                    .px(px(10.))
                    .py(px(16.))
                    .text_color(colors.muted)
                    .child("Все сессии размещены"),
            );
        }

        let refresh = self.action_control_with_accessible_name(
            "refresh-sessions".into(),
            "Обновить".into(),
            "Обновить список сессий".into(),
            ProductAction::RefreshSessions,
            ControlStyle {
                role: Role::Button,
                selected: false,
                compact: true,
            },
            cx,
        );
        let close = self.action_control(
            "sessions-dialog-close".into(),
            "Закрыть".into(),
            ProductAction::CloseDialog,
            ControlStyle {
                role: Role::Button,
                selected: false,
                compact: true,
            },
            cx,
        );
        div()
            .id("sessions-dialog-overlay")
            .accessibility_id("sessions-dialog-overlay")
            .absolute()
            .inset_0()
            .flex()
            .items_start()
            .justify_center()
            .overflow_y_scroll()
            .py(px(24.))
            .bg(rgb(0x090b0e).opacity(0.72))
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                this.product_action(ProductAction::CloseDialog, window, cx);
            }))
            .child(
                div()
                    .id("sessions-dialog")
                    .role(Role::Dialog)
                    .aria_label("Неразмещённые сессии")
                    .w_full()
                    .max_w(px(560.))
                    .mx(px(16.))
                    .flex()
                    .flex_col()
                    .rounded(px(12.))
                    .border_1()
                    .border_color(colors.border)
                    .bg(colors.raised)
                    .p(px(16.))
                    .on_click(cx.listener(|_, _: &ClickEvent, _, cx| cx.stop_propagation()))
                    .child(
                        div()
                            .id("sessions-dialog-heading")
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(
                                div()
                                    .id("sessions-dialog-title")
                                    .role(Role::Heading)
                                    .aria_level(2)
                                    .text_size(px(16.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(colors.text)
                                    .child("Неразмещённые сессии"),
                            )
                            .child(refresh),
                    )
                    .child(entries)
                    .child(div().flex().justify_end().mt(px(12.)).child(close)),
            )
            .into_any_element()
    }

    fn palette_overlay(
        &self,
        dialog: &Dialog,
        layouts: &BTreeMap<usize, FieldLayout>,
        cx: &mut Context<CanvasApp>,
    ) -> AnyElement {
        let colors = ProductColors::dark();
        let query = dialog.fields.first().map(EditBuffer::text).unwrap_or("");
        let mut card = div()
            .id("product-command-palette")
            .role(Role::Dialog)
            .aria_label("Команды PigADE")
            .w_full()
            .max_w(px(560.))
            .mx(px(16.))
            .mt(px(72.))
            .flex()
            .flex_col()
            .rounded(px(12.))
            .border_1()
            .border_color(colors.border)
            .bg(colors.raised)
            .p(px(10.))
            .on_click(cx.listener(|_, _: &ClickEvent, _, cx| cx.stop_propagation()));
        card = card.child(
            div()
                .id("palette-heading")
                .role(Role::Heading)
                .aria_level(2)
                .px(px(8.))
                .py(px(6.))
                .text_size(px(13.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(colors.text)
                .child("Команды"),
        );
        let field_label = "Поиск команды";
        card = card.child(
            div()
                .id("product-dialog-field-0")
                .role(Role::SearchInput)
                .aria_label(field_label)
                .h(px(42.))
                .w_full()
                .px(px(12.))
                .py(px(6.))
                .rounded(px(7.))
                .border_1()
                .border_color(colors.accent)
                .bg(colors.base)
                .text_color(colors.text)
                .overflow_hidden()
                .child(crate::edit_field::content(
                    &dialog.fields[0],
                    crate::edit_field::FieldOptions {
                        cache: layouts.get(&0),
                        dialog_id: dialog.id,
                        index: 0,
                        active: true,
                        multiline: false,
                        placeholder: field_label,
                        accent: colors.accent,
                        muted: colors.muted,
                    },
                    cx,
                ))
                .on_mouse_down(
                    gpui::MouseButton::Left,
                    cx.listener(|this, event, window, cx| {
                        this.editor_pointer_down(0, event, window, cx)
                    }),
                ),
        );

        let commands = palette_commands();
        let mut command_list = div()
            .id("product-command-list")
            .role(Role::List)
            .aria_label("Результаты команд")
            .flex()
            .flex_col()
            .gap_1()
            .mt(px(8.))
            .max_h(px(420.))
            .overflow_y_scroll();
        let mut count = 0;
        for (index, (label, action)) in commands.iter().enumerate() {
            if !query.is_empty() && !label.to_lowercase().contains(&query.to_lowercase()) {
                continue;
            }
            count += 1;
            command_list = command_list.child(self.action_control(
                format!("palette-command-{index}"),
                (*label).into(),
                action.clone(),
                ControlStyle {
                    role: Role::Button,
                    selected: false,
                    compact: false,
                },
                cx,
            ));
        }
        if count == 0 {
            command_list = command_list.child(
                div()
                    .id("palette-empty")
                    .px(px(10.))
                    .py(px(12.))
                    .text_color(colors.muted)
                    .child("Нет подходящих команд"),
            );
        }
        card = card.child(command_list);

        div()
            .id("product-command-palette-overlay")
            .accessibility_id("product-command-palette-overlay")
            .absolute()
            .inset_0()
            .flex()
            .items_start()
            .justify_center()
            .overflow_y_scroll()
            .bg(rgb(0x090b0e).opacity(0.65))
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                this.product_action(ProductAction::CloseDialog, window, cx);
            }))
            .child(card)
            .into_any_element()
    }
}

#[cfg(test)]
mod context_edit_tests {
    use super::*;

    fn document() -> ContextDocument {
        ContextDocument {
            scope_id: Uuid::from_u128(1),
            revision: 1,
            instructions: "Saved instructions".into(),
            role: "reviewer".into(),
            memory: "Facts".into(),
            content_hash: String::new(),
            saved_at_unix_ms: None,
            author: "user".into(),
        }
    }

    #[test]
    fn save_ack_keeps_newer_draft_caret_and_undo() {
        let mut ui = ProductUi::default();
        let saved = document();
        ui.apply_context_reply(saved.clone(), false, true);
        let dialog = ui.dialog.as_mut().unwrap();
        let id = dialog.id;
        dialog.fields[0].replace(None, " + edit", true).unwrap();
        let mut acknowledged = saved;
        acknowledged.revision = 2;
        acknowledged.instructions = dialog.fields[0].text().to_owned();
        dialog.fields[0]
            .replace(None, " + newer typing", true)
            .unwrap();
        let cursor = dialog.fields[0].cursor();
        ui.apply_context_reply(acknowledged, true, false);
        let dialog = ui.dialog.as_mut().unwrap();
        assert_eq!(dialog.id, id);
        assert_eq!(dialog.fields[0].cursor(), cursor);
        assert!(dialog.fields[0].text().ends_with(" + newer typing"));
        assert!(dialog.fields[0].undo());
        assert!(dialog.fields[0].text().ends_with(" + edit"));
        assert_eq!(ui.context.as_ref().unwrap().revision, 2);
        assert!(ui.notice.as_ref().unwrap().contains("несохранённые"));
    }

    #[test]
    fn canceled_load_and_save_ack_never_reopen_the_editor() {
        let mut ui = ProductUi::default();
        ui.apply_context_reply(document(), false, false);
        assert!(ui.dialog.is_none());
        assert!(ui.context.is_none());
        ui.apply_context_reply(document(), false, true);
        ui.dialog = None;
        ui.apply_context_reply(document(), true, false);
        assert!(ui.dialog.is_none());
    }
}

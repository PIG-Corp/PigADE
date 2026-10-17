//! Native PigADE product chrome. The host owns commands and durable state;
//! this module renders the current catalog and emits typed user actions.

use crate::edit_field::FieldLayout;
use gpui::{
    AnyElement, ClickEvent, Context, Div, FontWeight, Hsla, IntoElement, ParentElement, Role,
    Stateful, StatefulInteractiveElement, Styled, div,
    prelude::{FluentBuilder, InteractiveElement},
    px, rgb,
};
use pigade_storage::{Catalog, ContextDocument, Tab, Workspace, WorkspaceStatus};
use std::collections::{BTreeMap, HashSet};
use uuid::Uuid;

use super::{CanvasApp, chrome, protocol, text_edit::EditBuffer};

const SIDEBAR_WIDTH: f32 = 220.0;
const ROW_HEIGHT: f32 = 30.0;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum ProductAction {
    AddProject,
    ToggleSidebar,
    FocusPane(Uuid),
    OpenProjectPath,
    CreateProject,
    ChooseProjectParent,
    CreateWorkspace,
    CreateWorkspaceFor(Uuid),
    WorkspaceIsolation(bool),
    CreateTab,
    OpenTransfer,
    TransferTo(Uuid),
    OpenSessions,
    RefreshSessions,
    AttachSession(Uuid),
    RefreshLaunchPreview,
    ConfirmPreparedLaunch,
    PrepareResume,
    ToggleMemory,
    ToggleMemorySection(usize),
    ReloadMemory,
    ToggleContextDelivery,
    InspectSessionContext,
    InspectConversation,
    OpenGenerations,
    LoadGenerations(Option<u64>),
    SelectGeneration(Option<Uuid>),
    OpenHistory,
    HistoryStart,
    HistoryPrevious,
    HistoryNext,
    HistoryEnd,
    HistoryRefresh,
    HistorySearch,
    HistoryNextMatch,
    HistoryCopyPage,
    PanCanvas,
    RevealFocusedPane,
    SelectProject(Uuid),
    SelectWorkspace(Uuid),
    SelectTab(Uuid),
    Launch(String),
    ReviewLaunch(String),
    Layout(String),
    EditContext,
    EditProjectContext,
    OpenLauncher,
    ToggleLaunchDetails,
    OpenPaneMenu,
    OpenPalette,
    CloseDialog,
    SubmitDialog,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum DialogKind {
    AddProject,
    CreateProject,
    CreateWorkspace,
    Launcher,
    NewTab,
    Palette,
    PaneMenu,
    Context,
    ProjectContext,
    LaunchReview,
    ResumeReview,
    SessionContext,
    Conversation,
    Generations,
    Transfer,
    Sessions,
    History,
}

impl DialogKind {
    fn title(self) -> &'static str {
        match self {
            Self::AddProject => "Добавить проект",
            Self::CreateProject => "Создать проект",
            Self::CreateWorkspace => "Новый воркспейс",
            Self::Launcher => "Добавить агента",
            Self::NewTab => "Новая вкладка",
            Self::Palette => "Команды PigADE",
            Self::PaneMenu => "Управление панелями",
            Self::Context => "Контекст воркспейса",
            Self::ProjectContext => "Контекст проекта",
            Self::LaunchReview => "Проверка запуска",
            Self::ResumeReview => "Продолжить диалог CLI",
            Self::SessionContext => "Контекст выбранной сессии",
            Self::Conversation => "Диалог CLI",
            Self::Generations => "Поколения панели",
            Self::Transfer => "Перенести панель",
            Self::Sessions => "Неразмещённые сессии",
            Self::History => "История сессии",
        }
    }

    fn fields(self) -> &'static [&'static str] {
        match self {
            Self::AddProject => &["Путь к Git-проекту"],
            Self::CreateProject => &["Название проекта", "Папка проекта"],
            Self::CreateWorkspace => &["Название задачи / воркспейса"],
            Self::Launcher => &[],
            Self::NewTab => &["Название вкладки"],
            Self::Palette => &["Поиск команды"],
            Self::PaneMenu => &["Поиск действия панели"],
            Self::Context => &["Инструкции", "Роль", "Память"],
            Self::ProjectContext => &["Инструкции проекта"],
            Self::LaunchReview => &[
                "Роль для запуска",
                "Дополнительные инструкции агенту",
                "Разделы памяти: номера через запятую; пусто — без памяти",
            ],
            Self::ResumeReview => &[],
            Self::SessionContext => &[],
            Self::Conversation => &["Точный ID диалога CLI"],
            Self::Generations => &["Номер строки или «текущее»"],
            Self::Transfer => &[],
            Self::Sessions => &[],
            Self::History => &["Найти текст", "История"],
        }
    }

    fn submit_label(self) -> &'static str {
        match self {
            Self::AddProject => "Добавить проект",
            Self::CreateProject => "Создать проект",
            Self::CreateWorkspace => "Создать воркспейс",
            Self::Launcher => "Закрыть",
            Self::NewTab => "Создать вкладку",
            Self::Palette => "Закрыть",
            Self::PaneMenu => "Закрыть",
            Self::Context => "Сохранить ревизию",
            Self::ProjectContext => "Сохранить ревизию",
            Self::LaunchReview => "Подтвердить и запустить",
            Self::ResumeReview => "Продолжить диалог",
            Self::SessionContext => "Закрыть",
            Self::Conversation => "Сохранить привязку",
            Self::Generations => "Показать поколение",
            Self::Transfer => "Закрыть",
            Self::Sessions => "Закрыть",
            Self::History => "Закрыть",
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
            fields: vec![EditBuffer::default(); kind.fields().len()],
            active_field: 0,
        }
    }
}

#[derive(Default)]
pub(super) struct ProductUi {
    pub(super) compact: bool,
    pub(super) sidebar_compact: bool,
    pub(super) sidebar_override: Option<bool>,
    pub(super) catalog: Option<Catalog>,
    pub(super) context: Option<ContextDocument>,
    pub(super) sessions: Vec<SessionSummary>,
    pub(super) launch_preview: Option<protocol::LaunchPreview>,
    pub(super) launch_phase: Option<String>,
    pub(super) providers: Option<Vec<protocol::ProviderInfo>>,
    pub(super) provider_error: Option<String>,
    pub(super) launch_details: bool,
    pub(super) generation_page: Option<protocol::GenerationList>,
    pub(super) notice: Option<String>,
    pub(super) busy: bool,
    pub(super) dialog: Option<Dialog>,
    pub(super) workspace_isolated: bool,
    pub(super) workspace_project: Option<Uuid>,
    pub(super) history_sheet: Option<crate::history_view::HistorySheet>,
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
            base: rgb(chrome::BASE).into(),
            sidebar: rgb(chrome::SIDEBAR).into(),
            raised: rgb(chrome::RAISED).into(),
            hover: rgb(chrome::HOVER).into(),
            selected: rgb(chrome::SELECTED).into(),
            border: rgb(chrome::BORDER).into(),
            text: rgb(chrome::TEXT).into(),
            muted: rgb(chrome::MUTED).into(),
            accent: rgb(chrome::ACCENT).into(),
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
        ("Открыть папку проекта", ProductAction::AddProject),
        ("Создать проект", ProductAction::CreateProject),
        ("Выбрать путь вручную…", ProductAction::OpenProjectPath),
        ("Новый воркспейс", ProductAction::CreateWorkspace),
        ("Добавить агента", ProductAction::OpenLauncher),
        ("Управление панелями", ProductAction::OpenPaneMenu),
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
        (
            "Диалог CLI · привязать ID · B",
            ProductAction::InspectConversation,
        ),
        (
            "Продолжить привязанный диалог CLI · resume",
            ProductAction::PrepareResume,
        ),
        ("Перенести панель", ProductAction::OpenTransfer),
        (
            "Поколения панели · история · O",
            ProductAction::OpenGenerations,
        ),
        ("Сессии", ProductAction::OpenSessions),
        ("История панели · I", ProductAction::OpenHistory),
        ("Новая вкладка", ProductAction::CreateTab),
        ("Открыть терминал", ProductAction::Launch("shell".into())),
        (
            "Настроить запуск терминала",
            ProductAction::ReviewLaunch("shell".into()),
        ),
        (
            "Настроить запуск Claude Code",
            ProductAction::ReviewLaunch("claude".into()),
        ),
        (
            "Настроить запуск Codex",
            ProductAction::ReviewLaunch("codex".into()),
        ),
        (
            "Настроить запуск OpenCode",
            ProductAction::ReviewLaunch("opencode".into()),
        ),
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

fn panel_action(action: &ProductAction) -> bool {
    matches!(
        action,
        ProductAction::Layout(_)
            | ProductAction::OpenHistory
            | ProductAction::OpenGenerations
            | ProductAction::OpenTransfer
            | ProductAction::InspectSessionContext
            | ProductAction::InspectConversation
            | ProductAction::PrepareResume
            | ProductAction::RevealFocusedPane
    )
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
        DialogKind::History => index == 1,
        _ => false,
    }
}

impl ProductUi {
    pub(super) fn palette_action(&self) -> Option<ProductAction> {
        let dialog = self.dialog.as_ref()?;
        if !matches!(dialog.kind, DialogKind::Palette | DialogKind::PaneMenu) {
            return None;
        }
        let query = dialog.fields.first().map(EditBuffer::text).unwrap_or("");
        if query.trim().is_empty() {
            return None;
        }
        palette_commands()
            .into_iter()
            .filter(|(_, action)| dialog.kind != DialogKind::PaneMenu || panel_action(action))
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
        if self
            .dialog
            .as_ref()
            .is_some_and(|dialog| dialog.kind == DialogKind::History && dialog.active_field == 1)
        {
            return None;
        }
        let dialog = self.dialog.as_mut()?;
        dialog.fields.get_mut(dialog.active_field)
    }

    pub(super) fn selection_editor_mut(&mut self) -> Option<&mut EditBuffer> {
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

    pub(super) fn header(&self, cx: &mut Context<CanvasApp>) -> AnyElement {
        let colors = ProductColors::dark();
        let location = self
            .catalog
            .as_ref()
            .and_then(|catalog| {
                let project = catalog
                    .projects
                    .iter()
                    .find(|value| Some(value.id) == catalog.selection.project_id)?;
                let workspace = catalog
                    .workspaces
                    .iter()
                    .find(|value| Some(value.id) == catalog.selection.workspace_id);
                Some(match workspace {
                    Some(workspace) => {
                        format!("{}  /  {}", project.name, workspace_name(workspace))
                    }
                    None => project.name.clone(),
                })
            })
            .unwrap_or_default();
        div()
            .id("app-heading")
            .role(Role::Group)
            .aria_label("PigADE")
            .h(px(40.))
            .flex_shrink_0()
            .w_full()
            .px(px(10.))
            .flex()
            .items_center()
            .gap_2()
            .bg(colors.sidebar)
            .border_b_1()
            .border_color(colors.border)
            .child(self.icon_control(
                "toggle-sidebar",
                "Свернуть или развернуть проекты",
                "layers",
                ProductAction::ToggleSidebar,
                cx,
            ))
            .child(chrome::icon("logo-p", 20., chrome::ACCENT))
            .child(
                div()
                    .id("app-brand")
                    .role(Role::Heading)
                    .aria_level(1)
                    .font_weight(FontWeight::MEDIUM)
                    .text_size(px(13.))
                    .child("PigADE"),
            )
            .when(!self.compact, |bar| {
                bar.child(
                    div()
                        .text_color(colors.muted)
                        .text_size(px(10.))
                        .child("0.0.1-dev"),
                )
            })
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .px(px(16.))
                    .text_size(px(12.))
                    .text_color(colors.muted)
                    .truncate()
                    .child(location),
            )
            .child(self.icon_control(
                "header-commands",
                "Поиск команд (Ctrl+Shift+Space, P)",
                "search",
                ProductAction::OpenPalette,
                cx,
            ))
            .into_any_element()
    }

    pub(super) fn sidebar(
        &self,
        views: &[protocol::View],
        active_pane: Option<Uuid>,
        cx: &mut Context<CanvasApp>,
    ) -> AnyElement {
        let colors = ProductColors::dark();
        let compact = self.sidebar_compact;
        let mut tree = div()
            .id("product-project-tree")
            .role(Role::List)
            .aria_label("Проекты и воркспейсы")
            .flex()
            .flex_col()
            .gap_1()
            .overflow_y_scroll()
            .flex_1()
            .min_h_0();
        if let Some(catalog) = &self.catalog {
            for project in &catalog.projects {
                let project_action = ProductAction::SelectProject(project.id);
                let row = if compact {
                    self.icon_control(
                        &format!("project-{}", project.id),
                        &project.name,
                        "folder",
                        project_action,
                        cx,
                    )
                } else {
                    div()
                        .flex()
                        .items_center()
                        .gap_1()
                        .child(self.action_control(
                            format!("project-{}", project.id),
                            project.name.clone(),
                            project_action,
                            ControlStyle {
                                role: Role::TreeItem,
                                selected: false,
                                compact: false,
                            },
                            cx,
                        ))
                        .child(self.icon_control(
                            &format!("project-add-workspace-{}", project.id),
                            &format!("Новый воркспейс в {}", project.name),
                            "plus",
                            ProductAction::CreateWorkspaceFor(project.id),
                            cx,
                        ))
                        .into_any_element()
                };
                tree = tree.child(div().mt(px(5.)).child(row));
                for workspace in catalog
                    .workspaces
                    .iter()
                    .filter(|value| value.project_id == project.id)
                {
                    let selected = selected_workspace(catalog) == Some(workspace.id);
                    let title = workspace_name(workspace);
                    let label = if compact {
                        title.chars().take(2).collect::<String>()
                    } else {
                        title.clone()
                    };
                    tree = tree.child(div().pl(px(if compact { 0. } else { 12. })).child(
                        self.action_control_with_accessible_name(
                            format!("workspace-{}", workspace.id),
                            label,
                            title,
                            ProductAction::SelectWorkspace(workspace.id),
                            ControlStyle {
                                role: Role::TreeItem,
                                selected,
                                compact: false,
                            },
                            cx,
                        ),
                    ));
                    if selected && !compact {
                        for view in views {
                            let Ok(id) = view.session.parse::<Uuid>() else {
                                continue;
                            };
                            let title = if view.frame.title.is_empty() {
                                "Терминал"
                            } else {
                                view.frame.title.as_str()
                            };
                            let live = view.state == "running";
                            tree = tree.child(
                                div().pl(px(28.)).child(
                                    div()
                                        .id(format!("sidebar-pane-{id}"))
                                        .role(Role::Button)
                                        .aria_label(format!(
                                            "{title}: {}",
                                            session_state_label(&view.state)
                                        ))
                                        .h(px(26.))
                                        .px(px(6.))
                                        .flex()
                                        .items_center()
                                        .gap_2()
                                        .rounded(px(5.))
                                        .bg(if active_pane == Some(id) {
                                            colors.raised
                                        } else {
                                            colors.sidebar
                                        })
                                        .hover(|style| style.bg(colors.hover))
                                        .child(div().size(px(5.)).rounded_full().bg(rgb(if live {
                                            chrome::STATUS_GREEN
                                        } else {
                                            chrome::MUTED
                                        })))
                                        .child(
                                            div()
                                                .text_size(px(11.))
                                                .text_color(colors.muted)
                                                .truncate()
                                                .child(title.to_owned()),
                                        )
                                        .on_click(cx.listener(
                                            move |this, _: &ClickEvent, window, cx| {
                                                this.product_action(
                                                    ProductAction::FocusPane(id),
                                                    window,
                                                    cx,
                                                );
                                                cx.stop_propagation();
                                            },
                                        )),
                                ),
                            );
                        }
                    }
                }
            }
            if catalog.projects.is_empty() && !compact {
                tree = tree.child(
                    div()
                        .mt(px(22.))
                        .px(px(10.))
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(chrome::icon("folder", 20., chrome::MUTED))
                        .child(
                            div()
                                .text_color(colors.muted)
                                .text_size(px(12.))
                                .child("Пока нет проектов"),
                        ),
                );
            }
        }
        let mut footer = div()
            .flex()
            .flex_col()
            .gap_1()
            .pt(px(10.))
            .border_t_1()
            .border_color(colors.border);
        for (id, label, icon, action) in [
            (
                "sidebar-open-project",
                "Открыть папку…",
                "folder",
                ProductAction::AddProject,
            ),
            (
                "sidebar-create-project",
                "Создать проект…",
                "plus",
                ProductAction::CreateProject,
            ),
        ] {
            footer = footer.child(if compact {
                self.icon_control(id, label, icon, action, cx)
            } else {
                self.action_control(
                    id.into(),
                    label.into(),
                    action,
                    ControlStyle {
                        role: Role::Button,
                        selected: false,
                        compact: false,
                    },
                    cx,
                )
            });
        }
        if !compact
            && self
                .catalog
                .as_ref()
                .is_some_and(|catalog| catalog.selection.project_id.is_some())
        {
            footer = footer.child(self.action_control(
                "edit-project-context".into(),
                "Контекст проекта".into(),
                ProductAction::EditProjectContext,
                ControlStyle {
                    role: Role::Button,
                    selected: false,
                    compact: false,
                },
                cx,
            ));
        }
        let mut sidebar = div()
            .id("product-sidebar")
            .accessibility_id("product-sidebar")
            .role(Role::Group)
            .aria_label("Проекты")
            .w(px(if compact { 52. } else { SIDEBAR_WIDTH }))
            .flex_shrink_0()
            .h_full()
            .flex()
            .flex_col()
            .bg(colors.sidebar)
            .border_r_1()
            .border_color(colors.border)
            .px(px(if compact { 8. } else { 12. }))
            .py(px(10.));
        if !compact {
            sidebar = sidebar.child(
                div()
                    .h(px(30.))
                    .flex()
                    .items_center()
                    .px(px(8.))
                    .child(
                        div()
                            .flex_1()
                            .text_size(px(12.))
                            .text_color(colors.muted)
                            .child("Проекты"),
                    )
                    .child(self.icon_control(
                        "sidebar-add",
                        "Открыть проект",
                        "plus",
                        ProductAction::AddProject,
                        cx,
                    )),
            );
        }
        sidebar = sidebar.child(tree).child(footer);
        if !compact && (self.busy || self.notice.is_some()) {
            let message = self
                .notice
                .clone()
                .unwrap_or_else(|| "Идёт операция…".into());
            sidebar = sidebar.child(
                div()
                    .id("product-notice")
                    .role(Role::Status)
                    .aria_label(message.clone())
                    .mt(px(8.))
                    .px(px(8.))
                    .py(px(6.))
                    .text_size(px(11.))
                    .text_color(colors.muted)
                    .child(message),
            );
        }
        sidebar.into_any_element()
    }

    pub(super) fn tabbar(&self, cx: &mut Context<CanvasApp>) -> AnyElement {
        let colors = ProductColors::dark();
        let mut tabs = div()
            .id("product-tabbar")
            .accessibility_id("product-tabbar")
            .role(Role::TabList)
            .aria_label("Вкладки воркспейса")
            .h_full()
            .flex_1()
            .min_w_0()
            .flex()
            .items_center()
            .gap_1()
            .overflow_x_scroll()
            .px(px(8.));
        if let Some(catalog) = &self.catalog {
            for tab in catalog
                .tabs
                .iter()
                .filter(|tab| Some(tab.workspace_id) == catalog.selection.workspace_id)
            {
                tabs =
                    tabs.child(self.tab_control(tab, catalog.selection.tab_id == Some(tab.id), cx));
            }
        }
        tabs.child(self.icon_control(
            "new-tab",
            "Новая вкладка",
            "plus",
            ProductAction::CreateTab,
            cx,
        ))
        .text_color(colors.muted)
        .into_any_element()
    }

    pub(super) fn toolbar(&self, cx: &mut Context<CanvasApp>) -> AnyElement {
        let colors = ProductColors::dark();
        let mut bar = div()
            .id("product-toolbar")
            .role(Role::Toolbar)
            .aria_label("Команды воркспейса")
            .h_full()
            .flex_shrink_0()
            .flex()
            .items_center()
            .gap_1()
            .pr(px(8.));
        bar = bar
            .child(self.action_control(
                "open-agent-launcher".into(),
                "Агент".into(),
                ProductAction::OpenLauncher,
                ControlStyle {
                    role: Role::Button,
                    selected: true,
                    compact: true,
                },
                cx,
            ))
            .child(self.icon_control(
                "launch-shell",
                "Открыть терминал",
                "terminal",
                ProductAction::Launch("shell".into()),
                cx,
            ))
            .child(self.icon_control(
                "pane-menu",
                "Управление панелями",
                "more",
                ProductAction::OpenPaneMenu,
                cx,
            ));
        if !self.compact {
            bar = bar.child(self.icon_control(
                "edit-context",
                "Контекст воркспейса",
                "layers",
                ProductAction::EditContext,
                cx,
            ));
        }
        bar.border_l_1()
            .border_color(colors.border)
            .into_any_element()
    }

    fn icon_control(
        &self,
        id: &str,
        label: &str,
        icon: &'static str,
        action: ProductAction,
        cx: &mut Context<CanvasApp>,
    ) -> AnyElement {
        let colors = ProductColors::dark();
        div()
            .id(id.to_owned())
            .role(Role::Button)
            .aria_label(label.to_owned())
            .size(px(28.))
            .flex_shrink_0()
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(5.))
            .text_color(colors.muted)
            .hover(|style| style.bg(colors.hover).text_color(colors.text))
            .child(chrome::icon(icon, 15., chrome::MUTED))
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                this.product_action(action.clone(), window, cx);
                cx.stop_propagation();
            }))
            .into_any_element()
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
        let colors = ProductColors::dark();
        let welcome = catalog.projects.is_empty();
        if welcome {
            return Some(self.first_run(cx));
        }
        let state = if welcome {
            Some((
                "Рабочее место для CLI-агентов",
                "Откройте папку проекта, чтобы создать воркспейс и начать работу с терминалом.",
            ))
        } else if catalog.selection.workspace_id.is_none() {
            if let Some(project_id) = catalog.selection.project_id {
                let project = catalog
                    .projects
                    .iter()
                    .find(|project| project.id == project_id)?;
                if let Some(workspace) = catalog
                    .workspaces
                    .iter()
                    .find(|workspace| workspace.project_id == project.id)
                {
                    return Some(self.welcome_card(
                        "Выберите воркспейс",
                        "Откройте воркспейс этого проекта из списка слева.",
                        ProductAction::SelectWorkspace(workspace.id),
                        "Открыть воркспейс",
                        None,
                        cx,
                    ));
                } else {
                    Some((
                        "Создайте воркспейс",
                        "Воркспейс хранит задачу, вкладки и терминальные панели.",
                    ))
                }
            } else if let Some(workspace) = catalog.workspaces.first() {
                return Some(self.welcome_card(
                    "Выберите воркспейс",
                    "Откройте воркспейс из списка слева.",
                    ProductAction::SelectWorkspace(workspace.id),
                    "Открыть воркспейс",
                    None,
                    cx,
                ));
            } else {
                return Some(self.welcome_card(
                    "Откройте папку проекта",
                    "Проекты объединяют воркспейсы и рабочие вкладки.",
                    ProductAction::AddProject,
                    "Открыть папку",
                    Some((ProductAction::CreateProject, "Создать проект")),
                    cx,
                ));
            }
        } else if catalog.selection.tab_id.is_none() {
            Some((
                "Создайте вкладку",
                "Вкладка содержит раскладку терминальных панелей.",
            ))
        } else {
            let tab = catalog
                .tabs
                .iter()
                .find(|tab| Some(tab.id) == catalog.selection.tab_id)?;
            let layout = serde_json::from_str::<pigade_layout::TabLayout>(&tab.layout_json).ok()?;
            if !layout.panes().is_empty() {
                return None;
            }
            Some((
                "Добавьте агента или откройте терминал",
                "Выберите CLI для этой вкладки.",
            ))
        };
        let (title, detail) = state?;
        let primary = if welcome {
            ProductAction::AddProject
        } else if catalog.selection.workspace_id.is_none() {
            ProductAction::CreateWorkspace
        } else if catalog.selection.tab_id.is_none() {
            ProductAction::CreateTab
        } else {
            ProductAction::OpenLauncher
        };
        let primary_label = if welcome {
            "Открыть папку"
        } else if catalog.selection.workspace_id.is_none() {
            "Создать воркспейс"
        } else if catalog.selection.tab_id.is_none() {
            "Создать вкладку"
        } else {
            "Добавить агента"
        };
        let card = div()
            .id("product-empty-state-card")
            .role(Role::Group)
            .aria_label(title)
            .w_full()
            .max_w(px(520.))
            .mx(px(24.))
            .flex()
            .flex_col()
            .items_start()
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
                primary_label.into(),
                primary,
                ControlStyle {
                    role: Role::Button,
                    selected: true,
                    compact: true,
                },
                cx,
            ));
        let mut card = card;
        if welcome {
            card = card.child(self.action_control(
                "product-create-project".into(),
                "Создать проект".into(),
                ProductAction::CreateProject,
                ControlStyle {
                    role: Role::Button,
                    selected: false,
                    compact: true,
                },
                cx,
            ));
        } else if catalog.selection.tab_id.is_some() {
            for (provider, label) in [
                ("shell", "Терминал"),
                ("claude", "Claude"),
                ("codex", "Codex"),
                ("opencode", "OpenCode"),
            ] {
                card = card.child(self.provider_choice(provider, label, cx));
            }
        }

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

    fn welcome_card(
        &self,
        title: &'static str,
        detail: &'static str,
        primary: ProductAction,
        primary_label: &'static str,
        secondary: Option<(ProductAction, &'static str)>,
        cx: &mut Context<CanvasApp>,
    ) -> AnyElement {
        let colors = ProductColors::dark();
        let mut card = div()
            .id("product-empty-state-card")
            .role(Role::Group)
            .aria_label(title)
            .w_full()
            .max_w(px(520.))
            .mx(px(24.))
            .flex()
            .flex_col()
            .items_start()
            .gap_2()
            .p(px(24.))
            .child(
                div()
                    .id("workspace-welcome-title")
                    .role(Role::Heading)
                    .aria_level(2)
                    .text_size(px(20.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(colors.text)
                    .child(title),
            )
            .child(
                div()
                    .mb(px(12.))
                    .text_size(px(13.))
                    .text_color(colors.muted)
                    .child(detail),
            )
            .child(self.action_control(
                "product-empty-state-action".into(),
                primary_label.into(),
                primary,
                ControlStyle {
                    role: Role::Button,
                    selected: true,
                    compact: true,
                },
                cx,
            ));
        if let Some((action, label)) = secondary {
            card = card.child(self.action_control(
                "product-create-project".into(),
                label.into(),
                action,
                ControlStyle {
                    role: Role::Button,
                    selected: false,
                    compact: true,
                },
                cx,
            ));
        }
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
            .into_any_element()
    }

    fn first_run(&self, cx: &mut Context<CanvasApp>) -> AnyElement {
        let colors = ProductColors::dark();
        let mut actions = div().w_full().flex().gap_3().mt(px(28.));
        if self.compact {
            actions = actions.flex_col();
        }
        actions = actions
            .child(self.start_tile(
                "welcome-open-folder",
                ("Открыть папку", "Существующий Git-репозиторий"),
                "folder",
                ProductAction::AddProject,
                true,
                cx,
            ))
            .child(self.start_tile(
                "welcome-create-project",
                ("Создать проект", "Новая папка с Git"),
                "plus",
                ProductAction::CreateProject,
                false,
                cx,
            ));
        div()
            .id("product-empty-state")
            .accessibility_id("product-empty-state")
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .px(px(28.))
            .child(
                div()
                    .id("product-welcome")
                    .max_w(px(540.))
                    .w_full()
                    .flex()
                    .flex_col()
                    .items_center()
                    .child(
                        div()
                            .mb(px(20.))
                            .child(chrome::icon("logo-p", 42., chrome::ACCENT)),
                    )
                    .child(
                        div()
                            .id("welcome-title")
                            .role(Role::Heading)
                            .aria_level(2)
                            .text_size(px(if self.compact { 23. } else { 27. }))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(colors.text)
                            .child("Ваше рабочее пространство"),
                    )
                    .child(
                        div()
                            .mt(px(10.))
                            .text_size(px(14.))
                            .text_color(colors.muted)
                            .child("CLI-агенты и терминалы. В одном проекте."),
                    )
                    .child(actions)
                    .child(
                        div()
                            .mt(px(28.))
                            .pt(px(18.))
                            .w_full()
                            .border_t_1()
                            .border_color(colors.border)
                            .flex()
                            .justify_center()
                            .flex_wrap()
                            .gap_3()
                            .children(
                                ["Claude Code", "Codex", "OpenCode", "Shell"]
                                    .into_iter()
                                    .map(|label| {
                                        div()
                                            .text_size(px(11.))
                                            .text_color(colors.muted)
                                            .child(label)
                                    }),
                            ),
                    )
                    .child(
                        div()
                            .mt(px(32.))
                            .text_size(px(10.))
                            .text_color(colors.muted)
                            .child("Ctrl+Shift+Space, P   Все команды"),
                    ),
            )
            .into_any_element()
    }

    fn start_tile(
        &self,
        id: &str,
        copy: (&str, &str),
        icon: &'static str,
        action: ProductAction,
        primary: bool,
        cx: &mut Context<CanvasApp>,
    ) -> AnyElement {
        let (label, detail) = copy;
        let colors = ProductColors::dark();
        div()
            .id(id.to_owned())
            .role(Role::Button)
            .aria_label(label.to_owned())
            .flex_1()
            .min_w_0()
            .h(px(86.))
            .px(px(18.))
            .flex()
            .items_center()
            .gap_3()
            .rounded(px(9.))
            .border_1()
            .border_color(if primary {
                colors.accent
            } else {
                colors.border
            })
            .bg(if primary {
                colors.accent
            } else {
                colors.raised
            })
            .hover(move |style| {
                style.bg(if primary {
                    colors.accent.opacity(0.9)
                } else {
                    colors.hover
                })
            })
            .child(chrome::icon(
                icon,
                22.,
                if primary { chrome::BASE } else { chrome::MUTED },
            ))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .font_weight(FontWeight::MEDIUM)
                            .text_size(px(13.))
                            .text_color(if primary { colors.base } else { colors.text })
                            .child(label.to_owned()),
                    )
                    .child(
                        div()
                            .text_size(px(10.))
                            .text_color(if primary { colors.base } else { colors.muted })
                            .child(detail.to_owned()),
                    ),
            )
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                this.product_action(action.clone(), window, cx);
                cx.stop_propagation();
            }))
            .into_any_element()
    }

    fn provider_choice(
        &self,
        provider: &str,
        label: &str,
        cx: &mut Context<CanvasApp>,
    ) -> AnyElement {
        let colors = ProductColors::dark();
        if provider == "shell" {
            return self.action_control(
                format!("launch-{provider}"),
                label.into(),
                ProductAction::Launch(provider.into()),
                ControlStyle {
                    role: Role::Button,
                    selected: false,
                    compact: false,
                },
                cx,
            );
        }
        let Some(providers) = &self.providers else {
            let message = self
                .provider_error
                .clone()
                .unwrap_or_else(|| "Проверяем CLI…".into());
            return div()
                .id(format!("provider-{provider}-checking"))
                .h(px(30.))
                .flex()
                .items_center()
                .text_size(px(12.))
                .text_color(colors.muted)
                .child(format!("{label} · {message}"))
                .into_any_element();
        };
        let info = providers.iter().find(|info| info.provider == provider);
        let Some(info) = info else {
            return div()
                .id(format!("provider-{provider}-missing"))
                .h(px(30.))
                .flex()
                .items_center()
                .text_size(px(12.))
                .text_color(colors.muted)
                .child(format!("{label} · проверка недоступна"))
                .into_any_element();
        };
        if info.available {
            let version = info
                .version
                .as_deref()
                .map(|version| format!(" · {version}"))
                .unwrap_or_default();
            self.action_control(
                format!("launch-{provider}"),
                format!("{label}{version}"),
                ProductAction::Launch(provider.into()),
                ControlStyle {
                    role: Role::Button,
                    selected: false,
                    compact: false,
                },
                cx,
            )
        } else {
            let reason = info.reason.as_deref().unwrap_or("CLI не установлен");
            div()
                .id(format!("provider-{provider}-unavailable"))
                .role(Role::Status)
                .h(px(36.))
                .flex()
                .flex_col()
                .justify_center()
                .text_size(px(11.))
                .text_color(colors.muted)
                .child(format!("{label} · {reason}"))
                .into_any_element()
        }
    }

    fn tab_control(&self, tab: &Tab, selected: bool, cx: &mut Context<CanvasApp>) -> AnyElement {
        let colors = ProductColors::dark();
        let id = tab.id;
        div()
            .id(format!("tab-{id}"))
            .role(Role::Tab)
            .aria_label(tab.name.clone())
            .aria_selected(selected)
            .h_full()
            .max_w(px(180.))
            .px(px(12.))
            .flex_shrink_0()
            .flex()
            .items_center()
            .border_b_2()
            .border_color(if selected { colors.accent } else { colors.base })
            .bg(if selected { colors.raised } else { colors.base })
            .rounded_t(px(5.))
            .text_size(px(12.))
            .text_color(if selected {
                colors.accent
            } else {
                colors.muted
            })
            .hover(|style| style.bg(colors.hover))
            .child(tab.name.clone())
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                this.product_action(ProductAction::SelectTab(id), window, cx);
                cx.stop_propagation();
            }))
            .into_any_element()
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
        let primary = matches!(
            id.as_str(),
            "product-dialog-submit" | "launch-review-confirm" | "resume-review-confirm"
        ) || matches!(&action, ProductAction::OpenLauncher)
            || matches!(&action, ProductAction::AddProject) && style.selected;
        let foreground = if primary {
            colors.base
        } else if style.selected {
            colors.text
        } else {
            colors.muted
        };
        let background = if primary {
            colors.accent
        } else if style.selected {
            colors.selected
        } else {
            colors.base.opacity(0.)
        };
        let icon = match &action {
            ProductAction::AddProject | ProductAction::SelectProject(_) => Some("folder"),
            ProductAction::CreateProject
            | ProductAction::OpenLauncher
            | ProductAction::CreateTab => Some("plus"),
            ProductAction::SelectWorkspace(_) if self.sidebar_compact => None,
            ProductAction::SelectWorkspace(_)
            | ProductAction::CreateWorkspace
            | ProductAction::CreateWorkspaceFor(_) => Some("branch"),
            ProductAction::Launch(_) | ProductAction::ReviewLaunch(_) => Some("terminal"),
            ProductAction::EditContext | ProductAction::EditProjectContext => Some("layers"),
            _ => None,
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
            .gap_2()
            .when(!style.compact, |row| row.w_full())
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
            .hover(move |style| style.bg(if primary { colors.accent } else { colors.hover }))
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                this.product_action(action.clone(), window, cx);
                cx.stop_propagation();
            }))
            .children(icon.map(|name| {
                chrome::icon(
                    name,
                    14.,
                    if primary { chrome::BASE } else { chrome::MUTED },
                )
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
        if dialog.kind == DialogKind::ResumeReview {
            return self.resume_review_overlay(dialog, cx);
        }
        if dialog.kind == DialogKind::SessionContext {
            return self.session_context_overlay(cx);
        }
        if matches!(dialog.kind, DialogKind::Palette | DialogKind::PaneMenu) {
            return self.palette_overlay(dialog, layouts, cx);
        }
        if dialog.kind == DialogKind::Launcher {
            return self.launcher_overlay(cx);
        }
        if dialog.kind == DialogKind::Transfer {
            return self.transfer_overlay(cx);
        }
        if dialog.kind == DialogKind::Sessions {
            return self.sessions_overlay(cx);
        }
        if dialog.kind == DialogKind::History {
            return self.history_overlay(dialog, layouts, cx);
        }

        let can_bind = dialog.kind != DialogKind::Conversation
            || self.launch_preview.as_ref().is_some_and(|preview| {
                matches!(preview.provider.as_str(), "codex" | "claude" | "opencode")
            });
        let mut fields = div().flex().flex_col().gap_2().mt(px(16.));
        if dialog.kind == DialogKind::Conversation
            && let Some(preview) = &self.launch_preview
        {
            let binding_status = preview
                .conversation
                .as_ref()
                .map(|binding| {
                    format!(
                        "Ручная привязка v{} · указана вами · {}",
                        binding.revision, binding.conversation_id,
                    )
                })
                .unwrap_or_else(|| "Диалог ещё не привязан".into());
            fields = fields.child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .text_size(px(12.))
                    .text_color(colors.muted)
                    .child(format!(
                        "{} · CLI {}",
                        preview.provider, preview.cli_version
                    ))
                    .child(format!(
                        "Программа: {}",
                        preview
                            .executable
                            .as_ref()
                            .map(|path| path.display().to_string())
                            .unwrap_or_else(|| "неизвестна для старого host".into())
                    ))
                    .child(format!("Сессия {}", preview.session))
                    .child(format!("Поколение {}", preview.generation))
                    .child(format!("Контекст {}", preview.context.id))
                    .child(format!(
                        "Рабочая папка: {}",
                        preview
                            .cwd
                            .as_ref()
                            .map(|path| path.display().to_string())
                            .unwrap_or_else(|| "неизвестна для старого host".into())
                    ))
                    .child(binding_status),
            );
        }
        for (index, label) in dialog.kind.fields().iter().enumerate() {
            if !can_bind {
                break;
            }
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

        if dialog.kind == DialogKind::CreateProject {
            fields = fields.child(self.action_control(
                "choose-project-parent".into(),
                "Выбрать папку…".into(),
                ProductAction::ChooseProjectParent,
                ControlStyle {
                    role: Role::Button,
                    selected: false,
                    compact: true,
                },
                cx,
            ));
        }
        if dialog.kind == DialogKind::CreateWorkspace {
            let project_label = self
                .workspace_project
                .and_then(|project_id| {
                    self.catalog
                        .as_ref()?
                        .projects
                        .iter()
                        .find(|project| project.id == project_id)
                })
                .map(|project| format!("Проект: {}", project.name))
                .unwrap_or_else(|| "Проект выбран в списке слева.".into());
            fields = fields
                .child(
                    div()
                        .text_size(px(11.))
                        .text_color(colors.muted)
                        .child(project_label),
                )
                .child(
                    div()
                        .mt(px(6.))
                        .text_size(px(12.))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(colors.text)
                        .child("Рабочая папка"),
                )
                .child(
                    div()
                        .text_size(px(11.))
                        .text_color(colors.muted)
                        .child("Выберите, где хранить файлы задачи."),
                )
                .child(
                    div()
                        .flex()
                        .gap_2()
                        .child(self.action_control(
                            "workspace-shared".into(),
                            "В папке проекта".into(),
                            ProductAction::WorkspaceIsolation(false),
                            ControlStyle {
                                role: Role::Button,
                                selected: !self.workspace_isolated,
                                compact: true,
                            },
                            cx,
                        ))
                        .child(self.action_control(
                            "workspace-isolated".into(),
                            "Отдельная ветка / worktree".into(),
                            ProductAction::WorkspaceIsolation(true),
                            ControlStyle {
                                role: Role::Button,
                                selected: self.workspace_isolated,
                                compact: true,
                            },
                            cx,
                        )),
                )
                .child(div().text_size(px(11.)).text_color(colors.muted).child(
                    if self.workspace_isolated {
                        "Изолированная рабочая копия для этой задачи."
                    } else {
                        "Работа идёт в папке выбранного проекта."
                    },
                ));
        }

        if dialog.kind == DialogKind::Generations
            && let Some(page) = &self.generation_page
        {
            let mode = if page.pinned.is_some() {
                "Выбрано конкретное поколение"
            } else {
                "Автоматически показывается последнее поколение"
            };
            fields = fields.child(div().text_size(px(12.)).text_color(colors.muted).child(mode))
                .child(div().text_size(px(11.)).text_color(colors.muted).child("Введите номер строки или выберите её мышью. Просмотр не запускает CLI. История не принимает ввод."));
            let mut entries = div()
                .id("generation-entries")
                .role(Role::List)
                .aria_label("Поколения выбранной панели")
                .flex()
                .flex_col()
                .gap_1()
                .max_h(px(240.))
                .overflow_y_scroll();
            for (index, entry) in page.entries.iter().enumerate() {
                let state = match entry.state.as_str() {
                    "running" => "работает",
                    "exited" => "завершён",
                    "history" => "история",
                    "launch_outcome_unknown" => "исход запуска неизвестен",
                    "history_unavailable" => "история недоступна",
                    "degraded" => "ошибка терминала",
                    _ => "нет подтверждения запуска",
                };
                let order = if entry.ordinal == 0 {
                    "порядок неизвестен".into()
                } else {
                    format!("поколение {}", entry.ordinal)
                };
                let label = format!(
                    "{}. {} · {} · {}{}",
                    index + 1,
                    order,
                    crate::protocol::generation_label(&entry.generation.to_string()),
                    state,
                    if Some(entry.generation) == page.head {
                        " · последнее"
                    } else {
                        ""
                    }
                );
                let id = format!("generation-{}", entry.generation);
                if entry.has_frame {
                    entries = entries.child(self.action_control(
                        id,
                        label,
                        ProductAction::SelectGeneration(Some(entry.generation)),
                        ControlStyle {
                            role: Role::ListBoxOption,
                            selected: page.pinned.or(page.head) == Some(entry.generation),
                            compact: false,
                        },
                        cx,
                    ));
                } else {
                    entries = entries.child(
                        div()
                            .id(id)
                            .role(Role::Status)
                            .px(px(10.))
                            .py(px(5.))
                            .text_size(px(12.))
                            .text_color(colors.muted)
                            .child(label),
                    );
                }
            }
            if page.entries.is_empty() {
                entries = entries.child("У этой панели пока нет доступных поколений.");
            }
            fields = fields.child(entries);
            if page.head.is_some() {
                fields = fields.child(self.action_control(
                    "generation-follow-head".into(),
                    "Показывать последнее поколение".into(),
                    ProductAction::SelectGeneration(None),
                    ControlStyle {
                        role: Role::Button,
                        selected: page.pinned.is_none(),
                        compact: true,
                    },
                    cx,
                ));
            }
            fields = fields.child(self.action_control(
                "generation-refresh".into(),
                "Последние поколения".into(),
                ProductAction::LoadGenerations(None),
                ControlStyle {
                    role: Role::Button,
                    selected: false,
                    compact: true,
                },
                cx,
            ));
            if let Some(before) = page.next_before {
                fields = fields.child(self.action_control(
                    "generation-older".into(),
                    "Более ранние поколения".into(),
                    ProductAction::LoadGenerations(Some(before)),
                    ControlStyle {
                        role: Role::Button,
                        selected: false,
                        compact: true,
                    },
                    cx,
                ));
            }
        }

        if dialog.kind == DialogKind::Conversation && can_bind {
            fields = fields.child(div().id("conversation-binding-note").role(Role::Status)
                .text_size(px(12.)).text_color(colors.muted)
                .child("Скопируйте точный ID средствами CLI. Для Codex и Claude нужен UUID; для OpenCode — ses_…. Привязка сохраняет ваш выбор, но не проверяет существование беседы и не запускает процесс. Содержимое и авторизация CLI не копируются."))
                .child(div().id("conversation-resume-status").role(Role::Status)
                    .text_size(px(12.)).text_color(colors.accent)
                    .child(format!("Нативное продолжение по текущему предпросмотру: {}. Подготовка заново проверит установленный CLI.", capability_label(self.launch_preview.as_ref().is_some_and(|preview| preview.capabilities.native_resume)))));
            fields = fields.child(self.action_control(
                "conversation-reload".into(),
                "Обновить сохранённую привязку".into(),
                ProductAction::InspectConversation,
                ControlStyle {
                    role: Role::Button,
                    selected: false,
                    compact: true,
                },
                cx,
            ));
            if self
                .launch_preview
                .as_ref()
                .is_some_and(|preview| preview.conversation.is_some())
            {
                fields = fields.child(self.action_control(
                    "conversation-prepare-resume".into(),
                    "Продолжить привязанный диалог CLI".into(),
                    ProductAction::PrepareResume,
                    ControlStyle {
                        role: Role::Button,
                        selected: false,
                        compact: true,
                    },
                    cx,
                ));
            }
        } else if dialog.kind == DialogKind::Conversation {
            fields = fields.child(div().id("conversation-unsupported").role(Role::Status).text_size(px(12.)).text_color(colors.muted).child("У обычного shell нет диалога провайдера. Его история хранится как терминальное поколение."));
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

        let submit = if !can_bind {
            div().into_any_element()
        } else {
            self.action_control(
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
            )
        };
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

    fn launcher_overlay(&self, cx: &mut Context<CanvasApp>) -> AnyElement {
        let colors = ProductColors::dark();
        let mut choices = div()
            .id("agent-launcher-choices")
            .mt(px(16.))
            .flex()
            .flex_col()
            .gap_2();
        for (provider, label) in [
            ("claude", "Claude Code"),
            ("codex", "Codex"),
            ("opencode", "OpenCode"),
            ("shell", "Терминал"),
        ] {
            choices = choices.child(
                div()
                    .id(format!("launcher-choice-{provider}"))
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(self.provider_choice(provider, label, cx)),
            );
        }
        let availability = if self.providers.is_none() {
            self.provider_error
                .as_deref()
                .unwrap_or("Проверяем доступность CLI…")
        } else {
            "Недоступные CLI показаны с причиной установки."
        };
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
                this.product_action(ProductAction::CloseDialog, window, cx)
            }))
            .child(
                div()
                    .id("agent-launcher-dialog")
                    .role(Role::Dialog)
                    .aria_label("Добавить агента")
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
                    .on_click(cx.listener(|_, _: &ClickEvent, _, cx| cx.stop_propagation()))
                    .child(
                        div()
                            .id("provider-launcher-title")
                            .role(Role::Heading)
                            .aria_level(2)
                            .text_size(px(16.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(colors.text)
                            .child("Добавить агента"),
                    )
                    .child(
                        div()
                            .mt(px(6.))
                            .text_size(px(12.))
                            .text_color(colors.muted)
                            .child("Выберите CLI для новой терминальной панели."),
                    )
                    .child(
                        div()
                            .mt(px(3.))
                            .text_size(px(11.))
                            .text_color(colors.muted)
                            .child(availability.to_owned()),
                    )
                    .child(choices)
                    .child(div().mt(px(16.)).child(self.action_control(
                        "launcher-close".into(),
                        "Закрыть".into(),
                        ProductAction::CloseDialog,
                        ControlStyle {
                            role: Role::Button,
                            selected: false,
                            compact: true,
                        },
                        cx,
                    ))),
            )
            .into_any_element()
    }

    fn resume_review_overlay(&self, _dialog: &Dialog, cx: &mut Context<CanvasApp>) -> AnyElement {
        let colors = ProductColors::dark();
        let preview = self.launch_preview.as_ref();
        let can_confirm = preview
            .is_some_and(|preview| preview.can_launch && preview.resume.is_some())
            && self.launch_phase.is_none()
            && !self.busy;
        let mut details = div()
            .id("resume-review-details")
            .flex()
            .flex_col()
            .gap_1()
            .text_size(px(12.))
            .text_color(colors.muted);
        let mut context_blocks = div()
            .id("resume-review-context-empty")
            .flex()
            .flex_col()
            .gap_2();
        let mut warnings = div().id("resume-review-warnings").flex().flex_col().gap_1();
        if let Some(preview) = preview {
            if let Some(source) = &preview.resume {
                let binding = &source.binding;
                details = details
                    .child(format!("Диалог: {}", binding.conversation_id))
                    .child(format!(
                        "Провайдер и CLI: {} {}",
                        preview.provider, preview.cli_version
                    ))
                    .child(format!(
                        "Рабочая папка: {}",
                        preview
                            .cwd
                            .as_deref()
                            .map(|path| path.display().to_string())
                            .unwrap_or_else(|| "неизвестна".into())
                    ))
                    .child(format!(
                        "Программа: {}",
                        preview
                            .executable
                            .as_deref()
                            .map(|path| path.display().to_string())
                            .unwrap_or_else(|| "неизвестна".into())
                    ))
                    .child(format!(
                        "Привязка: ревизия {} · исходное поколение {}",
                        binding.revision, binding.source_generation
                    ))
                    .child(format!("Предыдущее поколение: {}", source.previous_head))
                    .child(format!(
                        "Контекст: {} · hash {}",
                        preview.context.id, preview.context.content_hash
                    ))
                    .child(format!("Новое поколение: {}", preview.generation));
                context_blocks = effective_context_blocks(
                    &preview.context,
                    "resume-review-context-blocks",
                    "Замороженные блоки контекста продолжения",
                    250.,
                    self.catalog.as_ref(),
                );
            } else {
                details = details.child("Предпросмотр продолжения недоступен.");
                context_blocks = context_blocks.child("Замороженный контекст недоступен.");
            }
            if let Some(reason) = &preview.unavailable_reason {
                warnings = warnings.child(
                    div()
                        .id("resume-review-unavailable-reason")
                        .role(Role::Status)
                        .text_size(px(12.))
                        .text_color(colors.accent)
                        .child(reason.clone()),
                );
            }
            for (index, warning) in preview.warnings.iter().enumerate() {
                warnings = warnings.child(
                    div()
                        .id(format!("resume-review-warning-{index}"))
                        .role(Role::Status)
                        .text_size(px(11.))
                        .text_color(colors.muted)
                        .child(warning.clone()),
                );
            }
            if !preview.can_launch && preview.unavailable_reason.is_none() {
                warnings = warnings.child(
                    div()
                        .id("resume-review-unavailable")
                        .role(Role::Status)
                        .text_size(px(12.))
                        .text_color(colors.accent)
                        .child("Продолжение сейчас недоступно."),
                );
            }
        } else {
            details = details.child("Подготовка продолжения ещё не загружена.");
            context_blocks = context_blocks
                .child("Обновите подготовку, чтобы проверить привязку и замороженный контекст.");
            warnings = warnings.child("Продолжение недоступно без актуальной подготовки.");
        }

        let confirm = if can_confirm {
            self.action_control(
                "resume-review-confirm".into(),
                DialogKind::ResumeReview.submit_label().into(),
                ProductAction::SubmitDialog,
                ControlStyle {
                    role: Role::Button,
                    selected: true,
                    compact: true,
                },
                cx,
            )
        } else {
            div()
                .id("resume-review-confirm-unavailable")
                .role(Role::Status)
                .aria_label("Продолжение недоступно")
                .px(px(9.))
                .py(px(6.))
                .rounded(px(6.))
                .bg(colors.base)
                .text_size(px(12.))
                .text_color(colors.muted)
                .child(DialogKind::ResumeReview.submit_label())
                .into_any_element()
        };
        let close = self.action_control(
            "resume-review-close".into(),
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
            .id("resume-review-overlay")
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
                    .id("resume-review-sheet")
                    .role(Role::Dialog)
                    .aria_label(DialogKind::ResumeReview.title())
                    .w_full()
                    .max_w(px(760.))
                    .mx(px(16.))
                    .flex()
                    .flex_col()
                    .gap_3()
                    .rounded(px(12.))
                    .border_1()
                    .border_color(colors.border)
                    .bg(colors.raised)
                    .p(px(18.))
                    .on_click(cx.listener(|_, _: &ClickEvent, _, cx| cx.stop_propagation()))
                    .child(
                        div()
                            .id("resume-review-title")
                            .role(Role::Heading)
                            .aria_level(2)
                            .text_size(px(17.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(DialogKind::ResumeReview.title()),
                    )
                    .child(details)
                    .child(context_blocks)
                    .child(warnings)
                    .child(
                        div()
                            .flex()
                            .justify_end()
                            .gap_2()
                            .child(close)
                            .child(confirm),
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
                && crate::memory_choice::matches_preview(
                    dialog.fields.get(2).map(EditBuffer::text).unwrap_or(""),
                    preview.memory.as_ref(),
                    preview.include_memory,
                )
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
            metadata = metadata.child(format!(
                "{} · CLI {}",
                preview.provider, preview.cli_version
            ));
            let details = div()
                .id("launch-review-details")
                .flex()
                .flex_col()
                .gap_1()
                .text_size(px(11.))
                .text_color(colors.muted)
                .child(format!(
                    "Адаптер v{} · цель: {target} · план {}",
                    preview.adapter_version, preview.plan_id
                ))
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
            preview_controls = preview_controls.child(
                self.action_control(
                    "launch-review-details-toggle".into(),
                    if self.launch_details {
                        "Скрыть подробности"
                    } else {
                        "Технические подробности"
                    }
                    .into(),
                    ProductAction::ToggleLaunchDetails,
                    ControlStyle {
                        role: Role::Button,
                        selected: false,
                        compact: true,
                    },
                    cx,
                ),
            );
            if self.launch_details {
                metadata = metadata.child(details);
                if let Some(cwd) = &preview.cwd {
                    metadata = metadata.child(format!("Рабочая папка: {}", cwd.display()));
                }
                if let Some(executable) = &preview.executable {
                    metadata = metadata.child(format!("Программа: {}", executable.display()));
                }
            }
            context_blocks = effective_context_blocks(
                &preview.context,
                "launch-review-context-blocks",
                "Блоки эффективного контекста",
                240.,
                self.catalog.as_ref(),
            );

            let memory_label = if dialog
                .fields
                .get(2)
                .is_some_and(|field| !field.text().trim().is_empty())
            {
                "Исключить память"
            } else {
                "Выбрать всю память"
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
            (2usize, "Разделы памяти: 1, 3 · пусто — без памяти"),
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

        let mut memory_picker = div()
            .id("launch-memory-picker")
            .flex()
            .flex_col()
            .gap_1()
            .mt(px(8.));
        if let Some(memory) = preview.and_then(|preview| preview.memory.as_ref()) {
            let draft = dialog.fields.get(2).map(EditBuffer::text).unwrap_or("");
            let selected = crate::memory_choice::parse_sections(draft, memory);
            let summary = match &selected {
                Ok(selected) => format!(
                    "Память v{} · {} из {} разделов · {} / {} байт UTF-8",
                    memory.revision,
                    selected.len(),
                    memory.sections.len(),
                    crate::memory_choice::selected_bytes(selected, memory),
                    crate::memory_choice::LIMIT,
                ),
                Err(error) => error.clone(),
            };
            memory_picker = memory_picker.child(
                div()
                    .id("launch-memory-counter")
                    .role(Role::Status)
                    .text_size(px(11.))
                    .text_color(colors.muted)
                    .child(summary),
            );
            if let Some(reason) = &memory.unavailable_reason {
                memory_picker = memory_picker.child(
                    div()
                        .text_size(px(12.))
                        .text_color(colors.muted)
                        .child(reason.clone()),
                );
            } else if memory.sections.is_empty() {
                memory_picker =
                    memory_picker.child(div().text_size(px(12.)).text_color(colors.muted).child(
                        "Память пока пуста. Добавьте Markdown-разделы в контекст воркспейса.",
                    ));
            } else {
                let mut sections = div()
                    .id("launch-memory-sections")
                    .role(Role::Group)
                    .aria_label("Выбор разделов памяти")
                    .flex()
                    .flex_col()
                    .gap_1()
                    .max_h(px(144.))
                    .overflow_y_scroll();
                for section in &memory.sections {
                    let index = section.index;
                    let checked = selected
                        .as_ref()
                        .is_ok_and(|selected| selected.contains(&index));
                    let title = if section.title.is_empty() {
                        "Без заголовка"
                    } else {
                        &section.title
                    };
                    let label = format!("{}. {} · {} байт", index + 1, title, section.bytes);
                    sections = sections.child(
                        div()
                            .id(format!("launch-memory-section-{index}"))
                            .role(Role::CheckBox)
                            .aria_label(label.clone())
                            .aria_toggled(if checked {
                                gpui::Toggled::True
                            } else {
                                gpui::Toggled::False
                            })
                            .flex()
                            .items_center()
                            .gap_2()
                            .px(px(8.))
                            .py(px(5.))
                            .rounded(px(5.))
                            .bg(if checked {
                                colors.selected
                            } else {
                                colors.base
                            })
                            .text_size(px(12.))
                            .text_color(colors.text)
                            .hover(move |style| style.bg(colors.hover))
                            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                this.product_action(
                                    ProductAction::ToggleMemorySection(index),
                                    window,
                                    cx,
                                );
                                cx.stop_propagation();
                            }))
                            .child(if checked { "☑" } else { "☐" })
                            .child(div().truncate().child(label)),
                    );
                }
                memory_picker = memory_picker.child(sections);
            }
            if let Err(error) = crate::memory_choice::selection(draft, memory) {
                memory_picker = memory_picker.child(
                    div()
                        .id("launch-memory-error")
                        .role(Role::Status)
                        .text_size(px(12.))
                        .text_color(colors.accent)
                        .child(error),
                );
            }
            memory_picker = memory_picker.child(div().text_size(px(11.)).text_color(colors.muted)
                .child("Разделы по заголовкам # и ##; вложенные заголовки остаются внутри. Tab → номера → Ctrl+R. Ctrl+Shift+R — загрузить новую ревизию и сбросить выбор."));
            memory_picker = memory_picker.child(self.action_control(
                "launch-memory-reload".into(),
                "Обновить ревизию памяти · сбросить выбор".into(),
                ProductAction::ReloadMemory,
                ControlStyle {
                    role: Role::Button,
                    selected: false,
                    compact: true,
                },
                cx,
            ));
        }

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
            .overflow_hidden()
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
                    .h_full()
                    .min_h(px(0.))
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
                            .flex_shrink_0()
                            .mb(px(8.))
                            .text_color(colors.text)
                            .child("Проверьте запуск до старта"),
                    )
                    .child(
                        div()
                            .id("launch-review-body")
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_h(px(0.))
                            .overflow_y_scroll()
                            .child(metadata.flex_shrink_0())
                            .child(preview_controls.flex_shrink_0())
                            .child(fields.flex_shrink_0())
                            .child(memory_picker.flex_shrink_0())
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
                                    .flex_shrink_0()
                                    .child("Эффективный контекст · блоки по порядку"),
                            )
                            .child(context_blocks.flex_shrink_0())
                            .child(warnings.flex_shrink_0()),
                    )
                    .child(
                        div()
                            .flex()
                            .justify_end()
                            .flex_shrink_0()
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
                .child(format!("Диалог CLI: {}", preview.conversation.as_ref().map(|binding| binding.conversation_id.as_str()).unwrap_or("не привязан")))
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

    fn history_overlay(
        &self,
        dialog: &Dialog,
        layouts: &BTreeMap<usize, FieldLayout>,
        cx: &mut Context<CanvasApp>,
    ) -> AnyElement {
        let colors = ProductColors::dark();
        let Some(history) = &self.history_sheet else {
            return div()
                .id("history-sheet-missing")
                .child("История недоступна")
                .into_any_element();
        };
        let query = dialog.fields.first().map(EditBuffer::text).unwrap_or("");
        let page = history.page.as_ref();
        let header = page.map(|page| {
            format!(
                "Сессия {} · поколение {} · sequence {} · {} строк · начало страницы: строка {}, байт {}",
                history.session, history.generation, page.sequence, page.total_rows,
                page.start.row.saturating_add(1), page.start.offset
            )
        }).unwrap_or_else(|| format!("Сессия {} · поколение {}", history.session, history.generation));
        let status = history.error.as_deref().unwrap_or(&history.status);
        let boundary_notices = page
            .map(|page| crate::history_view::boundary_messages(&page.rows))
            .unwrap_or_default();
        let mut notices = div().flex().flex_col().gap_1();
        for (index, message) in boundary_notices.into_iter().take(32).enumerate() {
            notices = notices.child(
                div()
                    .id(format!("history-boundary-{index}"))
                    .role(Role::Status)
                    .text_color(colors.accent)
                    .text_size(px(11.))
                    .child(message),
            );
        }
        if let Some(page) = page {
            for (index, warning) in page.warnings.iter().take(8).enumerate() {
                let shown: String = warning.chars().take(240).collect();
                notices = notices.child(
                    div()
                        .id(format!("history-warning-{index}"))
                        .role(Role::Status)
                        .text_color(colors.muted)
                        .text_size(px(11.))
                        .child(format!("Восстановление: {shown}")),
                );
            }
            if page.warnings.len() > 8 {
                notices = notices.child(
                    div()
                        .id("history-warnings-more")
                        .role(Role::Status)
                        .text_color(colors.muted)
                        .text_size(px(11.))
                        .child(format!("Ещё предупреждений: {}", page.warnings.len() - 8)),
                );
            }
        }
        let body = dialog.fields.get(1).map(EditBuffer::text).unwrap_or("");
        let mut controls = div().flex().flex_wrap().gap_1();
        for (id, label, action, enabled) in [
            (
                "history-start",
                "Начало",
                ProductAction::HistoryStart,
                page.is_some() && !self.busy,
            ),
            (
                "history-previous",
                "Предыдущая",
                ProductAction::HistoryPrevious,
                !self.busy
                    && (page.is_some_and(|p| p.previous.is_some()) || !history.back.is_empty()),
            ),
            (
                "history-next",
                "Следующая",
                ProductAction::HistoryNext,
                !self.busy && page.is_some_and(|p| p.next.is_some()),
            ),
            (
                "history-end",
                "Конец",
                ProductAction::HistoryEnd,
                page.is_some() && !self.busy,
            ),
            (
                "history-refresh",
                "Обновить",
                ProductAction::HistoryRefresh,
                !self.busy,
            ),
            (
                "history-search",
                "Найти",
                ProductAction::HistorySearch,
                !self.busy && !query.is_empty(),
            ),
            (
                "history-next-match",
                "Следующее совпадение",
                ProductAction::HistoryNextMatch,
                history.search_next.is_some() && !self.busy,
            ),
            (
                "history-copy",
                "Копировать страницу",
                ProductAction::HistoryCopyPage,
                !body.is_empty(),
            ),
        ] {
            controls = controls.child(if enabled {
                self.action_control(
                    id.into(),
                    label.into(),
                    action,
                    ControlStyle {
                        role: Role::Button,
                        selected: false,
                        compact: true,
                    },
                    cx,
                )
            } else {
                div()
                    .id(format!("{id}-disabled"))
                    .role(Role::Status)
                    .h(px(28.))
                    .px(px(9.))
                    .flex()
                    .items_center()
                    .text_size(px(12.))
                    .text_color(colors.muted)
                    .child(label)
                    .into_any_element()
            });
        }
        let query_field = div()
            .id("history-query-field")
            .role(Role::TextInput)
            .aria_label("Найти текст в снимке истории")
            .w_full()
            .h(px(40.))
            .px(px(10.))
            .py(px(5.))
            .rounded(px(6.))
            .border_1()
            .border_color(if dialog.active_field == 0 {
                colors.accent
            } else {
                colors.border
            })
            .bg(colors.base)
            .text_color(colors.text)
            .overflow_hidden()
            .child(crate::edit_field::content(
                &dialog.fields[0],
                crate::edit_field::FieldOptions {
                    cache: layouts.get(&0),
                    dialog_id: dialog.id,
                    index: 0,
                    active: dialog.active_field == 0,
                    multiline: false,
                    placeholder: "Найти текст",
                    accent: colors.accent,
                    muted: colors.muted,
                },
                cx,
            ))
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(|this, event, window, cx| {
                    this.editor_pointer_down(0, event, window, cx);
                }),
            );
        let body_content = div()
            .id("history-page-text")
            .accessibility_id("history-page-text")
            .role(Role::Document)
            .aria_label("Прочитанная страница истории")
            .w_full()
            .min_h(px(180.))
            .max_h(px(440.))
            .overflow_y_scroll()
            .bg(colors.base)
            .border_1()
            .border_color(if dialog.active_field == 1 {
                colors.accent
            } else {
                colors.border
            })
            .rounded(px(6.))
            .p(px(10.))
            .text_size(px(12.))
            .text_color(colors.text)
            .child(crate::edit_field::content(
                &dialog.fields[1],
                crate::edit_field::FieldOptions {
                    cache: layouts.get(&1),
                    dialog_id: dialog.id,
                    index: 1,
                    active: dialog.active_field == 1,
                    multiline: true,
                    placeholder: "История",
                    accent: colors.accent,
                    muted: colors.muted,
                },
                cx,
            ))
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(|this, event, window, cx| {
                    this.editor_pointer_down(1, event, window, cx);
                }),
            )
            .on_scroll_wheel(
                cx.listener(|this, event, window, cx| this.editor_wheel(1, event, window, cx)),
            );
        let close = self.action_control(
            "history-close".into(),
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
            .id("history-view-overlay")
            .accessibility_id("history-view-overlay")
            .absolute()
            .inset_0()
            .flex()
            .items_start()
            .justify_center()
            .overflow_y_scroll()
            .py(px(18.))
            .bg(rgb(0x090b0e).opacity(0.72))
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                this.product_action(ProductAction::CloseDialog, window, cx)
            }))
            .child(
                div()
                    .id("history-view-sheet")
                    .role(Role::Dialog)
                    .aria_label("История сессии")
                    .w_full()
                    .max_w(px(1050.))
                    .mx(px(16.))
                    .flex()
                    .flex_col()
                    .gap_2()
                    .rounded(px(12.))
                    .border_1()
                    .border_color(colors.border)
                    .bg(colors.raised)
                    .p(px(16.))
                    .on_click(cx.listener(|_, _: &ClickEvent, _, cx| cx.stop_propagation()))
                    .child(
                        div()
                            .id("history-heading")
                            .role(Role::Heading)
                            .aria_level(2)
                            .text_size(px(17.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("История сессии"),
                    )
                    .child(
                        div()
                            .id("history-header")
                            .role(Role::Status)
                            .text_size(px(11.))
                            .text_color(colors.muted)
                            .child(header),
                    )
                    .child(query_field)
                    .child(controls)
                    .child(
                        div()
                            .id("history-status")
                            .role(Role::Status)
                            .text_size(px(11.))
                            .text_color(if history.error.is_some() {
                                colors.accent
                            } else {
                                colors.muted
                            })
                            .child(status.to_owned()),
                    )
                    .child(notices)
                    .child(body_content)
                    .child(div().id("history-keyboard-hint").role(Role::Status).text_size(px(11.)).text_color(colors.muted).child("Tab: поиск / текст · Ctrl+F: поиск · F3: дальше · Shift+F3: сначала · PageUp/Down: страницы · Ctrl+R: обновить · Ctrl+Shift+C: вся страница"))
                    .child(div().flex().justify_end().child(close)),
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
                .child(if dialog.kind == DialogKind::PaneMenu {
                    "Панели"
                } else {
                    "Команды"
                }),
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
            if dialog.kind == DialogKind::PaneMenu && !panel_action(action) {
                continue;
            }
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
    fn history_text_allows_selection_but_not_editing_and_query_stays_editable() {
        let mut ui = ProductUi {
            dialog: Some(Dialog::new(DialogKind::History)),
            ..Default::default()
        };
        let dialog = ui.dialog.as_mut().unwrap();
        dialog.fields[1] = "界e\u{301}\nreadonly".into();
        dialog.active_field = 1;
        ui.selection_editor_mut().unwrap().select_all();
        assert_eq!(
            ui.active_editor().unwrap().selected_text(),
            "界e\u{301}\nreadonly"
        );
        ui.insert_text("overwritten");
        assert!(ui.active_editor_mut().is_none());
        assert_eq!(ui.active_editor().unwrap().text(), "界e\u{301}\nreadonly");
        ui.dialog.as_mut().unwrap().active_field = 0;
        ui.insert_text("界");
        assert_eq!(ui.active_editor().unwrap().text(), "界");
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

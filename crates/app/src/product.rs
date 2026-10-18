//! Native PigADE product chrome. The host owns commands and durable state;
//! this module renders the current catalog and emits typed user actions.

use crate::edit_field::FieldLayout;
use gpui::{
    AnyElement, ClickEvent, Context, Div, FontWeight, Hsla, IntoElement, ParentElement, Role,
    Stateful, StatefulInteractiveElement, Styled, Toggled, canvas, div,
    prelude::{FluentBuilder, InteractiveElement},
    px, rgb,
};
use pigade_storage::{Catalog, ContextDocument, Tab, Workspace, WorkspaceStatus};
use std::collections::{BTreeMap, HashMap, HashSet};
use uuid::Uuid;

use super::{CanvasApp, chrome, protocol, text_edit::EditBuffer};

#[path = "appearance_ui.rs"]
mod appearance_ui;
#[path = "git_status_ui.rs"]
mod git_status_ui;
#[path = "navigation_ui.rs"]
mod navigation_ui;
#[path = "profile_ui.rs"]
mod profile_ui;
#[path = "prompt_ui.rs"]
mod prompt_ui;
#[path = "session_control_ui.rs"]
mod session_control_ui;
#[path = "template_ui.rs"]
mod template_ui;

const SIDEBAR_WIDTH: f32 = 220.0;
const ROW_HEIGHT: f32 = 30.0;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum ProductAction {
    OpenAppearance,
    SetTheme(crate::appearance::Theme),
    ToggleReducedMotion,
    AdjustTerminalFont(i8),
    ResetTerminalFont,
    ReloadAppearance,
    OpenCloseTab,
    CloseManagedTab,
    ConfirmCloseTab,
    InspectCloseTab,
    OpenPrompts,
    ReloadPrompts,
    NewPrompt,
    EditPrompt(Uuid),
    DeletePrompt(Uuid),
    UsePrompt(Uuid),
    CopyPrompt(Uuid),
    SaveDraftAsPrompt,
    OpenAgentProfiles,
    ReloadAgentProfiles,
    NewAgentProfile,
    EditAgentProfile(Uuid),
    LaunchAgentProfile(Uuid),
    DeleteAgentProfile(Uuid),
    SetProfileProvider(String),
    ToggleProfileContext,
    SaveLaunchAsProfile,
    OpenControls,
    OpenWorkspaceGit,
    RefreshWorkspaceGit,
    SetCommandModifier(crate::controls::Modifier),
    AddProject,
    ToggleSidebar,
    FocusPane(Uuid),
    RenamePane,
    ActivatePane { tab: Uuid, session: Uuid },
    MorePanelResults,
    FirstPanelResults,
    ReloadPanelResults,
    OpenProjectPath,
    CreateProject,
    ChooseProjectParent,
    CreateWorkspace,
    CreateWorkspaceFor(Uuid),
    WorkspaceIsolation(bool),
    CreateTab,
    ChoosePreset(crate::template_flow::Preset),
    ConfigureTemplate(Uuid),
    NewTabFromTemplate(Uuid),
    EditRecipe,
    EditTemplateSlot(usize),
    SetTemplateProvider(String),
    SetTemplateRole(String),
    ToggleTemplateContext,
    ClearTemplateLaunches,
    ConfirmTemplateRun,
    RefreshTemplateRun,
    CancelTemplateRun,
    InspectTemplateRunSlot(usize),
    ReturnTemplateRun,
    OpenTemplateRuns,
    LoadTemplateRuns(Option<u64>),
    SelectTemplateRun(Uuid),
    ManageSelected(crate::catalog_flow::EntityKind),
    ManageProject(Uuid),
    ManageWorkspace(Uuid),
    ManageTab(Uuid),
    RenameTarget,
    ArchiveTarget,
    ToggleArchived,
    OpenLayouts,
    SaveLayout,
    ApplyLayout(Uuid),
    DeleteLayout(Uuid),
    RefreshCatalog,
    OpenTransfer,
    RefreshTransfer,
    TransferTo(Uuid),
    TransferPage(usize),
    ConfirmTerminalInsert,
    OpenSessions,
    RefreshSessions,
    AttachSession(Uuid),
    OpenStop,
    StopSession { session: Uuid, generation: Uuid },
    ConfirmStop,
    InspectStop,
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
    Appearance,
    CloseTab,
    Prompts,
    Prompt,
    DeletePrompt,
    AgentProfiles,
    AgentProfile,
    DeleteAgentProfile,
    WorkspaceGit,
    Controls,
    AddProject,
    CreateProject,
    CreateWorkspace,
    Launcher,
    NewTab,
    Recipe,
    RecipeSlot,
    NewTemplateTab,
    TemplateRun,
    TemplateRunContext,
    TemplateRuns,
    Manage,
    Rename,
    RenamePane,
    Layouts,
    SaveLayout,
    DeleteLayout,
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
    StopSession,
    History,
    TerminalInsert,
}

impl DialogKind {
    fn title(self) -> &'static str {
        match self {
            Self::Appearance => "Внешний вид",
            Self::CloseTab => "Закрыть вкладку?",
            Self::Prompts => "Библиотека промптов",
            Self::Prompt => "Сохранённый промпт",
            Self::DeletePrompt => "Удалить промпт?",
            Self::AgentProfiles => "Профили агентов",
            Self::AgentProfile => "Настройки профиля",
            Self::DeleteAgentProfile => "Удалить профиль?",
            Self::WorkspaceGit => "Git воркспейса",
            Self::Controls => "Клавиша управления панелями",
            Self::AddProject => "Добавить проект",
            Self::CreateProject => "Создать проект",
            Self::CreateWorkspace => "Новый воркспейс",
            Self::Launcher => "Добавить агента",
            Self::NewTab => "Новая вкладка",
            Self::Recipe => "Агенты в шаблоне",
            Self::RecipeSlot => "Настройки агента",
            Self::NewTemplateTab => "Вкладка по шаблону",
            Self::TemplateRun => "Запуск вкладки",
            Self::TemplateRunContext => "Контекст запуска",
            Self::TemplateRuns => "Незавершённые вкладки",
            Self::Manage => "Управление",
            Self::Rename => "Переименовать",
            Self::RenamePane => "Имя панели",
            Self::Layouts => "Сохранённые раскладки",
            Self::SaveLayout => "Сохранить раскладку",
            Self::DeleteLayout => "Удалить сохранённую раскладку?",
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
            Self::StopSession => "Остановить CLI?",
            Self::History => "История сессии",
            Self::TerminalInsert => "Проверить вставку",
        }
    }

    fn fields(self) -> &'static [&'static str] {
        match self {
            Self::Appearance | Self::TerminalInsert => &[],
            Self::CloseTab => &[],
            Self::Prompts | Self::DeletePrompt => &[],
            Self::Prompt => &["Название", "Текст промпта"],
            Self::AgentProfiles | Self::DeleteAgentProfile => &[],
            Self::AgentProfile => &[
                "Название профиля",
                "Роль",
                "Инструкции",
                "Модель · пусто — настройки CLI",
                "Reasoning effort · пусто — настройки CLI",
            ],
            Self::WorkspaceGit => &[],
            Self::Controls => &[],
            Self::AddProject => &["Путь к Git-проекту"],
            Self::CreateProject => &["Название проекта", "Папка проекта"],
            Self::CreateWorkspace => &["Название задачи / воркспейса"],
            Self::Launcher => &[],
            Self::NewTab => &["Название вкладки"],
            Self::RecipeSlot => &[
                "Роль · инструкции агенту",
                "Дополнительные инструкции",
                "Модель · пусто — настройки CLI",
                "Reasoning effort · пусто — настройки CLI",
            ],
            Self::NewTemplateTab => &["Название вкладки"],
            Self::Recipe
            | Self::TemplateRun
            | Self::TemplateRunContext
            | Self::TemplateRuns
            | Self::StopSession => &[],
            Self::Rename | Self::SaveLayout => &["Название"],
            Self::RenamePane => &["Имя панели · пусто — название CLI"],
            Self::Manage | Self::Layouts | Self::DeleteLayout => &[],
            Self::Palette => &["Поиск команд, панелей и воркспейсов"],
            Self::PaneMenu => &["Поиск действия панели"],
            Self::Context => &["Инструкции", "Роль", "Память"],
            Self::ProjectContext => &["Инструкции проекта"],
            Self::LaunchReview => &[
                "Роль для запуска",
                "Дополнительные инструкции агенту",
                "Разделы памяти: номера через запятую; пусто — без памяти",
                "Модель · пусто — настройки CLI",
                "Reasoning effort · пусто — настройки CLI",
            ],
            Self::ResumeReview => &[],
            Self::SessionContext => &[],
            Self::Conversation => &["Точный ID диалога CLI"],
            Self::Generations => &["Номер строки или «текущее»"],
            Self::Transfer => &["Найти проект, воркспейс или вкладку"],
            Self::Sessions => &[],
            Self::History => &["Найти текст", "История"],
        }
    }

    fn submit_label(self) -> &'static str {
        match self {
            Self::Appearance => "Готово",
            Self::CloseTab => "Закрыть вкладку",
            Self::Prompts => "Готово",
            Self::Prompt => "Сохранить промпт",
            Self::DeletePrompt => "Удалить промпт",
            Self::AgentProfiles => "Готово",
            Self::AgentProfile => "Сохранить профиль",
            Self::DeleteAgentProfile => "Удалить профиль",
            Self::WorkspaceGit => "Закрыть",
            Self::Controls => "Готово",
            Self::AddProject => "Добавить проект",
            Self::CreateProject => "Создать проект",
            Self::CreateWorkspace => "Создать воркспейс",
            Self::Launcher => "Закрыть",
            Self::NewTab => "Создать вкладку",
            Self::Recipe => "Сохранить",
            Self::RecipeSlot => "Готово",
            Self::NewTemplateTab => "Проверить запуск",
            Self::TemplateRun | Self::TemplateRuns => "Закрыть",
            Self::TemplateRunContext => "Назад",
            Self::Rename | Self::RenamePane | Self::SaveLayout => "Сохранить",
            Self::Manage | Self::Layouts => "Готово",
            Self::DeleteLayout => "Удалить раскладку",
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
            Self::StopSession => "Остановить CLI",
            Self::History => "Закрыть",
            Self::TerminalInsert => "Вставить",
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
    pub(super) tab_drop_target: Option<Uuid>,
    pub(super) transfer_source: Option<crate::transfer_flow::Source>,
    pub(super) transfer_index: usize,
    pub(super) transfer_offset: usize,
    pub(super) transfer_query_revision: Option<(Uuid, u64)>,
    pub(super) transfer_scroll: gpui::ScrollHandle,
    pub(super) panel_directory: Option<protocol::PanelDirectory>,
    pub(super) palette_index: usize,
    pub(super) palette_after: Option<Uuid>,
    pub(super) palette_scroll: gpui::ScrollHandle,
    pub(super) palette_loading: bool,
    pub(super) palette_error: Option<String>,
    pub(super) appearance: crate::appearance::Config,
    pub(super) appearance_pending: Option<Uuid>,
    pub(super) appearance_error: Option<String>,
    pub(super) motion_forced: bool,
    pub(super) command_modifier: crate::controls::Modifier,
    pub(super) controls_saving: bool,
    pub(super) compact: bool,
    pub(super) sidebar_compact: bool,
    pub(super) sidebar_override: Option<bool>,
    pub(super) catalog: Option<Catalog>,
    pub(super) management_target: Option<crate::catalog_flow::ManagedEntity>,
    pub(super) template_target: Option<pigade_storage::LayoutTemplate>,
    pub(super) template_draft: Option<pigade_layout::GeometryTemplate>,
    pub(super) tab_close_sheet: Option<crate::tab_close::Sheet>,
    pub(super) prompt_editor: Option<crate::prompt_flow::PromptEditor>,
    pub(super) prompt_delete: Option<pigade_storage::Prompt>,
    pub(super) prompt_origin: Option<Dialog>,
    pub(super) profile_editor: Option<crate::profile_flow::ProfileEditor>,
    pub(super) profile_delete: Option<pigade_storage::AgentProfile>,
    pub(super) recipe_editor: Option<crate::template_flow::RecipeEditor>,
    pub(super) template_run: Option<protocol::TemplateRunView>,
    pub(super) template_runs: Option<pigade_storage::TemplateRunPage>,
    pub(super) run_busy: bool,
    pub(super) run_automatic: bool,
    pub(super) run_unverified: bool,
    pub(super) run_context_slot: usize,
    pub(super) layout_scroll: gpui::ScrollHandle,
    pub(super) show_archived: bool,
    pub(super) context: Option<ContextDocument>,
    pub(super) sessions: Vec<SessionSummary>,
    pub(super) git_sheet: Option<crate::workspace_git::GitSheet>,
    pub(super) git_scroll: gpui::ScrollHandle,
    pub(super) stop_sheet: Option<crate::session_control::StopSheet>,
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
    pub(super) insert_review: Option<crate::terminal_insert::Review>,
    pub(super) insert_scroll: gpui::ScrollHandle,
}

pub(super) struct SessionSummary {
    pub(super) session: String,
    pub(super) generation: String,
    pub(super) title: String,
    pub(super) state: String,
    pub(super) pid: Option<u32>,
}

impl From<protocol::View> for SessionSummary {
    fn from(view: protocol::View) -> Self {
        Self {
            session: view.session,
            generation: view.generation,
            title: view.frame.title,
            state: view.state,
            pid: view.pid,
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
    status_amber: Hsla,
}

#[derive(Clone, Copy)]
struct ControlStyle {
    role: Role,
    selected: bool,
    compact: bool,
}

impl ProductColors {
    fn for_theme(theme: crate::appearance::Theme) -> Self {
        let colors = chrome::Colors::for_theme(theme);
        Self {
            base: rgb(colors.base).into(),
            sidebar: rgb(colors.sidebar).into(),
            raised: rgb(colors.raised).into(),
            hover: rgb(colors.hover).into(),
            selected: rgb(colors.selected).into(),
            border: rgb(colors.border).into(),
            text: rgb(colors.text).into(),
            muted: rgb(colors.muted).into(),
            accent: rgb(colors.accent).into(),
            status_amber: rgb(colors.status_amber).into(),
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
        (
            "Внешний вид · appearance · тема",
            ProductAction::OpenAppearance,
        ),
        (
            "Увеличить шрифт терминала",
            ProductAction::AdjustTerminalFont(1),
        ),
        (
            "Уменьшить шрифт терминала",
            ProductAction::AdjustTerminalFont(-1),
        ),
        (
            "Размер шрифта терминала · сбросить 14 px",
            ProductAction::ResetTerminalFont,
        ),
        (
            "Клавиша управления панелями / горячие клавиши",
            ProductAction::OpenControls,
        ),
        ("Открыть папку проекта", ProductAction::AddProject),
        ("Создать проект", ProductAction::CreateProject),
        ("Выбрать путь вручную…", ProductAction::OpenProjectPath),
        ("Новый воркспейс", ProductAction::CreateWorkspace),
        (
            "Git · ветка и изменения воркспейса",
            ProductAction::OpenWorkspaceGit,
        ),
        ("Добавить агента", ProductAction::OpenLauncher),
        ("Управление панелями", ProductAction::OpenPaneMenu),
        ("Переименовать панель", ProductAction::RenamePane),
        ("Контекст воркспейса · E", ProductAction::EditContext),
        ("Библиотека промптов · prompts", ProductAction::OpenPrompts),
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
        ("Остановить CLI", ProductAction::OpenStop),
        ("История панели · I", ProductAction::OpenHistory),
        ("Новая вкладка", ProductAction::CreateTab),
        ("Закрыть вкладку · close tab", ProductAction::OpenCloseTab),
        (
            "Новая вкладка · implementer + reviewer",
            ProductAction::ChoosePreset(crate::template_flow::Preset::Review),
        ),
        (
            "Новая вкладка · два агента + shell",
            ProductAction::ChoosePreset(crate::template_flow::Preset::AgentsShell),
        ),
        (
            "Незавершённое создание вкладок",
            ProductAction::OpenTemplateRuns,
        ),
        (
            "Управление проектом · переименовать",
            ProductAction::ManageSelected(crate::catalog_flow::EntityKind::Project),
        ),
        (
            "Управление воркспейсом · переименовать / архив",
            ProductAction::ManageSelected(crate::catalog_flow::EntityKind::Workspace),
        ),
        (
            "Управление вкладкой · переименовать",
            ProductAction::ManageSelected(crate::catalog_flow::EntityKind::Tab),
        ),
        ("Сохранённые раскладки · D", ProductAction::OpenLayouts),
        (
            "Профили агентов · profiles",
            ProductAction::OpenAgentProfiles,
        ),
        ("Сохранить текущую раскладку", ProductAction::SaveLayout),
        (
            "Показать / скрыть архив воркспейсов",
            ProductAction::ToggleArchived,
        ),
        ("Обновить каталог проектов", ProductAction::RefreshCatalog),
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
            | ProductAction::RenamePane
            | ProductAction::OpenStop
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

fn template_thumbnail(node: &pigade_layout::TemplateNode, colors: chrome::Colors) -> Div {
    fn draw(node: &pigade_layout::TemplateNode, depth: usize, colors: chrome::Colors) -> Div {
        let tile = div().flex_1().min_w_0().min_h_0().rounded(px(3.));
        match node {
            pigade_layout::TemplateNode::Pane => tile
                .bg(rgb(colors.accent).opacity(0.36))
                .border_1()
                .border_color(rgb(colors.accent).opacity(0.45)),
            // A thumbnail has a fixed pixel area. Collapse complex branches
            // rather than allocating a widget for every possible model node.
            pigade_layout::TemplateNode::Split { children, .. }
                if depth >= 3 || children.len() > 4 =>
            {
                tile.bg(rgb(colors.accent).opacity(0.24))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(px(10.))
                    .text_color(rgb(colors.accent))
                    .child("…")
            }
            pigade_layout::TemplateNode::Split {
                axis,
                weights,
                children,
            } => tile
                .flex()
                .gap(px(3.))
                .when(*axis == pigade_layout::Axis::Vertical, |row| row.flex_col())
                .children(children.iter().zip(weights).map(|(child, weight)| {
                    draw(child, depth + 1, colors)
                        .flex_basis(px(0.))
                        .flex_grow(*weight as f32)
                })),
        }
    }
    draw(node, 0, colors)
}

fn effective_context_blocks(
    context: &pigade_storage::EffectiveContext,
    id: &str,
    accessible_name: &str,
    max_height: f32,
    catalog: Option<&Catalog>,
    colors: ProductColors,
) -> Stateful<Div> {
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
        DialogKind::AgentProfile => matches!(index, 1 | 2),
        DialogKind::Prompt => index == 1,
        DialogKind::RecipeSlot => index < 2,
        DialogKind::History => index == 1,
        _ => false,
    }
}

impl ProductUi {
    pub(super) fn engine_options_supported(&self) -> bool {
        self.catalog
            .as_ref()
            .is_some_and(|catalog| catalog.engine_options_schema == Some(1))
    }
    fn colors(&self) -> ProductColors {
        ProductColors::for_theme(self.appearance.theme)
    }

    pub(super) fn replace_sessions(&mut self, sessions: Vec<SessionSummary>) {
        let picker = self
            .dialog
            .as_ref()
            .filter(|dialog| dialog.kind == DialogKind::Sessions)
            .map(|dialog| dialog.active_field);
        let selected = if self.unplaced_sessions().is_empty() {
            None
        } else {
            picker.and_then(|index| {
                self.session_menu_entries()
                    .get(index)
                    .map(|(_, action)| action.clone())
            })
        };
        self.sessions = sessions;
        if picker.is_some() {
            let entries = self.session_menu_entries();
            let index = selected
                .and_then(|action| {
                    entries
                        .iter()
                        .position(|(_, candidate)| *candidate == action)
                })
                .unwrap_or(0);
            if let Some(dialog) = &mut self.dialog {
                dialog.active_field = index;
            }
            self.layout_scroll
                .scroll_to_item(self.session_action_row(index));
        }
    }

    fn unplaced_sessions(&self) -> Vec<&SessionSummary> {
        let placed = placed_session_ids(self.catalog.as_ref());
        let mut by_session = HashMap::<Uuid, usize>::new();
        let mut sessions = Vec::<&SessionSummary>::new();
        for view in &self.sessions {
            let (Ok(session), Ok(generation)) = (
                Uuid::parse_str(&view.session),
                Uuid::parse_str(&view.generation),
            ) else {
                continue;
            };
            if session.is_nil() || generation.is_nil() || placed.contains(&session) {
                continue;
            }
            if let Some(&index) = by_session.get(&session) {
                let prior = sessions[index];
                let is_live =
                    matches!(view.state.as_str(), "running" | "degraded") && view.pid.is_some();
                let prior_is_live =
                    matches!(prior.state.as_str(), "running" | "degraded") && prior.pid.is_some();
                if is_live && !prior_is_live {
                    sessions[index] = view;
                }
            } else {
                by_session.insert(session, sessions.len());
                sessions.push(view);
            }
        }
        sessions.truncate(128);
        sessions
    }

    fn session_menu_entries(&self) -> Vec<(String, ProductAction)> {
        let has_tab = self.catalog.as_ref().and_then(selected_tab).is_some();
        let mut entries = Vec::new();
        for view in self.unplaced_sessions() {
            let Some(session) = Uuid::parse_str(&view.session).ok() else {
                continue;
            };
            let Some(generation) = Uuid::parse_str(&view.generation).ok() else {
                continue;
            };
            if has_tab {
                entries.push((
                    format!(
                        "Разместить · {}",
                        self.display_title(&view.session, &view.title)
                    ),
                    ProductAction::AttachSession(session),
                ));
            }
            if crate::session_control::can_stop(&view.state, view.pid) {
                entries.push((
                    format!(
                        "Остановить CLI · {}",
                        self.display_title(&view.session, &view.title)
                    ),
                    ProductAction::StopSession {
                        session,
                        generation,
                    },
                ));
            }
        }
        entries.push(("Обновить список".into(), ProductAction::RefreshSessions));
        entries.push(("Закрыть".into(), ProductAction::CloseDialog));
        entries
    }

    fn stop_menu_entries(&self) -> Vec<(String, ProductAction)> {
        use crate::session_control::Phase;
        match self.stop_sheet.as_ref().map(|sheet| sheet.phase) {
            Some(Phase::Draft) => vec![
                ("Отмена".into(), ProductAction::CloseDialog),
                ("Остановить CLI".into(), ProductAction::ConfirmStop),
            ],
            Some(Phase::Waiting | Phase::Unknown | Phase::Unavailable) => vec![
                ("Проверить состояние".into(), ProductAction::InspectStop),
                ("Закрыть".into(), ProductAction::CloseDialog),
            ],
            Some(Phase::Sending | Phase::Complete) | None => {
                vec![("Закрыть".into(), ProductAction::CloseDialog)]
            }
        }
    }

    pub(super) fn session_action_row(&self, action_index: usize) -> usize {
        let has_tab = self.catalog.as_ref().and_then(selected_tab).is_some();
        let sessions = self.unplaced_sessions();
        let mut action = 0;
        for (row, view) in sessions.iter().enumerate() {
            if has_tab {
                if action == action_index {
                    return row;
                }
                action += 1;
            }
            if crate::session_control::can_stop(&view.state, view.pid) {
                if action == action_index {
                    return row;
                }
                action += 1;
            }
        }
        let empty_message = usize::from(sessions.is_empty());
        sessions.len() + empty_message + action_index.saturating_sub(action)
    }

    pub(super) fn menu_entries(&self) -> Vec<(String, ProductAction)> {
        match self.dialog.as_ref().map(|dialog| dialog.kind) {
            Some(DialogKind::WorkspaceGit) => vec![
                ("Обновить".into(), ProductAction::RefreshWorkspaceGit),
                ("Закрыть".into(), ProductAction::CloseDialog),
            ],
            Some(DialogKind::Prompts) => self.prompt_menu_entries(),
            Some(DialogKind::AgentProfiles) => self.profile_menu_entries(),
            Some(DialogKind::Sessions) => self.session_menu_entries(),
            Some(DialogKind::StopSession) => self.stop_menu_entries(),
            Some(
                DialogKind::Recipe
                | DialogKind::TemplateRun
                | DialogKind::TemplateRuns
                | DialogKind::TemplateRunContext,
            ) => self.recipe_menu_entries(),
            Some(DialogKind::Manage) => match &self.management_target {
                Some(crate::catalog_flow::ManagedEntity::Project(project)) => vec![
                    ("Переименовать проект".into(), ProductAction::RenameTarget),
                    (
                        "Новый воркспейс".into(),
                        ProductAction::CreateWorkspaceFor(project.id),
                    ),
                ],
                Some(crate::catalog_flow::ManagedEntity::Workspace(workspace)) => {
                    let mut rows = vec![
                        (
                            "Открыть воркспейс".into(),
                            ProductAction::SelectWorkspace(workspace.id),
                        ),
                        (
                            "Переименовать воркспейс".into(),
                            ProductAction::RenameTarget,
                        ),
                    ];
                    if matches!(
                        workspace.status,
                        WorkspaceStatus::Ready | WorkspaceStatus::Archived
                    ) {
                        rows.push((
                            (if workspace.status == WorkspaceStatus::Archived {
                                "Вернуть из архива"
                            } else {
                                "В архив"
                            })
                            .into(),
                            ProductAction::ArchiveTarget,
                        ));
                    }
                    rows
                }
                Some(crate::catalog_flow::ManagedEntity::Tab(tab)) => vec![
                    ("Открыть вкладку".into(), ProductAction::SelectTab(tab.id)),
                    ("Переименовать вкладку".into(), ProductAction::RenameTarget),
                    ("Закрыть вкладку…".into(), ProductAction::CloseManagedTab),
                ],
                None => vec![],
            },
            Some(DialogKind::Layouts) => {
                if self
                    .catalog
                    .as_ref()
                    .and_then(|catalog| catalog.layout_templates.as_ref())
                    .is_none()
                {
                    return vec![];
                }
                let mut rows = vec![(
                    "Сохранить текущую раскладку…".into(),
                    ProductAction::SaveLayout,
                )];
                if let Some(templates) = self
                    .catalog
                    .as_ref()
                    .and_then(|catalog| catalog.layout_templates.as_ref())
                {
                    rows.extend(templates.iter().map(|template| {
                        (
                            template.name.clone(),
                            ProductAction::ApplyLayout(template.id),
                        )
                    }));
                }
                if self.recipe_supported() {
                    rows.push((
                        "Незавершённое создание вкладок".into(),
                        ProductAction::OpenTemplateRuns,
                    ));
                }
                rows
            }
            _ => vec![],
        }
    }

    pub(super) fn current_pane_count(&self) -> usize {
        self.catalog
            .as_ref()
            .and_then(|catalog| {
                catalog
                    .tabs
                    .iter()
                    .find(|tab| Some(tab.id) == catalog.selection.tab_id)
            })
            .and_then(|tab| serde_json::from_str::<pigade_layout::TabLayout>(&tab.layout_json).ok())
            .map_or(0, |layout| layout.panes().len())
    }

    pub(super) fn palette_action(&self) -> Option<ProductAction> {
        let dialog = self.dialog.as_ref()?;
        if !matches!(dialog.kind, DialogKind::Palette | DialogKind::PaneMenu) {
            return None;
        }
        self.palette_entries()
            .get(self.palette_index)
            .map(|entry| entry.action.clone())
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
        let colors = self.colors();
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
            .child(chrome::icon(
                "logo-p",
                20.,
                chrome::Colors::for_theme(self.appearance.theme).accent,
            ))
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
            .child(self.icon_control(
                "header-appearance",
                "Внешний вид",
                "layers",
                ProductAction::OpenAppearance,
                cx,
            ))
            .child(self.action_control_with_accessible_name(
                "command-modifier".into(),
                format!("{} ▾", self.command_modifier.label()),
                "Выбрать клавишу управления панелями".into(),
                ProductAction::OpenControls,
                ControlStyle {
                    role: Role::Button,
                    selected: false,
                    compact: true,
                },
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
        let colors = self.colors();
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
                        .child(div().flex_1().min_w_0().child(self.action_control(
                            format!("project-{}", project.id),
                            project.name.clone(),
                            project_action,
                            ControlStyle {
                                role: Role::TreeItem,
                                selected: false,
                                compact: false,
                            },
                            cx,
                        )))
                        .child(self.icon_control(
                            &format!("project-manage-{}", project.id),
                            &format!("Управление проектом {}", project.name),
                            "more",
                            ProductAction::ManageProject(project.id),
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
                for workspace in catalog.workspaces.iter().filter(|value| {
                    value.project_id == project.id
                        && (self.show_archived || value.status != WorkspaceStatus::Archived)
                }) {
                    let selected = selected_workspace(catalog) == Some(workspace.id);
                    let title = workspace_name(workspace);
                    let label = if compact {
                        title.chars().take(2).collect::<String>()
                    } else {
                        title.clone()
                    };
                    let mut workspace_row = div().flex().items_center().min_w_0().child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(self.action_control_with_accessible_name(
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
                            )),
                    );
                    if !compact {
                        workspace_row = workspace_row.child(self.icon_control(
                            &format!("workspace-manage-{}", workspace.id),
                            &format!("Управление воркспейсом {}", workspace_name(workspace)),
                            "more",
                            ProductAction::ManageWorkspace(workspace.id),
                            cx,
                        ));
                    }
                    tree = tree.child(
                        div()
                            .pl(px(if compact { 0. } else { 12. }))
                            .child(workspace_row),
                    );
                    if selected && !compact {
                        for view in views {
                            let Ok(id) = view.session.parse::<Uuid>() else {
                                continue;
                            };
                            let title = self.pane_title(view);
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
                                            chrome::Colors::for_theme(self.appearance.theme)
                                                .status_green
                                        } else {
                                            chrome::Colors::for_theme(self.appearance.theme).muted
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
                        .child(chrome::icon(
                            "folder",
                            20.,
                            chrome::Colors::for_theme(self.appearance.theme).muted,
                        ))
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
        let archived = self.catalog.as_ref().map_or(0, |catalog| {
            catalog
                .workspaces
                .iter()
                .filter(|workspace| workspace.status == WorkspaceStatus::Archived)
                .count()
        });
        if archived > 0 || self.show_archived {
            footer = footer.child(if compact {
                self.icon_control(
                    "sidebar-archive",
                    "Показать / скрыть архив воркспейсов",
                    "branch",
                    ProductAction::ToggleArchived,
                    cx,
                )
            } else {
                self.action_control(
                    "sidebar-archive".into(),
                    format!(
                        "{} архив · {archived}",
                        if self.show_archived {
                            "Скрыть"
                        } else {
                            "Показать"
                        }
                    ),
                    ProductAction::ToggleArchived,
                    ControlStyle {
                        role: Role::Button,
                        selected: self.show_archived,
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
        let colors = self.colors();
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
        let colors = self.colors();
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
                "workspace-git".into(),
                "Git".into(),
                ProductAction::OpenWorkspaceGit,
                ControlStyle {
                    role: Role::Button,
                    selected: false,
                    compact: true,
                },
                cx,
            ))
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
                "saved-layouts",
                "Сохранённые раскладки",
                "split-right",
                ProductAction::OpenLayouts,
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
        let colors = self.colors();
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
            .child(chrome::icon(
                icon,
                15.,
                chrome::Colors::for_theme(self.appearance.theme).muted,
            ))
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                if this.pane_drag.is_none() {
                    this.product_action(action.clone(), window, cx);
                }
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
        let colors = self.colors();
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
        let colors = self.colors();
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
        let colors = self.colors();
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
                    .child(div().mb(px(20.)).child(chrome::icon(
                        "logo-p",
                        42.,
                        chrome::Colors::for_theme(self.appearance.theme).accent,
                    )))
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
        let colors = self.colors();
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
                if primary {
                    chrome::Colors::for_theme(self.appearance.theme).on_accent
                } else {
                    chrome::Colors::for_theme(self.appearance.theme).muted
                },
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
        let colors = self.colors();
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
        let colors = self.colors();
        let id = tab.id;
        let drop_target = self.tab_drop_target == Some(id);
        let app = cx.entity();
        let bounds_observer = canvas(
            move |bounds, window, cx| {
                let clipped = bounds.intersect(&window.content_mask().bounds);
                app.update(cx, |this, _| {
                    if clipped.is_empty() {
                        this.tab_drop_bounds.remove(&id);
                    } else {
                        this.tab_drop_bounds.insert(id, clipped);
                    }
                });
            },
            |_, _, _, _| {},
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full();
        div()
            .id(format!("tab-{id}"))
            .role(Role::Tab)
            .aria_label(if drop_target {
                format!("Цель переноса панели: {}", tab.name)
            } else {
                tab.name.clone()
            })
            .aria_selected(selected)
            .relative()
            .h_full()
            .max_w(px(180.))
            .px(px(12.))
            .flex_shrink_0()
            .flex()
            .items_center()
            .border_b_2()
            .border_color(if drop_target || selected {
                colors.accent
            } else {
                colors.base
            })
            .bg(if drop_target {
                colors.selected
            } else if selected {
                colors.raised
            } else {
                colors.base
            })
            .rounded_t(px(5.))
            .text_size(px(12.))
            .text_color(if selected {
                colors.accent
            } else {
                colors.muted
            })
            .hover(|style| style.bg(colors.hover))
            .gap_1()
            .child(div().min_w_0().truncate().child(tab.name.clone()))
            // The target style must not change tab width beneath the pointer.
            .child(bounds_observer)
            .when(selected, |row| {
                row.child(self.icon_control(
                    &format!("manage-tab-{id}"),
                    "Управление вкладкой",
                    "more",
                    ProductAction::ManageTab(id),
                    cx,
                ))
            })
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                if this.pane_drag.is_none() {
                    this.product_action(ProductAction::SelectTab(id), window, cx);
                }
                cx.stop_propagation();
            }))
            .on_mouse_down(
                gpui::MouseButton::Right,
                cx.listener(move |this, _, window, cx| {
                    if this.pane_drag.is_none() {
                        this.product_action(ProductAction::ManageTab(id), window, cx);
                    }
                    cx.stop_propagation();
                }),
            )
            .into_any_element()
    }

    pub(super) fn tab_drop_card(
        &self,
        name: &str,
        preview: &pigade_layout::TabLayout,
    ) -> AnyElement {
        let colors = self.colors();
        let thumbnail = pigade_layout::GeometryTemplate::from_tab(preview)
            .ok()
            .and_then(|template| template.root)
            .map(|root| template_thumbnail(&root, chrome::colors(self.appearance.theme)))
            .unwrap_or_else(|| {
                div()
                    .w_full()
                    .h(px(54.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(px(11.))
                    .text_color(colors.muted)
                    .child("Пустая раскладка")
            });
        div()
            .absolute()
            .top(px(86.))
            .right(px(16.))
            .w(px(260.))
            .p(px(12.))
            .flex()
            .flex_col()
            .gap_2()
            .rounded(px(9.))
            .border_1()
            .border_color(colors.accent)
            .bg(colors.raised)
            .text_color(colors.text)
            .shadow_lg()
            .child(
                div()
                    .text_size(px(13.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(name.to_owned()),
            )
            .child(div().w_full().h(px(66.)).flex().child(thumbnail))
            .child(div().text_size(px(11.)).text_color(colors.muted).child(
                if preview.panes().len() == 1 {
                    "Первая панель во вкладке"
                } else {
                    "Справа от выбранной панели"
                },
            ))
            .child(
                div()
                    .text_size(px(11.))
                    .text_color(colors.muted)
                    .child("Отпустите, чтобы перенести · Esc — отмена"),
            )
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
        let colors = self.colors();
        let primary = matches!(
            id.as_str(),
            "product-dialog-submit" | "launch-review-confirm" | "resume-review-confirm"
        ) || matches!(&action, ProductAction::OpenLauncher)
            || matches!(&action, ProductAction::AddProject) && style.selected;
        let toggle = style.role == Role::CheckBox || matches!(&action, ProductAction::SetTheme(_));
        let foreground = if primary {
            Hsla::from(rgb(
                chrome::Colors::for_theme(self.appearance.theme).on_accent
            ))
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
            .when(toggle, |row| {
                row.aria_toggled(if style.selected {
                    Toggled::True
                } else {
                    Toggled::False
                })
            })
            .when(!toggle, |row| row.aria_selected(style.selected))
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
                    if primary {
                        chrome::Colors::for_theme(self.appearance.theme).on_accent
                    } else {
                        chrome::Colors::for_theme(self.appearance.theme).muted
                    },
                )
            }))
            .child(label)
            .into_any_element()
    }

    fn controls_overlay(&self, cx: &mut Context<CanvasApp>) -> AnyElement {
        let colors = self.colors();
        let mut choices = div().flex().flex_wrap().gap_2().my(px(16.));
        for (index, modifier) in crate::controls::Modifier::ALL.into_iter().enumerate() {
            choices = choices.child(self.action_control(
                format!("control-choice-{}", modifier.stable_name()),
                format!(
                    "{} · {}",
                    index + 1,
                    if modifier == crate::controls::Modifier::Disabled {
                        "Только prefix"
                    } else {
                        modifier.label()
                    }
                ),
                ProductAction::SetCommandModifier(modifier),
                ControlStyle {
                    role: Role::Button,
                    selected: self.command_modifier == modifier,
                    compact: true,
                },
                cx,
            ));
        }
        let key = if self.command_modifier == crate::controls::Modifier::Disabled {
            "Модификатор"
        } else {
            self.command_modifier.label()
        };
        div().id("controls-overlay").absolute().inset_0().flex().justify_center().items_start()
            .overflow_y_scroll().py(px(24.)).bg(rgb(0x090b0e).opacity(0.72))
            .on_click(cx.listener(|this,_:&ClickEvent,window,cx|this.product_action(ProductAction::CloseDialog,window,cx)))
            .child(div().id("controls-dialog").role(Role::Dialog).aria_label("Клавиша управления панелями")
                .w_full().max_w(px(560.)).mx(px(16.)).p(px(22.)).rounded(px(12.))
                .bg(colors.raised).border_1().border_color(colors.border).text_color(colors.text)
                .flex().flex_col().gap_2()
                .on_click(cx.listener(|_,_:&ClickEvent,_,cx|cx.stop_propagation()))
                .child(div().id("controls-title").role(Role::Heading).aria_level(2).text_size(px(17.)).child("Клавиша управления панелями"))
                .child(div().text_size(px(12.)).text_color(colors.muted).child("Зажмите выбранную клавишу, как Win в Hyprland. 1–5 — выбрать, Escape — закрыть."))
                .child(choices)
                .children([
                    format!("{key}+Q — закрыть панель; CLI остаётся в фоне"),
                    format!("{key}+F — развернуть панель / вернуть раскладку"),
                    format!("{key}+стрелки / [ ] — фокус; Shift+стрелки — обмен"),
                    format!("{key}+ЛКМ — перетащить; +ПКМ — изменить размер"),
                    format!("{key}+1…9 — вкладка; +Shift — перенести панель"),
                    format!("{key}+Tab / +Shift+Tab — следующая / предыдущая вкладка"),
                    format!("{key}+Enter — терминал; +; / +' — пропорции"),
                ].into_iter().map(|label|div().text_size(px(12.)).child(label)))
                .child(div().mt(px(12.)).text_size(px(11.)).text_color(colors.muted)
                    .child("Эти сочетания получает PigADE, остальные — CLI. Ctrl+C/D/Z/R работают как обычно. Ctrl+F и Ctrl+Q при выборе Ctrl заняты управлением. Super может перехватываться системой. Prefix Ctrl+Shift+Space остаётся доступен."))
                .children(self.notice.clone().map(|notice|div().id("controls-notice").role(Role::Status).text_size(px(12.)).child(notice)))
                .when(self.controls_saving,|view|view.child(div().id("controls-saving").role(Role::Status).child("Сохранение…")))
                .child(self.action_control("controls-close".into(),"Готово".into(),ProductAction::CloseDialog,ControlStyle{role:Role::Button,selected:false,compact:true},cx)))
            .into_any_element()
    }

    fn dialog_overlay(
        &self,
        dialog: &Dialog,
        layouts: &BTreeMap<usize, FieldLayout>,
        cx: &mut Context<CanvasApp>,
    ) -> AnyElement {
        let colors = self.colors();
        if dialog.kind == DialogKind::LaunchReview {
            return self.launch_review_overlay(dialog, layouts, cx);
        }
        if dialog.kind == DialogKind::Controls {
            return self.controls_overlay(cx);
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
            return self.transfer_overlay(dialog, layouts, cx);
        }
        if dialog.kind == DialogKind::WorkspaceGit {
            return self.git_overlay(cx);
        }
        if dialog.kind == DialogKind::Sessions {
            return self.sessions_overlay(cx);
        }
        if dialog.kind == DialogKind::StopSession {
            return self.stop_overlay(cx);
        }
        if dialog.kind == DialogKind::History {
            return self.history_overlay(dialog, layouts, cx);
        }
        if dialog.kind == DialogKind::TerminalInsert {
            return self.terminal_insert_overlay(cx);
        }

        let can_bind = dialog.kind != DialogKind::Conversation
            || self.launch_preview.as_ref().is_some_and(|preview| {
                matches!(preview.provider.as_str(), "codex" | "claude" | "opencode")
            });
        let mut fields = div().flex().flex_col().gap_2().mt(px(16.));
        if dialog.kind == DialogKind::RenamePane {
            fields=fields.child(div().text_size(px(12.)).text_color(colors.muted).whitespace_normal().child("Имя хранится в PigADE. Пустое поле возвращает название CLI. Процесс и его контекст сохраняются."));
        }
        if matches!(dialog.kind, DialogKind::Manage | DialogKind::Rename)
            && let Some(target) = &self.management_target
        {
            let detail = match target {
                crate::catalog_flow::ManagedEntity::Project(value) => {
                    value.canonical_path.display().to_string()
                }
                crate::catalog_flow::ManagedEntity::Workspace(value) => format!(
                    "{} · {}",
                    value.cwd.display(),
                    if value.mode == pigade_storage::WorkspaceMode::Worktree {
                        "Worktree"
                    } else {
                        "Общая папка проекта"
                    }
                ),
                crate::catalog_flow::ManagedEntity::Tab(_) => {
                    "Самостоятельная раскладка терминальных панелей".into()
                }
            };
            fields = fields
                .child(
                    div()
                        .text_size(px(14.))
                        .font_weight(FontWeight::MEDIUM)
                        .child(target.name().to_owned()),
                )
                .child(
                    div()
                        .text_size(px(11.))
                        .text_color(colors.muted)
                        .whitespace_normal()
                        .child(detail),
                );
            if let crate::catalog_flow::ManagedEntity::Workspace(value) = target {
                fields = fields.child(div().text_size(px(11.)).text_color(colors.muted).child(
                    if value.status == WorkspaceStatus::Archived {
                        "В архиве. CLI и файлы сохранены."
                    } else {
                        "Архив скрывает воркспейс. CLI продолжат работу в фоне."
                    },
                ));
            }
        }
        if dialog.kind == DialogKind::Manage {
            for (index, (label, action)) in self.menu_entries().into_iter().enumerate() {
                fields = fields.child(self.action_control(
                    format!("manage-action-{index}"),
                    label,
                    action,
                    ControlStyle {
                        role: Role::Button,
                        selected: dialog.active_field == index,
                        compact: false,
                    },
                    cx,
                ));
            }
            fields = fields.child(
                div()
                    .text_size(px(11.))
                    .text_color(colors.muted)
                    .child("↑ ↓ — выбрать · Enter — выполнить · Esc — закрыть"),
            );
        }
        if dialog.kind == DialogKind::SaveLayout
            && let Some(draft) = &self.template_draft
        {
            fields =
                fields.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_3()
                        .child(div().w(px(96.)).h(px(56.)).flex().children(
                            draft.root.as_ref().map(|node| {
                                template_thumbnail(
                                    node,
                                    chrome::Colors::for_theme(self.appearance.theme),
                                )
                            }),
                        ))
                        .child(
                            div()
                                .flex_1()
                                .text_size(px(12.))
                                .text_color(colors.muted)
                                .whitespace_normal()
                                .child(format!(
                                    "{} панелей · направления и пропорции разделений",
                                    draft.pane_count()
                                )),
                        ),
                );
        }
        if dialog.kind == DialogKind::DeleteLayout
            && let Some(template) = &self.template_target
        {
            fields = fields
                .child(div().text_size(px(14.)).child(template.name.clone()))
                .child(div().text_size(px(12.)).text_color(colors.muted).child(
                    "Будет удалён только шаблон. Открытые панели и их раскладки сохранятся.",
                ));
        }
        if dialog.kind == DialogKind::Layouts {
            fields = self.layout_library(dialog, fields, cx);
        }
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

        if dialog.kind == DialogKind::CloseTab
            && let Some(sheet) = &self.tab_close_sheet
        {
            fields=fields.child(div().text_size(px(14.)).child(sheet.tab.name.clone()))
                .child(div().text_size(px(12.)).text_color(colors.muted).whitespace_normal()
                    .child(format!("Панелей: {}. CLI продолжат работать; их можно вернуть через «Сессии». Файлы и история сохранятся.",sheet.panes)))
                .child(div().text_size(px(11.)).text_color(colors.muted).whitespace_normal()
                    .child("Вкладка будет убрана из каталога и истории раскладки. Undo не откроет её снова. Ctrl+Enter — закрыть · Ctrl+R — сверить состояние."));
            if sheet.request.is_some() {
                fields = fields.child(div().text_size(px(12.)).child("Ожидаем ответ host…"));
            }
            if sheet.uncertain {
                fields = fields.child(div().text_size(px(12.)).whitespace_normal().child(
                    "Закрытие не подтверждено. Сверьте состояние перед следующим действием.",
                ));
            }
            fields = fields.child(self.action_control(
                "close-tab-inspect".into(),
                "Обновить состояние".into(),
                ProductAction::InspectCloseTab,
                ControlStyle {
                    role: Role::Button,
                    selected: false,
                    compact: true,
                },
                cx,
            ));
        }
        if dialog.kind == DialogKind::Appearance {
            fields = self.appearance_fields(fields, cx);
        }
        fields = self.template_dialog_fields(dialog, fields, cx);
        fields = self.profile_dialog_fields(dialog, fields, cx);
        fields = self.prompt_dialog_fields(dialog, fields, cx);
        if crate::prompt_flow::accepts_prompt(dialog) {
            fields = fields.child(
                div()
                    .flex()
                    .gap_2()
                    .child(self.action_control(
                        "open-prompts-in-draft".into(),
                        "Промпты · Ctrl+Shift+P".into(),
                        ProductAction::OpenPrompts,
                        ControlStyle {
                            role: Role::Button,
                            selected: false,
                            compact: true,
                        },
                        cx,
                    ))
                    .child(self.action_control(
                        "save-draft-prompt".into(),
                        "Сохранить текст · Ctrl+Shift+B".into(),
                        ProductAction::SaveDraftAsPrompt,
                        ControlStyle {
                            role: Role::Button,
                            selected: false,
                            compact: true,
                        },
                        cx,
                    )),
            );
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

        let close_blocked = dialog.kind == DialogKind::CloseTab
            && self
                .tab_close_sheet
                .as_ref()
                .is_none_or(|sheet| sheet.request.is_some() || sheet.uncertain);
        let submit = if !can_bind || close_blocked {
            div().into_any_element()
        } else {
            self.action_control(
                "product-dialog-submit".into(),
                if dialog.kind == DialogKind::Recipe
                    && self
                        .recipe_editor
                        .as_ref()
                        .is_some_and(|editor| editor.origin == crate::template_flow::Origin::NewTab)
                {
                    "Продолжить"
                } else {
                    dialog.kind.submit_label()
                }
                .into(),
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
                    .max_w(px(
                        if matches!(
                            dialog.kind,
                            DialogKind::Layouts
                                | DialogKind::Recipe
                                | DialogKind::NewTemplateTab
                                | DialogKind::TemplateRun
                                | DialogKind::TemplateRunContext
                        ) {
                            560.
                        } else {
                            440.
                        },
                    ))
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

    fn layout_library(
        &self,
        dialog: &Dialog,
        mut content: Div,
        cx: &mut Context<CanvasApp>,
    ) -> Div {
        let colors = self.colors();
        let Some(templates) = self
            .catalog
            .as_ref()
            .and_then(|catalog| catalog.layout_templates.as_ref())
        else {
            return content.child(div().id("layout-library-unsupported").role(Role::Status).text_size(px(12.)).text_color(colors.muted).child("Этот session host не поддерживает шаблоны. Они будут доступны при следующем запуске нового host."));
        };
        let count = self.current_pane_count();
        content = content.child(div().text_size(px(12.)).text_color(colors.muted).whitespace_normal().child("Примените сохранённую геометрию к открытым панелям. Отмена раскладки вернёт прежнее размещение."));
        let mut rows = div()
            .id("layout-library-rows")
            .max_h(px(360.))
            .min_h_0()
            .flex()
            .flex_col()
            .gap_2()
            .overflow_y_scroll()
            .track_scroll(&self.layout_scroll)
            .child(div().flex_shrink_0().child(self.action_control(
                "layout-library-save".into(),
                "Сохранить текущую раскладку…".into(),
                ProductAction::SaveLayout,
                ControlStyle {
                    role: Role::Button,
                    selected: dialog.active_field == 0,
                    compact: false,
                },
                cx,
            )));
        for (index, template) in templates.iter().enumerate() {
            let id = template.id;
            let needed = template.geometry.pane_count();
            let selected = dialog.active_field == index + 1;
            let compatible = needed == count;
            rows = rows.child(
                div()
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .id(format!("layout-template-{id}"))
                            .role(Role::Button)
                            .aria_label(format!(
                                "{} · {needed} панелей · {}",
                                template.name,
                                if compatible {
                                    "Применить"
                                } else {
                                    "Другое число панелей"
                                }
                            ))
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .items_center()
                            .gap_3()
                            .p(px(10.))
                            .rounded(px(8.))
                            .border_1()
                            .border_color(if selected {
                                colors.accent
                            } else {
                                colors.border
                            })
                            .bg(if selected {
                                colors.selected
                            } else {
                                colors.base
                            })
                            .hover(|style| style.bg(colors.hover))
                            .child(div().w(px(86.)).h(px(52.)).flex().children(
                                template.geometry.root.as_ref().map(|node| {
                                    template_thumbnail(
                                        node,
                                        chrome::Colors::for_theme(self.appearance.theme),
                                    )
                                }),
                            ))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .flex()
                                    .flex_col()
                                    .gap_1()
                                    .child(
                                        div()
                                            .text_size(px(13.))
                                            .truncate()
                                            .child(template.name.clone()),
                                    )
                                    .child(
                                        div().text_size(px(11.)).text_color(colors.muted).child(
                                            if compatible {
                                                format!("{needed} панелей · Применить")
                                            } else {
                                                format!("Нужно {needed} панелей · открыто {count}")
                                            },
                                        ),
                                    ),
                            )
                            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                this.product_action(ProductAction::ApplyLayout(id), window, cx);
                                cx.stop_propagation();
                            })),
                    )
                    .children(self.recipe_supported().then(|| {
                        self.icon_control(
                            &format!("configure-template-{id}"),
                            &format!("Агенты в шаблоне {}", template.name),
                            "more",
                            ProductAction::ConfigureTemplate(id),
                            cx,
                        )
                    }))
                    .children(
                        (self.recipe_supported() && template.launches.is_some()).then(|| {
                            self.icon_control(
                                &format!("instantiate-template-{id}"),
                                &format!("Новая вкладка по шаблону {}", template.name),
                                "plus",
                                ProductAction::NewTabFromTemplate(id),
                                cx,
                            )
                        }),
                    )
                    .child(self.icon_control(
                        &format!("delete-template-{id}"),
                        &format!("Удалить раскладку {}", template.name),
                        "close",
                        ProductAction::DeleteLayout(id),
                        cx,
                    )),
            );
        }
        if templates.is_empty() {
            rows = rows.child(
                div()
                    .py(px(16.))
                    .text_size(px(12.))
                    .text_color(colors.muted)
                    .child("Раскладок пока нет. Сохраните удобное размещение панелей."),
            );
        }
        if self.recipe_supported() {
            rows = rows.child(self.action_control(
                "template-runs-library".into(),
                "Незавершённое создание вкладок".into(),
                ProductAction::OpenTemplateRuns,
                ControlStyle {
                    role: Role::Button,
                    selected: dialog.active_field == templates.len() + 1,
                    compact: false,
                },
                cx,
            ));
        }
        content.child(rows).child(
            div().text_size(px(11.)).text_color(colors.muted).child(
                "↑ ↓ — выбрать · Enter — геометрия · Ctrl+Enter — новая вкладка · Ctrl+E — агенты · Shift+Delete — удалить",
            ),
        )
    }

    fn launcher_overlay(&self, cx: &mut Context<CanvasApp>) -> AnyElement {
        let colors = self.colors();
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
        choices = choices.child(self.action_control(
            "launcher-profiles".into(),
            "Профили агентов…".into(),
            ProductAction::OpenAgentProfiles,
            ControlStyle {
                role: Role::Button,
                selected: false,
                compact: false,
            },
            cx,
        ));
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
        let colors = self.colors();
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
                    self.colors(),
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
                        .text_color(colors.status_amber)
                        .child(reason.clone()),
                );
            }
            for (index, warning) in preview.warnings.iter().enumerate() {
                warnings = warnings.child(
                    div()
                        .id(format!("resume-review-warning-{index}"))
                        .role(Role::Status)
                        .text_size(px(11.))
                        .text_color(colors.status_amber)
                        .child(warning.clone()),
                );
            }
            if !preview.can_launch && preview.unavailable_reason.is_none() {
                warnings = warnings.child(
                    div()
                        .id("resume-review-unavailable")
                        .role(Role::Status)
                        .text_size(px(12.))
                        .text_color(colors.status_amber)
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
        let colors = self.colors();
        let preview = self.launch_preview.as_ref();
        let role_override = dialog.fields.first().map(EditBuffer::text).unwrap_or("");
        let agent_instructions = dialog.fields.get(1).map(EditBuffer::text).unwrap_or("");
        let fields_match = preview.is_some_and(|preview| {
            preview.role_override == role_override
                && preview.agent_instructions == agent_instructions
                && pigade_storage::EngineOptions::from_fields(
                    dialog.fields.get(3).map(EditBuffer::text).unwrap_or(""),
                    dialog.fields.get(4).map(EditBuffer::text).unwrap_or(""),
                )
                .is_ok_and(|engine| engine == preview.engine)
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
            if let Some(profile) = &preview.profile {
                metadata = metadata.child(format!(
                    "Профиль {} · ревизия {} · снимок для этого запуска",
                    profile.name, profile.revision
                ));
            }
            metadata = metadata.child(format!(
                "Модель: {} · reasoning: {}",
                preview.engine.model.as_deref().unwrap_or("настройки CLI"),
                preview
                    .engine
                    .reasoning_effort
                    .as_deref()
                    .unwrap_or("настройки CLI")
            ));
            if self
                .catalog
                .as_ref()
                .is_some_and(|catalog| catalog.agent_profiles.is_some())
                && matches!(preview.provider.as_str(), "claude" | "codex" | "opencode")
            {
                metadata = metadata.child(self.action_control(
                    "launch-save-profile".into(),
                    "Сохранить настройки как профиль…".into(),
                    ProductAction::SaveLaunchAsProfile,
                    ControlStyle {
                        role: Role::Button,
                        selected: false,
                        compact: true,
                    },
                    cx,
                ));
            }
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
                ))
                .child(format!(
                    "Выбор модели: {} · reasoning: {}",
                    capability_label(preview.capabilities.model_selection),
                    capability_label(preview.capabilities.reasoning_effort)
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
                self.colors(),
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
                    .text_color(colors.status_amber)
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
                        .text_color(colors.status_amber)
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
            (3usize, "Модель · пусто — настройки CLI"),
            (4usize, "Reasoning effort · пусто — настройки CLI"),
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
            "Ctrl+R — обновить · Ctrl+M — память · Ctrl+D — контекст · Ctrl+Shift+S — профиль · Ctrl+Enter — запуск",
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
        let colors = self.colors();
        let mut content = div().flex().flex_col().gap_2();
        if let Some(preview) = &self.launch_preview {
            if let Some(profile) = &preview.profile {
                content = content.child(format!(
                    "Профиль {} · сохранённая ревизия {}",
                    profile.name, profile.revision
                ));
            }
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
                .child(format!("Запрошенная модель: {} · reasoning: {}", preview.engine.model.as_deref().unwrap_or("настройки CLI"), preview.engine.reasoning_effort.as_deref().unwrap_or("настройки CLI")))
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
                    self.colors(),
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

    fn transfer_overlay(
        &self,
        dialog: &Dialog,
        layouts: &BTreeMap<usize, FieldLayout>,
        cx: &mut Context<CanvasApp>,
    ) -> AnyElement {
        let colors = self.colors();
        let query_buffer = dialog.fields.first();
        let query = query_buffer.map(EditBuffer::text).unwrap_or("");
        let query_revision = query_buffer.map(EditBuffer::revision).unwrap_or(0);
        let current_query = Some((dialog.id, query_revision));
        let query_is_current = self.transfer_query_revision == current_query;
        let offset = if query_is_current {
            self.transfer_offset
        } else {
            0
        };
        let selected_index = if query_is_current {
            self.transfer_index
        } else {
            0
        };
        let source_label = self
            .transfer_source
            .and_then(|source| {
                let catalog = self.catalog.as_ref()?;
                let tab = catalog.tabs.iter().find(|tab| tab.id == source.tab)?;
                let workspace = catalog
                    .workspaces
                    .iter()
                    .find(|workspace| workspace.id == tab.workspace_id)?;
                let project = catalog
                    .projects
                    .iter()
                    .find(|project| project.id == workspace.project_id)?;
                let pane_title = self
                    .sessions
                    .iter()
                    .find(|session| session.session == source.pane.to_string())
                    .map(|session| session.title.as_str())
                    .unwrap_or("панель");
                Some(format!(
                    "Источник: {} • {} • {} · {}",
                    project.name,
                    workspace_name(workspace),
                    tab.name,
                    pane_title
                ))
            })
            .unwrap_or_else(|| {
                "Источник больше недоступен. Закройте окно и выберите панель снова.".into()
            });

        let page = self
            .catalog
            .as_ref()
            .zip(self.transfer_source.as_ref())
            .map(|(catalog, source)| crate::transfer_flow::targets(catalog, source, query, offset));
        let mut targets = div()
            .id("transfer-target-list")
            .role(Role::List)
            .aria_label("Целевые вкладки")
            .flex()
            .flex_col()
            .gap_1()
            .max_h(px(300.))
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&self.transfer_scroll);
        if let Some(page) = &page {
            for (index, target) in page.items.iter().enumerate() {
                let selected = index == selected_index;
                let target_tab = target.tab;
                let title = target.title.replace(" / ", " • ");
                let subtitle = target.subtitle.clone();
                targets = targets.child(
                    div()
                        .id(format!("transfer-target-{}", target.tab))
                        .role(Role::Button)
                        .aria_label(if subtitle.is_empty() {
                            title.clone()
                        } else {
                            format!("{} · {}", title, subtitle)
                        })
                        .aria_selected(selected)
                        .px(px(10.))
                        .py(px(8.))
                        .rounded(px(6.))
                        .bg(if selected {
                            colors.selected
                        } else {
                            colors.raised
                        })
                        .hover(|style| style.bg(colors.hover))
                        .flex()
                        .flex_col()
                        .gap_1()
                        .text_color(colors.text)
                        .child(div().text_size(px(13.)).child(title))
                        .when(!subtitle.is_empty(), |row| {
                            row.child(
                                div()
                                    .text_size(px(11.))
                                    .text_color(colors.muted)
                                    .child(subtitle),
                            )
                        })
                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                            if !this.product.busy {
                                this.product_action(
                                    ProductAction::TransferTo(target_tab),
                                    window,
                                    cx,
                                );
                            }
                            cx.stop_propagation();
                        })),
                );
            }
            if page.items.is_empty() {
                let message = if self.transfer_source.is_none() {
                    "Сначала выберите панель, которую нужно перенести."
                } else if query.trim().is_empty() {
                    "Нет доступных вкладок для переноса этой панели."
                } else {
                    "По этому запросу ничего не найдено."
                };
                targets = targets.child(
                    div()
                        .id("transfer-targets-empty")
                        .role(Role::Status)
                        .px(px(10.))
                        .py(px(12.))
                        .text_color(colors.muted)
                        .child(message),
                );
            }
        } else {
            targets = targets.child(
                div()
                    .id("transfer-targets-unavailable")
                    .role(Role::Status)
                    .px(px(10.))
                    .py(px(12.))
                    .text_color(colors.muted)
                    .child(if self.catalog.is_none() {
                        "Каталог недоступен. Обновите данные host и повторите попытку."
                    } else {
                        "Сначала выберите панель, которую нужно перенести."
                    }),
            );
        }

        let query_field = div()
            .id("transfer-query-field")
            .role(Role::TextInput)
            .aria_label("Найти проект, воркспейс или вкладку")
            .w_full()
            .h(px(40.))
            .mt(px(12.))
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
                    placeholder: "Найти проект, воркспейс или вкладку",
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
        let mut pagination = div()
            .id("transfer-pagination")
            .flex()
            .items_center()
            .justify_between()
            .mt(px(8.));
        let page_offset = page.as_ref().map(|page| page.offset).unwrap_or(offset);
        let has_more = page.as_ref().is_some_and(|page| page.has_more);
        let previous = self.action_control(
            "transfer-page-previous".into(),
            "Назад".into(),
            ProductAction::TransferPage(
                page_offset.saturating_sub(crate::transfer_flow::PAGE_SIZE),
            ),
            ControlStyle {
                role: Role::Button,
                selected: false,
                compact: true,
            },
            cx,
        );
        pagination = pagination.child(if page_offset > 0 && !self.busy {
            previous
        } else {
            div()
                .id("transfer-page-previous-disabled")
                .role(Role::Status)
                .h(px(28.))
                .px(px(9.))
                .flex()
                .items_center()
                .text_size(px(12.))
                .text_color(colors.muted)
                .child("Назад")
                .into_any_element()
        });
        let shown = page.as_ref().map_or(0, |page| page.items.len());
        pagination = pagination.child(
            div()
                .id("transfer-page-count")
                .role(Role::Status)
                .text_size(px(11.))
                .text_color(colors.muted)
                .child(if shown == 0 {
                    "0 результатов".to_owned()
                } else {
                    format!(
                        "{}–{}",
                        page_offset.saturating_add(1),
                        page_offset.saturating_add(shown)
                    )
                }),
        );
        let next = self.action_control(
            "transfer-page-next".into(),
            "Дальше".into(),
            ProductAction::TransferPage(
                page_offset.saturating_add(crate::transfer_flow::PAGE_SIZE),
            ),
            ControlStyle {
                role: Role::Button,
                selected: false,
                compact: true,
            },
            cx,
        );
        pagination = pagination.child(if has_more && !self.busy {
            next
        } else {
            div()
                .id("transfer-page-next-disabled")
                .role(Role::Status)
                .h(px(28.))
                .px(px(9.))
                .flex()
                .items_center()
                .text_size(px(12.))
                .text_color(colors.muted)
                .child("Дальше")
                .into_any_element()
        });
        let close = self.action_control(
            "transfer-dialog-close".into(),
            "Закрыть".into(),
            ProductAction::CloseDialog,
            ControlStyle {
                role: Role::Button,
                selected: false,
                compact: true,
            },
            cx,
        );
        let refresh = self.action_control(
            "transfer-refresh".into(),
            "Обновить".into(),
            ProductAction::RefreshTransfer,
            ControlStyle {
                role: Role::Button,
                selected: false,
                compact: true,
            },
            cx,
        );
        let mut card = div()
            .id("transfer-dialog")
            .role(Role::Dialog)
            .aria_label("Перенести панель")
            .w_full()
            .max_w(px(560.))
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
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .id("transfer-dialog-title")
                            .role(Role::Heading)
                            .aria_level(2)
                            .text_size(px(16.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Перенести панель"),
                    )
                    .child(if self.busy {
                        div()
                            .id("transfer-refresh-disabled")
                            .role(Role::Status)
                            .text_size(px(11.))
                            .text_color(colors.muted)
                            .child("Обновление…")
                            .into_any_element()
                    } else {
                        refresh
                    }),
            )
            .child(query_field)
            .child(
                div()
                    .mt(px(8.))
                    .text_size(px(12.))
                    .text_color(colors.muted)
                    .child(source_label),
            )
            .child(targets)
            .child(pagination)
            .child(
                div()
                    .mt(px(8.))
                    .text_size(px(11.))
                    .text_color(colors.muted)
                    .child("Справа от выбранной панели. Текущая вкладка, рабочая папка и контекст CLI сохраняются."),
            )
            .child(
                div()
                    .mt(px(6.))
                    .text_size(px(11.))
                    .text_color(colors.muted)
                    .child("↑/↓ — выбрать · Enter — перенести · Ctrl+R — обновить · Esc — закрыть"),
            );
        if let Some(notice) = &self.notice {
            card = card.child(
                div()
                    .id("transfer-notice")
                    .role(Role::Status)
                    .mt(px(6.))
                    .text_size(px(11.))
                    .text_color(colors.accent)
                    .child(notice.clone()),
            );
        }
        card = card.child(div().flex().justify_end().mt(px(12.)).child(close));
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
            .child(card)
            .into_any_element()
    }

    fn sessions_overlay(&self, cx: &mut Context<CanvasApp>) -> AnyElement {
        let colors = self.colors();
        let mut menu_index = 0;
        let mut entries = div()
            .id("unplaced-session-list")
            .role(Role::List)
            .aria_label("Неразмещённые сессии")
            .flex()
            .flex_col()
            .gap_1()
            .mt(px(14.))
            .max_h(px(360.))
            .overflow_y_scroll()
            .track_scroll(&self.layout_scroll);
        let has_tab = self.catalog.as_ref().and_then(selected_tab).is_some();
        let mut count = 0;

        for view in self.unplaced_sessions() {
            let Ok(session_id) = Uuid::parse_str(&view.session) else {
                continue;
            };
            count += 1;
            let short_id = session_id.simple().to_string()[..8].to_owned();
            let title = self.display_title(&view.session, &view.title);
            let state = session_state_label(&view.state);
            let attach = if has_tab {
                let attach = self.action_control_with_accessible_name(
                    format!("attach-session-{session_id}"),
                    "Разместить".into(),
                    format!("Разместить сессию {short_id} в выбранной вкладке"),
                    ProductAction::AttachSession(session_id),
                    ControlStyle {
                        role: Role::Button,
                        selected: self
                            .dialog
                            .as_ref()
                            .is_some_and(|dialog| dialog.active_field == menu_index),
                        compact: true,
                    },
                    cx,
                );
                menu_index += 1;
                attach
            } else {
                div()
                    .id(format!("attach-session-unavailable-{session_id}"))
                    .text_size(px(11.))
                    .text_color(colors.muted)
                    .child("Выберите вкладку")
                    .into_any_element()
            };
            let stop = if crate::session_control::can_stop(&view.state, view.pid) {
                let Ok(generation) = Uuid::parse_str(&view.generation) else {
                    continue;
                };
                let action = ProductAction::StopSession {
                    session: session_id,
                    generation,
                };
                let control = self.action_control(
                    format!("stop-session-{}-{}", session_id, generation),
                    "Остановить CLI".into(),
                    action,
                    ControlStyle {
                        role: Role::Button,
                        selected: self
                            .dialog
                            .as_ref()
                            .is_some_and(|dialog| dialog.active_field == menu_index),
                        compact: true,
                    },
                    cx,
                );
                menu_index += 1;
                Some(control)
            } else {
                None
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
                    .child(attach)
                    .children(stop),
            );
        }

        if count == 0 {
            entries = entries.child(
                div()
                    .id("sessions-all-placed")
                    .px(px(10.))
                    .py(px(16.))
                    .text_color(colors.muted)
                    .child(if self.busy {
                        "Обновляем список сессий…"
                    } else {
                        "Все сессии размещены"
                    }),
            );
        }

        let refresh = self.action_control_with_accessible_name(
            "refresh-sessions".into(),
            "Обновить".into(),
            "Обновить список сессий".into(),
            ProductAction::RefreshSessions,
            ControlStyle {
                role: Role::Button,
                selected: self
                    .dialog
                    .as_ref()
                    .is_some_and(|dialog| dialog.active_field == menu_index),
                compact: true,
            },
            cx,
        );
        menu_index += 1;
        let close = self.action_control(
            "sessions-dialog-close".into(),
            "Закрыть".into(),
            ProductAction::CloseDialog,
            ControlStyle {
                role: Role::Button,
                selected: self
                    .dialog
                    .as_ref()
                    .is_some_and(|dialog| dialog.active_field == menu_index),
                compact: true,
            },
            cx,
        );
        entries = entries
            .child(div().flex().justify_end().mt(px(8.)).child(refresh))
            .child(div().flex().justify_end().child(close));
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
                            ),
                    )
                    .child(entries),
            )
            .into_any_element()
    }

    fn history_overlay(
        &self,
        dialog: &Dialog,
        layouts: &BTreeMap<usize, FieldLayout>,
        cx: &mut Context<CanvasApp>,
    ) -> AnyElement {
        let colors = self.colors();
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

    fn terminal_insert_overlay(&self, cx: &mut Context<CanvasApp>) -> AnyElement {
        let colors = self.colors();
        let Some(review) = &self.insert_review else {
            return div()
                .id("terminal-insert-review-missing")
                .role(Role::Status)
                .child("Нет подготовленного текста для вставки")
                .into_any_element();
        };
        let short_id = |value: &str| value.chars().take(10).collect::<String>();
        let target = format!(
            "{} · сессия {} · поколение {}",
            review.label,
            short_id(&review.target.session),
            short_id(&review.target.generation),
        );
        let safety_note: Option<String> = match &review.origin {
            crate::terminal_insert::Origin::Clipboard
                if review.text.contains('\n') || review.text.contains('\r') =>
            {
                Some(
                    "Переводы строк могут выполнить команды в shell. Проверьте получателя и текст."
                        .into(),
                )
            }
            crate::terminal_insert::Origin::Files { count } => Some(format!(
                "Вставляются пути ({count}) в формате POSIX shell. Содержимое файлов не загружается. Enter не добавляется."
            )),
            _ => None,
        };
        let close = self.action_control(
            "terminal-insert-cancel".into(),
            "Отмена".into(),
            ProductAction::CloseDialog,
            ControlStyle {
                role: Role::Button,
                selected: false,
                compact: true,
            },
            cx,
        );
        let confirm = self.action_control(
            "terminal-insert-confirm".into(),
            "Вставить".into(),
            ProductAction::ConfirmTerminalInsert,
            ControlStyle {
                role: Role::Button,
                selected: true,
                compact: true,
            },
            cx,
        );
        div()
            .id("terminal-insert-overlay")
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
                    .id("terminal-insert-review")
                    .role(Role::Dialog)
                    .aria_label("Проверить вставку")
                    .w_full()
                    .max_w(px(640.))
                    .mx(px(16.))
                    .flex()
                    .flex_col()
                    .gap_2()
                    .rounded(px(12.))
                    .border_1()
                    .border_color(colors.border)
                    .bg(colors.raised)
                    .p(px(18.))
                    .text_color(colors.text)
                    .on_click(cx.listener(|_, _: &ClickEvent, _, cx| cx.stop_propagation()))
                    .child(
                        div()
                            .id("terminal-insert-title")
                            .role(Role::Heading)
                            .aria_level(2)
                            .text_size(px(17.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Проверить вставку"),
                    )
                    .child(
                        div()
                            .id("terminal-insert-target")
                            .role(Role::Status)
                            .text_size(px(12.))
                            .text_color(colors.muted)
                            .child(target),
                    )
                    .when_some(safety_note, |card, note| {
                        card.child(
                            div()
                                .id("terminal-insert-safety-note")
                                .role(Role::Alert)
                                .text_size(px(12.))
                                .text_color(colors.accent)
                                .whitespace_normal()
                                .child(note),
                        )
                    })
                    .child(
                        div()
                            .id("terminal-insert-text")
                            .accessibility_id("terminal-insert-text")
                            .role(Role::Document)
                            .aria_label("Текст, подготовленный для вставки")
                            .w_full()
                            .min_h(px(120.))
                            .max_h(px(360.))
                            .overflow_y_scroll()
                            .track_scroll(&self.insert_scroll)
                            .font_family(crate::font::PRIMARY_FAMILY)
                            .whitespace_normal()
                            .bg(colors.base)
                            .border_1()
                            .border_color(colors.border)
                            .rounded(px(6.))
                            .p(px(12.))
                            .text_size(px(12.))
                            .text_color(colors.text)
                            .child(review.text.clone()),
                    )
                    .children(self.notice.clone().map(|notice| {
                        div()
                            .id("terminal-insert-notice")
                            .role(Role::Status)
                            .text_size(px(12.))
                            .text_color(colors.accent)
                            .child(notice)
                    }))
                    .child(
                        div()
                            .id("terminal-insert-keyboard-hint")
                            .role(Role::Status)
                            .text_size(px(11.))
                            .text_color(colors.muted)
                            .child("Ctrl+Enter — вставить · Ctrl+C — копировать · PgUp/PgDn — текст · Escape — отмена"),
                    )
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

    fn palette_overlay(
        &self,
        dialog: &Dialog,
        layouts: &BTreeMap<usize, FieldLayout>,
        cx: &mut Context<CanvasApp>,
    ) -> AnyElement {
        let colors = self.colors();
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
        let field_label = "Команда, имя панели, роль, проект…";
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

        let entries = self.palette_entries();
        let mut command_list = div()
            .id("product-command-list")
            .role(Role::List)
            .aria_label("Результаты поиска")
            .flex()
            .flex_col()
            .gap_1()
            .mt(px(8.))
            .max_h(px(420.))
            .overflow_y_scroll()
            .track_scroll(&self.palette_scroll);
        for (index, entry) in entries.iter().enumerate() {
            let action = entry.action.clone();
            command_list = command_list.child(
                div()
                    .id(format!("palette-result-{index}"))
                    .role(Role::Button)
                    .aria_selected(index == self.palette_index)
                    .aria_label(format!("{} {}", entry.label, entry.detail))
                    .px(px(10.))
                    .py(px(8.))
                    .rounded(px(6.))
                    .bg(if index == self.palette_index {
                        colors.selected
                    } else {
                        colors.raised
                    })
                    .hover(|style| style.bg(colors.hover))
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_size(px(13.))
                            .text_color(colors.text)
                            .truncate()
                            .child(entry.label.clone()),
                    )
                    .children((!entry.detail.is_empty()).then(|| {
                        div()
                            .text_size(px(11.))
                            .text_color(colors.muted)
                            .truncate()
                            .child(entry.detail.clone())
                    }))
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.choose_palette(action.clone(), window, cx);
                        cx.stop_propagation();
                    })),
            );
        }
        if self.palette_loading {
            command_list = command_list.child(
                div()
                    .px(px(10.))
                    .py(px(10.))
                    .text_color(colors.muted)
                    .child("Поиск панелей…"),
            );
        } else if entries.is_empty() {
            command_list = command_list.child(
                div()
                    .px(px(10.))
                    .py(px(10.))
                    .text_color(colors.muted)
                    .child("Ничего не найдено. Уточните запрос."),
            );
        }
        if let Some(error) = &self.palette_error {
            command_list = command_list.child(
                div()
                    .px(px(10.))
                    .text_size(px(11.))
                    .text_color(colors.muted)
                    .whitespace_normal()
                    .child(format!("Поиск панелей: {error}")),
            );
        }
        card = card.child(
            div()
                .px(px(8.))
                .text_size(px(11.))
                .text_color(colors.muted)
                .child("↑ ↓ — выбрать · Enter — открыть · Escape — закрыть"),
        );
        if let Some(notice) = &self.notice {
            card = card.child(
                div()
                    .px(px(8.))
                    .text_size(px(11.))
                    .text_color(colors.muted)
                    .whitespace_normal()
                    .child(notice.clone()),
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

    #[test]
    fn legacy_host_catalog_cannot_offer_template_writes() {
        let mut ui = ProductUi {
            catalog: Some(
                serde_json::from_value(serde_json::json!({
                    "projects":[],"workspaces":[],"tabs":[],
                    "selection":pigade_storage::Selection::default(),
                    "layout":pigade_storage::LayoutDocument::default()
                }))
                .unwrap(),
            ),
            dialog: Some(Dialog::new(DialogKind::Layouts)),
            ..Default::default()
        };
        assert!(ui.catalog.as_ref().unwrap().layout_templates.is_none());
        assert!(ui.menu_entries().is_empty());
        ui.catalog.as_mut().unwrap().layout_templates = Some(vec![]);
        assert_eq!(
            ui.menu_entries(),
            vec![(
                "Сохранить текущую раскладку…".into(),
                ProductAction::SaveLayout
            )]
        );
    }

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
    #[test]
    fn stop_confirmation_defaults_to_cancel_and_never_offers_resend() {
        use crate::session_control::{Phase, StopSheet, Target};
        let dialog = Dialog::new(DialogKind::StopSession);
        let mut ui = ProductUi {
            stop_sheet: Some(StopSheet {
                dialog: dialog.id,
                target: Target {
                    session: Uuid::from_u128(1),
                    generation: Uuid::from_u128(2),
                    title: "Review".into(),
                },
                phase: Phase::Draft,
            }),
            dialog: Some(dialog),
            ..Default::default()
        };
        assert_eq!(ui.menu_entries()[0].1, ProductAction::CloseDialog);
        assert_eq!(ui.menu_entries()[1].1, ProductAction::ConfirmStop);
        for phase in [
            Phase::Sending,
            Phase::Waiting,
            Phase::Unknown,
            Phase::Unavailable,
            Phase::Complete,
        ] {
            ui.stop_sheet.as_mut().unwrap().phase = phase;
            assert!(
                ui.menu_entries()
                    .iter()
                    .all(|(_, action)| *action != ProductAction::ConfirmStop)
            );
        }
    }

    #[test]
    fn hidden_session_actions_keep_live_generation_and_keyboard_row_together() {
        let session = Uuid::from_u128(11);
        let historical = SessionSummary {
            session: session.to_string(),
            generation: Uuid::from_u128(12).to_string(),
            title: "History".into(),
            state: "history".into(),
            pid: None,
        };
        let live = SessionSummary {
            session: session.to_string(),
            generation: Uuid::from_u128(13).to_string(),
            title: "Review".into(),
            state: "running".into(),
            pid: Some(42),
        };
        let mut ui = ProductUi {
            dialog: Some(Dialog::new(DialogKind::Sessions)),
            ..Default::default()
        };
        ui.replace_sessions(vec![historical, live]);
        assert_eq!(ui.dialog.as_ref().unwrap().active_field, 0);
        let entries = ui.menu_entries();
        assert_eq!(
            entries[0].1,
            ProductAction::StopSession {
                session,
                generation: Uuid::from_u128(13)
            }
        );
        assert_eq!(entries.len(), 3);
        assert_eq!(ui.session_action_row(0), 0);
        assert_eq!(ui.session_action_row(1), 1);
        assert_eq!(ui.session_action_row(2), 2);
    }
    #[test]
    fn a_late_session_list_cannot_reopen_or_replace_another_dialog() {
        let mut ui = ProductUi::default();
        ui.replace_sessions(vec![]);
        assert!(ui.dialog.is_none());
        let dialog = Dialog::new(DialogKind::StopSession);
        let id = dialog.id;
        ui.dialog = Some(dialog);
        ui.replace_sessions(vec![]);
        assert_eq!(ui.dialog.as_ref().unwrap().kind, DialogKind::StopSession);
        assert_eq!(ui.dialog.as_ref().unwrap().id, id);
    }
}

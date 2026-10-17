//! Native PigADE product chrome. The host owns commands and durable state;
//! this module renders the current catalog and emits typed user actions.

use gpui::{
    AnyElement, ClickEvent, Context, Div, FontWeight, Hsla, IntoElement, ParentElement, Role,
    StatefulInteractiveElement, Styled, div, prelude::InteractiveElement, px, rgb,
};
use pigade_storage::{Catalog, ContextDocument, Tab, Workspace, WorkspaceStatus};
use uuid::Uuid;

use super::CanvasApp;

const SIDEBAR_WIDTH: f32 = 240.0;
const ROW_HEIGHT: f32 = 30.0;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum ProductAction {
    AddProject,
    CreateWorkspace,
    CreateTab,
    OpenTransfer,
    TransferTo(Uuid),
    SelectWorkspace(Uuid),
    SelectTab(Uuid),
    Launch(String),
    Layout(String),
    EditContext,
    OpenPalette,
    CloseDialog,
    SubmitDialog,
    EditField(usize),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum DialogKind {
    AddProject,
    CreateWorkspace,
    NewTab,
    Palette,
    Context,
    Transfer,
}

impl DialogKind {
    fn title(self) -> &'static str {
        match self {
            Self::AddProject => "Добавить проект",
            Self::CreateWorkspace => "Новый воркспейс",
            Self::NewTab => "Новая вкладка",
            Self::Palette => "Команды PigADE",
            Self::Context => "Контекст воркспейса",
            Self::Transfer => "Перенести панель",
        }
    }

    fn fields(self) -> &'static [&'static str] {
        match self {
            Self::AddProject => &["Путь к Git-проекту"],
            Self::CreateWorkspace => &["Задача", "Режим: worktree / shared"],
            Self::NewTab => &["Название вкладки"],
            Self::Palette => &["Поиск команды"],
            Self::Context => &["Инструкции", "Роль", "Память"],
            Self::Transfer => &[],
        }
    }

    fn submit_label(self) -> &'static str {
        match self {
            Self::AddProject => "Добавить проект",
            Self::CreateWorkspace => "Создать воркспейс",
            Self::NewTab => "Создать вкладку",
            Self::Palette => "Закрыть",
            Self::Context => "Сохранить ревизию",
            Self::Transfer => "Закрыть",
        }
    }
}

#[derive(Clone, Debug)]
pub(super) struct Dialog {
    pub(super) kind: DialogKind,
    pub(super) fields: Vec<String>,
    pub(super) active_field: usize,
}

impl Dialog {
    pub(super) fn new(kind: DialogKind) -> Self {
        Self {
            kind,
            fields: match kind {
                DialogKind::CreateWorkspace => vec![String::new(), "worktree".into()],
                _ => vec![String::new(); kind.fields().len()],
            },
            active_field: 0,
        }
    }
}

#[derive(Default)]
pub(super) struct ProductUi {
    pub(super) catalog: Option<Catalog>,
    pub(super) context: Option<ContextDocument>,
    pub(super) notice: Option<String>,
    pub(super) busy: bool,
    pub(super) dialog: Option<Dialog>,
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

fn palette_commands() -> Vec<(&'static str, ProductAction)> {
    vec![
        ("Добавить проект", ProductAction::AddProject),
        ("Новый воркспейс", ProductAction::CreateWorkspace),
        ("Контекст воркспейса", ProductAction::EditContext),
        ("Перенести панель", ProductAction::OpenTransfer),
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
    ]
}

fn multiline_lines(value: &str) -> Div {
    value
        .split('\n')
        .fold(div().flex().flex_col().items_start(), |lines, line| {
            lines.child(
                div()
                    .h(px(16.))
                    .text_size(px(12.))
                    .child(if line.is_empty() {
                        " ".to_owned()
                    } else {
                        line.to_owned()
                    }),
            )
        })
}

impl ProductUi {
    pub(super) fn palette_action(&self) -> Option<ProductAction> {
        let dialog = self.dialog.as_ref()?;
        if dialog.kind != DialogKind::Palette {
            return None;
        }
        let query = dialog.fields.first().map(String::as_str).unwrap_or("");
        if query.trim().is_empty() {
            return None;
        }
        palette_commands()
            .into_iter()
            .find(|(label, _)| label.to_lowercase().contains(&query.to_lowercase()))
            .map(|(_, action)| action)
    }

    pub(super) fn insert_text(&mut self, text: &str) {
        let Some(dialog) = self.dialog.as_mut() else {
            return;
        };
        if dialog.kind == DialogKind::Transfer {
            return;
        }
        let Some(field) = dialog.fields.get_mut(dialog.active_field) else {
            return;
        };
        const MAX_FIELD_BYTES: usize = 16 * 1024;
        let multiline_context = dialog.kind == DialogKind::Context;
        if text
            .chars()
            .any(|character| character.is_control() && !(multiline_context && character == '\n'))
            || field.len().saturating_add(text.len()) > MAX_FIELD_BYTES
        {
            return;
        }
        field.push_str(text);
    }

    pub(super) fn backspace(&mut self) {
        let Some(dialog) = self.dialog.as_mut() else {
            return;
        };
        if dialog.kind == DialogKind::Transfer {
            return;
        }
        let Some(field) = dialog.fields.get_mut(dialog.active_field) else {
            return;
        };
        field.pop();
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
                project_tree = project_tree.child(
                    div()
                        .id(format!("project-{}", project.id))
                        .role(Role::ListItem)
                        .aria_label(format!("Проект {}", project.name))
                        .w_full()
                        .px(px(8.))
                        .py(px(7.))
                        .text_size(px(12.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(colors.muted)
                        .child(project.name.clone()),
                );

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

    pub(super) fn overlay(&self, cx: &mut Context<CanvasApp>) -> Option<AnyElement> {
        let dialog = self.dialog.as_ref()?;
        Some(self.dialog_overlay(dialog, cx))
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

    fn dialog_overlay(&self, dialog: &Dialog, cx: &mut Context<CanvasApp>) -> AnyElement {
        let colors = ProductColors::dark();
        if dialog.kind == DialogKind::Palette {
            return self.palette_overlay(dialog, cx);
        }
        if dialog.kind == DialogKind::Transfer {
            return self.transfer_overlay(cx);
        }

        let mut fields = div().flex().flex_col().gap_2().mt(px(16.));
        for (index, label) in dialog.kind.fields().iter().enumerate() {
            let value = dialog.fields.get(index).map(String::as_str).unwrap_or("");
            let active = index == dialog.active_field;
            let multiline = dialog.kind == DialogKind::Context && index != 1;
            let shown = if value.is_empty() {
                if active {
                    format!("{label}…▏")
                } else {
                    format!("{label}…")
                }
            } else if active {
                format!("{value}▏")
            } else {
                value.to_owned()
            };
            let mut field = div()
                .id(format!("product-dialog-field-{index}"))
                .role(Role::TextInput)
                .aria_label(*label)
                .w_full()
                .h(px(if multiline { 90. } else { 42. }))
                .px(px(12.))
                .flex()
                .rounded(px(7.))
                .border_1()
                .border_color(if active { colors.accent } else { colors.border })
                .bg(colors.base)
                .text_color(if value.is_empty() {
                    colors.muted
                } else {
                    colors.text
                })
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    this.product_action(ProductAction::EditField(index), window, cx);
                    cx.stop_propagation();
                }));
            field = if multiline {
                field.items_start().child(multiline_lines(&shown))
            } else {
                field.items_center().child(shown)
            };
            fields = fields.child(field);
        }

        if dialog.kind == DialogKind::Context {
            fields = fields.child(
                div()
                    .id("context-revision-note")
                    .role(Role::Status)
                    .text_size(px(11.))
                    .text_color(colors.muted)
                    .child("Новая ревизия; работающие сессии не изменяются."),
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

    fn palette_overlay(&self, dialog: &Dialog, cx: &mut Context<CanvasApp>) -> AnyElement {
        let colors = ProductColors::dark();
        let query = dialog.fields.first().map(String::as_str).unwrap_or("");
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
        let shown = if query.is_empty() {
            format!("{field_label}…▏")
        } else {
            format!("{query}▏")
        };
        card = card.child(
            div()
                .id("product-dialog-field-0")
                .role(Role::SearchInput)
                .aria_label(field_label)
                .h(px(42.))
                .w_full()
                .px(px(12.))
                .flex()
                .items_center()
                .rounded(px(7.))
                .border_1()
                .border_color(colors.accent)
                .bg(colors.base)
                .text_color(if query.is_empty() {
                    colors.muted
                } else {
                    colors.text
                })
                .child(shown)
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                    this.product_action(ProductAction::EditField(0), window, cx);
                })),
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

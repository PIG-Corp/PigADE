//! In-pane immutable history cells. The live replica remains untouched.
use super::*;
use scrollback::{Completion, GridQuery, PaneIdentity};

fn identity(view: &View) -> Option<PaneIdentity> {
    Some(PaneIdentity {
        session: view.session.parse().ok()?,
        generation: view.generation.parse().ok()?,
    })
}
fn rgb_spec(value: &str, foreground: bool, palette: &[(u16, [u8; 3])]) -> String {
    let value = color(value, foreground, palette);
    format!(
        "Spec(Rgb {{ r: {}, g: {}, b: {} }})",
        value >> 16,
        (value >> 8) & 255,
        value & 255
    )
}
fn blank() -> Cell {
    Cell {
        text: " ".into(),
        fg: "Named(Foreground)".into(),
        bg: "Named(Background)".into(),
        flags: 0,
        underline: None,
        hyperlink: None,
    }
}

/// Resolve historical palettes once, preserving styles and physical cell positions.
/// This is a typed-cell projection, never an ANSI parser or a terminal model.
fn display_page(base: &View, page: &protocol::HistoryGridPage) -> Result<View, String> {
    let rows = page.rows_requested;
    let cols = page.cols;
    if !(1..=128).contains(&rows)
        || !(1..=1024).contains(&cols)
        || u32::from(rows) * u32::from(cols) > 32768
        || page.session.to_string() != base.session
        || page.generation.to_string() != base.generation
        || page.start > page.total_rows
        || page.rows.len() > usize::from(rows)
        || page.rows.len() as u64 > page.total_rows.saturating_sub(page.start)
        || page.hyperlinks.len() > 32768
        || page.warnings.len() > 16
    {
        return Err("Недопустимая геометрия или источник страницы истории".into());
    }
    let mut budget = page
        .hyperlinks
        .iter()
        .try_fold(0usize, |sum, (id, uri)| {
            sum.checked_add(id.len())?.checked_add(uri.len())
        })
        .ok_or("Переполнение размера страницы истории")?;
    if budget > 4 * 1024 * 1024 {
        return Err("Страница истории превышает 4 МиБ".into());
    }
    let mut cells = Vec::with_capacity(usize::from(rows) * usize::from(cols));
    for (index, row) in page.rows.iter().enumerate() {
        if row.row != page.start + index as u64
            || row.source_columns > 1024
            || row.cells.len() != usize::from(row.source_columns.min(cols))
            || (row.boundary.is_some() && !row.cells.is_empty())
        {
            return Err("Нарушен порядок или ширина строки истории".into());
        }
        for cell in &row.cells {
            budget = budget
                .checked_add(
                    cell.text.len()
                        + cell.fg.len()
                        + cell.bg.len()
                        + cell.underline.as_ref().map_or(0, String::len)
                        + 64,
                )
                .ok_or("Переполнение размера страницы истории")?;
            if budget > 4 * 1024 * 1024
                || cell.fg.len() > 64
                || cell.bg.len() > 64
                || cell
                    .underline
                    .as_ref()
                    .is_some_and(|value| value.len() > 64)
                || cell
                    .hyperlink
                    .is_some_and(|link| link as usize >= page.hyperlinks.len())
            {
                return Err("Страница истории превышает лимит или содержит неверную ссылку".into());
            }
            let mut cell = cell.clone();
            cell.fg = rgb_spec(&cell.fg, true, &row.palette_overrides);
            cell.bg = rgb_spec(&cell.bg, false, &row.palette_overrides);
            cell.underline = cell
                .underline
                .as_ref()
                .map(|value| rgb_spec(value, true, &row.palette_overrides));
            cells.push(cell);
        }
        if let Some(boundary) = &row.boundary {
            if boundary.len() > 1024 {
                return Err("Слишком длинная граница истории".into());
            }
            for ch in format!("--- {boundary} ---")
                .chars()
                .filter(|ch| !ch.is_control())
                .take(usize::from(cols))
            {
                let mut cell = blank();
                cell.text = ch.to_string();
                cell.fg = "Named(BrightBlack)".into();
                cells.push(cell);
            }
        }
        while cells.len() < (index + 1) * usize::from(cols) {
            let mut cell = blank();
            cell.fg = rgb_spec(&cell.fg, true, &row.palette_overrides);
            cell.bg = rgb_spec(&cell.bg, false, &row.palette_overrides);
            cells.push(cell);
        }
    }
    while cells.len() < usize::from(rows) * usize::from(cols) {
        cells.push(blank());
    }
    let mut view = base.clone();
    view.sequence = page.sequence;
    view.state = "history".into();
    view.frame.geometry = protocol::Geometry { rows, cols };
    view.frame.cells = cells;
    view.frame.hyperlinks = page.hyperlinks.clone();
    view.frame.cursor = (0, 0);
    view.frame.cursor_style = "Hidden".into();
    view.frame.cursor_presentation = Some(protocol::CursorPresentation {
        shape: protocol::CursorPresentationShape::Hidden,
        blinking: false,
    });
    view.frame.palette_overrides = vec![];
    view.frame.modes = 0;
    view.frame.sync_pending_bytes = 0;
    Ok(view)
}

impl CanvasApp {
    pub(super) fn history_target(&self, view: &View) -> bool {
        self.scrollback
            .active_identity()
            .is_some_and(|active| identity(view) == Some(active))
    }
    fn scrollback_current(&self) -> Option<PaneIdentity> {
        let active = self.scrollback.active_identity()?;
        self.state
            .views
            .iter()
            .any(|view| identity(view) == Some(active))
            .then_some(active)
    }
    pub(super) fn selected_history(&self) -> bool {
        self.state
            .views
            .get(self.selected)
            .is_some_and(|view| self.history_target(view))
    }
    fn history_dimensions(&self) -> (u16, u16) {
        self.state
            .views
            .iter()
            .find(|view| self.history_target(view))
            .map_or((24, 80), |view| {
                (view.frame.geometry.rows, view.frame.geometry.cols)
            })
    }
    pub(super) fn return_to_live(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.scrollback.end_live();
        self.scrollback_wheel.reset();
        self.scrollback_display = None;
        self.scrollback_selection = None;
        self.scrollback_dragging = false;
        self.input_epoch = self.input_epoch.wrapping_add(1);
        window.notify_text_input_changed(&self.focus);
        cx.notify();
    }
    pub(super) fn open_scrollback(
        &mut self,
        index: usize,
        back: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self
            .product
            .catalog
            .as_ref()
            .and_then(|catalog| catalog.history_grid_schema)
            != Some(1)
        {
            self.input_notice=Some("Прокрутка клеточной истории недоступна у подключённого host; доступен текстовый просмотр истории.".into());
            cx.notify();
            return;
        }
        if !self.text_input.preedit.is_empty()
            || self.selection_dragging
            || self.pane_drag.is_some()
            || self.resize_gesture.is_some()
        {
            return;
        }
        self.finish_pointer(cx);
        if self
            .pointer
            .as_ref()
            .is_some_and(|route| route.encoder.has_pressed())
        {
            return;
        }
        let Some(view) = self.state.views.get(index) else {
            return;
        };
        let Some(target) = identity(view) else { return };
        let rows = view.frame.geometry.rows;
        let cols = view.frame.geometry.cols;
        let mut frozen = view.clone();
        frozen.state = "history".into();
        frozen.frame.cursor_presentation = Some(protocol::CursorPresentation {
            shape: protocol::CursorPresentationShape::Hidden,
            blinking: false,
        });
        frozen.frame.modes = 0;
        self.scrollback.open(target, rows, cols, back);
        self.scrollback_wheel.reset();
        self.scrollback_display = Some(frozen);
        self.scrollback_selection = None;
        self.scrollback_dragging = false;
        self.input_epoch = self.input_epoch.wrapping_add(1);
        self.text_input.cancel();
        window.notify_text_input_changed(&self.focus);
        self.service_scrollback(window, cx);
        cx.notify();
    }
    pub(super) fn service_scrollback(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let current = self.scrollback_current();
        self.scrollback.reconcile_generation(current);
        if self.scrollback.is_live() {
            if self.scrollback_display.is_some() {
                self.input_epoch = self.input_epoch.wrapping_add(1);
                window.notify_text_input_changed(&self.focus);
            }
            self.scrollback_display = None;
            self.scrollback_selection = None;
            self.scrollback_dragging = false;
        }
        if self.product.busy || self.close_flow.active() {
            return;
        }
        let (rows, cols) = self.history_dimensions();
        let Some(request) = self.scrollback.prepare_next(current, rows, cols) else {
            return;
        };
        let query = match request.query {
            GridQuery::OpenGrid {
                session,
                generation,
                rows,
                cols,
                back,
            } => protocol::HistoryViewQuery::OpenGrid {
                session,
                generation,
                rows,
                cols,
                back,
            },
            GridQuery::Grid {
                snapshot,
                start,
                rows,
                cols,
            } => protocol::HistoryViewQuery::Grid {
                snapshot,
                start,
                rows,
                cols,
            },
            GridQuery::Close { snapshot } => protocol::HistoryViewQuery::Close { snapshot },
        };
        if self.try_product_command(
            json!({"command":"history_view","query":query,"_ui_request":request.id}),
            cx,
        ) {
            self.scrollback_request = Some(request.id);
        } else {
            self.scrollback.fail_request(
                request.id,
                "Запрос истории не принят. Прокрутите снова или вернитесь к текущему выводу.",
            );
            cx.notify();
        }
    }
    pub(super) fn receive_scrollback(
        &mut self,
        request: Option<uuid::Uuid>,
        reply: Option<protocol::HistoryViewReply>,
        cx: &mut Context<Self>,
    ) -> bool {
        if request.is_none() || request != self.scrollback_request {
            return false;
        }
        self.scrollback_request = None;
        let current = self.scrollback_current();
        let id = request.expect("matched request");
        let completion = match reply {
            Some(protocol::HistoryViewReply::Grid { page }) => {
                let converted = self
                    .state
                    .views
                    .iter()
                    .find(|view| {
                        identity(view)
                            == Some(PaneIdentity {
                                session: page.session,
                                generation: page.generation,
                            })
                    })
                    .ok_or_else(|| "Панель больше не отображается".into())
                    .and_then(|base| display_page(base, &page));
                match converted {
                    Ok(view) => {
                        let completion = self.scrollback.complete_page(id, Ok(page), current);
                        if matches!(completion, Completion::AcceptedPage(_)) {
                            self.scrollback_display = Some(view);
                            self.scrollback_selection = None;
                            self.scrollback_dragging = false;
                        }
                        completion
                    }
                    Err(error) => self.scrollback.complete_page(id, Err(error), current),
                }
            }
            Some(protocol::HistoryViewReply::Closed { snapshot }) => {
                self.scrollback.complete_close(id, snapshot)
            }
            _ => self.scrollback.fail_request(
                id,
                "Host вернул ответ другого типа; текущий вывод не заменён",
            ),
        };
        if matches!(completion, Completion::Failed) {
            self.input_notice = self.scrollback.error.clone();
        }
        if self.scrollback.is_live() {
            self.scrollback_display = None;
        }
        cx.notify();
        true
    }
    pub(super) fn fail_scrollback(
        &mut self,
        request: Option<uuid::Uuid>,
        error: &str,
        cx: &mut Context<Self>,
    ) -> bool {
        if request.is_none() || request != self.scrollback_request {
            return false;
        }
        self.scrollback_request = None;
        self.scrollback
            .fail_request(request.expect("matched"), error);
        cx.notify();
        true
    }
    pub(super) fn scrollback_status(&self) -> Option<String> {
        if self.scrollback.is_live() {
            return None;
        }
        if let Some(error) = &self.scrollback.error {
            return Some(format!("История: {error} · End / Escape — текущий вывод"));
        }
        Some(self.scrollback.visible().map_or_else(
            || "Загрузка истории · End / Escape — текущий вывод".into(),
            |page| {
                format!(
                    "История · строки {}–{} / {} · seq {} · End / Escape — текущий вывод{}",
                    page.start + 1,
                    (page.start + u64::from(page.rows)).min(page.total_rows),
                    page.total_rows,
                    page.sequence,
                    if self
                        .scrollback
                        .displayed_page()
                        .is_some_and(|page| page.horizontal_clipped)
                    {
                        " · строки исходной ширины обрезаны справа"
                    } else {
                        ""
                    }
                )
            },
        ))
    }
    pub(super) fn scrollback_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.product.dialog.is_some() || self.prefix || self.layout_mode.is_some() {
            return false;
        }
        let key = &event.keystroke;
        if !self.selected_history() {
            if key.key == "pageup"
                && key.modifiers.shift
                && !key.modifiers.control
                && !key.modifiers.alt
                && !key.modifiers.platform
            {
                if !event.is_held {
                    let rows = self
                        .state
                        .views
                        .get(self.selected)
                        .map_or(24, |view| view.frame.geometry.rows);
                    self.open_scrollback(self.selected, u64::from(rows), window, cx);
                }
                cx.stop_propagation();
                return true;
            }
            return false;
        }
        // Management modifier and prefix remain available; terminal input does not.
        if key.key == "space" && key.modifiers.control && key.modifiers.shift {
            return false;
        }
        if controls::classify(
            self.product.command_modifier,
            &key.key,
            input::Modifiers {
                control: key.modifiers.control,
                alt: key.modifiers.alt,
                shift: key.modifiers.shift,
                platform: key.modifiers.platform,
            },
        )
        .is_some()
        {
            return false;
        }
        let (rows, cols) = self.history_dimensions();
        if !key.modifiers.control && !key.modifiers.alt && !key.modifiers.platform {
            match key.key.as_str() {
                "escape" | "end" => self.return_to_live(window, cx),
                "home" => self.scrollback.home(rows, cols),
                "pageup" => self.scrollback.page_step(1, rows, cols),
                "pagedown" => self.scrollback.page_step(-1, rows, cols),
                "up" => self.scrollback.scroll_lines(1, rows, cols),
                "down" => self.scrollback.scroll_lines(-1, rows, cols),
                _ => (),
            }
        } else if key.modifiers.control
            && key.modifiers.shift
            && !key.modifiers.alt
            && !key.modifiers.platform
        {
            if key.key == "c" {
                self.copy_selection(cx);
            } else if key.key == "a" {
                self.select_visible(cx);
            }
        }
        self.service_scrollback(window, cx);
        cx.stop_propagation();
        cx.notify();
        true
    }
}

#[cfg(test)]
mod tests {
    use super::{blank, color, display_page};
    use crate::protocol::{self, View};
    fn fixture() -> (View, protocol::HistoryGridPage) {
        let session = uuid::Uuid::from_u128(1);
        let generation = uuid::Uuid::from_u128(2);
        let cell = blank();
        let view = View {
            session: session.to_string(),
            generation: generation.to_string(),
            sequence: 25,
            state: "running".into(),
            recovery_path: "checkpoint_tail".into(),
            pid: Some(42),
            storage_error: None,
            frame: protocol::Frame {
                geometry: protocol::Geometry { rows: 2, cols: 2 },
                cells: vec![cell.clone(); 4],
                hyperlinks: vec![],
                cursor: (1, 1),
                cursor_style: "Block".into(),
                cursor_presentation: None,
                modes: 1,
                colors: String::new(),
                palette_overrides: vec![],
                history_lines: 10,
                sync_pending_bytes: 0,
                title: "shell".into(),
            },
        };
        let mut styled = cell;
        styled.text = "界".into();
        styled.flags = 0x20;
        styled.fg = "Named(Red)".into();
        styled.underline = Some("Named(Red)".into());
        let page = protocol::HistoryGridPage {
            snapshot: uuid::Uuid::from_u128(3),
            session,
            generation,
            sequence: 20,
            total_rows: 12,
            start: 4,
            rows_requested: 2,
            rows: vec![
                protocol::HistoryGridRow {
                    row: 4,
                    source_columns: 1,
                    boundary: None,
                    cells: vec![styled.clone()],
                    palette_overrides: vec![(1, [1, 2, 3]), (257, [4, 5, 6])],
                },
                protocol::HistoryGridRow {
                    row: 5,
                    source_columns: 1,
                    boundary: None,
                    cells: vec![styled],
                    palette_overrides: vec![(1, [7, 8, 9])],
                },
            ],
            hyperlinks: vec![],
            cols: 2,
            horizontal_clipped: false,
            warnings: vec![],
        };
        (view, page)
    }
    #[test]
    fn historical_rows_keep_their_own_palette_and_never_mutate_live_replica_or_cursor() {
        let (base, page) = fixture();
        let original = base.clone();
        let display = display_page(&base, &page).unwrap();
        assert_eq!(base, original);
        assert_eq!(display.sequence, 20);
        assert_eq!(display.state, "history");
        assert_eq!(display.frame.cells[0].text, "界");
        assert_eq!(display.frame.cells[0].flags, 0x20);
        assert_eq!(color(&display.frame.cells[0].fg, true, &[]), 0x010203);
        assert_eq!(color(&display.frame.cells[2].fg, true, &[]), 0x070809);
        assert_eq!(color(&display.frame.cells[1].bg, false, &[]), 0x040506);
        assert_eq!(
            display.frame.cursor_presentation.unwrap().shape,
            protocol::CursorPresentationShape::Hidden
        );
        assert_eq!(display.frame.modes, 0);
    }
    #[test]
    fn wrong_generation_unordered_rows_and_dangling_hyperlinks_never_replace_the_display() {
        let (base, mut page) = fixture();
        page.generation = uuid::Uuid::nil();
        assert!(display_page(&base, &page).is_err());
        let (base, mut page) = fixture();
        page.rows[1].row = 4;
        assert!(display_page(&base, &page).is_err());
        let (base, mut page) = fixture();
        page.rows[0].cells[0].hyperlink = Some(0);
        assert!(display_page(&base, &page).is_err());
        let (base, mut page) = fixture();
        page.rows_requested = 129;
        assert!(display_page(&base, &page).is_err());
    }
}

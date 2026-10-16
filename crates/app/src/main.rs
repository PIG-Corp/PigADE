//! PigADE native application. The session host owns ANSI parsing and CLI lifetime.
mod input;
mod protocol;
use gpui::{prelude::*, *};
use protocol::{Cell, View, command, request};
use serde_json::{Value, json};
use std::{
    ops::Range,
    path::PathBuf,
    sync::{Arc, Mutex, mpsc},
    thread,
    time::Duration,
};

const FONT_SIZE: f32 = 14.;
const HEADER: f32 = 32.;
const GAP: f32 = 6.;
#[derive(Clone, Default)]
struct State {
    revision: u64,
    views: Vec<View>,
    error: Option<String>,
}
impl State {
    fn fail(&mut self, error: String) {
        if self.error.is_none() {
            self.error = Some(error);
            self.revision = self.revision.wrapping_add(1);
        }
    }
}

fn ipc(socket: PathBuf) -> (mpsc::SyncSender<Value>, Arc<Mutex<State>>) {
    let (tx, rx) = mpsc::sync_channel(32);
    let shared = Arc::new(Mutex::new(State::default()));
    let output = shared.clone();
    thread::spawn(move || {
        loop {
            // At most 32 queued commands, no retries of ambiguous input/resize.
            while let Ok(command) = rx.try_recv() {
                if let Err(e) = request(&socket, command) {
                    output
                        .lock()
                        .unwrap()
                        .fail(format!("Команда: исход неизвестен: {e}"));
                }
            }
            let known = output
                .lock()
                .unwrap()
                .views
                .iter()
                .map(View::known)
                .collect::<Vec<_>>();
            match protocol::poll(&socket, &known) {
                Ok(reply) => {
                    let mut state = output.lock().unwrap();
                    match protocol::merge_poll(&mut state.views, reply) {
                        Ok(true) => state.revision = state.revision.wrapping_add(1),
                        Ok(false) => (),
                        Err(error) => state.fail(error),
                    }
                }
                Err(e) => {
                    output.lock().unwrap().fail(e);
                }
            }
            thread::sleep(Duration::from_millis(50));
        }
    });
    (tx, shared)
}

struct CanvasApp {
    focus: FocusHandle,
    selected: usize,
    prefix: bool,
    state: State,
    tx: mpsc::SyncSender<Value>,
    shared: Arc<Mutex<State>>,
    panes: Vec<PaneGeometry>,
    text_input: input::TextInput,
    input_error: Option<String>,
    resized: Vec<(String, u16, u16)>,
}
impl CanvasApp {
    fn new(cx: &mut Context<Self>, tx: mpsc::SyncSender<Value>, shared: Arc<Mutex<State>>) -> Self {
        cx.spawn(async move |weak, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(50))
                    .await;
                if weak
                    .update(cx, |this, cx| {
                        let next = {
                            let shared = this.shared.lock().unwrap();
                            (this.state.revision != shared.revision).then(|| shared.clone())
                        };
                        if let Some(next) = next {
                            this.state = next;
                            this.selected =
                                this.selected.min(this.state.views.len().saturating_sub(1));
                            cx.notify();
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
        Self {
            focus: cx.focus_handle(),
            selected: 0,
            prefix: false,
            state: State::default(),
            tx,
            shared,
            panes: Vec::new(),
            text_input: input::TextInput::default(),
            input_error: None,
            resized: Vec::new(),
        }
    }
    fn send(&mut self, bytes: Vec<u8>, cx: &mut Context<Self>) {
        if bytes.is_empty() {
            return;
        }
        if bytes.len() > 16 * 1024 {
            self.input_error = Some("Ввод более 16 KiB не принят; разделите текст".into());
            cx.notify();
            return;
        }
        if let Some(view) = self.state.views.get(self.selected) {
            if view.state != "running" || self.error().is_some() || view.storage_error.is_some() {
                return;
            }
            let mut value = command(view, "input");
            value["bytes"] = json!(bytes);
            if self.tx.try_send(value).is_err() {
                self.input_error = Some("Очередь ввода заполнена; ввод не принят".into());
                cx.notify();
            }
        }
    }
    fn error(&self) -> Option<&str> {
        self.input_error.as_deref().or(self.state.error.as_deref())
    }
    fn key(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let k = &event.keystroke;
        if k.modifiers.control
            && k.modifiers.shift
            && !k.modifiers.alt
            && !k.modifiers.platform
            && k.key == "space"
        {
            self.prefix = !self.prefix;
            self.text_input.cancel();
            cx.notify();
            cx.stop_propagation();
            return;
        }
        if self.prefix {
            self.prefix = false;
            if let Ok(index) = k.key.parse::<usize>()
                && (1..=self.state.views.len()).contains(&index)
            {
                self.selected = index - 1;
            }
            // Only this explicit acknowledgement clears a local rejected-input error.
            // It never resends a failed or unknown command.
            if k.key == "escape" {
                self.input_error = None;
            }
            self.text_input.cancel();
            cx.notify();
            cx.stop_propagation();
            return;
        }
        // An active composition owns ordinary editing/navigation until commit.
        if !self.text_input.preedit.is_empty() && !k.modifiers.control && !k.modifiers.alt {
            return;
        }
        let modes = self
            .state
            .views
            .get(self.selected)
            .map_or(0, |v| v.frame.modes);
        let encoded = input::encode(
            input::Key {
                name: &k.key,
                text: k.key_char.as_deref(),
                modifiers: input::Modifiers {
                    control: k.modifiers.control,
                    alt: k.modifiers.alt,
                    shift: k.modifiers.shift,
                    platform: k.modifiers.platform,
                },
            },
            modes,
        );
        self.text_input.key(false);
        match encoded {
            input::Encoded::Bytes(bytes) => {
                self.send(bytes, cx);
                cx.stop_propagation();
            }
            input::Encoded::PlatformText { meta } => self.text_input.key(meta),
            input::Encoded::Unsupported(reason) => {
                self.input_error = Some(reason.into());
                cx.notify();
                cx.stop_propagation();
            }
            input::Encoded::Ignore => {
                cx.stop_propagation();
            }
        }
    }
    fn geometry(
        &mut self,
        bounds: Bounds<Pixels>,
        cell: Size<Pixels>,
        scale: f32,
    ) -> Vec<PaneGeometry> {
        self.panes = project_panels(bounds, self.state.views.len(), scale)
            .into_iter()
            .map(|bounds| PaneGeometry::new(bounds, cell, scale))
            .collect();
        for (view, pane) in self.state.views.iter().zip(&self.panes) {
            let (rows, cols) = (pane.rows, pane.cols);
            if view.state == "running"
                && !self
                    .resized
                    .contains(&(view.generation.clone(), rows, cols))
            {
                let mut value = command(view, "resize");
                value["geometry"] = json!({"rows":rows,"cols":cols});
                if self.tx.try_send(value).is_err() {
                    self.input_error = Some("Очередь resize заполнена; размер не принят".into());
                } else {
                    self.resized
                        .retain(|(generation, _, _)| generation != &view.generation);
                    self.resized.push((view.generation.clone(), rows, cols));
                }
            }
        }
        self.panes.clone()
    }
}

/// All coordinates, cell advances and viewport rounding share device pixels.
/// Font size itself is unchanged: no scale transform is applied to terminal text.
#[derive(Clone, Copy, Debug)]
struct PaneGeometry {
    bounds: Bounds<Pixels>,
    origin: Point<Pixels>,
    cell: Size<Pixels>,
    rows: u16,
    cols: u16,
}
fn valid_scale(scale: f32) -> f32 {
    if scale.is_finite() && scale > 0. {
        scale
    } else {
        1.
    }
}
impl PaneGeometry {
    fn new(bounds: Bounds<Pixels>, metrics: Size<Pixels>, scale: f32) -> Self {
        let scale = valid_scale(scale);
        let cw = (f32::from(metrics.width) * scale).ceil().max(1.);
        let ch = (f32::from(metrics.height) * scale).ceil().max(1.);
        let x = ((f32::from(bounds.left()) + 8.) * scale).ceil();
        let y = ((f32::from(bounds.top()) + HEADER + 8.) * scale).ceil();
        let right = ((f32::from(bounds.right()) - 8.) * scale).floor();
        let bottom = ((f32::from(bounds.bottom()) - 8.) * scale).floor();
        Self {
            bounds,
            origin: point(px(x / scale), px(y / scale)),
            cell: size(px(cw / scale), px(ch / scale)),
            rows: ((bottom - y).max(0.) / ch).floor().clamp(1., 128.) as u16,
            cols: ((right - x).max(0.) / cw).floor().clamp(2., 256.) as u16,
        }
    }
}
fn project_panels(bounds: Bounds<Pixels>, count: usize, scale: f32) -> Vec<Bounds<Pixels>> {
    let count = count.clamp(1, 4);
    let scale = valid_scale(scale);
    let x = (f32::from(bounds.left()) * scale).ceil();
    let y = (f32::from(bounds.top()) * scale).ceil();
    let right = (f32::from(bounds.right()) * scale).floor().max(x);
    let bottom = (f32::from(bounds.bottom()) * scale).floor().max(y);
    let width = right - x;
    let height = bottom - y;
    let gap = (GAP * scale).ceil();
    let split_w = ((width - gap).max(0.) / 2.).floor();
    let split_h = ((height - gap).max(0.) / 2.).floor();
    (0..count)
        .map(|index| {
            let columns = if count == 1 || (count == 3 && index == 2) {
                1
            } else {
                2
            };
            let rows = if count <= 2 { 1 } else { 2 };
            let left = if columns == 2 && index % 2 == 1 {
                x + split_w + gap
            } else {
                x
            };
            let top = if rows == 2 && index >= 2 {
                y + split_h + gap
            } else {
                y
            };
            let pane_right = if columns == 2 && index % 2 == 0 {
                x + split_w
            } else {
                right
            };
            let pane_bottom = if rows == 2 && index < 2 {
                y + split_h
            } else {
                bottom
            };
            Bounds::new(
                point(px(left / scale), px(top / scale)),
                size(
                    px((pane_right - left).max(0.) / scale),
                    px((pane_bottom - top).max(0.) / scale),
                ),
            )
        })
        .collect()
}
impl EntityInputHandler for CanvasApp {
    fn text_for_range(
        &mut self,
        _: Range<usize>,
        _: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        Some(self.text_input.preedit.clone())
    }
    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: 0..0,
            reversed: false,
        })
    }
    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        if self.text_input.preedit.is_empty() {
            None
        } else {
            Some(0..self.text_input.preedit.encode_utf16().count())
        }
    }
    fn unmark_text(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.text_input.cancel();
        cx.notify();
    }
    fn replace_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let modes = self
            .state
            .views
            .get(self.selected)
            .map_or(0, |v| v.frame.modes);
        if let Some(error) = input::protocol_error(modes) {
            self.input_error = Some(error.into());
        } else {
            let bytes = self.text_input.commit(text);
            self.send(bytes, cx);
        }
        cx.notify();
    }
    fn replace_and_mark_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        text: &str,
        _: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.text_input.preedit = text.into();
        cx.notify();
    }
    fn bounds_for_range(
        &mut self,
        _: Range<usize>,
        _: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let pane = self.panes.get(self.selected)?;
        let view = self.state.views.get(self.selected)?;
        Some(Bounds::new(
            point(
                pane.origin.x + pane.cell.width * view.frame.cursor.1 as f32,
                pane.origin.y + pane.cell.height * view.frame.cursor.0.max(0) as f32,
            ),
            pane.cell,
        ))
    }
    fn character_index_for_point(
        &mut self,
        _: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        None
    }
}
impl Render for CanvasApp {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity();
        let painter = entity.clone();
        let views = self.state.views.clone();
        let selected = self.selected;
        let attached = self.state.error.is_none();
        let status = self.error().map(str::to_owned).unwrap_or_else(|| {
            if self.prefix {
                "Режим PigADE: 1–4 — фокус · Escape — выход".into()
            } else {
                format!(
                    "PigADE 0.0.1-dev · {} сессий · Ctrl+Shift+Space, 1–4 — фокус",
                    views.len()
                )
            }
        });
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(0x17191d))
            .text_color(rgb(0xf2f4f7))
            .font_family("Inter")
            .text_size(px(13.))
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::key))
            .child(
                div()
                    .h(px(40.))
                    .px(px(12.))
                    .flex()
                    .items_center()
                    .child("PigADE"),
            )
            .child(
                div().flex_1().min_h_0().child(
                    canvas(
                        move |bounds, window, cx| {
                            let font = font("Liberation Mono");
                            let fontid = window.text_system().resolve_font(&font);
                            let width = window
                                .text_system()
                                .advance(fontid, px(FONT_SIZE), 'M')
                                .map(|s| s.width)
                                .unwrap_or(px(9.));
                            let height = terminal_line_height(
                                window.text_system().ascent(fontid, px(FONT_SIZE)),
                                window.text_system().descent(fontid, px(FONT_SIZE)),
                            );
                            let cell = size(width, height);
                            let scale = window.scale_factor();
                            let panes =
                                entity.update(cx, |this, _| this.geometry(bounds, cell, scale));
                            (font, panes)
                        },
                        move |bounds, (font, panes), window, cx| {
                            let focus = painter.read(cx).focus.clone();
                            window.handle_input(
                                &focus,
                                ElementInputHandler::new(bounds, painter.clone()),
                                cx,
                            );
                            window.with_content_mask(Some(ContentMask { bounds }), |window| {
                                for (index, pane) in panes.into_iter().enumerate() {
                                    paint_panel(
                                        pane,
                                        views.get(index),
                                        (index == selected, attached),
                                        &font,
                                        window,
                                        cx,
                                    );
                                }
                            });
                        },
                    )
                    .size_full(),
                ),
            )
            .child(
                div()
                    .h(px(24.))
                    .px(px(12.))
                    .text_color(rgb(0xb0b8c5))
                    .child(status),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, window, cx| {
                    if let Some(index) = this
                        .panes
                        .iter()
                        .position(|pane| pane.bounds.contains(&event.position))
                    {
                        this.selected = index;
                    }
                    this.focus.focus(window);
                    cx.notify();
                }),
            )
    }
}
/// Font metric descent is negative on GPUI Linux and positive on some
/// other backends. ShapedLine::paint uses a positive extent below its baseline.
/// Keep the full ascent/descent extent plus padding before device rounding.
fn terminal_line_height(ascent: Pixels, descent: Pixels) -> Pixels {
    px(f32::from(ascent).max(0.) + f32::from(descent).abs() + 4.)
}

#[derive(Clone, Copy)]
enum CellPaint<'a> {
    Background(usize, &'a Cell),
    Glyph(usize, &'a Cell),
}
fn cell_paint_order(cells: &[Cell]) -> impl Iterator<Item = CellPaint<'_>> {
    cells
        .iter()
        .enumerate()
        .map(|(i, c)| CellPaint::Background(i, c))
        .chain(
            cells
                .iter()
                .enumerate()
                .filter(|(_, c)| c.flags & (64 | 256 | 1024) == 0 && c.text != " ")
                .map(|(i, c)| CellPaint::Glyph(i, c)),
        )
}

fn text(
    text: String,
    origin: Point<Pixels>,
    font: &Font,
    color: Hsla,
    line_height: Pixels,
    window: &mut Window,
    cx: &mut App,
) {
    let len = text.len();
    if len == 0 {
        return;
    }
    let line = window.text_system().shape_line(
        text.into(),
        px(FONT_SIZE),
        &[TextRun {
            len,
            font: font.clone(),
            color,
            background_color: None,
            underline: None,
            strikethrough: None,
        }],
        None,
    );
    let _ = line.paint(origin, line_height, window, cx);
}
fn paint_panel(
    pane: PaneGeometry,
    view: Option<&View>,
    (selected, attached): (bool, bool),
    font: &Font,
    window: &mut Window,
    cx: &mut App,
) {
    let bounds = pane.bounds;
    let cell = pane.cell;
    window.paint_quad(fill(
        bounds,
        rgb(if selected { 0x7dafff } else { 0x373e49 }),
    ));
    let inner = Bounds::new(
        bounds.origin + point(px(1.), px(1.)),
        bounds.size - size(px(2.), px(2.)),
    );
    window.paint_quad(fill(inner, rgb(0x111317)));
    let title = view
        .map(|v| {
            format!(
                "{} · {} · seq {} · {}×{}",
                v.session.chars().take(8).collect::<String>(),
                if attached {
                    v.state.as_str()
                } else {
                    "История · связь потеряна"
                },
                v.sequence,
                v.frame.geometry.cols,
                v.frame.geometry.rows
            )
        })
        .unwrap_or_else(|| "Нет сессии — запустите через host CLI".into());
    text(
        title,
        bounds.origin + point(px(8.), px(5.)),
        font,
        rgb(0xb0b8c5).into(),
        px(HEADER - 8.),
        window,
        cx,
    );
    let Some(view) = view else {
        return;
    };
    let frame = &view.frame;
    window.with_content_mask(
        Some(ContentMask {
            bounds: Bounds::new(
                bounds.origin + point(px(1.), px(HEADER)),
                size(
                    bounds.size.width - px(2.),
                    bounds.size.height - px(HEADER + 1.),
                ),
            ),
        }),
        |window| {
            // Backgrounds must be complete before any cluster is painted. A
            // following spacer/row must never erase wide glyphs or descenders.
            for operation in cell_paint_order(&frame.cells) {
                let (i, c) = match operation {
                    CellPaint::Background(i, c) | CellPaint::Glyph(i, c) => (i, c),
                };
                let x = i % frame.geometry.cols as usize;
                let y = i / frame.geometry.cols as usize;
                let origin = pane.origin + point(cell.width * x as f32, cell.height * y as f32);
                let mut fg = color(&c.fg, true);
                let mut bg = color(&c.bg, false);
                if c.flags & 1 != 0 {
                    std::mem::swap(&mut fg, &mut bg);
                }
                match operation {
                    CellPaint::Background(..) => {
                        window.paint_quad(fill(Bounds::new(origin, cell), rgb(bg)))
                    }
                    CellPaint::Glyph(..) => {
                        let mut glyphfont = font.clone();
                        if c.flags & 2 != 0 {
                            glyphfont.weight = FontWeight::BOLD;
                        }
                        if c.flags & 4 != 0 {
                            glyphfont.style = FontStyle::Italic;
                        }
                        // Shape the entire host cell cluster, including combining
                        // marks. Wide cells may occupy their continuation span;
                        // only the pane viewport clips it, never an individual cell.
                        text(
                            c.text.clone(),
                            origin,
                            &glyphfont,
                            rgb(fg).into(),
                            cell.height,
                            window,
                            cx,
                        );
                    }
                }
            }
            if frame.modes & input::SHOW_CURSOR != 0 && frame.cursor.0 >= 0 {
                let origin = pane.origin
                    + point(
                        cell.width * frame.cursor.1 as f32,
                        cell.height * frame.cursor.0 as f32,
                    );
                window.paint_quad(fill(
                    Bounds::new(
                        origin + point(px(0.), cell.height - px(2.)),
                        size(cell.width, px(2.)),
                    ),
                    rgb(if selected { 0x7dafff } else { 0x606875 }),
                ));
            }
        },
    );
}
fn color(value: &str, foreground: bool) -> u32 {
    const ANSI: [u32; 16] = [
        0x111317, 0xe06c75, 0x98c379, 0xe5c07b, 0x61afef, 0xc678dd, 0x56b6c2, 0xdcdfe4, 0x5c6370,
        0xff808b, 0xb4e88c, 0xffd68a, 0x88c0ff, 0xdf9fff, 0x7bdde4, 0xffffff,
    ];
    if value.starts_with("Spec(") {
        let numbers: Vec<u32> = value
            .split(|c: char| !c.is_ascii_digit())
            .filter_map(|n| n.parse().ok())
            .collect();
        if numbers.len() == 3 {
            return (numbers[0] << 16) | (numbers[1] << 8) | numbers[2];
        }
    }
    if let Some(index) = value
        .strip_prefix("Indexed(")
        .and_then(|s| s.strip_suffix(')'))
        .and_then(|s| s.parse::<u32>().ok())
    {
        if index < 16 {
            return ANSI[index as usize];
        }
        if index < 232 {
            let i = index - 16;
            let channel = |v: u32| if v == 0 { 0 } else { 55 + v * 40 };
            return channel(i / 36) << 16 | channel(i / 6 % 6) << 8 | channel(i % 6);
        }
        let v = 8 + (index.min(255) - 232) * 10;
        return v << 16 | v << 8 | v;
    }
    let names = [
        "Black",
        "Red",
        "Green",
        "Yellow",
        "Blue",
        "Magenta",
        "Cyan",
        "White",
        "BrightBlack",
        "BrightRed",
        "BrightGreen",
        "BrightYellow",
        "BrightBlue",
        "BrightMagenta",
        "BrightCyan",
        "BrightWhite",
    ];
    if let Some(index) = names.iter().position(|n| value == format!("Named({n})")) {
        return ANSI[index];
    }
    if foreground { 0xf2f4f7 } else { 0x111317 }
}
fn main() {
    if std::env::args().nth(1).as_deref() == Some("--version") {
        println!(
            "PigADE native canvas {} (stage-0)",
            env!("CARGO_PKG_VERSION")
        );
        return;
    }
    let socket = std::env::args()
        .nth(1)
        .expect("usage: pigade-app /path/to/host.sock");
    let (tx, shared) = ipc(socket.into());
    Application::new().run(move |cx: &mut App| {
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        let bounds = Bounds::centered(None, size(px(1280.), px(800.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                app_id: Some("org.pigade.app".into()),
                ..Default::default()
            },
            move |window, cx| {
                let view = cx.new(|cx| CanvasApp::new(cx, tx, shared));
                view.read(cx).focus.focus(window);
                view
            },
        )
        .unwrap();
        cx.activate(true);
    });
}
#[cfg(test)]
mod tests {
    use super::protocol::Cell;
    use super::{
        CellPaint, PaneGeometry, cell_paint_order, color, project_panels, terminal_line_height,
    };
    use gpui::{Bounds, point, px, size};
    #[test]
    fn signed_font_descent_keeps_complete_extent_and_device_line_height() {
        let negative = terminal_line_height(px(12.), px(-4.));
        let positive = terminal_line_height(px(12.), px(4.));
        assert_eq!(negative, px(20.));
        assert_eq!(negative, positive);
        let panel = Bounds::new(point(px(0.), px(0.)), size(px(1280.), px(800.)));
        for scale in [1., 1.25, 1.5, 2.] {
            let pane = PaneGeometry::new(panel, size(px(8.4), negative), scale);
            // GPUI places the baseline at half the extra line space + ascent.
            let baseline = (f32::from(pane.cell.height) - 12. - 4.) / 2. + 12.;
            assert!(baseline - 12. >= 0.);
            assert!(baseline + 4. <= f32::from(pane.cell.height));
        }
    }
    #[test]
    fn backgrounds_cannot_erase_wide_spans_or_next_row_descenders() {
        let cell = |text: &str, flags| Cell {
            text: text.into(),
            fg: "Named(Foreground)".into(),
            bg: "Named(Background)".into(),
            flags,
            underline: None,
            hyperlink: None,
        };
        // Two columns, three rows. A CJK cluster spans the first row;
        // the second-row g extends into the next row's background area.
        let cells = [
            cell("界", 32),
            cell(" ", 64),
            cell("g", 0),
            cell("e\u{301}", 0),
            cell(" ", 0),
            cell(" ", 0),
        ];
        let mut coverage = [false; 6];
        let mut glyph_started = false;
        let mut clusters = Vec::new();
        for operation in cell_paint_order(&cells) {
            match operation {
                CellPaint::Background(index, _) => {
                    assert!(!glyph_started);
                    coverage[index] = false;
                }
                CellPaint::Glyph(index, cell) => {
                    glyph_started = true;
                    clusters.push(cell.text.as_str());
                    coverage[index] = true;
                    if index == 0 {
                        coverage[1] = true;
                    } // wide continuation
                    if index == 2 {
                        coverage[4] = true;
                    } // descender coverage
                }
            }
        }
        assert!(coverage[1] && coverage[4]);
        assert_eq!(clusters, ["界", "g", "e\u{301}"]);
    }
    #[test]
    fn pane_projection_uses_available_area_without_fake_panels() {
        let bounds = Bounds::new(point(px(0.), px(0.)), size(px(1280.), px(800.)));
        for count in 1..=4 {
            let panels = project_panels(bounds, count, 1.);
            assert_eq!(panels.len(), count);
            assert_eq!(
                panels[0].size.height,
                if count <= 2 { px(800.) } else { px(397.) }
            );
        }
        assert_eq!(project_panels(bounds, 1, 1.)[0], bounds);
        assert_eq!(project_panels(bounds, 3, 1.)[2].size.width, px(1280.));
    }
    #[test]
    fn font_metrics_and_fractional_scale_share_device_grid() {
        for scale in [1., 1.25, 1.5, 2.] {
            let bounds = Bounds::new(point(px(-679.3), px(342.2)), size(px(1280.7), px(800.5)));
            for count in 1..=4 {
                for panel in project_panels(bounds, count, scale) {
                    let pane = PaneGeometry::new(panel, size(px(8.4), px(18.2)), scale);
                    let integral = |logical: gpui::Pixels| {
                        let device = f32::from(logical) * scale;
                        (device - device.round()).abs() < 0.001
                    };
                    assert!(
                        integral(pane.cell.width)
                            && integral(pane.cell.height)
                            && integral(pane.origin.x)
                            && integral(pane.origin.y)
                    );
                    assert!(
                        pane.origin.x + pane.cell.width * pane.cols as f32
                            <= panel.right() - px(8.) + px(0.001)
                    );
                    assert!(
                        pane.origin.y + pane.cell.height * pane.rows as f32
                            <= panel.bottom() - px(8.) + px(0.001)
                    );
                    assert!(pane.rows > 0 && pane.cols > 0);
                }
            }
        }
    }
    #[test]
    fn tiny_canvas_preserves_nonzero_pty_size() {
        let bounds = Bounds::new(point(px(0.), px(0.)), size(px(1.), px(1.)));
        let pane = PaneGeometry::new(bounds, size(px(8.4), px(18.2)), 1.25);
        assert_eq!((pane.rows, pane.cols), (1, 2));
    }
    #[test]
    fn ansi_color_fields_are_display_only() {
        assert_eq!(color("Spec(Rgb { r: 1, g: 2, b: 3 })", true), 0x010203);
        assert_eq!(color("Indexed(196)", true), 0xff0000);
        assert_eq!(color("Named(BrightBlue)", true), 0x88c0ff);
    }
}

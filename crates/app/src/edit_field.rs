//! Shaped, selectable text-field contents for product dialogs.
//!
//! The outer dialog owns field sizing and decoration. This module owns only
//! text shaping, caret/selection geometry, and the native input registration.

use std::ops::Range;
use std::sync::Arc;

use gpui::{
    AnyElement, Bounds, ContentMask, Context, ElementInputHandler, Font, Hsla, IntoElement, Pixels,
    Point, SharedString, Styled, TextAlign, TextRun, WrappedLine, canvas, fill, point, px, size,
};
use uuid::Uuid;

use crate::text_edit::EditBuffer;

use super::CanvasApp;

#[derive(Clone)]
struct ShapedLine {
    start: usize,
    line: Arc<WrappedLine>,
}

/// Cached shaped contents and the current field's absolute geometry.
#[derive(Clone)]
pub(crate) struct FieldLayout {
    pub(crate) dialog_id: Uuid,
    pub(crate) cursor: usize,
    active: bool,
    pub(crate) revision: u64,
    pub(crate) bounds: Bounds<Pixels>,
    source: SharedString,
    lines: Arc<Vec<ShapedLine>>,
    line_height: Pixels,
    scroll_y: Pixels,
    scroll_x: Pixels,
    text_len: usize,
    wrap_width: Pixels,
    multiline: bool,
    color: Hsla,
    font_size: Pixels,
    font: Font,
}

impl FieldLayout {
    pub(crate) fn index_for_point(&self, position: Point<Pixels>) -> usize {
        let local = position - self.bounds.origin + point(self.scroll_x, self.scroll_y);
        let mut y = Pixels::ZERO;
        for (line_index, shaped) in self.lines.iter().enumerate() {
            let height = shaped.line.size(self.line_height).height;
            if local.y < y + height || line_index + 1 == self.lines.len() {
                let index = shaped
                    .line
                    .closest_index_for_position(point(local.x, local.y - y), self.line_height)
                    .unwrap_or_else(|index| index);
                return (shaped.start + index).min(self.text_len);
            }
            y += height;
        }
        self.text_len
    }

    pub(crate) fn bounds_for_range(&self, range: Range<usize>) -> Option<Bounds<Pixels>> {
        if range.start == range.end {
            return Some(self.caret_bounds(range.start));
        }
        self.range_fragments(range).into_iter().reduce(union_bounds)
    }

    pub(crate) fn max_scroll(&self) -> Pixels {
        let content_height = self
            .lines
            .iter()
            .map(|line| line.line.size(self.line_height).height)
            .sum::<Pixels>();
        (content_height - self.bounds.size.height).max(Pixels::ZERO)
    }

    fn caret_point(&self, index: usize) -> Point<Pixels> {
        let index = index.min(self.text_len);
        let mut y = Pixels::ZERO;
        for shaped in self.lines.iter() {
            let end = shaped.start + shaped.line.len();
            if index <= end || shaped.start == self.text_len {
                let local = index.saturating_sub(shaped.start).min(shaped.line.len());
                let position = shaped
                    .line
                    .position_for_index(local, self.line_height)
                    .unwrap_or(point(px(0.), px(0.)));
                return point(position.x, y + position.y);
            }
            y += shaped.line.size(self.line_height).height;
        }
        point(px(0.), y)
    }

    fn caret_bounds(&self, index: usize) -> Bounds<Pixels> {
        let caret = self.caret_point(index);
        Bounds::new(
            point(
                self.bounds.origin.x + caret.x - self.scroll_x,
                self.bounds.origin.y + caret.y - self.scroll_y,
            ),
            size(px(1.), self.line_height),
        )
        .intersect(&self.bounds)
    }

    fn max_scroll_x(&self) -> Pixels {
        if self.multiline {
            return Pixels::ZERO;
        }
        let content_width = self
            .lines
            .iter()
            .map(|line| line.line.width())
            .max()
            .unwrap_or(Pixels::ZERO);
        (content_width - self.bounds.size.width).max(Pixels::ZERO)
    }

    fn range_fragments(&self, range: Range<usize>) -> Vec<Bounds<Pixels>> {
        let start = range.start.min(range.end).min(self.text_len);
        let end = range.start.max(range.end).min(self.text_len);
        if start == end {
            return Vec::new();
        }
        let mut fragments = Vec::new();
        let mut y = Pixels::ZERO;
        for shaped in self.lines.iter() {
            let line_start = shaped.start;
            let line_end = line_start + shaped.line.len();
            let selected_start = start.max(line_start);
            let selected_end = end.min(line_end);
            if selected_start < selected_end {
                let from = shaped
                    .line
                    .position_for_index(selected_start - line_start, self.line_height)
                    .unwrap_or(point(px(0.), px(0.)));
                let to = shaped
                    .line
                    .position_for_index(selected_end - line_start, self.line_height)
                    .unwrap_or(point(shaped.line.width(), px(0.)));
                let first_row = (f32::from(from.y) / f32::from(self.line_height)).floor() as usize;
                let last_row = (f32::from(to.y) / f32::from(self.line_height)).floor() as usize;
                for row in first_row..=last_row {
                    let row_y = self.line_height * row as f32;
                    let left = if row == first_row { from.x } else { px(0.) };
                    let right = if row == last_row {
                        to.x.max(left + px(1.))
                    } else {
                        shaped.line.width()
                    };
                    let top = self.bounds.origin.y + y + row_y - self.scroll_y;
                    let rect = Bounds::new(
                        point(self.bounds.origin.x + left - self.scroll_x, top),
                        size((right - left).max(px(1.)), self.line_height),
                    )
                    .intersect(&self.bounds);
                    if !rect.is_empty() {
                        fragments.push(rect);
                    }
                }
            }
            y += shaped.line.size(self.line_height).height;
        }
        fragments
    }
}

fn union_bounds(left: Bounds<Pixels>, right: Bounds<Pixels>) -> Bounds<Pixels> {
    let x = left.left().min(right.left());
    let y = left.top().min(right.top());
    let right_edge = left.right().max(right.right());
    let bottom = left.bottom().max(right.bottom());
    Bounds::new(point(x, y), size(right_edge - x, bottom - y))
}

struct Prepared {
    layout: FieldLayout,
    scroll: Pixels,
    scroll_x: Pixels,
    selection: Range<usize>,
    marked: Option<Range<usize>>,
    active: bool,
    placeholder: bool,
    accent: Hsla,
}

pub(crate) struct FieldOptions<'a> {
    pub cache: Option<&'a FieldLayout>,
    pub dialog_id: Uuid,
    pub index: usize,
    pub active: bool,
    pub multiline: bool,
    pub placeholder: &'a str,
    pub accent: Hsla,
    pub muted: Hsla,
}

pub(crate) fn content(
    buffer: &EditBuffer,
    options: FieldOptions<'_>,
    cx: &mut Context<CanvasApp>,
) -> AnyElement {
    let FieldOptions {
        cache,
        dialog_id,
        index,
        active,
        multiline,
        placeholder,
        accent,
        muted,
    } = options;
    let entity = cx.entity();
    // The parent entity is already leased during Render. Read the cache
    // passed by that render instead of re-reading our own entity through App.
    let cached_source = cache
        .filter(|layout| layout.dialog_id == dialog_id && layout.revision == buffer.revision())
        .map(|layout| layout.source.clone());
    let text = if buffer.text().is_empty() && !placeholder.is_empty() {
        SharedString::new(placeholder)
    } else {
        cached_source.unwrap_or_else(|| SharedString::new(buffer.text()))
    };
    let actual_len = buffer.text().len();
    let cursor = buffer.cursor();
    let selection = buffer.selection();
    let marked = buffer.marked();
    let revision = buffer.revision();
    let placeholder_active = actual_len == 0 && !placeholder.is_empty();
    let weak_prepaint = entity.downgrade();
    let weak_paint = entity.downgrade();
    canvas(
        move |bounds, window, app| {
            let font_style = window.text_style();
            let font = font_style.font();
            let font_size = font_style.font_size.to_pixels(window.rem_size());
            let line_height = font_style.line_height_in_pixels(window.rem_size());
            let color = if placeholder_active {
                muted
            } else {
                font_style.color
            };
            let width = bounds.size.width;
            let previous = weak_prepaint
                .update(app, |view, _| view.editor_layouts.get(&index).cloned())
                .ok()
                .flatten();
            let cache_hit = previous.as_ref().is_some_and(|layout| {
                layout.dialog_id == dialog_id
                    && layout.revision == revision
                    && layout.wrap_width == width
                    && layout.multiline == multiline
                    && layout.color == color
                    && layout.font_size == font_size
                    && layout.line_height == line_height
                    && layout.font == font
                    && layout.source == text
                    && !placeholder_active
            });
            let mut layout = if cache_hit {
                previous.as_ref().expect("cache hit has a layout").clone()
            } else {
                let runs = [TextRun {
                    len: text.len(),
                    font: font.clone(),
                    color,
                    ..Default::default()
                }];
                let shaped = window
                    .text_system()
                    .shape_text(
                        text.clone(),
                        font_size,
                        &runs,
                        multiline.then_some(width),
                        None,
                    )
                    .unwrap_or_default();
                let mut start = 0;
                let mut lines = Vec::with_capacity(shaped.len().max(1));
                let line_count = shaped.len();
                for (line_index, line) in shaped.into_iter().enumerate() {
                    let len = line.len();
                    lines.push(ShapedLine {
                        start,
                        line: Arc::new(line),
                    });
                    start += len + usize::from(line_index + 1 < line_count);
                }
                FieldLayout {
                    dialog_id,
                    cursor,
                    active,
                    revision,
                    bounds,
                    source: text.clone(),
                    lines: Arc::new(lines),
                    line_height,
                    scroll_y: Pixels::ZERO,
                    scroll_x: Pixels::ZERO,
                    text_len: actual_len,
                    wrap_width: width,
                    multiline,
                    color,
                    font_size,
                    font: font.clone(),
                }
            };
            layout.bounds = bounds;
            layout.cursor = cursor;
            layout.revision = revision;
            let mut scroll = weak_prepaint
                .update(app, |view, _| {
                    view.editor_scroll
                        .get(&index)
                        .copied()
                        .unwrap_or(Pixels::ZERO)
                })
                .unwrap_or(Pixels::ZERO)
                .max(px(0.))
                .min(layout.max_scroll());
            let mut scroll_x = if multiline {
                Pixels::ZERO
            } else {
                previous
                    .as_ref()
                    .map_or(Pixels::ZERO, |old| old.scroll_x)
                    .max(Pixels::ZERO)
                    .min(layout.max_scroll_x())
            };
            let caret = layout.caret_point(cursor);
            let cursor_changed = previous.as_ref().is_none_or(|old| {
                old.dialog_id != dialog_id
                    || old.cursor != cursor
                    || old.revision != revision
                    || !old.active
            });
            if active && cursor_changed {
                let top = caret.y - scroll;
                if top < px(0.) {
                    scroll = caret.y;
                } else if top + line_height > bounds.size.height {
                    scroll = caret.y + line_height - bounds.size.height;
                }
                scroll = scroll.max(px(0.)).min(layout.max_scroll());
                if !multiline {
                    if caret.x < scroll_x {
                        scroll_x = caret.x;
                    } else if caret.x + px(1.) > scroll_x + bounds.size.width {
                        scroll_x = caret.x + px(1.) - bounds.size.width;
                    }
                    scroll_x = scroll_x.max(px(0.)).min(layout.max_scroll_x());
                }
            }
            layout.active = active;
            layout.scroll_y = scroll;
            layout.scroll_x = scroll_x;
            weak_prepaint
                .update(app, |view, _| {
                    view.editor_scroll.insert(index, scroll);
                    view.editor_layouts.insert(index, layout.clone());
                })
                .ok();
            Prepared {
                layout,
                scroll,
                scroll_x,
                selection,
                marked,
                active,
                placeholder: placeholder_active,
                accent,
            }
        },
        move |bounds, prepared, window, app| {
            let layout = &prepared.layout;
            let focused = weak_paint
                .upgrade()
                .is_some_and(|entity| entity.read(app).focus.is_focused(window))
                && window.is_window_active();
            window.with_content_mask(Some(ContentMask { bounds }), |window| {
                if prepared.active && !prepared.placeholder {
                    for rect in layout.range_fragments(prepared.selection.clone()) {
                        window.paint_quad(fill(rect, prepared.accent.opacity(0.28)));
                    }
                }
                let mut y = bounds.origin.y - prepared.scroll;
                for shaped in layout.lines.iter() {
                    shaped
                        .line
                        .paint(
                            point(bounds.origin.x - prepared.scroll_x, y),
                            layout.line_height,
                            TextAlign::Left,
                            Some(bounds),
                            window,
                            app,
                        )
                        .ok();
                    y += shaped.line.size(layout.line_height).height;
                }
                if let Some(marked) = prepared.marked.as_ref() {
                    for rect in layout.range_fragments(marked.clone()) {
                        window.paint_underline(
                            point(rect.origin.x, rect.bottom() - px(2.)),
                            rect.size.width,
                            &gpui::UnderlineStyle {
                                thickness: px(1.),
                                color: Some(prepared.accent),
                                wavy: false,
                            },
                        );
                    }
                }
                if prepared.active && focused {
                    let caret_bounds = layout.caret_bounds(layout.cursor);
                    if !caret_bounds.is_empty() {
                        window.paint_quad(fill(caret_bounds, prepared.accent));
                    }
                }
            });
            let entity = weak_paint.clone();
            if prepared.active {
                let focus = entity.update(app, |view, _| view.focus.clone()).ok();
                if let Some(focus) = focus
                    && let Some(entity) = entity.upgrade()
                {
                    window.handle_input(&focus, ElementInputHandler::new(bounds, entity), app);
                }
            }
        },
    )
    .size_full()
    .into_any_element()
}

//! Borrowed retirement notifications must not change the bounded hot grid.
use super::*;
use crate::term::cell::Cell;
use std::sync::{Arc, Mutex};

#[derive(Debug, Default)]
struct Rows<T> {
    rows: Mutex<Vec<(HistoryRetirement, usize, Vec<T>)>>,
    boundaries: Mutex<Vec<HistoryRetirement>>,
}
impl<T: Clone + std::fmt::Debug + Send + Sync> HistoryObserver<T> for Rows<T> {
    fn row(&self, row: &Row<T>, columns: usize, reason: HistoryRetirement) {
        self.rows.lock().unwrap().push((
            reason,
            columns,
            (0..row.len())
                .map(|column| row[Column(column)].clone())
                .collect(),
        ));
    }
    fn boundary(&self, reason: HistoryRetirement) {
        self.boundaries.lock().unwrap().push(reason);
    }
}

#[test]
fn notifications_retire_only_discarded_rows_in_chronological_order() {
    let sink = Arc::new(Rows::<usize>::default());
    let mut grid = Grid::<usize>::new(3, 2, 2);
    grid.set_history_observer(Some(sink.clone()));
    for line in 0..3 {
        grid[Line(line)][Column(0)] = 10 + line as usize;
    }
    grid.scroll_up::<usize>(&(Line(0)..Line(3)), 1);
    grid[Line(2)][Column(0)] = 20;
    grid.scroll_up::<usize>(&(Line(0)..Line(3)), 1);
    assert!(sink.rows.lock().unwrap().is_empty());
    grid[Line(2)][Column(0)] = 21;
    grid.scroll_up::<usize>(&(Line(0)..Line(3)), 1);
    assert_eq!(sink.rows.lock().unwrap()[0].2[0], 10);
    grid.scroll_up::<usize>(&(Line(1)..Line(3)), 1);
    assert_eq!(
        sink.rows.lock().unwrap().len(),
        1,
        "partial scroll is not normal history"
    );
    grid.update_history(0);
    let recorded: Vec<_> = sink
        .rows
        .lock()
        .unwrap()
        .iter()
        .map(|entry| (entry.0, entry.2[0]))
        .collect();
    assert_eq!(
        recorded,
        [
            (HistoryRetirement::Scroll, 10),
            (HistoryRetirement::HistoryLimit, 11),
            (HistoryRetirement::HistoryLimit, 12)
        ]
    );
}

#[test]
fn shrink_overflow_plus_hot_rows_matches_large_history_reference_once() {
    fn prepare(max_history: usize) -> Grid<Cell> {
        let mut grid = Grid::<Cell>::new(2, 4, max_history);
        for column in 0..4 {
            grid[Line(0)][Column(column)].c = 'A';
            grid[Line(1)][Column(column)].c = 'B';
        }
        grid[Line(0)][Column(3)].flags.insert(Flags::WRAPLINE);
        grid[Line(1)][Column(3)].flags.insert(Flags::WRAPLINE);
        grid.scroll_up::<_>(&(Line(0)..Line(2)), 1);
        for column in 0..4 {
            grid[Line(1)][Column(column)].c = 'C';
        }
        grid
    }
    fn text(grid: &Grid<Cell>) -> Vec<String> {
        (-(grid.history_size() as i32)..grid.lines as i32)
            .map(|line| {
                (0..grid.columns)
                    .map(|column| grid[Line(line)][Column(column)].c)
                    .collect()
            })
            .collect()
    }
    let mut bounded = prepare(1);
    let mut reference = prepare(100);
    let sink = Arc::new(Rows::<Cell>::default());
    bounded.set_history_observer(Some(sink.clone()));
    bounded.resize::<_>(true, 2, 2);
    reference.resize::<_>(true, 2, 2);
    let mut combined: Vec<String> = sink
        .rows
        .lock()
        .unwrap()
        .iter()
        .map(|entry| {
            assert_eq!(entry.0, HistoryRetirement::WidthReflow);
            assert_eq!(entry.1, 2);
            entry.2.iter().map(|cell| cell.c).collect()
        })
        .collect();
    assert!(!combined.is_empty());
    combined.extend(text(&bounded));
    assert_eq!(combined, text(&reference));
}

#[test]
fn reset_records_one_epoch_and_does_not_double_archive_history() {
    let sink = Arc::new(Rows::<usize>::default());
    let mut grid = Grid::<usize>::new(2, 2, 2);
    grid.set_history_observer(Some(sink.clone()));
    grid[Line(0)][Column(0)] = 1;
    grid[Line(1)][Column(0)] = 2;
    grid.scroll_up::<usize>(&(Line(0)..Line(2)), 1);
    grid[Line(1)][Column(0)] = 3;
    grid.reset::<usize>();
    assert_eq!(
        sink.rows
            .lock()
            .unwrap()
            .iter()
            .map(|entry| entry.2[0])
            .collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
    assert_eq!(
        *sink.boundaries.lock().unwrap(),
        vec![HistoryRetirement::Reset]
    );
    assert_eq!(grid.history_size(), 0);
    assert_eq!(grid[Line(0)][Column(0)], 0);
    // Runtime observer survives reset; the next real clear has its own boundary.
    grid.scroll_up::<usize>(&(Line(0)..Line(2)), 1);
    grid.clear_history();
    assert_eq!(
        *sink.boundaries.lock().unwrap(),
        vec![HistoryRetirement::Reset, HistoryRetirement::Clear]
    );
}

#[test]
fn height_growth_moves_history_into_view_without_archiving_new_blank_rows() {
    let mut grid = Grid::<usize>::new(3, 2, 2);
    for line in 0..3 {
        grid[Line(line)][Column(0)] = line as usize + 1;
    }
    grid.scroll_up::<usize>(&(Line(0)..Line(3)), 1);
    grid[Line(2)][Column(0)] = 4;
    grid.scroll_up::<usize>(&(Line(0)..Line(3)), 1);
    grid[Line(2)][Column(0)] = 5;
    assert_eq!(grid.history_size(), 2);
    let sink = Arc::new(Rows::<usize>::default());
    grid.set_history_observer(Some(sink.clone()));
    grid.resize::<usize>(true, 5, 2);
    assert_eq!(grid.history_size(), 0);
    assert_eq!(
        (0..5)
            .map(|line| grid[Line(line)][Column(0)])
            .collect::<Vec<_>>(),
        vec![1, 2, 3, 4, 5],
    );
    assert!(sink.rows.lock().unwrap().is_empty());
}

#[test]
fn width_growth_cursor_reflow_removes_only_bottom_bookkeeping() {
    let mut grid = Grid::<Cell>::new(3, 2, 2);
    for (line, character) in ['A', 'B', 'C'].into_iter().enumerate() {
        for column in 0..2 {
            grid[Line(line as i32)][Column(column)].c = character;
        }
    }
    for line in [1, 2] {
        grid[Line(line)][Column(1)].flags.insert(Flags::WRAPLINE);
    }
    grid.cursor.point.line = Line(1);
    grid.scroll_up::<_>(&(Line(0)..Line(3)), 1);
    grid[Line(0)][Column(0)].c = 'H';
    let sink = Arc::new(Rows::<Cell>::default());
    grid.set_history_observer(Some(sink.clone()));
    // This reaches grow_columns' cursor-line overflow/truncate path. It drops
    // an empty newest row, not old history; a generic Storage observer is wrong.
    grid.resize::<_>(true, 3, 3);
    assert_eq!(grid.history_size(), 0);
    let text: Vec<String> = (0..3)
        .map(|line| {
            (0..3)
                .map(|column| grid[Line(line)][Column(column)].c)
                .collect()
        })
        .collect();
    assert_eq!(text, ["AA ", "HBC", "C  "]);
    assert!(sink.rows.lock().unwrap().is_empty());
}

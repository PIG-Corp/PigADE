//! Read-only UI state and paging helpers for the host-owned history snapshot.

use crate::protocol::{HistoryCursor, HistoryPage, HistoryTextRow};
use uuid::Uuid;

pub(super) const PAGE_BACK_LIMIT: usize = 128;
pub(super) const SEARCH_TEXT_LIMIT: usize = 512;

pub(super) fn display_error(message: &str) -> String {
    if message.contains("snapshot expired or closed") {
        "Снимок истории устарел или закрыт. Нажмите «Обновить».".into()
    } else if message.contains("bounded replay tail") {
        "Нет подходящего полного снимка для чтения истории. Исходные данные сохранены; повторите обновление после записи checkpoint.".into()
    } else if message.contains("queue full") {
        "Очередь чтения истории занята. Повторите запрос.".into()
    } else {
        format!("Не удалось прочитать историю: {message}")
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Pending {
    Open,
    Page {
        start: HistoryCursor,
    },
    Search {
        text: String,
        cursor: Option<HistoryCursor>,
    },
    Close {
        refresh: bool,
    },
}

#[derive(Clone, Debug)]
pub(super) struct HistorySheet {
    pub(super) dialog_id: Uuid,
    pub(super) session: Uuid,
    pub(super) generation: Uuid,
    pub(super) snapshot: Option<Uuid>,
    pub(super) sequence: Option<u64>,
    pub(super) query_text: String,
    pub(super) page: Option<HistoryPage>,
    pub(super) back: Vec<HistoryCursor>,
    pub(super) search_next: Option<HistoryCursor>,
    pub(super) match_at: Option<HistoryCursor>,
    pub(super) status: String,
    pub(super) error: Option<String>,
    pub(super) pending: Option<Pending>,
    pub(super) closed: bool,
}

impl HistorySheet {
    pub(super) fn new(dialog_id: Uuid, session: Uuid, generation: Uuid) -> Self {
        Self {
            dialog_id,
            session,
            generation,
            snapshot: None,
            sequence: None,
            query_text: String::new(),
            page: None,
            back: Vec::new(),
            search_next: None,
            match_at: None,
            status: "Открытие снимка истории…".into(),
            error: None,
            pending: Some(Pending::Open),
            closed: false,
        }
    }

    pub(super) fn query_changed(&mut self) {
        self.search_next = None;
        self.match_at = None;
        self.status = "Запрос изменён; поиск начнётся заново".into();
        self.error = None;
    }

    pub(super) fn remember_page(&mut self, cursor: HistoryCursor) {
        if self.back.last() == Some(&cursor) {
            return;
        }
        self.back.push(cursor);
        if self.back.len() > PAGE_BACK_LIMIT {
            self.back.remove(0);
        }
    }
}

pub(super) fn page_plain_text(page: &HistoryPage) -> String {
    let mut output = String::new();
    for row in &page.rows {
        if row.boundary.is_some() {
            if !output.is_empty() && !output.ends_with('\n') {
                output.push('\n');
            }
            continue;
        }
        output.push_str(&row.chunk.text);
        if row.chunk.next_offset.is_none() && !row.chunk.wrap {
            output.push('\n');
        }
    }
    output
}

pub(super) fn boundary_messages(rows: &[HistoryTextRow]) -> Vec<String> {
    rows.iter()
        .filter_map(|row| row.boundary.as_ref())
        .map(|reason| format!("Граница истории: {reason}"))
        .collect()
}

pub(super) fn validate_page(page: &HistoryPage) -> Result<(), String> {
    if page.rows.len() > 128 {
        return Err("Недопустимый размер страницы истории".into());
    }
    let mut cursor = page.start;
    let mut page_bytes = 0usize;
    if cursor.row > page.total_rows {
        return Err("Некорректная начальная позиция истории".into());
    }
    for row in &page.rows {
        if row.row != cursor.row || row.chunk.offset != cursor.offset {
            return Err("Фрагменты истории не образуют непрерывную страницу".into());
        }
        if row.boundary.is_some() {
            if !row.chunk.text.is_empty()
                || row.chunk.offset != 0
                || row.chunk.total_bytes != 0
                || row.chunk.next_offset.is_some()
            {
                return Err("Некорректная граница истории".into());
            }
            cursor = HistoryCursor {
                row: cursor.row.checked_add(1).ok_or("history row overflow")?,
                offset: 0,
            };
            continue;
        }
        if row.chunk.text.len() > 64 * 1024
            || row.chunk.total_bytes > 16 * 1024 * 1024
            || row.chunk.offset > row.chunk.total_bytes
        {
            return Err("Недопустимый размер фрагмента истории".into());
        }
        page_bytes = page_bytes
            .checked_add(row.chunk.text.len())
            .ok_or("page byte count overflow")?;
        if page_bytes > 64 * 1024 {
            return Err("Страница истории превышает 64 КиБ".into());
        }
        let end = row
            .chunk
            .offset
            .checked_add(row.chunk.text.len() as u64)
            .ok_or("Некорректный фрагмент истории")?;
        if end > row.chunk.total_bytes
            || row
                .chunk
                .next_offset
                .is_some_and(|next| next != end || next > row.chunk.total_bytes)
        {
            return Err("Некорректный фрагмент истории".into());
        }
        if let Some(next_offset) = row.chunk.next_offset {
            cursor.offset = next_offset;
        } else if end == row.chunk.total_bytes {
            cursor = HistoryCursor {
                row: cursor.row.checked_add(1).ok_or("history row overflow")?,
                offset: 0,
            };
        } else {
            return Err("Некорректный конец фрагмента истории".into());
        }
    }
    if let Some(next) = page.next {
        if next != cursor || next.row >= page.total_rows || next.offset > 16 * 1024 * 1024 {
            return Err("Некорректный курсор следующей страницы".into());
        }
    } else if cursor.row < page.total_rows {
        return Err("Страница истории обрывается до конца снимка".into());
    }
    if page.previous.is_some_and(|previous| {
        previous.row >= page.total_rows || previous.offset > 16 * 1024 * 1024
    }) {
        return Err("Некорректный курсор предыдущей страницы".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{HistoryTextChunk, HistoryTextRow};

    fn page(rows: Vec<HistoryTextRow>) -> HistoryPage {
        HistoryPage {
            snapshot: Uuid::nil(),
            session: Uuid::nil(),
            generation: Uuid::nil(),
            sequence: 42,
            total_rows: rows.len() as u64,
            start: HistoryCursor { row: 0, offset: 0 },
            rows,
            next: None,
            previous: None,
            warnings: Vec::new(),
        }
    }

    fn row(row: u64, text: &str, wrap: bool, boundary: Option<&str>) -> HistoryTextRow {
        HistoryTextRow {
            row,
            chunk: HistoryTextChunk {
                text: text.into(),
                total_bytes: text.len() as u64,
                offset: 0,
                next_offset: None,
                wrap,
            },
            boundary: boundary.map(str::to_owned),
        }
    }

    #[test]
    fn page_copy_joins_soft_wraps_and_excludes_boundary_labels() {
        let page = page(vec![
            row(0, "soft", true, None),
            row(1, "wrap", false, None),
            row(2, "", false, Some("reset")),
            row(3, "hard", false, None),
        ]);
        assert_eq!(page_plain_text(&page), "softwrap\nhard\n");
        assert_eq!(
            boundary_messages(&page.rows),
            vec!["Граница истории: reset"]
        );
    }

    #[test]
    fn boundary_after_soft_wrap_keeps_a_hard_break() {
        let page = page(vec![
            row(0, "before", true, None),
            row(1, "", false, Some("reset")),
            row(2, "after", false, None),
        ]);
        assert_eq!(page_plain_text(&page), "before\nafter\n");
    }

    #[test]
    fn page_backstack_is_bounded_and_page_contract_is_validated() {
        let mut sheet = HistorySheet::new(Uuid::nil(), Uuid::nil(), Uuid::nil());
        for row in 0..(PAGE_BACK_LIMIT + 5) {
            sheet.remember_page(HistoryCursor {
                row: row as u64,
                offset: 0,
            });
        }
        assert_eq!(sheet.back.len(), PAGE_BACK_LIMIT);
        let page = page(vec![row(0, "界x", false, None)]);
        assert!(validate_page(&page).is_ok());
        let mut invalid = page;
        invalid.rows[0].chunk.next_offset = Some(1);
        assert!(validate_page(&invalid).is_err());
    }
}

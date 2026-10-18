//! Explicit transfer of a frozen visible terminal excerpt into a fresh launch.
//! No input is sent to either terminal and no provider history is inspected.
use crate::{CanvasApp, handoff, history_view, product, protocol, selection, text_edit};
use gpui::{ClipboardItem, Context};
use product::{Dialog, DialogKind, ProductAction};
use serde_json::json;

#[derive(Clone)]
pub(super) struct Prepared {
    pub preview: protocol::LaunchPreview,
    pub prompt: handoff::Prompt,
    pub fields: Vec<String>,
    pub provider: String,
}

pub(super) struct Sheet {
    pub dialog: uuid::Uuid,
    pub source: handoff::Source,
    pub tab: uuid::Uuid,
    pub target: Option<uuid::Uuid>,
    pub provider: String,
    pub prepared: Option<Prepared>,
    pub error: Option<String>,
    pub pending: bool,
    pub origin: Option<Dialog>,
    pub source_truncated: bool,
    pub initial_text: String,
    cache: std::cell::RefCell<Option<PromptCache>>,
}

struct PromptCache {
    revisions: Vec<u64>,
    result: Result<handoff::Prompt, String>,
}

pub(super) struct Pending {
    request: uuid::Uuid,
    dialog: uuid::Uuid,
    prepared: Option<Prepared>,
    fields: Vec<String>,
    provider: String,
    prompt: handoff::Prompt,
}

fn fields(dialog: &Dialog) -> Vec<String> {
    dialog
        .fields
        .iter()
        .map(|field| field.text().to_owned())
        .collect()
}

fn same_fields(dialog: &Dialog, saved: &[String]) -> bool {
    dialog.fields.len() == saved.len()
        && dialog
            .fields
            .iter()
            .zip(saved)
            .all(|(field, saved)| field.text() == saved)
}

fn matches_preview(
    sheet: &Sheet,
    prompt: &handoff::Prompt,
    provider: &str,
    preview: &protocol::LaunchPreview,
) -> bool {
    preview.tab_id == sheet.tab
        && preview.target == sheet.target
        && preview.side == pigade_layout::Direction::Right
        && preview.provider == provider
        && matches!(provider, "claude" | "codex")
        && preview.session != sheet.source.session
        && !preview.session.is_nil()
        && !preview.generation.is_nil()
        && !preview.plan_id.is_nil()
        && preview.agent_instructions == prompt.text
        && preview.role_override.is_empty()
        && preview.resume.is_none()
        && preview.conversation.is_none()
        && preview.custom_command.is_none()
        && !preview.include_memory
        && preview.deliver_context
        && preview.engine.is_empty()
        && preview.capabilities.startup_context
        && preview.delivery_method != "none"
        && preview.context.validate().is_ok()
        && preview
            .context
            .blocks
            .iter()
            .filter(|block| block.source == "agent_instructions")
            .count()
            == 1
        && preview
            .context
            .blocks
            .iter()
            .any(|block| block.source == "agent_instructions" && block.text == prompt.text)
}

impl product::ProductUi {
    pub(super) fn handoff_prompt(&self) -> Result<handoff::Prompt, String> {
        let sheet = self
            .handoff_sheet
            .as_ref()
            .ok_or("Нет снимка для передачи")?;
        let dialog = self
            .dialog
            .as_ref()
            .filter(|dialog| dialog.kind == DialogKind::Handoff && dialog.id == sheet.dialog)
            .ok_or("Черновик передачи закрыт")?;
        let revisions = dialog
            .fields
            .iter()
            .map(|field| field.revision())
            .collect::<Vec<_>>();
        if let Some(cache) = sheet
            .cache
            .borrow()
            .as_ref()
            .filter(|cache| cache.revisions == revisions)
        {
            return cache.result.clone();
        }
        let result = handoff::prepare(
            &sheet.source,
            dialog
                .fields
                .first()
                .map_or("", text_edit::EditBuffer::text),
            dialog.fields.get(1).map_or("", text_edit::EditBuffer::text),
            handoff::BYTE_BUDGET,
            sheet.source_truncated,
        );
        *sheet.cache.borrow_mut() = Some(PromptCache {
            revisions,
            result: result.clone(),
        });
        result
    }

    pub(super) fn handoff_ready(&self) -> bool {
        if self.busy {
            return false;
        }
        let (Some(sheet), Some(dialog)) = (&self.handoff_sheet, &self.dialog) else {
            return false;
        };
        let Some(prepared) = &sheet.prepared else {
            return false;
        };
        !sheet.pending
            && dialog.kind == DialogKind::Handoff
            && dialog.id == sheet.dialog
            && sheet.provider == prepared.provider
            && same_fields(dialog, &prepared.fields)
            && self
                .catalog
                .as_ref()
                .and_then(|catalog| catalog.selection.tab_id)
                == Some(sheet.tab)
            && prepared.preview.can_launch
    }
}

impl CanvasApp {
    pub(super) fn open_handoff(&mut self, cx: &mut Context<Self>) {
        if self.product.busy || self.handoff_pending.is_some() || self.product_launch_unknown {
            self.product.notice =
                Some("Дождитесь подтверждения текущей операции перед передачей контекста".into());
            return;
        }
        let Some((tab, layout)) = self.tab_layout() else {
            self.product.notice = Some("Сначала выберите готовую вкладку".into());
            return;
        };
        let origin = self
            .product
            .dialog
            .as_ref()
            .filter(|dialog| dialog.kind == DialogKind::History)
            .cloned();
        let captured = if let Some(origin) = &origin {
            self.product.history_sheet.as_ref().and_then(|history| {
                let page = history.page.as_ref()?;
                if history.dialog_id != origin.id || history.pending.is_some() || history.closed {
                    return None;
                }
                Some((
                    handoff::Source {
                        session: page.session,
                        generation: page.generation,
                        sequence: page.sequence,
                        title: "Страница истории терминала".into(),
                        scope: format!(
                            "Снимок {} · строка {} · смещение {}",
                            page.snapshot,
                            page.start.row.saturating_add(1),
                            page.start.offset
                        ),
                    },
                    history_view::page_plain_text(page),
                    page.start.row > 0 || page.start.offset > 0 || page.next.is_some(),
                ))
            })
        } else {
            let historical = self.selected_history();
            let view = self
                .scrollback_display
                .as_ref()
                .filter(|_| historical)
                .or_else(|| self.state.views.get(self.selected));
            view.and_then(|view| {
                let session = view.session.parse().ok()?;
                let generation = view.generation.parse().ok()?;
                let selected = (if historical {
                    &self.scrollback_selection
                } else {
                    &self.selection
                })
                .as_ref()
                .filter(|selection| selection.is_current(view));
                let selection = selected.cloned().or_else(|| {
                    selection::CellSelection::new(
                        view,
                        selection::CellPosition { row: 0, column: 0 },
                        selection::CellPosition {
                            row: usize::from(view.frame.geometry.rows).checked_sub(1)?,
                            column: usize::from(view.frame.geometry.cols).checked_sub(1)?,
                        },
                    )
                    .ok()
                })?;
                let widths = historical
                    .then(|| self.scrollback.displayed_page())
                    .flatten()
                    .map(|page| {
                        page.rows
                            .iter()
                            .map(|row| row.source_columns)
                            .collect::<Vec<_>>()
                    });
                let text = widths
                    .as_deref()
                    .map_or_else(
                        || selection.text(view),
                        |widths| selection.text_with_row_widths(view, widths),
                    )
                    .ok()?;
                Some((
                    handoff::Source {
                        session,
                        generation,
                        sequence: view.sequence,
                        title: self
                            .product
                            .pane_title(view)
                            .chars()
                            .filter(|ch| !ch.is_control())
                            .take(160)
                            .collect(),
                        scope: if selected.is_some() {
                            "Выделенный текст"
                        } else if historical {
                            "Показанный экран истории"
                        } else {
                            "Показанный экран терминала"
                        }
                        .into(),
                    },
                    text,
                    true,
                ))
            })
        };
        let Some((source, text, source_truncated)) = captured else {
            self.product.notice = Some(
                "Нет доступного снимка панели. Выберите терминал или страницу истории.".into(),
            );
            return;
        };
        if source.session.is_nil()
            || source.generation.is_nil()
            || text.trim().is_empty()
            || text.len() > handoff::MAX_SOURCE_BYTES
        {
            self.product.notice =
                Some("Выберите непустой текст терминала размером до 256 КиБ".into());
            return;
        }
        let mut dialog = Dialog::new(DialogKind::Handoff);
        dialog.fields[0] = text.clone().into();
        dialog.active_field = 1;
        self.product.handoff_sheet = Some(Sheet {
            dialog: dialog.id,
            source,
            tab,
            target: layout.focused,
            provider: "codex".into(),
            prepared: None,
            error: None,
            pending: false,
            origin,
            source_truncated,
            initial_text: text,
            cache: Default::default(),
        });
        self.product.dialog = Some(dialog);
        self.product.notice = None;
        cx.notify();
    }

    pub(super) fn handoff_action(&mut self, action: ProductAction, cx: &mut Context<Self>) {
        if matches!(action, ProductAction::OpenHandoff) {
            self.open_handoff(cx);
            return;
        }
        if let ProductAction::SetHandoffProvider(provider) = action {
            if matches!(provider.as_str(), "claude" | "codex" | "opencode")
                && let Some(sheet) = &mut self.product.handoff_sheet
            {
                sheet.provider = provider;
                sheet.error = None;
            }
            cx.notify();
            return;
        }
        if matches!(action, ProductAction::CopyHandoff) {
            match self.product.handoff_prompt() {
                Ok(prompt) => {
                    cx.write_to_clipboard(ClipboardItem::new_string(prompt.text));
                    self.product.notice =
                        Some("Текст передачи скопирован. В терминалы ничего не отправлено.".into());
                }
                Err(error) => self.product.notice = Some(error),
            }
            cx.notify();
            return;
        }
        if self.product.busy || self.product_launch_unknown || self.handoff_pending.is_some() {
            return;
        }
        if matches!(action, ProductAction::ConfirmHandoff) {
            if !self.product.handoff_ready() {
                self.product.notice = Some("Проверьте запуск после изменения черновика".into());
                return;
            }
            let sheet = self.product.handoff_sheet.as_ref().expect("ready sheet");
            let prepared = sheet.prepared.clone().expect("ready preview");
            let request = uuid::Uuid::new_v4();
            let pending = Pending {
                request,
                dialog: sheet.dialog,
                fields: prepared.fields.clone(),
                provider: prepared.provider.clone(),
                prompt: prepared.prompt.clone(),
                prepared: Some(prepared.clone()),
            };
            if self.try_product_command(json!({"command":"start_prepared","plan_id":prepared.preview.plan_id,"_ui_request":request}), cx) {
                self.product_launch_unknown = true;
                self.pending_launch = Some((prepared.preview.session,prepared.preview.generation));
                self.product.handoff_sheet.as_mut().expect("sheet").pending = true;
                self.handoff_pending = Some(pending);
            }
            return;
        }
        let prompt = match self.product.handoff_prompt() {
            Ok(value) => value,
            Err(error) => {
                self.product.notice = Some(error);
                return;
            }
        };
        let sheet = self
            .product
            .handoff_sheet
            .as_ref()
            .expect("prompt has sheet");
        if !matches!(sheet.provider.as_str(), "claude" | "codex") {
            self.product.notice=Some("Для этого CLI автоматическая передача не подтверждена. Скопируйте текст для ручной вставки.".into());
            return;
        }
        let request = uuid::Uuid::new_v4();
        let dialog = self.product.dialog.as_ref().expect("prompt has dialog");
        let pending = Pending {
            request,
            dialog: sheet.dialog,
            prepared: None,
            fields: fields(dialog),
            provider: sheet.provider.clone(),
            prompt: prompt.clone(),
        };
        let command = json!({"command":"prepare_launch","_ui_request":request,"request":{"tab":sheet.tab,"provider":sheet.provider,"side":"Right","target":sheet.target,"role_override":"","agent_instructions":prompt.text,"include_memory":false,"deliver_context":true}});
        if self.try_product_command(command, cx) {
            let sheet = self.product.handoff_sheet.as_mut().expect("sheet");
            sheet.pending = true;
            sheet.prepared = None;
            sheet.error = None;
            self.handoff_pending = Some(pending);
        }
    }

    /// Returns true only for this operation's reply, including after cancellation.
    pub(super) fn receive_handoff(
        &mut self,
        request: Option<uuid::Uuid>,
        preview: Option<&protocol::LaunchPreview>,
        phase: Option<&str>,
        confirmed: bool,
        error: Option<&str>,
    ) -> bool {
        if !self
            .handoff_pending
            .as_ref()
            .is_some_and(|pending| request == Some(pending.request))
        {
            return false;
        }
        let pending = self.handoff_pending.take().expect("matching handoff");
        if pending.prepared.is_some() {
            if confirmed {
                self.product.handoff_sheet = None;
                self.product.notice=Some("Создан новый агент с выбранным контекстом. Исходная сессия сохранена; применение контекста моделью не подтверждено.".into());
            } else if let Some(sheet) = &mut self.product.handoff_sheet {
                sheet.pending = false;
                sheet.error=Some(error.unwrap_or("Исход запуска неизвестен. Откройте «Сессии» для проверки; повтор не отправлен.").into());
                sheet.prepared = None;
            }
            return true;
        }
        let current = self
            .product
            .dialog
            .as_ref()
            .filter(|dialog| dialog.kind == DialogKind::Handoff && dialog.id == pending.dialog);
        let Some(sheet) = self
            .product
            .handoff_sheet
            .as_mut()
            .filter(|sheet| sheet.dialog == pending.dialog)
        else {
            return true;
        };
        sheet.pending = false;
        let Some(dialog) = current else {
            return true;
        };
        if !same_fields(dialog, &pending.fields) || sheet.provider != pending.provider {
            sheet.error =
                Some("Черновик изменён во время проверки. Проверьте запуск заново.".into());
            return true;
        }
        if let Some(error) = error {
            sheet.error = Some(error.into());
            return true;
        }
        let Some(preview) = preview.filter(|preview| {
            phase.is_none() && matches_preview(sheet, &pending.prompt, &pending.provider, preview)
        }) else {
            sheet.error=Some("CLI не подтвердил передачу выбранного контекста; запуск не выполнен. Можно скопировать текст.".into());
            return true;
        };
        sheet.error = preview.unavailable_reason.clone();
        sheet.prepared = Some(Prepared {
            preview: preview.clone(),
            prompt: pending.prompt,
            fields: pending.fields,
            provider: pending.provider,
        });
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{dirty_documents, workspace_git};

    fn draft() -> (Sheet, Dialog, protocol::LaunchPreview) {
        let source = handoff::Source {
            session: uuid::Uuid::new_v4(),
            generation: uuid::Uuid::new_v4(),
            sequence: 17,
            title: "Source".into(),
            scope: "Selected text".into(),
        };
        let mut dialog = Dialog::new(DialogKind::Handoff);
        dialog.fields = vec!["source output".into(), "Review this".into()];
        let sheet = Sheet {
            dialog: dialog.id,
            source,
            tab: uuid::Uuid::new_v4(),
            target: None,
            provider: "codex".into(),
            prepared: None,
            error: None,
            pending: false,
            origin: None,
            source_truncated: false,
            initial_text: "source output".into(),
            cache: Default::default(),
        };
        let prompt = handoff::prepare(
            &sheet.source,
            "source output",
            "Review this",
            handoff::BYTE_BUDGET,
            false,
        )
        .unwrap();
        let path =
            std::env::temp_dir().join(format!("pigade-handoff-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        let store = pigade_storage::MetadataStore::open(path.join("metadata.sqlite")).unwrap();
        let context = store
            .effective_context(
                uuid::Uuid::new_v4(),
                uuid::Uuid::new_v4(),
                "task",
                1,
                "",
                &prompt.text,
                false,
            )
            .unwrap();
        drop(store);
        std::fs::remove_dir_all(path).unwrap();
        let preview=serde_json::from_value(json!({"plan_id":uuid::Uuid::new_v4(),"tab_id":sheet.tab,"side":"Right","target":null,"session":uuid::Uuid::new_v4(),"generation":uuid::Uuid::new_v4(),"provider":"codex","cli_version":"0.162.1","adapter_version":1,"delivery_method":"argv","context":context,"capabilities":{"startup_context":true,"context_receipt":false,"native_resume":false,"conversation_id":false},"include_memory":false,"role_override":"","agent_instructions":prompt.text,"deliver_context":true,"can_launch":true,"unavailable_reason":null,"warnings":[]})).unwrap();
        (sheet, dialog, preview)
    }

    #[test]
    fn preparation_rejects_wrong_destination_source_reuse_or_different_material() {
        let (sheet, _, preview) = draft();
        let prompt = handoff::prepare(
            &sheet.source,
            "source output",
            "Review this",
            handoff::BYTE_BUDGET,
            false,
        )
        .unwrap();
        assert!(matches_preview(&sheet, &prompt, "codex", &preview));
        let mut wrong = preview.clone();
        wrong.session = sheet.source.session;
        assert!(!matches_preview(&sheet, &prompt, "codex", &wrong));
        wrong = preview.clone();
        wrong.tab_id = uuid::Uuid::new_v4();
        assert!(!matches_preview(&sheet, &prompt, "codex", &wrong));
        wrong = preview.clone();
        wrong.agent_instructions.push_str(" changed");
        assert!(!matches_preview(&sheet, &prompt, "codex", &wrong));
        wrong = preview.clone();
        wrong.context.text.push_str(" changed");
        assert!(!matches_preview(&sheet, &prompt, "codex", &wrong));
        wrong = preview.clone();
        wrong.deliver_context = false;
        assert!(!matches_preview(&sheet, &prompt, "codex", &wrong));
        assert!(!matches_preview(&sheet, &prompt, "opencode", &preview));
    }

    #[test]
    fn edits_provider_busy_and_dialog_replacement_invalidate_confirmation() {
        let (mut sheet, dialog, preview) = draft();
        let prompt = handoff::prepare(
            &sheet.source,
            "source output",
            "Review this",
            handoff::BYTE_BUDGET,
            false,
        )
        .unwrap();
        sheet.prepared = Some(Prepared {
            preview,
            prompt,
            fields: fields(&dialog),
            provider: "codex".into(),
        });
        let catalog=serde_json::from_value(json!({"projects":[],"workspaces":[],"tabs":[],"selection":{"tab_id":sheet.tab,"project_id":null,"workspace_id":null,"window":pigade_storage::WindowState::default(),"revision":1},"layout":{"revision":1,"json":"{}"}})).unwrap();
        let mut product = product::ProductUi {
            dialog: Some(dialog.clone()),
            handoff_sheet: Some(sheet),
            catalog: Some(catalog),
            ..Default::default()
        };
        assert!(product.handoff_ready());
        product.busy = true;
        assert!(!product.handoff_ready());
        product.busy = false;
        product.handoff_sheet.as_mut().unwrap().provider = "claude".into();
        assert!(!product.handoff_ready());
        product.handoff_sheet.as_mut().unwrap().provider = "codex".into();
        product.dialog.as_mut().unwrap().fields[1]
            .replace(None, " changed", true)
            .unwrap();
        assert!(!product.handoff_ready());
        product.dialog = Some(Dialog::new(DialogKind::Handoff));
        assert!(!product.handoff_ready());
        product.dialog = Some(dialog);
        product.catalog.as_mut().unwrap().selection.tab_id = Some(uuid::Uuid::new_v4());
        assert!(!product.handoff_ready());
    }

    #[test]
    fn cached_prompt_tracks_both_editor_revisions_and_close_guard_tracks_redaction() {
        let (sheet, dialog, _) = draft();
        let mut product = product::ProductUi {
            dialog: Some(dialog),
            handoff_sheet: Some(sheet),
            ..Default::default()
        };
        let original = product.handoff_prompt().unwrap();
        assert_eq!(product.handoff_prompt().unwrap(), original);
        product.dialog.as_mut().unwrap().fields[1].select_all();
        product.dialog.as_mut().unwrap().fields[1]
            .replace(None, "new ask", true)
            .unwrap();
        let changed = product.handoff_prompt().unwrap();
        assert_ne!(changed, original);
        assert!(changed.text.ends_with("new ask"));
        product.dialog.as_mut().unwrap().fields[1].select_all();
        product.dialog.as_mut().unwrap().fields[1]
            .replace(None, "", true)
            .unwrap();
        product.dialog.as_mut().unwrap().fields[0].select_all();
        product.dialog.as_mut().unwrap().fields[0]
            .replace(None, "redacted", true)
            .unwrap();
        assert!(product.handoff_prompt().is_err());
        let notices =
            dirty_documents::collect(&product, &workspace_git::Flow::default(), None, None);
        assert!(
            notices
                .iter()
                .any(|notice| notice.label == "Черновик передачи контекста")
        );
    }
}

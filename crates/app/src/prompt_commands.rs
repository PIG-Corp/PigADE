//! Explicit library operations and insertion into frozen product drafts only.
use super::*;
use product::{Dialog, DialogKind, ProductAction as A};

fn duplicate_prompt(
    prompt: &pigade_storage::Prompt,
) -> (prompt_flow::PromptEditor, String, String) {
    let suffix = " · копия";
    let mut end = prompt
        .title
        .len()
        .min(pigade_storage::MAX_PROMPT_TITLE_BYTES - suffix.len());
    while !prompt.title.is_char_boundary(end) {
        end -= 1;
    }
    (
        prompt_flow::PromptEditor::new(),
        format!("{}{}", prompt.title[..end].trim_end(), suffix),
        prompt.content.clone(),
    )
}

impl CanvasApp {
    pub(super) fn sync_prompt_library(&mut self) {
        let Some(dialog) = self.product.dialog.as_ref().filter(|dialog| {
            dialog.kind == DialogKind::Prompts
                && dialog.fields.iter().all(|field| field.marked().is_none())
        }) else {
            return;
        };
        let prompts = self
            .product
            .catalog
            .as_ref()
            .and_then(|catalog| catalog.prompts.as_deref())
            .unwrap_or_default();
        let previous = self.product.prompt_library.selected();
        let changed = self.product.prompt_library.sync(dialog, prompts);
        if changed {
            self.reveal_prompt_selection();
        }
        if previous != self.product.prompt_library.selected() {
            self.product
                .prompt_preview_scroll
                .set_offset(point(px(0.), px(0.)));
        }
    }

    fn reveal_prompt_selection(&self) {
        if let Some(index) = self
            .product
            .prompt_library
            .visible_rows()
            .iter()
            .position(|row| Some(row.prompt) == self.product.prompt_library.selected())
        {
            self.product.layout_scroll.scroll_to_item(index);
        }
    }

    fn remember_prompt_library(&mut self) {
        if let Some(dialog) = self
            .product
            .dialog
            .as_ref()
            .filter(|dialog| dialog.kind == DialogKind::Prompts)
        {
            self.product.prompt_library_dialog = Some(dialog.clone());
        }
    }

    fn reviewed_prompt(
        &self,
        reference: prompt_flow::PromptRef,
    ) -> Result<pigade_storage::Prompt, String> {
        if !self
            .product
            .dialog
            .as_ref()
            .is_some_and(|dialog| dialog.kind == DialogKind::Prompts)
            || self.product.prompt_library.error.is_some()
            || self.product.prompt_library.selected() != Some(reference)
        {
            return Err("Выбор промпта изменился; проверьте предпросмотр".into());
        }
        self.product
            .catalog
            .as_ref()
            .and_then(|catalog| catalog.prompts.as_ref())
            .and_then(|prompts| {
                prompts.iter().find(|prompt| {
                    prompt.id == reference.id && prompt.revision == reference.revision
                })
            })
            .cloned()
            .ok_or_else(|| {
                "Промпт изменён или удалён; обновите библиотеку и проверьте текст".into()
            })
    }

    pub(super) fn prompt_library_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self
            .product
            .dialog
            .as_ref()
            .is_some_and(|dialog| dialog.kind == DialogKind::Prompts)
        {
            return false;
        }
        self.sync_prompt_library();
        let k = &event.keystroke;
        let control = k.modifiers.control || k.modifiers.platform;
        let plain = !control && !k.modifiers.alt && !k.modifiers.shift;
        let composing =
            !self.text_input.preedit.is_empty()
                || self.product.dialog.as_ref().is_some_and(|dialog| {
                    dialog.fields.iter().any(|field| field.marked().is_some())
                });
        let reserved = matches!(k.key.as_str(), "enter" | "escape")
            || (plain && matches!(k.key.as_str(), "up" | "down" | "pageup" | "pagedown"))
            || (k.modifiers.alt
                && !control
                && matches!(k.key.as_str(), "pageup" | "pagedown" | "home" | "end"))
            || (control
                && !k.modifiers.alt
                && ((!k.modifiers.shift && matches!(k.key.as_str(), "n" | "e" | "d" | "r"))
                    || (k.modifiers.shift && matches!(k.key.as_str(), "delete" | "c"))));
        if !reserved {
            return false;
        }
        if composing {
            return false;
        }
        if !event.is_held
            || (plain && matches!(k.key.as_str(), "up" | "down" | "pageup" | "pagedown"))
        {
            let selected = self.product.prompt_library.selected();
            let action = match k.key.as_str() {
                "escape" => Some(A::CloseDialog),
                "enter" if !k.modifiers.alt && !k.modifiers.shift => selected.map(A::UsePrompt),
                "up" | "down" if plain => {
                    let previous = selected;
                    self.product
                        .prompt_library
                        .navigate(if k.key == "up" { -1 } else { 1 });
                    self.reveal_prompt_selection();
                    if previous != self.product.prompt_library.selected() {
                        self.product
                            .prompt_preview_scroll
                            .set_offset(point(px(0.), px(0.)));
                    }
                    None
                }
                "pageup" | "pagedown" if plain => Some(A::PromptPage(k.key == "pagedown")),
                "pageup" | "pagedown" | "home" | "end" if k.modifiers.alt && !control => {
                    let handle = &self.product.prompt_preview_scroll;
                    let mut offset = handle.offset();
                    offset.y = match k.key.as_str() {
                        "home" => px(0.),
                        "end" => -handle.max_offset().y,
                        "pageup" => (offset.y + px(160.)).min(px(0.)),
                        _ => (offset.y - px(160.)).max(-handle.max_offset().y),
                    };
                    handle.set_offset(offset);
                    None
                }
                "n" if control => Some(A::NewPrompt),
                "e" if control => selected.map(A::EditPrompt),
                "d" if control => selected.map(A::DuplicatePrompt),
                "r" if control => Some(A::ReloadPrompts),
                "c" if control && k.modifiers.shift => selected.map(A::CopyPrompt),
                "delete" if control && k.modifiers.shift => selected.map(A::DeletePrompt),
                _ => None,
            };
            if let Some(action) = action {
                self.product_action(action, window, cx);
            }
        }
        cx.notify();
        cx.stop_propagation();
        true
    }
    fn open_prompt_editor(
        &mut self,
        editor: prompt_flow::PromptEditor,
        title: String,
        content: String,
    ) {
        let mut dialog = Dialog::new(DialogKind::Prompt);
        dialog.fields = vec![title.into(), content.into()];
        dialog.fields[0].select_all();
        self.product.prompt_editor = Some(editor);
        self.product.dialog = Some(dialog);
        self.product.notice = None;
    }

    pub(super) fn prompt_action(&mut self, action: A, cx: &mut Context<Self>) {
        if self.product.busy
            && !matches!(
                action,
                A::SelectPrompt(_) | A::PromptPage(_) | A::UsePrompt(_) | A::CopyPrompt(_)
            )
        {
            self.product.notice = Some("Дождитесь завершения предыдущего действия".into());
            return;
        }
        if !self
            .product
            .catalog
            .as_ref()
            .is_some_and(|catalog| catalog.prompts.is_some())
        {
            self.product.notice =
                Some("Текущий session host не поддерживает библиотеку промптов".into());
            return;
        }
        match action {
            A::OpenPrompts => {
                if self
                    .product
                    .dialog
                    .as_ref()
                    .is_some_and(|dialog| dialog.kind == DialogKind::Prompts)
                {
                    return;
                }
                self.product.prompt_origin = self
                    .product
                    .dialog
                    .as_ref()
                    .filter(|dialog| prompt_flow::accepts_prompt(dialog))
                    .cloned();
                self.product.prompt_library = Default::default();
                self.product.prompt_library_dialog = None;
                self.product.dialog = Some(Dialog::new(DialogKind::Prompts));
                self.product.notice = None;
            }
            A::SaveDraftAsPrompt => {
                if let Some(origin) = self
                    .product
                    .dialog
                    .as_ref()
                    .filter(|dialog| prompt_flow::accepts_prompt(dialog))
                    .cloned()
                {
                    let field = &origin.fields[origin.active_field];
                    let text = if field.selected_text().is_empty() {
                        field.text()
                    } else {
                        field.selected_text()
                    }
                    .to_owned();
                    let mut editor = prompt_flow::PromptEditor::new();
                    editor.origin = Some(origin);
                    self.open_prompt_editor(editor, "Новый промпт".into(), text);
                }
            }
            A::ReloadPrompts => {
                let request = uuid::Uuid::new_v4();
                if self
                    .product
                    .dialog
                    .as_ref()
                    .is_some_and(|dialog| dialog.kind == DialogKind::Prompts)
                    && self
                        .try_product_command(json!({"command":"catalog","_ui_request":request}), cx)
                {
                    self.prompt_reload = Some(request);
                }
            }
            A::SelectPrompt(reference) => {
                if self
                    .product
                    .dialog
                    .as_ref()
                    .is_some_and(|dialog| dialog.kind == DialogKind::Prompts)
                {
                    if self.product.prompt_library.select(reference) {
                        self.reveal_prompt_selection();
                        self.product
                            .prompt_preview_scroll
                            .set_offset(point(px(0.), px(0.)));
                    } else {
                        self.product.notice =
                            Some("Список промптов изменился; выберите запись заново".into());
                    }
                }
            }
            A::PromptPage(forward) => {
                if self
                    .product
                    .dialog
                    .as_ref()
                    .is_some_and(|dialog| dialog.kind == DialogKind::Prompts)
                {
                    self.product.prompt_library.page(forward);
                    self.reveal_prompt_selection();
                    self.product
                        .prompt_preview_scroll
                        .set_offset(point(px(0.), px(0.)));
                }
            }
            A::NewPrompt => {
                if !self
                    .product
                    .dialog
                    .as_ref()
                    .is_some_and(|dialog| dialog.kind == DialogKind::Prompts)
                {
                    return;
                }
                self.remember_prompt_library();
                self.open_prompt_editor(
                    prompt_flow::PromptEditor::new(),
                    "Новый промпт".into(),
                    String::new(),
                );
            }
            A::EditPrompt(reference)
            | A::DuplicatePrompt(reference)
            | A::DeletePrompt(reference)
            | A::UsePrompt(reference)
            | A::CopyPrompt(reference) => {
                let prompt = match self.reviewed_prompt(reference) {
                    Ok(prompt) => prompt,
                    Err(error) => {
                        self.product.notice = Some(error);
                        cx.notify();
                        return;
                    }
                };
                self.remember_prompt_library();
                match action {
                    A::EditPrompt(_) => self.open_prompt_editor(
                        prompt_flow::PromptEditor::saved(prompt.clone()),
                        prompt.title,
                        prompt.content,
                    ),
                    A::DuplicatePrompt(_) => {
                        let (editor, title, content) = duplicate_prompt(&prompt);
                        self.open_prompt_editor(editor, title, content);
                    }
                    A::DeletePrompt(_) => {
                        self.product.prompt_delete = Some(prompt);
                        self.product.dialog = Some(Dialog::new(DialogKind::DeletePrompt));
                    }
                    A::UsePrompt(_) if self.product.prompt_origin.is_some() => {
                        match prompt_flow::insert_prompt(
                            self.product.prompt_origin.as_ref().expect("checked origin"),
                            &prompt.content,
                        ) {
                            Ok(draft) => {
                                self.product.dialog = Some(draft);
                                self.product.prompt_origin = None;
                                self.product.notice = Some(format!(
                                    "«{}» · ревизия {} вставлена в черновик",
                                    prompt.title, prompt.revision
                                ));
                            }
                            Err(error) => self.product.notice = Some(error),
                        }
                    }
                    _ => {
                        cx.write_to_clipboard(ClipboardItem::new_string(prompt.content));
                        self.product.notice = Some(format!(
                            "«{}» · ревизия {} скопирована",
                            prompt.title, prompt.revision
                        ));
                    }
                }
            }
            _ => (),
        }
        self.sync_prompt_library();
        cx.notify();
    }

    /// Return a suspended draft on cancel; closing a child returns to its library.
    pub(super) fn close_prompt_dialog(&mut self) -> bool {
        let Some(dialog) = &self.product.dialog else {
            return false;
        };
        match dialog.kind {
            DialogKind::Prompts => self.product.dialog = self.product.prompt_origin.take(),
            DialogKind::Prompt => {
                self.product.dialog = self
                    .product
                    .prompt_editor
                    .as_ref()
                    .and_then(|editor| editor.origin.clone())
                    .or_else(|| self.product.prompt_library_dialog.clone())
                    .or_else(|| Some(Dialog::new(DialogKind::Prompts)))
            }
            DialogKind::DeletePrompt => {
                self.product.dialog = self
                    .product
                    .prompt_library_dialog
                    .clone()
                    .or_else(|| Some(Dialog::new(DialogKind::Prompts)))
            }
            _ => return false,
        }
        true
    }

    pub(super) fn submit_prompt_dialog(
        &mut self,
        kind: DialogKind,
        fields: &[String],
        cx: &mut Context<Self>,
    ) -> bool {
        match kind {
            DialogKind::Prompts => {
                self.close_prompt_dialog();
            }
            DialogKind::Prompt => {
                if !self.product.busy
                    && let Some(editor) = &self.product.prompt_editor
                {
                    match editor.write(fields) {
                        Ok(write) => self.edit_catalog(
                            pigade_storage::CatalogBatch {
                                prompts: vec![write],
                                ..Default::default()
                            },
                            "Промпт сохранён",
                            Some(DialogKind::Prompts),
                            cx,
                        ),
                        Err(error) => self.product.notice = Some(error),
                    }
                }
            }
            DialogKind::DeletePrompt => {
                if !self.product.busy
                    && let Some(prompt) = &self.product.prompt_delete
                {
                    self.edit_catalog(
                        pigade_storage::CatalogBatch {
                            remove_prompts: vec![pigade_storage::PromptDelete {
                                id: prompt.id,
                                expected_revision: prompt.revision,
                            }],
                            ..Default::default()
                        },
                        "Промпт удалён из библиотеки",
                        Some(DialogKind::Prompts),
                        cx,
                    );
                }
            }
            _ => return false,
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::duplicate_prompt;

    #[test]
    fn duplicate_is_a_fresh_unsaved_identity_with_exact_content_and_bounded_unicode_title() {
        let source = pigade_storage::Prompt {
            id: uuid::Uuid::new_v4(),
            title: "界".repeat(85),
            content: "Review\n🦀 весь текст".into(),
            revision: 7,
        };
        let (editor, title, content) = duplicate_prompt(&source);
        assert_ne!(editor.id, source.id);
        assert!(editor.saved.is_none());
        assert!(editor.origin.is_none());
        assert!(title.ends_with(" · копия"));
        assert!(title.len() <= pigade_storage::MAX_PROMPT_TITLE_BYTES);
        assert_eq!(content, source.content);
        let write = editor.write(&[title, content]).unwrap();
        assert_eq!(write.expected_revision, None);
        assert_eq!(source.revision, 7);
        assert_ne!(write.value.id, source.id);
    }
}

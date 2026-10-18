//! Explicit library operations and insertion into frozen product drafts only.
use super::*;
use product::{Dialog, DialogKind, ProductAction as A};

impl CanvasApp {
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
        if self.product.busy {
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
                self.product.prompt_origin = self
                    .product
                    .dialog
                    .as_ref()
                    .filter(|dialog| prompt_flow::accepts_prompt(dialog))
                    .cloned();
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
            A::NewPrompt => self.open_prompt_editor(
                prompt_flow::PromptEditor::new(),
                "Новый промпт".into(),
                String::new(),
            ),
            A::EditPrompt(id) | A::DeletePrompt(id) | A::UsePrompt(id) | A::CopyPrompt(id) => {
                let Some(prompt) = self
                    .product
                    .catalog
                    .as_ref()
                    .and_then(|catalog| catalog.prompts.as_ref())
                    .and_then(|prompts| prompts.iter().find(|prompt| prompt.id == id))
                    .cloned()
                else {
                    self.product.notice = Some("Промпт удалён; обновите библиотеку".into());
                    return;
                };
                match action {
                    A::EditPrompt(_) => self.open_prompt_editor(
                        prompt_flow::PromptEditor::saved(prompt.clone()),
                        prompt.title,
                        prompt.content,
                    ),
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
                    .or_else(|| Some(Dialog::new(DialogKind::Prompts)))
            }
            DialogKind::DeletePrompt => {
                self.product.dialog = Some(Dialog::new(DialogKind::Prompts))
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

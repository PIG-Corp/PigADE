//! Read-only inventory of user-authored documents that would be lost on close.
//! This module deliberately reports labels and opaque keys only, never draft text.

use crate::product::{Dialog, DialogKind, ProductUi};
use crate::workspace_git::Flow as GitFlow;
use std::collections::HashSet;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DraftNotice {
    pub key: String,
    pub label: String,
}

const MAX_NOTICES: usize = 32;

fn field(dialog: &Dialog, index: usize) -> &str {
    dialog.fields.get(index).map_or("", |value| value.text())
}

fn meaningful(dialog: &Dialog) -> bool {
    dialog
        .fields
        .iter()
        .any(|value| !value.text().trim().is_empty())
}

fn notice(key: impl Into<String>, label: &'static str) -> DraftNotice {
    DraftNotice {
        key: key.into(),
        label: label.into(),
    }
}

fn context_changed(product: &ProductUi, dialog: &Dialog) -> bool {
    let saved = product.context.as_ref();
    let instruction_changed = saved.is_none_or(|value| field(dialog, 0) != value.instructions);
    if dialog.kind == DialogKind::ProjectContext {
        return instruction_changed;
    }
    instruction_changed
        || saved
            .is_none_or(|value| field(dialog, 1) != value.role || field(dialog, 2) != value.memory)
}

fn prompt_changed(product: &ProductUi, dialog: &Dialog) -> bool {
    let Some(editor) = product.prompt_editor.as_ref() else {
        return meaningful(dialog);
    };
    if let Some(saved) = &editor.saved {
        field(dialog, 0) != saved.title || field(dialog, 1) != saved.content
    } else {
        meaningful(dialog)
    }
}

fn profile_changed(product: &ProductUi, dialog: &Dialog) -> bool {
    let Some(editor) = product.profile_editor.as_ref() else {
        return meaningful(dialog);
    };
    if editor.settings.provider == "custom"
        || editor
            .saved
            .as_ref()
            .is_some_and(|saved| saved.settings.provider == "custom")
    {
        let Some(saved) = &editor.saved else {
            return meaningful(dialog);
        };
        if editor.settings != saved.settings {
            return true;
        }
        let baseline = crate::profile_flow::ProfileEditor::saved(saved.clone())
            .initial_fields(saved.name.clone());
        return dialog.fields.len() != baseline.len()
            || dialog
                .fields
                .iter()
                .zip(&baseline)
                .any(|(current, initial)| current.text() != initial.text());
    }
    let name_changed = editor.saved.as_ref().map_or_else(
        || !field(dialog, 0).trim().is_empty(),
        |saved| field(dialog, 0) != saved.name,
    );
    let settings_changed = if let Some(saved) = &editor.saved {
        let model = field(dialog, 3).trim();
        let effort = field(dialog, 4).trim();
        editor.settings != saved.settings
            || field(dialog, 1) != saved.settings.role
            || field(dialog, 2) != saved.settings.instructions
            || model != saved.settings.engine.model.as_deref().unwrap_or("")
            || effort
                != saved
                    .settings
                    .engine
                    .reasoning_effort
                    .as_deref()
                    .unwrap_or("")
    } else {
        meaningful(dialog)
            || editor.settings.deliver_context
                != matches!(editor.settings.provider.as_str(), "claude" | "codex")
            || !editor.settings.role.is_empty()
            || !editor.settings.instructions.is_empty()
            || !editor.settings.engine.is_empty()
    };
    name_changed || settings_changed
}

fn launch_review_changed(product: &ProductUi, dialog: &Dialog) -> bool {
    let Some(preview) = product.launch_preview.as_ref() else {
        return meaningful(dialog);
    };
    field(dialog, 0) != preview.role_override
        || field(dialog, 1) != preview.agent_instructions
        || field(dialog, 2)
            != crate::memory_choice::format_sections(
                &preview
                    .memory
                    .as_ref()
                    .map_or_else(Vec::new, |memory| memory.selected.clone()),
            )
        || field(dialog, 3) != preview.engine.model.as_deref().unwrap_or("")
        || field(dialog, 4) != preview.engine.reasoning_effort.as_deref().unwrap_or("")
}

fn dialog_changed(
    product: &ProductUi,
    dialog: &Dialog,
    rename_pane_baseline: Option<&str>,
    create_project_initial_parent: Option<&str>,
) -> Option<&'static str> {
    match dialog.kind {
        DialogKind::AddProject if meaningful(dialog) => Some("Путь открываемого проекта"),
        DialogKind::Conversation
            if field(dialog, 0)
                != product
                    .launch_preview
                    .as_ref()
                    .and_then(|preview| preview.conversation.as_ref())
                    .map_or("", |binding| binding.conversation_id.as_str()) =>
        {
            Some("Привязка диалога CLI")
        }
        DialogKind::Context | DialogKind::ProjectContext if context_changed(product, dialog) => {
            Some(if dialog.kind == DialogKind::ProjectContext {
                "Черновик инструкций проекта"
            } else {
                "Черновик контекста воркспейса"
            })
        }
        DialogKind::Prompt if prompt_changed(product, dialog) => Some("Черновик промпта"),
        DialogKind::AgentProfile if profile_changed(product, dialog) => {
            Some("Черновик профиля агента")
        }
        DialogKind::LaunchReview if launch_review_changed(product, dialog) => {
            Some("Изменённые параметры запуска")
        }
        DialogKind::RecipeSlot if recipe_slot_changed(product, dialog) => {
            Some("Черновик настроек агента")
        }
        DialogKind::Recipe | DialogKind::NewTemplateTab if recipe_changed(product) => {
            Some("Черновик раскладки")
        }
        DialogKind::CreateProject
            if !field(dialog, 0).trim().is_empty()
                || create_project_initial_parent
                    .is_some_and(|baseline| field(dialog, 1) != baseline) =>
        {
            Some("Новый проект")
        }
        DialogKind::CreateWorkspace if !field(dialog, 0).trim().is_empty() => {
            Some("Новый воркспейс")
        }
        DialogKind::Rename
            if product
                .management_target
                .as_ref()
                .is_some_and(|target| field(dialog, 0) != target.name()) =>
        {
            Some("Переименование")
        }
        DialogKind::RenamePane if rename_pane_changed(dialog, rename_pane_baseline) => {
            Some("Имя панели")
        }
        DialogKind::NewTab if !field(dialog, 0).trim().is_empty() => Some("Новая вкладка"),
        // Save Layout freezes a new geometry document even when its suggested
        // name is untouched, so closing the form discards an actual draft.
        DialogKind::SaveLayout if product.template_draft.is_some() => {
            Some("Новый шаблон раскладки")
        }
        _ => None,
    }
}

fn rename_pane_changed(dialog: &Dialog, baseline: Option<&str>) -> bool {
    match baseline {
        Some(saved) => field(dialog, 0) != saved,
        None => !field(dialog, 0).trim().is_empty(),
    }
}

fn recipe_slot_changed(product: &ProductUi, dialog: &Dialog) -> bool {
    let Some(editor) = product.recipe_editor.as_ref() else {
        return meaningful(dialog);
    };
    let Some(config) = editor.launches.get(editor.slot) else {
        return false;
    };
    if config.provider == "custom" {
        return recipe_changed(product);
    }
    field(dialog, 0) != config.role
        || field(dialog, 1) != config.instructions
        || field(dialog, 2).trim() != config.engine.model.as_deref().unwrap_or("")
        || field(dialog, 3).trim() != config.engine.reasoning_effort.as_deref().unwrap_or("")
        || recipe_changed(product)
}

fn recipe_changed(product: &ProductUi) -> bool {
    let Some(editor) = product.recipe_editor.as_ref() else {
        return false;
    };
    if let Some(saved) = &editor.saved {
        let baseline_launches = saved.launches.clone().unwrap_or_else(|| {
            vec![crate::template_flow::default_settings("shell"); saved.geometry.pane_count()]
        });
        editor.geometry != saved.geometry
            || editor.name != saved.name
            || editor.launches != baseline_launches
    } else {
        editor.origin == crate::template_flow::Origin::NewTab
            && matches!(
                product.dialog.as_ref().map(|dialog| dialog.kind),
                Some(
                    DialogKind::Recipe
                        | DialogKind::NewTemplateTab
                        | DialogKind::RecipeSlot
                        | DialogKind::RecipeProfiles
                )
            )
    }
}

fn add_dialog(
    product: &ProductUi,
    dialog: &Dialog,
    rename_pane_baseline: Option<&str>,
    create_project_initial_parent: Option<&str>,
    seen: &mut HashSet<Uuid>,
    notices: &mut Vec<DraftNotice>,
) {
    if notices.len() >= MAX_NOTICES || !seen.insert(dialog.id) {
        return;
    }
    if let Some(label) = dialog_changed(
        product,
        dialog,
        rename_pane_baseline,
        create_project_initial_parent,
    ) {
        let revisions: Vec<_> = dialog
            .fields
            .iter()
            .map(|field| field.revision().to_string())
            .collect();
        notices.push(notice(
            format!("dialog:{}:{}", dialog.id, revisions.join(",")),
            label,
        ));
    }
}

/// Return a bounded set of document-level drafts, excluding search and
/// navigation state. Draft contents are never included in notices.
pub(crate) fn collect(
    product: &ProductUi,
    git: &GitFlow,
    rename_pane_baseline: Option<&str>,
    create_project_initial_parent: Option<&str>,
) -> Vec<DraftNotice> {
    let mut notices = Vec::new();
    let mut seen_dialogs = HashSet::new();
    let mut seen_git_workspaces = HashSet::new();

    if let Some(dialog) = product.dialog.as_ref() {
        add_dialog(
            product,
            dialog,
            rename_pane_baseline,
            create_project_initial_parent,
            &mut seen_dialogs,
            &mut notices,
        );
    }
    if product
        .dialog
        .as_ref()
        .is_some_and(|dialog| dialog.kind == DialogKind::RecipeProfiles)
        && let Some(picker) = &product.recipe_profiles
    {
        add_dialog(
            product,
            &picker.origin,
            rename_pane_baseline,
            create_project_initial_parent,
            &mut seen_dialogs,
            &mut notices,
        );
    }
    if product
        .dialog
        .as_ref()
        .is_some_and(|dialog| dialog.kind == DialogKind::Prompts)
        && let Some(origin) = product.prompt_origin.as_ref()
    {
        add_dialog(
            product,
            origin,
            rename_pane_baseline,
            create_project_initial_parent,
            &mut seen_dialogs,
            &mut notices,
        );
    }
    if product
        .dialog
        .as_ref()
        .is_some_and(|dialog| dialog.kind == DialogKind::Prompt)
        && let Some(origin) = product
            .prompt_editor
            .as_ref()
            .and_then(|editor| editor.origin.as_ref())
    {
        add_dialog(
            product,
            origin,
            rename_pane_baseline,
            create_project_initial_parent,
            &mut seen_dialogs,
            &mut notices,
        );
    }
    if product
        .dialog
        .as_ref()
        .is_some_and(|dialog| dialog.kind == DialogKind::AgentProfile)
        && let Some(origin) = product
            .profile_editor
            .as_ref()
            .and_then(|editor| editor.origin.as_ref())
    {
        add_dialog(
            product,
            origin,
            rename_pane_baseline,
            create_project_initial_parent,
            &mut seen_dialogs,
            &mut notices,
        );
    }

    if notices.len() < MAX_NOTICES
        && let Some(dialog) = product.dialog.as_ref()
        && dialog.kind == DialogKind::WorkspaceGitCommit
        && meaningful(dialog)
    {
        let workspace = product
            .git_sheet
            .as_ref()
            .map(|sheet| sheet.target.workspace);
        if let Some(workspace) = workspace {
            seen_git_workspaces.insert(workspace);
            notices.push(notice(
                format!("git-commit:{workspace}"),
                "Черновик сообщения коммита",
            ));
        } else {
            notices.push(notice(
                format!("git-commit:{}", dialog.id),
                "Черновик сообщения коммита",
            ));
        }
    }
    for workspace in git.dirty_commit_workspaces() {
        if notices.len() >= MAX_NOTICES {
            break;
        }
        if seen_git_workspaces.insert(workspace) {
            notices.push(notice(
                format!("git-commit:{workspace}"),
                "Черновик сообщения коммита",
            ));
        }
    }

    notices.truncate(MAX_NOTICES);
    notices
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::product::Dialog;

    #[test]
    fn clean_saved_prompt_and_context_are_not_reported_but_post_save_edits_are() {
        let mut product = ProductUi::default();
        let saved = pigade_storage::Prompt {
            id: Uuid::from_u128(1),
            title: "Review".into(),
            content: "Check changes".into(),
            revision: 4,
        };
        let mut prompt = Dialog::new(DialogKind::Prompt);
        prompt.fields[0] = saved.title.clone().into();
        prompt.fields[1] = saved.content.clone().into();
        product.prompt_editor = Some(crate::prompt_flow::PromptEditor::saved(saved));
        product.dialog = Some(prompt.clone());
        let git = GitFlow::default();
        assert!(collect(&product, &git, None, None).is_empty());
        prompt.fields[1] = "Unsaved follow-up edit".into();
        product.dialog = Some(prompt);
        assert_eq!(
            collect(&product, &git, None, None)[0].label,
            "Черновик промпта"
        );
        assert!(
            !collect(&product, &git, None, None)[0]
                .label
                .contains("Unsaved")
        );
    }

    #[test]
    fn suspended_origin_is_found_once_and_clean_context_compares_to_saved_source() {
        let mut product = ProductUi::default();
        let scope = Uuid::from_u128(7);
        product.context = Some(pigade_storage::ContextDocument {
            scope_id: scope,
            revision: 3,
            instructions: "saved instructions".into(),
            role: "saved role".into(),
            memory: String::new(),
            content_hash: String::new(),
            saved_at_unix_ms: None,
            author: String::new(),
        });
        let mut origin = Dialog::new(DialogKind::Context);
        origin.fields[0] = "unsaved instructions".into();
        origin.fields[1] = "saved role".into();
        product.prompt_origin = Some(origin.clone());
        product.prompt_editor = Some(crate::prompt_flow::PromptEditor {
            id: Uuid::new_v4(),
            saved: None,
            origin: Some(origin),
        });
        product.dialog = Some(Dialog::new(DialogKind::Prompt));
        let notices = collect(&product, &GitFlow::default(), None, None);
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].label, "Черновик контекста воркспейса");
    }

    #[test]
    fn canceled_origins_are_ignored_and_pane_alias_clear_is_dirty() {
        let mut product = ProductUi::default();
        let mut canceled_origin = Dialog::new(DialogKind::Context);
        canceled_origin.fields[0] = "stale draft".into();
        product.prompt_origin = Some(canceled_origin);
        let git = GitFlow::default();
        product.dialog = Some(Dialog::new(DialogKind::Prompts));
        assert_eq!(collect(&product, &git, None, None).len(), 1);
        product.dialog = Some(Dialog::new(DialogKind::Palette));
        assert!(collect(&product, &git, None, None).is_empty());

        let mut rename = Dialog::new(DialogKind::RenamePane);
        product.dialog = Some(rename.clone());
        assert_eq!(
            collect(&product, &git, Some("saved alias"), None)[0].label,
            "Имя панели"
        );
        rename.fields[0] = "saved alias".into();
        product.dialog = Some(rename);
        assert!(collect(&product, &git, Some("saved alias"), None).is_empty());
    }

    #[test]
    fn project_parent_change_uses_the_exact_open_time_baseline() {
        let mut product = ProductUi::default();
        let mut dialog = Dialog::new(DialogKind::CreateProject);
        dialog.fields[1] = "/suggested/parent".into();
        product.dialog = Some(dialog.clone());
        let git = GitFlow::default();
        assert!(collect(&product, &git, None, Some("/suggested/parent")).is_empty());
        dialog.fields[1] = "/chosen/parent".into();
        product.dialog = Some(dialog);
        assert_eq!(
            collect(&product, &git, None, Some("/suggested/parent"))[0].label,
            "Новый проект"
        );
        assert!(collect(&product, &git, None, None).is_empty());
    }

    #[test]
    fn save_layout_geometry_is_a_draft_even_with_the_suggested_name() {
        let mut product = ProductUi {
            dialog: Some(Dialog::new(DialogKind::SaveLayout)),
            template_draft: Some(pigade_layout::GeometryTemplate {
                schema: 1,
                root: Some(pigade_layout::TemplateNode::Pane),
            }),
            ..Default::default()
        };
        product.dialog.as_mut().unwrap().fields[0] = "Development".into();
        assert_eq!(
            collect(&product, &GitFlow::default(), None, None)[0].label,
            "Новый шаблон раскладки"
        );
        product.template_draft = None;
        assert!(collect(&product, &GitFlow::default(), None, None).is_empty());
    }

    #[test]
    fn remembered_git_draft_is_reported_without_message_content() {
        let product = ProductUi::default();
        let mut git = GitFlow::default();
        let workspace = Uuid::from_u128(44);
        git.remember_commit_draft(workspace, "private commit message");
        let notices = collect(&product, &git, None, None);
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].key, format!("git-commit:{workspace}"));
        assert_eq!(notices[0].label, "Черновик сообщения коммита");
        assert!(!notices[0].label.contains("private"));
    }

    #[test]
    fn typed_project_paths_and_conversation_ids_are_protected_without_content_in_notices() {
        let mut dialog = Dialog::new(DialogKind::AddProject);
        dialog.fields[0] = "/private/project".into();
        let mut product = ProductUi {
            dialog: Some(dialog),
            ..Default::default()
        };
        let notices = collect(&product, &GitFlow::default(), None, None);
        assert_eq!(notices.len(), 1);
        assert!(!notices[0].label.contains("/private"));
        let mut conversation = Dialog::new(DialogKind::Conversation);
        product.dialog = Some(conversation.clone());
        assert!(collect(&product, &GitFlow::default(), None, None).is_empty());
        conversation.fields[0] = "specific-conversation".into();
        product.dialog = Some(conversation);
        assert_eq!(
            collect(&product, &GitFlow::default(), None, None)[0].label,
            "Привязка диалога CLI"
        );
    }

    #[test]
    fn empty_creation_forms_and_search_state_are_not_documents() {
        let mut product = ProductUi {
            dialog: Some(Dialog::new(DialogKind::CreateWorkspace)),
            ..Default::default()
        };
        let git = GitFlow::default();
        assert!(collect(&product, &git, None, None).is_empty());
        product.dialog = Some(Dialog::new(DialogKind::Palette));
        product.dialog.as_mut().unwrap().fields[0] = "deploy".into();
        assert!(collect(&product, &git, None, None).is_empty());
    }

    #[test]
    fn custom_profile_draft_protects_literal_and_invalid_arguments_without_exposing_them() {
        let mut settings = crate::template_flow::default_settings("custom");
        settings.custom_command = Some(pigade_storage::CustomCommand {
            executable: "/usr/bin/program".into(),
            args: vec!["literal \\ value".into()],
        });
        let saved = pigade_storage::AgentProfile {
            id: Uuid::from_u128(99),
            name: "Custom".into(),
            settings,
            revision: 1,
        };
        let editor = crate::profile_flow::ProfileEditor::saved(saved.clone());
        let mut dialog = Dialog::new(DialogKind::AgentProfile);
        dialog.fields = editor.initial_fields(saved.name);
        let mut product = ProductUi {
            dialog: Some(dialog),
            profile_editor: Some(editor),
            ..Default::default()
        };
        let git = GitFlow::default();
        assert!(collect(&product, &git, None, None).is_empty());
        product.dialog.as_mut().unwrap().fields[2] = "'private unfinished".into();
        let notices = collect(&product, &git, None, None);
        assert_eq!(notices.len(), 1);
        assert!(!notices[0].label.contains("private"));
        assert!(
            product
                .profile_editor
                .as_ref()
                .unwrap()
                .write(
                    &product
                        .dialog
                        .as_ref()
                        .unwrap()
                        .fields
                        .iter()
                        .map(|field| field.text().to_owned())
                        .collect::<Vec<_>>()
                )
                .is_err()
        );
    }
    #[test]
    fn profile_picker_protects_suspended_recipe_draft_but_not_its_query() {
        use crate::template_flow::{Origin, Preset};
        let mut editor = Preset::Solo.draft("codex");
        editor.origin = Origin::Saved;
        editor.saved = Some(pigade_storage::LayoutTemplate {
            id: Uuid::from_u128(110),
            name: editor.name.clone(),
            geometry: editor.geometry.clone(),
            launches: Some(editor.launches.clone()),
            revision: 1,
        });
        let mut origin = Dialog::new(DialogKind::RecipeSlot);
        origin.fields[0] = "private unsaved role".into();
        let picker =
            crate::recipe_profiles::Picker::new(origin, 0, editor.launches[0].clone(), &[], None)
                .unwrap();
        let mut search = Dialog::new(DialogKind::RecipeProfiles);
        search.fields[0] = "private query".into();
        let mut product = ProductUi {
            dialog: Some(search),
            recipe_editor: Some(editor),
            recipe_profiles: Some(picker),
            ..Default::default()
        };
        let notices = collect(&product, &GitFlow::default(), None, None);
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].label, "Черновик настроек агента");
        assert!(!notices[0].label.contains("private"));
        let editor = product.recipe_editor.as_ref().unwrap();
        product.recipe_profiles.as_mut().unwrap().origin.fields[0] =
            editor.launches[0].role.clone().into();
        assert!(collect(&product, &GitFlow::default(), None, None).is_empty());
        // A canceled selector is not a hidden dirty document.
        product.dialog = None;
        product.recipe_profiles.as_mut().unwrap().origin.fields[0] = "stale abandoned role".into();
        assert!(collect(&product, &GitFlow::default(), None, None).is_empty());
    }
}

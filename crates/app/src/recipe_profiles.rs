//! Immutable profile snapshots for choosing launch settings in a recipe slot.
use crate::template_flow::RecipeEditor;
use pigade_storage::{AgentProfile, AgentProfileRoster, LaunchSettings};
use std::collections::HashSet;
use uuid::Uuid;

pub(crate) struct Picker {
    pub(crate) origin: crate::product::Dialog,
    pub(crate) slot: usize,
    pub(crate) baseline: LaunchSettings,
    frozen: Vec<AgentProfile>,
    selected: Option<Uuid>,
}

impl Picker {
    pub(crate) fn new(
        origin: crate::product::Dialog,
        slot: usize,
        baseline: LaunchSettings,
        profiles: &[AgentProfile],
        roster: Option<&AgentProfileRoster>,
    ) -> Result<Self, String> {
        if profiles.len() > pigade_storage::MAX_AGENT_PROFILES {
            return Err("Библиотека содержит слишком много профилей агентов".into());
        }
        let mut seen = HashSet::with_capacity(profiles.len());
        for profile in profiles {
            if profile.id.is_nil() || !seen.insert(profile.id) {
                return Err("Список профилей содержит пустой или повторяющийся ID".into());
            }
            if profile.revision == 0 || profile.revision > i64::MAX as u64 {
                return Err("Профиль содержит неверную версию".into());
            }
            if profile.name.is_empty()
                || profile.name.len() > 256
                || profile.name.trim() != profile.name
                || profile.name.chars().any(char::is_control)
            {
                return Err("Профиль содержит неверное название".into());
            }
            pigade_storage::validate_agent_profile_settings(&profile.settings)
                .map_err(|error| error.to_string())?;
            let settings = serde_json::to_vec(&profile.settings).map_err(|e| e.to_string())?;
            if settings.len() > pigade_storage::MAX_AGENT_PROFILE_SETTINGS_BYTES {
                return Err("Настройки профиля превышают допустимый размер".into());
            }
        }

        let ids: Vec<_> = profiles.iter().map(|profile| profile.id).collect();
        let order = crate::profile_roster::display_order(&ids, roster);
        let mut frozen = Vec::with_capacity(profiles.len());
        for id in order.visible_ids() {
            if let Some(profile) = profiles.iter().find(|profile| profile.id == id) {
                frozen.push(profile.clone());
            }
        }
        Ok(Self {
            origin,
            slot,
            baseline,
            frozen,
            selected: None,
        })
    }

    pub(crate) fn profiles(&self, query: &str) -> Vec<&AgentProfile> {
        let query = query.to_lowercase();
        self.frozen
            .iter()
            .filter(|profile| {
                query.is_empty()
                    || profile.name.to_lowercase().contains(&query)
                    || profile.settings.provider.to_lowercase().contains(&query)
            })
            .collect()
    }

    pub(crate) fn selected(&self, query: &str) -> Option<&AgentProfile> {
        let visible = self.profiles(query);
        self.selected
            .and_then(|id| visible.iter().copied().find(|profile| profile.id == id))
            .or_else(|| visible.first().copied())
    }

    pub(crate) fn navigate(&mut self, query: &str, delta: isize) -> Option<usize> {
        let visible = self.profiles(query);
        if visible.is_empty() {
            self.selected = None;
            return None;
        }
        let current = self
            .selected
            .and_then(|id| visible.iter().position(|profile| profile.id == id))
            .unwrap_or(0);
        let length = visible.len() as isize;
        let next = (current as isize + delta.rem_euclid(length)).rem_euclid(length) as usize;
        self.selected = Some(visible[next].id);
        Some(next)
    }

    pub(crate) fn apply(
        &self,
        id: Uuid,
        editor: &mut RecipeEditor,
        custom_supported: bool,
        engine_supported: bool,
    ) -> Result<AgentProfile, String> {
        if editor.slot != self.slot || editor.launches.get(self.slot) != Some(&self.baseline) {
            return Err("Слот шаблона изменился после открытия профилей".into());
        }
        let profile = self
            .frozen
            .iter()
            .find(|profile| profile.id == id)
            .ok_or_else(|| "Выбранный профиль отсутствует в исходном списке".to_owned())?;
        if profile.settings.provider == "custom" && !custom_supported {
            return Err("Этот запуск не поддерживает пользовательские команды".into());
        }
        if !profile.settings.engine.is_empty() && !engine_supported {
            return Err("Этот запуск не поддерживает параметры модели".into());
        }
        let copied = profile.clone();
        editor.launches[self.slot] = copied.settings.clone();
        editor.changed();
        Ok(copied)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        product::DialogKind,
        template_flow::{Origin, Preset},
    };

    fn profile(id: u128, name: &str, provider: &str) -> AgentProfile {
        AgentProfile {
            id: Uuid::from_u128(id),
            name: name.into(),
            settings: LaunchSettings {
                provider: provider.into(),
                role: "Implementer".into(),
                instructions: "Keep the constraints".into(),
                deliver_context: true,
                engine: Default::default(),
                custom_command: None,
            },
            revision: 1,
        }
    }

    fn picker(profiles: &[AgentProfile], roster: Option<&AgentProfileRoster>) -> Picker {
        Picker::new(
            crate::product::Dialog::new(DialogKind::RecipeSlot),
            0,
            Preset::Solo.draft("codex").launches[0].clone(),
            profiles,
            roster,
        )
        .unwrap()
    }

    #[test]
    fn snapshots_are_isolated_and_copy_role_instructions_and_exact_custom_arguments() {
        let custom = AgentProfile {
            id: Uuid::from_u128(1),
            name: "Tool".into(),
            settings: LaunchSettings {
                provider: "custom".into(),
                role: String::new(),
                instructions: String::new(),
                deliver_context: false,
                engine: Default::default(),
                custom_command: Some(pigade_storage::CustomCommand {
                    executable: "/opt/tools/agent".into(),
                    args: vec!["--role".into(), "two words".into(), "$(literal)".into()],
                }),
            },
            revision: 3,
        };
        let source = profile(2, "Worker", "codex");
        let mut source_profiles = vec![custom.clone(), source.clone()];
        let picker = picker(&source_profiles, None);
        source_profiles[1].name = "Edited after open".into();
        source_profiles.clear();
        let mut editor = Preset::Solo.draft("codex");
        editor.origin = Origin::NewTab;
        editor.saved = Some(pigade_storage::LayoutTemplate {
            id: Uuid::from_u128(99),
            name: "Saved".into(),
            geometry: editor.geometry.clone(),
            launches: None,
            revision: 1,
        });
        let copied = picker.apply(custom.id, &mut editor, true, true).unwrap();
        assert_eq!(copied, custom);
        assert_eq!(editor.launches[0], custom.settings);
        assert!(editor.saved.is_none());

        assert_eq!(picker.profiles("")[1].name, "Worker");
        let mut editor = Preset::Solo.draft("codex");
        assert_eq!(
            picker
                .apply(source.id, &mut editor, true, true)
                .unwrap()
                .settings
                .role,
            "Implementer"
        );
    }

    #[test]
    fn roster_pins_filtering_and_navigation_are_stable_and_literal() {
        let profiles = [
            profile(1, "Alpha", "codex"),
            profile(2, "Regex [x]", "claude"),
            profile(3, "Beta", "opencode"),
        ];
        let roster = AgentProfileRoster {
            schema: 1,
            revision: 4,
            ordered_ids: vec![profiles[2].id, profiles[0].id, profiles[1].id],
            pinned_ids: vec![profiles[1].id],
        };
        let mut picker = picker(&profiles, Some(&roster));
        assert_eq!(
            picker.profiles("").iter().map(|p| p.id).collect::<Vec<_>>(),
            vec![profiles[1].id, profiles[2].id, profiles[0].id]
        );
        assert_eq!(picker.profiles("[").len(), 1);
        assert_eq!(picker.selected("[x]").unwrap().id, profiles[1].id);
        assert_eq!(picker.navigate("", 1), Some(1));
        assert_eq!(picker.navigate("", -1), Some(0));
        assert_eq!(picker.navigate("", 10), Some(1));
    }

    #[test]
    fn stale_slot_and_unsupported_capabilities_fail_without_mutating_editor() {
        let mut engineered = profile(2, "Engine", "codex");
        engineered.settings.engine.model = Some("gpt-6".into());
        let custom = AgentProfile {
            id: Uuid::from_u128(1),
            name: "Tool".into(),
            settings: LaunchSettings {
                provider: "custom".into(),
                role: String::new(),
                instructions: String::new(),
                deliver_context: false,
                engine: Default::default(),
                custom_command: Some(pigade_storage::CustomCommand {
                    executable: "/opt/tool".into(),
                    args: vec!["--x".into()],
                }),
            },
            revision: 1,
        };
        let picker = picker(&[custom.clone(), engineered.clone()], None);
        let mut editor = Preset::Solo.draft("codex");
        let before = editor.clone();
        assert!(picker.apply(custom.id, &mut editor, false, true).is_err());
        assert_eq!(editor.launches, before.launches);
        assert!(
            picker
                .apply(engineered.id, &mut editor, true, false)
                .is_err()
        );
        assert_eq!(editor.launches, before.launches);
        assert!(
            picker
                .apply(Uuid::from_u128(99), &mut editor, true, true)
                .is_err()
        );
        editor.launches[0].role.push_str(" changed");
        let changed = editor.clone();
        assert!(picker.apply(custom.id, &mut editor, true, true).is_err());
        assert_eq!(editor.launches, changed.launches);
    }
}

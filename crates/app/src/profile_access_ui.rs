//! Pinned saved-agent access in the sidebar, launcher, and launch preparation.
use super::*;
use crate::profile_roster;

fn provider_label(provider: &str) -> &str {
    match provider {
        "claude" => "Claude Code",
        "codex" => "Codex",
        "opencode" => "OpenCode",
        "custom" => "Другой CLI",
        other => other,
    }
}

impl ProductUi {
    /// Pinned profiles follow the validated roster's canonical pinned-first order.
    pub(crate) fn pinned_agent_profiles(&self) -> Vec<&pigade_storage::AgentProfile> {
        let Some(catalog) = &self.catalog else {
            return Vec::new();
        };
        let Some(profiles) = catalog.agent_profiles.as_ref() else {
            return Vec::new();
        };
        let ids: Vec<_> = profiles.iter().map(|profile| profile.id).collect();
        let order = profile_roster::display_order(&ids, catalog.agent_profile_roster.as_ref());
        if !order.can_mutate {
            return Vec::new();
        }
        let pinned: HashSet<_> = order.pinned_ids.iter().copied().collect();
        order
            .visible_ids()
            .into_iter()
            .filter(|id| pinned.contains(id))
            .filter_map(|id| profiles.iter().find(|profile| profile.id == id))
            .collect()
    }

    /// True only when the selected tab belongs to the selected ready workspace.
    pub(crate) fn profile_target_available(&self) -> bool {
        if self.busy {
            return false;
        }
        let Some(catalog) = self.catalog.as_ref() else {
            return false;
        };
        let (Some(workspace_id), Some(tab_id)) =
            (catalog.selection.workspace_id, catalog.selection.tab_id)
        else {
            return false;
        };
        catalog.workspaces.iter().any(|workspace| {
            workspace.id == workspace_id && workspace.status == WorkspaceStatus::Ready
        }) && catalog
            .tabs
            .iter()
            .any(|tab| tab.id == tab_id && tab.workspace_id == workspace_id)
    }

    /// Library entry is always available; pins are shown only in the full sidebar.
    pub(super) fn profile_access_sidebar(&self, cx: &mut Context<CanvasApp>) -> AnyElement {
        let colors = self.colors();
        if self.sidebar_compact {
            return self.icon_control(
                "sidebar-agent-profiles",
                "Профили агентов",
                "layers",
                ProductAction::OpenAgentProfiles,
                cx,
            );
        }

        let mut section = div()
            .id("sidebar-profile-access")
            .flex_shrink_0()
            .flex()
            .flex_col()
            .gap_1()
            .pt(px(8.))
            .border_t_1()
            .border_color(colors.border)
            .child(self.action_control(
                "sidebar-agent-profiles".into(),
                "Профили агентов".into(),
                ProductAction::OpenAgentProfiles,
                ControlStyle {
                    role: Role::Button,
                    selected: false,
                    compact: false,
                },
                cx,
            ));

        let profiles = self.pinned_agent_profiles();
        if !profiles.is_empty() {
            let mut pins = div()
                .id("sidebar-pinned-agent-profiles")
                .max_h(px(180.))
                .min_h_0()
                .flex()
                .flex_col()
                .gap_1()
                .overflow_y_scroll();
            let can_launch = self.profile_target_available();
            for profile in profiles {
                let id = profile.id;
                let label = format!(
                    "{} · {}",
                    profile.name,
                    provider_label(&profile.settings.provider)
                );
                let (action, hint) = if can_launch {
                    (
                        ProductAction::LaunchAgentProfile(id),
                        "Подготовить запуск профиля",
                    )
                } else {
                    (
                        ProductAction::OpenAgentProfiles,
                        "Сначала выберите воркспейс и вкладку",
                    )
                };
                pins = pins.child(div().flex_shrink_0().child(
                    self.action_control_with_accessible_name(
                        format!("sidebar-pinned-profile-{id}"),
                        label.clone(),
                        format!("{label}. {hint}"),
                        action,
                        ControlStyle {
                            role: Role::Button,
                            selected: false,
                            compact: true,
                        },
                        cx,
                    ),
                ));
            }
            section = section.child(pins);
        }
        section.into_any_element()
    }

    /// Pinned launch shortcuts are rendered without probing providers or processes.
    pub(super) fn profile_access_launcher(&self, cx: &mut Context<CanvasApp>) -> AnyElement {
        let profiles = self.pinned_agent_profiles();
        if profiles.is_empty() {
            return div()
                .id("launcher-pinned-profiles-empty")
                .h(px(0.))
                .into_any_element();
        }

        let colors = self.colors();
        let target = self.catalog.as_ref().and_then(|catalog| {
            let workspace = catalog
                .workspaces
                .iter()
                .find(|workspace| Some(workspace.id) == catalog.selection.workspace_id)?;
            let tab = catalog
                .tabs
                .iter()
                .find(|tab| Some(tab.id) == catalog.selection.tab_id)?;
            Some(format!("{} · {}", workspace_name(workspace), tab.name))
        });
        let hint = if self.profile_target_available() {
            format!(
                "Цель: {}",
                target.unwrap_or_else(|| "выбранная вкладка".into())
            )
        } else {
            "Выберите готовый воркспейс и его вкладку для запуска".into()
        };
        let mut section = div()
            .id("launcher-pinned-profiles")
            .mt(px(14.))
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .text_size(px(11.))
                    .text_color(colors.muted)
                    .child("Закреплённые профили"),
            );
        let can_launch = self.profile_target_available();
        let mut pins = div()
            .id("launcher-pinned-profile-rows")
            .max_h(px(180.))
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap_1();
        for (index, profile) in profiles.into_iter().enumerate() {
            let id = profile.id;
            let shortcut = if index < 9 {
                format!("Ctrl+{}", index + 1)
            } else {
                String::new()
            };
            let label = format!(
                "{} · {}",
                profile.name,
                provider_label(&profile.settings.provider)
            );
            let action = if can_launch {
                ProductAction::LaunchAgentProfile(id)
            } else {
                ProductAction::OpenAgentProfiles
            };
            pins = pins.child(div().flex_shrink_0().child(
                self.action_control_with_accessible_name(
                    format!("launcher-pinned-profile-{id}"),
                    format!("{shortcut}  {label}"),
                    format!("{shortcut}. {label}. {hint}"),
                    action,
                    ControlStyle {
                        role: Role::Button,
                        selected: false,
                        compact: true,
                    },
                    cx,
                ),
            ));
        }
        section = section.child(pins).child(
            div()
                .text_size(px(10.))
                .text_color(colors.muted)
                .child(hint),
        );
        section.into_any_element()
    }

    /// Read-only status while the exact host launch request is being prepared.
    pub(super) fn profile_preparing_overlay(&self, cx: &mut Context<CanvasApp>) -> AnyElement {
        let colors = self.colors();
        let label = self
            .profile_preparing_label
            .as_deref()
            .unwrap_or("Профиль агента");
        let status = self
            .notice
            .as_deref()
            .filter(|notice| !notice.is_empty())
            .unwrap_or("Подготовка профиля…");
        div()
            .id("profile-preparing-overlay")
            .accessibility_id("profile-preparing-overlay")
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .bg(rgb(0x090b0e).opacity(0.72))
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                this.product_action(ProductAction::CloseDialog, window, cx)
            }))
            .child(
                div()
                    .id("profile-preparing-dialog")
                    .role(Role::Dialog)
                    .aria_label("Подготовка сохранённого профиля")
                    .w_full()
                    .max_w(px(400.))
                    .mx(px(16.))
                    .flex()
                    .flex_col()
                    .gap_2()
                    .rounded(px(12.))
                    .border_1()
                    .border_color(colors.border)
                    .bg(colors.raised)
                    .px(px(20.))
                    .py(px(18.))
                    .on_click(cx.listener(|_, _: &ClickEvent, _, cx| cx.stop_propagation()))
                    .child(
                        div()
                            .id("profile-preparing-title")
                            .role(Role::Heading)
                            .aria_level(2)
                            .text_size(px(15.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(colors.text)
                            .child(label.to_owned()),
                    )
                    .child(
                        div()
                            .id("profile-preparing-status")
                            .role(Role::Status)
                            .text_size(px(12.))
                            .text_color(colors.muted)
                            .child(status.to_owned()),
                    )
                    .child(self.action_control(
                        "profile-preparing-cancel".into(),
                        "Отмена".into(),
                        ProductAction::CloseDialog,
                        ControlStyle {
                            role: Role::Button,
                            selected: false,
                            compact: false,
                        },
                        cx,
                    ))
                    .child(self.action_control(
                        "profile-preparing-library".into(),
                        "Профили агентов…".into(),
                        ProductAction::OpenAgentProfiles,
                        ControlStyle {
                            role: Role::Button,
                            selected: false,
                            compact: true,
                        },
                        cx,
                    )),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::ProductUi;
    use pigade_storage::{AgentProfile, AgentProfileRoster, Catalog, Selection, WorkspaceStatus};
    use uuid::Uuid;

    fn product() -> ProductUi {
        let workspace = Uuid::from_u128(10);
        let tab = Uuid::from_u128(20);
        let profiles: Vec<_> = (1..=3)
            .map(|id| AgentProfile {
                id: Uuid::from_u128(id),
                name: format!("Agent {id}"),
                settings: crate::template_flow::default_settings("codex"),
                revision: 1,
            })
            .collect();
        let catalog: Catalog = serde_json::from_value(serde_json::json!({
            "projects": [], "workspaces": [{"id":workspace,"project_id":Uuid::from_u128(30),"task":"Task","cwd":"/repo","mode":"shared_checkout","status":"ready","error":null,"context_revision":null,"revision":1}],
            "tabs":[{"id":tab,"workspace_id":workspace,"name":"Development","layout_json":"null","active_pane":null,"revision":1}],
            "selection":Selection { workspace_id:Some(workspace),tab_id:Some(tab),..Default::default() },
            "layout":pigade_storage::LayoutDocument::default(),"agent_profiles":profiles,
            "agent_profile_roster":AgentProfileRoster {schema:1,revision:1,ordered_ids:vec![Uuid::from_u128(3),Uuid::from_u128(1),Uuid::from_u128(2)],pinned_ids:vec![Uuid::from_u128(2),Uuid::from_u128(3)]}
        })).unwrap();
        ProductUi {
            catalog: Some(catalog),
            ..Default::default()
        }
    }

    #[test]
    fn quick_access_uses_only_valid_pins_in_canonical_order_without_mutation() {
        let mut product = product();
        let before = product.catalog.clone();
        assert_eq!(
            product
                .pinned_agent_profiles()
                .iter()
                .map(|profile| profile.id)
                .collect::<Vec<_>>(),
            vec![Uuid::from_u128(3), Uuid::from_u128(2)]
        );
        assert_eq!(product.catalog, before);
        product
            .catalog
            .as_mut()
            .unwrap()
            .agent_profile_roster
            .as_mut()
            .unwrap()
            .pinned_ids
            .push(Uuid::from_u128(2));
        assert!(product.pinned_agent_profiles().is_empty());
        product.catalog.as_mut().unwrap().agent_profile_roster = None;
        assert!(product.pinned_agent_profiles().is_empty());
    }

    #[test]
    fn quick_launch_target_requires_matching_ready_workspace_tab_and_idle_ui() {
        let mut product = product();
        assert!(product.profile_target_available());
        product.busy = true;
        assert!(!product.profile_target_available());
        product.busy = false;
        product.catalog.as_mut().unwrap().workspaces[0].status = WorkspaceStatus::Archived;
        assert!(!product.profile_target_available());
        product.catalog.as_mut().unwrap().workspaces[0].status = WorkspaceStatus::Ready;
        product.catalog.as_mut().unwrap().tabs[0].workspace_id = Uuid::from_u128(99);
        assert!(!product.profile_target_available());
        product.catalog.as_mut().unwrap().selection.tab_id = None;
        assert!(!product.profile_target_available());
    }
}

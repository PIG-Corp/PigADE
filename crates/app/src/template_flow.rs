//! UI-local recipe drafts and admission tracking. Launch identity belongs to the host.
use pigade_layout::{Axis, GeometryTemplate, TemplateNode};
use pigade_storage::{LaunchSettings, LayoutTemplate};
use serde::Serialize;
use uuid::Uuid;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Preset {
    Solo,
    Review,
    AgentsShell,
    Four,
}

impl Preset {
    pub(crate) const ALL: [Self; 4] = [Self::Solo, Self::Review, Self::AgentsShell, Self::Four];
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Solo => "Один агент",
            Self::Review => "Implementer + reviewer",
            Self::AgentsShell => "Два агента + shell",
            Self::Four => "Четыре панели",
        }
    }
    pub(crate) fn draft(self, provider: &str) -> RecipeEditor {
        fn split(axis: Axis, children: Vec<TemplateNode>) -> TemplateNode {
            TemplateNode::Split {
                axis,
                weights: vec![1.0 / children.len() as f64; children.len()],
                children,
            }
        }
        let root = match self {
            Self::Solo => TemplateNode::Pane,
            Self::Review => split(
                Axis::Horizontal,
                vec![TemplateNode::Pane, TemplateNode::Pane],
            ),
            Self::AgentsShell => split(
                Axis::Horizontal,
                vec![
                    TemplateNode::Pane,
                    split(Axis::Vertical, vec![TemplateNode::Pane, TemplateNode::Pane]),
                ],
            ),
            Self::Four => split(
                Axis::Horizontal,
                vec![
                    split(Axis::Vertical, vec![TemplateNode::Pane, TemplateNode::Pane]),
                    split(Axis::Vertical, vec![TemplateNode::Pane, TemplateNode::Pane]),
                ],
            ),
        };
        let geometry = GeometryTemplate {
            schema: 1,
            root: Some(root),
        };
        let mut launches = vec![default_settings(provider); geometry.pane_count()];
        if self == Self::Review {
            launches[0].role = role_text("implementer").into();
            launches[1].role = role_text("reviewer").into();
            for config in &mut launches {
                config.deliver_context = matches!(provider, "claude" | "codex");
            }
        }
        if self == Self::AgentsShell {
            launches[2] = default_settings("shell");
        }
        RecipeEditor {
            geometry,
            launches,
            name: self.label().into(),
            saved: None,
            origin: Origin::NewTab,
            slot: 0,
            slot_parent: None,
        }
    }
}

pub(crate) fn default_settings(provider: &str) -> LaunchSettings {
    LaunchSettings {
        provider: provider.into(),
        role: String::new(),
        instructions: String::new(),
        deliver_context: false,
        engine: Default::default(),
    }
}

pub(crate) fn role_text(role: &str) -> &'static str {
    match role {
        "implementer" => {
            "Implementer: реализуй согласованную задачу, учитывай существующий код, проверяй результат и кратко опиши изменения."
        }
        "reviewer" => {
            "Reviewer: проверь изменения на ошибки, регрессии и соответствие задаче. Укажи конкретные места и доказательства; не меняй код без явной задачи."
        }
        "researcher" => {
            "Researcher: исследуй вопрос по первичным источникам и существующему коду, отделяй проверенные факты от предположений, запиши выводы и ссылки."
        }
        "tester" => {
            "Tester: проверь пользовательский сценарий и важные границы, воспроизведи ошибки и сообщи точные шаги и результат проверки."
        }
        _ => "",
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Origin {
    Saved,
    NewTab,
}

#[derive(Clone, Debug)]
pub(crate) struct RecipeEditor {
    pub(crate) geometry: GeometryTemplate,
    pub(crate) launches: Vec<LaunchSettings>,
    pub(crate) name: String,
    pub(crate) saved: Option<LayoutTemplate>,
    pub(crate) origin: Origin,
    pub(crate) slot: usize,
    pub(crate) slot_parent: Option<crate::product::DialogKind>,
}

impl RecipeEditor {
    pub(crate) fn from_saved(template: LayoutTemplate, origin: Origin) -> Result<Self, String> {
        let count = template.geometry.pane_count();
        if !(1..=32).contains(&count) {
            return Err("Настройки запуска доступны для раскладок от 1 до 32 панелей".into());
        }
        let launches = template
            .launches
            .clone()
            .unwrap_or_else(|| vec![default_settings("shell"); count]);
        Ok(Self {
            geometry: template.geometry.clone(),
            launches,
            name: template.name.clone(),
            saved: Some(template),
            origin,
            slot: 0,
            slot_parent: None,
        })
    }
    pub(crate) fn source(&self) -> RunSource {
        if let Some(saved) = &self.saved {
            RunSource::Saved {
                id: saved.id,
                revision: saved.revision,
            }
        } else {
            RunSource::Custom {
                geometry: self.geometry.clone(),
                launches: self.launches.clone(),
            }
        }
    }
    pub(crate) fn changed(&mut self) {
        if self.origin == Origin::NewTab {
            self.saved = None;
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "source", rename_all = "snake_case")]
pub(crate) enum RunSource {
    Saved {
        id: Uuid,
        revision: u64,
    },
    Custom {
        geometry: GeometryTemplate,
        launches: Vec<LaunchSettings>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RequestKind {
    Prepare,
    Advance,
    Read,
    Cancel,
    List,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Pending {
    pub(crate) request: Uuid,
    pub(crate) dialog: Option<Uuid>,
    pub(crate) kind: RequestKind,
    pub(crate) run: Option<Uuid>,
}

#[derive(Default)]
pub(crate) struct RunFlow {
    pub(crate) pending: Option<Pending>,
    pub(crate) automatic: bool,
    pub(crate) cancel_requested: bool,
    pub(crate) unknown: bool,
}

impl RunFlow {
    pub(crate) fn take_reply(&mut self, request: Option<Uuid>) -> Option<Pending> {
        if self
            .pending
            .is_some_and(|pending| Some(pending.request) == request)
        {
            self.pending.take()
        } else {
            None
        }
    }
    pub(crate) fn pause(&mut self) {
        self.automatic = false;
    }
    pub(crate) fn failed(&mut self, kind: RequestKind) {
        self.pause();
        if matches!(
            kind,
            RequestKind::Advance | RequestKind::Cancel | RequestKind::Read
        ) {
            self.unknown = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn presets_are_identity_free_and_require_explicit_context_delivery() {
        for preset in Preset::ALL {
            let recipe = preset.draft("codex");
            recipe.geometry.validate().unwrap();
            assert_eq!(recipe.geometry.pane_count(), recipe.launches.len());
            for config in recipe.launches {
                config.validate().unwrap();
            }
        }
        let recipe = Preset::Review.draft("opencode");
        assert!(recipe.launches.iter().all(|config| !config.deliver_context));
        assert_eq!(
            Preset::AgentsShell
                .draft("codex")
                .launches
                .last()
                .unwrap()
                .provider,
            "shell"
        );
    }
    #[test]
    fn lost_advance_ack_cannot_authorize_another_launch_and_stale_reply_cannot_consume_new_request()
    {
        let first = Uuid::new_v4();
        let second = Uuid::new_v4();
        let mut flow = RunFlow {
            pending: Some(Pending {
                request: second,
                dialog: None,
                kind: RequestKind::Advance,
                run: Some(Uuid::new_v4()),
            }),
            automatic: true,
            ..Default::default()
        };
        assert!(flow.take_reply(Some(first)).is_none());
        assert!(flow.pending.is_some());
        let pending = flow.take_reply(Some(second)).unwrap();
        flow.failed(pending.kind);
        assert!(!flow.automatic && flow.unknown);
        assert!(flow.take_reply(Some(second)).is_none());
    }
}

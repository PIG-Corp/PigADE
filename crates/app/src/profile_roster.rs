//! Pure ordering and pin rules for the reusable agent profile library.
use pigade_storage::AgentProfileRoster;
use std::collections::HashSet;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DisplayOrder {
    pub ordered_ids: Vec<Uuid>,
    pub pinned_ids: Vec<Uuid>,
    pub can_mutate: bool,
    pub warning: Option<&'static str>,
}

pub(crate) fn display_order(
    profile_ids: &[Uuid],
    roster: Option<&AgentProfileRoster>,
) -> DisplayOrder {
    let fallback = || DisplayOrder {
        ordered_ids: profile_ids.to_vec(),
        pinned_ids: Vec::new(),
        can_mutate: false,
        warning: None,
    };
    let Some(roster) = roster else {
        return fallback();
    };
    if roster.schema != 1 {
        return DisplayOrder {
            warning: Some(
                "Хост вернул неподдерживаемую версию порядка профилей; показан исходный список.",
            ),
            ..fallback()
        };
    }
    let expected: HashSet<_> = profile_ids.iter().copied().collect();
    let ordered: HashSet<_> = roster.ordered_ids.iter().copied().collect();
    let pinned: HashSet<_> = roster.pinned_ids.iter().copied().collect();
    if profile_ids.len() > 128
        || profile_ids.iter().any(Uuid::is_nil)
        || expected.len() != profile_ids.len()
        || ordered.len() != roster.ordered_ids.len()
        || ordered != expected
        || pinned.len() != roster.pinned_ids.len()
        || !pinned.is_subset(&ordered)
        || roster.revision == 0
        || roster.revision > i64::MAX as u64
    {
        return DisplayOrder {
            warning: Some(
                "Порядок профилей повреждён; показан исходный список. Обновите библиотеку.",
            ),
            ..fallback()
        };
    }
    DisplayOrder {
        ordered_ids: roster.ordered_ids.clone(),
        pinned_ids: roster.pinned_ids.clone(),
        can_mutate: true,
        warning: None,
    }
}

impl DisplayOrder {
    pub(crate) fn visible_ids(&self) -> Vec<Uuid> {
        let pinned: HashSet<_> = self.pinned_ids.iter().copied().collect();
        self.ordered_ids
            .iter()
            .filter(|id| pinned.contains(id))
            .chain(self.ordered_ids.iter().filter(|id| !pinned.contains(id)))
            .copied()
            .collect()
    }

    pub(crate) fn selected_id(&self, active_field: usize) -> Option<Uuid> {
        active_field
            .checked_sub(1)
            .and_then(|index| self.visible_ids().get(index).copied())
    }

    pub(crate) fn visible_index(&self, id: Uuid) -> Option<usize> {
        self.visible_ids().iter().position(|value| *value == id)
    }
}

pub(crate) fn set_pinned(
    profile_ids: &[Uuid],
    roster: &AgentProfileRoster,
    id: Uuid,
    pinned: bool,
) -> Option<(Vec<Uuid>, Vec<Uuid>)> {
    if !display_order(profile_ids, Some(roster)).can_mutate || !roster.ordered_ids.contains(&id) {
        return None;
    }
    let mut pinned_ids = roster.pinned_ids.clone();
    if pinned {
        if !pinned_ids.contains(&id) {
            pinned_ids.push(id);
        }
    } else {
        pinned_ids.retain(|value| *value != id);
    }
    Some((roster.ordered_ids.clone(), pinned_ids))
}

pub(crate) fn move_within_partition(
    profile_ids: &[Uuid],
    roster: &AgentProfileRoster,
    id: Uuid,
    delta: isize,
) -> Option<(Vec<Uuid>, Vec<Uuid>)> {
    if !display_order(profile_ids, Some(roster)).can_mutate || !matches!(delta, -1 | 1) {
        return None;
    }
    let pinned: HashSet<_> = roster.pinned_ids.iter().copied().collect();
    let is_pinned = pinned.contains(&id);
    let mut group: Vec<_> = roster
        .ordered_ids
        .iter()
        .copied()
        .filter(|value| pinned.contains(value) == is_pinned)
        .collect();
    let index = group.iter().position(|value| *value == id)?;
    let neighbor = index.checked_add_signed(delta)?;
    if neighbor >= group.len() {
        return None;
    }
    group.swap(index, neighbor);
    let mut group = group.into_iter();
    let ordered_ids = roster
        .ordered_ids
        .iter()
        .map(|value| {
            if pinned.contains(value) == is_pinned {
                group.next().expect("same-partition order is preserved")
            } else {
                *value
            }
        })
        .collect();
    Some((ordered_ids, roster.pinned_ids.clone()))
}

pub(crate) fn command_payload(
    request: Uuid,
    expected_revision: u64,
    ordered_ids: &[Uuid],
    pinned_ids: &[Uuid],
) -> serde_json::Value {
    serde_json::json!({
        "command": "set_agent_profile_roster",
        "_ui_request": request,
        "expected_revision": expected_revision,
        "ordered_ids": ordered_ids,
        "pinned_ids": pinned_ids,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roster(ordered_ids: Vec<Uuid>, pinned_ids: Vec<Uuid>) -> AgentProfileRoster {
        AgentProfileRoster {
            schema: 1,
            revision: 7,
            ordered_ids,
            pinned_ids,
        }
    }

    #[test]
    fn pinned_display_is_stable_and_unknown_roster_falls_back_without_mutation() {
        let a = Uuid::from_u128(1);
        let b = Uuid::from_u128(2);
        let c = Uuid::from_u128(3);
        let ids = [a, b, c];
        let view = display_order(&ids, Some(&roster(vec![c, a, b], vec![b, c])));
        assert_eq!(view.visible_ids(), vec![c, b, a]);
        assert_eq!(view.selected_id(2), Some(b));
        assert!(view.can_mutate);

        let old_host = display_order(&ids, None);
        assert_eq!(old_host.visible_ids(), ids);
        assert!(!old_host.can_mutate);
    }

    #[test]
    fn invalid_and_unknown_rosters_never_drive_mutation_or_hide_profiles() {
        let a = Uuid::from_u128(1);
        let b = Uuid::from_u128(2);
        let ids = [a, b];
        let duplicate = roster(vec![a, a], vec![]);
        let invalid = display_order(&ids, Some(&duplicate));
        assert!(!invalid.can_mutate);
        assert_eq!(invalid.visible_ids(), ids);
        assert!(invalid.warning.is_some());

        let mut unknown = roster(vec![b, a], vec![b]);
        unknown.schema = 2;
        assert!(!display_order(&ids, Some(&unknown)).can_mutate);
        unknown.schema = 1;
        unknown.revision = u64::MAX;
        assert!(!display_order(&ids, Some(&unknown)).can_mutate);
    }

    #[test]
    fn pin_change_and_move_obey_partition_boundaries_and_canonical_slots() {
        let a = Uuid::from_u128(1);
        let b = Uuid::from_u128(2);
        let c = Uuid::from_u128(3);
        let ids = [a, b, c];
        let state = roster(vec![a, b, c], vec![b]);
        assert_eq!(
            set_pinned(&ids, &state, a, true),
            Some((vec![a, b, c], vec![b, a]))
        );
        assert_eq!(move_within_partition(&ids, &state, b, -1), None);
        assert_eq!(move_within_partition(&ids, &state, b, 1), None);
        assert_eq!(
            move_within_partition(&ids, &state, a, 1),
            Some((vec![c, b, a], vec![b]))
        );
        assert_eq!(move_within_partition(&ids, &state, a, -1), None);
    }

    #[test]
    fn roster_write_payload_carries_explicit_cas_and_exact_order() {
        let request = Uuid::from_u128(99);
        let ordered = [Uuid::from_u128(3), Uuid::from_u128(1), Uuid::from_u128(2)];
        let pinned = [Uuid::from_u128(1)];
        let payload = command_payload(request, 17, &ordered, &pinned);
        assert_eq!(payload["command"], "set_agent_profile_roster");
        assert_eq!(payload["_ui_request"], request.to_string());
        assert_eq!(payload["expected_revision"], 17);
        assert_eq!(
            payload["ordered_ids"],
            serde_json::to_value(ordered).unwrap()
        );
        assert_eq!(payload["pinned_ids"], serde_json::to_value(pinned).unwrap());
    }
}

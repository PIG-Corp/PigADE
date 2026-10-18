//! Platform-neutral tiling transactions. This crate has no process or terminal effects.
//!
//! The host durably commits a preview before publishing it. Window geometry is a
//! projection of the saved tree: it never changes axes, weights or pane identity.
use std::collections::{BTreeMap, BTreeSet, VecDeque};

use anyhow::{Result, bail, ensure};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub type PaneId = Uuid;
pub type TabId = Uuid;
pub type SplitId = Uuid;
pub const HISTORY_CAPACITY: usize = 100;
const MAX_DEPTH: usize = 64;
const MAX_NODES: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Axis {
    Horizontal,
    Vertical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

impl Direction {
    fn axis(self) -> Axis {
        match self {
            Self::Left | Self::Right => Axis::Horizontal,
            Self::Up | Self::Down => Axis::Vertical,
        }
    }

    fn before(self) -> bool {
        matches!(self, Self::Left | Self::Up)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Node {
    Pane {
        id: PaneId,
    },
    Split {
        id: SplitId,
        axis: Axis,
        weights: Vec<f64>,
        children: Vec<Node>,
    },
}

/// A portable pane arrangement. Pane and split identities are intentionally omitted.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GeometryTemplate {
    pub schema: u16,
    pub root: Option<TemplateNode>,
}

/// Identity-free counterpart to [`Node`] used by saved geometry templates.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum TemplateNode {
    Pane,
    Split {
        axis: Axis,
        weights: Vec<f64>,
        children: Vec<TemplateNode>,
    },
}

impl GeometryTemplate {
    pub fn from_tab(tab: &TabLayout) -> Result<Self> {
        fn strip(node: &Node) -> TemplateNode {
            match node {
                Node::Pane { .. } => TemplateNode::Pane,
                Node::Split {
                    axis,
                    weights,
                    children,
                    ..
                } => TemplateNode::Split {
                    axis: *axis,
                    weights: weights.clone(),
                    children: children.iter().map(strip).collect(),
                },
            }
        }
        let template = Self {
            schema: 1,
            root: tab.root.as_ref().map(strip),
        };
        template.validate()?;
        Ok(template)
    }

    pub fn pane_count(&self) -> usize {
        fn count(node: &TemplateNode) -> usize {
            match node {
                TemplateNode::Pane => 1,
                TemplateNode::Split { children, .. } => children.iter().map(count).sum(),
            }
        }
        self.root.as_ref().map(count).unwrap_or_default()
    }

    pub fn validate(&self) -> Result<()> {
        ensure!(self.schema == 1, "unsupported geometry template schema");
        fn validate_node(node: &TemplateNode, depth: usize, count: &mut usize) -> Result<()> {
            ensure!(depth <= MAX_DEPTH, "layout too deep");
            *count += 1;
            ensure!(*count <= MAX_NODES, "too many layout nodes");
            if let TemplateNode::Split {
                weights, children, ..
            } = node
            {
                ensure!(
                    children.len() >= 2 && weights.len() == children.len(),
                    "invalid split arity"
                );
                ensure!(
                    weights.iter().all(|w| w.is_finite() && *w > 0.0),
                    "invalid split weight"
                );
                ensure!(
                    (weights.iter().sum::<f64>() - 1.0).abs() < 1e-9,
                    "weights must be normalized"
                );
                for child in children {
                    validate_node(child, depth + 1, count)?;
                }
            }
            Ok(())
        }
        let mut count = 0;
        if let Some(root) = &self.root {
            validate_node(root, 0, &mut count)?;
        }
        Ok(())
    }
}

impl Node {
    pub fn pane(id: PaneId) -> Self {
        Self::Pane { id }
    }

    pub fn panes(&self) -> Vec<PaneId> {
        let mut panes = Vec::new();
        self.collect_panes(&mut panes);
        panes
    }

    fn collect_panes(&self, panes: &mut Vec<PaneId>) {
        match self {
            Self::Pane { id } => panes.push(*id),
            Self::Split { children, .. } => {
                for child in children {
                    child.collect_panes(panes);
                }
            }
        }
    }

    fn contains(&self, pane: PaneId) -> bool {
        match self {
            Self::Pane { id } => *id == pane,
            Self::Split { children, .. } => children.iter().any(|child| child.contains(pane)),
        }
    }

    fn split_mut(&mut self, split: SplitId) -> Option<&mut Self> {
        match self {
            Self::Split { id, .. } if *id == split => Some(self),
            Self::Split { children, .. } => {
                children.iter_mut().find_map(|node| node.split_mut(split))
            }
            Self::Pane { .. } => None,
        }
    }

    fn dock(&mut self, target: PaneId, pane: PaneId, direction: Direction, split: SplitId) -> bool {
        match self {
            Self::Pane { id } if *id == target => {
                let previous = self.clone();
                let new = Self::pane(pane);
                let children = if direction.before() {
                    vec![new, previous]
                } else {
                    vec![previous, new]
                };
                *self = Self::Split {
                    id: split,
                    axis: direction.axis(),
                    weights: vec![0.5, 0.5],
                    children,
                };
                true
            }
            Self::Split { children, .. } => children
                .iter_mut()
                .any(|node| node.dock(target, pane, direction, split)),
            _ => false,
        }
    }

    // Consuming removal makes empty/single-child collapse explicit.
    fn remove(self, pane: PaneId) -> Option<Self> {
        match self {
            Self::Pane { id } => (id != pane).then_some(Self::Pane { id }),
            Self::Split {
                id,
                axis,
                weights,
                children,
            } => {
                let mut retained_weights = Vec::new();
                let mut retained_children = Vec::new();
                for (weight, child) in weights.into_iter().zip(children) {
                    if let Some(child) = child.remove(pane) {
                        retained_weights.push(weight);
                        retained_children.push(child);
                    }
                }
                match retained_children.len() {
                    0 => None,
                    1 => retained_children.pop(),
                    _ => {
                        normalize(&mut retained_weights);
                        Some(Self::Split {
                            id,
                            axis,
                            weights: retained_weights,
                            children: retained_children,
                        })
                    }
                }
            }
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TabLayout {
    pub root: Option<Node>,
    pub focused: Option<PaneId>,
    pub focus_mode: bool,
}

impl TabLayout {
    pub fn panes(&self) -> Vec<PaneId> {
        self.root.as_ref().map(Node::panes).unwrap_or_default()
    }

    fn remove(&mut self, pane: PaneId) {
        self.root = self.root.take().and_then(|node| node.remove(pane));
        if self.focused == Some(pane) {
            self.focused = self.panes().first().copied();
        }
        if self.root.is_none() {
            self.focus_mode = false;
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LayoutState {
    pub tabs: BTreeMap<TabId, TabLayout>,
}

impl LayoutState {
    /// Validate untrusted/persisted trees before traversing or publishing them.
    pub fn validate(&self) -> Result<()> {
        ensure!(self.tabs.len() <= 256, "too many tabs");
        let mut ids = BTreeSet::new();
        let mut nodes = 0;
        for (tab_id, tab) in &self.tabs {
            ensure!(ids.insert(*tab_id), "duplicate tab ID");
            if let Some(root) = &tab.root {
                validate_node(root, 0, &mut nodes, &mut ids)?;
                ensure!(tab.focused.is_some(), "nonempty tab needs focus");
                ensure!(
                    tab.focused.is_some_and(|id| root.contains(id)),
                    "focus outside tab"
                );
            } else {
                ensure!(
                    tab.focused.is_none() && !tab.focus_mode,
                    "empty tab has focus"
                );
            }
        }
        Ok(())
    }

    pub fn pane_tab(&self, pane: PaneId) -> Option<TabId> {
        self.tabs.iter().find_map(|(id, tab)| {
            tab.root
                .as_ref()
                .is_some_and(|root| root.contains(pane))
                .then_some(*id)
        })
    }

    pub fn panes(&self) -> BTreeSet<PaneId> {
        self.tabs.values().flat_map(TabLayout::panes).collect()
    }

    fn tab_mut(&mut self, tab: TabId) -> Result<&mut TabLayout> {
        self.tabs
            .get_mut(&tab)
            .ok_or_else(|| anyhow::anyhow!("unknown tab {tab}"))
    }

    fn split_mut(&mut self, split: SplitId) -> Result<&mut Node> {
        self.tabs
            .values_mut()
            .find_map(|tab| tab.root.as_mut()?.split_mut(split))
            .ok_or_else(|| anyhow::anyhow!("unknown split {split}"))
    }

    fn insert(
        &mut self,
        tab: TabId,
        pane: PaneId,
        target: Option<PaneId>,
        side: Direction,
        split: SplitId,
    ) -> Result<()> {
        ensure!(self.pane_tab(pane).is_none(), "pane already placed");
        let tab = self.tab_mut(tab)?;
        match (&mut tab.root, target) {
            (None, None) => tab.root = Some(Node::pane(pane)),
            (Some(root), Some(target)) => {
                ensure!(root.dock(target, pane, side, split), "unknown target pane")
            }
            _ => bail!("nonempty tab needs a target; empty tab must have none"),
        }
        tab.focused = Some(pane);
        Ok(())
    }
}

fn validate_node(
    node: &Node,
    depth: usize,
    count: &mut usize,
    ids: &mut BTreeSet<Uuid>,
) -> Result<()> {
    ensure!(depth <= MAX_DEPTH, "layout too deep");
    *count += 1;
    ensure!(*count <= MAX_NODES, "too many layout nodes");
    match node {
        Node::Pane { id } => ensure!(ids.insert(*id), "duplicate pane/node ID"),
        Node::Split {
            id,
            weights,
            children,
            ..
        } => {
            ensure!(ids.insert(*id), "duplicate split/node ID");
            ensure!(
                children.len() >= 2 && weights.len() == children.len(),
                "invalid split arity"
            );
            ensure!(
                weights.iter().all(|w| w.is_finite() && *w > 0.0),
                "invalid split weight"
            );
            ensure!(
                (weights.iter().sum::<f64>() - 1.0).abs() < 1e-9,
                "weights must be normalized"
            );
            for child in children {
                validate_node(child, depth + 1, count, ids)?;
            }
        }
    }
    Ok(())
}

fn normalize(weights: &mut [f64]) {
    let sum: f64 = weights.iter().sum();
    for weight in weights {
        *weight /= sum;
    }
}

/// Caller-supplied split IDs keep previews and committed drops byte-for-byte identical.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Command {
    AddTab {
        tab: TabId,
    },
    Insert {
        tab: TabId,
        pane: PaneId,
        target: Option<PaneId>,
        side: Direction,
        split: SplitId,
    },
    Move {
        pane: PaneId,
        target: PaneId,
        side: Direction,
        split: SplitId,
    },
    Swap {
        first: PaneId,
        second: PaneId,
    },
    /// A single transaction spanning both tabs (and their owning workspaces).
    Transfer {
        pane: PaneId,
        to_tab: TabId,
        target: Option<PaneId>,
        side: Direction,
        split: SplitId,
    },
    ClosePane {
        pane: PaneId,
    },
    Resize {
        split: SplitId,
        weights: Vec<f64>,
    },
    /// Move one boundary while preserving its neighbors' combined share.
    ResizeBoundary {
        split: SplitId,
        boundary: usize,
        fraction: f64,
    },
    Equalize {
        split: SplitId,
    },
    Rotate {
        split: SplitId,
    },
    Focus {
        pane: PaneId,
    },
    ToggleFocusMode {
        tab: TabId,
    },
    ApplyTemplate {
        tab: TabId,
        template: GeometryTemplate,
    },
}

impl Command {
    fn run(self, state: &mut LayoutState) -> Result<()> {
        match self {
            Self::AddTab { tab } => {
                ensure!(!state.tabs.contains_key(&tab), "tab already exists");
                state.tabs.insert(tab, TabLayout::default());
            }
            Self::Insert {
                tab,
                pane,
                target,
                side,
                split,
            } => state.insert(tab, pane, target, side, split)?,
            Self::Move {
                pane,
                target,
                side,
                split,
            } => {
                ensure!(pane != target, "cannot dock pane against itself");
                let to_tab = state
                    .pane_tab(target)
                    .ok_or_else(|| anyhow::anyhow!("unknown target"))?;
                let from_tab = state
                    .pane_tab(pane)
                    .ok_or_else(|| anyhow::anyhow!("unknown pane"))?;
                state.tab_mut(from_tab)?.remove(pane);
                state.insert(to_tab, pane, Some(target), side, split)?;
            }
            Self::Transfer {
                pane,
                to_tab,
                target,
                side,
                split,
            } => {
                ensure!(target != Some(pane), "cannot transfer onto itself");
                let from_tab = state
                    .pane_tab(pane)
                    .ok_or_else(|| anyhow::anyhow!("unknown pane"))?;
                state.tab_mut(from_tab)?.remove(pane);
                state.insert(to_tab, pane, target, side, split)?;
            }
            Self::Swap { first, second } => {
                let first_tab = state
                    .pane_tab(first)
                    .ok_or_else(|| anyhow::anyhow!("unknown first pane"))?;
                let second_tab = state
                    .pane_tab(second)
                    .ok_or_else(|| anyhow::anyhow!("unknown second pane"))?;
                // Simultaneous traversal avoids a temporary UUID that could collide.
                for tab in state.tabs.values_mut() {
                    if let Some(root) = &mut tab.root {
                        swap_panes(root, first, second);
                    }
                }
                // Focus follows the controlling pane across a cross-tab swap.
                if first_tab != second_tab {
                    state.tab_mut(first_tab)?.focused = Some(second);
                    state.tab_mut(second_tab)?.focused = Some(first);
                }
            }
            Self::ClosePane { pane } => {
                let tab = state
                    .pane_tab(pane)
                    .ok_or_else(|| anyhow::anyhow!("unknown pane"))?;
                state.tab_mut(tab)?.remove(pane);
            }
            Self::Resize { split, mut weights } => {
                let Node::Split {
                    weights: current,
                    children,
                    ..
                } = state.split_mut(split)?
                else {
                    unreachable!()
                };
                ensure!(weights.len() == children.len(), "wrong weight count");
                ensure!(
                    weights.iter().all(|w| w.is_finite() && *w > 0.0),
                    "invalid weight"
                );
                let sum: f64 = weights.iter().sum();
                ensure!(sum.is_finite(), "weight sum overflow");
                normalize(&mut weights);
                *current = weights;
            }
            Self::ResizeBoundary {
                split,
                boundary,
                fraction,
            } => {
                ensure!(
                    fraction.is_finite() && fraction > 0.0 && fraction < 1.0,
                    "invalid boundary fraction"
                );
                let Node::Split { weights, .. } = state.split_mut(split)? else {
                    unreachable!()
                };
                ensure!(boundary < weights.len() - 1, "unknown boundary");
                let pair = weights[boundary] + weights[boundary + 1];
                weights[boundary] = pair * fraction;
                weights[boundary + 1] = pair * (1.0 - fraction);
            }
            Self::Equalize { split } => {
                let Node::Split { weights, .. } = state.split_mut(split)? else {
                    unreachable!()
                };
                let weight = 1.0 / weights.len() as f64;
                weights.fill(weight);
            }
            Self::Rotate { split } => {
                let Node::Split { axis, .. } = state.split_mut(split)? else {
                    unreachable!()
                };
                *axis = match *axis {
                    Axis::Horizontal => Axis::Vertical,
                    Axis::Vertical => Axis::Horizontal,
                };
            }
            Self::Focus { pane } => {
                let tab = state
                    .pane_tab(pane)
                    .ok_or_else(|| anyhow::anyhow!("unknown pane"))?;
                state.tab_mut(tab)?.focused = Some(pane);
            }
            Self::ToggleFocusMode { tab } => {
                let tab = state.tab_mut(tab)?;
                ensure!(tab.root.is_some(), "empty tab cannot enter focus mode");
                tab.focus_mode = !tab.focus_mode;
            }
            Self::ApplyTemplate { tab, template } => {
                template.validate()?;
                let current = state.tab_mut(tab)?;
                if !current.focus_mode && GeometryTemplate::from_tab(current)? == template {
                    return Ok(());
                }
                let panes = current.panes();
                ensure!(
                    panes.len() == template.pane_count(),
                    "template pane count does not match tab"
                );
                let mut available: BTreeSet<_> = state.tabs.keys().copied().collect();
                fn collect_ids(node: &Node, ids: &mut BTreeSet<Uuid>) {
                    match node {
                        Node::Pane { id } => {
                            ids.insert(*id);
                        }
                        Node::Split { id, children, .. } => {
                            ids.insert(*id);
                            for child in children {
                                collect_ids(child, ids);
                            }
                        }
                    }
                }
                for tab in state.tabs.values() {
                    if let Some(root) = &tab.root {
                        collect_ids(root, &mut available);
                    }
                }
                let mut pane_iter = panes.into_iter();
                fn materialize(
                    node: &TemplateNode,
                    panes: &mut impl Iterator<Item = PaneId>,
                    used: &mut BTreeSet<Uuid>,
                ) -> Node {
                    match node {
                        TemplateNode::Pane => {
                            Node::pane(panes.next().expect("validated pane count"))
                        }
                        TemplateNode::Split {
                            axis,
                            weights,
                            children,
                        } => {
                            let split = loop {
                                let candidate = Uuid::new_v4();
                                if used.insert(candidate) {
                                    break candidate;
                                }
                            };
                            Node::Split {
                                id: split,
                                axis: *axis,
                                weights: weights.clone(),
                                children: children
                                    .iter()
                                    .map(|child| materialize(child, panes, used))
                                    .collect(),
                            }
                        }
                    }
                }
                let root = template
                    .root
                    .as_ref()
                    .map(|node| materialize(node, &mut pane_iter, &mut available));
                let target = state.tab_mut(tab)?;
                target.root = root;
                target.focus_mode = false;
            }
        }
        Ok(())
    }
}

fn swap_panes(node: &mut Node, first: PaneId, second: PaneId) {
    match node {
        Node::Pane { id } if *id == first => *id = second,
        Node::Pane { id } if *id == second => *id = first,
        Node::Split { children, .. } => {
            for child in children {
                swap_panes(child, first, second);
            }
        }
        _ => {}
    }
}

/// Versioned placement history for one atomic host metadata commit.
/// Process state and terminal input are deliberately absent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LayoutHistory {
    schema: u16,
    current: LayoutState,
    undo: VecDeque<HistoryStep>,
    redo: Vec<HistoryStep>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct HistoryStep {
    before: LayoutState,
    after: LayoutState,
}

/// Snapshot history contains placement only. Undo can never spawn or stop a CLI.
#[derive(Debug, Clone)]
pub struct LayoutModel {
    state: LayoutState,
    undo: VecDeque<HistoryStep>,
    redo: Vec<HistoryStep>,
}

impl Default for LayoutModel {
    fn default() -> Self {
        Self::new(LayoutState::default()).expect("empty layout is valid")
    }
}

impl LayoutModel {
    pub fn new(state: LayoutState) -> Result<Self> {
        state.validate()?;
        Ok(Self {
            state,
            undo: VecDeque::new(),
            redo: Vec::new(),
        })
    }

    /// Validate all snapshots and their placement continuity before accepting history.
    /// Focus clicks are permitted between transactions and do not consume undo slots.
    pub fn from_history(history: LayoutHistory) -> Result<Self> {
        ensure!(history.schema == 1, "unsupported layout history schema");
        ensure!(
            history.undo.len() + history.redo.len() <= HISTORY_CAPACITY,
            "layout history exceeds capacity"
        );
        history.current.validate()?;
        for step in history.undo.iter().chain(&history.redo) {
            step.before.validate()?;
            step.after.validate()?;
            ensure!(
                !same_placement(&step.before, &step.after),
                "empty placement history step"
            );
        }
        let mut previous = None;
        for step in &history.undo {
            if let Some(previous) = previous {
                ensure!(
                    same_placement(previous, &step.before),
                    "broken undo history chain"
                );
            }
            previous = Some(&step.after);
        }
        if let Some(previous) = previous {
            ensure!(
                same_placement(previous, &history.current),
                "undo history differs from current placement"
            );
        }
        let mut next = &history.current;
        for step in history.redo.iter().rev() {
            ensure!(
                same_placement(next, &step.before),
                "broken redo history chain"
            );
            next = &step.after;
        }
        Ok(Self {
            state: history.current,
            undo: history.undo,
            redo: history.redo,
        })
    }

    /// Catalog-created tabs are metadata, not undoable placement operations.
    /// Rebase every snapshot so undo never removes a tab owned by the catalog.
    pub fn add_tab_metadata(&mut self, tab: TabId) -> Result<()> {
        let mut history = self.export_history();
        fn insert(state: &mut LayoutState, tab: TabId) -> Result<()> {
            ensure!(
                !state.tabs.contains_key(&tab),
                "tab already occurs in placement history"
            );
            state.tabs.insert(tab, TabLayout::default());
            Ok(())
        }
        insert(&mut history.current, tab)?;
        for step in history.undo.iter_mut().chain(&mut history.redo) {
            insert(&mut step.before, tab)?;
            insert(&mut step.after, tab)?;
        }
        *self = Self::from_history(history)?;
        Ok(())
    }

    /// Catalog-removed tabs are metadata, not undoable placement operations.
    /// Remove the tab from every snapshot and discard history steps made empty by
    /// that removal, while preserving the remaining placement history chain.
    pub fn remove_tab_metadata(&mut self, tab: TabId) -> Result<()> {
        let mut history = self.export_history();
        ensure!(
            history.current.tabs.contains_key(&tab),
            "tab does not occur in current placement"
        );

        fn remove(state: &mut LayoutState, tab: TabId) {
            state.tabs.remove(&tab);
        }

        remove(&mut history.current, tab);
        for step in history.undo.iter_mut().chain(&mut history.redo) {
            remove(&mut step.before, tab);
            remove(&mut step.after, tab);
        }
        history
            .undo
            .retain(|step| !same_placement(&step.before, &step.after));
        history
            .redo
            .retain(|step| !same_placement(&step.before, &step.after));
        *self = Self::from_history(history)?;
        Ok(())
    }

    pub fn export_history(&self) -> LayoutHistory {
        LayoutHistory {
            schema: 1,
            current: self.state.clone(),
            undo: self.undo.clone(),
            redo: self.redo.clone(),
        }
    }

    pub fn state(&self) -> &LayoutState {
        &self.state
    }
    pub fn undo_len(&self) -> usize {
        self.undo.len()
    }
    pub fn redo_len(&self) -> usize {
        self.redo.len()
    }

    /// Build a non-mutating drop/resize preview; failure cannot partially detach a pane.
    pub fn preview(&self, command: Command) -> Result<LayoutState> {
        let mut state = self.state.clone();
        command.run(&mut state)?;
        state.validate()?;
        Ok(state)
    }

    /// Apply after the host has durably committed this exact preview.
    pub fn apply(&mut self, command: Command) -> Result<bool> {
        let focus_only = matches!(command, Command::Focus { .. });
        let next = self.preview(command)?;
        if next == self.state {
            return Ok(false);
        }
        let previous = std::mem::replace(&mut self.state, next);
        if !focus_only {
            self.undo.push_back(HistoryStep {
                before: previous,
                after: self.state.clone(),
            });
            if self.undo.len() > HISTORY_CAPACITY {
                self.undo.pop_front();
            }
            self.redo.clear();
        }
        Ok(true)
    }

    pub fn undo(&mut self) -> bool {
        if let Some(mut step) = self.undo.pop_back() {
            step.after = std::mem::replace(&mut self.state, step.before.clone());
            self.redo.push(step);
            true
        } else {
            false
        }
    }

    pub fn redo(&mut self) -> bool {
        if let Some(mut step) = self.redo.pop() {
            step.before = std::mem::replace(&mut self.state, step.after.clone());
            self.undo.push_back(step);
            true
        } else {
            false
        }
    }
}

fn same_placement(first: &LayoutState, second: &LayoutState) -> bool {
    first.tabs.len() == second.tabs.len()
        && first.tabs.iter().all(|(id, tab)| {
            second
                .tabs
                .get(id)
                .is_some_and(|other| tab.root == other.root && tab.focus_mode == other.focus_mode)
        })
}

#[cfg(test)]
mod metadata_history_tests {
    use super::*;

    fn id(value: u128) -> Uuid {
        Uuid::from_u128(value)
    }

    fn two_tab_transfer() -> LayoutModel {
        let mut model = LayoutModel::default();
        model.add_tab_metadata(id(1)).unwrap();
        model.add_tab_metadata(id(2)).unwrap();
        model
            .apply(Command::Insert {
                tab: id(1),
                pane: id(10),
                target: None,
                side: Direction::Right,
                split: id(100),
            })
            .unwrap();
        model
            .apply(Command::Insert {
                tab: id(2),
                pane: id(20),
                target: None,
                side: Direction::Right,
                split: id(200),
            })
            .unwrap();
        model
            .apply(Command::Transfer {
                pane: id(10),
                to_tab: id(2),
                target: Some(id(20)),
                side: Direction::Right,
                split: id(201),
            })
            .unwrap();
        model
    }

    #[test]
    fn removing_tab_rebases_cross_tab_move_undo_and_redo() {
        let mut model = two_tab_transfer();
        model.remove_tab_metadata(id(2)).unwrap();
        assert!(!model.state().tabs.contains_key(&id(2)));
        assert!(model.state().tabs[&id(1)].root.is_none());

        assert!(model.undo());
        assert!(!model.state().tabs.contains_key(&id(2)));
        assert_eq!(model.state().pane_tab(id(10)), Some(id(1)));
        assert_eq!(model.state().pane_tab(id(20)), None);
        assert!(model.redo());
        assert!(!model.state().tabs.contains_key(&id(2)));
        assert!(model.state().tabs[&id(1)].root.is_none());
        assert_eq!(model.state().pane_tab(id(20)), None);
    }

    #[test]
    fn removing_tab_rebases_existing_redo_history() {
        let mut model = two_tab_transfer();
        assert!(model.undo());
        assert_eq!(model.state().pane_tab(id(10)), Some(id(1)));
        model.remove_tab_metadata(id(2)).unwrap();
        assert_eq!(model.redo_len(), 1);

        assert!(model.redo());
        assert!(!model.state().tabs.contains_key(&id(2)));
        assert!(model.state().tabs[&id(1)].root.is_none());
        assert!(model.undo());
        assert!(!model.state().tabs.contains_key(&id(2)));
        assert_eq!(model.state().pane_tab(id(10)), Some(id(1)));
    }

    #[test]
    fn removing_sole_tab_leaves_valid_empty_layout() {
        let mut model = LayoutModel::default();
        model.add_tab_metadata(id(1)).unwrap();
        model.remove_tab_metadata(id(1)).unwrap();

        assert!(model.state().tabs.is_empty());
        LayoutModel::from_history(model.export_history()).unwrap();
    }

    #[test]
    fn removing_missing_tab_does_not_mutate_model_or_history() {
        let mut model = two_tab_transfer();
        let before = model.export_history();

        assert!(model.remove_tab_metadata(id(3)).is_err());
        assert_eq!(model.export_history(), before);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Size {
    pub width: f64,
    pub height: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl Rect {
    fn valid(self) -> bool {
        [
            self.x,
            self.y,
            self.width,
            self.height,
            self.x + self.width,
            self.y + self.height,
        ]
        .into_iter()
        .all(f64::is_finite)
            && self.width > 0.0
            && self.height > 0.0
    }

    fn center(self) -> (f64, f64) {
        (self.x + self.width / 2.0, self.y + self.height / 2.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PaneRect {
    pub pane: PaneId,
    pub rect: Rect,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Divider {
    pub split: SplitId,
    /// Boundary between child `boundary` and `boundary + 1`.
    pub boundary: usize,
    pub axis: Axis,
    pub rect: Rect,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CanvasGeometry {
    /// May exceed the viewport to honor every pane's minimum size.
    pub bounds: Rect,
    /// Stable depth-first order, also used to resolve focus ties.
    pub panes: Vec<PaneRect>,
    pub dividers: Vec<Divider>,
}

impl LayoutState {
    /// Pure projection: callers provide terminal/chrome minimums in logical units.
    /// A small viewport expands the scrollable canvas, never the saved tree.
    pub fn project(
        &self,
        tab: TabId,
        viewport: Rect,
        minimum: Size,
        gap: f64,
    ) -> Result<CanvasGeometry> {
        self.validate()?;
        ensure!(viewport.valid(), "invalid viewport");
        ensure!(
            minimum.width.is_finite()
                && minimum.width > 0.0
                && minimum.height.is_finite()
                && minimum.height > 0.0,
            "invalid pane minimum"
        );
        ensure!(gap.is_finite() && gap >= 0.0, "invalid gap");
        let tab = self
            .tabs
            .get(&tab)
            .ok_or_else(|| anyhow::anyhow!("unknown tab"))?;
        let mut geometry = CanvasGeometry {
            bounds: viewport,
            panes: Vec::new(),
            dividers: Vec::new(),
        };
        let Some(root) = &tab.root else {
            return Ok(geometry);
        };
        let size = if tab.focus_mode {
            minimum
        } else {
            required_size(root, minimum, gap)
        };
        geometry.bounds.width = viewport.width.max(size.width);
        geometry.bounds.height = viewport.height.max(size.height);
        ensure!(
            geometry.bounds.valid(),
            "layout cannot be represented in finite geometry"
        );
        if tab.focus_mode {
            geometry.panes.push(PaneRect {
                pane: tab.focused.expect("validated focus"),
                rect: geometry.bounds,
            });
        } else {
            project_node(root, geometry.bounds, gap, &mut geometry);
            ensure!(
                geometry.panes.iter().all(|pane| pane.rect.valid()),
                "layout precision exhausted"
            );
        }
        Ok(geometry)
    }
}

fn required_size(node: &Node, minimum: Size, gap: f64) -> Size {
    match node {
        Node::Pane { .. } => minimum,
        Node::Split {
            axis,
            weights,
            children,
            ..
        } => {
            let mut size = Size {
                width: 0.0,
                height: 0.0,
            };
            for (weight, child) in weights.iter().zip(children) {
                let child = required_size(child, minimum, gap);
                match axis {
                    Axis::Horizontal => {
                        size.width = size.width.max(child.width / weight);
                        size.height = size.height.max(child.height);
                    }
                    Axis::Vertical => {
                        size.width = size.width.max(child.width);
                        size.height = size.height.max(child.height / weight);
                    }
                }
            }
            match axis {
                Axis::Horizontal => size.width += gap * (children.len() - 1) as f64,
                Axis::Vertical => size.height += gap * (children.len() - 1) as f64,
            }
            size
        }
    }
}

fn project_node(node: &Node, rect: Rect, gap: f64, geometry: &mut CanvasGeometry) {
    match node {
        Node::Pane { id } => geometry.panes.push(PaneRect { pane: *id, rect }),
        Node::Split {
            id,
            axis,
            weights,
            children,
        } => {
            let extent = match axis {
                Axis::Horizontal => rect.width,
                Axis::Vertical => rect.height,
            };
            let usable = extent - gap * (children.len() - 1) as f64;
            let mut offset = 0.0;
            for (index, (weight, child)) in weights.iter().zip(children).enumerate() {
                let last = index + 1 == children.len();
                let length = if last {
                    extent - offset
                } else {
                    usable * weight
                };
                let child_rect = match axis {
                    Axis::Horizontal => Rect {
                        x: rect.x + offset,
                        width: length,
                        ..rect
                    },
                    Axis::Vertical => Rect {
                        y: rect.y + offset,
                        height: length,
                        ..rect
                    },
                };
                project_node(child, child_rect, gap, geometry);
                offset += length;
                if !last {
                    let divider_rect = match axis {
                        Axis::Horizontal => Rect {
                            x: rect.x + offset,
                            width: gap,
                            ..rect
                        },
                        Axis::Vertical => Rect {
                            y: rect.y + offset,
                            height: gap,
                            ..rect
                        },
                    };
                    geometry.dividers.push(Divider {
                        split: *id,
                        boundary: index,
                        axis: *axis,
                        rect: divider_rect,
                    });
                    offset += gap;
                }
            }
        }
    }
}

impl CanvasGeometry {
    /// No wrap. Overlap on the perpendicular axis takes precedence over distance.
    /// Then choose nearest edge, nearest center, and stable tree order.
    pub fn neighbor(&self, from: PaneId, direction: Direction) -> Option<PaneId> {
        let source = self.panes.iter().find(|pane| pane.pane == from)?.rect;
        let (sx, sy) = source.center();
        let mut best: Option<((bool, f64, f64), PaneId)> = None;
        for pane in &self.panes {
            if pane.pane == from {
                continue;
            }
            let rect = pane.rect;
            let (x, y) = rect.center();
            let (in_half, overlap, distance) = match direction {
                Direction::Left => (
                    x < sx,
                    overlap(source.y, source.height, rect.y, rect.height),
                    (source.x - rect.x - rect.width).max(0.0),
                ),
                Direction::Right => (
                    x > sx,
                    overlap(source.y, source.height, rect.y, rect.height),
                    (rect.x - source.x - source.width).max(0.0),
                ),
                Direction::Up => (
                    y < sy,
                    overlap(source.x, source.width, rect.x, rect.width),
                    (source.y - rect.y - rect.height).max(0.0),
                ),
                Direction::Down => (
                    y > sy,
                    overlap(source.x, source.width, rect.x, rect.width),
                    (rect.y - source.y - source.height).max(0.0),
                ),
            };
            if !in_half {
                continue;
            }
            let score = (!overlap, distance, (x - sx).hypot(y - sy));
            // Strict comparison preserves depth-first order on ties.
            if best.as_ref().is_none_or(|(previous, _)| score < *previous) {
                best = Some((score, pane.pane));
            }
        }
        best.map(|(_, pane)| pane)
    }
}

fn overlap(start: f64, length: f64, other: f64, other_length: f64) -> bool {
    start < other + other_length && other < start + length
}

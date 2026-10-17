use std::collections::BTreeSet;

use pigade_layout::{
    Axis, Command, Direction, GeometryTemplate, LayoutModel, LayoutState, Node, Rect, Size,
    TabLayout, TemplateNode,
};
use uuid::Uuid;

fn id(value: u128) -> Uuid {
    Uuid::from_u128(value)
}

fn model() -> LayoutModel {
    let mut model = LayoutModel::default();
    model.apply(Command::AddTab { tab: id(1) }).unwrap();
    model.apply(Command::AddTab { tab: id(2) }).unwrap();
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
            tab: id(1),
            pane: id(11),
            target: Some(id(10)),
            side: Direction::Right,
            split: id(101),
        })
        .unwrap();
    model
        .apply(Command::Insert {
            tab: id(1),
            pane: id(12),
            target: Some(id(10)),
            side: Direction::Down,
            split: id(102),
        })
        .unwrap();
    model
}

fn geometry(
    state: &LayoutState,
    tab: Uuid,
    width: f64,
    height: f64,
) -> pigade_layout::CanvasGeometry {
    state
        .project(
            tab,
            Rect {
                x: 0.0,
                y: 0.0,
                width,
                height,
            },
            Size {
                width: 80.0,
                height: 50.0,
            },
            6.0,
        )
        .unwrap()
}

fn split_ids(node: &Node, ids: &mut Vec<Uuid>) {
    if let Node::Split { id, children, .. } = node {
        ids.push(*id);
        for child in children {
            split_ids(child, ids);
        }
    }
}

fn weighted_template() -> GeometryTemplate {
    GeometryTemplate {
        schema: 1,
        root: Some(TemplateNode::Split {
            axis: Axis::Vertical,
            weights: vec![0.3, 0.7],
            children: vec![
                TemplateNode::Pane,
                TemplateNode::Split {
                    axis: Axis::Horizontal,
                    weights: vec![0.2, 0.8],
                    children: vec![TemplateNode::Pane, TemplateNode::Pane],
                },
            ],
        }),
    }
}

#[test]
fn geometry_template_roundtrips_without_identity() {
    let template = GeometryTemplate::from_tab(&TabLayout {
        root: Some(Node::Split {
            id: id(100),
            axis: Axis::Vertical,
            weights: vec![0.3, 0.7],
            children: vec![
                Node::pane(id(10)),
                Node::Split {
                    id: id(101),
                    axis: Axis::Horizontal,
                    weights: vec![0.2, 0.8],
                    children: vec![Node::pane(id(11)), Node::pane(id(12))],
                },
            ],
        }),
        focused: Some(id(12)),
        focus_mode: true,
    })
    .unwrap();
    assert_eq!(template, weighted_template());
    assert_eq!(template.pane_count(), 3);
    let encoded = serde_json::to_string(&template).unwrap();
    assert!(!encoded.contains(&id(10).to_string()));
    assert!(!encoded.contains(&id(100).to_string()));
    assert_eq!(
        serde_json::from_str::<GeometryTemplate>(&encoded).unwrap(),
        template
    );
}

#[test]
fn applying_geometry_template_is_atomic_and_one_undoable_placement() {
    let mut model = model();
    model.apply(Command::Focus { pane: id(11) }).unwrap();
    model
        .apply(Command::ToggleFocusMode { tab: id(1) })
        .unwrap();
    let before = model.state().clone();
    let other_tab = before.tabs[&id(2)].clone();

    let mut mismatch = weighted_template();
    mismatch.root = Some(TemplateNode::Pane);
    assert!(
        model
            .apply(Command::ApplyTemplate {
                tab: id(1),
                template: mismatch,
            })
            .is_err()
    );
    let mut bad_schema = weighted_template();
    bad_schema.schema = 2;
    assert!(
        model
            .apply(Command::ApplyTemplate {
                tab: id(1),
                template: bad_schema,
            })
            .is_err()
    );
    let mut bad_weights = weighted_template();
    if let Some(TemplateNode::Split { weights, .. }) = &mut bad_weights.root {
        weights[0] = 0.0;
    }
    assert!(
        model
            .apply(Command::ApplyTemplate {
                tab: id(1),
                template: bad_weights,
            })
            .is_err()
    );
    let mut too_deep = TemplateNode::Pane;
    for _ in 0..=64 {
        too_deep = TemplateNode::Split {
            axis: Axis::Horizontal,
            weights: vec![0.5, 0.5],
            children: vec![too_deep, TemplateNode::Pane],
        };
    }
    assert!(
        model
            .apply(Command::ApplyTemplate {
                tab: id(1),
                template: GeometryTemplate {
                    schema: 1,
                    root: Some(too_deep),
                },
            })
            .is_err()
    );
    assert_eq!(model.state(), &before);

    let undo_before = model.undo_len();
    assert!(
        model
            .apply(Command::ApplyTemplate {
                tab: id(1),
                template: weighted_template(),
            })
            .unwrap()
    );
    let applied = model.state().clone();
    assert_eq!(model.undo_len(), undo_before + 1);
    let tab = &applied.tabs[&id(1)];
    assert_eq!(tab.panes(), vec![id(10), id(12), id(11)]);
    assert_eq!(tab.focused, Some(id(11)));
    assert!(!tab.focus_mode);
    assert_eq!(applied.tabs[&id(2)], other_tab);
    let Node::Split {
        id: outer,
        children,
        ..
    } = tab.root.as_ref().unwrap()
    else {
        panic!("expected templated split")
    };
    let Node::Split { id: inner, .. } = &children[1] else {
        panic!("expected nested templated split")
    };
    assert_ne!(*outer, id(100));
    assert_ne!(*inner, id(102));
    let applied_undo_len = model.undo_len();
    assert!(
        !model
            .apply(Command::ApplyTemplate {
                tab: id(1),
                template: weighted_template(),
            })
            .unwrap()
    );
    assert_eq!(model.undo_len(), applied_undo_len);
    assert_eq!(model.state(), &applied);
    assert!(model.undo());
    assert_eq!(model.state(), &before);
    assert!(model.redo());
    assert_eq!(model.state(), &applied);
}

#[test]
fn empty_geometry_is_valid_but_cannot_replace_nonempty_tab() {
    let empty = GeometryTemplate {
        schema: 1,
        root: None,
    };
    empty.validate().unwrap();
    assert_eq!(empty.pane_count(), 0);
    let mut model = model();
    let before = model.state().clone();
    assert!(
        model
            .apply(Command::ApplyTemplate {
                tab: id(1),
                template: empty,
            })
            .is_err()
    );
    assert_eq!(model.state(), &before);
}

#[test]
fn all_four_docks_keep_original_region_and_ids() {
    for direction in [
        Direction::Left,
        Direction::Right,
        Direction::Up,
        Direction::Down,
    ] {
        let mut model = LayoutModel::default();
        model.apply(Command::AddTab { tab: id(1) }).unwrap();
        model
            .apply(Command::Insert {
                tab: id(1),
                pane: id(10),
                target: None,
                side: direction,
                split: id(100),
            })
            .unwrap();
        model
            .apply(Command::Insert {
                tab: id(1),
                pane: id(11),
                target: Some(id(10)),
                side: direction,
                split: id(101),
            })
            .unwrap();
        let state = model.state();
        let Some(Node::Split {
            axis,
            weights,
            children,
            ..
        }) = &state.tabs[&id(1)].root
        else {
            panic!("expected split")
        };
        assert_eq!(
            *axis,
            if matches!(direction, Direction::Left | Direction::Right) {
                Axis::Horizontal
            } else {
                Axis::Vertical
            }
        );
        assert_eq!(weights, &[0.5, 0.5]);
        let expected = if matches!(direction, Direction::Left | Direction::Up) {
            [id(11), id(10)]
        } else {
            [id(10), id(11)]
        };
        assert_eq!(
            children.iter().flat_map(Node::panes).collect::<Vec<_>>(),
            expected
        );
    }
}

#[test]
fn previews_and_failed_transfers_are_atomic() {
    let mut model = model();
    let before = model.state().clone();
    let command = Command::Transfer {
        pane: id(10),
        to_tab: id(2),
        target: None,
        side: Direction::Right,
        split: id(500),
    };
    let preview = model.preview(command.clone()).unwrap();
    assert_eq!(&before, model.state());
    model.apply(command).unwrap();
    assert_eq!(&preview, model.state());
    assert_eq!(model.state().pane_tab(id(10)), Some(id(2)));
    model.undo();
    assert_eq!(&before, model.state());
    assert!(
        model
            .apply(Command::Transfer {
                pane: id(10),
                to_tab: id(999),
                target: None,
                side: Direction::Down,
                split: id(501)
            })
            .is_err()
    );
    assert_eq!(&before, model.state());
    assert!(
        model
            .apply(Command::Move {
                pane: id(10),
                target: id(10),
                side: Direction::Down,
                split: id(501)
            })
            .is_err()
    );
    assert_eq!(&before, model.state());
}

#[test]
fn move_swap_collapse_and_close_undo_preserve_panes() {
    let mut model = model();
    let panes = model.state().panes();
    model
        .apply(Command::Move {
            pane: id(12),
            target: id(11),
            side: Direction::Down,
            split: id(500),
        })
        .unwrap();
    assert_eq!(model.state().panes(), panes);
    model
        .apply(Command::Swap {
            first: id(10),
            second: id(11),
        })
        .unwrap();
    assert_eq!(model.state().panes(), panes);
    model.apply(Command::ClosePane { pane: id(12) }).unwrap();
    assert_eq!(model.state().panes().len(), 2);
    model.undo();
    assert_eq!(model.state().panes(), panes);
    model.apply(Command::ClosePane { pane: id(12) }).unwrap();
    model.apply(Command::ClosePane { pane: id(11) }).unwrap();
    assert_eq!(model.state().tabs[&id(1)].root, Some(Node::pane(id(10))));
    model
        .apply(Command::ToggleFocusMode { tab: id(1) })
        .unwrap();
    model.apply(Command::ClosePane { pane: id(10) }).unwrap();
    assert_eq!(model.state().tabs[&id(1)], TabLayout::default());
}

#[test]
fn focus_mode_and_window_resize_are_projections_only() {
    let mut model = model();
    model.apply(Command::Focus { pane: id(10) }).unwrap();
    let tree = model.state().tabs[&id(1)].root.clone();
    let before = model.state().clone();
    for (w, h) in [
        (800.0, 600.0),
        (600.0, 800.0),
        (10.0, 10.0),
        (3840.0, 2160.0),
    ] {
        let geometry = geometry(model.state(), id(1), w, h);
        assert_eq!(geometry.panes.len(), 3);
        for pane in geometry.panes {
            assert!(pane.rect.width >= 80.0 - 1e-9);
            assert!(pane.rect.height >= 50.0 - 1e-9);
        }
    }
    assert_eq!(&before, model.state());
    model
        .apply(Command::ToggleFocusMode { tab: id(1) })
        .unwrap();
    let projected = geometry(model.state(), id(1), 800.0, 600.0);
    assert_eq!(projected.panes.len(), 1);
    assert_eq!(projected.panes[0].pane, id(10));
    assert!(projected.dividers.is_empty());
    assert_eq!(model.state().tabs[&id(1)].root, tree);
    model
        .apply(Command::ToggleFocusMode { tab: id(1) })
        .unwrap();
    assert_eq!(&before, model.state());
}

#[test]
fn directional_focus_prefers_overlap_then_stable_tree_order_without_wrap() {
    let state = model().state().clone();
    let projected = geometry(&state, id(1), 1000.0, 800.0);
    assert_eq!(projected.neighbor(id(10), Direction::Down), Some(id(12)));
    assert_eq!(projected.neighbor(id(10), Direction::Right), Some(id(11)));
    assert_eq!(projected.neighbor(id(11), Direction::Left), Some(id(10)));
    assert_eq!(projected.neighbor(id(11), Direction::Right), None);
    assert_eq!(projected.neighbor(id(10), Direction::Left), None);
    let custom = pigade_layout::CanvasGeometry {
        bounds: Rect {
            x: 0.0,
            y: 0.0,
            width: 1000.0,
            height: 800.0,
        },
        dividers: Vec::new(),
        panes: vec![
            pigade_layout::PaneRect {
                pane: id(10),
                rect: Rect {
                    x: 0.0,
                    y: 0.0,
                    width: 100.0,
                    height: 100.0,
                },
            },
            pigade_layout::PaneRect {
                pane: id(11),
                rect: Rect {
                    x: 110.0,
                    y: 110.0,
                    width: 100.0,
                    height: 100.0,
                },
            },
            pigade_layout::PaneRect {
                pane: id(12),
                rect: Rect {
                    x: 800.0,
                    y: 0.0,
                    width: 100.0,
                    height: 100.0,
                },
            },
        ],
    };
    assert_eq!(custom.neighbor(id(10), Direction::Right), Some(id(12)));
}

#[test]
fn resize_equalize_and_rotate_are_explicit_and_reversible() {
    let mut model = model();
    model
        .apply(Command::Resize {
            split: id(101),
            weights: vec![2.0, 6.0],
        })
        .unwrap();
    let Some(Node::Split { weights, .. }) = &model.state().tabs[&id(1)].root else {
        panic!()
    };
    assert_eq!(weights, &[0.25, 0.75]);
    model
        .apply(Command::ResizeBoundary {
            split: id(101),
            boundary: 0,
            fraction: 0.3,
        })
        .unwrap();
    let before = model.state().clone();
    model.apply(Command::Rotate { split: id(101) }).unwrap();
    model.undo();
    assert_eq!(&before, model.state());
    model.apply(Command::Equalize { split: id(101) }).unwrap();
    let Some(Node::Split { weights, .. }) = &model.state().tabs[&id(1)].root else {
        panic!()
    };
    assert_eq!(weights, &[0.5, 0.5]);
    let before = model.state().clone();
    for bad in [
        vec![0.0, 1.0],
        vec![-1.0, 2.0],
        vec![f64::NAN, 1.0],
        vec![f64::MAX, f64::MAX],
        vec![1.0],
    ] {
        assert!(
            model
                .apply(Command::Resize {
                    split: id(101),
                    weights: bad
                })
                .is_err()
        );
        assert_eq!(&before, model.state());
    }
}

#[test]
fn hundred_undo_redo_and_branching_history_preserve_exact_state() {
    let mut model = model();
    let original = model.state().clone();
    for _ in 0..100 {
        model.apply(Command::Rotate { split: id(101) }).unwrap();
    }
    assert_eq!(model.undo_len(), 100);
    for _ in 0..100 {
        assert!(model.undo());
    }
    assert_eq!(&original, model.state());
    assert!(!model.undo());
    assert_eq!(model.redo_len(), 100);
    for _ in 0..100 {
        assert!(model.redo());
    }
    assert_eq!(&original, model.state());
    assert!(!model.redo());
    model.undo();
    model
        .apply(Command::Resize {
            split: id(101),
            weights: vec![1.0, 2.0],
        })
        .unwrap();
    assert_eq!(model.redo_len(), 0);
    assert!(!model.redo());
}

#[test]
fn restored_state_validates_identity_weights_and_focus() {
    let state = model().state().clone();
    let restored: LayoutState =
        serde_json::from_slice(&serde_json::to_vec(&state).unwrap()).unwrap();
    assert_eq!(state, restored);
    LayoutModel::new(restored).unwrap();
    let mut bad = state.clone();
    bad.tabs.get_mut(&id(2)).unwrap().root = Some(Node::pane(id(10)));
    bad.tabs.get_mut(&id(2)).unwrap().focused = Some(id(10));
    assert!(LayoutModel::new(bad).is_err());
    let mut bad = state.clone();
    bad.tabs.get_mut(&id(1)).unwrap().focused = Some(id(999));
    assert!(LayoutModel::new(bad).is_err());
    let mut bad = state;
    if let Some(Node::Split { weights, .. }) = &mut bad.tabs.get_mut(&id(1)).unwrap().root {
        weights[0] = 10.0;
    }
    assert!(LayoutModel::new(bad).is_err());
}

#[test]
fn deterministic_operation_sequences_preserve_all_invariants() {
    // Dependency-free property-style corpus: 32 seeds × 250 transactions.
    for seed in 1..=32u64 {
        let mut rng = seed;
        let mut model = model();
        for pane in 13..18 {
            model
                .apply(Command::Insert {
                    tab: id(1),
                    pane: id(pane),
                    target: Some(id(11)),
                    side: Direction::Down,
                    split: id(1000 + pane),
                })
                .unwrap();
        }
        let expected: BTreeSet<_> = (10..18).map(id).collect();
        for step in 0..250u128 {
            rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1);
            let panes: Vec<_> = model.state().panes().into_iter().collect();
            let pane = panes[(rng as usize / 17) % panes.len()];
            let target = panes[(rng as usize / 113) % panes.len()];
            let side = [
                Direction::Left,
                Direction::Right,
                Direction::Up,
                Direction::Down,
            ][(rng as usize / 7) % 4];
            let mut splits = Vec::new();
            for tab in model.state().tabs.values() {
                if let Some(node) = &tab.root {
                    split_ids(node, &mut splits);
                }
            }
            let split = splits[(rng as usize / 3) % splits.len()];
            let command = match rng % 7 {
                0 => Command::Move {
                    pane,
                    target,
                    side,
                    split: id(10_000 + step),
                },
                1 => Command::Swap {
                    first: pane,
                    second: target,
                },
                2 => Command::Rotate { split },
                3 => Command::Equalize { split },
                4 => Command::ResizeBoundary {
                    split,
                    boundary: 0,
                    fraction: 0.1 + (rng % 80) as f64 / 100.0,
                },
                5 => {
                    let to_tab = if model.state().pane_tab(pane) == Some(id(1)) {
                        id(2)
                    } else {
                        id(1)
                    };
                    Command::Transfer {
                        pane,
                        to_tab,
                        target: model.state().tabs[&to_tab].panes().first().copied(),
                        side,
                        split: id(10_000 + step),
                    }
                }
                _ => Command::ToggleFocusMode {
                    tab: model.state().pane_tab(pane).unwrap(),
                },
            };
            let before = model.state().clone();
            match model.apply(command) {
                Ok(true) => {
                    let after = model.state().clone();
                    assert!(model.undo());
                    assert_eq!(&before, model.state());
                    assert!(model.redo());
                    assert_eq!(&after, model.state());
                }
                Ok(false) | Err(_) => assert_eq!(&before, model.state()),
            }
            model.state().validate().unwrap();
            assert_eq!(model.state().panes(), expected);
            for tab in [id(1), id(2)] {
                for pane in geometry(model.state(), tab, 20.0, 20.0).panes {
                    assert!(pane.rect.width >= 80.0 - 1e-7);
                    assert!(pane.rect.height >= 50.0 - 1e-7);
                }
            }
        }
    }
}

#[test]
fn history_roundtrip_preserves_undo_redo_across_reopen() {
    let mut model = model();
    for _ in 0..95 {
        model.apply(Command::Rotate { split: id(101) }).unwrap();
    }
    for _ in 0..50 {
        model.undo();
    }
    model.apply(Command::Focus { pane: id(10) }).unwrap();
    let saved = serde_json::to_vec(&model.export_history()).unwrap();
    let mut restored = LayoutModel::from_history(serde_json::from_slice(&saved).unwrap()).unwrap();
    assert_eq!(restored.state(), model.state());
    assert_eq!(restored.undo_len(), model.undo_len());
    assert_eq!(restored.redo_len(), 50);
    for _ in 0..50 {
        assert!(model.redo());
        assert!(restored.redo());
        assert_eq!(restored.state(), model.state());
    }
    for _ in 0..100 {
        assert!(restored.undo());
        restored.state().validate().unwrap();
    }
    assert!(!restored.undo());
}

#[test]
fn history_import_rejects_schema_overflow_and_broken_chain() {
    let history = serde_json::to_value(model().export_history()).unwrap();
    let mut bad = history.clone();
    bad["schema"] = serde_json::json!(2);
    assert!(LayoutModel::from_history(serde_json::from_value(bad).unwrap()).is_err());
    let mut bad = history.clone();
    bad["undo"][1]["before"] = serde_json::to_value(LayoutState::default()).unwrap();
    assert!(LayoutModel::from_history(serde_json::from_value(bad).unwrap()).is_err());
    let mut bad = history.clone();
    let step = bad["undo"][0].clone();
    bad["undo"] = serde_json::Value::Array(vec![step; 101]);
    assert!(LayoutModel::from_history(serde_json::from_value(bad).unwrap()).is_err());
    let mut bad = history;
    bad["current"]["tabs"][id(1).to_string()]["focused"] = serde_json::json!(id(999));
    assert!(LayoutModel::from_history(serde_json::from_value(bad).unwrap()).is_err());
}

#[test]
fn focus_clicks_do_not_consume_geometry_history_or_discard_redo() {
    let mut model = model();
    let slots = model.undo_len();
    for pane in [id(10), id(11), id(12), id(10)] {
        model.apply(Command::Focus { pane }).unwrap();
    }
    assert_eq!(model.undo_len(), slots);
    model.undo();
    let redos = model.redo_len();
    model.apply(Command::Focus { pane: id(11) }).unwrap();
    assert_eq!(model.redo_len(), redos);
    let mut restored = LayoutModel::from_history(model.export_history()).unwrap();
    assert!(restored.redo());
    assert!(model.redo());
    assert_eq!(restored.state(), model.state());
}

#[test]
fn catalog_tabs_rebase_history_without_consuming_or_erasing_undo() {
    let mut model = model();
    model.undo();
    let slots = (model.undo_len(), model.redo_len());
    model.add_tab_metadata(id(999)).unwrap();
    assert_eq!((model.undo_len(), model.redo_len()), slots);
    while model.undo() {
        assert_eq!(
            model.state().tabs.get(&id(999)),
            Some(&TabLayout::default())
        );
    }
    while model.redo() {
        assert_eq!(
            model.state().tabs.get(&id(999)),
            Some(&TabLayout::default())
        );
    }
    let before = model.export_history();
    assert!(model.add_tab_metadata(id(999)).is_err());
    assert_eq!(model.export_history(), before);
    LayoutModel::from_history(model.export_history()).unwrap();
}

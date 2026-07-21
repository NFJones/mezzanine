//! Unit tests for window layout behavior and pane sizing invariants.

use super::{
    IdFactory, LayoutPolicy, Pane, PaneGeometry, PaneId, PaneNavigationDirection, PaneSizeSpec,
    PaneTitleSource, ResizeAxis, ResizeDirection, Size, SplitDirection, Window, WindowId,
    new_window_pane_size, range_overlap_u16,
};

/// Verifies new-window pane sizing resolves absolute, percentage, and
/// directional requests before the product creates a process-backed window.
///
/// This keeps the arithmetic in the mux domain while allowing the product
/// adapter to apply one already-validated size as part of its rollback-aware
/// window and PTY creation flow.
#[test]
fn new_window_pane_size_resolves_every_request_shape() {
    let window_size = Size::new(80, 24).unwrap();

    assert_eq!(
        new_window_pane_size(
            window_size,
            PaneSizeSpec::Cells {
                columns: Some(60),
                rows: None,
            },
        )
        .unwrap(),
        Size::new(60, 24).unwrap()
    );
    assert_eq!(
        new_window_pane_size(
            window_size,
            PaneSizeSpec::Percent {
                percent: 50,
                axis: ResizeAxis::Both,
            },
        )
        .unwrap(),
        Size::new(40, 12).unwrap()
    );
    assert_eq!(
        new_window_pane_size(
            window_size,
            PaneSizeSpec::Edge {
                edge: ResizeDirection::Left,
                amount: 10,
            },
        )
        .unwrap(),
        Size::new(70, 24).unwrap()
    );
}

/// Verifies new-window pane sizing rejects zero and out-of-window requests
/// before the product mutates session state or starts a PTY.
///
/// The failure cases protect the lower-owned validation boundary that replaced
/// the product's duplicate percentage and directional arithmetic.
#[test]
fn new_window_pane_size_rejects_invalid_requests_without_effects() {
    let window_size = Size::new(80, 24).unwrap();

    let zero_percent = new_window_pane_size(
        window_size,
        PaneSizeSpec::Percent {
            percent: 0,
            axis: ResizeAxis::Columns,
        },
    )
    .unwrap_err();
    assert_eq!(zero_percent.kind(), crate::MuxErrorKind::InvalidArgs);

    let outside_window = new_window_pane_size(
        window_size,
        PaneSizeSpec::Percent {
            percent: 101,
            axis: ResizeAxis::Columns,
        },
    )
    .unwrap_err();
    assert_eq!(outside_window.kind(), crate::MuxErrorKind::InvalidArgs);

    let zero_direction = new_window_pane_size(
        window_size,
        PaneSizeSpec::Delta {
            direction: ResizeDirection::Down,
            amount: 0,
        },
    )
    .unwrap_err();
    assert_eq!(zero_direction.kind(), crate::MuxErrorKind::InvalidArgs);
}

/// Verifies half-open range overlap uses terminal-cell geometry semantics.
///
/// Pane targeting and divider rendering share this helper so touching endpoints,
/// empty ranges, and reversed ranges must all produce zero overlap instead of
/// an off-by-one shared-cell result.
#[test]
fn range_overlap_u16_uses_half_open_ranges() {
    assert_eq!(range_overlap_u16(0, 5, 3, 8), 2);
    assert_eq!(range_overlap_u16(0, 5, 5, 8), 0);
    assert_eq!(range_overlap_u16(5, 5, 0, 8), 0);
    assert_eq!(range_overlap_u16(9, 2, 0, 8), 0);
}
/// Verifies first pane occupies window size.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn first_pane_occupies_window_size() {
    let mut ids = IdFactory::default();
    let size = Size::new(120, 40).unwrap();
    let window = Window::new(&mut ids, 0, "main", size);

    assert_eq!(window.panes().len(), 1);
    assert_eq!(window.active_pane().size, size);
}

/// Verifies even layouts rebalance when panes are added and removed.
///
/// A window in an even layout mode is asking the layout engine to keep all panes
/// uniformly apportioned. Splitting or closing panes must therefore rebuild the
/// flat layout immediately instead of preserving the old active-pane split
/// proportions used by tiled layouts.
#[test]
fn even_layout_rebalances_after_pane_count_changes() {
    let mut ids = IdFactory::default();
    let size = Size::new(100, 20).unwrap();
    let mut window = Window::new(&mut ids, 0, "main", size);
    window.set_layout_policy(LayoutPolicy::EvenVertical);

    window
        .split_active_select(&mut ids, SplitDirection::Vertical, true)
        .unwrap();
    assert_eq!(
        window
            .panes()
            .iter()
            .map(|pane| pane.size.columns)
            .collect::<Vec<_>>(),
        vec![50, 50]
    );

    window
        .split_active_select(&mut ids, SplitDirection::Vertical, true)
        .unwrap();
    let widths = window
        .panes()
        .iter()
        .map(|pane| pane.size.columns)
        .collect::<Vec<_>>();
    assert_eq!(widths.iter().sum::<u16>(), 100);
    assert!(widths.iter().max().unwrap() - widths.iter().min().unwrap() <= 1);

    let removed_id = window.panes()[1].id.to_string();
    window.kill_pane(Some(&removed_id)).unwrap();
    assert_eq!(
        window
            .panes()
            .iter()
            .map(|pane| pane.size.columns)
            .collect::<Vec<_>>(),
        vec![50, 50]
    );
}

/// Verifies even grid layout uses both rows and columns.
///
/// Subagent bucket windows rely on this policy to keep several background
/// panes readable in a single window. Four panes in an 80x24 window should
/// become a stable 2x2 grid, and resizing the window should preserve that grid
/// shape while reapportioning each pane evenly.
#[test]
fn even_grid_layout_rebalances_panes_across_rows_and_columns() {
    let mut ids = IdFactory::default();
    let mut window = Window::new(&mut ids, 0, "main", Size::new(80, 24).unwrap());
    for _ in 0..3 {
        window
            .split_active_select(&mut ids, SplitDirection::Vertical, true)
            .unwrap();
    }

    window.set_layout_policy(LayoutPolicy::EvenGrid);

    assert_eq!(window.layout_policy(), LayoutPolicy::EvenGrid);
    assert_eq!(
        window.pane_geometries(),
        vec![
            PaneGeometry {
                index: 0,
                column: 0,
                row: 0,
                columns: 40,
                rows: 12,
            },
            PaneGeometry {
                index: 1,
                column: 40,
                row: 0,
                columns: 40,
                rows: 12,
            },
            PaneGeometry {
                index: 2,
                column: 0,
                row: 12,
                columns: 40,
                rows: 12,
            },
            PaneGeometry {
                index: 3,
                column: 40,
                row: 12,
                columns: 40,
                rows: 12,
            },
        ]
    );

    window.resize_window(Size::new(100, 30).unwrap()).unwrap();

    assert_eq!(
        window
            .panes()
            .iter()
            .map(|pane| pane.size)
            .collect::<Vec<_>>(),
        vec![Size::new(50, 15).unwrap(); 4]
    );
}

/// Verifies vertical split halves columns.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn vertical_split_halves_columns() {
    let mut ids = IdFactory::default();
    let mut window = Window::new(&mut ids, 0, "main", Size::new(121, 40).unwrap());

    window
        .split_active(&mut ids, SplitDirection::Vertical)
        .unwrap();

    assert_eq!(window.panes()[0].size, Size::new(61, 40).unwrap());
    assert_eq!(window.panes()[1].size, Size::new(60, 40).unwrap());
    assert_eq!(window.active_pane().index, 1);
}

/// Verifies horizontal split halves rows.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn horizontal_split_halves_rows() {
    let mut ids = IdFactory::default();
    let mut window = Window::new(&mut ids, 0, "main", Size::new(120, 41).unwrap());

    window
        .split_active(&mut ids, SplitDirection::Horizontal)
        .unwrap();

    assert_eq!(window.panes()[0].size, Size::new(120, 21).unwrap());
    assert_eq!(window.panes()[1].size, Size::new(120, 20).unwrap());
    assert_eq!(window.active_pane().index, 1);
}

/// Verifies that a requested split size is applied by shrinking the spawning
/// pane and assigning the requested cells to the created pane. This guards the
/// pane-creation path against producing overlapping rectangles when a size is
/// supplied for the new pane.
#[test]
fn split_with_size_spec_rebalances_sibling_without_overlap() {
    let mut ids = IdFactory::default();
    let mut window = Window::new(&mut ids, 0, "main", Size::new(80, 24).unwrap());

    window
        .split_active_with_size_spec(
            &mut ids,
            SplitDirection::Vertical,
            PaneSizeSpec::Cells {
                columns: Some(20),
                rows: None,
            },
        )
        .unwrap();

    assert_eq!(window.panes()[0].size, Size::new(60, 24).unwrap());
    assert_eq!(window.panes()[1].size, Size::new(20, 24).unwrap());
    assert_eq!(
        window.pane_geometries(),
        vec![
            PaneGeometry {
                index: 0,
                column: 0,
                row: 0,
                columns: 60,
                rows: 24,
            },
            PaneGeometry {
                index: 1,
                column: 60,
                row: 0,
                columns: 20,
                rows: 24,
            },
        ],
    );
}

/// Verifies that pane creation rejects a requested split size that would change
/// the cross-axis dimension. Accepting that shape would leave unused space or
/// overlap another pane because the split tree has no place to put the extra
/// rows or columns.
#[test]
fn split_with_size_spec_rejects_cross_axis_conflict_without_mutation() {
    let mut ids = IdFactory::default();
    let mut window = Window::new(&mut ids, 0, "main", Size::new(80, 24).unwrap());

    let error = window
        .split_active_with_size_spec(
            &mut ids,
            SplitDirection::Vertical,
            PaneSizeSpec::Cells {
                columns: Some(20),
                rows: Some(10),
            },
        )
        .unwrap_err();

    assert_eq!(error.kind(), crate::MuxErrorKind::InvalidArgs);
    assert_eq!(window.panes().len(), 1);
    assert_eq!(
        window.pane_geometries(),
        vec![PaneGeometry {
            index: 0,
            column: 0,
            row: 0,
            columns: 80,
            rows: 24,
        }]
    );
}

/// Verifies select pane accepts id or index.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn select_pane_accepts_id_or_index() {
    let mut ids = IdFactory::default();
    let mut window = Window::new(&mut ids, 0, "main", Size::new(120, 40).unwrap());
    let pane_id = window
        .split_active(&mut ids, SplitDirection::Vertical)
        .unwrap()
        .id
        .to_string();

    window.select_pane(&pane_id).unwrap();
    assert_eq!(window.active_pane().id.to_string(), pane_id);

    window.select_pane("0").unwrap();
    assert_eq!(window.active_pane().index, 0);
}

/// Verifies killing pane removes it and keeps one active.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn killing_pane_removes_it_and_keeps_one_active() {
    let mut ids = IdFactory::default();
    let mut window = Window::new(&mut ids, 0, "main", Size::new(120, 40).unwrap());
    window
        .split_active(&mut ids, SplitDirection::Vertical)
        .unwrap();

    let removed = window.kill_pane(None).unwrap();

    assert_eq!(removed.index, 1);
    assert_eq!(window.panes().len(), 1);
    assert!(window.active_pane().active);
}

/// Verifies closing the focused pane returns to the newest surviving pane in
/// that window rather than selecting a structural neighbor.
#[test]
fn killing_active_pane_uses_local_mru_focus_history() {
    let mut ids = IdFactory::default();
    let mut window = Window::new(&mut ids, 0, "main", Size::new(120, 40).unwrap());
    window
        .split_active(&mut ids, SplitDirection::Vertical)
        .unwrap();
    window
        .split_active(&mut ids, SplitDirection::Horizontal)
        .unwrap();
    let first = window.panes()[0].id.clone();
    let second = window.panes()[1].id.clone();
    let third = window.panes()[2].id.clone();

    window.select_pane(first.as_str()).unwrap();
    window.select_pane(second.as_str()).unwrap();
    window.select_pane(third.as_str()).unwrap();
    window.kill_pane(Some(third.as_str())).unwrap();

    assert_eq!(window.active_pane().id, second);
}

/// Verifies that killing an inactive target pane preserves focus on the current
/// active pane. This matches default mux behavior where targeted pane closure
/// should not unexpectedly move the user's focus unless the active pane itself
/// was closed.
#[test]
fn killing_inactive_pane_preserves_active_pane_focus() {
    let mut ids = IdFactory::default();
    let mut window = Window::new(&mut ids, 0, "main", Size::new(120, 40).unwrap());
    window
        .split_active(&mut ids, SplitDirection::Vertical)
        .unwrap();
    let active_before = window.active_pane().id.clone();

    let removed = window.kill_pane(Some("0")).unwrap();

    assert_eq!(removed.index, 0);
    assert_eq!(window.active_pane().id, active_before);
    assert_eq!(window.active_pane().index, 0);
}

/// Verifies pane focus history retains only the ten newest stable identities.
#[test]
fn killing_active_pane_uses_bounded_local_focus_history() {
    let mut ids = IdFactory::default();
    let mut window = Window::new(&mut ids, 0, "main", Size::new(60_000, 40).unwrap());
    let oldest = window.active_pane().id.clone();
    for _ in 0..11 {
        window
            .split_active(&mut ids, SplitDirection::Vertical)
            .unwrap();
    }

    assert_eq!(window.pane_focus_history.len(), 10);
    assert!(!window.pane_focus_history.contains(&oldest));
}

/// Verifies that closing a pane collapses its split-tree slot and resizes the
/// remaining adjacent pane to occupy the released space. This prevents stale
/// gaps after closing one pane in a nested layout.
#[test]
fn killing_nested_pane_reflows_remaining_tree_without_gap() {
    let mut ids = IdFactory::default();
    let mut window = Window::new(&mut ids, 0, "main", Size::new(90, 30).unwrap());
    window
        .split_active(&mut ids, SplitDirection::Vertical)
        .unwrap();
    window
        .split_active(&mut ids, SplitDirection::Horizontal)
        .unwrap();

    let removed = window.kill_pane(Some("1")).unwrap();

    assert_eq!(removed.index, 1);
    assert_eq!(window.panes().len(), 2);
    assert_eq!(
        window.pane_geometries(),
        vec![
            PaneGeometry {
                index: 0,
                column: 0,
                row: 0,
                columns: 45,
                rows: 30,
            },
            PaneGeometry {
                index: 1,
                column: 45,
                row: 0,
                columns: 45,
                rows: 30,
            },
        ]
    );
}

/// Verifies that closing the middle pane in a stacked split only gives the
/// released cells to an adjacent pane in that stack. This mirrors a user dragging
/// the vertical border between a full-height left pane and three right-hand
/// panes, then closing the sandwiched right-hand pane: the manually dragged
/// left/right border must remain in place instead of being normalized back to an
/// even split across the whole window.
#[test]
fn killing_middle_stacked_pane_preserves_manual_cross_axis_resize() {
    let mut ids = IdFactory::default();
    let mut window = Window::new(&mut ids, 0, "main", Size::new(120, 30).unwrap());
    window
        .split_active(&mut ids, SplitDirection::Vertical)
        .unwrap();
    window
        .split_active(&mut ids, SplitDirection::Horizontal)
        .unwrap();
    window
        .split_active(&mut ids, SplitDirection::Horizontal)
        .unwrap();
    window
        .replace_pane_geometries(vec![
            PaneGeometry {
                index: 0,
                column: 0,
                row: 0,
                columns: 40,
                rows: 30,
            },
            PaneGeometry {
                index: 1,
                column: 40,
                row: 0,
                columns: 80,
                rows: 15,
            },
            PaneGeometry {
                index: 2,
                column: 40,
                row: 15,
                columns: 80,
                rows: 8,
            },
            PaneGeometry {
                index: 3,
                column: 40,
                row: 23,
                columns: 80,
                rows: 7,
            },
        ])
        .unwrap();

    let removed = window.kill_pane(Some("2")).unwrap();

    assert_eq!(removed.index, 2);
    assert_eq!(
        window.pane_geometries(),
        vec![
            PaneGeometry {
                index: 0,
                column: 0,
                row: 0,
                columns: 40,
                rows: 30,
            },
            PaneGeometry {
                index: 1,
                column: 40,
                row: 0,
                columns: 80,
                rows: 15,
            },
            PaneGeometry {
                index: 2,
                column: 40,
                row: 15,
                columns: 80,
                rows: 15,
            },
        ]
    );
}

/// Verifies swapping panes exchanges identity without changing slots.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn swapping_panes_exchanges_identity_without_changing_slots() {
    let mut ids = IdFactory::default();
    let mut window = Window::new(&mut ids, 0, "main", Size::new(121, 40).unwrap());
    window
        .split_active(&mut ids, SplitDirection::Vertical)
        .unwrap();
    let first_id = window.panes()[0].id.clone();
    let second_id = window.panes()[1].id.clone();

    window.swap_panes(None, "1").unwrap();

    assert_eq!(window.panes()[0].id, first_id);
    assert_eq!(window.panes()[0].size, Size::new(61, 40).unwrap());
    assert!(!window.panes()[0].active);
    assert_eq!(window.panes()[1].id, second_id);
    assert_eq!(window.panes()[1].size, Size::new(60, 40).unwrap());
    assert!(window.panes()[1].active);
}

/// Verifies moved pane can be inserted after existing pane.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn moved_pane_can_be_inserted_after_existing_pane() {
    let mut ids = IdFactory::default();
    let mut window = Window::new(&mut ids, 0, "main", Size::new(80, 24).unwrap());
    let moved_id = window
        .split_active(&mut ids, SplitDirection::Vertical)
        .unwrap()
        .id
        .clone();

    let moved = window.take_pane(Some(moved_id.as_str())).unwrap();
    window
        .insert_existing_after(None, moved, SplitDirection::Horizontal, true)
        .unwrap();

    assert_eq!(window.panes().len(), 2);
    assert_eq!(window.active_pane().id, moved_id);
    assert_eq!(window.panes()[1].id, moved_id);
}

/// Verifies restored window keeps saved identity and active pane.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn restored_window_keeps_saved_identity_and_active_pane() {
    let panes = vec![
        Pane {
            id: PaneId::parse('%', "%8").unwrap(),
            index: 4,
            title: "left".to_string(),
            title_source: PaneTitleSource::Explicit,
            size: Size::new(40, 24).unwrap(),
            active: false,
            live: false,
        },
        Pane {
            id: PaneId::parse('%', "%9").unwrap(),
            index: 5,
            title: "right".to_string(),
            title_source: PaneTitleSource::Explicit,
            size: Size::new(40, 24).unwrap(),
            active: true,
            live: false,
        },
    ];

    let window = Window::from_restored_parts(
        WindowId::parse('@', "@3").unwrap(),
        0,
        "restored",
        Size::new(80, 24).unwrap(),
        panes,
    )
    .unwrap();

    assert_eq!(window.id.as_str(), "@3");
    assert_eq!(window.active_pane().id.as_str(), "%9");
    assert_eq!(window.panes()[0].index, 0);
    assert_eq!(window.panes()[1].index, 1);
}

/// Verifies pane navigation zoom rotation and layout cycle are deterministic.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn pane_navigation_zoom_rotation_and_layout_cycle_are_deterministic() {
    let mut ids = IdFactory::default();
    let mut window = Window::new(&mut ids, 0, "main", Size::new(90, 30).unwrap());
    window
        .split_active(&mut ids, SplitDirection::Vertical)
        .unwrap();
    window
        .split_active(&mut ids, SplitDirection::Horizontal)
        .unwrap();
    let original = window
        .panes()
        .iter()
        .map(|pane| pane.id.clone())
        .collect::<Vec<_>>();

    window
        .select_adjacent_pane(PaneNavigationDirection::Down)
        .unwrap();
    assert_eq!(window.last_active_pane_index(), Some(2));
    window.select_last_pane().unwrap();
    assert_eq!(window.active_pane().index, 2);

    let zoomed = window.toggle_zoom_active().cloned().unwrap();
    assert_eq!(window.zoomed_pane_id(), Some(&zoomed));
    assert!(window.toggle_zoom_active().is_none());

    window.rotate_panes(false);
    assert_eq!(window.panes()[0].id, original[1]);
    assert_eq!(window.panes()[1].id, original[2]);
    assert_eq!(window.panes()[2].id, original[0]);

    assert_eq!(window.cycle_layout(), LayoutPolicy::EvenVertical);
    assert_eq!(
        window
            .panes()
            .iter()
            .map(|pane| pane.size.columns)
            .collect::<Vec<_>>(),
        vec![30, 30, 30]
    );
    assert_eq!(window.cycle_layout(), LayoutPolicy::EvenHorizontal);
    assert_eq!(
        window
            .panes()
            .iter()
            .map(|pane| pane.size.rows)
            .collect::<Vec<_>>(),
        vec![10, 10, 10]
    );
}

/// Verifies that directional pane navigation follows stored pane rectangles for
/// an irregular tiled layout while preserving mux-like backtracking and wrap
/// behavior. The right-hand pane is split horizontally, so moving down reaches
/// the lower-right pane, moving left returns to the full-height pane the focus
/// came from, and moving left from that edge wraps to the first pane on the
/// opposite side.
#[test]
fn pane_navigation_uses_stored_geometry_with_backtracking_and_wrapping() {
    let mut ids = IdFactory::default();
    let mut window = Window::new(&mut ids, 0, "main", Size::new(90, 30).unwrap());
    window
        .split_active(&mut ids, SplitDirection::Vertical)
        .unwrap();
    window
        .split_active(&mut ids, SplitDirection::Horizontal)
        .unwrap();
    let ids = window
        .panes()
        .iter()
        .map(|pane| pane.id.clone())
        .collect::<Vec<_>>();

    window
        .select_adjacent_pane(PaneNavigationDirection::Down)
        .unwrap();
    assert_eq!(window.active_pane().id, ids[1]);

    window
        .select_adjacent_pane(PaneNavigationDirection::Left)
        .unwrap();
    assert_eq!(window.active_pane().id, ids[0]);

    window
        .select_adjacent_pane(PaneNavigationDirection::Right)
        .unwrap();
    assert_eq!(window.active_pane().id, ids[1]);

    window
        .select_adjacent_pane(PaneNavigationDirection::Left)
        .unwrap();
    assert_eq!(window.active_pane().id, ids[0]);

    window
        .select_adjacent_pane(PaneNavigationDirection::Left)
        .unwrap();
    assert_eq!(window.active_pane().id, ids[1]);

    window
        .select_adjacent_pane(PaneNavigationDirection::Right)
        .unwrap();
    assert_eq!(window.active_pane().id, ids[0]);
}

/// Verifies vertical navigation from a full-height pane does not wrap into a
/// neighboring over/under split pair with no horizontal overlap.
#[test]
fn pane_navigation_full_height_pane_does_not_vertically_wrap_to_side_stack() {
    let mut ids = IdFactory::default();
    let mut window = Window::new(&mut ids, 0, "main", Size::new(90, 30).unwrap());
    window
        .split_active(&mut ids, SplitDirection::Vertical)
        .unwrap();
    window
        .split_active(&mut ids, SplitDirection::Horizontal)
        .unwrap();
    let full_height = window.panes()[0].id.clone();

    window.select_pane(full_height.as_str()).unwrap();
    window
        .select_adjacent_pane(PaneNavigationDirection::Down)
        .unwrap();
    assert_eq!(window.active_pane().id, full_height);

    window
        .select_adjacent_pane(PaneNavigationDirection::Up)
        .unwrap();
    assert_eq!(window.active_pane().id, full_height);

    window
        .select_adjacent_pane(PaneNavigationDirection::Right)
        .unwrap();
    assert_ne!(window.active_pane().id, full_height);
}

/// Verifies that wrapping from one edge to the opposite edge does not trap focus
/// between the outermost panes. After a left-edge wrap to the rightmost pane,
/// moving left again must choose the nearest internal pane instead of blindly
/// backtracking to the previous outer edge pane.
#[test]
fn pane_navigation_after_wrap_can_move_back_to_internal_panes() {
    let mut ids = IdFactory::default();
    let mut window = Window::new(&mut ids, 0, "main", Size::new(90, 30).unwrap());
    window
        .split_active(&mut ids, SplitDirection::Vertical)
        .unwrap();
    window
        .split_active(&mut ids, SplitDirection::Vertical)
        .unwrap();
    let mut panes_by_column = window
        .pane_geometries()
        .into_iter()
        .map(|geometry| (geometry.column, window.panes()[geometry.index].id.clone()))
        .collect::<Vec<_>>();
    panes_by_column.sort_by_key(|(column, _)| *column);
    let left = panes_by_column[0].1.clone();
    let middle = panes_by_column[1].1.clone();
    let right = panes_by_column[2].1.clone();

    window.select_pane(left.as_str()).unwrap();
    window
        .select_adjacent_pane(PaneNavigationDirection::Left)
        .unwrap();
    assert_eq!(window.active_pane().id, right);

    window
        .select_adjacent_pane(PaneNavigationDirection::Left)
        .unwrap();
    assert_eq!(window.active_pane().id, middle);
}

/// Verifies that pane rectangles are stored as window state after mutation and
/// are accepted from restored snapshot metadata. This guards the conformance
/// path where snapshots and protocol output must read the same authoritative
/// rectangle state instead of deriving a separate rectangle view per caller.
#[test]
fn pane_geometry_is_stored_after_split_and_snapshot_restore() {
    let mut ids = IdFactory::default();
    let mut window = Window::new(&mut ids, 0, "main", Size::new(121, 40).unwrap());
    window
        .split_active(&mut ids, SplitDirection::Vertical)
        .unwrap();

    assert_eq!(
        window.pane_geometries(),
        vec![
            PaneGeometry {
                index: 0,
                column: 0,
                row: 0,
                columns: 61,
                rows: 40,
            },
            PaneGeometry {
                index: 1,
                column: 61,
                row: 0,
                columns: 60,
                rows: 40,
            },
        ],
    );

    let panes = vec![
        Pane {
            id: PaneId::parse('%', "%8").unwrap(),
            index: 0,
            title: "left".to_string(),
            title_source: PaneTitleSource::Explicit,
            size: Size::new(30, 24).unwrap(),
            active: true,
            live: false,
        },
        Pane {
            id: PaneId::parse('%', "%9").unwrap(),
            index: 1,
            title: "right".to_string(),
            title_source: PaneTitleSource::Explicit,
            size: Size::new(50, 24).unwrap(),
            active: false,
            live: false,
        },
    ];
    let restored_geometries = vec![
        PaneGeometry {
            index: 0,
            column: 0,
            row: 0,
            columns: 30,
            rows: 24,
        },
        PaneGeometry {
            index: 1,
            column: 30,
            row: 0,
            columns: 50,
            rows: 24,
        },
    ];

    let restored = Window::from_restored_parts_with_geometries(
        WindowId::parse('@', "@4").unwrap(),
        0,
        "restored",
        Size::new(80, 24).unwrap(),
        panes,
        Some(restored_geometries.clone()),
        LayoutPolicy::EvenVertical,
    )
    .unwrap();

    assert_eq!(restored.pane_geometries(), restored_geometries);
    assert_eq!(restored.layout_policy(), LayoutPolicy::EvenVertical);
}

/// Verifies that restored pane rectangles remain a validated layout surface
/// instead of unchecked snapshot metadata. Overlapping rectangles would make
/// focus, rendering, and resize behavior ambiguous, so restore rejects that
/// payload shape before constructing a window.
#[test]
fn restored_window_rejects_overlapping_stored_geometry() {
    let panes = vec![
        Pane {
            id: PaneId::parse('%', "%8").unwrap(),
            index: 0,
            title: "left".to_string(),
            title_source: PaneTitleSource::Explicit,
            size: Size::new(40, 24).unwrap(),
            active: true,
            live: false,
        },
        Pane {
            id: PaneId::parse('%', "%9").unwrap(),
            index: 1,
            title: "right".to_string(),
            title_source: PaneTitleSource::Explicit,
            size: Size::new(40, 24).unwrap(),
            active: false,
            live: false,
        },
    ];
    let geometries = vec![
        PaneGeometry {
            index: 0,
            column: 0,
            row: 0,
            columns: 40,
            rows: 24,
        },
        PaneGeometry {
            index: 1,
            column: 20,
            row: 0,
            columns: 40,
            rows: 24,
        },
    ];

    let error = Window::from_restored_parts_with_geometries(
        WindowId::parse('@', "@4").unwrap(),
        0,
        "restored",
        Size::new(80, 24).unwrap(),
        panes,
        Some(geometries),
        LayoutPolicy::Tiled,
    )
    .unwrap_err();

    assert_eq!(error.kind(), crate::MuxErrorKind::InvalidArgs);
}

mod alignment {
    use crate::css_pixels::CssPixels;
    use crate::formatting_context::grid::alignment::*;

    fn px(value: i64) -> CssPixels {
        CssPixels::from_integer(value)
    }

    #[test]
    fn auto_margins_absorb_positive_space_before_self_alignment() {
        let aligned = align_item(
            px(40),
            false,
            false,
            px(100),
            px(0),
            px(0),
            px(0),
            px(0),
            true,
            true,
            Alignment::End,
        );
        assert_eq!(aligned.margin_start, px(30));
        assert_eq!(aligned.margin_end, px(30));
        assert_eq!(aligned.size, px(40));
    }

    #[test]
    fn overflowing_center_alignment_preserves_negative_free_space() {
        let aligned = align_item(
            px(120),
            false,
            false,
            px(100),
            px(0),
            px(0),
            px(0),
            px(0),
            false,
            false,
            Alignment::Center,
        );
        assert_eq!(aligned.margin_start, px(-10));
        assert_eq!(aligned.margin_end, px(-10));
    }

    #[test]
    fn content_distribution_uses_cpp_gap_denominators() {
        assert_eq!(
            distributed_gap_size(Alignment::SpaceBetween, px(300), px(180), 2, px(10)),
            px(60)
        );
        assert_eq!(
            distributed_gap_size(Alignment::SpaceAround, px(300), px(180), 2, px(10)),
            px(40)
        );
        assert_eq!(
            distributed_gap_size(Alignment::SpaceEvenly, px(300), px(180), 2, px(10)),
            px(30)
        );
    }
}

mod facts {
    use crate::formatting_context::grid::facts::*;

    #[test]
    fn ffi_grid_fact_types_are_plain_c_abi_values() {
        assert_eq!(NO_GRID_INDEX, u32::MAX);
        assert!(size_of::<FfiGridTrackEntry>() >= size_of::<FfiGridTrackBreadth>() * 3);
        assert_eq!(FfiGridPlacementKind::Auto as u8, 0);
        assert_eq!(FfiGridPlacementKind::Line as u8, 1);
        assert_eq!(FfiGridPlacementKind::Span as u8, 2);
    }
}

mod placement {
    use crate::formatting_context::grid::facts::{FfiGridPlacement, FfiGridPlacementKind, NO_GRID_INDEX};
    use crate::formatting_context::grid::placement::*;
    use crate::formatting_context::grid::template::LineName;

    fn place_items(
        items: &[PlacementInput],
        explicit_column_count: usize,
        explicit_row_count: usize,
        flow: AutoFlowAxis,
        dense: bool,
    ) -> Vec<PlacedItem> {
        place_items_with_grid(items, explicit_column_count, explicit_row_count, flow, dense).items
    }

    fn auto(id: usize, column_span: usize) -> PlacementInput {
        PlacementInput {
            id,
            order: 0,
            row: ResolvedAxisPlacement { start: None, span: 1 },
            column: ResolvedAxisPlacement {
                start: None,
                span: column_span,
            },
        }
    }

    fn by_id(items: Vec<PlacedItem>) -> Vec<PlacedItem> {
        let mut items = items;
        items.sort_by_key(|item| item.id);
        items
    }

    fn auto_placement() -> FfiGridPlacement {
        FfiGridPlacement::default()
    }

    fn line(number: Option<i32>, name: Option<(u32, u32, u32)>) -> FfiGridPlacement {
        FfiGridPlacement {
            kind: FfiGridPlacementKind::Line as u8,
            has_line_number: number.is_some(),
            line_number: number.unwrap_or_default(),
            has_name: name.is_some(),
            name_index: name.map(|indices| indices.0).unwrap_or(NO_GRID_INDEX),
            implicit_start_name_index: name.map(|indices| indices.1).unwrap_or(NO_GRID_INDEX),
            implicit_end_name_index: name.map(|indices| indices.2).unwrap_or(NO_GRID_INDEX),
        }
    }

    fn span(count: i32) -> FfiGridPlacement {
        FfiGridPlacement {
            kind: FfiGridPlacementKind::Span as u8,
            has_line_number: true,
            line_number: count,
            ..FfiGridPlacement::default()
        }
    }

    fn explicit_name(index: u32) -> LineName {
        LineName {
            name_index: index,
            raw: index as usize,
            implicit: false,
            adopted_from_parent: false,
            area_name_raw: 0,
            area_is_start: false,
        }
    }

    #[test]
    fn dense_packing_backfills_a_hole_while_sparse_keeps_cursor_order() {
        let inputs = [auto(0, 2), auto(1, 2), auto(2, 1)];
        let sparse = by_id(place_items(&inputs, 3, 1, AutoFlowAxis::Row, false));
        let dense = by_id(place_items(&inputs, 3, 1, AutoFlowAxis::Row, true));

        assert_eq!((sparse[0].row, sparse[0].column), (0, 0));
        assert_eq!((sparse[1].row, sparse[1].column), (1, 0));
        assert_eq!((sparse[2].row, sparse[2].column), (1, 2));
        assert_eq!((dense[2].row, dense[2].column), (0, 2));
    }

    #[test]
    fn order_buckets_keep_document_order_within_equal_orders() {
        let mut first = auto(0, 1);
        first.order = 2;
        let mut second = auto(1, 1);
        second.order = -1;
        let mut third = auto(2, 1);
        third.order = -1;
        let placed = place_items(&[first, second, third], 3, 1, AutoFlowAxis::Row, false);
        assert_eq!(placed.iter().map(|item| item.id).collect::<Vec<_>>(), vec![1, 2, 0]);
    }

    #[test]
    fn column_flow_advances_rows_before_columns() {
        let placed = by_id(place_items(
            &[auto(0, 1), auto(1, 1), auto(2, 1)],
            1,
            2,
            AutoFlowAxis::Column,
            false,
        ));
        assert_eq!((placed[0].row, placed[0].column), (0, 0));
        assert_eq!((placed[1].row, placed[1].column), (1, 0));
        assert_eq!((placed[2].row, placed[2].column), (0, 1));
    }

    #[test]
    fn positive_and_negative_numeric_lines_resolve_against_explicit_grid() {
        let lines = vec![Vec::new(); 5];
        let resolved = resolve_placement_position(line(Some(-3), None), line(Some(-1), None), &[], &lines, 5, 4);
        assert_eq!(
            resolved,
            ResolvedPlacementPosition {
                start: 2,
                end: 4,
                span: 2
            }
        );
    }

    #[test]
    fn area_aliases_win_over_bare_named_lines() {
        let lines = vec![
            vec![explicit_name(1)],
            vec![explicit_name(2)],
            vec![explicit_name(3)],
            vec![explicit_name(1)],
        ];
        let resolved = resolve_placement_position(
            line(None, Some((1, 2, 3))),
            line(None, Some((1, 2, 3))),
            &[0, 1, 2, 3],
            &lines,
            4,
            3,
        );
        assert_eq!(
            resolved,
            ResolvedPlacementPosition {
                start: 1,
                end: 2,
                span: 1
            }
        );
    }

    #[test]
    fn span_from_end_and_two_span_rule_match_cpp() {
        let lines = vec![Vec::new(); 5];
        let from_end = resolve_placement_position(span(2), line(Some(4), None), &[], &lines, 5, 4);
        assert_eq!(
            from_end,
            ResolvedPlacementPosition {
                start: 1,
                end: 3,
                span: 2
            }
        );
        let two_spans = resolve_placement_position(span(3), span(7), &[], &lines, 5, 4);
        assert_eq!(two_spans.span, 3);
    }

    #[test]
    fn automatic_subgrid_span_is_floored_at_one() {
        assert_eq!(resolve_placement_span(auto_placement(), auto_placement(), Some(0)), 1);
        assert_eq!(resolve_placement_span(auto_placement(), auto_placement(), Some(4)), 4);
    }
}

mod sizing {
    use crate::css_pixels::CssPixels;
    use crate::formatting_context::grid::sizing::*;
    use crate::formatting_context::grid::tracks::{Track, TrackSizingFunction, initialize_track_sizes};
    use crate::geometry::AvailableSize;

    fn px(value: i64) -> CssPixels {
        CssPixels::from_integer(value)
    }

    fn flexible_track(base_size: CssPixels, flex_factor: f64) -> Track {
        Track {
            min_sizing: TrackSizingFunction::Auto,
            max_sizing: TrackSizingFunction::Flex(flex_factor),
            base_size,
            growth_limit: None,
            flex_factor: Some(flex_factor),
            max_is_intrinsic: false,
            max_is_max_content: false,
            base_size_frozen: false,
            growth_limit_frozen: false,
            infinitely_growable: false,
            planned_increase: CssPixels::default(),
            item_incurred_increase: CssPixels::default(),
            is_gap: false,
            is_auto_fit: false,
            is_auto_repeat: false,
            is_collapsed: false,
        }
    }

    fn apply_base_size_increases(tracks: &mut [Track], planned_increases: &[CssPixels]) {
        assert_eq!(tracks.len(), planned_increases.len());
        for (track, increase) in tracks.iter_mut().zip(planned_increases) {
            track.base_size += *increase;
            if track
                .growth_limit
                .is_some_and(|growth_limit| growth_limit < track.base_size)
            {
                track.growth_limit = Some(track.base_size);
            }
        }
    }

    #[test]
    fn spanning_item_distributes_in_span_order_and_grows_past_limits() {
        let mut tracks = [
            Track {
                min_sizing: TrackSizingFunction::Auto,
                max_sizing: TrackSizingFunction::Fixed(crate::style_facts::FfiSizeValue::auto_value()),
                base_size: px(50),
                growth_limit: Some(px(100)),
                flex_factor: None,
                max_is_intrinsic: false,
                max_is_max_content: false,
                base_size_frozen: false,
                growth_limit_frozen: false,
                infinitely_growable: false,
                planned_increase: px(0),
                item_incurred_increase: px(0),
                is_gap: false,
                is_auto_fit: false,
                is_auto_repeat: false,
                is_collapsed: false,
            },
            Track {
                min_sizing: TrackSizingFunction::Auto,
                max_sizing: TrackSizingFunction::MaxContent,
                base_size: px(50),
                growth_limit: None,
                flex_factor: None,
                max_is_intrinsic: true,
                max_is_max_content: false,
                base_size_frozen: false,
                growth_limit_frozen: false,
                infinitely_growable: false,
                planned_increase: px(0),
                item_incurred_increase: px(0),
                is_gap: false,
                is_auto_fit: false,
                is_auto_repeat: false,
                is_collapsed: false,
            },
        ];
        let increases =
            distribute_spanning_base_size(&mut tracks, &[true, true], px(300), SpaceDistributionPhase::Minimum);
        assert_eq!(increases, vec![px(50), px(150)]);
        apply_base_size_increases(&mut tracks, &increases);
        assert_eq!(tracks[0].base_size, px(100));
        assert_eq!(tracks[1].base_size, px(200));
    }

    #[test]
    fn unaffected_tracks_still_reduce_the_remaining_contribution() {
        let mut tracks = [
            Track::fixed(px(80)),
            Track {
                min_sizing: TrackSizingFunction::Auto,
                max_sizing: TrackSizingFunction::MaxContent,
                base_size: px(20),
                growth_limit: None,
                flex_factor: None,
                max_is_intrinsic: true,
                max_is_max_content: false,
                base_size_frozen: false,
                growth_limit_frozen: false,
                infinitely_growable: false,
                planned_increase: px(0),
                item_incurred_increase: px(0),
                is_gap: false,
                is_auto_fit: false,
                is_auto_repeat: false,
                is_collapsed: false,
            },
        ];
        let increases =
            distribute_spanning_base_size(&mut tracks, &[false, true], px(160), SpaceDistributionPhase::MinContent);
        assert_eq!(increases, vec![px(0), px(60)]);
    }

    #[test]
    fn full_order_sizes_spanning_intrinsic_tracks_before_maximize_and_stretch() {
        let mut tracks = [Track::auto(), Track::auto()];
        let item = ItemContribution {
            spanned_tracks: vec![0, 1],
            span: 2,
            minimum: px(120),
            min_content: px(120),
            limited_min_content: px(120),
            max_content: px(160),
            limited_max_content: px(160),
            is_scroll_container: false,
        };
        run_track_sizing(
            &mut tracks,
            px(10),
            &[item],
            AvailableSize::definite(px(210)),
            || None,
            false,
            true,
        );
        assert_eq!(tracks[0].base_size, px(100));
        assert_eq!(tracks[1].base_size, px(100));
    }

    #[test]
    fn indefinite_fr_fraction_uses_spanning_item_max_content() {
        let mut tracks = [flexible_track(px(0), 1.0), flexible_track(px(0), 2.0)];
        let item = ItemContribution {
            spanned_tracks: vec![0, 1],
            span: 2,
            minimum: px(0),
            min_content: px(0),
            limited_min_content: px(0),
            max_content: px(300),
            limited_max_content: px(300),
            is_scroll_container: false,
        };
        initialize_track_sizes(&mut tracks, AvailableSize::indefinite());
        expand_flexible_tracks_indefinite(&mut tracks, &[item]);
        assert_eq!(tracks[0].base_size, px(100));
        assert_eq!(tracks[1].base_size, px(200));
    }

    #[test]
    fn percentage_fit_content_becomes_max_content_when_indefinite() {
        use crate::formatting_context::grid::facts::{FfiGridTrackBreadth, FfiGridTrackBreadthKind};
        use crate::formatting_context::grid::template::TrackDefinition;
        use crate::style_facts::{FfiSizeKind, FfiSizeValue};

        let percentage = FfiSizeValue {
            kind: FfiSizeKind::Percentage as u8,
            px: px(0),
            fraction: 0.5,
            calc: std::ptr::null(),
            contains_percentage: true,
            contains_anchor_function: false,
        };
        let auto = FfiGridTrackBreadth {
            kind: FfiGridTrackBreadthKind::Auto as u8,
            value: FfiSizeValue::auto_value(),
            flex_factor: 0.0,
        };
        let mut tracks = [Track::from_definition(TrackDefinition {
            min: auto,
            max: FfiGridTrackBreadth {
                kind: FfiGridTrackBreadthKind::FitContent as u8,
                value: percentage,
                flex_factor: 0.0,
            },
            is_auto_fit: false,
            is_auto_repeat: false,
        })];
        initialize_track_sizes(&mut tracks, AvailableSize::indefinite());
        assert!(tracks[0].max_sizing.is_max_content());
    }
}

mod template {
    use crate::formatting_context::grid::facts::{
        FfiGridArea, FfiGridTrackBreadth, FfiGridTrackBreadthKind, FfiGridTrackEntry, FfiGridTrackEntryKind,
        FfiGridTrackList, NO_GRID_INDEX,
    };
    use crate::formatting_context::grid::template::*;
    use crate::style_facts::{FfiSizeKind, FfiSizeValue};

    fn breadth(kind: FfiGridTrackBreadthKind) -> FfiGridTrackBreadth {
        FfiGridTrackBreadth {
            kind: kind as u8,
            value: FfiSizeValue {
                kind: FfiSizeKind::Auto as u8,
                px: Default::default(),
                fraction: 0.0,
                calc: std::ptr::null(),
                contains_percentage: false,
                contains_anchor_function: false,
            },
            flex_factor: 0.0,
        }
    }

    fn entry(kind: FfiGridTrackEntryKind, next: u32) -> FfiGridTrackEntry {
        FfiGridTrackEntry {
            kind: kind as u8,
            next_sibling: next,
            name_index_start: 0,
            name_index_count: 0,
            size: breadth(FfiGridTrackBreadthKind::Auto),
            min_size: breadth(FfiGridTrackBreadthKind::Auto),
            max_size: breadth(FfiGridTrackBreadthKind::Auto),
            repeat_type: REPEAT_FIXED,
            repeat_count: 0,
            repeat_list: FfiGridTrackList::default(),
        }
    }

    #[test]
    fn recursive_repeat_preserves_boundary_line_name_order() {
        let mut entries = vec![
            entry(FfiGridTrackEntryKind::LineNames, 1),
            entry(FfiGridTrackEntryKind::Repeat, 2),
            entry(FfiGridTrackEntryKind::LineNames, NO_GRID_INDEX),
            entry(FfiGridTrackEntryKind::LineNames, 4),
            entry(FfiGridTrackEntryKind::TrackSize, 5),
            entry(FfiGridTrackEntryKind::LineNames, NO_GRID_INDEX),
        ];
        entries[0].name_index_count = 1;
        entries[1].repeat_count = 2;
        entries[1].repeat_list.first_entry = 3;
        entries[2].name_index_start = 3;
        entries[2].name_index_count = 1;
        entries[3].name_index_start = 1;
        entries[3].name_index_count = 1;
        entries[5].name_index_start = 2;
        entries[5].name_index_count = 1;
        let source = TrackListSource {
            names: &(0..=40).collect::<Vec<_>>(),
            entries: &entries,
            name_indices: &[10, 20, 30, 40],
        };
        let expanded = expand_standalone(
            source,
            FfiGridTrackList {
                first_entry: 0,
                ..Default::default()
            },
            |_index, _entry| 1,
        );

        assert_eq!(expanded.tracks.len(), 2);
        assert_eq!(
            expanded
                .lines
                .iter()
                .map(|line| line.iter().map(|name| name.name_index).collect::<Vec<_>>())
                .collect::<Vec<_>>(),
            vec![vec![10, 20], vec![30, 20], vec![30, 40]]
        );
    }

    #[test]
    fn auto_repeat_marks_every_expanded_track() {
        let mut entries = vec![
            entry(FfiGridTrackEntryKind::Repeat, NO_GRID_INDEX),
            entry(FfiGridTrackEntryKind::TrackSize, NO_GRID_INDEX),
        ];
        entries[0].repeat_type = REPEAT_AUTO_FIT;
        entries[0].repeat_list.first_entry = 1;
        let expanded = expand_standalone(
            TrackListSource {
                names: &[],
                entries: &entries,
                name_indices: &[],
            },
            FfiGridTrackList {
                first_entry: 0,
                ..Default::default()
            },
            |_index, _entry| 3,
        );
        assert_eq!(expanded.tracks.len(), 3);
        assert!(
            expanded
                .tracks
                .iter()
                .all(|track| track.is_auto_fit && track.is_auto_repeat)
        );
    }

    #[test]
    fn template_areas_grow_axes_and_add_preinterned_names() {
        let mut columns = vec![Vec::new()];
        let mut rows = vec![Vec::new()];
        add_template_area_lines(
            &mut columns,
            &mut rows,
            &[FfiGridArea {
                name_index: 7,
                implicit_start_name_index: 8,
                implicit_end_name_index: 9,
                row_start: 1,
                row_end: 3,
                column_start: 2,
                column_end: 4,
            }],
            &(0..=9).collect::<Vec<_>>(),
        );
        assert_eq!(columns.len(), 5);
        assert_eq!(rows.len(), 4);
        assert_eq!(columns[2][0], LineName::implicit(8, 8, 7, true));
        assert_eq!(columns[4][0], LineName::implicit(9, 9, 7, false));
        assert_eq!(rows[1][0], LineName::implicit(8, 8, 7, true));
        assert_eq!(rows[3][0], LineName::implicit(9, 9, 7, false));
    }

    #[test]
    fn named_line_counting_matches_cpp_positive_and_negative_order() {
        let lines = vec![
            vec![LineName::explicit(1, 1)],
            vec![LineName::explicit(2, 2)],
            vec![LineName::explicit(1, 1)],
            vec![LineName::explicit(1, 1)],
        ];
        assert_eq!(nth_named_line(&lines, 1, 1), Some(0));
        assert_eq!(nth_named_line(&lines, 1, 2), Some(2));
        // The current C++ code converts -1 to lines.len() - 1 before
        // counting matching names from the start, so three matches are not
        // enough. Preserve that behavior for the port.
        assert_eq!(nth_named_line(&lines, 1, -1), None);
    }
}

mod tracks {
    use crate::css_pixels::CssPixels;
    use crate::formatting_context::grid::tracks::*;

    fn px(value: i64) -> CssPixels {
        CssPixels::from_integer(value)
    }

    fn flexible_track(base_size: CssPixels, flex_factor: f64) -> Track {
        Track {
            min_sizing: TrackSizingFunction::Auto,
            max_sizing: TrackSizingFunction::Flex(flex_factor),
            base_size,
            growth_limit: None,
            flex_factor: Some(flex_factor),
            max_is_intrinsic: false,
            max_is_max_content: false,
            base_size_frozen: false,
            growth_limit_frozen: false,
            infinitely_growable: false,
            planned_increase: CssPixels::default(),
            item_incurred_increase: CssPixels::default(),
            is_gap: false,
            is_auto_fit: false,
            is_auto_repeat: false,
            is_collapsed: false,
        }
    }

    #[test]
    fn fr_distribution_respects_fixed_tracks_and_factor_ratios() {
        let mut tracks = [
            Track::fixed(px(100)),
            flexible_track(px(0), 1.0),
            flexible_track(px(0), 2.0),
        ];
        expand_flexible_tracks(&mut tracks, px(400));
        assert_eq!(tracks[0].base_size, px(100));
        assert_eq!(tracks[1].base_size, px(100));
        assert_eq!(tracks[2].base_size, px(200));
    }

    #[test]
    fn fr_distribution_restarts_with_base_size_violations_inflexible() {
        let mut tracks = [
            Track::fixed(px(100)),
            flexible_track(px(150), 1.0),
            flexible_track(px(0), 2.0),
        ];
        expand_flexible_tracks(&mut tracks, px(400));
        assert_eq!(tracks[1].base_size, px(150));
        assert_eq!(tracks[2].base_size, px(150));
    }

    #[test]
    fn sub_one_flex_sum_leaves_the_remaining_fraction_unfilled() {
        let mut tracks = [flexible_track(px(0), 0.5)];
        expand_flexible_tracks(&mut tracks, px(100));
        assert_eq!(tracks[0].base_size, px(50));
    }
}

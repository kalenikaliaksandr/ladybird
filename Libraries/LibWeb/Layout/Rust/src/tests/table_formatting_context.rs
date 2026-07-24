mod borders {
    use crate::css_pixels::CssPixels;
    use crate::formatting_context::FfiBorderData;
    use crate::formatting_context::table::borders::*;

    fn border(width: i32, style: u8, color: u32) -> FfiBorderData {
        FfiBorderData {
            width: CssPixels::from_raw(width),
            line_style: style,
            color,
        }
    }

    #[test]
    fn conflict_priority_matches_width_style_and_hidden_rules() {
        assert!(border_is_less_specific(
            border(64, LINE_STYLE_SOLID, 1),
            border(128, LINE_STYLE_DOTTED, 2),
        ));
        assert!(border_is_less_specific(
            border(64, LINE_STYLE_DASHED, 1),
            border(64, LINE_STYLE_SOLID, 2),
        ));
        assert!(border_is_less_specific(
            border(1024, LINE_STYLE_SOLID, 1),
            border(0, LINE_STYLE_HIDDEN, 2),
        ));
        assert!(!border_is_less_specific(
            border(0, LINE_STYLE_HIDDEN, 1),
            border(1024, LINE_STYLE_DOUBLE, 2),
        ));
    }

    #[test]
    fn spanning_cell_hides_crossing_row_and_column_segments() {
        let visible = ElementBorders {
            top: border(64, LINE_STYLE_SOLID, 0xff00_0000),
            right: border(64, LINE_STYLE_SOLID, 0xff00_0000),
            bottom: border(64, LINE_STYLE_SOLID, 0xff00_0000),
            left: border(64, LINE_STYLE_SOLID, 0xff00_0000),
        };
        let own = ElementBorders::default();
        let mut grid = CollapsedBorderGrid::new(2, 2);
        grid.hide_segments_inside_span(0, 2, 0, 2);
        grid.apply_borders(visible, 0, 1, 0, 2, ELEMENT_ROW);
        let result = grid.resolve_for_cell(0, 2, 0, 2, own);
        assert_eq!(result.top.border_data.line_style, LINE_STYLE_SOLID);
        assert_eq!(result.bottom.border_data.line_style, LINE_STYLE_NONE);
    }
}

mod distribution {
    use crate::css_pixels::CssPixels;
    use crate::formatting_context::table::distribution::*;

    fn px(value: i64) -> CssPixels {
        CssPixels::from_integer(value)
    }

    #[test]
    fn interpolates_between_consecutive_width_guesses() {
        let mut columns = [
            Column {
                min_size: px(10),
                max_size: px(30),
                is_constrained: true,
                has_originating_cells: true,
                ..Column::default()
            },
            Column {
                min_size: px(20),
                max_size: px(40),
                is_constrained: true,
                has_originating_cells: true,
                ..Column::default()
            },
        ];
        distribute_inline_size(&mut columns, px(50), false);
        assert_eq!(columns[0].used_inline_size, px(20));
        assert_eq!(columns[1].used_inline_size, px(30));
    }

    #[test]
    fn fixed_mode_gives_unspecified_columns_equal_excess() {
        let mut columns = [
            Column {
                min_size: px(20),
                max_size: px(20),
                is_constrained: true,
                ..Column::default()
            },
            Column::default(),
            Column::default(),
        ];
        distribute_inline_size(&mut columns, px(100), true);
        assert_eq!(columns[0].used_inline_size, px(20));
        assert_eq!(columns[1].used_inline_size, px(40));
        assert_eq!(columns[2].used_inline_size, px(40));
    }

    #[test]
    fn percentage_guess_precedes_max_content_growth() {
        let mut columns = [
            Column {
                min_size: px(10),
                max_size: px(80),
                has_intrinsic_percentage: true,
                intrinsic_percentage: 50.0,
                has_originating_cells: true,
                ..Column::default()
            },
            Column {
                min_size: px(10),
                max_size: px(80),
                has_originating_cells: true,
                ..Column::default()
            },
        ];
        distribute_inline_size(&mut columns, px(100), false);
        assert_eq!(columns[0].used_inline_size, px(50));
        assert_eq!(columns[1].used_inline_size, px(50));
    }
}

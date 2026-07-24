mod ellipsis {
    use crate::css_enums::{direction, white_space_collapse, writing_mode};
    use crate::css_pixels::CssPixels;
    use crate::formatting_context::inline::ellipsis::*;
    use crate::formatting_context::inline::fragment::{FragmentBuildFacts, GLYPH_TEXT_TYPE_LTR, GlyphData};
    use crate::formatting_context::inline::iterator::text::FfiDrawGlyph;
    use crate::formatting_context::inline::line_box::LineBoxData;
    use std::ffi::c_void;

    struct Font;
    impl EllipsisFontProvider for Font {
        fn font_glyph_width(&mut self, _font: *const c_void, _code_point: u32) -> f32 {
            2.0
        }

        fn font_glyph_id(&mut self, _font: *const c_void, _code_point: u32) -> u32 {
            42
        }
    }

    #[test]
    fn ellipsis_truncates_glyphs_and_marks_later_fragments() {
        let text = [b'a' as u16, b'b' as u16, b'c' as u16];
        let facts = FragmentBuildFacts {
            style_source: std::ptr::dangling_mut::<c_void>(),
            is_atomic_inline: false,
            white_space_collapse: white_space_collapse::COLLAPSE,
            letter_spacing: CssPixels::default(),
            first_available_font: std::ptr::dangling::<c_void>(),
            text_utf16: text.as_ptr(),
            text_length_in_code_units: text.len(),
            style_block_axis_is_reverse: false,
        };
        let mut line = LineBoxData::new(direction::LTR, writing_mode::HORIZONTAL_TB);
        line.original_available_inline_size = crate::geometry::AvailableSize::definite(CssPixels::from_integer(5));
        line.add_fragment(
            std::ptr::dangling_mut::<c_void>(),
            0,
            3,
            CssPixels::default(),
            CssPixels::default(),
            CssPixels::default(),
            CssPixels::default(),
            CssPixels::from_integer(9),
            CssPixels::from_integer(10),
            CssPixels::default(),
            CssPixels::default(),
            Some(GlyphData {
                glyphs: [0.0, 3.0, 6.0]
                    .map(|x| FfiDrawGlyph {
                        x,
                        glyph_width: 3.0,
                        should_paint: true,
                        ..Default::default()
                    })
                    .into(),
                font: std::ptr::dangling::<c_void>(),
                text_type: GLYPH_TEXT_TYPE_LTR,
                width: 9.0,
            }),
            facts,
            false,
        );
        line.add_fragment(
            2usize as *mut c_void,
            0,
            0,
            CssPixels::default(),
            CssPixels::default(),
            CssPixels::default(),
            CssPixels::default(),
            CssPixels::from_integer(1),
            CssPixels::from_integer(10),
            CssPixels::default(),
            CssPixels::default(),
            None,
            facts,
            false,
        );
        apply(std::slice::from_mut(&mut line), &mut Font);
        assert_eq!(line.fragments[0].glyphs.as_ref().unwrap().glyphs.len(), 2);
        assert_eq!(line.fragments[0].glyphs.as_ref().unwrap().glyphs[1].glyph_id, 42);
        assert!(line.fragments[1].is_fully_truncated);
        assert_eq!(line.inline_length, CssPixels::from_integer(5));
    }
}

mod justification {
    use crate::css_enums::{direction, text_justify, white_space_collapse, writing_mode};
    use crate::css_pixels::CssPixels;
    use crate::formatting_context::inline::fragment::FragmentBuildFacts;
    use crate::formatting_context::inline::justification::*;
    use crate::formatting_context::inline::line_box::LineBoxData;
    use std::ffi::c_void;

    fn facts(text: &[u16]) -> FragmentBuildFacts {
        FragmentBuildFacts {
            style_source: std::ptr::dangling_mut::<c_void>(),
            is_atomic_inline: false,
            white_space_collapse: white_space_collapse::COLLAPSE,
            letter_spacing: CssPixels::default(),
            first_available_font: std::ptr::dangling::<c_void>(),
            text_utf16: text.as_ptr(),
            text_length_in_code_units: text.len(),
            style_block_axis_is_reverse: false,
        }
    }

    #[test]
    fn only_single_space_fragments_expand_and_markers_follow_shifted_offsets() {
        let text = [b'a' as u16, b' ' as u16, b' ' as u16];
        let mut line = LineBoxData::new(direction::LTR, writing_mode::HORIZONTAL_TB);
        for (start, length, width) in [(0, 1, 2), (1, 1, 1), (2, 1, 1)] {
            line.add_fragment(
                std::ptr::dangling_mut::<c_void>(),
                start,
                length,
                CssPixels::default(),
                CssPixels::default(),
                CssPixels::default(),
                CssPixels::default(),
                CssPixels::from_integer(width),
                CssPixels::from_integer(10),
                CssPixels::default(),
                CssPixels::default(),
                None,
                facts(&text),
                true,
            );
        }
        line.original_available_inline_size = crate::geometry::AvailableSize::definite(CssPixels::from_integer(10));
        line.add_static_position_marker(2usize as *mut c_void, false);
        apply_to_fragments(text_justify::AUTO, &mut line, false);
        assert_eq!(line.fragments[1].inline_length, CssPixels::from_integer(4));
        assert_eq!(line.fragments[2].inline_offset, CssPixels::from_integer(6));
        assert_eq!(
            line.static_position_markers[0].inline_offset,
            CssPixels::from_integer(10)
        );
    }
}

mod pieces {
    use crate::formatting_context::inline::pieces::*;

    #[test]
    fn pieces_emit_in_line_depth_discovery_order() {
        let piece = |line_index, depth, discovery_index| StagedPiece {
            piece: InlineBoxPieceData {
                node: std::ptr::null_mut(),
                first_fragment_index: 0,
                fragment_count: 0,
                border_box_rect: CssPixelRect::default(),
                present_edges: 0,
                is_geometry_only_placeholder: false,
            },
            line_index,
            depth,
            discovery_index,
        };
        let mut pieces = [piece(2, 1, 0), piece(1, 2, 1), piece(1, 1, 2), piece(1, 1, 0)];
        sort_for_emission(&mut pieces);
        let keys: Vec<_> = pieces
            .iter()
            .map(|piece| (piece.line_index, piece.depth, piece.discovery_index))
            .collect();
        assert_eq!(keys, [(1, 1, 0), (1, 1, 2), (1, 2, 1), (2, 1, 0)]);
    }
}

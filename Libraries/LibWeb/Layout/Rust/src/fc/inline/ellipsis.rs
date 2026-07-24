/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

use super::line_box::LineBoxData;
use super::text::FfiDrawGlyph;
use crate::css_pixels::CssPixels;
use std::ffi::c_void;

const ELLIPSIS_CODE_POINT: u32 = 0x2026;

pub(crate) trait EllipsisFontProvider {
    fn font_glyph_width(&mut self, font: *const c_void, code_point: u32) -> f32;
    fn font_glyph_id(&mut self, font: *const c_void, code_point: u32) -> u32;
}

pub(crate) fn apply(line_boxes: &mut [LineBoxData], provider: &mut impl EllipsisFontProvider) {
    for line in line_boxes {
        if !line.original_available_inline_size.is_definite() {
            continue;
        }
        let available_inline_size = line.original_available_inline_size.to_px_or_zero();
        if line.inline_length <= available_inline_size || line.fragments.is_empty() {
            continue;
        }

        let mut line_has_visible_content = false;
        for index in 0..line.fragments.len() {
            let fragment_start = line.fragments[index].inline_offset;
            let fragment_end = fragment_start + line.fragments[index].inline_length;
            if fragment_end <= available_inline_size {
                line_has_visible_content = true;
                continue;
            }
            let Some(glyph_data) = &line.fragments[index].glyphs else {
                continue;
            };
            let font = glyph_data.font;
            let ellipsis_inline_size = provider.font_glyph_width(font, ELLIPSIS_CODE_POINT);
            let available_in_fragment = (available_inline_size - fragment_start).raw_value() as f32 / 64.0;
            let max_text_inline_size = available_in_fragment - ellipsis_inline_size;

            let glyphs = &line.fragments[index].glyphs.as_ref().unwrap().glyphs;
            let mut keep_count = 0usize;
            let mut last_kept_end = 0.0f32;
            let mut glyph_block_offset = 0.0f32;
            for glyph in glyphs {
                let glyph_end = glyph.x + glyph.glyph_width;
                if glyph_end > max_text_inline_size && (keep_count > 0 || line_has_visible_content) {
                    break;
                }
                keep_count += 1;
                last_kept_end = glyph_end;
                glyph_block_offset = glyph.y;
            }

            let glyph_data = line.fragments[index].glyphs.as_mut().unwrap();
            glyph_data.glyphs.truncate(keep_count);
            glyph_data.glyphs.push(FfiDrawGlyph {
                x: last_kept_end,
                y: glyph_block_offset,
                length_in_code_units: 1,
                glyph_width: ellipsis_inline_size,
                glyph_id: provider.font_glyph_id(font, ELLIPSIS_CODE_POINT),
                should_paint: true,
            });
            line.fragments[index].inline_length =
                CssPixels::nearest_value_for_f32(last_kept_end + ellipsis_inline_size);
            for later in &mut line.fragments[index + 1..] {
                later.is_fully_truncated = true;
            }
            line.inline_length = available_inline_size;
            line.clamp_static_position_markers_to_inline_length();
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::css_enums::{direction, white_space_collapse, writing_mode};
    use crate::fc::inline::fragment::{FragmentBuildFacts, GLYPH_TEXT_TYPE_LTR, GlyphData};

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

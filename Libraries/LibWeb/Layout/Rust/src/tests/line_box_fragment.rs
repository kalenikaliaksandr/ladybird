use crate::css_enums::{direction, writing_mode};
use crate::css_pixels::CssPixels;
use crate::formatting_context::inline::fragment::*;
use crate::formatting_context::inline::iterator::text::FfiDrawGlyph;
use std::ffi::c_void;

fn glyph(x: f32) -> FfiDrawGlyph {
    FfiDrawGlyph {
        x,
        should_paint: true,
        ..Default::default()
    }
}

fn fragment(direction: u8, text_type: u8, glyphs: &[f32]) -> LineBoxFragmentData {
    LineBoxFragmentData::new(
        std::ptr::dangling_mut::<c_void>(),
        0,
        1,
        CssPixels::default(),
        CssPixels::default(),
        CssPixels::from_integer(10),
        CssPixels::from_integer(10),
        CssPixels::default(),
        direction,
        writing_mode::HORIZONTAL_TB,
        Some(GlyphData {
            glyphs: glyphs.iter().copied().map(glyph).collect(),
            font: std::ptr::dangling::<c_void>(),
            text_type,
            width: 10.0,
        }),
        FragmentBuildFacts {
            style_source: std::ptr::dangling_mut::<c_void>(),
            is_atomic_inline: false,
            white_space_collapse: 0,
            letter_spacing: CssPixels::default(),
            first_available_font: std::ptr::dangling::<c_void>(),
            text_utf16: std::ptr::null(),
            text_length_in_code_units: 0,
            style_block_axis_is_reverse: false,
        },
    )
}

fn run(text_type: u8, glyphs: &[f32]) -> GlyphData {
    GlyphData {
        glyphs: glyphs.iter().copied().map(glyph).collect(),
        font: std::ptr::dangling::<c_void>(),
        text_type,
        width: 3.0,
    }
}

#[test]
fn ltr_fragment_switches_between_ltr_and_rtl_insertions() {
    let mut fragment = fragment(direction::LTR, GLYPH_TEXT_TYPE_LTR, &[0.0, 4.0]);
    fragment.append_glyph_run(run(GLYPH_TEXT_TYPE_RTL, &[0.0, 2.0]), CssPixels::from_integer(3));
    fragment.append_glyph_run(run(GLYPH_TEXT_TYPE_LTR, &[0.0]), CssPixels::from_integer(2));
    let positions: Vec<_> = fragment
        .glyphs
        .unwrap()
        .glyphs
        .into_iter()
        .map(|glyph| glyph.x)
        .collect();
    assert_eq!(positions, [0.0, 4.0, 10.0, 12.0, 13.0]);
}

#[test]
fn rtl_end_padding_does_not_translate_existing_glyphs() {
    let mut fragment = fragment(direction::RTL, GLYPH_TEXT_TYPE_RTL, &[1.0]);
    fragment.append_glyph_run(run(GLYPH_TEXT_TYPE_END_PADDING, &[0.0]), CssPixels::from_integer(2));
    let positions: Vec<_> = fragment
        .glyphs
        .unwrap()
        .glyphs
        .into_iter()
        .map(|glyph| glyph.x)
        .collect();
    assert_eq!(positions, [1.0, 0.0]);
}

use crate::css_enums::{direction, white_space_collapse, writing_mode};
use crate::css_pixels::CssPixels;
use crate::formatting_context::inline::fragment::FragmentBuildFacts;
use crate::formatting_context::inline::line_box::*;
use std::ffi::c_void;

#[derive(Default)]
struct FakeTextProvider {
    cursor_node: *mut c_void,
}

impl LineBoxTextProvider for FakeTextProvider {
    fn document_cursor_is_on_node(&mut self, node: *mut c_void) -> bool {
        node == self.cursor_node
    }

    fn font_glyph_width(&mut self, _font: *const c_void, _code_point: u32) -> f32 {
        5.0
    }
}

fn facts(text: &[u16]) -> FragmentBuildFacts {
    FragmentBuildFacts {
        style_source: std::ptr::dangling_mut::<c_void>(),
        is_atomic_inline: false,
        white_space_collapse: white_space_collapse::COLLAPSE,
        letter_spacing: CssPixels::from_integer(1),
        first_available_font: std::ptr::dangling::<c_void>(),
        text_utf16: text.as_ptr(),
        text_length_in_code_units: text.len(),
        style_block_axis_is_reverse: false,
    }
}

#[test]
fn trimming_removes_whitespace_and_clamps_markers_each_step() {
    let text = [b'a' as u16, b' ' as u16, b' ' as u16];
    let mut line = LineBoxData::new(direction::LTR, writing_mode::HORIZONTAL_TB);
    line.add_fragment(
        std::ptr::dangling_mut::<c_void>(),
        0,
        3,
        CssPixels::default(),
        CssPixels::default(),
        CssPixels::default(),
        CssPixels::default(),
        CssPixels::from_integer(13),
        CssPixels::from_integer(10),
        CssPixels::default(),
        CssPixels::default(),
        None,
        facts(&text),
        false,
    );
    line.add_static_position_marker(2usize as *mut c_void, false);
    line.trim_trailing_whitespace(&mut FakeTextProvider::default());
    assert_eq!(line.fragments[0].length_in_code_units, 1);
    assert_eq!(line.inline_length, CssPixels::from_integer(1));
    assert_eq!(
        line.static_position_markers[0].inline_offset,
        CssPixels::from_integer(1)
    );
    assert!(line.fragments[0].has_trailing_whitespace);
}

#[test]
fn cursor_on_fragment_pins_trailing_whitespace() {
    let text = [b' ' as u16];
    let mut line = LineBoxData::new(direction::LTR, writing_mode::HORIZONTAL_TB);
    line.add_fragment(
        3usize as *mut c_void,
        0,
        1,
        CssPixels::default(),
        CssPixels::default(),
        CssPixels::default(),
        CssPixels::default(),
        CssPixels::from_integer(5),
        CssPixels::from_integer(10),
        CssPixels::default(),
        CssPixels::default(),
        None,
        facts(&text),
        false,
    );
    let mut provider = FakeTextProvider {
        cursor_node: 3usize as *mut c_void,
    };
    line.trim_trailing_whitespace(&mut provider);
    assert_eq!(line.fragments.len(), 1);
}

/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

use super::line_box::LineBoxData;
use crate::css_enums::text_justify;
use crate::css_pixels::CssPixels;

pub(crate) fn apply_to_fragments(text_justify: u8, line: &mut LineBoxData, is_last_line: bool) {
    if text_justify == text_justify::NONE || is_last_line || line.has_forced_break {
        return;
    }
    assert!(matches!(
        text_justify,
        text_justify::AUTO | text_justify::INTER_WORD | text_justify::INTER_CHARACTER
    ));

    let excess_inline_space = line.original_available_inline_size.to_px_or_zero() - line.inline_length;
    let mut excess_inline_space_including_whitespace = excess_inline_space;
    let mut whitespace_count = 0usize;
    for fragment in &line.fragments {
        if fragment.is_justifiable_whitespace() {
            whitespace_count += 1;
            excess_inline_space_including_whitespace += fragment.inline_length;
        }
    }
    let justified_space_inline_size = if whitespace_count > 0 {
        excess_inline_space_including_whitespace / whitespace_count
    } else {
        CssPixels::default()
    };

    let mut running_diff = CssPixels::default();
    for fragment in &mut line.fragments {
        fragment.inline_offset += running_diff;
        if fragment.is_justifiable_whitespace() && fragment.inline_length != justified_space_inline_size {
            let diff = justified_space_inline_size - fragment.inline_length;
            running_diff += diff;
            for marker in &mut line.static_position_markers {
                // This intentionally compares against the fragment's already
                // shifted offset, matching the C++ ordering.
                if marker.inline_offset > fragment.inline_offset {
                    marker.inline_offset += diff;
                }
            }
            fragment.inline_length = justified_space_inline_size;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::css_enums::{direction, white_space_collapse, writing_mode};
    use crate::fc::inline::fragment::FragmentBuildFacts;
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

/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

use crate::css::css_enums::{text_transform, white_space_collapse};
use crate::css::serialize::for_each_code_point_utf16;
use crate::layout::RenderedTextBoundary;
use std::ffi::c_void;

use super::{code_point_is_ascii_space, code_unit_length_for_code_point};

#[derive(Clone, Copy)]
#[repr(C)]
struct UnicodeLayoutTextMappingEdit {
    source_start: usize,
    source_length: usize,
    destination_start: usize,
    destination_length: usize,
}

type UnicodeLayoutTextMappingSink = unsafe extern "C" fn(
    context: *mut c_void,
    ascii_text: *const u8,
    utf16_text: *const u16,
    length_in_code_units: usize,
    edits: *const UnicodeLayoutTextMappingEdit,
    edit_count: usize,
);

unsafe extern "C" {
    fn unicode_layout_apply_case_mapping(
        text: *const u16,
        length_in_code_units: usize,
        mapping: u8,
        ascii_locale: *const u8,
        locale_length: usize,
        preserve_existing_trailing_code_points: bool,
        context: *mut c_void,
        sink: UnicodeLayoutTextMappingSink,
    );
    fn unicode_layout_apply_fullwidth_mapping(
        text: *const u16,
        length_in_code_units: usize,
        context: *mut c_void,
        sink: UnicodeLayoutTextMappingSink,
    );
    fn unicode_layout_may_require_bidi_processing(text: *const u16, length_in_code_units: usize) -> bool;
}

const CASE_MAPPING_LOWERCASE: u8 = 0;
const CASE_MAPPING_UPPERCASE: u8 = 1;
const CASE_MAPPING_TITLECASE: u8 = 2;

const PASSWORD_MASK_CODE_UNIT: u16 = 0x25cf;

pub(crate) struct TextSource {
    pub(crate) text: Vec<u16>,
    pub(crate) dom_start_offset: usize,
    pub(crate) dom_length: usize,
    pub(crate) is_password_input: bool,
    pub(crate) untransformed_text_is_ascii_whitespace: bool,
}

impl TextSource {
    pub(crate) fn new(text: Vec<u16>, dom_start_offset: usize, dom_length: usize, is_password_input: bool) -> Self {
        let untransformed_text_is_ascii_whitespace =
            text.iter().all(|unit| code_point_is_ascii_space(u32::from(*unit)));
        Self {
            text,
            dom_start_offset,
            dom_length,
            is_password_input,
            untransformed_text_is_ascii_whitespace,
        }
    }
}

/// Length-preserving regions have an implicit one-to-one mapping. Each edit
/// records only a source span whose rendered length differs, keeping the
/// common identity mapping allocation-free.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RenderedTextEdit {
    pub(crate) dom_start_offset: usize,
    pub(crate) dom_length_in_code_units: usize,
    pub(crate) rendered_start_offset: usize,
    pub(crate) rendered_length_in_code_units: usize,
}

pub(crate) struct RenderedText {
    pub(crate) text: Vec<u16>,
    pub(crate) edits: Vec<RenderedTextEdit>,
}

#[derive(Default)]
struct EditRecorder {
    edits: Vec<RenderedTextEdit>,
}

impl EditRecorder {
    fn append(
        &mut self,
        dom_start_offset: usize,
        dom_length_in_code_units: usize,
        rendered_start_offset: usize,
        rendered_length_in_code_units: usize,
    ) {
        if dom_length_in_code_units == rendered_length_in_code_units {
            return;
        }
        if rendered_length_in_code_units == 0
            && let Some(previous_edit) = self.edits.last_mut()
            && previous_edit.rendered_length_in_code_units == 0
            && previous_edit.dom_start_offset + previous_edit.dom_length_in_code_units == dom_start_offset
            && previous_edit.rendered_start_offset == rendered_start_offset
        {
            previous_edit.dom_length_in_code_units += dom_length_in_code_units;
            return;
        }
        self.edits.push(RenderedTextEdit {
            dom_start_offset,
            dom_length_in_code_units,
            rendered_start_offset,
            rendered_length_in_code_units,
        });
    }

    fn adopt_mapping_result(&mut self, result: MappingResult) -> Vec<u16> {
        for edit in result.edits {
            self.append(
                edit.source_start,
                edit.source_length,
                edit.destination_start,
                edit.destination_length,
            );
        }
        result.text
    }
}

#[derive(Default)]
struct MappingResult {
    text: Vec<u16>,
    edits: Vec<UnicodeLayoutTextMappingEdit>,
}

/// # Safety
/// Whichever of `ascii_text`/`utf16_text` is non-null must address
/// `length_in_code_units` units of storage live for the duration of the call,
/// unless the length is zero.
pub(crate) unsafe fn utf16_vec_from_ffi(
    ascii_text: *const u8,
    utf16_text: *const u16,
    length_in_code_units: usize,
) -> Vec<u16> {
    if length_in_code_units == 0 {
        return Vec::new();
    }
    if !ascii_text.is_null() {
        // SAFETY: The caller guarantees the ASCII storage is live for this call.
        return unsafe { std::slice::from_raw_parts(ascii_text, length_in_code_units) }
            .iter()
            .map(|unit| u16::from(*unit))
            .collect();
    }
    assert!(!utf16_text.is_null(), "FFI text carries no storage");
    // SAFETY: The caller guarantees the UTF-16 storage is live for this call.
    unsafe { std::slice::from_raw_parts(utf16_text, length_in_code_units) }.to_vec()
}

unsafe extern "C" fn collect_mapping_result(
    context: *mut c_void,
    ascii_text: *const u8,
    utf16_text: *const u16,
    length_in_code_units: usize,
    edits: *const UnicodeLayoutTextMappingEdit,
    edit_count: usize,
) {
    // SAFETY: The mapping entry points below pass a MappingResult they own
    // for the duration of the synchronous call, and the C++ side hands out
    // storage that stays live until the sink returns.
    let result = unsafe { &mut *context.cast::<MappingResult>() };
    result.text = unsafe { utf16_vec_from_ffi(ascii_text, utf16_text, length_in_code_units) };
    result.edits = if edit_count == 0 {
        Vec::new()
    } else {
        unsafe { std::slice::from_raw_parts(edits, edit_count) }.to_vec()
    };
}

fn apply_case_mapping(
    text: &[u16],
    mapping: u8,
    ascii_locale: Option<&[u8]>,
    preserve_existing_trailing_code_points: bool,
) -> MappingResult {
    let mut result = MappingResult::default();
    let (locale_pointer, locale_length) = match ascii_locale {
        Some(locale) => (locale.as_ptr(), locale.len()),
        None => (std::ptr::null(), 0),
    };
    // SAFETY: Both buffers outlive the synchronous call, and the sink only
    // touches the MappingResult owned by this frame.
    unsafe {
        unicode_layout_apply_case_mapping(
            text.as_ptr(),
            text.len(),
            mapping,
            locale_pointer,
            locale_length,
            preserve_existing_trailing_code_points,
            (&raw mut result).cast(),
            collect_mapping_result,
        );
    }
    result
}

fn apply_fullwidth_mapping(text: &[u16]) -> MappingResult {
    let mut result = MappingResult::default();
    // SAFETY: See apply_case_mapping.
    unsafe {
        unicode_layout_apply_fullwidth_mapping(
            text.as_ptr(),
            text.len(),
            (&raw mut result).cast(),
            collect_mapping_result,
        );
    }
    result
}

pub(crate) fn may_require_bidi_processing(text: &[u16]) -> bool {
    if text.iter().all(|unit| *unit < 0x80) {
        return false;
    }
    // SAFETY: The buffer outlives the synchronous call.
    unsafe { unicode_layout_may_require_bidi_processing(text.as_ptr(), text.len()) }
}

// https://w3c.github.io/mathml-core/#italic-mappings
fn map_code_point_to_math_italic(code_point: u32) -> u32 {
    match code_point {
        0x0041..=0x005A => 0x1D434 + (code_point - 0x0041),
        0x0061..=0x0067 => 0x1D44E + (code_point - 0x0061),
        0x0068 => 0x0210E,
        0x0069..=0x007A => 0x1D456 + (code_point - 0x0069),
        0x0131 => 0x1D6A4,
        0x0237 => 0x1D6A5,
        0x0391..=0x03A1 => 0x1D6E2 + (code_point - 0x0391),
        0x03F4 => 0x1D6F3,
        0x03A3..=0x03A9 => 0x1D6F4 + (code_point - 0x03A3),
        0x2207 => 0x1D6FB,
        0x03B1..=0x03C9 => 0x1D6FC + (code_point - 0x03B1),
        0x2202 => 0x1D715,
        0x03F5 => 0x1D716,
        0x03D1 => 0x1D717,
        0x03F0 => 0x1D718,
        0x03D5 => 0x1D719,
        0x03F1 => 0x1D71A,
        0x03D6 => 0x1D71B,
        _ => code_point,
    }
}

fn append_code_point(text: &mut Vec<u16>, code_point: u32) {
    if code_point >= 0x10000 {
        let offset = code_point - 0x10000;
        text.push(0xd800 + (offset >> 10) as u16);
        text.push(0xdc00 + (offset & 0x3ff) as u16);
    } else {
        text.push(code_point as u16);
    }
}

// https://w3c.github.io/mathml-core/#new-text-transform-values
fn apply_math_auto_text_transform(source: &[u16], recorder: &mut EditRecorder) -> Vec<u16> {
    let mut text = Vec::with_capacity(source.len());
    let mut dom_offset = 0;
    let mut rendered_offset = 0;
    for_each_code_point_utf16(source, |source_code_point| {
        let source_length = code_unit_length_for_code_point(source_code_point);
        let rendered_code_point = map_code_point_to_math_italic(source_code_point);
        let rendered_length = code_unit_length_for_code_point(rendered_code_point);
        recorder.append(dom_offset, source_length, rendered_offset, rendered_length);
        dom_offset += source_length;
        rendered_offset += rendered_length;
        append_code_point(&mut text, rendered_code_point);
    });
    text
}

fn apply_text_transform(source: &[u16], text_transform: u8, casing_locale: Option<&[u8]>) -> RenderedText {
    let mut recorder = EditRecorder::default();
    let text = match text_transform {
        text_transform::UPPERCASE | text_transform::LOWERCASE | text_transform::CAPITALIZE => {
            let (mapping, preserve_existing_trailing_code_points) = match text_transform {
                text_transform::UPPERCASE => (CASE_MAPPING_UPPERCASE, false),
                text_transform::LOWERCASE => (CASE_MAPPING_LOWERCASE, false),
                _ => (CASE_MAPPING_TITLECASE, true),
            };
            recorder.adopt_mapping_result(apply_case_mapping(
                source,
                mapping,
                casing_locale,
                preserve_existing_trailing_code_points,
            ))
        }
        text_transform::MATH_AUTO => apply_math_auto_text_transform(source, &mut recorder),
        text_transform::FULL_WIDTH => recorder.adopt_mapping_result(apply_fullwidth_mapping(source)),
        // FIXME: Implement text-transform full-size-kana.
        text_transform::FULL_SIZE_KANA | text_transform::NONE => source.to_vec(),
        _ => unreachable!("unknown text-transform value {text_transform}"),
    };
    RenderedText {
        text,
        edits: recorder.edits,
    }
}

fn mask_password(source: &[u16], recorder: &mut EditRecorder) -> Vec<u16> {
    let mut text = Vec::with_capacity(source.len());
    let mut dom_offset = 0;
    let mut rendered_offset = 0;
    for_each_code_point_utf16(source, |source_code_point| {
        let source_length = code_unit_length_for_code_point(source_code_point);
        recorder.append(dom_offset, source_length, rendered_offset, 1);
        dom_offset += source_length;
        rendered_offset += 1;
        text.push(PASSWORD_MASK_CODE_UNIT);
    });
    text
}

/// A TextSliceNode renders a sub-range of its DOM text (the `::first-letter`
/// split). The complete DOM text is transformed before the rendered slice is
/// extracted so contextual transforms such as capitalize see the same
/// surrounding text for both slices.
fn slice_rendered_text(rendered: RenderedText, dom_start_offset: usize, dom_length: usize) -> RenderedText {
    let dom_end_offset = dom_start_offset + dom_length;
    let rendered_start_offset =
        rendered_text_offset_for_dom_offset(&rendered.edits, 0, dom_start_offset, RenderedTextBoundary::Start);
    let rendered_end_offset =
        rendered_text_offset_for_dom_offset(&rendered.edits, 0, dom_end_offset, RenderedTextBoundary::End);
    let text = rendered.text[rendered_start_offset..rendered_end_offset].to_vec();

    let mut sliced_edits = Vec::new();
    for edit in &rendered.edits {
        let edit_dom_end = edit.dom_start_offset + edit.dom_length_in_code_units;
        if edit_dom_end <= dom_start_offset || edit.dom_start_offset >= dom_end_offset {
            continue;
        }
        let edit_rendered_end = edit.rendered_start_offset + edit.rendered_length_in_code_units;
        let sliced_dom_start = edit.dom_start_offset.max(dom_start_offset);
        let sliced_dom_end = edit_dom_end.min(dom_end_offset);
        let sliced_rendered_start = edit.rendered_start_offset.max(rendered_start_offset);
        let sliced_rendered_end = edit_rendered_end.min(rendered_end_offset);
        if sliced_dom_end - sliced_dom_start == sliced_rendered_end - sliced_rendered_start {
            continue;
        }
        sliced_edits.push(RenderedTextEdit {
            dom_start_offset: sliced_dom_start,
            dom_length_in_code_units: sliced_dom_end - sliced_dom_start,
            rendered_start_offset: sliced_rendered_start - rendered_start_offset,
            rendered_length_in_code_units: sliced_rendered_end - sliced_rendered_start,
        });
    }
    RenderedText {
        text,
        edits: sliced_edits,
    }
}

// https://drafts.csswg.org/css-text-4/#white-space-phase-1
fn convert_collapsible_whitespace(rendered: RenderedText, white_space_collapse: u8) -> RenderedText {
    const SPACE: u16 = b' ' as u16;
    const TAB: u16 = b'\t' as u16;
    const NEWLINE: u16 = b'\n' as u16;

    if rendered.text.is_empty()
        || !rendered
            .text
            .iter()
            .any(|unit| code_point_is_ascii_space(u32::from(*unit)))
    {
        return rendered;
    }

    let mut convert_newlines = false;
    let mut convert_tabs = false;

    // If white-space-collapse is set to collapse or preserve-breaks, white space characters are considered collapsible
    // and are processed by performing the following steps:
    if matches!(
        white_space_collapse,
        white_space_collapse::COLLAPSE | white_space_collapse::PRESERVE_BREAKS
    ) {
        // 1. FIXME: Any sequence of collapsible spaces and tabs immediately preceding or following a segment break is removed.

        // 2. Collapsible segment breaks are transformed for rendering according to the segment break transformation
        //    rules.
        {
            // https://drafts.csswg.org/css-text-4/#line-break-transform
            // FIXME: When white-space-collapse is not collapse, segment breaks are not collapsible. For values other than
            // collapse or preserve-spaces (which transforms them into spaces), segment breaks are instead transformed
            // into a preserved line feed (U+000A).

            // When white-space-collapse is collapse, segment breaks are collapsible, and are collapsed as follows:
            if white_space_collapse == white_space_collapse::COLLAPSE {
                // 1. FIXME: First, any collapsible segment break immediately following another collapsible segment break is
                //    removed.

                // 2. FIXME: Then any remaining segment break is either transformed into a space (U+0020) or removed depending
                //    on the context before and after the break. The rules for this operation are UA-defined in this
                //    level.
                convert_newlines = true;
            }
        }

        // 3. Every collapsible tab is converted to a collapsible space (U+0020).
        convert_tabs = true;

        // 4. Any collapsible space immediately following another collapsible space—even one outside the boundary of the
        //    inline containing that space, provided both spaces are within the same inline formatting context—is
        //    collapsed to have zero advance width. (It is invisible, but retains its soft wrap opportunity, if any.)
        // AD-HOC: This is handled by the text chunker by removing the space.
    }

    // If white-space-collapse is set to preserve-spaces, each tab and segment break is converted to a space.
    if white_space_collapse == white_space_collapse::PRESERVE_SPACES {
        convert_tabs = true;
        convert_newlines = true;
    }

    let RenderedText { mut text, edits } = rendered;

    // AD-HOC: It's important to not change the amount of code units in the resulting transformed text, so the text
    //         chunker can produce code unit offsets that still match the original text.
    if convert_newlines || convert_tabs {
        for unit in &mut text {
            if (convert_newlines && *unit == NEWLINE) || (convert_tabs && *unit == TAB) {
                *unit = SPACE;
            }
        }
    }

    RenderedText { text, edits }
}

pub(crate) fn derive_rendered_text(
    source: &TextSource,
    text_transform: u8,
    white_space_collapse: u8,
    casing_locale: Option<&[u8]>,
) -> RenderedText {
    let mut rendered = if source.is_password_input {
        let mut recorder = EditRecorder::default();
        let text = mask_password(&source.text, &mut recorder);
        RenderedText {
            text,
            edits: recorder.edits,
        }
    } else {
        apply_text_transform(&source.text, text_transform, casing_locale)
    };

    if source.dom_start_offset > 0 || source.dom_length < source.text.len() {
        rendered = slice_rendered_text(rendered, source.dom_start_offset, source.dom_length);
    }

    convert_collapsible_whitespace(rendered, white_space_collapse)
}

/// Maps a DOM offset to a rendered offset. When the offset falls inside a
/// length-changing span, the boundary selects that span's rendered start or
/// end.
pub(crate) fn rendered_text_offset_for_dom_offset(
    edits: &[RenderedTextEdit],
    dom_base_offset: usize,
    dom_offset: usize,
    boundary: RenderedTextBoundary,
) -> usize {
    let mut previous_dom_end = dom_base_offset;
    let mut previous_rendered_end = 0;
    for edit in edits {
        if dom_offset < edit.dom_start_offset {
            return previous_rendered_end + dom_offset - previous_dom_end;
        }

        let dom_end = edit.dom_start_offset + edit.dom_length_in_code_units;
        let rendered_end = edit.rendered_start_offset + edit.rendered_length_in_code_units;
        if dom_offset <= dom_end {
            if dom_offset == edit.dom_start_offset {
                return edit.rendered_start_offset;
            }
            if dom_offset == dom_end {
                return rendered_end;
            }
            return match boundary {
                RenderedTextBoundary::Start => edit.rendered_start_offset,
                RenderedTextBoundary::End => rendered_end,
            };
        }

        previous_dom_end = dom_end;
        previous_rendered_end = rendered_end;
    }

    previous_rendered_end + dom_offset - previous_dom_end
}

/// Maps a rendered offset to a DOM offset. When the offset falls inside a
/// length-changing span, the boundary selects that span's DOM start or end.
pub(crate) fn dom_offset_for_rendered_text_offset(
    edits: &[RenderedTextEdit],
    dom_base_offset: usize,
    rendered_text_offset: usize,
    boundary: RenderedTextBoundary,
) -> usize {
    let mut previous_dom_end = dom_base_offset;
    let mut previous_rendered_end = 0;
    for edit in edits {
        if rendered_text_offset < edit.rendered_start_offset {
            return previous_dom_end + rendered_text_offset - previous_rendered_end;
        }

        let dom_end = edit.dom_start_offset + edit.dom_length_in_code_units;
        let rendered_end = edit.rendered_start_offset + edit.rendered_length_in_code_units;
        if rendered_text_offset == edit.rendered_start_offset {
            if edit.rendered_length_in_code_units > 0 {
                return edit.dom_start_offset;
            }
            if matches!(boundary, RenderedTextBoundary::End) {
                return edit.dom_start_offset;
            }
        } else if rendered_text_offset < rendered_end {
            return match boundary {
                RenderedTextBoundary::Start => edit.dom_start_offset,
                RenderedTextBoundary::End => dom_end,
            };
        } else if rendered_text_offset == rendered_end && matches!(boundary, RenderedTextBoundary::End) {
            return dom_end;
        }

        previous_dom_end = dom_end;
        previous_rendered_end = rendered_end;
    }

    previous_dom_end + rendered_text_offset - previous_rendered_end
}

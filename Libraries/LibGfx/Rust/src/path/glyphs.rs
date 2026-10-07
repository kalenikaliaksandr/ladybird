/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

use super::measure::ContourMeasure;
use super::sk_matrix::{PI, SkMatrix};
use super::{Path, PathFillType};
use crate::text_layout::DrawGlyph;
use std::ffi::c_void;

unsafe extern "C" {
    /// Gives the ascent of the font, or false for a font that draws nothing.
    fn ladybird_gfx_font_glyph_outline_ascent(font: *const c_void, out_ascent: *mut f32) -> bool;
    /// Appends the outline of a glyph to `outline`, in pixels, with y down and the origin at the
    /// start of the baseline.
    fn ladybird_gfx_font_append_glyph_outline(font: *const c_void, glyph_id: u32, outline: *mut c_void);
}

/// A shaped run of glyphs in one font, as the path operations below take one.
pub struct GlyphRun {
    pub font: crate::font::FontHandle,
    pub glyphs: Vec<DrawGlyph>,
}

fn ascent_of_visible_font(font: *const c_void) -> Option<f32> {
    let mut ascent = 0.0;
    // SAFETY: The caller keeps the font live for the call.
    unsafe { ladybird_gfx_font_glyph_outline_ascent(font, &raw mut ascent) }.then_some(ascent)
}

/// Replaces `outline` with the outline of a glyph.
fn set_to_glyph_outline(outline: &mut Path, font: *const c_void, glyph_id: u32) {
    outline.clear();
    // SAFETY: The caller keeps the font live for the call, and the path is live and unique.
    unsafe { ladybird_gfx_font_append_glyph_outline(font, glyph_id, (&raw mut *outline).cast()) };
}

/// Appends the outlines of the glyphs, with each glyph's position at the top of its line box.
pub(crate) fn append_glyph_run(path: &mut Path, font: *const c_void, glyphs: &[DrawGlyph]) {
    let Some(ascent) = ascent_of_visible_font(font) else {
        return;
    };
    path.set_fill_type(PathFillType::Winding);
    let mut outline = Path::new();
    for glyph in glyphs {
        set_to_glyph_outline(&mut outline, font, glyph.glyph_id);
        path.sk_add_path(outline.snapshot(), &SkMatrix::translate(glyph.x, glyph.y + ascent));
    }
    path.update_state_from_snapshot();
}

/// The outlines of the glyphs, with each glyph on the first contour of `path` at `offset` plus its
/// position, and turned to follow the contour. A glyph whose middle is past the end of the contour
/// ends the text.
pub(crate) fn place_glyph_runs_along(path: &Path, runs: &[GlyphRun], offset: f32) -> Path {
    let measure = ContourMeasure::first_of(path.snapshot());
    let length = measure.as_ref().map_or(0.0, ContourMeasure::length);

    let mut output = Path::new();
    let mut outline = Path::new();
    'runs: for run in runs {
        let font = run.font.as_raw();
        if ascent_of_visible_font(font).is_none() {
            continue;
        }
        for glyph in &run.glyphs {
            let distance = offset + glyph.x;
            let Some((position, tangent)) = measure
                .as_ref()
                .and_then(|measure| measure.position_and_tangent(distance))
            else {
                continue;
            };
            let midpoint_distance = distance + glyph.glyph_width / 2.0;
            if midpoint_distance > length {
                break 'runs;
            }
            set_to_glyph_outline(&mut outline, font, glyph.glyph_id);
            let degrees = tangent.y.atan2(tangent.x) * (180.0 / PI);
            let matrix = SkMatrix::translate(position.x, position.y).pre_rotate(degrees);
            output.sk_add_path(outline.snapshot(), &matrix);
        }
    }
    output.update_state_from_snapshot();
    output
}

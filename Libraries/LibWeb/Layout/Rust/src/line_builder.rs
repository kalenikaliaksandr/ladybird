/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

use super::fragment::{FragmentBuildFacts, GlyphData, LineBoxFragmentData};
use super::line_box::LineBoxData;
use super::{InlineFormattingContext, Node};
use crate::css_enums::{direction, text_align, vertical_align, writing_mode};
use crate::css_pixels::CssPixels;
use crate::geometry::AvailableSize;
use crate::style_facts::StyleValues;
use crate::used_values::FfiLineBoxFragmentCoordinate;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ForcedBreak {
    No,
    Yes,
}

#[derive(Clone, Copy)]
struct VerticalAlignMetrics {
    baseline: CssPixels,
    block_size: CssPixels,
    effective_box_block_start_offset: CssPixels,
    effective_box_block_end_offset: CssPixels,
    line_height: CssPixels,
}

pub(crate) struct LineBuilder {
    context: *mut InlineFormattingContext,
    available_inline_size_for_current_line: AvailableSize,
    current_block_offset: CssPixels,
    max_block_size_on_current_line: CssPixels,
    unbreakable_run_inline_size_interrupted_by_float: CssPixels,
    text_indent: CssPixels,
    text_indent_hanging: bool,
    text_indent_each_line: bool,
    direction: u8,
    writing_mode: u8,
    last_line_needs_update: bool,
    should_advance_to_last_line_box_block_end: bool,
    current_line_committed_pending_margin: bool,
    pending_margin_follows_block_level_box: bool,
}

impl LineBuilder {
    pub(crate) fn new(context: *mut InlineFormattingContext) -> Self {
        // SAFETY: The run owns the boxed context for this builder's lifetime.
        let context_ref = unsafe { &*context };
        let style = context_ref.style(context_ref.containing_block);
        let containing_inline_size = context_ref.containing_used().content_inline_size;
        let mut builder = Self {
            context,
            available_inline_size_for_current_line: AvailableSize::indefinite(),
            current_block_offset: CssPixels::default(),
            max_block_size_on_current_line: CssPixels::default(),
            unbreakable_run_inline_size_interrupted_by_float: CssPixels::default(),
            text_indent: style.text_indent().to_px(containing_inline_size),
            text_indent_hanging: style.text_indent_hanging(),
            text_indent_each_line: style.text_indent_each_line(),
            direction: style.direction,
            writing_mode: style.writing_mode,
            last_line_needs_update: false,
            should_advance_to_last_line_box_block_end: false,
            current_line_committed_pending_margin: false,
            pending_margin_follows_block_level_box: false,
        };
        builder.begin_new_line(false, true, ForcedBreak::No);
        builder
    }

    fn context(&self) -> &InlineFormattingContext {
        // SAFETY: Shared access does not escape the method.
        unsafe { &*self.context }
    }

    fn line_count(&self) -> usize {
        self.context().line_data().line_boxes.len()
    }

    fn ensure_last_line_index(&mut self) -> usize {
        if self.line_count() == 0 {
            let direction = self.direction;
            let writing_mode = self.writing_mode;
            self.context()
                .line_data_mut()
                .line_boxes
                .push(LineBoxData::new(direction, writing_mode));
        }
        self.line_count() - 1
    }

    fn line(&self, index: usize) -> &LineBoxData {
        &self.context().line_data().line_boxes[index]
    }

    fn line_mut(&mut self, index: usize) -> &mut LineBoxData {
        &mut self.context().line_data_mut().line_boxes[index]
    }

    fn containing_style(&self) -> StyleValues {
        self.context().style(self.context().containing_block)
    }

    fn fragment_facts(&self, node: Node) -> FragmentBuildFacts {
        let style_source = self.context().style_source(node);
        let style = self.context().style(style_source);
        let facts = self.context().facts(node);
        let (text_utf16, text_length) = if facts.is_text_node {
            let parent = self.context().parent_node(node);
            let parent_style = self.context().style(parent);
            let should_wrap = parent_style.text_wrap_mode == crate::css_enums::text_wrap_mode::WRAP;
            let should_respect = matches!(
                parent_style.white_space_collapse,
                crate::css_enums::white_space_collapse::PRESERVE
                    | crate::css_enums::white_space_collapse::PRESERVE_BREAKS
                    | crate::css_enums::white_space_collapse::BREAK_SPACES
            );
            let callbacks = self.context().callbacks;
            let ffi = crate::layout_state::state_mut(self.context().state)
                .text_facts(&callbacks, node, should_wrap, should_respect, false)
                .ffi();
            (ffi.text_utf16, ffi.text_length_in_code_units)
        } else {
            (std::ptr::null(), 0)
        };
        FragmentBuildFacts {
            style_source,
            is_atomic_inline: facts.is_atomic_inline,
            white_space_collapse: style.white_space_collapse,
            letter_spacing: style.letter_spacing,
            first_available_font: style.first_available_font(),
            text_utf16,
            text_length_in_code_units: text_length,
            style_block_axis_is_reverse: matches!(
                style.writing_mode,
                writing_mode::VERTICAL_RL | writing_mode::SIDEWAYS_RL
            ),
        }
    }

    fn begin_new_line(&mut self, advance: bool, first_in_sequence: bool, forced: ForcedBreak) {
        let line_height = self.containing_style().line_height;
        if advance {
            if first_in_sequence {
                if self.should_advance_to_last_line_box_block_end && self.line_count() > 1 {
                    self.current_block_offset = self.line(self.line_count() - 2).physical_vertical_end();
                } else {
                    self.current_block_offset += self.max_block_size_on_current_line.max(line_height);
                }
            } else if let Some(next) = self
                .context()
                .next_float_band_block_start_after(self.current_block_offset)
            {
                self.current_block_offset = next;
            } else {
                self.current_block_offset += self.max_block_size_on_current_line.max(line_height);
            }
        }
        self.recalculate_available_space();
        let line_index = self.ensure_last_line_index();
        self.line_mut(line_index).original_available_inline_size = self.available_inline_size_for_current_line;
        self.max_block_size_on_current_line = CssPixels::default();
        self.last_line_needs_update = true;
        self.should_advance_to_last_line_box_block_end = false;
        let mut should_indent = self.line_count() <= 1 || (self.text_indent_each_line && forced == ForcedBreak::Yes);
        if self.text_indent_hanging {
            should_indent = !should_indent;
        }
        if should_indent {
            let indent = self.text_indent;
            self.line_mut(line_index).inline_length += indent;
        }
        self.current_line_committed_pending_margin = false;
    }

    pub(crate) fn break_line(&mut self, forced: ForcedBreak, next_item_inline_size: Option<CssPixels>) {
        let line_index = self.ensure_last_line_index();
        {
            let line = self.line_mut(line_index);
            line.has_break = true;
            line.has_forced_break = forced == ForcedBreak::Yes;
        }
        self.last_line_needs_update = true;
        self.update_last_line();

        let mut break_count = 0usize;
        loop {
            self.context()
                .line_data_mut()
                .line_boxes
                .push(LineBoxData::new(self.direction, self.writing_mode));
            self.begin_new_line(true, break_count == 0, forced);
            break_count += 1;
            let line_height = self.containing_style().line_height;
            let current_line_block_size = self.max_block_size_on_current_line.max(line_height);
            let floats_intrude = self.context().any_floats_intrude_in_block_range(
                self.current_block_offset,
                self.current_block_offset + current_line_block_size,
            );
            let next_too_wide = next_item_inline_size
                .is_some_and(|size| self.available_inline_size_for_current_line.pixels_greater_than(size));
            if !floats_intrude
                || (self
                    .context()
                    .can_fit_new_line_at_block_offset(self.current_block_offset, line_height)
                    && !next_too_wide)
            {
                break;
            }
        }
    }

    pub(crate) fn break_if_needed(&mut self, next_item_inline_size: CssPixels) -> bool {
        if !self.should_break(next_item_inline_size) {
            return false;
        }
        self.break_line(ForcedBreak::No, Some(next_item_inline_size));
        true
    }

    pub(crate) fn append_box(
        &mut self,
        node: Node,
        leading_size: CssPixels,
        trailing_size: CssPixels,
        leading_margin: CssPixels,
        trailing_margin: CssPixels,
    ) {
        self.prepare_to_append_inline_content();
        let used = self.context().used(node);
        let content_inline = used.content_inline_size;
        let content_block = used.content_block_size;
        let border_start = used.border_box_top(false);
        let border_end = used.border_box_bottom(false);
        let margin_block_size = used.margin_box_block_size(false);
        let line_index = self.ensure_last_line_index();
        let fragment_index = self.line(line_index).fragments.len();
        let text_align_is_justify = self.containing_style().text_align == text_align::JUSTIFY;
        let fragment_facts = self.fragment_facts(node);
        self.line_mut(line_index).add_fragment(
            node,
            0,
            0,
            leading_size,
            trailing_size,
            leading_margin,
            trailing_margin,
            content_inline,
            content_block,
            border_start,
            border_end,
            None,
            fragment_facts,
            text_align_is_justify,
        );
        self.max_block_size_on_current_line = self.max_block_size_on_current_line.max(margin_block_size);
        let used = self.context().used_mut(node);
        used.has_containing_line_box_fragment = false;
        if self.context().facts(node).is_atomic_inline {
            used.has_containing_line_box_fragment = true;
            used.containing_line_box_fragment = FfiLineBoxFragmentCoordinate {
                line_box_index: line_index,
                fragment_index,
            };
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn append_text_chunk(
        &mut self,
        node: Node,
        offset_in_node: usize,
        length_in_node: usize,
        leading_size: CssPixels,
        trailing_size: CssPixels,
        leading_margin: CssPixels,
        trailing_margin: CssPixels,
        content_inline_size: CssPixels,
        content_block_size: CssPixels,
        glyphs: GlyphData,
    ) {
        self.prepare_to_append_inline_content();
        let line_index = self.ensure_last_line_index();
        let text_align_is_justify = self.containing_style().text_align == text_align::JUSTIFY;
        let facts = self.fragment_facts(node);
        self.line_mut(line_index).add_fragment(
            node,
            offset_in_node,
            length_in_node,
            leading_size,
            trailing_size,
            leading_margin,
            trailing_margin,
            content_inline_size,
            content_block_size,
            CssPixels::default(),
            CssPixels::default(),
            Some(glyphs),
            facts,
            text_align_is_justify,
        );
        self.max_block_size_on_current_line = self
            .max_block_size_on_current_line
            .max(self.line(line_index).block_length);
    }

    pub(crate) fn append_static_position_marker(&mut self, node: Node, preceded_by_start_edges: bool) {
        let line_index = self.ensure_last_line_index();
        self.line_mut(line_index)
            .add_static_position_marker(node, preceded_by_start_edges);
    }

    pub(crate) fn prepare_to_append_inline_content(&mut self) {
        if self.current_line_committed_pending_margin {
            return;
        }
        let line_index = self.ensure_last_line_index();
        if self.line(line_index).has_block_level_box {
            return;
        }
        let margin = self.context().parent_commit_pending_margin_before_inline_content();
        if margin != CssPixels::default() {
            self.current_block_offset += margin;
            self.recalculate_available_space();
        }
        self.current_line_committed_pending_margin = true;
        self.pending_margin_follows_block_level_box = false;
    }

    pub(crate) fn commit_pending_margin_before_float(&mut self) {
        if self.pending_margin_follows_block_level_box {
            self.prepare_to_append_inline_content();
        }
    }

    pub(crate) fn finish_current_line_before_block_level_box(&mut self) {
        if self.line_count() == 0 || self.line(self.line_count() - 1).is_empty() {
            return;
        }
        let line_index = self.line_count() - 1;
        self.line_mut(line_index).has_break = true;
        self.line_mut(line_index).has_forced_break = true;
        self.last_line_needs_update = true;
        self.update_last_line();
        self.context()
            .line_data_mut()
            .line_boxes
            .push(LineBoxData::new(self.direction, self.writing_mode));
        self.begin_new_line(true, true, ForcedBreak::No);
    }

    pub(crate) fn append_block_level_box(&mut self, node: Node, block_end: CssPixels, block_end_margin: CssPixels) {
        let used = self.context().used(node);
        assert!(used.has_content_offset);
        let horizontal = self.writing_mode == writing_mode::HORIZONTAL_TB;
        let inline_offset = if horizontal {
            used.content_offset.x
        } else {
            used.content_offset.y
        };
        let block_offset = if horizontal {
            used.content_offset.y
        } else {
            used.content_offset.x
        };
        let inline_length = if horizontal {
            used.content_inline_size
        } else {
            used.content_block_size
        };
        let block_length = if horizontal {
            used.content_block_size
        } else {
            used.content_inline_size
        };
        let border_start = used.border_box_top(false);
        let line_inline_length = if horizontal {
            used.margin_box_inline_size(false)
        } else {
            used.margin_box_block_size(false)
        };
        let line_index = self.ensure_last_line_index();
        assert!(self.line(line_index).fragments.is_empty());
        let fragment = LineBoxFragmentData::new(
            node,
            0,
            0,
            inline_offset,
            block_offset,
            inline_length,
            block_length,
            border_start,
            self.direction,
            self.writing_mode,
            None,
            self.fragment_facts(node),
        );
        let current_block_offset = self.current_block_offset;
        {
            let line = self.line_mut(line_index);
            line.fragments.push(fragment);
            line.inline_length = line_inline_length;
            line.block_length = CssPixels::default();
            line.block_end = block_end;
            line.baseline = CssPixels::default();
            line.has_block_level_box = true;
            line.block_level_box_block_end_margin = block_end_margin;
            line.has_break = true;
            line.has_forced_break = true;
            for marker in &mut line.static_position_markers {
                marker.block_offset += current_block_offset;
            }
        }
        let used = self.context().used_mut(node);
        used.has_containing_line_box_fragment = true;
        used.containing_line_box_fragment = FfiLineBoxFragmentCoordinate {
            line_box_index: line_index,
            fragment_index: 0,
        };
        self.pending_margin_follows_block_level_box = true;
        self.current_block_offset = block_end;
        self.max_block_size_on_current_line = CssPixels::default();
        self.last_line_needs_update = false;
        self.should_advance_to_last_line_box_block_end = false;
        self.context()
            .line_data_mut()
            .line_boxes
            .push(LineBoxData::new(self.direction, self.writing_mode));
        self.begin_new_line(false, true, ForcedBreak::No);
    }

    pub(crate) fn ceiling_for_float_to_be_inserted_here(&mut self, node: Node) -> CssPixels {
        let float_inline_size = self.context().used(node).margin_box_inline_size(false);
        let mut candidate = self.current_block_offset;
        let line_index = self.ensure_last_line_index();
        // Raw pointers deliberately split the two state arenas so the font
        // callback can re-enter without an outstanding Rust borrow.
        let line = &raw mut self.context().line_data_mut().line_boxes[line_index];
        let context = self.context;
        // SAFETY: Both objects are live and distinct for this serialized call.
        let current_line_inline_size =
            unsafe { (*line).physical_horizontal_extent() - (*line).trailing_whitespace_inline_size(&mut *context) };
        // SAFETY: The line is not borrowed elsewhere.
        let line_is_empty_or_whitespace = unsafe { (*line).is_empty_or_ends_in_whitespace() };
        let mut needed = current_line_inline_size;
        if !line_is_empty_or_whitespace {
            needed += self.unbreakable_run_inline_size_interrupted_by_float;
        }
        self.unbreakable_run_inline_size_interrupted_by_float = CssPixels::default();
        if current_line_inline_size > CssPixels::default()
            && self
                .available_inline_size_for_current_line
                .pixels_greater_than(needed + float_inline_size)
        {
            // This intentionally mixes physical horizontal and inline axes in
            // vertical writing modes, matching the old implementation.
            candidate += self.line(line_index).physical_vertical_extent();
        }
        candidate.max(self.context().block_axis_float_clearance())
    }

    fn should_break(&mut self, next_item_inline_size: CssPixels) -> bool {
        if self.available_inline_size_for_current_line.is_max_content() {
            return false;
        }
        if self.line_count() == 0 || self.line(self.line_count() - 1).is_empty() {
            let line_height = self.containing_style().line_height;
            if !self
                .context()
                .any_floats_intrude_in_block_range(self.current_block_offset, self.current_block_offset + line_height)
            {
                return false;
            }
        }
        let line_index = self.ensure_last_line_index();
        let current_inline_size = self.line(line_index).physical_horizontal_extent();
        self.available_inline_size_for_current_line
            .pixels_greater_than(current_inline_size + next_item_inline_size)
    }

    fn baseline_for_style(style: StyleValues, line_height: CssPixels) -> CssPixels {
        let typographic = CssPixels::nearest_value_for_f32(style.font_ascent() + style.font_descent());
        CssPixels::nearest_value_for_f32(style.font_ascent()) + (line_height - typographic) / 2
    }

    fn normal_line_height(style: StyleValues) -> CssPixels {
        let ascent = style.font_ascent().round_ties_even() as i64;
        let descent = style.font_descent().round_ties_even() as i64;
        CssPixels::from_integer(ascent.saturating_add(descent))
    }

    fn block_offset_for_alignment(
        &self,
        style: StyleValues,
        metrics: VerticalAlignMetrics,
        line_box_baseline: CssPixels,
    ) -> CssPixels {
        let alphabetic =
            self.current_block_offset + line_box_baseline - metrics.baseline + metrics.effective_box_block_start_offset;
        if !style.vertical_align_is_keyword() {
            return alphabetic - style.vertical_align_value().to_px(metrics.line_height);
        }
        match style.vertical_align_keyword() {
            vertical_align::BASELINE => alphabetic,
            vertical_align::TOP => self.current_block_offset + metrics.effective_box_block_start_offset,
            vertical_align::MIDDLE => {
                let x_height = CssPixels::nearest_value_for_f32(self.containing_style().font_x_height());
                self.current_block_offset
                    + line_box_baseline
                    + (metrics.effective_box_block_start_offset
                        - metrics.effective_box_block_end_offset
                        - x_height
                        - metrics.block_size)
                        / 2
            }
            vertical_align::SUB => alphabetic + self.containing_style().font_size / 5,
            vertical_align::SUPER => alphabetic - self.containing_style().font_size / 3,
            vertical_align::BOTTOM | vertical_align::TEXT_BOTTOM | vertical_align::TEXT_TOP => alphabetic,
            _ => unreachable!("invalid vertical-align keyword"),
        }
    }

    fn inline_box_alignment_metrics(&self, node: Node) -> VerticalAlignMetrics {
        let style = self.context().style(node);
        let used = self.context().used(node);
        VerticalAlignMetrics {
            baseline: Self::baseline_for_style(style, style.line_height),
            block_size: style.line_height,
            effective_box_block_start_offset: used.border_box_top(false),
            effective_box_block_end_offset: used.border_box_bottom(false),
            line_height: style.line_height,
        }
    }

    pub(crate) fn update_last_line(&mut self) {
        if !self.last_line_needs_update {
            return;
        }
        self.last_line_needs_update = false;
        if self.line_count() == 0 {
            return;
        }
        let line_index = self.line_count() - 1;
        if self.line(line_index).has_block_level_box {
            self.should_advance_to_last_line_box_block_end = false;
            return;
        }
        if !self.line(line_index).fragments.is_empty() {
            self.prepare_to_append_inline_content();
        }

        let containing_style = self.containing_style();
        let current_line_block_size = self.max_block_size_on_current_line.max(containing_style.line_height);
        let start_inline_offset = self
            .context()
            .leftmost_inline_offset_at(self.current_block_offset, current_line_block_size);
        let mut inline_offset = start_inline_offset;
        let mut block_offset = CssPixels::default();
        let excess = self.available_inline_size_for_current_line.to_px_or_zero() - self.line(line_index).inline_length;
        if self.writing_mode != writing_mode::HORIZONTAL_TB {
            block_offset =
                self.available_inline_size_for_current_line.to_px_or_zero() - self.line(line_index).block_length;
        }
        if excess > CssPixels::default() {
            match containing_style.text_align {
                text_align::CENTER | text_align::LIBWEB_CENTER | text_align::LIBWEB_INHERIT_OR_CENTER => {
                    inline_offset += excess / 2;
                }
                text_align::START if containing_style.direction == direction::RTL => inline_offset += excess,
                text_align::END if containing_style.direction == direction::LTR => inline_offset += excess,
                text_align::RIGHT | text_align::LIBWEB_RIGHT => inline_offset += excess,
                text_align::MATCH_PARENT => unreachable!("match-parent must be resolved"),
                _ => {}
            }
        }

        let strut_baseline = Self::baseline_for_style(containing_style, containing_style.line_height);
        let mut should_align_strut = false;
        let mut line_box_baseline = strut_baseline;
        let fragment_count = self.line(line_index).fragments.len();
        for fragment_index in 0..fragment_count {
            let (node, style_source) = {
                let fragment = &self.line(line_index).fragments[fragment_index];
                (fragment.layout_node, fragment.style_source)
            };
            let style = self.context().style(style_source);
            let fragment_baseline = if self.context().facts(node).is_text_node {
                Self::baseline_for_style(style, style.line_height)
            } else {
                crate::formatting_context::box_baseline(
                    self.context().state,
                    &self.context().callbacks,
                    node,
                    crate::formatting_context::BaselineSet::Last,
                )
            };
            self.line_mut(line_index).fragments[fragment_index].baseline = fragment_baseline;
            let adjusted_baseline = if style.vertical_align_is_keyword() {
                fragment_baseline
            } else {
                fragment_baseline + style.vertical_align_value().to_px(style.line_height)
            };
            if adjusted_baseline > line_box_baseline {
                if !self.context().facts(node).is_text_node {
                    should_align_strut |= style.display.is_inline_outside()
                        && style.display.is_flex_inside()
                        && style.vertical_align_is_keyword()
                        && style.vertical_align_keyword() == vertical_align::BASELINE;
                }
                line_box_baseline = adjusted_baseline;
            }
        }

        let strut_start = self.current_block_offset;
        let strut_end = if should_align_strut {
            self.current_block_offset + line_box_baseline + (containing_style.line_height - strut_baseline)
        } else {
            self.current_block_offset + containing_style.line_height
        };
        let mut earliest = strut_start;
        let mut latest = strut_end;

        for fragment_index in 0..fragment_count {
            let snapshot = self.line(line_index).fragments[fragment_index].clone();
            let style = self.context().style(snapshot.style_source);
            let mut metrics = VerticalAlignMetrics {
                baseline: snapshot.baseline,
                block_size: snapshot.physical_vertical_extent(),
                effective_box_block_start_offset: snapshot.border_box_block_start,
                // Intentional old quirk: start is reused as end.
                effective_box_block_end_offset: snapshot.border_box_block_start,
                line_height: style.line_height,
            };
            if snapshot.is_atomic_inline {
                let used = self.context().used(snapshot.layout_node);
                metrics.effective_box_block_start_offset = used.margin_top + used.border_box_top(false);
                metrics.effective_box_block_end_offset = used.margin_bottom + used.border_box_bottom(false);
            }
            let new_inline_offset = inline_offset + snapshot.inline_offset;
            let mut new_block_offset = self.block_offset_for_alignment(style, metrics, line_box_baseline);
            let own_alignment_is_line_relative = style.vertical_align_is_keyword()
                && matches!(
                    style.vertical_align_keyword(),
                    vertical_align::TOP | vertical_align::BOTTOM
                );
            let containing_block = self.context().containing_block;
            let mut ancestor = if snapshot.style_source == containing_block {
                std::ptr::null_mut()
            } else {
                self.context().parent_node(snapshot.style_source)
            };
            while !own_alignment_is_line_relative
                && !ancestor.is_null()
                && self.context().facts(ancestor).is_fragmented_inline
                && ancestor != containing_block
            {
                let ancestor_style = self.context().style(ancestor);
                if ancestor_style.vertical_align_is_keyword() {
                    if ancestor_style.vertical_align_keyword() == vertical_align::BASELINE {
                        ancestor = self.context().parent_node(ancestor);
                        continue;
                    }
                    if matches!(
                        ancestor_style.vertical_align_keyword(),
                        vertical_align::TOP | vertical_align::BOTTOM
                    ) {
                        break;
                    }
                }
                let ancestor_metrics = self.inline_box_alignment_metrics(ancestor);
                let baseline_style = ancestor_style.with_vertical_align_keyword(vertical_align::BASELINE);
                new_block_offset +=
                    self.block_offset_for_alignment(ancestor_style, ancestor_metrics, line_box_baseline)
                        - self.block_offset_for_alignment(baseline_style, ancestor_metrics, line_box_baseline);
                ancestor = self.context().parent_node(ancestor);
            }
            {
                let fragment = &mut self.line_mut(line_index).fragments[fragment_index];
                fragment.inline_offset = new_inline_offset;
                fragment.block_offset = new_block_offset.floor() + block_offset;
            }

            let fragment = self.line(line_index).fragments[fragment_index].clone();
            let (inline_box_start, mut inline_box_end) = if fragment.is_atomic_inline {
                let used = self.context().used(fragment.layout_node);
                (
                    fragment.block_offset - (used.margin_top + used.border_box_top(false)),
                    fragment.block_offset
                        + used.content_block_size
                        + used.margin_bottom
                        + used.border_box_bottom(false),
                )
            } else {
                let typographic = CssPixels::nearest_value_for_f32(style.font_ascent() + style.font_descent());
                let half_leading = (style.line_height - typographic) / 2;
                (
                    fragment.block_offset + fragment.baseline
                        - CssPixels::nearest_value_for_f32(style.font_ascent())
                        - half_leading,
                    fragment.block_offset
                        + fragment.baseline
                        + CssPixels::nearest_value_for_f32(style.font_descent())
                        + half_leading,
                )
            };
            if !style.vertical_align_is_keyword() {
                inline_box_end += style.vertical_align_value().to_px(style.line_height);
            }
            earliest = earliest.min(inline_box_start);
            latest = latest.max(inline_box_end);

            if self.context().facts(fragment.layout_node).is_text_node
                && self.writing_mode == writing_mode::HORIZONTAL_TB
            {
                let font_box_size = Self::normal_line_height(style);
                let font_baseline = Self::baseline_for_style(style, font_box_size);
                let fragment_mut = &mut self.line_mut(line_index).fragments[fragment_index];
                fragment_mut.block_offset += fragment_mut.baseline - font_baseline;
                fragment_mut.baseline = font_baseline;
                fragment_mut.block_length = font_box_size;
            }
        }

        let current_block_offset = self.current_block_offset;
        {
            let line = self.line_mut(line_index);
            for marker in &mut line.static_position_markers {
                marker.inline_offset += inline_offset;
                marker.block_offset += block_offset + current_block_offset;
            }
            line.block_length = latest - earliest;
            line.block_end = current_block_offset + line.block_length;
            line.baseline = line_box_baseline;
        }
        self.should_advance_to_last_line_box_block_end = should_align_strut;
    }

    pub(crate) fn remove_last_line_if_empty(&mut self) {
        if self.line_count() != 0 && self.line(self.line_count() - 1).is_empty() {
            self.context().line_data_mut().line_boxes.pop();
            self.last_line_needs_update = false;
        }
    }

    pub(crate) fn recalculate_available_space(&mut self) {
        let current_line_block_size = self
            .max_block_size_on_current_line
            .max(self.containing_style().line_height);
        self.available_inline_size_for_current_line = self
            .context()
            .available_space_for_line(self.current_block_offset, current_line_block_size);
        if self.line_count() != 0 {
            let line_index = self.line_count() - 1;
            self.line_mut(line_index).original_available_inline_size = self.available_inline_size_for_current_line;
        }
    }

    pub(crate) fn did_introduce_clearance(&mut self, clearance: CssPixels) {
        if clearance <= self.current_block_offset {
            return;
        }
        if self.line_count() > 1 {
            let previous = self.line_count() - 2;
            self.line_mut(previous).block_end = clearance;
        }
        self.current_block_offset = clearance;
    }

    pub(crate) fn set_trailing_whitespace_on_previous_line(&mut self) {
        if self.line_count() < 2 {
            return;
        }
        let previous = self.line_count() - 2;
        if let Some(fragment) = self.line_mut(previous).fragments.last_mut() {
            fragment.has_trailing_whitespace = true;
        }
    }

    pub(crate) fn current_block_offset(&self) -> CssPixels {
        self.current_block_offset
    }

    pub(crate) fn set_unbreakable_run_inline_size_interrupted_by_float(&mut self, inline_size: CssPixels) {
        self.unbreakable_run_inline_size_interrupted_by_float = inline_size;
    }
}

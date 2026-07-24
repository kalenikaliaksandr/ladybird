/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

use crate::css_pixels::CssPixels;
use crate::fc::inline::{InlineFormattingContext, Node};
use std::collections::HashMap;
use std::ffi::c_void;

pub(crate) const EDGE_TOP: u8 = 1 << 0;
pub(crate) const EDGE_RIGHT: u8 = 1 << 1;
pub(crate) const EDGE_BOTTOM: u8 = 1 << 2;
pub(crate) const EDGE_LEFT: u8 = 1 << 3;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct CssPixelRect {
    pub(crate) x: CssPixels,
    pub(crate) y: CssPixels,
    pub(crate) width: CssPixels,
    pub(crate) height: CssPixels,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct InlineBoxPieceData {
    pub(crate) node: *mut c_void,
    pub(crate) first_fragment_index: u32,
    pub(crate) fragment_count: u32,
    pub(crate) border_box_rect: CssPixelRect,
    pub(crate) present_edges: u8,
    pub(crate) is_geometry_only_placeholder: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct StagedPiece {
    pub(crate) piece: InlineBoxPieceData,
    pub(crate) line_index: u32,
    pub(crate) depth: u32,
    pub(crate) discovery_index: usize,
}

pub(crate) fn sort_for_emission(pieces: &mut [StagedPiece]) {
    pieces.sort_by_key(|piece| (piece.line_index, piece.depth, piece.discovery_index));
}

#[derive(Clone, Debug)]
struct PerLine {
    line_index: usize,
    has_contributions: bool,
    contributions_inline_start: CssPixels,
    contributions_inline_end: CssPixels,
    first_direct_fragment_block_start: Option<CssPixels>,
    max_direct_fragment_block_length: CssPixels,
    fallback_block_start_from_contributions: Option<CssPixels>,
    interrupting_block_position: Option<(CssPixels, CssPixels)>,
    first_fragment_index: Option<u32>,
    fragment_count: u32,
}

impl PerLine {
    fn new(line_index: usize) -> Self {
        Self {
            line_index,
            has_contributions: false,
            contributions_inline_start: CssPixels::default(),
            contributions_inline_end: CssPixels::default(),
            first_direct_fragment_block_start: None,
            max_direct_fragment_block_length: CssPixels::default(),
            fallback_block_start_from_contributions: None,
            interrupting_block_position: None,
            first_fragment_index: None,
            fragment_count: 0,
        }
    }
}

#[derive(Clone, Debug)]
struct PerNode {
    node: Node,
    parent_index: Option<usize>,
    depth: u32,
    lines: Vec<PerLine>,
    first_line_with_content: Option<usize>,
    last_line_with_content: Option<usize>,
}

fn ensure_line(per_node: &mut PerNode, line_index: usize) -> &mut PerLine {
    let insertion = per_node.lines.partition_point(|line| line.line_index <= line_index);
    if insertion != 0 && per_node.lines[insertion - 1].line_index == line_index {
        return &mut per_node.lines[insertion - 1];
    }
    per_node.lines.insert(insertion, PerLine::new(line_index));
    &mut per_node.lines[insertion]
}

fn note_contribution(line: &mut PerLine, inline_start: CssPixels, inline_end: CssPixels, block_start: CssPixels) {
    if !line.has_contributions || inline_start < line.contributions_inline_start {
        line.fallback_block_start_from_contributions = Some(block_start);
    }
    if !line.has_contributions {
        line.has_contributions = true;
        line.contributions_inline_start = inline_start;
        line.contributions_inline_end = inline_end;
    } else {
        line.contributions_inline_start = line.contributions_inline_start.min(inline_start);
        line.contributions_inline_end = line.contributions_inline_end.max(inline_end);
    }
}

fn nesting_depth(context: &InlineFormattingContext, node: Node) -> u32 {
    let mut depth = 1;
    let mut ancestor = context.nearest_fragmented_inline_ancestor(node);
    while !ancestor.is_null() {
        depth += 1;
        ancestor = context.nearest_fragmented_inline_ancestor(ancestor);
    }
    depth
}

fn edge_bits(horizontal: bool, low: bool, high: bool) -> u8 {
    let mut edges = if horizontal {
        EDGE_TOP | EDGE_BOTTOM
    } else {
        EDGE_LEFT | EDGE_RIGHT
    };
    if low {
        edges |= if horizontal { EDGE_LEFT } else { EDGE_TOP };
    }
    if high {
        edges |= if horizontal { EDGE_RIGHT } else { EDGE_BOTTOM };
    }
    edges
}

pub(crate) fn compute(context: &InlineFormattingContext) -> Vec<InlineBoxPieceData> {
    let horizontal =
        context.style(context.containing_block).writing_mode == crate::css_enums::writing_mode::HORIZONTAL_TB;
    let mut per_nodes = Vec::<PerNode>::new();
    let mut node_to_index = HashMap::<usize, usize>::new();

    let mut committed_fragment_index = 0u32;
    for (line_index, line) in context.line_data().line_boxes.iter().enumerate() {
        for fragment in &line.fragments {
            if fragment.is_fully_truncated {
                continue;
            }
            let fragment_index = committed_fragment_index;
            committed_fragment_index += 1;
            let interrupting = context.style(fragment.style_source).display.is_block_outside();
            let position = fragment.offset();
            let size = fragment.size();
            let mut inline_start = if horizontal { position.0 } else { position.1 };
            let mut inline_end = inline_start + if horizontal { size.0 } else { size.1 };
            let block_start = if horizontal { position.1 } else { position.0 };
            let block_length = if horizontal { size.1 } else { size.0 };

            if !interrupting && fragment.is_atomic_inline {
                let pointer = context.try_used_pointer(fragment.layout_node);
                if !pointer.is_null() {
                    // SAFETY: This state-owned entry is read only for the pass.
                    let used = unsafe { &*pointer };
                    if horizontal {
                        inline_start -= used.margin_left + used.border_box_left(false);
                        inline_end += used.margin_right + used.border_box_right(false);
                    } else {
                        inline_start -= used.margin_top + used.border_box_top(false);
                        inline_end += used.margin_bottom + used.border_box_bottom(false);
                    }
                }
            }

            let mut direct = true;
            let mut previous: Option<usize> = None;
            let mut ancestor = context.nearest_fragmented_inline_ancestor(fragment.layout_node);
            while !ancestor.is_null() {
                let node_index = if let Some(index) = node_to_index.get(&(ancestor as usize)) {
                    *index
                } else {
                    let index = per_nodes.len();
                    per_nodes.push(PerNode {
                        node: ancestor,
                        parent_index: None,
                        depth: nesting_depth(context, ancestor),
                        lines: Vec::new(),
                        first_line_with_content: None,
                        last_line_with_content: None,
                    });
                    node_to_index.insert(ancestor as usize, index);
                    index
                };
                if let Some(previous) = previous {
                    per_nodes[previous].parent_index = Some(node_index);
                }
                previous = Some(node_index);
                let per_node = &mut per_nodes[node_index];
                let line = ensure_line(per_node, line_index);
                let first = *line.first_fragment_index.get_or_insert(fragment_index);
                line.fragment_count = fragment_index + 1 - first;
                if interrupting {
                    line.interrupting_block_position.get_or_insert(position);
                    ancestor = context.nearest_fragmented_inline_ancestor(ancestor);
                    continue;
                }
                if direct {
                    note_contribution(line, inline_start, inline_end, block_start);
                    line.first_direct_fragment_block_start.get_or_insert(block_start);
                    line.max_direct_fragment_block_length = line.max_direct_fragment_block_length.max(block_length);
                } else if line.fallback_block_start_from_contributions.is_none() {
                    line.fallback_block_start_from_contributions = Some(block_start);
                }
                per_node.first_line_with_content.get_or_insert(line_index);
                per_node.last_line_with_content = Some(line_index);
                direct = false;
                ancestor = context.nearest_fragmented_inline_ancestor(ancestor);
            }
        }
    }

    let without_fragments: Vec<_> = context
        .fragmented_inlines_in_pre_order
        .iter()
        .copied()
        .filter(|node| context.facts(*node).has_dom_node && !node_to_index.contains_key(&(*node as usize)))
        .collect();

    let mut staged = Vec::<StagedPiece>::new();
    let mut deepest_first: Vec<_> = (0..per_nodes.len()).collect();
    deepest_first.sort_by(|left, right| {
        per_nodes[*right]
            .depth
            .cmp(&per_nodes[*left].depth)
            .then_with(|| left.cmp(right))
    });

    for node_index in deepest_first {
        let snapshot = per_nodes[node_index].clone();
        let pointer = context.try_used_pointer(snapshot.node);
        let used = if pointer.is_null() {
            None
        } else {
            // SAFETY: The stable entry is read only.
            Some(unsafe { &*pointer })
        };
        let reversed = context.facts(snapshot.node).inline_axis_is_reverse;
        let (
            border_padding_low,
            border_padding_high,
            border_padding_block_low,
            border_padding_block_high,
            margin_low,
            margin_high,
        ) = if let Some(used) = used {
            if horizontal {
                (
                    used.border_box_left(false),
                    used.border_box_right(false),
                    used.border_box_top(false),
                    used.border_box_bottom(false),
                    used.margin_left,
                    used.margin_right,
                )
            } else {
                (
                    used.border_box_top(false),
                    used.border_box_bottom(false),
                    used.border_box_left(false),
                    used.border_box_right(false),
                    used.margin_top,
                    used.margin_bottom,
                )
            }
        } else {
            (
                CssPixels::default(),
                CssPixels::default(),
                CssPixels::default(),
                CssPixels::default(),
                CssPixels::default(),
                CssPixels::default(),
            )
        };

        for line in snapshot.lines {
            if !line.has_contributions {
                if let Some(position) = line.interrupting_block_position {
                    staged.push(StagedPiece {
                        piece: InlineBoxPieceData {
                            node: snapshot.node,
                            first_fragment_index: line.first_fragment_index.unwrap_or(0),
                            fragment_count: line.fragment_count,
                            border_box_rect: CssPixelRect {
                                x: position.0,
                                y: position.1,
                                ..Default::default()
                            },
                            present_edges: edge_bits(horizontal, true, true),
                            is_geometry_only_placeholder: true,
                        },
                        line_index: line.line_index as u32,
                        depth: snapshot.depth,
                        discovery_index: node_index,
                    });
                }
                continue;
            }
            let first = snapshot.first_line_with_content == Some(line.line_index);
            let last = snapshot.last_line_with_content == Some(line.line_index);
            let has_low_edge = if reversed { last } else { first };
            let has_high_edge = if reversed { first } else { last };
            let content_block_start = line
                .first_direct_fragment_block_start
                .or(line.fallback_block_start_from_contributions)
                .unwrap_or_default();
            let content_block_length = if line.first_direct_fragment_block_start.is_some() {
                line.max_direct_fragment_block_length
            } else {
                context.style(snapshot.node).line_height
            };
            let border_inline_start = line.contributions_inline_start
                - if has_low_edge {
                    border_padding_low
                } else {
                    CssPixels::default()
                };
            let border_inline_end = line.contributions_inline_end
                + if has_high_edge {
                    border_padding_high
                } else {
                    CssPixels::default()
                };
            let border_block_start = content_block_start - border_padding_block_low;
            let border_block_length = content_block_length + border_padding_block_low + border_padding_block_high;
            let rect = if horizontal {
                CssPixelRect {
                    x: border_inline_start,
                    y: border_block_start,
                    width: border_inline_end - border_inline_start,
                    height: border_block_length,
                }
            } else {
                CssPixelRect {
                    x: border_block_start,
                    y: border_inline_start,
                    width: border_block_length,
                    height: border_inline_end - border_inline_start,
                }
            };
            staged.push(StagedPiece {
                piece: InlineBoxPieceData {
                    node: snapshot.node,
                    first_fragment_index: line.first_fragment_index.unwrap_or(0),
                    fragment_count: line.fragment_count,
                    border_box_rect: rect,
                    present_edges: edge_bits(horizontal, has_low_edge, has_high_edge),
                    is_geometry_only_placeholder: false,
                },
                line_index: line.line_index as u32,
                depth: snapshot.depth,
                discovery_index: node_index,
            });
            if let Some(parent_index) = snapshot.parent_index {
                let parent_line = ensure_line(&mut per_nodes[parent_index], line.line_index);
                note_contribution(
                    parent_line,
                    border_inline_start - if has_low_edge { margin_low } else { CssPixels::default() },
                    border_inline_end
                        + if has_high_edge {
                            margin_high
                        } else {
                            CssPixels::default()
                        },
                    content_block_start,
                );
            }
        }
    }

    for (index, node) in without_fragments.into_iter().enumerate() {
        if context.try_used_pointer(node).is_null() {
            continue;
        }
        let line_height = context.style(node).line_height;
        staged.push(StagedPiece {
            piece: InlineBoxPieceData {
                node,
                first_fragment_index: 0,
                fragment_count: 0,
                border_box_rect: if horizontal {
                    CssPixelRect {
                        height: line_height,
                        ..Default::default()
                    }
                } else {
                    CssPixelRect {
                        width: line_height,
                        ..Default::default()
                    }
                },
                present_edges: edge_bits(horizontal, true, true),
                is_geometry_only_placeholder: true,
            },
            line_index: 0,
            depth: nesting_depth(context, node),
            discovery_index: per_nodes.len() + index,
        });
    }
    sort_for_emission(&mut staged);
    staged.into_iter().map(|staged| staged.piece).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

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

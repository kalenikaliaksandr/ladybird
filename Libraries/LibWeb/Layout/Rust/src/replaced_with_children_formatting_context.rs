/*
 * Copyright (c) 2026, Gregory Bertilson <gregory@ladybird.org>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

use super::sizing::SizingContext;
use super::{FcFrame, FfiFormattingContextType};
use crate::css_pixels::CssPixels;
use crate::geometry::{AvailableSize, AvailableSpace, LayoutInput};
use crate::used_values::FfiCssPixelPoint;

pub(super) fn run(frame: &mut FcFrame, layout_input: LayoutInput) {
    let available_space = layout_input.available_space;
    // Mark the replaced element as having definite dimensions when the parent FC has
    // computed them from intrinsic size, so children with percentage sizes can resolve.
    let root_facts = frame.state.box_facts(&frame.callbacks, frame.box_);
    let (content_inline_size, root_has_definite_block_size, root_content_block_size) = {
        let root_state = frame.state.used_values(&frame.callbacks, frame.box_);
        if root_facts.has_definite_natural_width {
            root_state.has_definite_inline_size.set(true);
        }
        if root_facts.has_definite_natural_height {
            root_state.has_definite_block_size.set(true);
        }
        (
            root_state.content_inline_size.get(),
            root_state.has_definite_block_size(),
            root_state.content_block_size.get(),
        )
    };

    // For the block axis, use the parent-set content block size if it has been resolved (e.g. an
    // explicit or intrinsic block size); otherwise use the parent formatting context's space.
    let child_available_block_size = if root_has_definite_block_size {
        AvailableSize::definite(root_content_block_size)
    } else {
        available_space.block_size
    };

    let child_available_space = AvailableSpace {
        inline_size: AvailableSize::definite(content_inline_size),
        block_size: child_available_block_size,
    };

    // The TreeBuilder wraps shadow DOM children in an anonymous BlockContainer.
    // Delegate layout to a BFC for that wrapper.
    let mut wrapper = frame.callbacks.first_child(frame.box_);
    while !wrapper.is_null() {
        if frame.state.box_facts(&frame.callbacks, wrapper).is_block_container {
            break;
        }
        wrapper = frame.callbacks.next_sibling(wrapper);
    }
    if wrapper.is_null() {
        return;
    }

    let wrapper_constraints = SizingContext::new(frame.state, frame.callbacks)
        .constraints_for_child_context(frame.box_, layout_input.containing_block_constraints);
    let wrapper_state = frame
        .state
        .create_used_values(&frame.callbacks, wrapper, wrapper_constraints);
    wrapper_state.set_content_inline_size(content_inline_size);

    let mut bfc = super::create_formatting_context(
        frame.state,
        wrapper,
        super::FcParents::default(),
        FfiFormattingContextType::Block as u8,
        frame.layout_mode,
        frame.should_collect_devtools_layout_data,
        frame.callbacks,
    );
    super::run_formatting_context(
        &mut bfc,
        LayoutInput {
            available_space: child_available_space,
            containing_block_constraints: wrapper_constraints,
            has_content_box_position_in_bfc_root: false,
            content_box_position_in_bfc_root: FfiCssPixelPoint::default(),
            table_grid_min_border_box_block_size: None,
        },
        None,
        None,
    );

    frame.automatic_content_inline_size = content_inline_size;
    frame.automatic_content_block_size = bfc.automatic_content_block_size;

    wrapper_state.set_content_block_size(frame.automatic_content_block_size);

    super::place_child(frame.state, &frame.callbacks, wrapper, FfiCssPixelPoint::default());

    super::parent_did_dimension(&mut bfc);
}

pub(super) fn run_noop(frame: &mut FcFrame) {
    // FIXME: This is a hack. Get rid of it.
    frame.automatic_content_inline_size = CssPixels::default();
    frame.automatic_content_block_size = CssPixels::default();
}

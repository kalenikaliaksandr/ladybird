/*
 * Copyright (c) 2026, Gregory Bertilson <gregory@ladybird.org>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

use super::sizing::{Node, SizingContext};
use super::{FfiFormattingContextType, FormattingContextInstance};
use crate::box_facts::FfiLayoutNavCallback;
use crate::css_pixels::CssPixels;
use crate::geometry::{AvailableSize, AvailableSpace, FfiLayoutInput};
use crate::layout_state::state_mut;
use crate::used_values::FfiCssPixelPoint;
use std::ffi::c_void;

fn navigate(instance: &FormattingContextInstance, callback: FfiLayoutNavCallback, node: Node) -> Node {
    // SAFETY: Navigation is synchronous and all layout nodes remain live
    // throughout the layout pass.
    unsafe { callback(instance.callbacks.navigation.context, node) }
}

pub(super) fn run(instance: &mut FormattingContextInstance, layout_input: FfiLayoutInput) {
    let available_space = layout_input.available_space;
    // Mark the replaced element as having definite dimensions when the parent FC has
    // computed them from intrinsic size, so children with percentage sizes can resolve.
    let root_facts = state_mut(instance.state).box_facts(&instance.callbacks, instance.box_);
    let (content_inline_size, root_has_definite_block_size, root_content_block_size) = {
        let root_state = state_mut(instance.state).used_values(&instance.callbacks, instance.box_);
        // SAFETY: The state owns stable used-values entries for the entire pass.
        let root_state = unsafe { &mut *root_state };
        if root_facts.has_definite_natural_width {
            root_state.has_definite_inline_size = true;
        }
        if root_facts.has_definite_natural_height {
            root_state.has_definite_block_size = true;
        }
        (
            root_state.content_inline_size,
            root_state.has_definite_block_size(),
            root_state.content_block_size,
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
    let mut wrapper = navigate(instance, instance.callbacks.navigation.first_child, instance.box_);
    while !wrapper.is_null() {
        if state_mut(instance.state)
            .box_facts(&instance.callbacks, wrapper)
            .is_block_container
        {
            break;
        }
        wrapper = navigate(instance, instance.callbacks.navigation.next_sibling, wrapper);
    }
    if wrapper.is_null() {
        return;
    }

    let wrapper_constraints = SizingContext::new(instance.state, instance.callbacks)
        .constraints_for_child_context(instance.box_, layout_input.containing_block_constraints);
    let wrapper_state = state_mut(instance.state).create_used_values(&instance.callbacks, wrapper, wrapper_constraints);
    assert!(!wrapper_state.is_null());
    // SAFETY: The newly created entry is uniquely initialized here.
    let wrapper_state = unsafe { &mut *wrapper_state };
    wrapper_state.set_content_inline_size(content_inline_size);

    let parent_rust_fc = instance as *mut FormattingContextInstance as *mut c_void;
    let mut bfc = super::create_formatting_context(
        instance.state,
        wrapper,
        parent_rust_fc,
        FfiFormattingContextType::Block as u8,
        instance.layout_mode,
        instance.should_collect_devtools_layout_data,
        instance.callbacks,
    );
    super::run_formatting_context(
        &mut bfc,
        FfiLayoutInput {
            available_space: child_available_space,
            containing_block_constraints: wrapper_constraints,
            has_content_box_position_in_bfc_root: false,
            content_box_position_in_bfc_root: FfiCssPixelPoint::default(),
            has_table_grid_min_border_box_block_size: false,
            table_grid_min_border_box_block_size: CssPixels::default(),
        },
    );

    instance.automatic_content_inline_size = content_inline_size;
    instance.automatic_content_block_size = bfc.automatic_content_block_size;

    wrapper_state.set_content_block_size(instance.automatic_content_block_size);

    super::place_child(
        instance.state,
        &instance.callbacks,
        wrapper,
        FfiCssPixelPoint::default(),
    );

    super::parent_did_dimension(&mut bfc);
}

pub(super) fn parent_did_dimension(instance: &mut FormattingContextInstance) {
    let box_ = instance.box_;
    super::abspos::layout_children_for_instance(instance, box_);
}

pub(super) fn run_noop(instance: &mut FormattingContextInstance) {
    // FIXME: This is a hack. Get rid of it.
    instance.automatic_content_inline_size = CssPixels::default();
    instance.automatic_content_block_size = CssPixels::default();
}

/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

use crate::css_pixels::CssPixels;
use crate::display::FfiDisplay;
use std::ffi::c_void;

#[derive(Clone, Copy)]
#[repr(C)]
pub struct FfiLayoutBoxFacts {
    pub is_box: bool,
    pub is_block_container: bool,
    pub is_replaced_box: bool,
    pub is_replaced_box_with_children: bool,
    pub is_floating: bool,
    pub is_absolutely_positioned: bool,
    pub is_inline: bool,
    pub is_inline_block: bool,
    pub children_are_inline: bool,
    pub is_anonymous: bool,
    pub can_have_children: bool,
    pub has_replaced_element_table_display_adjustment: bool,
    pub creates_block_formatting_context: bool,

    pub has_definite_natural_width: bool,
    pub natural_width: CssPixels,
    pub has_definite_natural_height: bool,
    pub natural_height: CssPixels,
    pub has_definite_natural_aspect_ratio: bool,
    pub natural_aspect_ratio: f64,

    pub layout_index: u32,
    pub display: FfiDisplay,
    pub is_svg_box: bool,
    pub is_svg_svg_box: bool,
    pub is_table_box: bool,
    pub is_table_wrapper: bool,
    pub is_table_row_group: bool,
    pub is_table_header_group: bool,
    pub is_table_footer_group: bool,
    pub is_table_row: bool,
    pub is_table_cell: bool,
    pub is_table_column_group: bool,
    pub is_table_column: bool,
    pub is_table_caption: bool,
    pub is_viewport: bool,

    pub document_in_quirks_mode: bool,
    pub is_html_html_element: bool,
    pub is_html_body_element: bool,
}

impl Default for FfiLayoutBoxFacts {
    fn default() -> Self {
        Self {
            is_box: false,
            is_block_container: false,
            is_replaced_box: false,
            is_replaced_box_with_children: false,
            is_floating: false,
            is_absolutely_positioned: false,
            is_inline: false,
            is_inline_block: false,
            children_are_inline: false,
            is_anonymous: false,
            can_have_children: false,
            has_replaced_element_table_display_adjustment: false,
            creates_block_formatting_context: false,
            has_definite_natural_width: false,
            natural_width: CssPixels::default(),
            has_definite_natural_height: false,
            natural_height: CssPixels::default(),
            has_definite_natural_aspect_ratio: false,
            natural_aspect_ratio: 0.0,
            layout_index: 0,
            display: FfiDisplay::block(),
            is_svg_box: false,
            is_svg_svg_box: false,
            is_table_box: false,
            is_table_wrapper: false,
            is_table_row_group: false,
            is_table_header_group: false,
            is_table_footer_group: false,
            is_table_row: false,
            is_table_cell: false,
            is_table_column_group: false,
            is_table_column: false,
            is_table_caption: false,
            is_viewport: false,
            document_in_quirks_mode: false,
            is_html_html_element: false,
            is_html_body_element: false,
        }
    }
}

pub type FfiLayoutNavCallback = unsafe extern "C" fn(context: *mut c_void, node: *mut c_void) -> *mut c_void;

#[derive(Clone, Copy)]
#[repr(C)]
pub struct FfiLayoutNavCallbacks {
    pub context: *mut c_void,
    pub parent: FfiLayoutNavCallback,
    pub first_child: FfiLayoutNavCallback,
    pub next_sibling: FfiLayoutNavCallback,
    pub previous_sibling: FfiLayoutNavCallback,
    pub containing_block: FfiLayoutNavCallback,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[repr(C)]
    struct FakeNode {
        parent: *mut FakeNode,
        first_child: *mut FakeNode,
        next_sibling: *mut FakeNode,
        previous_sibling: *mut FakeNode,
        containing_block: *mut FakeNode,
    }

    unsafe extern "C" fn parent(_: *mut c_void, node: *mut c_void) -> *mut c_void {
        unsafe { (*node.cast::<FakeNode>()).parent.cast() }
    }

    unsafe extern "C" fn first_child(_: *mut c_void, node: *mut c_void) -> *mut c_void {
        unsafe { (*node.cast::<FakeNode>()).first_child.cast() }
    }

    unsafe extern "C" fn next_sibling(_: *mut c_void, node: *mut c_void) -> *mut c_void {
        unsafe { (*node.cast::<FakeNode>()).next_sibling.cast() }
    }

    unsafe extern "C" fn previous_sibling(_: *mut c_void, node: *mut c_void) -> *mut c_void {
        unsafe { (*node.cast::<FakeNode>()).previous_sibling.cast() }
    }

    unsafe extern "C" fn containing_block(_: *mut c_void, node: *mut c_void) -> *mut c_void {
        unsafe { (*node.cast::<FakeNode>()).containing_block.cast() }
    }

    #[test]
    fn navigation_vtable_exposes_exactly_the_five_links() {
        let callbacks = FfiLayoutNavCallbacks {
            context: std::ptr::null_mut(),
            parent,
            first_child,
            next_sibling,
            previous_sibling,
            containing_block,
        };
        let mut relative = FakeNode {
            parent: std::ptr::null_mut(),
            first_child: std::ptr::null_mut(),
            next_sibling: std::ptr::null_mut(),
            previous_sibling: std::ptr::null_mut(),
            containing_block: std::ptr::null_mut(),
        };
        let relative_pointer = (&raw mut relative).cast::<c_void>();
        let mut node = FakeNode {
            parent: relative_pointer.cast(),
            first_child: relative_pointer.cast(),
            next_sibling: relative_pointer.cast(),
            previous_sibling: relative_pointer.cast(),
            containing_block: relative_pointer.cast(),
        };
        let node_pointer = (&raw mut node).cast::<c_void>();

        for callback in [
            callbacks.parent,
            callbacks.first_child,
            callbacks.next_sibling,
            callbacks.previous_sibling,
            callbacks.containing_block,
        ] {
            assert_eq!(unsafe { callback(callbacks.context, node_pointer) }, relative_pointer);
        }
    }
}

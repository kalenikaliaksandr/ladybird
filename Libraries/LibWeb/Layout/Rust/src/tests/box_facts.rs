use crate::box_facts::*;
use std::ffi::c_void;

#[repr(C)]
struct FakeNode {
    parent: *mut FakeNode,
    first_child: *mut FakeNode,
    next_sibling: *mut FakeNode,
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

unsafe extern "C" fn containing_block(_: *mut c_void, node: *mut c_void) -> *mut c_void {
    unsafe { (*node.cast::<FakeNode>()).containing_block.cast() }
}

#[test]
fn navigation_vtable_exposes_exactly_the_four_links() {
    let callbacks = FfiLayoutNavCallbacks {
        context: std::ptr::null_mut(),
        parent,
        first_child,
        next_sibling,
        containing_block,
    };
    let mut relative = FakeNode {
        parent: std::ptr::null_mut(),
        first_child: std::ptr::null_mut(),
        next_sibling: std::ptr::null_mut(),
        containing_block: std::ptr::null_mut(),
    };
    let relative_pointer = (&raw mut relative).cast::<c_void>();
    let mut node = FakeNode {
        parent: relative_pointer.cast(),
        first_child: relative_pointer.cast(),
        next_sibling: relative_pointer.cast(),
        containing_block: relative_pointer.cast(),
    };
    let node_pointer = (&raw mut node).cast::<c_void>();

    for callback in [
        callbacks.parent,
        callbacks.first_child,
        callbacks.next_sibling,
        callbacks.containing_block,
    ] {
        assert_eq!(unsafe { callback(callbacks.context, node_pointer) }, relative_pointer);
    }
}

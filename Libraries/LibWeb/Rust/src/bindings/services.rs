/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! The services of LibWeb's C++ bindings that the Rust bindings use, which the bindings generator defines in
//! Bindings/IntrinsicDefinitions.cpp.

use libjs_embedding::abi::{JSObject, JSRealm};

unsafe extern "C" {
    /// The interface prototype object of the interface in the realm, which the realm's intrinsics create on first use.
    /// The interface name is a Web::Bindings::InterfaceName of an interface implemented in Rust.
    pub(crate) fn web_bindings_ensure_web_prototype(realm: *mut JSRealm, interface_name: u16) -> *mut JSObject;
}

#[cfg(test)]
mod stubs {
    use libjs_embedding::abi::{JSObject, JSRealm};

    #[unsafe(no_mangle)]
    unsafe extern "C" fn web_bindings_ensure_web_prototype(_: *mut JSRealm, _: u16) -> *mut JSObject {
        unreachable!("web_bindings_ensure_web_prototype is not available without LibWeb")
    }
}

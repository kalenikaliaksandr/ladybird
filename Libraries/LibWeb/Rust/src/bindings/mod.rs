/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! The runtime of the WebIDL bindings that Meta/Generators/generate_libweb_bindings.py writes in Rust, for the
//! interfaces that their IDL marks [ImplementedIn=Rust]. The bindings define a trait for each such interface, which its
//! implementation in this crate implements, and the native functions, constructor and prototype of the interface.

pub(crate) mod conversions;
#[cfg(web_idl_bindings)]
pub(crate) mod generated;
pub(crate) mod interface;
pub(crate) mod messages;
pub(crate) mod services;

use libjs_embedding::completion::Thrown;
use libjs_embedding::error::{throw_range_error, throw_type_error};
use libjs_embedding::realm::Realm;
use libjs_embedding::vm::Vm;

/// What the bindings pass the implementation of an interface: the VM, and the realm that the implementation creates
/// its results in. For an attribute or an operation, that is the realm of the object it runs on; for a constructor,
/// the current realm.
pub(crate) struct CallContext {
    vm: Vm,
    realm: Realm,
}

impl CallContext {
    pub(crate) fn new(vm: Vm, realm: Realm) -> Self {
        Self { vm, realm }
    }

    pub(crate) fn vm(&self) -> Vm {
        self.vm
    }

    pub(crate) fn realm(&self) -> Realm {
        self.realm
    }
}

/// An exception that the implementation of an interface throws.
pub(crate) enum Exception {
    /// A value that JavaScript code threw.
    Thrown(Thrown),
    /// A simple exception, which the bindings create in the current realm.
    TypeError(String),
    RangeError(String),
}

impl From<Thrown> for Exception {
    fn from(thrown: Thrown) -> Self {
        Self::Thrown(thrown)
    }
}

impl Exception {
    pub(crate) fn throw(self, vm: Vm) -> Thrown {
        match self {
            Self::Thrown(thrown) => thrown,
            Self::TypeError(message) => throw_type_error(vm, &message),
            Self::RangeError(message) => throw_range_error(vm, &message),
        }
    }
}

pub(crate) type WebIdlResult<T> = Result<T, Exception>;

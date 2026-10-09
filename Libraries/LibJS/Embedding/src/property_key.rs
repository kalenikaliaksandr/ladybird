/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! Property keys. A key crosses the ABI as the one word of the runtime's own property key: the raw word of an
//! AK::Utf16FlyString for a string, which AK interns process-wide, or the address of a symbol with bit 1 set.

use core::marker::PhantomData;
use core::ptr::NonNull;

use ak::Utf16FlyString;

use crate::abi::{JSPropertyKey, JSSymbol, JSWellKnownSymbol, js_symbol_well_known};
use crate::vm::Vm;

const SYMBOL_FLAG: usize = 2;

/// A property key that borrows the string or symbol it names.
pub struct PropertyKey<'a> {
    key: JSPropertyKey,
    _borrow: PhantomData<&'a ()>,
}

impl<'a> PropertyKey<'a> {
    /// A string key. The string must not be a canonical array index such as "0", which the runtime keys by its
    /// number.
    pub fn from_fly_string(string: &'a Utf16FlyString) -> Self {
        Self {
            key: JSPropertyKey {
                bits: string.raw_identity(),
            },
            _borrow: PhantomData,
        }
    }

    pub(crate) fn as_abi(&self) -> *const JSPropertyKey {
        &raw const self.key
    }
}

impl PropertyKey<'static> {
    /// A symbol key. The VM keeps the well-known symbols alive.
    pub fn well_known_symbol(vm: Vm, symbol: JSWellKnownSymbol) -> Self {
        // SAFETY: The VM is live, and every well-known symbol exists.
        let symbol: NonNull<JSSymbol> = NonNull::new(unsafe { js_symbol_well_known(vm.as_raw(), symbol) })
            .expect("the runtime has the well-known symbol");
        Self {
            key: JSPropertyKey {
                bits: symbol.as_ptr() as usize | SYMBOL_FLAG,
            },
            _borrow: PhantomData,
        }
    }
}

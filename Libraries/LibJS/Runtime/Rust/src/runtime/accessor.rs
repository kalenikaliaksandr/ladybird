/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

use core::cell::Cell;

use crate::gc::class::{GcCell, define_cell};
use crate::gc::visitor::{Trace, Visitor};
use crate::interpreter::vm::Vm;
pub use crate::layout::accessor::Accessor;
use crate::layout::cell::{CellHeader, Gc};
use crate::layout::function_object::FunctionObject;
use crate::runtime::symbol::Symbol;

define_cell!(Accessor, Accessor);

// SAFETY: Visits the getter, the setter and the key of the cached value, which are all the cells an accessor holds.
unsafe impl Trace for Accessor {
    fn trace(&self, visitor: &mut Visitor) {
        self.getter.trace(visitor);
        self.setter.trace(visitor);
        self.cached_value_key.trace(visitor);
    }
}

impl Accessor {
    pub fn create(
        vm: &Vm,
        getter: Option<Gc<FunctionObject>>,
        setter: Option<Gc<FunctionObject>>,
        cached_value_key: Option<Gc<Symbol>>,
    ) -> Gc<Accessor> {
        assert!(cached_value_key.is_none_or(|cached_value_key| cached_value_key.is_private()));
        vm.heap().allocate(Accessor {
            header: CellHeader::for_class(Self::CLASS),
            getter: Cell::new(getter),
            setter: Cell::new(setter),
            // Cached accessor values live in private properties on the holder object.
            cached_value_key: Cell::new(cached_value_key),
        })
    }

    pub fn getter(&self) -> Option<Gc<FunctionObject>> {
        self.getter.get()
    }

    pub fn set_getter(&self, getter: Option<Gc<FunctionObject>>) {
        self.getter.set(getter);
    }

    pub fn setter(&self) -> Option<Gc<FunctionObject>> {
        self.setter.get()
    }

    pub fn set_setter(&self, setter: Option<Gc<FunctionObject>>) {
        self.setter.set(setter);
    }

    pub fn cached_value_key(&self) -> Option<Gc<Symbol>> {
        self.cached_value_key.get()
    }

    pub fn set_cached_value_key(&self, cached_value_key: Option<Gc<Symbol>>) {
        assert!(cached_value_key.is_none_or(|cached_value_key| cached_value_key.is_private()));
        self.cached_value_key.set(cached_value_key);
    }
}

#[cfg(all(test, libjs_runtime_tests_with_libgc))]
mod tests {
    use super::*;
    use crate::layout::value::Value;

    #[test]
    fn accessors_keep_their_cached_value_key_alive() {
        let vm = Vm::create();
        let accessor = Accessor::create(&vm, None, None, Some(Symbol::create_private(&vm)));
        let value = Value::from_accessor(accessor);
        vm.heap().collect_garbage();
        assert!(value.is_accessor());
        let accessor = value.as_accessor();
        assert!(accessor.getter().is_none() && accessor.setter().is_none());
        assert!(accessor.cached_value_key().is_some_and(|key| key.is_private()));
        accessor.set_cached_value_key(None);
        assert!(accessor.cached_value_key().is_none());
    }
}

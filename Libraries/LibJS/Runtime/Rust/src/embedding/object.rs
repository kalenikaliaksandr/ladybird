/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! Objects, their properties and their internal methods.

use crate::layout::cell::Gc;
use crate::layout::value::Value;
use crate::runtime::object::HostIntrinsicAccessor;
use crate::runtime::realm::Realm;

pub fn call_host_intrinsic_accessor(accessor: HostIntrinsicAccessor, realm: Gc<Realm>) -> Value {
    // SAFETY: The embedder defined the accessor for a property of an object of this runtime, and computes its value in
    //         the realm of that object.
    Value(unsafe { accessor(realm.as_ptr()) })
}

#[cfg(all(test, libjs_runtime_tests_with_libgc))]
mod tests {
    use core::cell::Cell;

    use crate::interpreter::vm::Vm;
    use crate::layout::value::Value;
    use crate::runtime::completion::Must;
    use crate::runtime::property_attributes::DEFAULT_ATTRIBUTES;
    use crate::runtime::realm::Realm;
    use crate::runtime::realm::test_realm::{TestRealm, key, own_keys};

    std::thread_local! {
        static REALMS_SEEN_BY_THE_ACCESSOR: Cell<Vec<*mut Realm>> = const { Cell::new(Vec::new()) };
    }

    unsafe extern "C" fn host_intrinsic_accessor(realm: *mut Realm) -> u64 {
        REALMS_SEEN_BY_THE_ACCESSOR.with(|realms| {
            let mut seen = realms.take();
            seen.push(realm);
            realms.set(seen);
        });
        Value::from_i32(42).0
    }

    #[test]
    fn a_host_intrinsic_accessor_computes_its_property_when_first_read() {
        let vm = Vm::create();
        let test_realm = TestRealm::new(&vm);
        let object = test_realm.object();
        object.define_host_intrinsic_accessor(&vm, &key("answer"), DEFAULT_ATTRIBUTES, host_intrinsic_accessor);
        assert_eq!(own_keys(&vm, &object), "answer");
        assert!(REALMS_SEEN_BY_THE_ACCESSOR.take().is_empty());

        assert_eq!(object.get(&vm, &key("answer")).must(), Value::from_i32(42));
        assert_eq!(object.get(&vm, &key("answer")).must(), Value::from_i32(42));
        assert_eq!(REALMS_SEEN_BY_THE_ACCESSOR.take(), [test_realm.realm.as_ptr()]);
    }
}

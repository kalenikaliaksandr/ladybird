/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

use core::cell::Cell;

use crate::gc::class::{GcCell, define_cell};
use crate::gc::visitor::{Trace, Visitor};
use crate::interpreter::runtime_functions::unimplemented_runtime_function;
use crate::interpreter::vm::Vm;
use crate::layout::cell::{CellHeader, Gc};
use crate::layout::environment::{DeclarativeEnvironment, GlobalEnvironment};
use crate::layout::object::Object;
pub use crate::layout::realm::Realm;
use crate::runtime::shape::Shape;

/// The parts of a realm the interpreter does not read. [[HostDefined]] comes with the hosts that define it.
#[derive(Default)]
pub struct RealmStorage {
    /// The intrinsics the unit tests stand in for the realm's intrinsics, which do not exist yet.
    #[cfg(test)]
    pub test_intrinsics: test_intrinsics::TestIntrinsics,
}

define_cell!(Realm, Other);

// SAFETY: Visits every cell a realm holds.
unsafe impl Trace for Realm {
    fn trace(&self, visitor: &mut Visitor) {
        self.intrinsics.trace(visitor);
        self.global_object.trace(visitor);
        self.global_environment.trace(visitor);
        self.global_declarative_environment.trace(visitor);
        #[cfg(test)]
        self.storage.test_intrinsics.trace(visitor);
    }
}

/// Defines an accessor for each intrinsic the object model asks the realm for. Until the realm has its intrinsics,
/// each stops the process with the intrinsic's name.
macro_rules! define_intrinsic_accessors {
    ($($name:ident: $type:ty => $description:literal,)*) => {
        impl Realm {
            $(
                pub fn $name(&self) -> Gc<$type> {
                    #[cfg(test)]
                    if let Some(intrinsic) = self.storage.test_intrinsics.$name.get() {
                        return intrinsic;
                    }
                    unimplemented_runtime_function(concat!("the realm intrinsic ", $description), 0)
                }
            )*
        }

        #[cfg(test)]
        pub mod test_intrinsics {
            use super::*;

            #[derive(Default)]
            pub struct TestIntrinsics {
                $(pub $name: Cell<Option<Gc<$type>>>,)*
            }

            // SAFETY: Visits every intrinsic.
            unsafe impl Trace for TestIntrinsics {
                fn trace(&self, visitor: &mut Visitor) {
                    $(self.$name.trace(visitor);)*
                }
            }
        }
    };
}

define_intrinsic_accessors! {
    empty_object_shape: Shape => "empty object shape",
    new_object_shape: Shape => "new object shape",
    object_prototype: Object => "%Object.prototype%",
    array_prototype: Object => "%Array.prototype%",
    string_prototype: Object => "%String.prototype%",
    number_prototype: Object => "%Number.prototype%",
    boolean_prototype: Object => "%Boolean.prototype%",
    bigint_prototype: Object => "%BigInt.prototype%",
    symbol_prototype: Object => "%Symbol.prototype%",
}

impl Realm {
    /// A realm without intrinsics, a global object or a global environment, which
    /// InitializeHostDefinedRealm goes on to create.
    pub fn create(vm: &Vm) -> Gc<Realm> {
        vm.heap().allocate(Realm {
            header: CellHeader::for_class(Self::CLASS),
            global_object: Cell::new(None),
            global_declarative_environment: Cell::new(None),
            global_environment: Cell::new(None),
            intrinsics: Cell::new(None),
            storage: RealmStorage::default(),
        })
    }

    pub fn global_object(&self) -> Gc<Object> {
        self.global_object.get().expect("the realm has a global object")
    }

    pub fn set_global_object(&self, global: Gc<Object>) {
        self.global_object.set(Some(global));
    }

    pub fn global_environment(&self) -> Gc<GlobalEnvironment> {
        self.global_environment
            .get()
            .expect("the realm has a global environment")
    }

    pub fn global_declarative_environment(&self) -> Gc<DeclarativeEnvironment> {
        self.global_declarative_environment
            .get()
            .expect("the realm has a global environment")
    }
}

/// A realm with the intrinsics the object model asks for, created the way C++ Intrinsics creates them, running in an
/// execution context of its own until it is dropped.
#[cfg(all(test, libjs_runtime_tests_with_libgc))]
pub mod test_realm {
    use super::*;
    use crate::runtime::array::Array;
    use crate::runtime::completion::Must;
    use crate::runtime::object::Object;

    pub struct TestRealm<'vm> {
        vm: &'vm Vm,
        pub realm: Gc<Realm>,
        stack_mark: *mut u8,
    }

    impl<'vm> TestRealm<'vm> {
        pub fn new(vm: &'vm Vm) -> Self {
            let realm = Realm::create(vm);
            let intrinsics = &realm.storage.test_intrinsics;
            intrinsics.empty_object_shape.set(Some(Shape::create(vm, realm)));
            let object_prototype = Object::create_prototype(vm, realm, None);
            intrinsics.object_prototype.set(Some(object_prototype));
            let new_object_shape = Shape::create(vm, realm);
            new_object_shape.set_prototype_without_transition(vm, object_prototype);
            intrinsics.new_object_shape.set(Some(new_object_shape));
            let array_prototype = Array::create(vm, realm, 0, Some(object_prototype)).must();
            intrinsics.array_prototype.set(Some(array_prototype.upcast()));

            let stack = vm.interpreter_stack();
            let stack_mark = stack.top.get();
            let context = stack.allocate(0, 0, 0).expect("the interpreter stack has room");
            // SAFETY: The context was just allocated.
            unsafe { context.as_ref() }.realm.set(Some(realm));
            vm.push_execution_context(context);
            Self { vm, realm, stack_mark }
        }

        /// A new ordinary object whose prototype is %Object.prototype%, like `{}`.
        pub fn object(&self) -> Gc<Object> {
            Object::create(self.vm, self.realm, Some(self.realm.object_prototype()))
        }

        pub fn array(&self, elements: &[crate::layout::value::Value]) -> Gc<Array> {
            Array::create_from(self.vm, self.realm, elements)
        }
    }

    impl Drop for TestRealm<'_> {
        fn drop(&mut self) {
            self.vm.pop_execution_context();
            self.vm.interpreter_stack().deallocate(self.stack_mark);
        }
    }

    pub fn key(name: &str) -> crate::runtime::property_key::PropertyKey {
        crate::runtime::property_key::PropertyKey::from_utf8(name)
    }

    fn key_values_to_string(keys: &crate::gc::root::MarkedVec<'_, crate::layout::value::Value>) -> String {
        (0..keys.len())
            .map(|index| {
                let key = keys.get(index).expect("the index is in bounds");
                if key.is_symbol() {
                    crate::utf16::Utf16View::of_string(&key.as_symbol().descriptive_string()).to_utf8()
                } else {
                    key.as_string().to_utf8()
                }
            })
            .collect::<Vec<_>>()
            .join(",")
    }

    /// Reflect.ownKeys(object).map(String).join(",")
    pub fn own_keys(vm: &Vm, object: &Object) -> String {
        key_values_to_string(&object.internal_own_property_keys(vm).must())
    }

    /// Object.keys(object).join(",")
    pub fn enumerable_keys(vm: &Vm, object: &Object) -> String {
        key_values_to_string(
            &object
                .enumerable_own_property_names(vm, crate::runtime::object::PropertyKind::Key)
                .must(),
        )
    }

    /// Runs `operation`, which is expected to throw, and returns the message of the error the runtime stops at,
    /// since throwing stops the process until realms have their error constructors.
    pub fn thrown_message<T>(operation: impl FnOnce() -> T) -> String {
        let previous_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(operation));
        std::panic::set_hook(previous_hook);
        let Err(payload) = result else {
            panic!("the operation did not throw");
        };
        payload
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| payload.downcast_ref::<&str>().map(|message| (*message).to_string()))
            .unwrap_or_default()
    }
}

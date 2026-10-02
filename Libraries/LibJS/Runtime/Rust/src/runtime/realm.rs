/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

use core::cell::Cell;

use crate::gc::class::{Extends, GcCell, define_cell};
use crate::gc::visitor::{Trace, Visitor};
use crate::interpreter::execution_context::OwnedExecutionContext;
use crate::interpreter::vm::Vm;
use crate::layout::accessor::Accessor;
use crate::layout::cell::{CellHeader, Gc};
use crate::layout::environment::{DeclarativeEnvironment, GlobalEnvironment};
use crate::layout::function_object::FunctionObject;
use crate::layout::object::Object;
pub use crate::layout::realm::Realm;
use crate::runtime::completion::ThrowCompletionOr;
use crate::runtime::global_object::{GlobalObject, set_default_global_bindings};
use crate::runtime::intrinsics::Intrinsics;
use crate::runtime::object::allocate_object;
use crate::runtime::shape::Shape;

/// The parts of a realm the interpreter does not read. [[HostDefined]] comes with the hosts that define it.
#[derive(Default)]
pub struct RealmStorage {}

define_cell!(Realm, Other);

// SAFETY: Visits every cell a realm holds.
unsafe impl Trace for Realm {
    fn trace(&self, visitor: &mut Visitor) {
        self.intrinsics.trace(visitor);
        self.global_object.trace(visitor);
        self.global_environment.trace(visitor);
        self.global_declarative_environment.trace(visitor);
    }
}

/// Accessors for the intrinsics CreateIntrinsics creates up front, which the object model asks the realm for.
macro_rules! realm_intrinsic_accessors {
    (cells { $($name:ident: $type:ty,)* } offsets { $($offset:ident,)* }) => {
        impl Realm {
            $(
                pub fn $name(&self) -> Gc<$type> {
                    self.intrinsics().$name()
                }
            )*
            $(
                pub fn $offset(&self) -> u32 {
                    self.intrinsics().$offset()
                }
            )*
        }
    };
}

realm_intrinsic_accessors! {
    cells {
        empty_object_shape: Shape,
        new_object_shape: Shape,
        iterator_result_object_shape: Shape,
        normal_function_prototype_shape: Shape,
        normal_function_shape: Shape,
        async_function_shape: Shape,
        generator_function_shape: Shape,
        async_generator_function_shape: Shape,
        native_function_shape: Shape,
        unmapped_arguments_object_shape: Shape,
        mapped_arguments_object_shape: Shape,
        regexp_builtin_exec_array_shape: Shape,
        throw_type_error_accessor: Accessor,
        throw_type_error_function: FunctionObject,
        array_prototype_values_function: FunctionObject,
        eval_function: FunctionObject,
        array_iterator_prototype: Object,
    }
    offsets {
        iterator_result_object_value_offset,
        iterator_result_object_done_offset,
        normal_function_prototype_constructor_offset,
        normal_function_length_offset,
        normal_function_name_offset,
        generator_function_prototype_property_offset,
        native_function_length_offset,
        native_function_name_offset,
        unmapped_arguments_object_length_offset,
        unmapped_arguments_object_well_known_symbol_iterator_offset,
        unmapped_arguments_object_callee_offset,
        mapped_arguments_object_length_offset,
        mapped_arguments_object_well_known_symbol_iterator_offset,
        mapped_arguments_object_callee_offset,
        regexp_builtin_exec_array_index_offset,
        regexp_builtin_exec_array_input_offset,
        regexp_builtin_exec_array_groups_offset,
    }
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

    // 9.3.1 InitializeHostDefinedRealm ( ), https://tc39.es/ecma262/#sec-initializehostdefinedrealm
    #[allow(clippy::unnecessary_wraps, reason = "the operation can throw in the spec")]
    pub fn initialize_host_defined_realm(
        vm: &Vm,
        create_global_object: Option<&dyn Fn(Gc<Realm>) -> Gc<Object>>,
        create_global_this_value: Option<&dyn Fn(Gc<Realm>) -> Gc<Object>>,
    ) -> ThrowCompletionOr<OwnedExecutionContext> {
        // 1. Let realm be a new Realm Record
        let realm = Realm::create(vm);

        // 2. Perform CreateIntrinsics(realm).
        Intrinsics::create(vm, realm);

        // FIXME: 3. Set realm.[[AgentSignifier]] to AgentSignifier().

        // NOTE: Done on step 1.
        // 4. Set realm.[[GlobalObject]] to undefined.
        // 5. Set realm.[[GlobalEnv]] to undefined.

        // FIXME: 6. Set realm.[[TemplateMap]] to a new empty List.

        // 7. Let newContext be a new execution context.
        let new_context = OwnedExecutionContext::create(0, 0, 0);

        // 8. Set the Function of newContext to null.
        new_context.function.set(None);

        // 9. Set the Realm of newContext to realm.
        new_context.realm.set(Some(realm));

        // 10. Set the ScriptOrModule of newContext to null.
        new_context
            .script_or_module
            .set(crate::layout::execution_context::ScriptOrModule::Empty);

        // 11. Push newContext onto the execution context stack; newContext is now the running execution context.
        vm.push_execution_context(new_context.as_non_null());

        // 12. If the host requires use of an exotic object to serve as realm's global object, then
        let global = if let Some(create_global_object) = create_global_object {
            // a. Let global be such an object created in a host-defined manner.
            create_global_object(realm)
        }
        // 13. Else,
        else {
            // a. Let global be OrdinaryObjectCreate(realm.[[Intrinsics]].[[%Object.prototype%]]).
            // NOTE: We allocate a proper GlobalObject directly as this plain object is
            //       turned into one via SetDefaultGlobalBindings in the spec.
            allocate_object(vm, GlobalObject::new(vm, GlobalObject::CLASS, realm)).upcast()
        };

        // 14. If the host requires that the this binding in realm's global scope return an object other than the global object, then
        let this_value = if let Some(create_global_this_value) = create_global_this_value {
            // a. Let thisValue be such an object created in a host-defined manner.
            create_global_this_value(realm)
        }
        // 15. Else,
        else {
            // a. Let thisValue be global.
            global
        };

        // 16. Set realm.[[GlobalObject]] to global.
        realm.global_object.set(Some(global));

        // 17. Set realm.[[GlobalEnv]] to NewGlobalEnvironment(global, thisValue).
        realm.set_global_environment(GlobalEnvironment::create(vm, global, this_value));

        // 18. Perform ? SetDefaultGlobalBindings(realm).
        set_default_global_bindings(vm, realm);

        // 19. Create any host-defined global object properties on global.
        global.initialize(vm, realm);

        // 20. Return unused.
        Ok(new_context)
    }

    /// Realm::create<T>(): allocates an object and runs its initialize(), which defines the properties of built-in
    /// objects.
    pub fn create_object<T: GcCell + Extends<Object>>(&self, vm: &Vm, object: T) -> Gc<T> {
        let object = allocate_object(vm, object);
        object.upcast::<Object>().initialize(vm, self.as_gc());
        object
    }

    fn as_gc(&self) -> Gc<Realm> {
        // SAFETY: Realms only exist as cells, since Realm::create() allocates every one.
        unsafe { Gc::from_ref(self) }
    }

    pub fn intrinsics(&self) -> Gc<Intrinsics> {
        self.intrinsics.get().expect("the realm has its intrinsics")
    }

    pub fn set_intrinsics(&self, intrinsics: Gc<Intrinsics>) {
        assert!(self.intrinsics.get().is_none());
        self.intrinsics.set(Some(intrinsics));
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

    pub fn set_global_environment(&self, environment: Gc<GlobalEnvironment>) {
        self.global_environment.set(Some(environment));
        self.global_declarative_environment
            .set(Some(environment.declarative_record()));
    }

    pub fn global_declarative_environment(&self) -> Gc<DeclarativeEnvironment> {
        self.global_declarative_environment
            .get()
            .expect("the realm has a global environment")
    }

    pub fn string_prototype(&self, vm: &Vm) -> Gc<Object> {
        self.intrinsics().string_prototype(vm)
    }

    pub fn number_prototype(&self, vm: &Vm) -> Gc<Object> {
        self.intrinsics().number_prototype(vm)
    }

    pub fn boolean_prototype(&self, vm: &Vm) -> Gc<Object> {
        self.intrinsics().boolean_prototype(vm)
    }

    pub fn bigint_prototype(&self, vm: &Vm) -> Gc<Object> {
        self.intrinsics().bigint_prototype(vm)
    }

    pub fn symbol_prototype(&self, vm: &Vm) -> Gc<Object> {
        self.intrinsics().symbol_prototype(vm)
    }
}

/// A realm with its intrinsics, made by CreateIntrinsics but without a global object, running in an execution context
/// of its own until it is dropped.
#[cfg(all(test, libjs_runtime_tests_with_libgc))]
pub mod test_realm {
    use super::*;
    use crate::runtime::array::Array;

    pub struct TestRealm<'vm> {
        vm: &'vm Vm,
        pub realm: Gc<Realm>,
        stack_mark: *mut u8,
    }

    impl<'vm> TestRealm<'vm> {
        pub fn new(vm: &'vm Vm) -> Self {
            let realm = Realm::create(vm);
            Intrinsics::create(vm, realm);

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

    impl<'vm> TestRealm<'vm> {
        /// A test realm with the intrinsics that functions are created with, among them %Array.prototype.values%,
        /// which arguments objects are created with.
        pub fn with_function_intrinsics(vm: &'vm Vm) -> Self {
            Self::new(vm)
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
        use crate::runtime::completion::Must;
        key_values_to_string(&object.internal_own_property_keys(vm).must())
    }

    /// Object.keys(object).join(",")
    pub fn enumerable_keys(vm: &Vm, object: &Object) -> String {
        use crate::runtime::completion::Must;
        key_values_to_string(
            &object
                .enumerable_own_property_names(vm, crate::runtime::object::PropertyKind::Key)
                .must(),
        )
    }

    /// Runs `operation`, which is expected to throw, and returns the message of the error it throws. While it runs,
    /// throwing stops the process with "creating a <kind> with the message "<message>"" instead, so that the
    /// operations the tests run outside the interpreter never reach its exception handling.
    pub fn thrown_message<T>(operation: impl FnOnce() -> T) -> String {
        let previous_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let previous_throws_stop_the_process = crate::runtime::error::set_throws_stop_the_process_for_tests(true);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(operation));
        crate::runtime::error::set_throws_stop_the_process_for_tests(previous_throws_stop_the_process);
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

#[cfg(all(test, libjs_runtime_tests_with_libgc))]
mod tests {
    use core::cell::Cell;

    use super::*;
    use crate::layout::value::Value;
    use crate::runtime::completion::Must;
    use crate::runtime::realm::test_realm::own_keys;

    #[test]
    fn initialize_host_defined_realm_runs_a_new_realm_with_its_global_object() {
        let vm = Vm::create();
        assert!(vm.current_realm().is_none());
        let context = Realm::initialize_host_defined_realm(&vm, None, None).must();
        let realm = context.realm.get().expect("the context has the new realm");
        assert!(vm.running_execution_context() == Some(context.as_non_null()));
        assert!(vm.current_realm() == Some(realm));
        assert!(context.function.get().is_none() && context.executable.get().is_none());

        let global = realm.global_object();
        assert!(global.is::<GlobalObject>() && global.has_global_object_flag());
        assert!(global.prototype() == Some(realm.object_prototype()));
        let environment = realm.global_environment();
        assert!(environment.global_this_value() == global);
        assert!(environment.object_record().binding_object() == global);
        assert!(realm.global_declarative_environment() == environment.declarative_record());
        assert!(global.get(&vm, &vm.names.globalThis).must() == Value::from_object(global));
        assert!(own_keys(&vm, &global).starts_with(
            "eval,isFinite,isNaN,parseFloat,parseInt,decodeURI,decodeURIComponent,encodeURI,encodeURIComponent,globalThis,\
             Infinity,NaN,undefined,AggregateError,"
        ));
        assert!(vm.pop_execution_context() == context.as_non_null());
    }

    #[test]
    fn initialize_host_defined_realm_uses_the_hosts_global_object_and_this_value() {
        let vm = Vm::create();
        let calls = Cell::new(0);
        let create_global_object = |realm: Gc<Realm>| {
            calls.set(calls.get() + 1);
            allocate_object(&vm, GlobalObject::new(&vm, GlobalObject::CLASS, realm)).upcast()
        };
        let create_global_this_value = |realm: Gc<Realm>| Object::create(&vm, realm, Some(realm.object_prototype()));
        let context =
            Realm::initialize_host_defined_realm(&vm, Some(&create_global_object), Some(&create_global_this_value))
                .must();
        let realm = context.realm.get().expect("the context has the new realm");
        assert_eq!(calls.get(), 1);
        let global = realm.global_object();
        let this_value = realm.global_environment().global_this_value();
        assert!(this_value != global && !this_value.is::<GlobalObject>());
        // SetDefaultGlobalBindings defines globalThis as the this value of the global environment.
        assert!(global.get(&vm, &vm.names.globalThis).must() == Value::from_object(this_value));
        assert!(
            global.get(&vm, &vm.names.Error).must() == Value::from_object(realm.intrinsics().error_constructor(&vm))
        );

        // Each realm has intrinsics of its own.
        let other_context = Realm::initialize_host_defined_realm(&vm, None, None).must();
        let other_realm = other_context.realm.get().expect("the context has the new realm");
        assert!(other_realm.object_prototype() != realm.object_prototype());
        assert!(vm.current_realm() == Some(other_realm));
        vm.pop_execution_context();
        assert!(vm.current_realm() == Some(realm));
        vm.pop_execution_context();
    }
}

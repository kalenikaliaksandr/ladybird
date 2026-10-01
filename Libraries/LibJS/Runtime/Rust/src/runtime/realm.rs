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
use crate::layout::accessor::Accessor;
use crate::layout::cell::{CellHeader, Gc};
use crate::layout::environment::{DeclarativeEnvironment, GlobalEnvironment};
use crate::layout::function_object::FunctionObject;
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

/// Defines an accessor for each intrinsic the object model asks the realm for, and for each property offset of the
/// premade shapes among them. Until the realm has its intrinsics, each stops the process with the intrinsic's name.
macro_rules! define_intrinsic_accessors {
    (
        cells { $($name:ident: $type:ty => $description:literal,)* }
        offsets { $($offset_name:ident => $offset_description:literal,)* }
    ) => {
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
            $(
                pub fn $offset_name(&self) -> u32 {
                    #[cfg(test)]
                    if let Some(offset) = self.storage.test_intrinsics.$offset_name.get() {
                        return offset;
                    }
                    unimplemented_runtime_function(concat!("the realm intrinsic ", $offset_description), 0)
                }
            )*
        }

        #[cfg(test)]
        pub mod test_intrinsics {
            use super::*;

            #[derive(Default)]
            pub struct TestIntrinsics {
                $(pub $name: Cell<Option<Gc<$type>>>,)*
                $(pub $offset_name: Cell<Option<u32>>,)*
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
    cells {
        empty_object_shape: Shape => "empty object shape",
        new_object_shape: Shape => "new object shape",
        object_prototype: Object => "%Object.prototype%",
        array_prototype: Object => "%Array.prototype%",
        string_prototype: Object => "%String.prototype%",
        number_prototype: Object => "%Number.prototype%",
        boolean_prototype: Object => "%Boolean.prototype%",
        bigint_prototype: Object => "%BigInt.prototype%",
        symbol_prototype: Object => "%Symbol.prototype%",
        function_prototype: Object => "%Function.prototype%",
        generator_function_prototype: Object => "%GeneratorFunction.prototype%",
        async_function_prototype: Object => "%AsyncFunction.prototype%",
        async_generator_function_prototype: Object => "%AsyncGeneratorFunction.prototype%",
        generator_function_prototype_prototype: Object => "%GeneratorFunction.prototype.prototype%",
        async_generator_function_prototype_prototype: Object => "%AsyncGeneratorFunction.prototype.prototype%",
        normal_function_prototype_shape: Shape => "normal function prototype shape",
        normal_function_shape: Shape => "normal function shape",
        async_function_shape: Shape => "async function shape",
        generator_function_shape: Shape => "generator function shape",
        async_generator_function_shape: Shape => "async generator function shape",
        native_function_shape: Shape => "native function shape",
        unmapped_arguments_object_shape: Shape => "unmapped arguments object shape",
        mapped_arguments_object_shape: Shape => "mapped arguments object shape",
        array_prototype_values_function: FunctionObject => "%Array.prototype.values%",
        throw_type_error_accessor: Accessor => "%ThrowTypeError% accessor",
    }
    offsets {
        normal_function_prototype_constructor_offset => "normal function prototype constructor offset",
        normal_function_length_offset => "normal function length offset",
        normal_function_name_offset => "normal function name offset",
        generator_function_prototype_property_offset => "generator function prototype property offset",
        native_function_length_offset => "native function length offset",
        native_function_name_offset => "native function name offset",
        unmapped_arguments_object_length_offset => "unmapped arguments object length offset",
        unmapped_arguments_object_well_known_symbol_iterator_offset => "unmapped arguments object @@iterator offset",
        unmapped_arguments_object_callee_offset => "unmapped arguments object callee offset",
        mapped_arguments_object_length_offset => "mapped arguments object length offset",
        mapped_arguments_object_well_known_symbol_iterator_offset => "mapped arguments object @@iterator offset",
        mapped_arguments_object_callee_offset => "mapped arguments object callee offset",
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

    impl<'vm> TestRealm<'vm> {
        /// A test realm that also has the intrinsics functions and arguments objects are created with, made the way
        /// C++ Intrinsics makes them. %Function.prototype% is an ordinary object here, and %Array.prototype.values%
        /// returns undefined.
        pub fn with_function_intrinsics(vm: &'vm Vm) -> Self {
            use crate::layout::value::Value;
            use crate::runtime::error::ErrorKind;
            use crate::runtime::error_types::ErrorType;
            use crate::runtime::native_function::{NativeFunction, RawNativeFunction, raw_native};
            use crate::runtime::property_attributes::{Attribute, PropertyAttributes};
            use crate::runtime::property_key::PropertyKey;

            let test_realm = Self::new(vm);
            let realm = test_realm.realm;
            let intrinsics = &realm.storage.test_intrinsics;
            let names = &vm.names;
            let object_prototype = realm.object_prototype();
            let configurable = PropertyAttributes::new(Attribute::CONFIGURABLE);
            let writable = PropertyAttributes::new(Attribute::WRITABLE);
            let writable_configurable = PropertyAttributes::new(Attribute::WRITABLE | Attribute::CONFIGURABLE);
            let offset = |shape: Gc<Shape>, property_key: &PropertyKey| {
                Some(
                    shape
                        .lookup(property_key)
                        .expect("the premade shape has the property")
                        .offset,
                )
            };

            let function_prototype = Object::create_prototype(vm, realm, Some(object_prototype));
            intrinsics.function_prototype.set(Some(function_prototype));

            let normal_function_prototype_shape = Shape::create(vm, realm);
            normal_function_prototype_shape.set_prototype_without_transition(vm, object_prototype);
            normal_function_prototype_shape.add_property_without_transition(
                vm,
                &names.constructor,
                writable_configurable,
            );
            intrinsics
                .normal_function_prototype_constructor_offset
                .set(offset(normal_function_prototype_shape, &names.constructor));
            intrinsics
                .normal_function_prototype_shape
                .set(Some(normal_function_prototype_shape));

            let create_function_shape = |prototype: Gc<Object>, has_prototype_property: bool| {
                let shape = Shape::create(vm, realm);
                shape.set_prototype_without_transition(vm, prototype);
                shape.add_property_without_transition(vm, &names.length, configurable);
                shape.add_property_without_transition(vm, &names.name, configurable);
                if has_prototype_property {
                    shape.add_property_without_transition(vm, &names.prototype, writable);
                }
                shape
            };

            let normal_function_shape = create_function_shape(function_prototype, false);
            intrinsics
                .normal_function_length_offset
                .set(offset(normal_function_shape, &names.length));
            intrinsics
                .normal_function_name_offset
                .set(offset(normal_function_shape, &names.name));
            intrinsics.normal_function_shape.set(Some(normal_function_shape));

            let native_function_shape = create_function_shape(function_prototype, false);
            intrinsics
                .native_function_length_offset
                .set(offset(native_function_shape, &names.length));
            intrinsics
                .native_function_name_offset
                .set(offset(native_function_shape, &names.name));
            intrinsics.native_function_shape.set(Some(native_function_shape));

            let iterator = PropertyKey::from(vm.well_known_symbols().iterator);
            let create_arguments_object_shape = |callee_attributes: PropertyAttributes| {
                let shape = Shape::create(vm, realm);
                shape.set_prototype_without_transition(vm, object_prototype);
                shape.set_has_parameter_map();
                shape.add_property_without_transition(vm, &names.length, writable_configurable);
                shape.add_property_without_transition(vm, &iterator, writable_configurable);
                shape.add_property_without_transition(vm, &names.callee, callee_attributes);
                shape
            };

            let unmapped_arguments_object_shape = create_arguments_object_shape(PropertyAttributes::new(0));
            intrinsics
                .unmapped_arguments_object_length_offset
                .set(offset(unmapped_arguments_object_shape, &names.length));
            intrinsics
                .unmapped_arguments_object_well_known_symbol_iterator_offset
                .set(offset(unmapped_arguments_object_shape, &iterator));
            intrinsics
                .unmapped_arguments_object_callee_offset
                .set(offset(unmapped_arguments_object_shape, &names.callee));
            intrinsics
                .unmapped_arguments_object_shape
                .set(Some(unmapped_arguments_object_shape));

            let mapped_arguments_object_shape = create_arguments_object_shape(writable_configurable);
            intrinsics
                .mapped_arguments_object_length_offset
                .set(offset(mapped_arguments_object_shape, &names.length));
            intrinsics
                .mapped_arguments_object_well_known_symbol_iterator_offset
                .set(offset(mapped_arguments_object_shape, &iterator));
            intrinsics
                .mapped_arguments_object_callee_offset
                .set(offset(mapped_arguments_object_shape, &names.callee));
            intrinsics
                .mapped_arguments_object_shape
                .set(Some(mapped_arguments_object_shape));

            let generator_function_prototype = Object::create_prototype(vm, realm, Some(function_prototype));
            let async_function_prototype = Object::create_prototype(vm, realm, Some(function_prototype));
            let async_generator_function_prototype = Object::create_prototype(vm, realm, Some(function_prototype));
            intrinsics
                .generator_function_prototype
                .set(Some(generator_function_prototype));
            intrinsics.async_function_prototype.set(Some(async_function_prototype));
            intrinsics
                .async_generator_function_prototype
                .set(Some(async_generator_function_prototype));
            intrinsics
                .generator_function_prototype_prototype
                .set(Some(Object::create_prototype(vm, realm, Some(object_prototype))));
            intrinsics
                .async_generator_function_prototype_prototype
                .set(Some(Object::create_prototype(vm, realm, Some(object_prototype))));

            let generator_function_shape = create_function_shape(generator_function_prototype, true);
            intrinsics
                .generator_function_prototype_property_offset
                .set(offset(generator_function_shape, &names.prototype));
            intrinsics.generator_function_shape.set(Some(generator_function_shape));
            intrinsics
                .async_function_shape
                .set(Some(create_function_shape(async_function_prototype, false)));
            intrinsics
                .async_generator_function_shape
                .set(Some(create_function_shape(async_generator_function_prototype, true)));

            // 10.2.4.1 %ThrowTypeError% ( ), https://tc39.es/ecma262/#sec-%throwtypeerror%
            let throw_type_error_function = NativeFunction::create(
                vm,
                (),
                |vm, _| vm.throw_completion(ErrorKind::TypeError, ErrorType::RestrictedFunctionPropertiesAccess, &[]),
                0,
                &PropertyKey::from(ak::Utf16FlyString::default()),
                Some(realm),
                None,
                None,
            );
            throw_type_error_function.define_direct_property(
                vm,
                &names.length,
                Value::from_i32(0),
                PropertyAttributes::new(0),
            );
            throw_type_error_function.define_direct_property(
                vm,
                &names.name,
                Value::from_string(vm.empty_string()),
                PropertyAttributes::new(0),
            );
            throw_type_error_function.internal_prevent_extensions(vm).must();
            intrinsics
                .throw_type_error_accessor
                .set(Some(crate::runtime::accessor::Accessor::create(
                    vm,
                    Some(throw_type_error_function.upcast()),
                    Some(throw_type_error_function.upcast()),
                    None,
                )));

            let array_prototype_values = RawNativeFunction::create(
                vm,
                raw_native!(|_| Ok(Value::UNDEFINED)),
                0,
                &names.values,
                Some(realm),
                None,
                None,
            );
            realm.array_prototype().define_direct_property(
                vm,
                &names.values,
                Value::from_object(array_prototype_values),
                writable_configurable,
            );
            intrinsics
                .array_prototype_values_function
                .set(Some(array_prototype_values.upcast()));

            test_realm
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

/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

use core::fmt;
use core::ops::Deref;

use ak::Utf16String;
use libjs_runtime_macros::Trace;

use super::completion::{Throw, ThrowCompletionOr};
use super::error_types::ErrorType;
use crate::gc::class::{Class, GcCell, define_cell};
use crate::interpreter::vm::Vm;
use crate::layout::cell::Gc;
use crate::layout::object::Object;
use crate::layout::value::Value;
use crate::runtime::error_data::{CompactTraceback, ErrorData};
use crate::runtime::object::MayInterfereWithIndexedPropertyAccess;
use crate::runtime::primitive_string::PrimitiveString;
use crate::runtime::property_attributes::{Attribute, PropertyAttributes};
use crate::runtime::realm::Realm;

/// The constructors of the errors the runtime throws: %Error% and the NativeError constructors, as the C++ Error
/// subclasses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    Error,
    EvalError,
    InternalError,
    RangeError,
    ReferenceError,
    SyntaxError,
    TypeError,
    URIError,
}

impl ErrorKind {
    /// T::create(realm, message) for the error class T this kind names: a new error of this kind in `realm`, whose
    /// "message" is `message`.
    pub fn create(self, vm: &Vm, realm: Gc<Realm>, message: Utf16String) -> Gc<Error> {
        match self {
            Self::Error => Error::create_with_message(vm, realm, message),
            Self::EvalError => EvalError::create_with_message(vm, realm, message).upcast(),
            Self::InternalError => InternalError::create_with_message(vm, realm, message).upcast(),
            Self::RangeError => RangeError::create_with_message(vm, realm, message).upcast(),
            Self::ReferenceError => ReferenceError::create_with_message(vm, realm, message).upcast(),
            Self::SyntaxError => SyntaxError::create_with_message(vm, realm, message).upcast(),
            Self::TypeError => TypeError::create_with_message(vm, realm, message).upcast(),
            Self::URIError => URIError::create_with_message(vm, realm, message).upcast(),
        }
    }
}

#[cfg(test)]
std::thread_local! {
    static THROWS_STOP_THE_PROCESS_FOR_TESTS: core::cell::Cell<bool> = const { core::cell::Cell::new(false) };
}

/// Has every throw stop the process with the message of the error it would throw, for the tests that check those
/// messages outside the interpreter. Returns whether throws did so before.
#[cfg(test)]
pub fn set_throws_stop_the_process_for_tests(throws_stop_the_process: bool) -> bool {
    THROWS_STOP_THE_PROCESS_FOR_TESTS.with(|flag| flag.replace(throws_stop_the_process))
}

impl Vm {
    /// 5.2.3.2 Throw an Exception, https://tc39.es/ecma262/#sec-throw-an-exception
    #[cold]
    pub fn throw_completion<T>(
        &self,
        kind: ErrorKind,
        error_type: ErrorType,
        arguments: &[&dyn fmt::Display],
    ) -> ThrowCompletionOr<T> {
        self.throw_completion_with_message(kind, error_type.message(arguments))
    }

    /// Throws a new error of `kind` with `message`.
    #[cold]
    pub fn throw_completion_with_message<T>(&self, kind: ErrorKind, message: String) -> ThrowCompletionOr<T> {
        #[cfg(test)]
        if THROWS_STOP_THE_PROCESS_FOR_TESTS.with(core::cell::Cell::get) {
            crate::interpreter::runtime_functions::unimplemented_runtime_function(
                &format!("creating a {kind:?} with the message \"{message}\""),
                0,
            );
        }

        let realm = if kind == ErrorKind::TypeError {
            self.type_error_realm()
        } else {
            self.current_realm()
        };
        let realm = realm.expect("an error is thrown in an execution context with a realm");
        let completion = kind.create(self, realm, Utf16String::from_utf8(&message));
        Err(Throw::new(Value::from_object(completion)))
    }
}

/// The Error objects, which have an [[ErrorData]] internal slot. The NativeError objects extend it.
#[repr(C)]
#[derive(Trace)]
pub struct Error {
    base: Object,
    error_data: ErrorData,
}

define_cell!(Error, Object, extends: [Object]);

impl Deref for Error {
    type Target = Object;

    fn deref(&self) -> &Object {
        &self.base
    }
}

impl Object {
    /// Whether the object has an [[ErrorData]] internal slot.
    pub fn has_error_data(&self) -> bool {
        self.error_data().is_some()
    }

    pub fn error_data(&self) -> Option<&ErrorData> {
        if !self.is::<Error>() {
            return None;
        }
        // SAFETY: The object is an Error, which starts with its Object.
        let error = unsafe { &*core::ptr::from_ref(self).cast::<Error>() };
        Some(&error.error_data)
    }
}

impl Error {
    /// Error(Object& prototype), for `class`, which is Error or a class that extends it.
    pub fn new(vm: &Vm, class: &'static Class, prototype: Gc<Object>) -> Error {
        Error {
            base: Object::new_with_prototype(vm, class, prototype, MayInterfereWithIndexedPropertyAccess::No),
            error_data: ErrorData::new(vm),
        }
    }

    pub fn create(vm: &Vm, realm: Gc<Realm>) -> Gc<Error> {
        realm.create_object(vm, Error::new(vm, Error::CLASS, realm.intrinsics().error_prototype(vm)))
    }

    pub fn create_with_message(vm: &Vm, realm: Gc<Realm>, message: Utf16String) -> Gc<Error> {
        let error = Error::create(vm, realm);
        error.set_message(vm, message);
        error
    }

    pub fn stack_string(&self, compact: CompactTraceback) -> Utf16String {
        self.error_data.stack_string(compact)
    }

    // 20.5.8.1 InstallErrorCause ( O, options ), https://tc39.es/ecma262/#sec-installerrorcause
    pub fn install_error_cause(&self, vm: &Vm, options: Value) -> ThrowCompletionOr<()> {
        // 1. If Type(options) is Object and ? HasProperty(options, "cause") is true, then
        if options.is_object() && options.as_object().has_property(vm, &vm.names.cause)? {
            // a. Let cause be ? Get(options, "cause").
            let cause = options.as_object().get(vm, &vm.names.cause)?;

            // b. Perform CreateNonEnumerableDataPropertyOrThrow(O, "cause", cause).
            self.create_non_enumerable_data_property_or_throw(vm, &vm.names.cause, cause);
        }

        // 2. Return unused.
        Ok(())
    }

    pub fn set_message(&self, vm: &Vm, message: Utf16String) {
        let attributes = PropertyAttributes::new(Attribute::WRITABLE | Attribute::CONFIGURABLE);
        self.define_direct_property(
            vm,
            &vm.names.message,
            Value::from_string(PrimitiveString::create(vm, message)),
            attributes,
        );
    }
}

// NOTE: Making these inherit from Error is not required by the spec but
//       our way of implementing the [[ErrorData]] internal slot, which is
//       used in Object.prototype.toString().
macro_rules! define_native_errors {
    ($($class:ident: $prototype:ident;)*) => {
        $(
            #[repr(C)]
            #[derive(Trace)]
            pub struct $class {
                base: Error,
            }

            define_cell!($class, Object, extends: [Error, Object]);

            impl Deref for $class {
                type Target = Error;

                fn deref(&self) -> &Error {
                    &self.base
                }
            }

            impl $class {
                pub fn new(vm: &Vm, prototype: Gc<Object>) -> $class {
                    $class {
                        base: Error::new(vm, Self::CLASS, prototype),
                    }
                }

                pub fn create(vm: &Vm, realm: Gc<Realm>) -> Gc<$class> {
                    realm.create_object(vm, $class::new(vm, realm.intrinsics().$prototype(vm)))
                }

                pub fn create_with_message(vm: &Vm, realm: Gc<Realm>, message: Utf16String) -> Gc<$class> {
                    let error = $class::create(vm, realm);
                    error.set_message(vm, message);
                    error
                }
            }
        )*
    };
}

define_native_errors! {
    EvalError: eval_error_prototype;
    InternalError: internal_error_prototype;
    RangeError: range_error_prototype;
    ReferenceError: reference_error_prototype;
    SyntaxError: syntax_error_prototype;
    TypeError: type_error_prototype;
    URIError: uri_error_prototype;
}

/// Running scripts the way the C++ js binary runs `js -c`, with "eval" as their file name.
#[cfg(all(test, libjs_runtime_tests_with_libgc))]
pub mod test_scripts {
    use super::*;
    use crate::script::Script;

    pub fn run_script(vm: &Vm, realm: Gc<Realm>, source: &str) -> ThrowCompletionOr<Value> {
        let source: Vec<u16> = source.encode_utf16().collect();
        let parsed = libjs_rust::compile::parse(&source, libjs_rust::ast::ProgramType::Script, 1);
        assert!(!parsed.has_errors(), "the script parses");
        let script =
            Script::compile_parsed_program_with_filename(vm, parsed, &source, realm, Utf16String::from_utf8("eval"));
        vm.run_script(script, None)
    }

    pub fn utf8(value: Value) -> String {
        crate::utf16::Utf16View::of_string(&value.to_utf16_string_without_side_effects()).to_utf8()
    }
}

#[cfg(all(test, libjs_runtime_tests_with_libgc))]
mod tests {
    use super::test_scripts::{run_script, utf8};
    use super::*;
    use crate::runtime::abstract_operations::{call, construct};
    use crate::runtime::completion::Must;
    use crate::runtime::native_function::NativeFunction;
    use crate::runtime::property_key::PropertyKey;
    use crate::runtime::realm::test_realm::{key, own_keys, thrown_message};
    use crate::utf16::Utf16View;
    use crate::utilities::initialize_realm;

    fn string(vm: &Vm, text: &str) -> Value {
        Value::from_string(PrimitiveString::create_from_utf8(vm, text))
    }

    fn thrown(vm: &Vm, kind: ErrorKind, error_type: ErrorType, arguments: &[&dyn fmt::Display]) -> Gc<Object> {
        let thrown = vm
            .throw_completion::<()>(kind, error_type, arguments)
            .expect_err("throw_completion throws");
        assert!(thrown.value().is_object());
        thrown.value().as_object()
    }

    fn message_of(vm: &Vm, error: Gc<Object>) -> String {
        utf8(error.get(vm, &vm.names.message).must())
    }

    /// "Name: message" of the error an operation threw. Throws in raw native functions cannot be turned into panics
    /// for thrown_message(), since panics cannot unwind through their C calling convention.
    fn thrown_error<T>(vm: &Vm, completion: ThrowCompletionOr<T>) -> String {
        let Err(thrown) = completion else {
            panic!("the operation did not throw");
        };
        let error = thrown.value().as_object();
        assert!(error.has_error_data());
        format!(
            "{}: {}",
            utf8(error.get(vm, &vm.names.name).must()),
            message_of(vm, error)
        )
    }

    fn checks_errors_thrown_by_the_runtime(vm: &Vm) {
        let root_execution_context = initialize_realm(vm);
        let realm = root_execution_context.realm();
        let intrinsics = realm.intrinsics();

        let kinds = [
            (ErrorKind::Error, "Error", intrinsics.error_prototype(vm)),
            (ErrorKind::EvalError, "EvalError", intrinsics.eval_error_prototype(vm)),
            (
                ErrorKind::InternalError,
                "InternalError",
                intrinsics.internal_error_prototype(vm),
            ),
            (
                ErrorKind::RangeError,
                "RangeError",
                intrinsics.range_error_prototype(vm),
            ),
            (
                ErrorKind::ReferenceError,
                "ReferenceError",
                intrinsics.reference_error_prototype(vm),
            ),
            (
                ErrorKind::SyntaxError,
                "SyntaxError",
                intrinsics.syntax_error_prototype(vm),
            ),
            (ErrorKind::TypeError, "TypeError", intrinsics.type_error_prototype(vm)),
            (ErrorKind::URIError, "URIError", intrinsics.uri_error_prototype(vm)),
        ];
        for (kind, class, prototype) in kinds {
            let error = thrown(vm, kind, ErrorType::InvalidLength, &[&"array"]);
            assert_eq!(error.class().name, class);
            assert!(error.has_error_data() && error.is::<Error>());
            assert!(error.prototype() == Some(prototype));
            assert_eq!(own_keys(vm, &error), "message");
            assert_eq!(message_of(vm, error), "Invalid array length");
            let message = error
                .internal_get_own_property(vm, &vm.names.message)
                .must()
                .expect("the error has a message");
            assert!(message.writable == Some(true) && message.enumerable == Some(false));
            assert!(message.configurable == Some(true));
            let name = error.get(vm, &vm.names.name).must();
            assert_eq!(utf8(name), class);
        }

        // An error created outside a script has no frames to show but the bottom one, which the stack leaves out.
        let error = TypeError::create_with_message(vm, realm, Utf16String::from_utf8("no frames"));
        assert_eq!(utf8(error.get(vm, &vm.names.stack).must()), "TypeError: no frames\n");
        assert!(!realm.object_prototype().has_error_data());
        assert!(!intrinsics.error_prototype(vm).has_error_data());
    }

    #[test]
    fn the_runtime_throws_errors_of_the_current_realm() {
        let vm = Vm::create();
        checks_errors_thrown_by_the_runtime(&vm);
    }

    #[test]
    fn the_runtime_throws_errors_when_collecting_garbage_on_every_allocation() {
        let vm = Vm::create();
        vm.heap().set_should_collect_on_every_allocation(true);
        checks_errors_thrown_by_the_runtime(&vm);
        vm.heap().set_should_collect_on_every_allocation(false);
    }

    #[test]
    fn a_type_error_realm_scope_overrides_the_realm_of_type_errors_at_its_depth() {
        let vm = Vm::create();
        let root_execution_context = initialize_realm(&vm);
        let realm = root_execution_context.realm();
        // Like $262.createRealm, which creates a realm and leaves its execution context right away.
        let other_realm_context = Realm::initialize_host_defined_realm(&vm, None, None).must();
        let other_realm = other_realm_context.realm.get().expect("the context has a realm");
        vm.pop_execution_context();
        assert!(vm.current_realm() == Some(realm));

        let type_error_prototype = |realm: Gc<Realm>| realm.intrinsics().type_error_prototype(&vm);
        let throw = |kind| thrown(&vm, kind, ErrorType::NotAnObject, &[&"x"]).prototype();
        {
            let _scope = vm.type_error_realm_scope(other_realm);
            assert!(throw(ErrorKind::TypeError) == Some(type_error_prototype(other_realm)));
            assert!(throw(ErrorKind::RangeError) == Some(realm.intrinsics().range_error_prototype(&vm)));

            // The override only applies at the execution context stack depth of its scope.
            let stack = vm.interpreter_stack();
            let stack_mark = stack.top.get();
            let context = stack.allocate(0, 0, 0).expect("the interpreter stack has room");
            // SAFETY: The context was just allocated.
            unsafe { context.as_ref() }.realm.set(Some(realm));
            vm.push_execution_context(context);
            assert!(throw(ErrorKind::TypeError) == Some(type_error_prototype(realm)));
            vm.pop_execution_context();
            stack.deallocate(stack_mark);

            assert!(throw(ErrorKind::TypeError) == Some(type_error_prototype(other_realm)));
        }
        assert!(throw(ErrorKind::TypeError) == Some(type_error_prototype(realm)));
    }

    #[test]
    fn error_constructors_create_errors_with_messages_and_causes() {
        let vm = Vm::create();
        let root_execution_context = initialize_realm(&vm);
        let realm = root_execution_context.realm();
        let intrinsics = realm.intrinsics();
        let error_constructor = intrinsics.error_constructor(&vm);
        let type_error_constructor = intrinsics.type_error_constructor(&vm);

        // Calling an error constructor constructs.
        let called = call(
            &vm,
            Value::from_object(error_constructor),
            Value::UNDEFINED,
            &[string(&vm, "m")],
        )
        .must();
        let called = called.as_object();
        assert!(called.prototype() == Some(intrinsics.error_prototype(&vm)));
        assert_eq!(own_keys(&vm, &called), "message");
        assert_eq!(message_of(&vm, called), "m");

        let options = Object::create(&vm, realm, Some(realm.object_prototype()));
        options
            .create_data_property_or_throw(&vm, &vm.names.cause, Value::from_i32(5))
            .must();
        let with_cause = construct(
            &vm,
            type_error_constructor.upcast(),
            &[Value::from_i32(42), Value::from_object(options)],
            None,
        )
        .must();
        assert!(with_cause.is::<TypeError>());
        assert_eq!(own_keys(&vm, &with_cause), "message,cause");
        assert_eq!(message_of(&vm, with_cause), "42");
        assert!(with_cause.get(&vm, &vm.names.cause).must() == Value::from_i32(5));
        let cause = with_cause
            .internal_get_own_property(&vm, &vm.names.cause)
            .must()
            .expect("the error has a cause");
        assert!(cause.writable == Some(true) && cause.enumerable == Some(false) && cause.configurable == Some(true));

        let without_message = construct(&vm, error_constructor.upcast(), &[Value::UNDEFINED, Value::NULL], None).must();
        assert_eq!(own_keys(&vm, &without_message), "");

        // OrdinaryCreateFromConstructor takes the prototype from the new target, or the realm's intrinsic.
        let new_target = NativeFunction::create(&vm, (), |_, _| Ok(Value::UNDEFINED), 0, &key("t"), None, None, None);
        let custom_prototype = Object::create(&vm, realm, None);
        new_target.define_direct_property(
            &vm,
            &vm.names.prototype,
            Value::from_object(custom_prototype),
            PropertyAttributes::new(Attribute::WRITABLE),
        );
        let custom = construct(&vm, type_error_constructor.upcast(), &[], Some(new_target.upcast())).must();
        assert!(custom.is::<TypeError>() && custom.prototype() == Some(custom_prototype));
        new_target.define_direct_property(
            &vm,
            &vm.names.prototype,
            Value::from_i32(1),
            PropertyAttributes::new(Attribute::WRITABLE),
        );
        let defaulted = construct(&vm, type_error_constructor.upcast(), &[], Some(new_target.upcast())).must();
        assert!(defaulted.prototype() == Some(intrinsics.type_error_prototype(&vm)));

        let is_error = error_constructor.get(&vm, &vm.names.isError).must();
        assert!(call(&vm, is_error, Value::UNDEFINED, &[Value::from_object(defaulted)]).must() == Value::TRUE);
        assert!(call(&vm, is_error, Value::UNDEFINED, &[Value::from_object(options)]).must() == Value::FALSE);
        assert!(call(&vm, is_error, Value::UNDEFINED, &[Value::from_i32(1)]).must() == Value::FALSE);

        // The AggregateError constructor needs the iterator protocol for its errors.
        let aggregate_error_constructor = intrinsics.aggregate_error_constructor(&vm);
        assert!(aggregate_error_constructor.prototype() == Some(error_constructor.upcast()));
        assert!(type_error_constructor.prototype() == Some(error_constructor.upcast()));
        let message = thrown_message(|| construct(&vm, aggregate_error_constructor.upcast(), &[], None));
        assert!(message.contains("GetIterator and IteratorToList"), "{message}");
    }

    #[test]
    fn error_prototype_to_string_follows_the_spec() {
        let vm = Vm::create();
        let root_execution_context = initialize_realm(&vm);
        let realm = root_execution_context.realm();
        let to_string = realm
            .intrinsics()
            .error_prototype(&vm)
            .get(&vm, &vm.names.toString)
            .must();
        let object_with = |name: Option<&str>, message: Option<&str>| {
            let object = Object::create(&vm, realm, Some(realm.object_prototype()));
            if let Some(name) = name {
                object
                    .create_data_property_or_throw(&vm, &vm.names.name, string(&vm, name))
                    .must();
            }
            if let Some(message) = message {
                object
                    .create_data_property_or_throw(&vm, &vm.names.message, string(&vm, message))
                    .must();
            }
            Value::from_object(object)
        };
        let to_string_of = |this: Value| utf8(call(&vm, to_string, this, &[]).must());
        // What Error.prototype.toString.call(...) returns in the C++ js binary.
        assert_eq!(to_string_of(object_with(Some(""), Some("m"))), "m");
        assert_eq!(to_string_of(object_with(Some("N"), Some(""))), "N");
        assert_eq!(to_string_of(object_with(None, None)), "Error");
        assert_eq!(to_string_of(object_with(None, Some("x"))), "Error: x");
        assert_eq!(to_string_of(object_with(Some("N"), Some("x"))), "N: x");
        assert_eq!(
            thrown_error(&vm, call(&vm, to_string, Value::from_i32(1), &[])),
            "TypeError: 1 is not an object"
        );
    }

    #[test]
    fn error_stacks_show_the_frames_the_cpp_runtime_shows() {
        let vm = Vm::create();
        let root_execution_context = initialize_realm(&vm);
        let realm = root_execution_context.realm();
        // What the C++ js binary computes for each script, run with -c.
        let cases = [
            (
                "var s; try { null.x } catch (e) { s = e.stack } s",
                "TypeError: Cannot access property \"x\" on null object\n    at eval:1:18\n",
            ),
            (
                "var s;\ntry {\n    undefined.y;\n} catch (e) {\n    s = e.stack;\n}\ns",
                "TypeError: Cannot access property \"y\" on undefined object \"undefined\"\n    at eval:3:14\n",
            ),
            (
                "var s; try { Function.prototype.caller = 1 } catch (e) { s = e.stack } s",
                "TypeError: Restricted function properties like 'callee', 'caller' and 'arguments' may not be \
                 accessed in strict mode\n    at <unknown>\n    at eval:1:40\n",
            ),
            (
                "var e; try { x } catch (err) { e = err } var first = e.stack; e.message = \"changed\"; \
                 e.name = \"N\"; first === e.stack ? e.stack : \"recomputed\"",
                "ReferenceError: 'x' is not defined\n    at eval:1:14\n",
            ),
        ];
        for (source, expected) in cases {
            assert_eq!(utf8(run_script(&vm, realm, source).must()), expected, "{source}");
        }

        // A frame of a function shows its name, as in "function f() { null.x }; f()".
        run_script(&vm, realm, "function f() { null.x }").must();
        let f = realm.global_object().get(&vm, &key("f")).must();
        let thrown = call(&vm, f, Value::UNDEFINED, &[]).expect_err("f throws");
        let error = thrown.value().as_object();
        let error = error.downcast::<Error>().expect("f throws an error");
        assert_eq!(
            Utf16View::of_string(&error.stack_string(CompactTraceback::No)).to_utf8(),
            "    at f (eval:1:20)\n"
        );

        // The stack setter defines an own data property.
        let stack_setter = realm
            .intrinsics()
            .error_prototype(&vm)
            .internal_get_own_property(&vm, &vm.names.stack)
            .must()
            .expect("Error.prototype has a stack accessor")
            .set
            .flatten()
            .expect("the stack accessor has a setter");
        let object = Object::create(&vm, realm, Some(realm.object_prototype()));
        let result = call(
            &vm,
            Value::from_object(stack_setter),
            Value::from_object(object),
            &[Value::from_i32(3)],
        )
        .must();
        assert!(result == Value::TRUE);
        let stack = object
            .internal_get_own_property(&vm, &PropertyKey::from_utf8("stack"))
            .must()
            .expect("the setter defined a stack");
        assert!(stack.value == Some(Value::from_i32(3)) && stack.enumerable == Some(true));
        assert_eq!(
            thrown_error(
                &vm,
                call(&vm, Value::from_object(stack_setter), Value::from_object(object), &[])
            ),
            "TypeError: set stack() needs one argument"
        );
        assert_eq!(
            thrown_error(
                &vm,
                call(&vm, Value::from_object(stack_setter), Value::from_i32(1), &[])
            ),
            "TypeError: 1 is not an object"
        );
        let stack_getter = realm
            .intrinsics()
            .error_prototype(&vm)
            .internal_get_own_property(&vm, &vm.names.stack)
            .must()
            .and_then(|stack| stack.get.flatten())
            .expect("the stack accessor has a getter");
        assert_eq!(
            thrown_error(
                &vm,
                call(&vm, Value::from_object(stack_getter), Value::from_i32(1), &[])
            ),
            "TypeError: 1 is not an object"
        );
        assert!(
            call(&vm, Value::from_object(stack_getter), Value::from_object(object), &[]).must() == Value::UNDEFINED
        );
    }
}

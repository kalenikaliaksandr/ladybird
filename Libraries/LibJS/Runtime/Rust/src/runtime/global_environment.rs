/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

use core::cell::Cell;
use core::ops::Deref;

use ak::Utf16FlyString;

use crate::gc::class::{GcCell, define_cell};
use crate::gc::visitor::{Trace, Visitor};
use crate::interpreter::vm::Vm;
use crate::layout::cell::Gc;
pub use crate::layout::environment::GlobalEnvironment;
use crate::layout::object::Object;
use crate::layout::value::Value;
use crate::runtime::completion::{Must, ThrowCompletionOr};
use crate::runtime::declarative_environment::DeclarativeEnvironment;
use crate::runtime::environment::{ENVIRONMENT_METHODS, Environment, EnvironmentMethods, InitializeBindingHint};
use crate::runtime::error::ErrorKind;
use crate::runtime::error_types::ErrorType;
use crate::runtime::object::ShouldThrowExceptions;
use crate::runtime::object_environment::{IsWithEnvironment, ObjectEnvironment};
use crate::runtime::property_descriptor::PropertyDescriptor;
use crate::runtime::property_key::PropertyKey;

define_cell!(GlobalEnvironment, Other, extends: [Environment]);

// SAFETY: Visits the outer environment and the three records a global environment holds.
unsafe impl Trace for GlobalEnvironment {
    fn trace(&self, visitor: &mut Visitor) {
        self.base.trace(visitor);
        self.object_record.trace(visitor);
        self.global_this_value.trace(visitor);
        self.declarative_record.trace(visitor);
    }
}

impl Deref for GlobalEnvironment {
    type Target = Environment;

    fn deref(&self) -> &Environment {
        &self.base
    }
}

fn global_environment(environment: &Environment) -> &GlobalEnvironment {
    environment
        .downcast_ref::<GlobalEnvironment>()
        .expect("only global environments have the global environment methods")
}

/// The methods JS::GlobalEnvironment overrides.
pub const GLOBAL_ENVIRONMENT_METHODS: EnvironmentMethods = EnvironmentMethods {
    has_this_binding: |_| true,
    get_this_binding: |environment, vm| global_environment(environment).get_this_binding(vm),
    has_binding: |environment, vm, name, _| global_environment(environment).has_binding(vm, name),
    create_mutable_binding: |environment, vm, name, can_be_deleted| {
        global_environment(environment).create_mutable_binding(vm, name, can_be_deleted)
    },
    create_immutable_binding: |environment, vm, name, strict| {
        global_environment(environment).create_immutable_binding(vm, name, strict)
    },
    initialize_binding: |environment, vm, name, value, hint| {
        global_environment(environment).initialize_binding(vm, name, value, hint)
    },
    set_mutable_binding: |environment, vm, name, value, strict| {
        global_environment(environment).set_mutable_binding(vm, name, value, strict)
    },
    get_binding_value: |environment, vm, name, strict| {
        global_environment(environment).get_binding_value(vm, name, strict)
    },
    delete_binding: |environment, vm, name| global_environment(environment).delete_binding(vm, name),
    is_global_environment: true,
    ..ENVIRONMENT_METHODS
};

impl GlobalEnvironment {
    // 9.1.2.5 NewGlobalEnvironment ( G, thisValue ), https://tc39.es/ecma262/#sec-newglobalenvironment
    pub fn create(vm: &Vm, global_object: Gc<Object>, this_value: Gc<Object>) -> Gc<GlobalEnvironment> {
        let object_record = ObjectEnvironment::create(vm, global_object, IsWithEnvironment::No, None);
        let declarative_record = DeclarativeEnvironment::create(vm, None);
        vm.heap().allocate(GlobalEnvironment {
            base: Environment::new(Self::CLASS, None, false),
            object_record: Cell::new(Some(object_record)),
            global_this_value: Cell::new(Some(this_value)),
            declarative_record: Cell::new(Some(declarative_record)),
        })
    }

    pub fn object_record(&self) -> Gc<ObjectEnvironment> {
        self.object_record
            .get()
            .expect("a global environment has an object record")
    }

    pub fn global_this_value(&self) -> Gc<Object> {
        self.global_this_value
            .get()
            .expect("a global environment has a this value")
    }

    pub fn declarative_record(&self) -> Gc<DeclarativeEnvironment> {
        self.declarative_record
            .get()
            .expect("a global environment has a declarative record")
    }

    // 9.1.1.4.11 GetThisBinding ( ), https://tc39.es/ecma262/#sec-global-environment-records-getthisbinding
    pub fn get_this_binding(&self, _vm: &Vm) -> ThrowCompletionOr<Value> {
        // 1. Return envRec.[[GlobalThisValue]].
        Ok(Value::from_object(self.global_this_value()))
    }

    // 9.1.1.4.1 HasBinding ( N ), https://tc39.es/ecma262/#sec-global-environment-records-hasbinding-n
    pub fn has_binding(&self, vm: &Vm, name: &Utf16FlyString) -> ThrowCompletionOr<bool> {
        // 1. Let DclRec be envRec.[[DeclarativeRecord]].
        // 2. If ! DclRec.HasBinding(N) is true, return true.
        if self.declarative_record().has_binding(name, None).must() {
            return Ok(true);
        }

        // 3. Let ObjRec be envRec.[[ObjectRecord]].
        // 4. Return ? ObjRec.HasBinding(N).
        self.object_record().has_binding(vm, name)
    }

    // 9.1.1.4.2 CreateMutableBinding ( N, D ), https://tc39.es/ecma262/#sec-global-environment-records-createmutablebinding-n-d
    pub fn create_mutable_binding(
        &self,
        vm: &Vm,
        name: &Utf16FlyString,
        can_be_deleted: bool,
    ) -> ThrowCompletionOr<()> {
        // 1. Let DclRec be envRec.[[DeclarativeRecord]].
        // 2. If ! DclRec.HasBinding(N) is true, throw a TypeError exception.
        if self.declarative_record().has_binding(name, None).must() {
            return vm.throw_completion(
                ErrorKind::TypeError,
                ErrorType::GlobalEnvironmentAlreadyHasBinding,
                &[name],
            );
        }

        // 3. Return ! DclRec.CreateMutableBinding(N, D).
        self.declarative_record()
            .create_mutable_binding(vm, name, can_be_deleted)
            .must();
        Ok(())
    }

    // 9.1.1.4.3 CreateImmutableBinding ( N, S ), https://tc39.es/ecma262/#sec-global-environment-records-createimmutablebinding-n-s
    pub fn create_immutable_binding(&self, vm: &Vm, name: &Utf16FlyString, strict: bool) -> ThrowCompletionOr<()> {
        // 1. Let DclRec be envRec.[[DeclarativeRecord]].
        // 2. If ! DclRec.HasBinding(N) is true, throw a TypeError exception.
        if self.declarative_record().has_binding(name, None).must() {
            return vm.throw_completion(
                ErrorKind::TypeError,
                ErrorType::GlobalEnvironmentAlreadyHasBinding,
                &[name],
            );
        }

        // 3. Return ! DclRec.CreateImmutableBinding(N, S).
        self.declarative_record()
            .create_immutable_binding(vm, name, strict)
            .must();
        Ok(())
    }

    // 9.1.1.4.4 InitializeBinding ( N, V, hint ), https://tc39.es/ecma262/#sec-global-environment-records-initializebinding-n-v
    pub fn initialize_binding(
        &self,
        vm: &Vm,
        name: &Utf16FlyString,
        value: Value,
        hint: InitializeBindingHint,
    ) -> ThrowCompletionOr<()> {
        // 1. Let DclRec be envRec.[[DeclarativeRecord]].
        // 2. If ! DclRec.HasBinding(N) is true, then
        if self.declarative_record().has_binding(name, None).must() {
            // a. Return ! DclRec.InitializeBinding(N, V, hint).
            self.declarative_record()
                .initialize_binding(vm, name, value, hint)
                .must();
            return Ok(());
        }

        // 3. Assert: If the binding exists, it must be in the object Environment Record.
        // 4. Assert: hint is normal.
        assert!(hint == InitializeBindingHint::Normal);
        // 5. Let ObjRec be envRec.[[ObjectRecord]].
        // 6. Return ? ObjRec.InitializeBinding(N, V, normal).
        self.object_record()
            .initialize_binding(vm, name, value, InitializeBindingHint::Normal)
    }

    // 9.1.1.4.5 SetMutableBinding ( N, V, S ), https://tc39.es/ecma262/#sec-global-environment-records-setmutablebinding-n-v-s
    pub fn set_mutable_binding(
        &self,
        vm: &Vm,
        name: &Utf16FlyString,
        value: Value,
        strict: bool,
    ) -> ThrowCompletionOr<()> {
        // 1. Let DclRec be envRec.[[DeclarativeRecord]].
        // 2. If ! DclRec.HasBinding(N) is true, then
        if self.declarative_record().has_binding(name, None).must() {
            // a. Return ? DclRec.SetMutableBinding(N, V, S).
            return self.declarative_record().set_mutable_binding(vm, name, value, strict);
        }

        // 3. Let ObjRec be envRec.[[ObjectRecord]].
        // 4. Return ? ObjRec.SetMutableBinding(N, V, S).
        self.object_record().set_mutable_binding(vm, name, value, strict)
    }

    // 9.1.1.4.6 GetBindingValue ( N, S ), https://tc39.es/ecma262/#sec-global-environment-records-getbindingvalue-n-s
    pub fn get_binding_value(&self, vm: &Vm, name: &Utf16FlyString, strict: bool) -> ThrowCompletionOr<Value> {
        // 1. Let DclRec be envRec.[[DeclarativeRecord]].
        // 2. If ! DclRec.HasBinding(N) is true, then
        let declarative_record = self.declarative_record();
        let mut index = None;
        if declarative_record.has_binding(name, Some(&mut index)).must() {
            // a. Return ? DclRec.GetBindingValue(N, S).
            if let Some(index) = index {
                return declarative_record.get_binding_value_direct(vm, index);
            }
            return declarative_record.get_binding_value(vm, name, strict);
        }

        // 3. Let ObjRec be envRec.[[ObjectRecord]].
        // 4. Return ? ObjRec.GetBindingValue(N, S).
        self.object_record().get_binding_value(vm, name, strict)
    }

    // 9.1.1.4.7 DeleteBinding ( N ), https://tc39.es/ecma262/#sec-global-environment-records-deletebinding-n
    pub fn delete_binding(&self, vm: &Vm, name: &Utf16FlyString) -> ThrowCompletionOr<bool> {
        // 1. Let DclRec be envRec.[[DeclarativeRecord]].
        // 2. If ! DclRec.HasBinding(N) is true, then
        if self.declarative_record().has_binding(name, None).must() {
            // a. Return ! DclRec.DeleteBinding(N).
            return Ok(self.declarative_record().delete_binding(vm, name).must());
        }

        // 3. Let ObjRec be envRec.[[ObjectRecord]].
        // 4. Let globalObject be ObjRec.[[BindingObject]].

        // 5. Let existingProp be ? HasOwnProperty(globalObject, N).
        let existing_prop = self
            .object_record()
            .binding_object()
            .has_own_property(vm, &PropertyKey::from(name.clone()))?;

        // 6. If existingProp is true, then
        if existing_prop {
            // a. Return ? ObjRec.DeleteBinding(N).
            return self.object_record().delete_binding(vm, name);
        }

        // 7. Return true.
        Ok(true)
    }

    // 9.1.1.4.12 HasLexicalDeclaration ( envRec, N ), https://tc39.es/ecma262/#sec-haslexicaldeclaration
    pub fn has_lexical_declaration(&self, name: &Utf16FlyString) -> bool {
        // 1. Let DclRec be envRec.[[DeclarativeRecord]].
        // 2. Return ! DclRec.HasBinding(N).
        self.declarative_record().has_binding(name, None).must()
    }

    // 9.1.1.4.13 HasRestrictedGlobalProperty ( envRec, N ), https://tc39.es/ecma262/#sec-hasrestrictedglobalproperty
    pub fn has_restricted_global_property(&self, vm: &Vm, name: &Utf16FlyString) -> ThrowCompletionOr<bool> {
        // 1. Let ObjRec be envRec.[[ObjectRecord]].
        // 2. Let globalObject be ObjRec.[[BindingObject]].
        let global_object = self.object_record().binding_object();

        // 3. Let existingProp be ? globalObject.[[GetOwnProperty]](N).
        let existing_prop = global_object.internal_get_own_property(vm, &PropertyKey::from(name.clone()))?;

        // 4. If existingProp is undefined, return false.
        let Some(existing_prop) = existing_prop else {
            return Ok(false);
        };

        // 5. If existingProp.[[Configurable]] is true, return false.
        if is_configurable(&existing_prop) {
            return Ok(false);
        }

        // 6. Return true.
        Ok(true)
    }

    // 9.1.1.4.14 CanDeclareGlobalVar ( envRec, N ), https://tc39.es/ecma262/#sec-candeclareglobalvar
    pub fn can_declare_global_var(&self, vm: &Vm, name: &Utf16FlyString) -> ThrowCompletionOr<bool> {
        // 1. Let ObjRec be envRec.[[ObjectRecord]].
        // 2. Let globalObject be ObjRec.[[BindingObject]].
        let global_object = self.object_record().binding_object();

        // 3. Let hasProperty be ? HasOwnProperty(globalObject, N).
        let has_property = global_object.has_own_property(vm, &PropertyKey::from(name.clone()))?;

        // 4. If hasProperty is true, return true.
        if has_property {
            return Ok(true);
        }

        // 5. Return ? IsExtensible(globalObject).
        global_object.is_extensible(vm)
    }

    // 9.1.1.4.15 CanDeclareGlobalFunction ( envRec, N ), https://tc39.es/ecma262/#sec-candeclareglobalfunction
    pub fn can_declare_global_function(&self, vm: &Vm, name: &Utf16FlyString) -> ThrowCompletionOr<bool> {
        // 1. Let ObjRec be envRec.[[ObjectRecord]].
        // 2. Let globalObject be ObjRec.[[BindingObject]].
        let global_object = self.object_record().binding_object();

        // 3. Let existingProp be ? globalObject.[[GetOwnProperty]](N).
        let existing_prop = global_object.internal_get_own_property(vm, &PropertyKey::from(name.clone()))?;

        // 4. If existingProp is undefined, return ? IsExtensible(globalObject).
        let Some(existing_prop) = existing_prop else {
            return global_object.is_extensible(vm);
        };

        // 5. If existingProp.[[Configurable]] is true, return true.
        if is_configurable(&existing_prop) {
            return Ok(true);
        }

        // 6. If IsDataDescriptor(existingProp) is true and existingProp has attribute values { [[Writable]]: true, [[Enumerable]]: true }, return true.
        if existing_prop.is_data_descriptor()
            && existing_prop.writable.expect("a data property has [[Writable]]")
            && existing_prop.enumerable.expect("a property has [[Enumerable]]")
        {
            return Ok(true);
        }

        // 7. Return false.
        Ok(false)
    }

    // 9.1.1.4.16 CreateGlobalVarBinding ( envRec, N, D ), https://tc39.es/ecma262/#sec-createglobalvarbinding
    pub fn create_global_var_binding(
        &self,
        vm: &Vm,
        name: &Utf16FlyString,
        can_be_deleted: bool,
    ) -> ThrowCompletionOr<()> {
        // 1. Let ObjRec be envRec.[[ObjectRecord]].
        // 2. Let globalObject be ObjRec.[[BindingObject]].
        let global_object = self.object_record().binding_object();

        // 3. Let hasProperty be ? HasOwnProperty(globalObject, N).
        let has_property = global_object.has_own_property(vm, &PropertyKey::from(name.clone()))?;

        // 4. Let extensible be ? IsExtensible(globalObject).
        let extensible = global_object.is_extensible(vm)?;

        // 5. If hasProperty is false and extensible is true, then
        if !has_property && extensible {
            // a. Perform ? ObjRec.CreateMutableBinding(N, D).
            self.object_record().create_mutable_binding(vm, name, can_be_deleted)?;

            // b. Perform ? ObjRec.InitializeBinding(N, undefined, normal).
            self.object_record()
                .initialize_binding(vm, name, Value::UNDEFINED, InitializeBindingHint::Normal)?;
        }

        // 6. Return UNUSED.
        Ok(())
    }

    // 9.1.1.4.17 CreateGlobalFunctionBinding ( envRec, N, V, D ), https://tc39.es/ecma262/#sec-createglobalfunctionbinding
    pub fn create_global_function_binding(
        &self,
        vm: &Vm,
        name: &Utf16FlyString,
        value: Value,
        can_be_deleted: bool,
    ) -> ThrowCompletionOr<()> {
        let name_key = PropertyKey::from(name.clone());

        // 1. Let ObjRec be envRec.[[ObjectRecord]].
        // 2. Let globalObject be ObjRec.[[BindingObject]].
        let global_object = self.object_record().binding_object();

        // 3. Let existingProp be ? globalObject.[[GetOwnProperty]](N).
        let existing_prop = global_object.internal_get_own_property(vm, &name_key)?;

        // 4. If existingProp is undefined or existingProp.[[Configurable]] is true, then
        let mut desc = if existing_prop.as_ref().is_none_or(is_configurable) {
            //     a. Let desc be the PropertyDescriptor { [[Value]]: V, [[Writable]]: true, [[Enumerable]]: true, [[Configurable]]: D }.
            PropertyDescriptor {
                value: Some(value),
                writable: Some(true),
                enumerable: Some(true),
                configurable: Some(can_be_deleted),
                ..Default::default()
            }
        }
        // 5. Else,
        else {
            // a. Let desc be the PropertyDescriptor { [[Value]]: V }.
            PropertyDescriptor {
                value: Some(value),
                ..Default::default()
            }
        };

        // 6. Perform ? DefinePropertyOrThrow(globalObject, N, desc).
        global_object.define_property_or_throw(vm, &name_key, &mut desc)?;

        // 7. Perform ? Set(globalObject, N, V, false).
        global_object.set(vm, &name_key, value, ShouldThrowExceptions::Yes)?;

        // 8. Return UNUSED.
        Ok(())
    }
}

/// `*descriptor.configurable` of a property descriptor that [[GetOwnProperty]] returned, which is complete.
fn is_configurable(descriptor: &PropertyDescriptor) -> bool {
    descriptor.configurable.expect("a property has [[Configurable]]")
}

/// A global object for a test realm, the way InitializeHostDefinedRealm gives a realm one: an ordinary object whose
/// prototype is %Object.prototype%, with the global environment for it.
#[cfg(all(test, libjs_runtime_tests_with_libgc))]
pub mod test_global_object {
    use super::*;
    use crate::runtime::object::{MayInterfereWithIndexedPropertyAccess, allocate_object};
    use crate::runtime::realm::Realm;

    pub fn set_up_global_object(vm: &Vm, realm: Gc<Realm>) -> Gc<Object> {
        let global = allocate_object(
            vm,
            Object::new_global_object(vm, Object::CLASS, realm, MayInterfereWithIndexedPropertyAccess::No),
        );
        global.set_prototype(vm, Some(realm.object_prototype()));
        realm.set_global_object(global);
        realm.set_global_environment(GlobalEnvironment::create(vm, global, global));
        global
    }
}

#[cfg(all(test, libjs_runtime_tests_with_libgc))]
mod tests {
    use super::test_global_object::set_up_global_object;
    use super::*;
    use crate::runtime::property_attributes::{Attribute, DEFAULT_ATTRIBUTES, PropertyAttributes};
    use crate::runtime::realm::test_realm::{TestRealm, key, own_keys, thrown_message};

    fn name(name: &str) -> Utf16FlyString {
        Utf16FlyString::from_utf8(name)
    }

    fn attributes_of(vm: &Vm, object: &Object, name: &str) -> (bool, bool, bool) {
        let descriptor = object
            .internal_get_own_property(vm, &key(name))
            .must()
            .expect("the property exists");
        (
            descriptor.writable.unwrap_or(false),
            descriptor.enumerable.unwrap(),
            descriptor.configurable.unwrap(),
        )
    }

    #[test]
    fn a_realm_caches_the_declarative_record_of_its_global_environment() {
        let vm = Vm::create();
        let test_realm = TestRealm::new(&vm);
        let global = set_up_global_object(&vm, test_realm.realm);
        let environment = test_realm.realm.global_environment();
        assert!(test_realm.realm.global_declarative_environment() == environment.declarative_record());
        assert!(test_realm.realm.global_object() == global);
        assert!(environment.object_record().binding_object() == global);
        assert!(!environment.object_record().is_with_environment());
        assert!(environment.global_this_value() == global);
        assert!(environment.outer_environment().is_none());

        let as_environment: Gc<Environment> = environment.upcast();
        assert!(as_environment.is_global_environment());
        assert!(!as_environment.is_declarative_environment());
        assert!(as_environment.has_this_binding());
        assert_eq!(as_environment.get_this_binding(&vm).must(), Value::from_object(global));
        assert!(as_environment.with_base_object().is_none());
    }

    #[test]
    fn lexical_bindings_live_in_the_declarative_record_and_vars_on_the_global_object() {
        let vm = Vm::create();
        vm.heap().set_should_collect_on_every_allocation(true);
        let test_realm = TestRealm::new(&vm);
        let global = set_up_global_object(&vm, test_realm.realm);
        let environment: Gc<Environment> = test_realm.realm.global_environment().upcast();
        let global_environment = test_realm.realm.global_environment();
        let (lexical, constant, var) = (name("lexical"), name("constant"), name("var"));

        environment.create_mutable_binding(&vm, &lexical, false).must();
        environment.create_immutable_binding(&vm, &constant, true).must();
        global_environment.create_global_var_binding(&vm, &var, true).must();
        assert!(global_environment.has_lexical_declaration(&lexical));
        assert!(!global_environment.has_lexical_declaration(&var));
        assert!(environment.has_binding(&vm, &var, None).must());
        assert_eq!(own_keys(&vm, &global), "var");
        assert_eq!(attributes_of(&vm, &global, "var"), (true, true, true));

        environment
            .initialize_binding(&vm, &lexical, Value::from_i32(1), InitializeBindingHint::Normal)
            .must();
        environment
            .initialize_binding(&vm, &constant, Value::from_i32(2), InitializeBindingHint::Normal)
            .must();
        environment
            .set_mutable_binding(&vm, &var, Value::from_i32(3), true)
            .must();
        assert_eq!(
            environment.get_binding_value(&vm, &lexical, true).must(),
            Value::from_i32(1)
        );
        assert_eq!(
            environment.get_binding_value(&vm, &constant, true).must(),
            Value::from_i32(2)
        );
        assert_eq!(global.get(&vm, &key("var")).must(), Value::from_i32(3));
        assert!(
            thrown_message(|| environment.set_mutable_binding(&vm, &constant, Value::NULL, false))
                .contains("creating a TypeError with the message \"Invalid assignment to const variable\"")
        );
        assert!(
            thrown_message(|| environment.create_mutable_binding(&vm, &lexical, false))
                .contains("creating a TypeError with the message \"Global environment already has binding 'lexical'\"")
        );

        // Lexical bindings cannot be deleted; deletable vars and missing names can.
        assert!(!environment.delete_binding(&vm, &lexical).must());
        assert!(environment.delete_binding(&vm, &name("missing")).must());
        assert!(environment.delete_binding(&vm, &var).must());
        assert_eq!(own_keys(&vm, &global), "");
    }

    #[test]
    fn global_var_bindings_keep_existing_properties() {
        let vm = Vm::create();
        let test_realm = TestRealm::new(&vm);
        let global = set_up_global_object(&vm, test_realm.realm);
        let environment = test_realm.realm.global_environment();
        global.define_direct_property(&vm, &key("existing"), Value::from_i32(7), DEFAULT_ATTRIBUTES);
        environment
            .create_global_var_binding(&vm, &name("existing"), false)
            .must();
        assert_eq!(global.get(&vm, &key("existing")).must(), Value::from_i32(7));
        assert_eq!(attributes_of(&vm, &global, "existing"), (true, true, true));

        assert!(global.internal_prevent_extensions(&vm).must());
        environment
            .create_global_var_binding(&vm, &name("refused"), false)
            .must();
        assert!(!global.has_own_property(&vm, &key("refused")).must());
    }

    #[test]
    fn global_function_bindings_redefine_or_only_assign() {
        let vm = Vm::create();
        let test_realm = TestRealm::new(&vm);
        let global = set_up_global_object(&vm, test_realm.realm);
        let environment = test_realm.realm.global_environment();

        environment
            .create_global_function_binding(&vm, &name("fresh"), Value::from_i32(1), false)
            .must();
        assert_eq!(attributes_of(&vm, &global, "fresh"), (true, true, false));

        // A non-configurable property keeps its attributes and only takes the new value.
        global.define_direct_property(
            &vm,
            &key("pinned"),
            Value::from_i32(0),
            PropertyAttributes::new(Attribute::WRITABLE),
        );
        environment
            .create_global_function_binding(&vm, &name("pinned"), Value::from_i32(2), true)
            .must();
        assert_eq!(global.get(&vm, &key("pinned")).must(), Value::from_i32(2));
        assert_eq!(attributes_of(&vm, &global, "pinned"), (true, false, false));

        // A configurable one is redefined with the attributes of a function declaration.
        global.define_direct_property(
            &vm,
            &key("loose"),
            Value::from_i32(0),
            PropertyAttributes::new(Attribute::CONFIGURABLE),
        );
        environment
            .create_global_function_binding(&vm, &name("loose"), Value::from_i32(3), false)
            .must();
        assert_eq!(attributes_of(&vm, &global, "loose"), (true, true, false));
        assert_eq!(global.get(&vm, &key("loose")).must(), Value::from_i32(3));
    }

    #[test]
    fn declarability_follows_the_global_objects_properties() {
        let vm = Vm::create();
        let test_realm = TestRealm::new(&vm);
        let global = set_up_global_object(&vm, test_realm.realm);
        let environment = test_realm.realm.global_environment();
        let define = |name: &str, attributes: u8| {
            global.define_direct_property(&vm, &key(name), Value::from_i32(0), PropertyAttributes::new(attributes));
        };
        define("configurable", Attribute::CONFIGURABLE);
        define("writable_enumerable", Attribute::WRITABLE | Attribute::ENUMERABLE);
        define("read_only", Attribute::ENUMERABLE);
        global.define_direct_accessor(
            &vm,
            &key("accessor"),
            None,
            None,
            PropertyAttributes::new(Attribute::ENUMERABLE),
        );

        let restricted = |name: &str| {
            environment
                .has_restricted_global_property(&vm, &self::name(name))
                .must()
        };
        let can_declare_var = |name: &str| environment.can_declare_global_var(&vm, &self::name(name)).must();
        let can_declare_function = |name: &str| environment.can_declare_global_function(&vm, &self::name(name)).must();

        assert!(!restricted("missing"));
        assert!(!restricted("configurable"));
        assert!(restricted("writable_enumerable"));
        assert!(restricted("read_only"));
        assert!(restricted("accessor"));

        assert!(can_declare_var("missing"));
        assert!(can_declare_var("read_only"));
        assert!(can_declare_function("missing"));
        assert!(can_declare_function("configurable"));
        assert!(can_declare_function("writable_enumerable"));
        assert!(!can_declare_function("read_only"));
        assert!(!can_declare_function("accessor"));

        assert!(global.internal_prevent_extensions(&vm).must());
        assert!(!can_declare_var("missing"));
        assert!(can_declare_var("read_only"));
        assert!(!can_declare_function("missing"));
        assert!(can_declare_function("configurable"));
    }

    #[test]
    fn global_this_can_differ_from_the_global_object() {
        let vm = Vm::create();
        let test_realm = TestRealm::new(&vm);
        let global = test_realm.object();
        let this_value = test_realm.object();
        let environment = GlobalEnvironment::create(&vm, global, this_value);
        assert_eq!(
            environment.upcast::<Environment>().get_this_binding(&vm).must(),
            Value::from_object(this_value)
        );
        assert!(environment.object_record().binding_object() == global);
    }
}

/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! The slow paths that create, look up and change bindings, and their helpers.

use core::cell::Cell;

use ak::Utf16FlyString;
use libjs_abi::EnvironmentMode;

use crate::interpreter::runtime_functions::unimplemented_runtime_function;
use crate::interpreter::vm::Vm;
use crate::layout::cell::Gc;
use crate::runtime::abstract_operations::new_declarative_environment;
use crate::runtime::completion::ThrowCompletionOr;
use crate::runtime::declarative_environment::DeclarativeEnvironment;
use crate::runtime::environment::Environment;
use crate::runtime::environment_coordinate::EnvironmentCoordinate;
use crate::runtime::environment_shape::EnvironmentShapeCache;
use crate::runtime::error::ErrorKind;
use crate::utf16::Utf16View;

/// The environment `coordinate` refers to from `environment`, if the interpreter may keep finding the binding there:
/// every environment on the way must be declarative and not open to eval adding bindings.
pub fn get_cacheable_environment(
    environment: Gc<Environment>,
    coordinate: EnvironmentCoordinate,
) -> Option<Gc<DeclarativeEnvironment>> {
    assert!(coordinate.is_valid());

    let mut environment = environment;
    for _ in 0..coordinate.hops {
        if !environment.is_declarative_environment() || environment.is_permanently_screwed_by_eval() {
            return None;
        }
        environment = environment.outer_environment()?;
    }
    if environment.is_declarative_environment() && !environment.is_permanently_screwed_by_eval() {
        return environment.downcast::<DeclarativeEnvironment>();
    }
    None
}

/// The environment a dynamic binding access found its binding in last time, if it still applies. A cache that no
/// longer applies is cleared, so the access resolves the binding again.
pub fn get_cached_environment(
    environment: Gc<Environment>,
    cache: &Cell<EnvironmentCoordinate>,
) -> Option<Gc<DeclarativeEnvironment>> {
    if !cache.get().is_valid() {
        return None;
    }

    if let Some(cached_environment) = get_cacheable_environment(environment, cache.get()) {
        return Some(cached_environment);
    }

    cache.set(EnvironmentCoordinate::invalid());
    None
}

/// Remembers where a binding was resolved from `environment`, given the coordinate of the Reference that
/// ResolveBinding produced, if any.
pub fn update_environment_coordinate_cache(
    environment: Gc<Environment>,
    reference_environment_coordinate: Option<EnvironmentCoordinate>,
    cache: &Cell<EnvironmentCoordinate>,
) {
    let Some(candidate) = reference_environment_coordinate else {
        return;
    };
    if get_cacheable_environment(environment, candidate).is_some() {
        cache.set(candidate);
    }
}

/// The environment a static coordinate refers to. The bytecode only carries static coordinates of declarative
/// environments.
pub fn environment_at_coordinate(
    environment: Gc<Environment>,
    coordinate: EnvironmentCoordinate,
) -> Gc<DeclarativeEnvironment> {
    assert!(coordinate.is_valid());

    let mut environment = environment;
    for _ in 0..coordinate.hops {
        environment = environment
            .outer_environment()
            .expect("a static coordinate stays within the environment chain");
    }
    environment
        .downcast::<DeclarativeEnvironment>()
        .expect("static coordinates refer to declarative environments")
}

/// What CreateLexicalEnvironment does besides storing the new environment as the running context's lexical
/// environment and in its destination.
pub fn create_lexical_environment(
    vm: &Vm,
    parent: Gc<Environment>,
    shape_cache: EnvironmentShapeCache,
    capacity: u32,
    is_catch_environment: bool,
) -> Gc<DeclarativeEnvironment> {
    let environment = new_declarative_environment(vm, parent);
    environment.set_environment_shape_cache(shape_cache, capacity as usize);
    environment.ensure_capacity(capacity as usize);
    environment.set_is_catch_environment(is_catch_environment);
    environment
}

/// What CreateVariableEnvironment does besides storing the new environment as the running context's variable and
/// lexical environment. The shape cache is the active function's var environment shape, when `capacity` is the
/// number of var bindings that function declares.
pub fn create_variable_environment(
    vm: &Vm,
    lexical_environment: Gc<Environment>,
    shape_cache: Option<EnvironmentShapeCache>,
    capacity: u32,
) -> Gc<DeclarativeEnvironment> {
    let var_environment = new_declarative_environment(vm, lexical_environment);
    if let Some(shape_cache) = shape_cache {
        var_environment.set_environment_shape_cache(shape_cache, capacity as usize);
    }
    var_environment.ensure_capacity(capacity as usize);
    var_environment
}

fn running_execution_context_environment(vm: &Vm, mode: EnvironmentMode) -> Gc<Environment> {
    let context = vm
        .running_execution_context()
        .expect("bindings are created in an execution context");
    // SAFETY: The running execution context is live.
    let context = unsafe { context.as_ref() };
    match mode {
        EnvironmentMode::Lexical => context.lexical_environment.get(),
        EnvironmentMode::Var => context.variable_environment.get(),
    }
    .expect("the running execution context has its environments")
}

/// GlobalEnvironment::create_global_var_binding, which the global environment unit provides.
fn create_global_var_binding(_vm: &Vm, _name: &Utf16FlyString, _can_be_deleted: bool) -> ThrowCompletionOr<()> {
    unimplemented_runtime_function("GlobalEnvironment::create_global_var_binding, for CreateVariable", 0)
}

/// What CreateVariable does.
pub fn create_variable(
    vm: &Vm,
    name: &Utf16FlyString,
    mode: EnvironmentMode,
    is_global: bool,
    is_immutable: bool,
    is_strict: bool,
) -> ThrowCompletionOr<()> {
    if mode == EnvironmentMode::Lexical {
        assert!(!is_global);

        let lexical_environment = running_execution_context_environment(vm, EnvironmentMode::Lexical);

        // Note: This is papering over an issue where "FunctionDeclarationInstantiation" creates these bindings for us.
        //       Instead of crashing in there, we'll just raise an exception here.
        if lexical_environment.has_binding(vm, name, None)? {
            return vm.throw_completion_with_message(
                ErrorKind::InternalError,
                format!(
                    "Lexical environment already has binding '{}'",
                    Utf16View::of_fly_string(name).to_utf8()
                ),
            );
        }

        if is_immutable {
            return lexical_environment.create_immutable_binding(vm, name, is_strict);
        }
        return lexical_environment.create_mutable_binding(vm, name, is_strict);
    }

    if !is_global {
        let variable_environment = running_execution_context_environment(vm, EnvironmentMode::Var);
        if is_immutable {
            return variable_environment.create_immutable_binding(vm, name, is_strict);
        }
        return variable_environment.create_mutable_binding(vm, name, is_strict);
    }

    // NOTE: CreateVariable with m_is_global set to true is expected to only be used in GlobalDeclarationInstantiation currently, which only uses "false" for "can_be_deleted".
    //       The only area that sets "can_be_deleted" to true is EvalDeclarationInstantiation, which is currently fully implemented in C++ and not in Bytecode.
    create_global_var_binding(vm, name, false)
}

#[cfg(all(test, libjs_runtime_tests_with_libgc))]
mod tests {
    use super::*;
    use crate::bytecode::executable::{Executable, ExecutableCacheCounts};
    use crate::layout::value::Value;
    use crate::runtime::environment::InitializeBindingHint;
    use crate::runtime::function_environment::FunctionEnvironment;

    fn coordinate(hops: u32, index: u32) -> EnvironmentCoordinate {
        EnvironmentCoordinate { hops, index }
    }

    #[test]
    fn cached_coordinates_hold_while_the_chain_stays_declarative_and_free_of_eval() {
        let vm = Vm::create();
        let function = FunctionEnvironment::create(&vm, None);
        let block = DeclarativeEnvironment::create(&vm, Some(function.upcast()));
        let inner = DeclarativeEnvironment::create(&vm, Some(block.upcast()));
        let inner_environment: Gc<Environment> = inner.upcast();

        assert!(get_cacheable_environment(inner_environment, coordinate(0, 0)) == Some(inner));
        assert!(get_cacheable_environment(inner_environment, coordinate(2, 0)) == Some(function.upcast()));
        assert!(get_cacheable_environment(inner_environment, coordinate(3, 0)).is_none());
        assert!(environment_at_coordinate(inner_environment, coordinate(1, 5)) == block);

        let cache = Cell::new(EnvironmentCoordinate::invalid());
        assert!(get_cached_environment(inner_environment, &cache).is_none());
        update_environment_coordinate_cache(inner_environment, None, &cache);
        assert!(!cache.get().is_valid());
        update_environment_coordinate_cache(inner_environment, Some(coordinate(4, 0)), &cache);
        assert!(!cache.get().is_valid());
        update_environment_coordinate_cache(inner_environment, Some(coordinate(1, 3)), &cache);
        assert_eq!(cache.get(), coordinate(1, 3));
        assert!(get_cached_environment(inner_environment, &cache) == Some(block));

        // Once eval may add bindings along the way, the cache is dropped and the binding resolved again.
        inner.set_permanently_screwed_by_eval();
        assert!(get_cached_environment(inner_environment, &cache).is_none());
        assert!(!cache.get().is_valid());
    }

    #[test]
    fn variables_are_created_in_the_running_contexts_environments() {
        let vm = Vm::create();
        vm.heap().set_should_collect_on_every_allocation(true);
        let counts = ExecutableCacheCounts {
            property_lookup_caches: 0,
            global_variable_caches: 0,
            environment_coordinate_caches: 1,
            environment_shape_caches: 1,
        };
        let executable = vm.heap().allocate(Executable::new(
            vec![0u8; 8].into_boxed_slice(),
            5,
            0,
            0,
            Box::new([]),
            &counts,
            true,
        ));
        let stack = vm.interpreter_stack();
        let mark = stack.top.get();
        let context = stack.allocate(0, 0, 0).expect("the stack has room");
        vm.push_execution_context(context);
        // SAFETY: The context was just pushed.
        let context_ref = unsafe { context.as_ref() };

        let function = FunctionEnvironment::create(&vm, None);
        let var_environment = create_variable_environment(&vm, function.upcast(), None, 1);
        context_ref.variable_environment.set(Some(var_environment.upcast()));
        let lexical_environment = create_lexical_environment(
            &vm,
            var_environment.upcast(),
            executable.environment_shape_cache(0),
            1,
            true,
        );
        context_ref.lexical_environment.set(Some(lexical_environment.upcast()));
        assert!(lexical_environment.is_catch_environment());

        let (x, y) = (Utf16FlyString::from_utf8("x"), Utf16FlyString::from_utf8("y"));
        create_variable(&vm, &x, EnvironmentMode::Lexical, false, true, true).unwrap();
        create_variable(&vm, &y, EnvironmentMode::Var, false, false, false).unwrap();
        assert!(lexical_environment.shape.get().is_some());
        assert!(executable.environment_shape_cache(0).shape() == lexical_environment.shape.get());
        assert_eq!(lexical_environment.binding_index(&x), Some(0));
        assert_eq!(var_environment.binding_index(&y), Some(0));
        assert!(!lexical_environment.binding_is_mutable_by_name(&x));

        executable.environment_coordinate_cache(0).set(coordinate(1, 0));
        let cached = get_cached_environment(lexical_environment.upcast(), executable.environment_coordinate_cache(0))
            .expect("the var environment is one hop out");
        cached
            .initialize_binding_direct(&vm, 0, Value::from_i32(9), InitializeBindingHint::Normal)
            .unwrap();
        assert_eq!(
            var_environment.get_binding_value(&vm, &y, false).unwrap(),
            Value::from_i32(9)
        );

        vm.pop_execution_context();
        stack.deallocate(mark);
    }
}

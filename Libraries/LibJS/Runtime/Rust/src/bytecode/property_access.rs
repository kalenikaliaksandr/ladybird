/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! Mirrors Libraries/LibJS/Bytecode/PropertyAccess.h: the property gets and puts that consult and fill the inline
//! caches the interpreter's fast paths read.

use core::fmt::Display;

use ak::Utf16FlyString;
use libjs_abi::PutKind;

use crate::bytecode::executable::{
    KeyedPropertyLookupCache, KeyedPropertyLookupCacheEntry, PropertyLookupCache, PropertyLookupCacheEntryType,
};
use crate::interpreter::vm::Vm;
use crate::layout::cell::Gc;
use crate::layout::value::Value;
use crate::runtime::abstract_operations::call_function_object;
use crate::runtime::class_field_definition::ClassElementName;
use crate::runtime::completion::{Must, ThrowCompletionOr};
use crate::runtime::ecmascript_function_object::as_ecmascript_function_object;
use crate::runtime::error::ErrorKind;
use crate::runtime::error_types::ErrorType;
use crate::runtime::object::{
    CacheableGetPropertyMetadata, CacheableGetPropertyMetadataType, CacheableSetPropertyMetadata,
    CacheableSetPropertyMetadataType, Object, PropertyLookupPhase,
};
use crate::runtime::property_attributes::{Attribute, PropertyAttributes};
use crate::runtime::property_key::PropertyKey;
use crate::utf16::Utf16View;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GetByIdMode {
    Normal,
    Length,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CachePropertyAbsence {
    No,
    Yes,
}

/// Whether the code doing a property access is strict mode code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Strict {
    No,
    Yes,
}

fn display_fly_string(string: &Utf16FlyString) -> String {
    Utf16View::of_fly_string(string).to_utf8()
}

pub fn get_cached_property_value(vm: &Vm, value: Value, this_value: Value) -> ThrowCompletionOr<Value> {
    if !value.is_accessor() {
        return Ok(value);
    }

    // https://tc39.es/ecma262/#sec-ordinaryget
    // If _getter_ is *undefined*, return *undefined*.
    let Some(getter) = value.as_accessor().getter() else {
        return Ok(Value::UNDEFINED);
    };
    call_function_object(vm, getter, this_value, &[])
}

pub fn object_can_cache_property_additions(object: &Object) -> bool {
    !object.may_interfere_with_indexed_property_access() && !object.requires_slow_add_own_property()
}

pub fn property_addition_is_cacheable(vm: &Vm, object: &Object, property_key: &PropertyKey) -> bool {
    if !property_key.is_string() {
        return object_can_cache_property_additions(object);
    }
    object_can_cache_property_additions(object)
        && !(object.has_magical_length_property() && property_key.as_string() == vm.names.length.as_string())
}

pub fn get_by_value_with_keyed_cache(
    vm: &Vm,
    base_object: Gc<Object>,
    this_value: Value,
    property_key: &PropertyKey,
) -> ThrowCompletionOr<Value> {
    if !property_key.is_string() {
        return base_object.internal_get(vm, property_key, this_value, None, PropertyLookupPhase::OwnProperty);
    }

    let property_name = property_key.as_string();
    let shape = base_object.shape();
    let keyed_property_lookup_cache = vm.keyed_property_lookup_cache();
    let entry_index = KeyedPropertyLookupCache::entry_index_for(shape, property_name);
    let entry = keyed_property_lookup_cache.entry(entry_index);
    if entry.shape == Some(shape)
        && entry.property_name.as_ref() == Some(property_name)
        && (!shape.is_dictionary() || shape.dictionary_generation() == entry.shape_dictionary_generation)
    {
        let prototype_chain_validity_is_valid = entry
            .prototype_chain_validity
            .is_some_and(|validity| validity.is_valid());
        match entry.entry_type {
            PropertyLookupCacheEntryType::GetOwnProperty => {
                return get_cached_property_value(vm, base_object.get_direct(entry.property_offset), this_value);
            }
            PropertyLookupCacheEntryType::GetPropertyInPrototypeChain => {
                if prototype_chain_validity_is_valid {
                    let prototype = entry.prototype.expect("an inherited property has a holder");
                    return get_cached_property_value(vm, prototype.get_direct(entry.property_offset), this_value);
                }
            }
            PropertyLookupCacheEntryType::GetMissingProperty
                if base_object.is_cacheable_for_property_absence()
                    && (shape.prototype().is_none() || prototype_chain_validity_is_valid) =>
            {
                return Ok(Value::UNDEFINED);
            }
            _ => {}
        }
    }

    let prototype_chain_validity = shape
        .prototype()
        .and_then(|prototype| prototype.shape().prototype_chain_validity());

    let dictionary_generation = shape.dictionary_generation();
    let mut cacheable_metadata = CacheableGetPropertyMetadata {
        property_absence_is_cacheable: base_object.is_cacheable_for_property_absence(),
        ..Default::default()
    };
    let value = base_object.internal_get(
        vm,
        property_key,
        this_value,
        Some(&mut cacheable_metadata),
        PropertyLookupPhase::OwnProperty,
    )?;

    // A getter may have changed the object's shape or the property storage of a dictionary shape, which
    // leaves the metadata describing a lookup that no longer applies.
    if shape != base_object.shape()
        || shape.dictionary_generation() != dictionary_generation
        || cacheable_metadata.r#type == CacheableGetPropertyMetadataType::NotCacheable
    {
        return Ok(value);
    }

    let mut entry = KeyedPropertyLookupCacheEntry {
        shape: Some(shape),
        property_name: Some(property_name.clone()),
        ..Default::default()
    };
    if shape.is_dictionary() {
        entry.shape_dictionary_generation = shape.dictionary_generation();
    }
    match cacheable_metadata.r#type {
        CacheableGetPropertyMetadataType::GetOwnProperty => {
            entry.entry_type = PropertyLookupCacheEntryType::GetOwnProperty;
            entry.property_offset = cacheable_metadata
                .property_offset
                .expect("cacheable metadata has an offset");
        }
        CacheableGetPropertyMetadataType::GetPropertyInPrototypeChain => {
            entry.entry_type = PropertyLookupCacheEntryType::GetPropertyInPrototypeChain;
            entry.property_offset = cacheable_metadata
                .property_offset
                .expect("cacheable metadata has an offset");
            entry.prototype = cacheable_metadata.prototype;
            entry.prototype_chain_validity = prototype_chain_validity;
        }
        CacheableGetPropertyMetadataType::GetMissingProperty => {
            entry.entry_type = PropertyLookupCacheEntryType::GetMissingProperty;
            entry.prototype_chain_validity = prototype_chain_validity;
        }
        CacheableGetPropertyMetadataType::NotCacheable => unreachable!("an uncacheable lookup returned above"),
    }
    keyed_property_lookup_cache.set_entry(entry_index, entry);
    Ok(value)
}

// Non-standard
pub fn get_own_property_without_side_effects(
    object: &Object,
    property_key: &PropertyKey,
    cache: &PropertyLookupCache,
) -> Value {
    let shape = object.shape();

    if let Some(cache_entry) = cache.first_entry()
        && (cache_entry.entry_type == PropertyLookupCacheEntryType::GetOwnProperty
            || cache_entry.entry_type == PropertyLookupCacheEntryType::GetMissingProperty)
        && Some(shape) == cache_entry.shape
        && (!shape.is_dictionary() || shape.dictionary_generation() == cache_entry.shape_dictionary_generation)
    {
        if cache_entry.entry_type == PropertyLookupCacheEntryType::GetMissingProperty {
            return Value::EMPTY;
        }
        return object.get_direct(cache_entry.property_offset);
    }

    let metadata = shape.lookup(property_key);
    let cache_type = if metadata.is_some() {
        PropertyLookupCacheEntryType::GetOwnProperty
    } else {
        PropertyLookupCacheEntryType::GetMissingProperty
    };
    cache.update(cache_type, |entry| {
        entry.shape = Some(shape);
        if let Some(metadata) = metadata {
            entry.property_offset = metadata.offset;
        }
        if shape.is_dictionary() {
            entry.shape_dictionary_generation = shape.dictionary_generation();
        }
    });

    let Some(metadata) = metadata else {
        return Value::EMPTY;
    };
    object.get_direct(metadata.offset)
}

pub fn base_object_for_get_impl(vm: &Vm, base_value: Value) -> Option<Gc<Object>> {
    if base_value.is_object() {
        return Some(base_value.as_object());
    }

    // OPTIMIZATION: For various primitives we can avoid actually creating a new object for them.
    if base_value.is_nullish() {
        return None;
    }
    let realm = vm
        .current_realm()
        .expect("there is a current realm to find the prototype of a primitive in");
    if base_value.is_string() {
        return Some(realm.string_prototype(vm));
    }
    if base_value.is_number() {
        return Some(realm.number_prototype(vm));
    }
    if base_value.is_boolean() {
        return Some(realm.boolean_prototype(vm));
    }
    if base_value.is_bigint() {
        return Some(realm.bigint_prototype(vm));
    }
    if base_value.is_symbol() {
        return Some(realm.symbol_prototype(vm));
    }

    None
}

#[cold]
fn throw_null_or_undefined_property_get<T>(
    vm: &Vm,
    base_value: Value,
    get_base_identifier: impl FnOnce() -> Option<Utf16FlyString>,
    property_name: &dyn Display,
) -> ThrowCompletionOr<T> {
    assert!(base_value.is_nullish());

    if let Some(base_identifier) = get_base_identifier() {
        return vm.throw_completion(
            ErrorKind::TypeError,
            ErrorType::ToObjectNullOrUndefinedWithPropertyAndName,
            &[property_name, &base_value, &display_fly_string(&base_identifier)],
        );
    }
    vm.throw_completion(
        ErrorKind::TypeError,
        ErrorType::ToObjectNullOrUndefinedWithProperty,
        &[property_name, &base_value],
    )
}

/// The object a property get on `base_value` looks the property up in. `property_name` is the property as the error
/// for a null or undefined base names it: a key, or the value a key has yet to be computed from.
pub fn base_object_for_get(
    vm: &Vm,
    base_value: Value,
    get_base_identifier: impl FnOnce() -> Option<Utf16FlyString>,
    property_name: &dyn Display,
) -> ThrowCompletionOr<Gc<Object>> {
    if let Some(base_object) = base_object_for_get_impl(vm, base_value) {
        return Ok(base_object);
    }

    // NOTE: At this point this is guaranteed to throw (null or undefined).
    throw_null_or_undefined_property_get(vm, base_value, get_base_identifier, property_name)
}

#[allow(clippy::too_many_arguments)]
pub fn get_by_id(
    vm: &Vm,
    mode: GetByIdMode,
    get_base_identifier: impl FnOnce() -> Option<Utf16FlyString>,
    property_name: &PropertyKey,
    base_value: Value,
    this_value: Value,
    cache: &PropertyLookupCache,
    cache_property_absence: CachePropertyAbsence,
) -> ThrowCompletionOr<Value> {
    if mode == GetByIdMode::Length && base_value.is_string() {
        return Ok(Value::from_f64(
            base_value.as_string().length_in_utf16_code_units() as f64
        ));
    }

    if base_value.is_string() {
        // https://tc39.es/ecma262/#sec-stringgetownproperty
        // String exotic objects expose virtual own properties for canonical string indexes.
        let string_value = base_value.as_string().get(vm, property_name)?;
        if let Some(string_value) = string_value {
            return Ok(string_value);
        }
    }

    let base_obj = base_object_for_get(vm, base_value, get_base_identifier, property_name)?;

    // OPTIMIZATION: Fast path for the magical "length" property on Array objects.
    if mode == GetByIdMode::Length && base_obj.has_magical_length_property() {
        return Ok(Value::from_f64(f64::from(base_obj.indexed_array_like_size())));
    }

    let shape = base_obj.shape();

    for cache_entry in cache.entries_for_shape(shape).as_slice() {
        if cache_entry.entry_type == PropertyLookupCacheEntryType::GetMissingProperty {
            if cache_property_absence == CachePropertyAbsence::No {
                continue;
            }
            if !base_obj.is_cacheable_for_property_absence() {
                continue;
            }
            if Some(shape) != cache_entry.shape {
                continue;
            }
            if shape.is_dictionary() && shape.dictionary_generation() != cache_entry.shape_dictionary_generation {
                continue;
            }
            if shape.prototype().is_some()
                && !cache_entry
                    .prototype_chain_validity
                    .is_some_and(|validity| validity.is_valid())
            {
                continue;
            }
            return Ok(Value::UNDEFINED);
        }

        if cache_entry.entry_type != PropertyLookupCacheEntryType::GetOwnProperty
            && cache_entry.entry_type != PropertyLookupCacheEntryType::GetPropertyInPrototypeChain
        {
            continue;
        }

        if let Some(cached_prototype) = cache_entry.prototype {
            // OPTIMIZATION: If the prototype chain hasn't been mutated in a way that would invalidate the cache, we can use it.
            let can_use_cache = Some(shape) == cache_entry.shape
                && (!shape.is_dictionary() || shape.dictionary_generation() == cache_entry.shape_dictionary_generation)
                && cache_entry
                    .prototype_chain_validity
                    .is_some_and(|validity| validity.is_valid());
            if can_use_cache {
                let value = cached_prototype.get_direct(cache_entry.property_offset);
                return get_cached_property_value(vm, value, this_value);
            }
        } else if Some(shape) == cache_entry.shape {
            // OPTIMIZATION: If the shape of the object hasn't changed, we can use the cached property offset.
            let can_use_cache =
                !shape.is_dictionary() || shape.dictionary_generation() == cache_entry.shape_dictionary_generation;

            if can_use_cache {
                let value = base_obj.get_direct(cache_entry.property_offset);
                return get_cached_property_value(vm, value, this_value);
            }
        }
    }
    let prototype_chain_validity = shape
        .prototype()
        .and_then(|prototype| prototype.shape().prototype_chain_validity());

    let dictionary_generation = shape.dictionary_generation();
    let mut cacheable_metadata = CacheableGetPropertyMetadata {
        property_absence_is_cacheable: base_obj.is_cacheable_for_property_absence(),
        ..Default::default()
    };
    let value = base_obj.internal_get(
        vm,
        property_name,
        this_value,
        Some(&mut cacheable_metadata),
        PropertyLookupPhase::OwnProperty,
    )?;

    // If internal_get() caused object's shape change, we can no longer be sure
    // that collected metadata is valid, e.g. if getter in prototype chain added
    // property with the same name into the object itself. The same applies when
    // a getter changed the property storage of a dictionary shape.
    if shape == base_obj.shape() && shape.dictionary_generation() == dictionary_generation {
        match cacheable_metadata.r#type {
            CacheableGetPropertyMetadataType::GetOwnProperty => {
                cache.update(PropertyLookupCacheEntryType::GetOwnProperty, |entry| {
                    entry.shape = Some(shape);
                    entry.property_offset = cacheable_metadata
                        .property_offset
                        .expect("cacheable metadata has an offset");

                    if shape.is_dictionary() {
                        entry.shape_dictionary_generation = shape.dictionary_generation();
                    }
                });
            }
            CacheableGetPropertyMetadataType::GetPropertyInPrototypeChain => {
                cache.update(PropertyLookupCacheEntryType::GetPropertyInPrototypeChain, |entry| {
                    entry.shape = Some(base_obj.shape());
                    entry.property_offset = cacheable_metadata
                        .property_offset
                        .expect("cacheable metadata has an offset");
                    entry.prototype = cacheable_metadata.prototype;
                    entry.prototype_chain_validity = prototype_chain_validity;

                    if shape.is_dictionary() {
                        entry.shape_dictionary_generation = shape.dictionary_generation();
                    }
                });
            }
            CacheableGetPropertyMetadataType::GetMissingProperty
                if cache_property_absence == CachePropertyAbsence::Yes =>
            {
                cache.update(PropertyLookupCacheEntryType::GetMissingProperty, |entry| {
                    entry.shape = Some(shape);
                    entry.prototype_chain_validity = prototype_chain_validity;

                    if shape.is_dictionary() {
                        entry.shape_dictionary_generation = shape.dictionary_generation();
                    }
                });
            }
            _ => {}
        }
    }

    Ok(value)
}

#[cold]
fn throw_null_or_undefined_property_access<T>(
    vm: &Vm,
    base_value: Value,
    base_identifier: Option<Utf16FlyString>,
    property_identifier: &PropertyKey,
) -> ThrowCompletionOr<T> {
    assert!(base_value.is_nullish());

    if let Some(base_identifier) = base_identifier {
        return vm.throw_completion(
            ErrorKind::TypeError,
            ErrorType::ToObjectNullOrUndefinedWithPropertyAndName,
            &[property_identifier, &base_value, &display_fly_string(&base_identifier)],
        );
    }
    vm.throw_completion(
        ErrorKind::TypeError,
        ErrorType::ToObjectNullOrUndefinedWithProperty,
        &[property_identifier, &base_value],
    )
}

#[allow(clippy::too_many_arguments)]
pub fn put_by_property_key(
    vm: &Vm,
    base: Value,
    this_value: Value,
    value: Value,
    get_base_identifier: impl FnOnce() -> Option<Utf16FlyString>,
    name: &PropertyKey,
    kind: PutKind,
    strict: Strict,
    caches: Option<&PropertyLookupCache>,
) -> ThrowCompletionOr<()> {
    // Better error message than to_object would give
    if strict == Strict::Yes && base.is_nullish() {
        return vm.throw_completion(
            ErrorKind::TypeError,
            ErrorType::ReferenceNullishSetProperty,
            &[name, &base],
        );
    }

    // a. Let baseObj be ? ToObject(V.[[Base]]).
    if base.is_nullish() {
        return throw_null_or_undefined_property_access(vm, base, get_base_identifier(), name);
    }
    let object = base.to_object(vm)?;

    if kind == PutKind::Getter || kind == PutKind::Setter {
        // The generator should only pass us functions for getters and setters.
        assert!(value.is_function());
    }
    match kind {
        PutKind::Getter | PutKind::Setter => {
            let function = value.as_function();
            if let Some(ecmascript_function) = as_ecmascript_function_object(function)
                && ecmascript_function.name().is_empty()
            {
                let prefix = if kind == PutKind::Getter { "get" } else { "set" };
                ecmascript_function.set_inferred_name(vm, &ClassElementName::PropertyKey(name.clone()), Some(prefix));
            }
            let attributes = PropertyAttributes::new(Attribute::CONFIGURABLE | Attribute::ENUMERABLE);
            if kind == PutKind::Getter {
                object.define_direct_accessor(vm, name, Some(function), None, attributes);
            } else {
                object.define_direct_accessor(vm, name, None, Some(function), attributes);
            }
        }
        PutKind::Normal => {
            let this_value_object = this_value.to_object(vm).must();
            let from_shape = this_value_object.shape();
            let from_shape_dictionary_generation = from_shape.dictionary_generation();
            if let Some(caches) = caches {
                for cache in caches.entries_for_shape(object.shape()).as_slice() {
                    match cache.entry_type {
                        PropertyLookupCacheEntryType::Empty => {}
                        PropertyLookupCacheEntryType::ChangePropertyInPrototypeChain => {
                            let Some(cached_prototype) = cache.prototype else {
                                continue;
                            };
                            let Some(cached_shape) = cache.shape else {
                                continue;
                            };
                            // OPTIMIZATION: If the prototype chain hasn't been mutated in a way that would invalidate the cache, we can use it.
                            let can_use_cache = object.shape() == cached_shape
                                && (!cached_shape.is_dictionary()
                                    || object.shape().dictionary_generation() == cache.shape_dictionary_generation)
                                && cache
                                    .prototype_chain_validity
                                    .is_some_and(|validity| validity.is_valid());
                            if can_use_cache {
                                let value_in_prototype = cached_prototype.get_direct(cache.property_offset);
                                if value_in_prototype.is_accessor() {
                                    let Some(setter) = value_in_prototype.as_accessor().setter() else {
                                        continue;
                                    };
                                    call_function_object(vm, setter, this_value, &[value])?;
                                    return Ok(());
                                }
                            }
                        }
                        PropertyLookupCacheEntryType::ChangeOwnProperty => {
                            let Some(cached_shape) = cache.shape else {
                                continue;
                            };
                            if cached_shape != object.shape() {
                                continue;
                            }

                            if cached_shape.is_dictionary()
                                && cached_shape.dictionary_generation() != cache.shape_dictionary_generation
                            {
                                continue;
                            }

                            let value_in_object = object.get_direct(cache.property_offset);
                            if value_in_object.is_accessor() {
                                let Some(setter) = value_in_object.as_accessor().setter() else {
                                    continue;
                                };
                                call_function_object(vm, setter, this_value, &[value])?;
                                return Ok(());
                            }
                            if !cache.writes_data_property {
                                continue;
                            }
                            object.put_direct(cache.property_offset, value);
                            return Ok(());
                        }
                        PropertyLookupCacheEntryType::AddOwnProperty => {
                            // OPTIMIZATION: If the object's shape is the same as the one cached before adding the new property, we can
                            //               reuse the resulting shape from the cache.
                            if cache.from_shape != Some(object.shape()) {
                                continue;
                            }
                            if !property_addition_is_cacheable(vm, &object, name) {
                                continue;
                            }
                            let Some(cached_shape) = cache.shape else {
                                continue;
                            };

                            // Cannot add properties to non-extensible objects (frozen, sealed, or preventExtensions).
                            if !object.internal_is_extensible(vm)? {
                                continue;
                            }

                            if cached_shape.is_dictionary()
                                && object.shape().dictionary_generation() != cache.shape_dictionary_generation
                            {
                                continue;
                            }

                            // The cache is invalid if the prototype chain has been mutated, since such a mutation could have added a setter for the property.
                            if cache
                                .prototype_chain_validity
                                .is_some_and(|validity| !validity.is_valid())
                            {
                                continue;
                            }
                            object.unsafe_set_shape(cached_shape);
                            object.put_direct(cache.property_offset, value);
                            return Ok(());
                        }
                        PropertyLookupCacheEntryType::GetOwnProperty
                        | PropertyLookupCacheEntryType::GetPropertyInPrototypeChain
                        | PropertyLookupCacheEntryType::GetMissingProperty => {}
                    }
                }
            }

            let mut cacheable_metadata = CacheableSetPropertyMetadata::default();
            let succeeded = object.internal_set(
                vm,
                name,
                value,
                this_value,
                Some(&mut cacheable_metadata),
                PropertyLookupPhase::OwnProperty,
            )?;

            if let Some(caches) = caches
                && succeeded
                && cacheable_metadata.r#type == CacheableSetPropertyMetadataType::AddOwnProperty
            {
                caches.update(PropertyLookupCacheEntryType::AddOwnProperty, |cache| {
                    cache.from_shape = Some(from_shape);
                    cache.property_offset = cacheable_metadata
                        .property_offset
                        .expect("cacheable metadata has an offset");
                    cache.shape = Some(object.shape());
                    if let Some(prototype) = cacheable_metadata.prototype {
                        cache.prototype_chain_validity = prototype.shape().prototype_chain_validity();
                    }
                    if object.shape().is_dictionary() {
                        cache.shape_dictionary_generation = object.shape().dictionary_generation();
                    }
                });
            }

            // If internal_set() caused object's shape change, we can no longer be sure
            // that collected metadata is valid, e.g. if setter in prototype chain added
            // property with the same name into the object itself. The same applies when
            // a setter changed the property storage of a dictionary shape.
            if let Some(caches) = caches
                && succeeded
                && from_shape == object.shape()
                && from_shape.dictionary_generation() == from_shape_dictionary_generation
            {
                match cacheable_metadata.r#type {
                    CacheableSetPropertyMetadataType::AddOwnProperty => {
                        // Something went wrong if we ended up here, because cacheable addition of a new property should've changed the shape.
                        unreachable!("a cacheable addition of a property changes the shape");
                    }
                    CacheableSetPropertyMetadataType::ChangeOwnProperty => {
                        caches.update(PropertyLookupCacheEntryType::ChangeOwnProperty, |cache| {
                            cache.shape = Some(object.shape());
                            cache.property_offset = cacheable_metadata
                                .property_offset
                                .expect("cacheable metadata has an offset");
                            cache.writes_data_property = cacheable_metadata.writes_data_property;

                            if object.shape().is_dictionary() {
                                cache.shape_dictionary_generation = object.shape().dictionary_generation();
                            }
                        });
                    }
                    CacheableSetPropertyMetadataType::ChangePropertyInPrototypeChain => {
                        caches.update(PropertyLookupCacheEntryType::ChangePropertyInPrototypeChain, |cache| {
                            cache.shape = Some(object.shape());
                            cache.property_offset = cacheable_metadata
                                .property_offset
                                .expect("cacheable metadata has an offset");
                            let prototype = cacheable_metadata
                                .prototype
                                .expect("a property in the prototype chain has a holder");
                            cache.prototype = Some(prototype);
                            cache.prototype_chain_validity = prototype.shape().prototype_chain_validity();

                            if object.shape().is_dictionary() {
                                cache.shape_dictionary_generation = object.shape().dictionary_generation();
                            }
                        });
                    }
                    CacheableSetPropertyMetadataType::NotCacheable => {}
                }
            }

            if !succeeded && strict == Strict::Yes {
                if base.is_object() {
                    return vm.throw_completion(
                        ErrorKind::TypeError,
                        ErrorType::ReferenceNullishSetProperty,
                        &[name, &base],
                    );
                }
                return vm.throw_completion(
                    ErrorKind::TypeError,
                    ErrorType::ReferencePrimitiveSetProperty,
                    &[name, &base.typeof_(vm).to_utf8(), &base],
                );
            }
        }
        PutKind::Own => {
            if let Some(caches) = caches {
                for cache in caches.entries_for_shape(object.shape()).as_slice() {
                    if cache.entry_type == PropertyLookupCacheEntryType::AddOwnProperty {
                        // PutKind::Own is not currently emitted for platform
                        // objects, but keep this aligned with the normal PutById
                        // AddOwnProperty cache hit so a future bytecode path cannot
                        // bypass subclass hooks for objects that require them.
                        if cache.from_shape != Some(object.shape()) {
                            continue;
                        }
                        if !property_addition_is_cacheable(vm, &object, name) {
                            continue;
                        }
                        let Some(cached_shape) = cache.shape else {
                            continue;
                        };
                        if cached_shape.is_dictionary()
                            && object.shape().dictionary_generation() != cache.shape_dictionary_generation
                        {
                            continue;
                        }
                        object.unsafe_set_shape(cached_shape);
                        object.put_direct(cache.property_offset, value);
                        return Ok(());
                    }
                }
            }

            let from_shape = object.shape();
            object.define_direct_property(
                vm,
                name,
                value,
                PropertyAttributes::new(Attribute::ENUMERABLE | Attribute::WRITABLE | Attribute::CONFIGURABLE),
            );

            if let Some(caches) = caches
                && from_shape != object.shape()
            {
                caches.update(PropertyLookupCacheEntryType::AddOwnProperty, |cache| {
                    cache.from_shape = Some(from_shape);
                    cache.shape = Some(object.shape());
                    cache.property_offset = object.shape().lookup(name).expect("the property was just added").offset;
                    if object.shape().is_dictionary() {
                        cache.shape_dictionary_generation = object.shape().dictionary_generation();
                    }
                });
            }
        }
        PutKind::Prototype => {
            if value.is_object() || value.is_null() {
                object
                    .internal_set_prototype_of(vm, value.is_object().then(|| value.as_object()))
                    .must();
            }
        }
    }

    Ok(())
}

#[cfg(all(test, libjs_runtime_tests_with_libgc))]
mod tests {
    use super::*;
    use crate::bytecode::executable::{
        Executable, ExecutableCacheCounts, PROPERTY_LOOKUP_CACHE_DATA_TAG_MASK, PropertyLookupCacheEntry,
    };
    use crate::gc::root::MarkedVec;
    use crate::runtime::object::ShouldThrowExceptions;
    use crate::runtime::property_descriptor::PropertyDescriptor;
    use crate::runtime::realm::test_realm::{TestRealm, key, thrown_message};

    fn int(value: i32) -> Value {
        Value::from_i32(value)
    }

    fn set(vm: &Vm, object: &Object, name: &str, value: Value) {
        object.set(vm, &key(name), value, ShouldThrowExceptions::Yes).must();
    }

    fn get_cached(vm: &Vm, object: Gc<Object>, name: &str, cache: &PropertyLookupCache) -> Value {
        let object = Value::from_object(object);
        get_by_id(
            vm,
            GetByIdMode::Normal,
            || None,
            &key(name),
            object,
            object,
            cache,
            CachePropertyAbsence::Yes,
        )
        .must()
    }

    fn put_cached(vm: &Vm, object: Gc<Object>, name: &str, value: Value, kind: PutKind, cache: &PropertyLookupCache) {
        let object = Value::from_object(object);
        put_by_property_key(
            vm,
            object,
            object,
            value,
            || None,
            &key(name),
            kind,
            Strict::No,
            Some(cache),
        )
        .must();
    }

    /// The entry the interpreter reads: the one at the start of the cache's data, whatever its tier.
    fn entry_the_interpreter_reads(cache: &PropertyLookupCache) -> &PropertyLookupCacheEntry {
        let data = cache.data.get() & !PROPERTY_LOOKUP_CACHE_DATA_TAG_MASK;
        assert!(data != 0);
        // SAFETY: Every tier starts with an entry, and the cache owns its data until it changes tier.
        unsafe { &*core::ptr::with_exposed_provenance::<PropertyLookupCacheEntry>(data) }
    }

    fn tier(cache: &PropertyLookupCache) -> usize {
        cache.data.get() & PROPERTY_LOOKUP_CACHE_DATA_TAG_MASK
    }

    #[test]
    fn gets_fill_the_entries_the_interpreter_reads() {
        let vm = Vm::create();
        let test_realm = TestRealm::new(&vm);
        let o = test_realm.object();
        set(&vm, &o, "x", int(1));
        set(&vm, &o, "y", int(2));

        let cache = PropertyLookupCache::new();
        assert_eq!(get_cached(&vm, o, "y", &cache), int(2));
        let entry = cache.first_entry().unwrap();
        assert_eq!(entry.entry_type, PropertyLookupCacheEntryType::GetOwnProperty);
        assert_eq!(entry.property_offset, 1);
        assert!(entry.shape == Some(o.shape()) && entry.prototype.is_none());
        assert_eq!(entry_the_interpreter_reads(&cache).property_offset.get(), 1);
        assert_eq!(get_cached(&vm, o, "y", &cache), int(2));

        // An inherited property is cached with its holder and the validity of the holder's chain.
        let child = Object::create(&vm, test_realm.realm, Some(o));
        let inherited_cache = PropertyLookupCache::new();
        assert_eq!(get_cached(&vm, child, "x", &inherited_cache), int(1));
        let entry = inherited_cache.first_entry().unwrap();
        assert_eq!(
            entry.entry_type,
            PropertyLookupCacheEntryType::GetPropertyInPrototypeChain
        );
        assert!(entry.prototype == Some(o) && entry.shape == Some(child.shape()));
        assert!(entry.prototype_chain_validity == o.shape().prototype_chain_validity());
        assert_eq!(entry.property_offset, 0);

        // A new value in the holder is read through the cache; a new property in the holder invalidates it.
        set(&vm, &o, "x", int(3));
        assert_eq!(get_cached(&vm, child, "x", &inherited_cache), int(3));
        set(&vm, &o, "w", int(4));
        assert!(!entry.prototype_chain_validity.unwrap().is_valid());
        assert_eq!(get_cached(&vm, child, "x", &inherited_cache), int(3));
        let refilled = inherited_cache.first_entry().unwrap();
        assert!(refilled.prototype_chain_validity.unwrap().is_valid());

        // Absence is cached for the whole chain.
        let missing_cache = PropertyLookupCache::new();
        assert_eq!(get_cached(&vm, child, "missing", &missing_cache), Value::UNDEFINED);
        let entry = missing_cache.first_entry().unwrap();
        assert_eq!(entry.entry_type, PropertyLookupCacheEntryType::GetMissingProperty);
        assert!(
            entry
                .prototype_chain_validity
                .is_some_and(|validity| validity.is_valid())
        );
        set(&vm, &o, "missing", int(5));
        assert_eq!(get_cached(&vm, child, "missing", &missing_cache), int(5));

        // Arrays are not cacheable for absence, since their length is not a stored property.
        let array = test_realm.array(&[int(1)]);
        let array_cache = PropertyLookupCache::new();
        assert_eq!(
            get_cached(&vm, array.upcast(), "nothing", &array_cache),
            Value::UNDEFINED
        );
        assert!(array_cache.first_entry().is_none());
    }

    #[test]
    fn caches_move_through_the_tiers_keeping_the_newest_entry_first() {
        let vm = Vm::create();
        let test_realm = TestRealm::new(&vm);
        let objects = MarkedVec::new(&vm);
        for index in 0..6 {
            let object = test_realm.object();
            set(&vm, &object, &format!("a{index}"), int(index));
            set(&vm, &object, "x", int(100 + index));
            objects.push(object);
        }
        let object_at = |index: usize| -> Gc<Object> { objects.get(index).expect("the index is in bounds") };

        let cache = PropertyLookupCache::new();
        for index in 0..4 {
            let object = object_at(index);
            assert_eq!(get_cached(&vm, object, "x", &cache), int(100 + index as i32));
            assert!(cache.first_entry().unwrap().shape == Some(object.shape()));
        }
        assert_eq!(tier(&cache), 1);
        assert_eq!(cache.entries_for_shape(object_at(0).shape()).as_slice().len(), 4);

        assert_eq!(get_cached(&vm, object_at(4), "x", &cache), int(104));
        assert_eq!(tier(&cache), 2);
        assert!(entry_the_interpreter_reads(&cache).shape.get() == Some(object_at(4).shape()));
        for index in 0..objects.len() {
            let object = object_at(index);
            assert_eq!(get_cached(&vm, object, "x", &cache), int(100 + index as i32));
            let found = cache.entries_for_shape(object.shape());
            assert_eq!(found.as_slice().len(), 1);
            assert!(entry_the_interpreter_reads(&cache).shape.get() == Some(object.shape()));
        }
        cache.clear();
        assert!(cache.first_entry().is_none());
    }

    #[test]
    fn puts_fill_the_cache_and_replay_additions() {
        let vm = Vm::create();
        let test_realm = TestRealm::new(&vm);
        let cache = PropertyLookupCache::new();
        let a = test_realm.object();
        put_cached(&vm, a, "x", int(1), PutKind::Normal, &cache);
        let entry = cache.first_entry().unwrap();
        assert_eq!(entry.entry_type, PropertyLookupCacheEntryType::AddOwnProperty);
        assert!(entry.from_shape == Some(test_realm.realm.new_object_shape()) && entry.shape == Some(a.shape()));
        assert_eq!(entry.property_offset, 0);

        let b = test_realm.object();
        put_cached(&vm, b, "x", int(2), PutKind::Normal, &cache);
        assert!(b.shape() == a.shape() && tier(&cache) == 0);
        assert_eq!(b.get(&vm, &key("x")).must(), int(2));

        put_cached(&vm, a, "x", int(3), PutKind::Normal, &cache);
        assert_eq!(tier(&cache), 1);
        let entry = cache.first_entry().unwrap();
        assert_eq!(entry.entry_type, PropertyLookupCacheEntryType::ChangeOwnProperty);
        assert!(entry.writes_data_property && entry.shape == Some(a.shape()));
        put_cached(&vm, b, "x", int(4), PutKind::Normal, &cache);
        assert_eq!(b.get(&vm, &key("x")).must(), int(4));
        assert_eq!(a.get(&vm, &key("x")).must(), int(3));

        // A cached addition does not apply to an object that is not extensible.
        let c = test_realm.object();
        c.internal_prevent_extensions(&vm).must();
        put_cached(&vm, c, "x", int(5), PutKind::Normal, &cache);
        assert!(c.storage_get(&vm, &key("x")).is_none());
        let strict_failure = thrown_message(|| {
            let c = Value::from_object(c);
            put_by_property_key(
                &vm,
                c,
                c,
                int(5),
                || None,
                &key("x"),
                PutKind::Normal,
                Strict::Yes,
                Some(&cache),
            )
        });
        assert!(strict_failure.contains("Cannot set property 'x' of [object Object]"));

        // A put to an inherited accessor without a setter fails and caches nothing.
        let prototype = test_realm.object();
        let mut accessor = PropertyDescriptor {
            get: Some(None),
            set: Some(None),
            configurable: Some(true),
            ..Default::default()
        };
        prototype.define_property_or_throw(&vm, &key("y"), &mut accessor).must();
        let d = Object::create(&vm, test_realm.realm, Some(prototype));
        let accessor_cache = PropertyLookupCache::new();
        put_cached(&vm, d, "y", int(1), PutKind::Normal, &accessor_cache);
        assert!(accessor_cache.first_entry().is_none() && d.storage_get(&vm, &key("y")).is_none());

        // An own put defines the property over the accessor and caches the addition.
        put_cached(&vm, d, "y", int(1), PutKind::Own, &accessor_cache);
        assert_eq!(d.storage_get(&vm, &key("y")).unwrap().value, int(1));
        let entry = accessor_cache.first_entry().unwrap();
        assert_eq!(entry.entry_type, PropertyLookupCacheEntryType::AddOwnProperty);
        let e = Object::create(&vm, test_realm.realm, Some(prototype));
        put_cached(&vm, e, "y", int(2), PutKind::Own, &accessor_cache);
        assert!(e.shape() == d.shape());
        assert_eq!(e.storage_get(&vm, &key("y")).unwrap().value, int(2));
    }

    #[test]
    fn own_property_reads_without_side_effects_cache_their_lookups() {
        let vm = Vm::create();
        let test_realm = TestRealm::new(&vm);
        let o = test_realm.object();
        set(&vm, &o, "x", int(1));
        let cache = PropertyLookupCache::new();
        assert_eq!(get_own_property_without_side_effects(&o, &key("x"), &cache), int(1));
        assert_eq!(get_own_property_without_side_effects(&o, &key("x"), &cache), int(1));
        let missing_cache = PropertyLookupCache::new();
        assert_eq!(
            get_own_property_without_side_effects(&o, &key("y"), &missing_cache),
            Value::EMPTY
        );
        assert_eq!(
            missing_cache.first_entry().unwrap().entry_type,
            PropertyLookupCacheEntryType::GetMissingProperty
        );
    }

    #[inline(never)]
    fn fill_caches_with_temporary_shapes(vm: &Vm, test_realm: &TestRealm, executable: &Executable) {
        for index in 0..64 {
            let object = test_realm.object();
            let name = format!("temporary{index}");
            set(vm, &object, &name, int(index as i32));
            assert_eq!(
                get_cached(vm, object, &name, executable.property_lookup_cache(index)),
                int(index as i32)
            );
        }
    }

    #[test]
    fn executables_forget_the_shapes_that_die() {
        let vm = Vm::create();
        let test_realm = TestRealm::new(&vm);
        let counts = ExecutableCacheCounts {
            property_lookup_caches: 64,
            global_variable_caches: 0,
            environment_coordinate_caches: 0,
            environment_shape_caches: 0,
        };
        let executable = Executable::create_from_parts(
            &vm,
            Executable::new(vec![0u8; 8].into_boxed_slice(), 5, 0, 0, Box::new([]), &counts, true),
        );
        fill_caches_with_temporary_shapes(&vm, &test_realm, &executable);
        vm.heap().collect_garbage();
        let pruned = (0..64)
            .filter(|index| {
                executable
                    .property_lookup_cache(*index)
                    .first_entry()
                    .is_some_and(|entry| entry.shape.is_none())
            })
            .count();
        assert!(pruned >= 32, "only {pruned} of the 64 dead shapes were pruned");

        // The live entries still work, and so does filling a pruned one again.
        let o = test_realm.object();
        set(&vm, &o, "temporary0", int(7));
        assert_eq!(
            get_cached(&vm, o, "temporary0", executable.property_lookup_cache(0)),
            int(7)
        );
    }
}

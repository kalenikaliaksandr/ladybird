/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! Libraries/LibJS/Runtime/Value.cpp: the conversions and operators on values.

use core::fmt::{self, Write};
use core::ptr::NonNull;

use ak::Utf16String;
use num_traits::Zero;

use crate::build_configuration::HEAP_REGION_OFFSET_MASK;
use crate::bytecode::executable::{PropertyLookupCache, StaticPropertyLookupCacheSite};
use crate::bytecode::property_access::{CachePropertyAbsence, GetByIdMode, get_by_id};
use crate::gc::capi::js_heap_region_base;
use crate::gc::class_id::ClassId;
use crate::interpreter::runtime_functions::unimplemented_runtime_function;
use crate::interpreter::vm::Vm;
use crate::layout::cell::Gc;
use crate::layout::function_object::FunctionObject;
use crate::layout::object::Object;
use crate::layout::primitive_string::PrimitiveString;
use crate::layout::value::Value;
use crate::runtime::abstract_operations::{call, call_function_object};
use crate::runtime::accessor::Accessor;
use crate::runtime::array::Array;
use crate::runtime::big_int::{BigInt, SignedBigInteger};
use crate::runtime::big_int_algorithms::{self, CompareResult};
use crate::runtime::big_int_object::BigIntObject;
use crate::runtime::boolean_object::BooleanObject;
use crate::runtime::bound_function::BoundFunction;
use crate::runtime::completion::{Must, ThrowCompletionOr};
use crate::runtime::error::ErrorKind;
use crate::runtime::error_types::ErrorType;
use crate::runtime::number_object::NumberObject;
use crate::runtime::object::PropertyLookupPhase;
use crate::runtime::property_key::PropertyKey;
use crate::runtime::string_object::StringObject;
use crate::runtime::symbol::Symbol;
use crate::runtime::symbol_object::SymbolObject;
use crate::runtime::value_conversions::{self, string_to_number};
use crate::utf16::Utf16View;
use libjs_abi::Builtin;
use libjs_abi::value as nan_box;

impl Value {
    pub const fn from_bool(value: bool) -> Self {
        if value { Self::TRUE } else { Self::FALSE }
    }

    pub const fn from_i32(value: i32) -> Self {
        Self(nan_box::SHIFTED_INT32_TAG | value as u32 as u64)
    }

    /// Like the C++ Value(double): integral doubles that fit in an i32, other than negative zero, are stored as
    /// Int32, and every NaN becomes the canonical NaN.
    pub fn from_f64(value: f64) -> Self {
        let is_negative_zero = value.to_bits() == nan_box::NEGATIVE_ZERO_BITS;
        if value >= f64::from(i32::MIN) && value <= f64::from(i32::MAX) && value.trunc() == value && !is_negative_zero {
            return Self::from_i32(value as i32);
        }
        if value.is_nan() {
            return Self(nan_box::CANON_NAN_BITS);
        }
        Self(value.to_bits())
    }

    pub const fn tag(self) -> u64 {
        self.0 >> nan_box::TAG_SHIFT
    }

    pub const fn is_empty(self) -> bool {
        self.0 == nan_box::EMPTY_VALUE
    }

    pub const fn is_undefined(self) -> bool {
        self.0 == nan_box::UNDEFINED_VALUE
    }

    pub const fn is_null(self) -> bool {
        self.0 == nan_box::NULL_VALUE
    }

    pub const fn is_nullish(self) -> bool {
        (self.tag() & nan_box::IS_NULLISH_EXTRACT_PATTERN) == nan_box::IS_NULLISH_PATTERN
    }

    pub const fn is_boolean(self) -> bool {
        self.tag() == nan_box::BOOLEAN_TAG
    }

    pub const fn is_int32(self) -> bool {
        self.tag() == nan_box::INT32_TAG
    }

    pub const fn is_double(self) -> bool {
        (self.0 & nan_box::CANON_NAN_BITS) != nan_box::CANON_NAN_BITS || self.0 == nan_box::CANON_NAN_BITS
    }

    pub const fn is_number(self) -> bool {
        self.is_double() || self.is_int32()
    }

    pub const fn is_cell(self) -> bool {
        (self.tag() & nan_box::IS_CELL_PATTERN) == nan_box::IS_CELL_PATTERN
    }

    pub const fn is_object(self) -> bool {
        self.tag() == nan_box::OBJECT_TAG
    }

    pub const fn is_string(self) -> bool {
        self.tag() == nan_box::STRING_TAG
    }

    pub const fn is_symbol(self) -> bool {
        self.tag() == nan_box::SYMBOL_TAG
    }

    pub const fn is_bigint(self) -> bool {
        self.tag() == nan_box::BIGINT_TAG
    }

    pub const fn is_accessor(self) -> bool {
        self.tag() == nan_box::ACCESSOR_TAG
    }

    pub fn as_bool(self) -> bool {
        assert!(self.is_boolean());
        self.0 & 1 != 0
    }

    pub fn as_i32(self) -> i32 {
        debug_assert!(self.is_int32());
        self.0 as u32 as i32
    }

    /// The numeric value of a number.
    pub fn as_f64(self) -> f64 {
        debug_assert!(self.is_number());
        if self.is_int32() {
            return f64::from(self.as_i32());
        }
        f64::from_bits(self.0)
    }

    pub fn is_integral_number(self) -> bool {
        if self.is_int32() {
            return true;
        }
        self.is_finite_number() && self.as_f64().trunc() == self.as_f64()
    }

    pub fn is_finite_number(self) -> bool {
        if !self.is_number() {
            return false;
        }
        if self.is_int32() {
            return true;
        }
        self.as_f64().is_finite()
    }

    pub(crate) fn with_cell_tag<T>(tag: u64, cell: Gc<T>) -> Self {
        let address = cell.as_ptr() as usize as u64;
        Self((tag << nan_box::TAG_SHIFT) | (address & HEAP_REGION_OFFSET_MASK))
    }

    /// # Safety
    ///
    /// The value must hold a cell of type T.
    pub(crate) unsafe fn cell<T>(self) -> Gc<T> {
        debug_assert!(self.is_cell());
        // SAFETY: Cell values are offsets into the heap region, whose base LibGC fixed before any cell existed.
        let base = unsafe { js_heap_region_base } as u64;
        let address = (base + (self.0 & HEAP_REGION_OFFSET_MASK)) as usize;
        // SAFETY: The caller guarantees the value holds a live cell of type T.
        unsafe { Gc::from_non_null(NonNull::new_unchecked(core::ptr::without_provenance_mut::<T>(address))) }
    }

    pub fn from_object<T>(object: Gc<T>) -> Self {
        Self::with_cell_tag(nan_box::OBJECT_TAG, object)
    }

    pub fn from_string(string: Gc<PrimitiveString>) -> Self {
        Self::with_cell_tag(nan_box::STRING_TAG, string)
    }

    pub fn from_symbol(symbol: Gc<Symbol>) -> Self {
        Self::with_cell_tag(nan_box::SYMBOL_TAG, symbol)
    }

    pub fn from_bigint(bigint: Gc<BigInt>) -> Self {
        Self::with_cell_tag(nan_box::BIGINT_TAG, bigint)
    }

    pub fn from_accessor(accessor: Gc<Accessor>) -> Self {
        Self::with_cell_tag(nan_box::ACCESSOR_TAG, accessor)
    }

    pub fn as_object(self) -> Gc<Object> {
        assert!(self.is_object());
        // SAFETY: The tag says the value holds an object.
        unsafe { self.cell() }
    }

    pub fn as_string(self) -> Gc<PrimitiveString> {
        assert!(self.is_string());
        // SAFETY: The tag says the value holds a string.
        unsafe { self.cell() }
    }

    pub fn as_symbol(self) -> Gc<Symbol> {
        assert!(self.is_symbol());
        // SAFETY: The tag says the value holds a symbol.
        unsafe { self.cell() }
    }

    pub fn as_bigint(self) -> Gc<BigInt> {
        assert!(self.is_bigint());
        // SAFETY: The tag says the value holds a BigInt.
        unsafe { self.cell() }
    }

    pub fn as_accessor(self) -> Gc<Accessor> {
        assert!(self.is_accessor());
        // SAFETY: The tag says the value holds an accessor.
        unsafe { self.cell() }
    }

    pub fn is_nan(self) -> bool {
        self.is_number() && self.as_f64().is_nan()
    }

    pub fn is_infinity(self) -> bool {
        self.is_number() && self.as_f64().is_infinite()
    }

    pub fn is_positive_zero(self) -> bool {
        self.is_number() && self.as_f64().to_bits() == 0
    }

    pub fn is_negative_zero(self) -> bool {
        self.is_double() && self.0 == nan_box::NEGATIVE_ZERO_BITS
    }

    pub const fn is_positive_infinity(self) -> bool {
        self.0 == nan_box::POSITIVE_INFINITY_BITS
    }

    pub const fn is_negative_infinity(self) -> bool {
        self.0 == nan_box::NEGATIVE_INFINITY_BITS
    }

    pub const fn is_non_negative_int32(self) -> bool {
        (self.0 & (nan_box::TAG_EXTRACTION | 0x8000_0000)) == nan_box::SHIFTED_INT32_TAG
    }

    // 7.2.3 IsCallable ( argument ), https://tc39.es/ecma262/#sec-iscallable
    pub fn is_function(self) -> bool {
        // 1. If argument is not an Object, return false.
        // 2. If argument has a [[Call]] internal method, return true.
        // 3. Return false.
        self.is_object() && self.as_object().is_function()
    }

    pub fn as_function(self) -> Gc<FunctionObject> {
        assert!(self.is_function());
        // SAFETY: The object has the IsFunction flag, which only function objects set.
        unsafe { self.cell() }
    }

    // 7.2.4 IsConstructor ( argument ), https://tc39.es/ecma262/#sec-isconstructor
    pub fn is_constructor(self) -> bool {
        // 1. If Type(argument) is not Object, return false.
        if !self.is_function() {
            return false;
        }

        // 2. If argument has a [[Construct]] internal method, return true.
        // 3. Return false.
        self.as_object().has_constructor()
    }

    // 7.2.2 IsArray ( argument ), https://tc39.es/ecma262/#sec-isarray
    pub fn is_array(self, _vm: &Vm) -> ThrowCompletionOr<bool> {
        // 1. If argument is not an Object, return false.
        if !self.is_object() {
            return Ok(false);
        }

        let object = self.as_object();

        // 2. If argument is an Array exotic object, return true.
        if object.is::<Array>() {
            return Ok(true);
        }

        // 3. If argument is a Proxy exotic object, then
        if object.class().id == ClassId::ProxyObject {
            // a. Perform ? ValidateNonRevokedProxy(argument).
            // b. Let proxyTarget be argument.[[ProxyTarget]].
            // c. Return ? IsArray(proxyTarget).
            unimplemented_runtime_function("IsArray of a Proxy exotic object, which needs ProxyObject", 0);
        }

        // 4. Return false.
        Ok(false)
    }

    // 7.2.8 IsRegExp ( argument ), https://tc39.es/ecma262/#sec-isregexp
    pub fn is_regexp(self, vm: &Vm) -> ThrowCompletionOr<bool> {
        // 1. If argument is not an Object, return false.
        if !self.is_object() {
            return Ok(false);
        }

        // 2. Let matcher be ? Get(argument, @@match).
        let matcher = self.as_object().get_with_cache(
            vm,
            &PropertyKey::from(vm.well_known_symbols().match_),
            vm.static_property_lookup_cache(StaticPropertyLookupCacheSite::ValueIsRegExp),
        )?;

        // 3. If matcher is not undefined, return ToBoolean(matcher).
        if !matcher.is_undefined() {
            return Ok(matcher.to_boolean());
        }

        // 4. If argument has a [[RegExpMatcher]] internal slot, return true.
        // 5. Return false.
        Ok(self.as_object().class().id == ClassId::RegExpObject)
    }

    // 7.1.2 ToBoolean ( argument ), https://tc39.es/ecma262/#sec-toboolean
    pub fn to_boolean(self) -> bool {
        if self.is_boolean() {
            return self.as_bool();
        }
        self.to_boolean_slow_case()
    }

    fn to_boolean_slow_case(self) -> bool {
        if self.is_double() {
            if self.is_nan() {
                return false;
            }
            return self.as_f64() != 0.0;
        }

        match self.tag() {
            // 1. If argument is a Boolean, return argument.
            nan_box::BOOLEAN_TAG => self.as_bool(),
            // 2. If argument is any of undefined, null, +0𝔽, -0𝔽, NaN, 0ℤ, or the empty String, return false.
            nan_box::UNDEFINED_TAG | nan_box::NULL_TAG => false,
            nan_box::INT32_TAG => self.as_i32() != 0,
            nan_box::STRING_TAG => !self.as_string().is_empty(),
            nan_box::BIGINT_TAG => !num_traits::Zero::is_zero(self.as_bigint().big_integer()),
            nan_box::OBJECT_TAG => {
                // B.3.6.1 Changes to ToBoolean, https://tc39.es/ecma262/#sec-IsHTMLDDA-internal-slot-to-boolean
                // 3. If argument is an Object and argument has an [[IsHTMLDDA]] internal slot, return false.
                // 4. Return true.
                !self.as_object().is_htmldda()
            }
            nan_box::SYMBOL_TAG => true,
            _ => unreachable!("ToBoolean of a value that is not a language value"),
        }
    }

    // 7.1.1 ToPrimitive ( input [ , preferredType ] ), https://tc39.es/ecma262/#sec-toprimitive
    pub fn to_primitive(self, vm: &Vm, preferred_type: PreferredType) -> ThrowCompletionOr<Value> {
        if !self.is_object() {
            return Ok(self);
        }
        self.to_primitive_slow_case(vm, preferred_type)
    }

    fn to_primitive_slow_case(self, vm: &Vm, mut preferred_type: PreferredType) -> ThrowCompletionOr<Value> {
        // 1. If input is an Object, then
        if self.is_object() {
            // a. Let exoticToPrim be ? GetMethod(input, @@toPrimitive).
            let exotic_to_primitive = self.get_method_with_cache(
                vm,
                &PropertyKey::from(vm.well_known_symbols().to_primitive),
                vm.static_property_lookup_cache(StaticPropertyLookupCacheSite::ValueToPrimitive),
            )?;

            // b. If exoticToPrim is not undefined, then
            if let Some(exotic_to_primitive) = exotic_to_primitive {
                let hint = match preferred_type {
                    // i. If preferredType is not present, let hint be "default".
                    PreferredType::Default => "default",
                    // ii. Else if preferredType is string, let hint be "string".
                    PreferredType::String => "string",
                    // iii. Else,
                    // 1. Assert: preferredType is number.
                    // 2. Let hint be "number".
                    PreferredType::Number => "number",
                };

                // iv. Let result be ? Call(exoticToPrim, input, « hint »).
                let hint_string = Value::from_string(PrimitiveString::create_from_utf8(vm, hint));
                let result = call_function_object(vm, exotic_to_primitive, self, &[hint_string])?;

                // v. If result is not an Object, return result.
                if !result.is_object() {
                    return Ok(result);
                }

                // vi. Throw a TypeError exception.
                return vm.throw_completion(
                    ErrorKind::TypeError,
                    ErrorType::ToPrimitiveReturnedObject,
                    &[&self, &hint],
                );
            }

            // c. If preferredType is not present, let preferredType be number.
            if preferred_type == PreferredType::Default {
                preferred_type = PreferredType::Number;
            }

            // d. Return ? OrdinaryToPrimitive(input, preferredType).
            return self.as_object().ordinary_to_primitive(vm, preferred_type);
        }

        // 2. Return input.
        Ok(self)
    }

    // 7.1.18 ToObject ( argument ), https://tc39.es/ecma262/#sec-toobject
    pub fn to_object(self, vm: &Vm) -> ThrowCompletionOr<Gc<Object>> {
        if self.is_object() {
            return Ok(self.as_object());
        }
        self.to_object_slow(vm)
    }

    fn to_object_slow(self, vm: &Vm) -> ThrowCompletionOr<Gc<Object>> {
        let realm = vm
            .current_realm()
            .expect("ToObject runs in an execution context with a realm");
        assert!(!self.is_empty());

        // Number
        if self.is_number() {
            // Return a new Number object whose [[NumberData]] internal slot is set to argument. See 21.1 for a description of Number objects.
            return Ok(NumberObject::create(vm, realm, self.as_f64()).upcast());
        }

        match self.tag() {
            // Undefined
            // Null
            nan_box::UNDEFINED_TAG | nan_box::NULL_TAG => {
                // Throw a TypeError exception.
                vm.throw_completion(ErrorKind::TypeError, ErrorType::ToObjectNullOrUndefined, &[])
            }
            // Boolean
            nan_box::BOOLEAN_TAG => {
                // Return a new Boolean object whose [[BooleanData]] internal slot is set to argument. See 20.3 for a description of Boolean objects.
                Ok(BooleanObject::create(vm, realm, self.as_bool()).upcast())
            }
            // String
            nan_box::STRING_TAG => {
                // Return a new String object whose [[StringData]] internal slot is set to argument. See 22.1 for a description of String objects.
                Ok(StringObject::create(vm, realm, self.as_string(), realm.intrinsics().string_prototype(vm)).upcast())
            }
            // Symbol
            nan_box::SYMBOL_TAG => {
                // Return a new Symbol object whose [[SymbolData]] internal slot is set to argument. See 20.4 for a description of Symbol objects.
                Ok(SymbolObject::create(vm, realm, self.as_symbol()).upcast())
            }
            // BigInt
            nan_box::BIGINT_TAG => {
                // Return a new BigInt object whose [[BigIntData]] internal slot is set to argument. See 21.2 for a description of BigInt objects.
                Ok(BigIntObject::create(vm, realm, self.as_bigint()).upcast())
            }
            // Object
            nan_box::OBJECT_TAG => {
                // Return argument.
                Ok(self.as_object())
            }
            _ => unreachable!("ToObject of a value that is not a language value"),
        }
    }

    // 7.1.4 ToNumber ( argument ), https://tc39.es/ecma262/#sec-tonumber
    pub fn to_number(self, vm: &Vm) -> ThrowCompletionOr<Value> {
        if self.is_number() {
            return Ok(self);
        }
        self.to_number_slow_case(vm)
    }

    fn to_number_slow_case(self, vm: &Vm) -> ThrowCompletionOr<Value> {
        assert!(!self.is_empty());

        // 1. If argument is a Number, return argument.
        if self.is_number() {
            return Ok(self);
        }

        match self.tag() {
            // 2. If argument is either a Symbol or a BigInt, throw a TypeError exception.
            nan_box::SYMBOL_TAG => {
                vm.throw_completion(ErrorKind::TypeError, ErrorType::Convert, &[&"symbol", &"number"])
            }
            nan_box::BIGINT_TAG => {
                vm.throw_completion(ErrorKind::TypeError, ErrorType::Convert, &[&"BigInt", &"number"])
            }
            // 3. If argument is undefined, return NaN.
            nan_box::UNDEFINED_TAG => Ok(Value::from_f64(f64::NAN)),
            // 4. If argument is either null or false, return +0𝔽.
            nan_box::NULL_TAG => Ok(Value::from_i32(0)),
            // 5. If argument is true, return 1𝔽.
            nan_box::BOOLEAN_TAG => Ok(Value::from_i32(i32::from(self.as_bool()))),
            // 6. If argument is a String, return StringToNumber(argument).
            nan_box::STRING_TAG => {
                let string = self.as_string().utf16_string();
                let code_units = string.to_utf16();
                Ok(Value::from_f64(string_to_number(&code_units)))
            }
            // 7. Assert: argument is an Object.
            nan_box::OBJECT_TAG => {
                // 8. Let primValue be ? ToPrimitive(argument, number).
                let primitive_value = self.to_primitive(vm, PreferredType::Number)?;

                // 9. Assert: primValue is not an Object.
                assert!(!primitive_value.is_object());

                // 10. Return ? ToNumber(primValue).
                primitive_value.to_number(vm)
            }
            _ => unreachable!("ToNumber of a value that is not a language value"),
        }
    }

    // 7.1.3 ToNumeric ( value ), https://tc39.es/ecma262/#sec-tonumeric
    pub fn to_numeric(self, vm: &Vm) -> ThrowCompletionOr<Value> {
        // OPTIMIZATION: Fast path for when this value is already a number.
        if self.is_number() {
            return Ok(self);
        }
        self.to_numeric_slow_case(vm)
    }

    #[cold]
    fn to_numeric_slow_case(self, vm: &Vm) -> ThrowCompletionOr<Value> {
        // OPTIMIZATION: Fast paths for some trivial common cases.
        if self.is_boolean() {
            return Ok(Value::from_i32(i32::from(self.as_bool())));
        }
        if self.is_null() {
            return Ok(Value::from_i32(0));
        }
        if self.is_undefined() {
            return Ok(Value::from_f64(f64::NAN));
        }

        // 1. Let primValue be ? ToPrimitive(value, number).
        let primitive_value = self.to_primitive(vm, PreferredType::Number)?;

        // 2. If primValue is a BigInt, return primValue.
        if primitive_value.is_bigint() {
            return Ok(primitive_value);
        }

        // 3. Return ? ToNumber(primValue).
        primitive_value.to_number(vm)
    }

    // 7.1.13 ToBigInt ( argument ), https://tc39.es/ecma262/#sec-tobigint
    pub fn to_bigint(self, vm: &Vm) -> ThrowCompletionOr<Gc<BigInt>> {
        // 1. Let prim be ? ToPrimitive(argument, number).
        let primitive = self.to_primitive(vm, PreferredType::Number)?;

        // 2. Return the value that prim corresponds to in Table 12.

        // Number
        if primitive.is_number() {
            // Throw a TypeError exception.
            return vm.throw_completion(ErrorKind::TypeError, ErrorType::Convert, &[&"number", &"BigInt"]);
        }

        match primitive.tag() {
            // Undefined
            nan_box::UNDEFINED_TAG => {
                // Throw a TypeError exception.
                vm.throw_completion(ErrorKind::TypeError, ErrorType::Convert, &[&"undefined", &"BigInt"])
            }
            // Null
            nan_box::NULL_TAG => {
                // Throw a TypeError exception.
                vm.throw_completion(ErrorKind::TypeError, ErrorType::Convert, &[&"null", &"BigInt"])
            }
            // Boolean
            nan_box::BOOLEAN_TAG => {
                // Return 1n if prim is true and 0n if prim is false.
                let value = i32::from(primitive.as_bool());
                Ok(BigInt::create(vm, SignedBigInteger::from(value)))
            }
            // BigInt
            nan_box::BIGINT_TAG => {
                // Return prim.
                Ok(primitive.as_bigint())
            }
            nan_box::STRING_TAG => {
                // 1. Let n be ! StringToBigInt(prim).
                let bigint = string_to_bigint(vm, &primitive.as_string());

                // 2. If n is undefined, throw a SyntaxError exception.
                let Some(bigint) = bigint else {
                    return vm.throw_completion(ErrorKind::SyntaxError, ErrorType::BigIntInvalidValue, &[&primitive]);
                };

                // 3. Return n.
                Ok(bigint)
            }
            // Symbol
            nan_box::SYMBOL_TAG => {
                // Throw a TypeError exception.
                vm.throw_completion(ErrorKind::TypeError, ErrorType::Convert, &[&"symbol", &"BigInt"])
            }
            _ => unreachable!("ToBigInt of a value that is not a primitive language value"),
        }
    }

    // 7.1.15 ToBigInt64 ( argument ), https://tc39.es/ecma262/#sec-tobigint64
    pub fn to_bigint_int64(self, vm: &Vm) -> ThrowCompletionOr<i64> {
        // 1. Let n be ? ToBigInt(argument).
        let bigint = self.to_bigint(vm)?;

        // 2. Let int64bit be ℝ(n) modulo 2^64.
        // 3. If int64bit ≥ 2^63, return ℤ(int64bit - 2^64); otherwise return ℤ(int64bit).
        Ok(big_int_algorithms::to_u64(bigint.big_integer()) as i64)
    }

    // 7.1.16 ToBigUint64 ( argument ), https://tc39.es/ecma262/#sec-tobiguint64
    pub fn to_bigint_uint64(self, vm: &Vm) -> ThrowCompletionOr<u64> {
        // 1. Let n be ? ToBigInt(argument).
        let bigint = self.to_bigint(vm)?;

        // 2. Let int64bit be ℝ(n) modulo 2^64.
        // 3. Return ℤ(int64bit).
        Ok(big_int_algorithms::to_u64(bigint.big_integer()))
    }

    pub fn to_double(self, vm: &Vm) -> ThrowCompletionOr<f64> {
        Ok(self.to_number(vm)?.as_f64())
    }

    // 7.1.19 ToPropertyKey ( argument ), https://tc39.es/ecma262/#sec-topropertykey
    pub fn to_property_key(self, vm: &Vm) -> ThrowCompletionOr<PropertyKey> {
        // OPTIMIZATION: Return the value as a numeric PropertyKey, if possible.
        if self.is_non_negative_int32() {
            return Ok(PropertyKey::from_number(self.as_i32() as u64));
        }

        // OPTIMIZATION: If this is already a string, we can skip all the ceremony.
        if self.is_string() {
            return Ok(self.as_string().property_key(vm));
        }

        // 1. Let key be ? ToPrimitive(argument, string).
        let key = self.to_primitive(vm, PreferredType::String)?;

        // 2. If key is a Symbol, then
        if key.is_symbol() {
            // a. Return key.
            return Ok(PropertyKey::from_symbol(key.as_symbol()));
        }

        // OPTIMIZATION: Keep the atomized storage when ToPrimitive produced a string.
        if key.is_string() {
            return Ok(key.as_string().property_key(vm));
        }

        // 3. Return ! ToString(key).
        Ok(PropertyKey::from_utf16_string(&key.to_utf16_string(vm).must()))
    }

    // 7.1.6 ToInt32 ( argument ), https://tc39.es/ecma262/#sec-toint32
    pub fn to_i32(self, vm: &Vm) -> ThrowCompletionOr<i32> {
        if self.is_int32() {
            return Ok(self.as_i32());
        }
        self.to_i32_slow_case(vm)
    }

    fn to_i32_slow_case(self, vm: &Vm) -> ThrowCompletionOr<i32> {
        assert!(!self.is_int32());

        // 1. Let number be ? ToNumber(argument).
        let number = self.to_number(vm)?.as_f64();

        // 2-5.
        Ok(value_conversions::to_i32(number))
    }

    // 7.1.7 ToUint32 ( argument ), https://tc39.es/ecma262/#sec-touint32
    pub fn to_u32(self, vm: &Vm) -> ThrowCompletionOr<u32> {
        Ok(self.to_i32(vm)? as u32)
    }

    // 7.1.8 ToInt16 ( argument ), https://tc39.es/ecma262/#sec-toint16
    pub fn to_i16(self, vm: &Vm) -> ThrowCompletionOr<i16> {
        // 1. Let number be ? ToNumber(argument).
        let number = self.to_number(vm)?.as_f64();

        // 2-5.
        Ok(value_conversions::to_i16(number))
    }

    // 7.1.9 ToUint16 ( argument ), https://tc39.es/ecma262/#sec-touint16
    pub fn to_u16(self, vm: &Vm) -> ThrowCompletionOr<u16> {
        // 1. Let number be ? ToNumber(argument).
        let number = self.to_number(vm)?.as_f64();

        // 2-5.
        Ok(value_conversions::to_u16(number))
    }

    // 7.1.10 ToInt8 ( argument ), https://tc39.es/ecma262/#sec-toint8
    pub fn to_i8(self, vm: &Vm) -> ThrowCompletionOr<i8> {
        // 1. Let number be ? ToNumber(argument).
        let number = self.to_number(vm)?.as_f64();

        // 2-5.
        Ok(value_conversions::to_i8(number))
    }

    // 7.1.11 ToUint8 ( argument ), https://tc39.es/ecma262/#sec-touint8
    pub fn to_u8(self, vm: &Vm) -> ThrowCompletionOr<u8> {
        // OPTIMIZATION: Fast path for the common case of an int32.
        if self.is_int32() {
            return Ok((self.as_i32() & i32::from(u8::MAX)) as u8);
        }

        // 1. Let number be ? ToNumber(argument).
        let number = self.to_number(vm)?.as_f64();

        // 2-5.
        Ok(value_conversions::to_u8(number))
    }

    // 7.1.12 ToUint8Clamp ( argument ), https://tc39.es/ecma262/#sec-touint8clamp
    pub fn to_u8_clamp(self, vm: &Vm) -> ThrowCompletionOr<u8> {
        // 1. Let number be ? ToNumber(argument).
        let number = self.to_number(vm)?.as_f64();

        // 2-9.
        Ok(value_conversions::to_u8_clamp(number))
    }

    // 7.1.5 ToIntegerOrInfinity ( argument ), https://tc39.es/ecma262/#sec-tointegerorinfinity
    pub fn to_integer_or_infinity(self, vm: &Vm) -> ThrowCompletionOr<f64> {
        // 1. Let number be ? ToNumber(argument).
        let number = self.to_number(vm)?;

        // 2-7.
        Ok(value_conversions::to_integer_or_infinity(number.as_f64()))
    }

    // 7.1.20 ToLength ( argument ), https://tc39.es/ecma262/#sec-tolength
    pub fn to_length(self, vm: &Vm) -> ThrowCompletionOr<u64> {
        // 1. Let len be ? ToIntegerOrInfinity(argument).
        let len = self.to_integer_or_infinity(vm)?;

        // 2. If len ≤ 0, return +0𝔽.
        // 3. Return 𝔽(min(len, 2^53 - 1)).
        Ok(value_conversions::to_length(len))
    }

    // 7.1.22 ToIndex ( argument ), https://tc39.es/ecma262/#sec-toindex
    pub fn to_index(self, vm: &Vm) -> ThrowCompletionOr<u64> {
        // 1. If value is undefined, then
        if self.is_undefined() {
            // a. Return 0.
            return Ok(0);
        }

        // 2. Else,
        // a. Let integer be ? ToIntegerOrInfinity(value).
        let number = self.to_number(vm)?.as_f64();

        // b-e.
        value_conversions::to_index(number).or_else(|error| error.throw_completion(vm))
    }

    // 13.5.3 The typeof Operator, https://tc39.es/ecma262/#sec-typeof-operator
    pub fn typeof_(self, vm: &Vm) -> Gc<PrimitiveString> {
        let cached_strings = vm.cached_strings();

        // 9. If val is a Number, return "number".
        if self.is_number() {
            return cached_strings.number;
        }

        match self.tag() {
            // 4. If val is undefined, return "undefined".
            nan_box::UNDEFINED_TAG => cached_strings.undefined,
            // 5. If val is null, return "object".
            nan_box::NULL_TAG => cached_strings.object,
            // 6. If val is a String, return "string".
            nan_box::STRING_TAG => cached_strings.string,
            // 7. If val is a Symbol, return "symbol".
            nan_box::SYMBOL_TAG => cached_strings.symbol,
            // 8. If val is a Boolean, return "boolean".
            nan_box::BOOLEAN_TAG => cached_strings.boolean,
            // 10. If val is a BigInt, return "bigint".
            nan_box::BIGINT_TAG => cached_strings.bigint,
            // 11. Assert: val is an Object.
            nan_box::OBJECT_TAG => {
                // B.3.6.3 Changes to the typeof Operator, https://tc39.es/ecma262/#sec-IsHTMLDDA-internal-slot-typeof
                // 12. If val has an [[IsHTMLDDA]] internal slot, return "undefined".
                if self.as_object().is_htmldda() {
                    return cached_strings.undefined;
                }
                // 13. If val has a [[Call]] internal slot, return "function".
                if self.is_function() {
                    return cached_strings.function;
                }
                // 14. Return "object".
                cached_strings.object
            }
            _ => unreachable!("typeof of a value that is not a language value"),
        }
    }

    pub fn to_utf16_string_without_side_effects(self) -> Utf16String {
        if self.is_double() {
            return Utf16String::from_utf16(&number_to_utf16_string(self.as_f64()));
        }

        match self.tag() {
            nan_box::UNDEFINED_TAG => Utf16String::from_utf8("undefined"),
            nan_box::NULL_TAG => Utf16String::from_utf8("null"),
            nan_box::BOOLEAN_TAG => Utf16String::from_utf8(if self.as_bool() { "true" } else { "false" }),
            nan_box::INT32_TAG => Utf16String::from_utf8(&self.as_i32().to_string()),
            nan_box::STRING_TAG => self.as_string().utf16_string(),
            nan_box::SYMBOL_TAG => self.as_symbol().descriptive_string(),
            nan_box::BIGINT_TAG => self.as_bigint().to_utf16_string(),
            nan_box::OBJECT_TAG => Utf16String::from_utf8(&format!("[object {}]", self.as_object().class().name)),
            nan_box::ACCESSOR_TAG => Utf16String::from_utf8("<accessor>"),
            nan_box::EMPTY_TAG => Utf16String::from_utf8("<empty>"),
            _ => unreachable!("a value with an unknown tag"),
        }
    }

    pub fn to_primitive_string(self, vm: &Vm) -> ThrowCompletionOr<Gc<PrimitiveString>> {
        if self.is_string() {
            return Ok(self.as_string());
        }
        if self.is_non_negative_int32() {
            return Ok(PrimitiveString::create_from_unsigned_integer(
                vm,
                u64::from(self.as_i32() as u32),
            ));
        }
        let string = self.to_utf16_string(vm)?;
        Ok(PrimitiveString::create(vm, string))
    }

    // 7.1.17 ToString ( argument ), https://tc39.es/ecma262/#sec-tostring
    pub fn to_utf16_string(self, vm: &Vm) -> ThrowCompletionOr<Utf16String> {
        if self.is_double() {
            return Ok(Utf16String::from_utf16(&number_to_utf16_string(self.as_f64())));
        }

        match self.tag() {
            // 1. If argument is a String, return argument.
            nan_box::STRING_TAG => Ok(self.as_string().utf16_string()),
            // 2. If argument is a Symbol, throw a TypeError exception.
            nan_box::SYMBOL_TAG => {
                vm.throw_completion(ErrorKind::TypeError, ErrorType::Convert, &[&"symbol", &"string"])
            }
            // 3. If argument is undefined, return "undefined".
            nan_box::UNDEFINED_TAG => Ok(Utf16String::from_utf8("undefined")),
            // 4. If argument is null, return "null".
            nan_box::NULL_TAG => Ok(Utf16String::from_utf8("null")),
            // 5. If argument is true, return "true".
            // 6. If argument is false, return "false".
            nan_box::BOOLEAN_TAG => Ok(Utf16String::from_utf8(if self.as_bool() { "true" } else { "false" })),
            // 7. If argument is a Number, return Number::toString(argument, 10).
            nan_box::INT32_TAG => Ok(Utf16String::from_utf8(&self.as_i32().to_string())),
            // 8. If argument is a BigInt, return BigInt::toString(argument, 10).
            nan_box::BIGINT_TAG => Ok(Utf16String::from_utf8(&big_int_algorithms::to_base(
                self.as_bigint().big_integer(),
                10,
            ))),
            // 9. Assert: argument is an Object.
            nan_box::OBJECT_TAG => {
                // 10. Let primValue be ? ToPrimitive(argument, string).
                let primitive_value = self.to_primitive(vm, PreferredType::String)?;

                // 11. Assert: primValue is not an Object.
                assert!(!primitive_value.is_object());

                // 12. Return ? ToString(primValue).
                primitive_value.to_utf16_string(vm)
            }
            _ => unreachable!("ToString of a value that is not a language value"),
        }
    }

    // 7.3.3 GetV ( V, P ), https://tc39.es/ecma262/#sec-getv
    pub fn get(self, vm: &Vm, property_key: &PropertyKey) -> ThrowCompletionOr<Value> {
        // 1. Let O be ? ToObject(V).
        let object = {
            // OPTIMIZATION: For primitive values, we can skip allocation of temporary object and look
            //               directly into prototype where requested property is located.
            let current_realm = || vm.current_realm().expect("there is a current realm");
            if self.is_string() && *property_key != vm.names.length {
                current_realm().string_prototype(vm)
            } else if self.is_boolean() {
                current_realm().boolean_prototype(vm)
            } else if self.is_number() {
                current_realm().number_prototype(vm)
            } else if self.is_bigint() {
                current_realm().bigint_prototype(vm)
            } else if self.is_symbol() {
                current_realm().symbol_prototype(vm)
            } else {
                self.to_object(vm)?
            }
        };

        // 2. Return ? O.[[Get]](P, V).
        object.internal_get(vm, property_key, self, None, PropertyLookupPhase::OwnProperty)
    }

    pub fn get_with_cache(
        self,
        vm: &Vm,
        property_key: &PropertyKey,
        cache: &PropertyLookupCache,
    ) -> ThrowCompletionOr<Value> {
        if self.is_nullish() {
            return vm.throw_completion(ErrorKind::TypeError, ErrorType::ToObjectNullOrUndefined, &[]);
        }
        get_by_id(
            vm,
            GetByIdMode::Normal,
            || None,
            property_key,
            self,
            self,
            cache,
            CachePropertyAbsence::Yes,
        )
    }

    // 7.3.11 GetMethod ( V, P ), https://tc39.es/ecma262/#sec-getmethod
    pub fn get_method(self, vm: &Vm, property_key: &PropertyKey) -> ThrowCompletionOr<Option<Gc<FunctionObject>>> {
        // 1. Let func be ? GetV(V, P).
        let function = self.get(vm, property_key)?;
        Self::finish_get_method(vm, function)
    }

    // 7.3.11 GetMethod ( V, P ), https://tc39.es/ecma262/#sec-getmethod
    pub fn get_method_with_cache(
        self,
        vm: &Vm,
        property_key: &PropertyKey,
        cache: &PropertyLookupCache,
    ) -> ThrowCompletionOr<Option<Gc<FunctionObject>>> {
        // 1. Let func be ? GetV(V, P).
        let function = self.get_with_cache(vm, property_key, cache)?;
        Self::finish_get_method(vm, function)
    }

    fn finish_get_method(vm: &Vm, function: Value) -> ThrowCompletionOr<Option<Gc<FunctionObject>>> {
        // 2. If func is either undefined or null, return undefined.
        if function.is_nullish() {
            return Ok(None);
        }

        // 3. If IsCallable(func) is false, throw a TypeError exception.
        if !function.is_function() {
            return vm.throw_completion(ErrorKind::TypeError, ErrorType::NotAFunction, &[&function]);
        }

        // 4. Return func.
        Ok(Some(function.as_function()))
    }

    // 7.3.21 Invoke ( V, P [ , argumentsList ] ), https://tc39.es/ecma262/#sec-invoke
    pub fn invoke(self, vm: &Vm, property_key: &PropertyKey, arguments: &[Value]) -> ThrowCompletionOr<Value> {
        // 1. If argumentsList is not present, set argumentsList to a new empty List.

        // 2. Let func be ? GetV(V, P).
        let function = self.get(vm, property_key)?;

        // 3. Return ? Call(func, V, argumentsList).
        call(vm, function, self, arguments)
    }
}

/// Mirrors JS::Value::PreferredType.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreferredType {
    Default,
    String,
    Number,
}

/// Mirrors AK::TriState, which IsLessThan returns for its undefined result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TriState {
    False,
    True,
    Unknown,
}

fn both_number(lhs: Value, rhs: Value) -> bool {
    lhs.is_number() && rhs.is_number()
}

fn both_bigint(lhs: Value, rhs: Value) -> bool {
    lhs.is_bigint() && rhs.is_bigint()
}

// 7.1.14 StringToBigInt ( str ), https://tc39.es/ecma262/#sec-stringtobigint
fn string_to_bigint(vm: &Vm, string: &PrimitiveString) -> Option<Gc<BigInt>> {
    let utf16_string = string.utf16_string();
    let code_units = utf16_string.to_utf16();

    // 1-5.
    let bigint = value_conversions::string_to_bigint(&code_units)?;

    // 6. Return ℤ(mv).
    Some(BigInt::create(vm, bigint))
}

fn create_bigint_value(vm: &Vm, big_integer: SignedBigInteger) -> Value {
    Value::from_bigint(BigInt::create(vm, big_integer))
}

// 13.10 Relational Operators, https://tc39.es/ecma262/#sec-relational-operators
// RelationalExpression : RelationalExpression > ShiftExpression
pub fn greater_than(vm: &Vm, lhs: Value, rhs: Value) -> ThrowCompletionOr<bool> {
    // 1. Let lref be ? Evaluation of RelationalExpression.
    // 2. Let lval be ? GetValue(lref).
    // 3. Let rref be ? Evaluation of ShiftExpression.
    // 4. Let rval be ? GetValue(rref).
    // NOTE: This is handled in the AST or Bytecode interpreter.

    // 5. Let r be ? IsLessThan(rval, lval, false).
    let relation = is_less_than(vm, lhs, rhs, false)?;

    // 6. If r is undefined, return false. Otherwise, return r.
    if relation == TriState::Unknown {
        return Ok(false);
    }
    Ok(relation == TriState::True)
}

// 13.10 Relational Operators, https://tc39.es/ecma262/#sec-relational-operators
// RelationalExpression : RelationalExpression >= ShiftExpression
pub fn greater_than_equals(vm: &Vm, lhs: Value, rhs: Value) -> ThrowCompletionOr<bool> {
    // 1. Let lref be ? Evaluation of RelationalExpression.
    // 2. Let lval be ? GetValue(lref).
    // 3. Let rref be ? Evaluation of ShiftExpression.
    // 4. Let rval be ? GetValue(rref).
    // NOTE: This is handled in the AST or Bytecode interpreter.

    // 5. Let r be ? IsLessThan(lval, rval, true).
    let relation = is_less_than(vm, lhs, rhs, true)?;

    // 6. If r is true or undefined, return false. Otherwise, return true.
    if relation == TriState::Unknown || relation == TriState::True {
        return Ok(false);
    }
    Ok(true)
}

// 13.10 Relational Operators, https://tc39.es/ecma262/#sec-relational-operators
// RelationalExpression : RelationalExpression < ShiftExpression
pub fn less_than(vm: &Vm, lhs: Value, rhs: Value) -> ThrowCompletionOr<bool> {
    // 1. Let lref be ? Evaluation of RelationalExpression.
    // 2. Let lval be ? GetValue(lref).
    // 3. Let rref be ? Evaluation of ShiftExpression.
    // 4. Let rval be ? GetValue(rref).
    // NOTE: This is handled in the AST or Bytecode interpreter.

    // 5. Let r be ? IsLessThan(lval, rval, true).
    let relation = is_less_than(vm, lhs, rhs, true)?;

    // 6. If r is undefined, return false. Otherwise, return r.
    if relation == TriState::Unknown {
        return Ok(false);
    }
    Ok(relation == TriState::True)
}

// 13.10 Relational Operators, https://tc39.es/ecma262/#sec-relational-operators
// RelationalExpression : RelationalExpression <= ShiftExpression
pub fn less_than_equals(vm: &Vm, lhs: Value, rhs: Value) -> ThrowCompletionOr<bool> {
    // 1. Let lref be ? Evaluation of RelationalExpression.
    // 2. Let lval be ? GetValue(lref).
    // 3. Let rref be ? Evaluation of ShiftExpression.
    // 4. Let rval be ? GetValue(rref).
    // NOTE: This is handled in the AST or Bytecode interpreter.

    // 5. Let r be ? IsLessThan(rval, lval, false).
    let relation = is_less_than(vm, lhs, rhs, false)?;

    // 6. If r is true or undefined, return false. Otherwise, return true.
    if relation == TriState::True || relation == TriState::Unknown {
        return Ok(false);
    }
    Ok(true)
}

// 13.12 Binary Bitwise Operators, https://tc39.es/ecma262/#sec-binary-bitwise-operators
// BitwiseANDExpression : BitwiseANDExpression & EqualityExpression
pub fn bitwise_and(vm: &Vm, lhs: Value, rhs: Value) -> ThrowCompletionOr<Value> {
    // 13.15.3 ApplyStringOrNumericBinaryOperator ( lval, opText, rval ), https://tc39.es/ecma262/#sec-applystringornumericbinaryoperator
    // 1-2, 6. N/A.

    // 3. Let lnum be ? ToNumeric(lval).
    let lhs_numeric = lhs.to_numeric(vm)?;

    // 4. Let rnum be ? ToNumeric(rval).
    let rhs_numeric = rhs.to_numeric(vm)?;

    // 7. Let operation be the abstract operation associated with opText and Type(lnum) in the following table:
    // [...]
    // 8. Return operation(lnum, rnum).
    if both_number(lhs_numeric, rhs_numeric) {
        // 6.1.6.1.17 Number::bitwiseAND ( x, y ), https://tc39.es/ecma262/#sec-numeric-types-number-bitwiseAND
        // 1. Return NumberBitwiseOp(&, x, y).
        if !lhs_numeric.is_finite_number() || !rhs_numeric.is_finite_number() {
            return Ok(Value::from_i32(0));
        }
        return Ok(Value::from_i32(lhs_numeric.to_i32(vm)? & rhs_numeric.to_i32(vm)?));
    }
    if both_bigint(lhs_numeric, rhs_numeric) {
        // 6.1.6.2.18 BigInt::bitwiseAND ( x, y ), https://tc39.es/ecma262/#sec-numeric-types-bigint-bitwiseAND
        // 1. Return BigIntBitwiseOp(&, x, y).
        let result = lhs_numeric.as_bigint().big_integer() & rhs_numeric.as_bigint().big_integer();
        return Ok(create_bigint_value(vm, result));
    }

    // 5. If Type(lnum) is different from Type(rnum), throw a TypeError exception.
    vm.throw_completion(
        ErrorKind::TypeError,
        ErrorType::BigIntBadOperatorOtherType,
        &[&"bitwise AND"],
    )
}

// 13.12 Binary Bitwise Operators, https://tc39.es/ecma262/#sec-binary-bitwise-operators
// BitwiseORExpression : BitwiseORExpression | BitwiseXORExpression
pub fn bitwise_or(vm: &Vm, lhs: Value, rhs: Value) -> ThrowCompletionOr<Value> {
    // 13.15.3 ApplyStringOrNumericBinaryOperator ( lval, opText, rval ), https://tc39.es/ecma262/#sec-applystringornumericbinaryoperator
    // 1-2, 6. N/A.

    // 3. Let lnum be ? ToNumeric(lval).
    let lhs_numeric = lhs.to_numeric(vm)?;

    // 4. Let rnum be ? ToNumeric(rval).
    let rhs_numeric = rhs.to_numeric(vm)?;

    // 7. Let operation be the abstract operation associated with opText and Type(lnum) in the following table:
    // [...]
    // 8. Return operation(lnum, rnum).
    if both_number(lhs_numeric, rhs_numeric) {
        // 6.1.6.1.19 Number::bitwiseOR ( x, y ), https://tc39.es/ecma262/#sec-numeric-types-number-bitwiseOR
        // 1. Return NumberBitwiseOp(|, x, y).
        if !lhs_numeric.is_finite_number() && !rhs_numeric.is_finite_number() {
            return Ok(Value::from_i32(0));
        }
        if !lhs_numeric.is_finite_number() {
            return Ok(rhs_numeric);
        }
        if !rhs_numeric.is_finite_number() {
            return Ok(lhs_numeric);
        }
        return Ok(Value::from_i32(lhs_numeric.to_i32(vm)? | rhs_numeric.to_i32(vm)?));
    }
    if both_bigint(lhs_numeric, rhs_numeric) {
        // 6.1.6.2.20 BigInt::bitwiseOR ( x, y )
        // 1. Return BigIntBitwiseOp(|, x, y).
        let result = lhs_numeric.as_bigint().big_integer() | rhs_numeric.as_bigint().big_integer();
        return Ok(create_bigint_value(vm, result));
    }

    // 5. If Type(lnum) is different from Type(rnum), throw a TypeError exception.
    vm.throw_completion(
        ErrorKind::TypeError,
        ErrorType::BigIntBadOperatorOtherType,
        &[&"bitwise OR"],
    )
}

// 13.12 Binary Bitwise Operators, https://tc39.es/ecma262/#sec-binary-bitwise-operators
// BitwiseXORExpression : BitwiseXORExpression ^ BitwiseANDExpression
pub fn bitwise_xor(vm: &Vm, lhs: Value, rhs: Value) -> ThrowCompletionOr<Value> {
    // 13.15.3 ApplyStringOrNumericBinaryOperator ( lval, opText, rval ), https://tc39.es/ecma262/#sec-applystringornumericbinaryoperator
    // 1-2, 6. N/A.

    // 3. Let lnum be ? ToNumeric(lval).
    let lhs_numeric = lhs.to_numeric(vm)?;

    // 4. Let rnum be ? ToNumeric(rval).
    let rhs_numeric = rhs.to_numeric(vm)?;

    // 7. Let operation be the abstract operation associated with opText and Type(lnum) in the following table:
    // [...]
    // 8. Return operation(lnum, rnum).
    if both_number(lhs_numeric, rhs_numeric) {
        // 6.1.6.1.18 Number::bitwiseXOR ( x, y ), https://tc39.es/ecma262/#sec-numeric-types-number-bitwiseXOR
        // 1. Return NumberBitwiseOp(^, x, y).
        if !lhs_numeric.is_finite_number() && !rhs_numeric.is_finite_number() {
            return Ok(Value::from_i32(0));
        }
        if !lhs_numeric.is_finite_number() {
            return Ok(rhs_numeric);
        }
        if !rhs_numeric.is_finite_number() {
            return Ok(lhs_numeric);
        }
        return Ok(Value::from_i32(lhs_numeric.to_i32(vm)? ^ rhs_numeric.to_i32(vm)?));
    }
    if both_bigint(lhs_numeric, rhs_numeric) {
        // 6.1.6.2.19 BigInt::bitwiseXOR ( x, y ), https://tc39.es/ecma262/#sec-numeric-types-bigint-bitwiseXOR
        // 1. Return BigIntBitwiseOp(^, x, y).
        let result = lhs_numeric.as_bigint().big_integer() ^ rhs_numeric.as_bigint().big_integer();
        return Ok(create_bigint_value(vm, result));
    }

    // 5. If Type(lnum) is different from Type(rnum), throw a TypeError exception.
    vm.throw_completion(
        ErrorKind::TypeError,
        ErrorType::BigIntBadOperatorOtherType,
        &[&"bitwise XOR"],
    )
}

// 13.5.6 Bitwise NOT Operator ( ~ ), https://tc39.es/ecma262/#sec-bitwise-not-operator
// UnaryExpression : ~ UnaryExpression
pub fn bitwise_not(vm: &Vm, lhs: Value) -> ThrowCompletionOr<Value> {
    // 1. Let expr be ? Evaluation of UnaryExpression.
    // NOTE: This is handled in the AST or Bytecode interpreter.

    // 2. Let oldValue be ? ToNumeric(? GetValue(expr)).
    let old_value = lhs.to_numeric(vm)?;

    // 3. If oldValue is a Number, then
    if old_value.is_number() {
        // a. Return Number::bitwiseNOT(oldValue).

        // 6.1.6.1.2 Number::bitwiseNOT ( x ), https://tc39.es/ecma262/#sec-numeric-types-number-bitwiseNOT
        // 1. Let oldValue be ! ToInt32(x).
        // 2. Return the result of applying bitwise complement to oldValue. The mathematical value of the result is
        //    exactly representable as a 32-bit two's complement bit string.
        return Ok(Value::from_i32(!old_value.to_i32(vm)?));
    }

    // 4. Else,
    // a. Assert: oldValue is a BigInt.
    assert!(old_value.is_bigint());

    // b. Return BigInt::bitwiseNOT(oldValue).

    // 6.1.6.2.2 BigInt::bitwiseNOT ( x ), https://tc39.es/ecma262/#sec-numeric-types-bigint-bitwiseNOT
    // 1. Return -x - 1ℤ.
    let result = !old_value.as_bigint().big_integer();
    Ok(create_bigint_value(vm, result))
}

// 13.5.4 Unary + Operator, https://tc39.es/ecma262/#sec-unary-plus-operator
// UnaryExpression : + UnaryExpression
pub fn unary_plus(vm: &Vm, lhs: Value) -> ThrowCompletionOr<Value> {
    // 1. Let expr be ? Evaluation of UnaryExpression.
    // NOTE: This is handled in the AST or Bytecode interpreter.

    // 2. Return ? ToNumber(? GetValue(expr)).
    lhs.to_number(vm)
}

// 13.5.5 Unary - Operator, https://tc39.es/ecma262/#sec-unary-minus-operator
// UnaryExpression : - UnaryExpression
pub fn unary_minus(vm: &Vm, lhs: Value) -> ThrowCompletionOr<Value> {
    // 1. Let expr be ? Evaluation of UnaryExpression.
    // NOTE: This is handled in the AST or Bytecode interpreter.

    // 2. Let oldValue be ? ToNumeric(? GetValue(expr)).
    let old_value = lhs.to_numeric(vm)?;

    // 3. If oldValue is a Number, then
    if old_value.is_number() {
        // a. Return Number::unaryMinus(oldValue).

        // 6.1.6.1.1 Number::unaryMinus ( x ), https://tc39.es/ecma262/#sec-numeric-types-number-unaryMinus
        // 1. If x is NaN, return NaN.
        if old_value.is_nan() {
            return Ok(Value::from_f64(f64::NAN));
        }

        // 2. Return the result of negating x; that is, compute a Number with the same magnitude but opposite sign.
        return Ok(Value::from_f64(-old_value.as_f64()));
    }

    // 4. Else,
    // a. Assert: oldValue is a BigInt.
    assert!(old_value.is_bigint());

    // b. Return BigInt::unaryMinus(oldValue).

    // 6.1.6.2.1 BigInt::unaryMinus ( x ), https://tc39.es/ecma262/#sec-numeric-types-bigint-unaryMinus
    // 1. If x is 0ℤ, return 0ℤ.
    if old_value.as_bigint().big_integer().is_zero() {
        return Ok(create_bigint_value(vm, SignedBigInteger::zero()));
    }

    // 2. Return the BigInt value that represents the negation of ℝ(x).
    let big_integer_negated = -old_value.as_bigint().big_integer();
    Ok(create_bigint_value(vm, big_integer_negated))
}

// 13.9.1 The Left Shift Operator ( << ), https://tc39.es/ecma262/#sec-left-shift-operator
// ShiftExpression : ShiftExpression << AdditiveExpression
pub fn left_shift(vm: &Vm, lhs: Value, rhs: Value) -> ThrowCompletionOr<Value> {
    // 13.15.3 ApplyStringOrNumericBinaryOperator ( lval, opText, rval ), https://tc39.es/ecma262/#sec-applystringornumericbinaryoperator
    // 1-2, 6. N/A.

    // 3. Let lnum be ? ToNumeric(lval).
    let lhs_numeric = lhs.to_numeric(vm)?;

    // 4. Let rnum be ? ToNumeric(rval).
    let rhs_numeric = rhs.to_numeric(vm)?;

    // 7. Let operation be the abstract operation associated with opText and Type(lnum) in the following table:
    // [...]
    // 8. Return operation(lnum, rnum).
    if both_number(lhs_numeric, rhs_numeric) {
        // 6.1.6.1.9 Number::leftShift ( x, y ), https://tc39.es/ecma262/#sec-numeric-types-number-leftShift

        // OPTIMIZATION: Handle infinite values according to the results returned by ToInt32/ToUint32.
        if !lhs_numeric.is_finite_number() {
            return Ok(Value::from_i32(0));
        }
        if !rhs_numeric.is_finite_number() {
            return Ok(lhs_numeric);
        }

        // 1. Let lnum be ! ToInt32(x).
        let lhs_i32 = lhs_numeric.to_i32(vm).must();

        // 2. Let rnum be ! ToUint32(y).
        let rhs_u32 = rhs_numeric.to_u32(vm).must();

        // 3. Let shiftCount be ℝ(rnum) modulo 32.
        let shift_count = rhs_u32 % 32;

        // 4. Return the result of left shifting lnum by shiftCount bits. The mathematical value of the result is
        //    exactly representable as a 32-bit two's complement bit string.
        return Ok(Value::from_i32(lhs_i32 << shift_count));
    }
    if both_bigint(lhs_numeric, rhs_numeric) {
        // AD-HOC: Prevent allocating huge amounts of memory.
        // 6.1.6.2.9 BigInt::leftShift ( x, y ), https://tc39.es/ecma262/#sec-numeric-types-bigint-leftShift
        // 1. If y < 0ℤ, then
        //    a. Return the BigInt value that represents ℝ(x) / 2^-y, rounding down to the nearest integer, including for negative numbers.
        // 2. Return the BigInt value that represents ℝ(x) × 2^y.
        let result = big_int_algorithms::left_shift(
            lhs_numeric.as_bigint().big_integer(),
            rhs_numeric.as_bigint().big_integer(),
        );
        return match result {
            Ok(result) => Ok(create_bigint_value(vm, result)),
            Err(error) => error.throw_completion(vm),
        };
    }

    // 5. If Type(lnum) is different from Type(rnum), throw a TypeError exception.
    vm.throw_completion(
        ErrorKind::TypeError,
        ErrorType::BigIntBadOperatorOtherType,
        &[&"left-shift"],
    )
}

// 13.9.2 The Signed Right Shift Operator ( >> ), https://tc39.es/ecma262/#sec-signed-right-shift-operator
// ShiftExpression : ShiftExpression >> AdditiveExpression
pub fn right_shift(vm: &Vm, lhs: Value, rhs: Value) -> ThrowCompletionOr<Value> {
    // 13.15.3 ApplyStringOrNumericBinaryOperator ( lval, opText, rval ), https://tc39.es/ecma262/#sec-applystringornumericbinaryoperator
    // 1-2, 6. N/A.

    // 3. Let lnum be ? ToNumeric(lval).
    let lhs_numeric = lhs.to_numeric(vm)?;

    // 4. Let rnum be ? ToNumeric(rval).
    let rhs_numeric = rhs.to_numeric(vm)?;

    // 7. Let operation be the abstract operation associated with opText and Type(lnum) in the following table:
    // [...]
    // 8. Return operation(lnum, rnum).
    if both_number(lhs_numeric, rhs_numeric) {
        // 6.1.6.1.10 Number::signedRightShift ( x, y ), https://tc39.es/ecma262/#sec-numeric-types-number-signedRightShift

        // OPTIMIZATION: Handle infinite values according to the results returned by ToInt32/ToUint32.
        if !lhs_numeric.is_finite_number() {
            return Ok(Value::from_i32(0));
        }
        if !rhs_numeric.is_finite_number() {
            return Ok(lhs_numeric);
        }

        // 1. Let lnum be ! ToInt32(x).
        let lhs_i32 = lhs_numeric.to_i32(vm).must();

        // 2. Let rnum be ! ToUint32(y).
        let rhs_u32 = rhs_numeric.to_u32(vm).must();

        // 3. Let shiftCount be ℝ(rnum) modulo 32.
        let shift_count = rhs_u32 % 32;

        // 4. Return the result of performing a sign-extending right shift of lnum by shiftCount bits.
        //    The most significant bit is propagated. The mathematical value of the result is exactly representable
        //    as a 32-bit two's complement bit string.
        return Ok(Value::from_i32(lhs_i32 >> shift_count));
    }
    if both_bigint(lhs_numeric, rhs_numeric) {
        // 6.1.6.2.10 BigInt::signedRightShift ( x, y ), https://tc39.es/ecma262/#sec-numeric-types-bigint-signedRightShift
        // 1. Return BigInt::leftShift(x, -y).
        let rhs_negated = -rhs_numeric.as_bigint().big_integer();
        // NOTE: Like the C++ runtime, this passes the original left operand, which converts it with ToNumeric again.
        return left_shift(vm, lhs, create_bigint_value(vm, rhs_negated));
    }

    // 5. If Type(lnum) is different from Type(rnum), throw a TypeError exception.
    vm.throw_completion(
        ErrorKind::TypeError,
        ErrorType::BigIntBadOperatorOtherType,
        &[&"right-shift"],
    )
}

// 13.9.3 The Unsigned Right Shift Operator ( >>> ), https://tc39.es/ecma262/#sec-unsigned-right-shift-operator
// ShiftExpression : ShiftExpression >>> AdditiveExpression
pub fn unsigned_right_shift(vm: &Vm, lhs: Value, rhs: Value) -> ThrowCompletionOr<Value> {
    // 13.15.3 ApplyStringOrNumericBinaryOperator ( lval, opText, rval ), https://tc39.es/ecma262/#sec-applystringornumericbinaryoperator
    // 1-2, 5-6. N/A.

    // 3. Let lnum be ? ToNumeric(lval).
    let lhs_numeric = lhs.to_numeric(vm)?;

    // 4. Let rnum be ? ToNumeric(rval).
    let rhs_numeric = rhs.to_numeric(vm)?;

    // 7. Let operation be the abstract operation associated with opText and Type(lnum) in the following table:
    // [...]
    // 8. Return operation(lnum, rnum).
    if both_number(lhs_numeric, rhs_numeric) {
        // 6.1.6.1.11 Number::unsignedRightShift ( x, y ), https://tc39.es/ecma262/#sec-numeric-types-number-unsignedRightShift

        // OPTIMIZATION: Handle infinite values according to the results returned by ToUint32.
        if !lhs_numeric.is_finite_number() {
            return Ok(Value::from_i32(0));
        }
        if !rhs_numeric.is_finite_number() {
            return Ok(lhs_numeric);
        }

        // 1. Let lnum be ! ToUint32(x).
        let lhs_u32 = lhs_numeric.to_u32(vm).must();

        // 2. Let rnum be ! ToUint32(y).
        let rhs_u32 = rhs_numeric.to_u32(vm).must();

        // 3. Let shiftCount be ℝ(rnum) modulo 32.
        let shift_count = rhs_u32 % 32;

        // 4. Return the result of performing a zero-filling right shift of lnum by shiftCount bits.
        //    Vacated bits are filled with zero. The mathematical value of the result is exactly representable
        //    as a 32-bit unsigned bit string.
        return Ok(Value::from_f64(f64::from(lhs_u32 >> shift_count)));
    }

    // 6. If lnum is a BigInt, then
    // d. If opText is >>>, return ? BigInt::unsignedRightShift(lnum, rnum).

    // 6.1.6.2.11 BigInt::unsignedRightShift ( x, y ), https://tc39.es/ecma262/#sec-numeric-types-bigint-unsignedRightShift
    // 1. Throw a TypeError exception.
    vm.throw_completion(
        ErrorKind::TypeError,
        ErrorType::BigIntBadOperator,
        &[&"unsigned right-shift"],
    )
}

// 13.8.1 The Addition Operator ( + ), https://tc39.es/ecma262/#sec-addition-operator-plus
// AdditiveExpression : AdditiveExpression + MultiplicativeExpression
pub fn add(vm: &Vm, lhs: Value, rhs: Value) -> ThrowCompletionOr<Value> {
    // 13.15.3 ApplyStringOrNumericBinaryOperator ( lval, opText, rval ), https://tc39.es/ecma262/#sec-applystringornumericbinaryoperator

    // 1. If opText is +, then

    // a. Let lprim be ? ToPrimitive(lval).
    let lhs_primitive = lhs.to_primitive(vm, PreferredType::Default)?;

    // b. Let rprim be ? ToPrimitive(rval).
    let rhs_primitive = rhs.to_primitive(vm, PreferredType::Default)?;

    // c. If lprim is a String or rprim is a String, then
    if lhs_primitive.is_string() || rhs_primitive.is_string() {
        // i. Let lstr be ? ToString(lprim).
        let lhs_string = lhs_primitive.to_primitive_string(vm)?;

        // ii. Let rstr be ? ToString(rprim).
        let rhs_string = rhs_primitive.to_primitive_string(vm)?;

        // iii. Return the string-concatenation of lstr and rstr.
        return Ok(Value::from_string(PrimitiveString::create_from_concatenation(
            vm, lhs_string, rhs_string,
        )?));
    }

    // d. Set lval to lprim.
    // e. Set rval to rprim.

    // 2. NOTE: At this point, it must be a numeric operation.

    // 3. Let lnum be ? ToNumeric(lval).
    let lhs_numeric = lhs_primitive.to_numeric(vm)?;

    // 4. Let rnum be ? ToNumeric(rval).
    let rhs_numeric = rhs_primitive.to_numeric(vm)?;

    // 6. N/A.

    // 7. Let operation be the abstract operation associated with opText and Type(lnum) in the following table:
    // [...]
    // 8. Return operation(lnum, rnum).
    if both_number(lhs_numeric, rhs_numeric) {
        // 6.1.6.1.7 Number::add ( x, y ), https://tc39.es/ecma262/#sec-numeric-types-number-add
        let x = lhs_numeric.as_f64();
        let y = rhs_numeric.as_f64();
        return Ok(Value::from_f64(x + y));
    }
    if both_bigint(lhs_numeric, rhs_numeric) {
        // 6.1.6.2.7 BigInt::add ( x, y ), https://tc39.es/ecma262/#sec-numeric-types-bigint-add
        let result = lhs_numeric.as_bigint().big_integer() + rhs_numeric.as_bigint().big_integer();
        return Ok(create_bigint_value(vm, result));
    }

    // 5. If Type(lnum) is different from Type(rnum), throw a TypeError exception.
    vm.throw_completion(
        ErrorKind::TypeError,
        ErrorType::BigIntBadOperatorOtherType,
        &[&"addition"],
    )
}

// 13.8.2 The Subtraction Operator ( - ), https://tc39.es/ecma262/#sec-subtraction-operator-minus
// AdditiveExpression : AdditiveExpression - MultiplicativeExpression
pub fn sub(vm: &Vm, lhs: Value, rhs: Value) -> ThrowCompletionOr<Value> {
    // 13.15.3 ApplyStringOrNumericBinaryOperator ( lval, opText, rval ), https://tc39.es/ecma262/#sec-applystringornumericbinaryoperator
    // 1-2, 6. N/A.

    // 3. Let lnum be ? ToNumeric(lval).
    let lhs_numeric = lhs.to_numeric(vm)?;

    // 4. Let rnum be ? ToNumeric(rval).
    let rhs_numeric = rhs.to_numeric(vm)?;

    // 7. Let operation be the abstract operation associated with opText and Type(lnum) in the following table:
    // [...]
    // 8. Return operation(lnum, rnum).
    if both_number(lhs_numeric, rhs_numeric) {
        // 6.1.6.1.8 Number::subtract ( x, y ), https://tc39.es/ecma262/#sec-numeric-types-number-subtract
        let x = lhs_numeric.as_f64();
        let y = rhs_numeric.as_f64();
        // 1. Return Number::add(x, Number::unaryMinus(y)).
        return Ok(Value::from_f64(x - y));
    }
    if both_bigint(lhs_numeric, rhs_numeric) {
        // 6.1.6.2.8 BigInt::subtract ( x, y ), https://tc39.es/ecma262/#sec-numeric-types-bigint-subtract
        // 1. Return the BigInt value that represents the difference x minus y.
        let result = lhs_numeric.as_bigint().big_integer() - rhs_numeric.as_bigint().big_integer();
        return Ok(create_bigint_value(vm, result));
    }

    // 5. If Type(lnum) is different from Type(rnum), throw a TypeError exception.
    vm.throw_completion(
        ErrorKind::TypeError,
        ErrorType::BigIntBadOperatorOtherType,
        &[&"subtraction"],
    )
}

// 13.7 Multiplicative Operators, https://tc39.es/ecma262/#sec-multiplicative-operators
// MultiplicativeExpression : MultiplicativeExpression MultiplicativeOperator ExponentiationExpression
pub fn mul(vm: &Vm, lhs: Value, rhs: Value) -> ThrowCompletionOr<Value> {
    // 13.15.3 ApplyStringOrNumericBinaryOperator ( lval, opText, rval ), https://tc39.es/ecma262/#sec-applystringornumericbinaryoperator
    // 1-2, 6. N/A.

    // 3. Let lnum be ? ToNumeric(lval).
    let lhs_numeric = lhs.to_numeric(vm)?;

    // 4. Let rnum be ? ToNumeric(rval).
    let rhs_numeric = rhs.to_numeric(vm)?;

    // 7. Let operation be the abstract operation associated with opText and Type(lnum) in the following table:
    // [...]
    // 8. Return operation(lnum, rnum).
    if both_number(lhs_numeric, rhs_numeric) {
        // 6.1.6.1.4 Number::multiply ( x, y ), https://tc39.es/ecma262/#sec-numeric-types-number-multiply
        let x = lhs_numeric.as_f64();
        let y = rhs_numeric.as_f64();
        return Ok(Value::from_f64(x * y));
    }
    if both_bigint(lhs_numeric, rhs_numeric) {
        // 6.1.6.2.4 BigInt::multiply ( x, y ), https://tc39.es/ecma262/#sec-numeric-types-bigint-multiply
        // 1. Return the BigInt value that represents the product of x and y.
        let result = lhs_numeric.as_bigint().big_integer() * rhs_numeric.as_bigint().big_integer();
        return Ok(create_bigint_value(vm, result));
    }

    // 5. If Type(lnum) is different from Type(rnum), throw a TypeError exception.
    vm.throw_completion(
        ErrorKind::TypeError,
        ErrorType::BigIntBadOperatorOtherType,
        &[&"multiplication"],
    )
}

// 13.7 Multiplicative Operators, https://tc39.es/ecma262/#sec-multiplicative-operators
// MultiplicativeExpression : MultiplicativeExpression MultiplicativeOperator ExponentiationExpression
pub fn div(vm: &Vm, lhs: Value, rhs: Value) -> ThrowCompletionOr<Value> {
    // 13.15.3 ApplyStringOrNumericBinaryOperator ( lval, opText, rval ), https://tc39.es/ecma262/#sec-applystringornumericbinaryoperator
    // 1-2, 6. N/A.

    // 3. Let lnum be ? ToNumeric(lval).
    let lhs_numeric = lhs.to_numeric(vm)?;

    // 4. Let rnum be ? ToNumeric(rval).
    let rhs_numeric = rhs.to_numeric(vm)?;

    // 7. Let operation be the abstract operation associated with opText and Type(lnum) in the following table:
    // [...]
    // 8. Return operation(lnum, rnum).
    if both_number(lhs_numeric, rhs_numeric) {
        // 6.1.6.1.5 Number::divide ( x, y ), https://tc39.es/ecma262/#sec-numeric-types-number-divide
        return Ok(Value::from_f64(lhs_numeric.as_f64() / rhs_numeric.as_f64()));
    }
    if both_bigint(lhs_numeric, rhs_numeric) {
        // 6.1.6.2.5 BigInt::divide ( x, y ), https://tc39.es/ecma262/#sec-numeric-types-bigint-divide
        // 1. If y is 0ℤ, throw a RangeError exception.
        if rhs_numeric.as_bigint().big_integer().is_zero() {
            return vm.throw_completion(ErrorKind::RangeError, ErrorType::DivisionByZero, &[]);
        }
        // 2. Let quotient be ℝ(x) / ℝ(y).
        // 3. Return the BigInt value that represents quotient rounded towards 0 to the next integer value.
        let result = lhs_numeric.as_bigint().big_integer() / rhs_numeric.as_bigint().big_integer();
        return Ok(create_bigint_value(vm, result));
    }

    // 5. If Type(lnum) is different from Type(rnum), throw a TypeError exception.
    vm.throw_completion(
        ErrorKind::TypeError,
        ErrorType::BigIntBadOperatorOtherType,
        &[&"division"],
    )
}

// 13.7 Multiplicative Operators, https://tc39.es/ecma262/#sec-multiplicative-operators
// MultiplicativeExpression : MultiplicativeExpression MultiplicativeOperator ExponentiationExpression
pub fn r#mod(vm: &Vm, lhs: Value, rhs: Value) -> ThrowCompletionOr<Value> {
    // 13.15.3 ApplyStringOrNumericBinaryOperator ( lval, opText, rval ), https://tc39.es/ecma262/#sec-applystringornumericbinaryoperator
    // 1-2, 6. N/A.

    // 3. Let lnum be ? ToNumeric(lval).
    let lhs_numeric = lhs.to_numeric(vm)?;

    // 4. Let rnum be ? ToNumeric(rval).
    let rhs_numeric = rhs.to_numeric(vm)?;

    // 7. Let operation be the abstract operation associated with opText and Type(lnum) in the following table:
    // [...]
    // 8. Return operation(lnum, rnum).
    if both_number(lhs_numeric, rhs_numeric) {
        // 6.1.6.1.6 Number::remainder ( n, d ), https://tc39.es/ecma262/#sec-numeric-types-number-remainder
        // The ECMA specification is describing the mathematical definition of modulus
        // implemented by fmod.
        let n = lhs_numeric.as_f64();
        let d = rhs_numeric.as_f64();
        return Ok(Value::from_f64(n % d));
    }
    if both_bigint(lhs_numeric, rhs_numeric) {
        // 6.1.6.2.6 BigInt::remainder ( n, d ), https://tc39.es/ecma262/#sec-numeric-types-bigint-remainder
        // 1. If d is 0ℤ, throw a RangeError exception.
        if rhs_numeric.as_bigint().big_integer().is_zero() {
            return vm.throw_completion(ErrorKind::RangeError, ErrorType::DivisionByZero, &[]);
        }
        // 2. If n is 0ℤ, return 0ℤ.
        // 3. Let quotient be ℝ(n) / ℝ(d).
        // 4. Let q be the BigInt whose sign is the sign of quotient and whose magnitude is floor(abs(quotient)).
        // 5. Return n - (d × q).
        let result = lhs_numeric.as_bigint().big_integer() % rhs_numeric.as_bigint().big_integer();
        return Ok(create_bigint_value(vm, result));
    }

    // 5. If Type(lnum) is different from Type(rnum), throw a TypeError exception.
    vm.throw_completion(
        ErrorKind::TypeError,
        ErrorType::BigIntBadOperatorOtherType,
        &[&"modulo"],
    )
}

// 13.6 Exponentiation Operator, https://tc39.es/ecma262/#sec-exp-operator
// ExponentiationExpression : UpdateExpression ** ExponentiationExpression
pub fn exp(vm: &Vm, lhs: Value, rhs: Value) -> ThrowCompletionOr<Value> {
    // 3. Let lnum be ? ToNumeric(lval).
    let lhs_numeric = lhs.to_numeric(vm)?;

    // 4. Let rnum be ? ToNumeric(rval).
    let rhs_numeric = rhs.to_numeric(vm)?;

    // 7. Let operation be the abstract operation associated with opText and Type(lnum) in the following table:
    // [...]
    // 8. Return operation(lnum, rnum).
    if both_number(lhs_numeric, rhs_numeric) {
        return Ok(Value::from_f64(value_conversions::exp_double(
            lhs_numeric.as_f64(),
            rhs_numeric.as_f64(),
        )));
    }
    if both_bigint(lhs_numeric, rhs_numeric) {
        // 6.1.6.2.3 BigInt::exponentiate ( base, exponent ), https://tc39.es/ecma262/#sec-numeric-types-bigint-exponentiate
        // 1. If exponent < 0ℤ, throw a RangeError exception.
        // AD-HOC: Prevent allocating huge amounts of memory.
        // 2. If base is 0ℤ and exponent is 0ℤ, return 1ℤ.
        // 3. Return the BigInt value that represents ℝ(base) raised to the power ℝ(exponent).
        let result = big_int_algorithms::exponentiate(
            lhs_numeric.as_bigint().big_integer(),
            rhs_numeric.as_bigint().big_integer(),
        );
        return match result {
            Ok(result) => Ok(create_bigint_value(vm, result)),
            Err(error) => error.throw_completion(vm),
        };
    }
    vm.throw_completion(
        ErrorKind::TypeError,
        ErrorType::BigIntBadOperatorOtherType,
        &[&"exponentiation"],
    )
}

pub fn r#in(vm: &Vm, lhs: Value, rhs: Value) -> ThrowCompletionOr<Value> {
    if !rhs.is_object() {
        return vm.throw_completion(ErrorKind::TypeError, ErrorType::InOperatorWithObject, &[]);
    }
    let lhs_property_key = lhs.to_property_key(vm)?;
    Ok(Value::from_bool(rhs.as_object().has_property(vm, &lhs_property_key)?))
}

/// Whether a function is the native function the realm installs as %Function.prototype%[@@hasInstance]. Only
/// NativeFunction gives a function a builtin, so the builtin alone identifies it.
fn is_ordinary_has_instance_builtin(function: Gc<FunctionObject>) -> bool {
    // SAFETY: A Gc points to a live cell, and every function object starts with its FunctionObject.
    let function = unsafe { function.as_non_null().as_ref() };
    function.has_builtin.get() && Builtin::from_u8(function.builtin.get()) == Some(Builtin::OrdinaryHasInstance)
}

/// BoundFunction::bound_target_function() of a function that is a bound function exotic object.
fn bound_target_function(function: Gc<FunctionObject>) -> Option<Gc<FunctionObject>> {
    function
        .downcast::<BoundFunction>()
        .map(|bound_function| bound_function.bound_target_function())
}

// 13.10.2 InstanceofOperator ( V, target ), https://tc39.es/ecma262/#sec-instanceofoperator
pub fn instance_of(vm: &Vm, value: Value, target: Value) -> ThrowCompletionOr<Value> {
    // 1. If target is not an Object, throw a TypeError exception.
    if !target.is_object() {
        return vm.throw_completion(ErrorKind::TypeError, ErrorType::NotAnObject, &[&target]);
    }

    // 2. Let instOfHandler be ? GetMethod(target, @@hasInstance).
    let instance_of_handler = target.get_method_with_cache(
        vm,
        &PropertyKey::from(vm.well_known_symbols().has_instance),
        vm.static_property_lookup_cache(StaticPropertyLookupCacheSite::InstanceOfHasInstance),
    )?;

    // 3. If instOfHandler is not undefined, then
    if let Some(instance_of_handler) = instance_of_handler {
        // OPTIMIZATION: If the handler is the default OrdinaryHasInstance, we can skip doing a generic call.
        if is_ordinary_has_instance_builtin(instance_of_handler) {
            return ordinary_has_instance(vm, value, target);
        }
        // a. Return ToBoolean(? Call(instOfHandler, target, « V »)).
        return Ok(Value::from_bool(
            call_function_object(vm, instance_of_handler, target, &[value])?.to_boolean(),
        ));
    }

    // 4. If IsCallable(target) is false, throw a TypeError exception.
    if !target.is_function() {
        return vm.throw_completion(ErrorKind::TypeError, ErrorType::NotAFunction, &[&target]);
    }

    // 5. Return ? OrdinaryHasInstance(target, V).
    // NOTE: Like the C++ runtime, this passes target as the instance and V as the constructor.
    ordinary_has_instance(vm, target, value)
}

// 7.3.22 OrdinaryHasInstance ( C, O ), https://tc39.es/ecma262/#sec-ordinaryhasinstance
// NOTE: As in the C++ runtime, lhs is O and rhs is C.
pub fn ordinary_has_instance(vm: &Vm, lhs: Value, rhs: Value) -> ThrowCompletionOr<Value> {
    // 1. If IsCallable(C) is false, return false.
    if !rhs.is_function() {
        return Ok(Value::FALSE);
    }

    let rhs_function = rhs.as_function();

    // 2. If C has a [[BoundTargetFunction]] internal slot, then
    if let Some(bound_target) = bound_target_function(rhs_function) {
        // a. Let BC be C.[[BoundTargetFunction]].
        // b. Return ? InstanceofOperator(O, BC).
        return instance_of(vm, lhs, Value::from_object(bound_target));
    }

    // 3. If O is not an Object, return false.
    if !lhs.is_object() {
        return Ok(Value::FALSE);
    }

    let mut lhs_object = lhs.as_object();

    // 4. Let P be ? Get(C, "prototype").
    let rhs_prototype = rhs.get_with_cache(
        vm,
        &vm.names.prototype,
        vm.static_property_lookup_cache(StaticPropertyLookupCacheSite::OrdinaryHasInstancePrototype),
    )?;

    // 5. If P is not an Object, throw a TypeError exception.
    if !rhs_prototype.is_object() {
        return vm.throw_completion(ErrorKind::TypeError, ErrorType::InstanceOfOperatorBadPrototype, &[&rhs]);
    }

    // 6. Repeat,
    loop {
        // a. Set O to ? O.[[GetPrototypeOf]]().
        // b. If O is null, return false.
        let Some(prototype) = lhs_object.internal_get_prototype_of(vm)? else {
            return Ok(Value::FALSE);
        };
        lhs_object = prototype;

        // c. If SameValue(P, O) is true, return true.
        if same_value(rhs_prototype, Value::from_object(lhs_object)) {
            return Ok(Value::TRUE);
        }
    }
}

fn same_type_for_equality(lhs: Value, rhs: Value) -> bool {
    // If the top two bytes are identical then either:
    // both are NaN boxed Values with the same type
    // or they are doubles which happen to have the same top bytes.
    if (lhs.0 & nan_box::TAG_EXTRACTION) == (rhs.0 & nan_box::TAG_EXTRACTION) {
        return true;
    }

    if lhs.is_number() && rhs.is_number() {
        return true;
    }

    // One of the Values is not a number and they do not have the same tag
    false
}

// 7.2.10 SameValue ( x, y ), https://tc39.es/ecma262/#sec-samevalue
pub fn same_value(lhs: Value, rhs: Value) -> bool {
    // 1. If Type(x) is different from Type(y), return false.
    if !same_type_for_equality(lhs, rhs) {
        return false;
    }

    // 2. If x is a Number, then
    if lhs.is_number() {
        // a. Return Number::sameValue(x, y).

        // 6.1.6.1.14 Number::sameValue ( x, y ), https://tc39.es/ecma262/#sec-numeric-types-number-sameValue
        // 1. If x is NaN and y is NaN, return true.
        if lhs.is_nan() && rhs.is_nan() {
            return true;
        }
        // 2. If x is +0𝔽 and y is -0𝔽, return false.
        if lhs.is_positive_zero() && rhs.is_negative_zero() {
            return false;
        }
        // 3. If x is -0𝔽 and y is +0𝔽, return false.
        if lhs.is_negative_zero() && rhs.is_positive_zero() {
            return false;
        }
        // 4. If x is the same Number value as y, return true.
        // 5. Return false.
        return lhs.as_f64() == rhs.as_f64();
    }

    // 3. Return SameValueNonNumber(x, y).
    same_value_non_number(lhs, rhs)
}

// 7.2.11 SameValueZero ( x, y ), https://tc39.es/ecma262/#sec-samevaluezero
pub fn same_value_zero(lhs: Value, rhs: Value) -> bool {
    // 1. If Type(x) is different from Type(y), return false.
    if !same_type_for_equality(lhs, rhs) {
        return false;
    }

    // 2. If x is a Number, then
    if lhs.is_number() {
        // a. Return Number::sameValueZero(x, y).
        if lhs.is_nan() && rhs.is_nan() {
            return true;
        }
        return lhs.as_f64() == rhs.as_f64();
    }

    // 3. Return SameValueNonNumber(x, y).
    same_value_non_number(lhs, rhs)
}

// 7.2.12 SameValueNonNumber ( x, y ), https://tc39.es/ecma262/#sec-samevaluenonnumeric
pub fn same_value_non_number(lhs: Value, rhs: Value) -> bool {
    // 1. Assert: Type(x) is the same as Type(y).
    assert!(same_type_for_equality(lhs, rhs));
    assert!(!lhs.is_number());

    // 2. If x is a BigInt, then
    if lhs.is_bigint() {
        // a. Return BigInt::equal(x, y).

        // 6.1.6.2.13 BigInt::equal ( x, y ), https://tc39.es/ecma262/#sec-numeric-types-bigint-equal
        // 1. If ℝ(x) = ℝ(y), return true; otherwise return false.
        return lhs.as_bigint().big_integer() == rhs.as_bigint().big_integer();
    }

    // 5. If x is a String, then
    if lhs.is_string() {
        // a. If x and y are exactly the same sequence of code units (same length and same code units at corresponding indices), return true; otherwise, return false.
        return *lhs.as_string() == *rhs.as_string();
    }

    // 3. If x is undefined, return true.
    // 4. If x is null, return true.
    // 6. If x is a Boolean, then
    //    a. If x and y are both true or both false, return true; otherwise, return false.
    // 7. If x is a Symbol, then
    //    a. If x and y are both the same Symbol value, return true; otherwise, return false.
    // 8. If x and y are the same Object value, return true. Otherwise, return false.
    // NOTE: All the options above will have the exact same bit representation in Value, so we can directly compare the bits.
    lhs.0 == rhs.0
}

// 7.2.15 IsStrictlyEqual ( x, y ), https://tc39.es/ecma262/#sec-isstrictlyequal
pub fn is_strictly_equal(lhs: Value, rhs: Value) -> bool {
    // 1. If Type(x) is different from Type(y), return false.
    if !same_type_for_equality(lhs, rhs) {
        return false;
    }

    // 2. If x is a Number, then
    if lhs.is_number() {
        // a. Return Number::equal(x, y).

        // 6.1.6.1.13 Number::equal ( x, y ), https://tc39.es/ecma262/#sec-numeric-types-number-equal
        // 1. If x is NaN, return false.
        // 2. If y is NaN, return false.
        if lhs.is_nan() || rhs.is_nan() {
            return false;
        }
        // 3. If x is the same Number value as y, return true.
        // 4. If x is +0𝔽 and y is -0𝔽, return true.
        // 5. If x is -0𝔽 and y is +0𝔽, return true.
        // 6. Return false.
        return lhs.as_f64() == rhs.as_f64();
    }

    // 3. Return SameValueNonNumber(x, y).
    same_value_non_number(lhs, rhs)
}

// 7.2.14 IsLooselyEqual ( x, y ), https://tc39.es/ecma262/#sec-islooselyequal
pub fn is_loosely_equal(vm: &Vm, lhs: Value, rhs: Value) -> ThrowCompletionOr<bool> {
    // 1. If Type(x) is the same as Type(y), then
    if same_type_for_equality(lhs, rhs) {
        // a. Return IsStrictlyEqual(x, y).
        return Ok(is_strictly_equal(lhs, rhs));
    }

    // 2. If x is null and y is undefined, return true.
    // 3. If x is undefined and y is null, return true.
    if lhs.is_nullish() && rhs.is_nullish() {
        return Ok(true);
    }

    // 4. NOTE: This step is replaced in section B.3.6.2.
    // B.3.6.2 Changes to IsLooselyEqual, https://tc39.es/ecma262/#sec-IsHTMLDDA-internal-slot-aec
    // 4. Perform the following steps:
    // a. If Type(x) is Object and x has an [[IsHTMLDDA]] internal slot and y is either null or undefined, return true.
    if lhs.is_object() && rhs.is_nullish() {
        // OPTIMIZATION: We can return early here since non-HTMLDDA objects and nullish values are never equal.
        return Ok(lhs.as_object().is_htmldda());
    }

    // b. If x is either null or undefined and Type(y) is Object and y has an [[IsHTMLDDA]] internal slot, return true.
    if lhs.is_nullish() && rhs.is_object() {
        // OPTIMIZATION: We can return early here since non-HTMLDDA objects and nullish values are never equal.
        return Ok(rhs.as_object().is_htmldda());
    }

    // == End of B.3.6.2 ==

    // 5. If Type(x) is Number and Type(y) is String, return ! IsLooselyEqual(x, ! ToNumber(y)).
    if lhs.is_number() && rhs.is_string() {
        return is_loosely_equal(vm, lhs, rhs.to_number(vm).must());
    }

    // 6. If Type(x) is String and Type(y) is Number, return ! IsLooselyEqual(! ToNumber(x), y).
    if lhs.is_string() && rhs.is_number() {
        return is_loosely_equal(vm, lhs.to_number(vm).must(), rhs);
    }

    // 7. If Type(x) is BigInt and Type(y) is String, then
    if lhs.is_bigint() && rhs.is_string() {
        // a. Let n be StringToBigInt(y).
        let bigint = string_to_bigint(vm, &rhs.as_string());

        // b. If n is undefined, return false.
        let Some(bigint) = bigint else {
            return Ok(false);
        };

        // c. Return ! IsLooselyEqual(x, n).
        return is_loosely_equal(vm, lhs, Value::from_bigint(bigint));
    }

    // 8. If Type(x) is String and Type(y) is BigInt, return ! IsLooselyEqual(y, x).
    if lhs.is_string() && rhs.is_bigint() {
        return is_loosely_equal(vm, rhs, lhs);
    }

    // 9. If Type(x) is Boolean, return ! IsLooselyEqual(! ToNumber(x), y).
    if lhs.is_boolean() {
        return is_loosely_equal(vm, lhs.to_number(vm).must(), rhs);
    }

    // 10. If Type(y) is Boolean, return ! IsLooselyEqual(x, ! ToNumber(y)).
    if rhs.is_boolean() {
        return is_loosely_equal(vm, lhs, rhs.to_number(vm).must());
    }

    // 11. If Type(x) is either String, Number, BigInt, or Symbol and Type(y) is Object, return ! IsLooselyEqual(x, ? ToPrimitive(y)).
    if (lhs.is_string() || lhs.is_number() || lhs.is_bigint() || lhs.is_symbol()) && rhs.is_object() {
        let rhs_primitive = rhs.to_primitive(vm, PreferredType::Default)?;
        return is_loosely_equal(vm, lhs, rhs_primitive);
    }

    // 12. If Type(x) is Object and Type(y) is either String, Number, BigInt, or Symbol, return ! IsLooselyEqual(? ToPrimitive(x), y).
    if lhs.is_object() && (rhs.is_string() || rhs.is_number() || rhs.is_bigint() || rhs.is_symbol()) {
        let lhs_primitive = lhs.to_primitive(vm, PreferredType::Default)?;
        return is_loosely_equal(vm, lhs_primitive, rhs);
    }

    // 13. If Type(x) is BigInt and Type(y) is Number, or if Type(x) is Number and Type(y) is BigInt, then
    if (lhs.is_bigint() && rhs.is_number()) || (lhs.is_number() && rhs.is_bigint()) {
        // a. If x or y are any of NaN, +∞𝔽, or -∞𝔽, return false.
        if lhs.is_nan() || lhs.is_infinity() || rhs.is_nan() || rhs.is_infinity() {
            return Ok(false);
        }

        // b. If ℝ(x) = ℝ(y), return true; otherwise return false.
        if (lhs.is_number() && !lhs.is_integral_number()) || (rhs.is_number() && !rhs.is_integral_number()) {
            return Ok(false);
        }

        assert!(!lhs.is_nan() && !rhs.is_nan());

        let (number_side, bigint_side) = if lhs.is_number() { (lhs, rhs) } else { (rhs, lhs) };

        return Ok(
            big_int_algorithms::compare_to_double(bigint_side.as_bigint().big_integer(), number_side.as_f64())
                == CompareResult::DoubleEqualsBigInt,
        );
    }

    // 14. Return false.
    Ok(false)
}

// 7.2.13 IsLessThan ( x, y, LeftFirst ), https://tc39.es/ecma262/#sec-islessthan
pub fn is_less_than(vm: &Vm, lhs: Value, rhs: Value, left_first: bool) -> ThrowCompletionOr<TriState> {
    let x_primitive;
    let y_primitive;

    // 1. If the LeftFirst flag is true, then
    if left_first {
        // a. Let px be ? ToPrimitive(x, number).
        x_primitive = lhs.to_primitive(vm, PreferredType::Number)?;

        // b. Let py be ? ToPrimitive(y, number).
        y_primitive = rhs.to_primitive(vm, PreferredType::Number)?;
    } else {
        // a. NOTE: The order of evaluation needs to be reversed to preserve left to right evaluation.

        // b. Let py be ? ToPrimitive(y, number).
        y_primitive = lhs.to_primitive(vm, PreferredType::Number)?;

        // c. Let px be ? ToPrimitive(x, number).
        x_primitive = rhs.to_primitive(vm, PreferredType::Number)?;
    }

    // 3. If px is a String and py is a String, then
    if x_primitive.is_string() && y_primitive.is_string() {
        let x_string = x_primitive.as_string();
        let y_string = y_primitive.as_string();

        // a. Let lx be the length of px.
        // b. Let ly be the length of py.
        // c. For each integer i such that 0 ≤ i < min(lx, ly), in ascending order, do
        //     i. Let cx be the integer that is the numeric value of the code unit at index i within px.
        //     ii. Let cy be the integer that is the numeric value of the code unit at index i within py.
        //     iii. If cx < cy, return true.
        //     iv. If cx > cy, return false.
        // d. If lx < ly, return true. Otherwise, return false.
        let is_less = x_string
            .utf16_string_view()
            .is_code_unit_less_than(y_string.utf16_string_view());
        return Ok(if is_less { TriState::True } else { TriState::False });
    }

    // 4. Else,
    // a. If px is a BigInt and py is a String, then
    if x_primitive.is_bigint() && y_primitive.is_string() {
        // i. Let ny be StringToBigInt(py).
        let y_bigint = string_to_bigint(vm, &y_primitive.as_string());

        // ii. If ny is undefined, return undefined.
        let Some(y_bigint) = y_bigint else {
            return Ok(TriState::Unknown);
        };

        // iii. Return BigInt::lessThan(px, ny).
        if x_primitive.as_bigint().big_integer() < y_bigint.big_integer() {
            return Ok(TriState::True);
        }
        return Ok(TriState::False);
    }

    // b. If px is a String and py is a BigInt, then
    if x_primitive.is_string() && y_primitive.is_bigint() {
        // i. Let nx be StringToBigInt(px).
        let x_bigint = string_to_bigint(vm, &x_primitive.as_string());

        // ii. If nx is undefined, return undefined.
        let Some(x_bigint) = x_bigint else {
            return Ok(TriState::Unknown);
        };

        // iii. Return BigInt::lessThan(nx, py).
        if x_bigint.big_integer() < y_primitive.as_bigint().big_integer() {
            return Ok(TriState::True);
        }
        return Ok(TriState::False);
    }

    // c. NOTE: Because px and py are primitive values, evaluation order is not important.

    // d. Let nx be ? ToNumeric(px).
    let x_numeric = x_primitive.to_numeric(vm)?;

    // e. Let ny be ? ToNumeric(py).
    let y_numeric = y_primitive.to_numeric(vm)?;

    // h. If nx or ny is NaN, return undefined.
    if x_numeric.is_nan() || y_numeric.is_nan() {
        return Ok(TriState::Unknown);
    }

    // i. If nx is -∞𝔽 or ny is +∞𝔽, return true.
    if x_numeric.is_positive_infinity() || y_numeric.is_negative_infinity() {
        return Ok(TriState::False);
    }

    // j. If nx is +∞𝔽 or ny is -∞𝔽, return false.
    if x_numeric.is_negative_infinity() || y_numeric.is_positive_infinity() {
        return Ok(TriState::True);
    }

    // f. If Type(nx) is the same as Type(ny), then

    // i. If nx is a Number, then
    if x_numeric.is_number() && y_numeric.is_number() {
        // 1. Return Number::lessThan(nx, ny).
        if x_numeric.as_f64() < y_numeric.as_f64() {
            return Ok(TriState::True);
        }
        return Ok(TriState::False);
    }

    // ii. Else,
    if x_numeric.is_bigint() && y_numeric.is_bigint() {
        // 1. Assert: nx is a BigInt.
        // 2. Return BigInt::lessThan(nx, ny).
        if x_numeric.as_bigint().big_integer() < y_numeric.as_bigint().big_integer() {
            return Ok(TriState::True);
        }
        return Ok(TriState::False);
    }

    // g. Assert: nx is a BigInt and ny is a Number, or nx is a Number and ny is a BigInt.
    assert!((x_numeric.is_number() && y_numeric.is_bigint()) || (x_numeric.is_bigint() && y_numeric.is_number()));

    // k. If ℝ(nx) < ℝ(ny), return true; otherwise return false.
    assert!(!x_numeric.is_nan() && !y_numeric.is_nan());
    let x_lower_than_y = if x_numeric.is_number() {
        big_int_algorithms::compare_to_double(y_numeric.as_bigint().big_integer(), x_numeric.as_f64())
            == CompareResult::DoubleLessThanBigInt
    } else {
        big_int_algorithms::compare_to_double(x_numeric.as_bigint().big_integer(), y_numeric.as_f64())
            == CompareResult::DoubleGreaterThanBigInt
    };
    if x_lower_than_y {
        return Ok(TriState::True);
    }
    Ok(TriState::False)
}

/// Formats a value the way AK formats a C++ JS::Value, without side effects.
impl fmt::Display for Value {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let string = self.to_utf16_string_without_side_effects();
        formatter.write_str(&Utf16View::of_string(&string).to_utf8())
    }
}

#[cfg(all(test, libjs_runtime_tests_with_libgc))]
mod object_model_tests {
    use super::*;
    use crate::runtime::completion::Must;
    use crate::runtime::realm::test_realm::TestRealm;

    fn string(vm: &Vm, string: &str) -> Value {
        Value::from_string(PrimitiveString::create_from_utf8(vm, string))
    }

    #[test]
    fn same_value_compares_numbers_strings_and_cells_like_the_spec() {
        let vm = Vm::create();
        let test_realm = TestRealm::new(&vm);
        let nan = Value::from_f64(f64::NAN);
        let negative_zero = Value::from_f64(-0.0);
        let positive_zero = Value::from_i32(0);
        assert!(same_value(nan, nan) && same_value_zero(nan, nan));
        assert!(!same_value(positive_zero, negative_zero) && same_value_zero(positive_zero, negative_zero));
        assert!(same_value(Value::from_f64(2.0), Value::from_i32(2)));
        let long_text = "a string long enough to be stored on the heap";
        assert!(same_value(string(&vm, long_text), string(&vm, long_text)));
        assert!(!same_value(string(&vm, long_text), string(&vm, "another string")));
        let object = Value::from_object(test_realm.object());
        assert!(same_value(object, object));
        assert!(!same_value(object, Value::from_object(test_realm.object())));
        assert!(!same_value(Value::UNDEFINED, Value::NULL) && !same_value(Value::from_i32(1), Value::TRUE));
    }

    #[test]
    fn primitives_convert_to_booleans_and_numbers() {
        let vm = Vm::create();
        let test_realm = TestRealm::new(&vm);
        let falsy = [
            string(&vm, ""),
            Value::from_i32(0),
            Value::from_f64(-0.0),
            Value::from_f64(f64::NAN),
            Value::UNDEFINED,
            Value::NULL,
            Value::FALSE,
        ];
        assert!(falsy.iter().all(|value| !value.to_boolean()));
        let object = test_realm.object();
        let truthy = [
            string(&vm, "0"),
            Value::from_f64(0.5),
            Value::from_object(object),
            Value::TRUE,
        ];
        assert!(truthy.iter().all(|value| value.to_boolean()));
        object.set_is_htmldda();
        assert!(!Value::from_object(object).to_boolean());
        assert_eq!(Value::from_object(object).typeof_(&vm).to_utf8(), "undefined");

        let number = |value: Value| value.to_number(&vm).must().as_f64();
        assert_eq!(number(string(&vm, "  12  ")), 12.0);
        assert_eq!(number(string(&vm, "0x10")), 16.0);
        assert_eq!(number(string(&vm, "")), 0.0);
        assert!(number(string(&vm, "1 2")).is_nan());
        assert_eq!(number(Value::TRUE), 1.0);
        assert_eq!(number(Value::NULL), 0.0);
        assert!(number(Value::UNDEFINED).is_nan());
        assert_eq!(Value::from_i32(-1).to_u32(&vm).must(), u32::MAX);
        assert_eq!(Value::from_f64(4294967296.5).to_u32(&vm).must(), 0);
        assert_eq!(string(&vm, "-7.9").to_length(&vm).must(), 0);
        assert_eq!(Value::from_f64(1e300).to_length(&vm).must(), 9007199254740991);
        assert_eq!(string(&vm, "x").typeof_(&vm).to_utf8(), "string");
        assert_eq!(Value::NULL.typeof_(&vm).to_utf8(), "object");
        assert_eq!(
            format!(
                "{} {} {} {}",
                Value::from_object(test_realm.object()),
                Value::from_f64(1.5),
                Value::NULL,
                string(&vm, "text")
            ),
            "[object Object] 1.5 null text"
        );
    }
}

/// Replays the outcomes the C++ runtime computes in oracle/value_operators.js, both by calling the operators and by
/// running scripts through the interpreter, whose slow paths call them.
#[cfg(all(test, libjs_runtime_tests_with_libgc))]
mod operator_tests {
    use super::*;
    use crate::bytecode::op;
    use crate::gc::root::MarkedVec;
    use crate::interpreter::runtime_functions::SlowPathControl;
    use crate::interpreter::slow_paths::operators;
    use crate::runtime::abstract_operations::{CanonicalIndexMode, canonical_numeric_index_string};
    use crate::runtime::global_environment::test_global_object::set_up_global_object;
    use crate::runtime::realm::Realm;
    use crate::runtime::realm::test_realm::TestRealm;
    use crate::runtime::symbol;
    use crate::script::Script;

    include!("../../oracle/value_operators_table.rs");

    fn escape_code_units(code_units: impl Iterator<Item = u16>) -> String {
        let mut text = String::new();
        for code_unit in code_units {
            if (0x20..0x7e).contains(&code_unit) && code_unit != u16::from(b'\\') && code_unit != u16::from(b'|') {
                text.push(char::from(code_unit as u8));
            } else {
                text.push_str(&format!("\\u{code_unit:04x}"));
            }
        }
        text
    }

    fn unescape_code_units(text: &str) -> Vec<u16> {
        let mut code_units = Vec::new();
        let mut rest = text;
        while let Some(index) = rest.find("\\u") {
            code_units.extend(rest[..index].encode_utf16());
            let hex = &rest[index + 2..index + 6];
            code_units.push(u16::from_str_radix(hex, 16).expect("an escape has four hex digits"));
            rest = &rest[index + 6..];
        }
        code_units.extend(rest.encode_utf16());
        code_units
    }

    fn describe(value: Value) -> String {
        if value.is_undefined() {
            return "undefined".to_string();
        }
        if value.is_null() {
            return "null".to_string();
        }
        if value.is_boolean() {
            return value.as_bool().to_string();
        }
        if value.is_number() {
            if value.is_negative_zero() {
                return "n:-0".to_string();
            }
            return format!("n:{}", number_to_string(value.as_f64()));
        }
        if value.is_string() {
            return format!(
                "s:{}",
                escape_code_units(value.as_string().utf16_string_view().code_units())
            );
        }
        if value.is_bigint() {
            return format!("b:{}", value.as_bigint().big_integer());
        }
        if value.is_symbol() {
            let description = value.as_symbol().description().cloned().unwrap_or_default();
            return format!(
                "sym:{}",
                escape_code_units(Utf16View::of_string(&description).code_units())
            );
        }
        assert!(value.is_object());
        "obj".to_string()
    }

    /// Throwing an error stops the process until realms have error constructors, with a message that names the
    /// error's constructor and its message. This turns that message into the notation of the table.
    fn describe_error(panic_message: &str) -> String {
        let error = panic_message
            .split_once("creating a ")
            .and_then(|(_, rest)| rest.split_once(" with the message \""))
            .and_then(|(kind, rest)| {
                rest.rsplit_once("\" (pc ")
                    .map(|(message, _)| format!("{kind}: {message}"))
            });
        let Some(error) = error else {
            return format!("!? {panic_message}");
        };
        let error = escape_code_units(error.encode_utf16());
        match ERRORS.iter().position(|expected| *expected == error) {
            Some(index) => format!("!{index}"),
            None => format!("!? {error}"),
        }
    }

    fn outcome(operation: impl FnOnce() -> ThrowCompletionOr<Value>) -> String {
        let previous_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let previous_throws_stop_the_process = crate::runtime::error::set_throws_stop_the_process_for_tests(true);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(operation));
        crate::runtime::error::set_throws_stop_the_process_for_tests(previous_throws_stop_the_process);
        std::panic::set_hook(previous_hook);
        match result {
            Ok(Ok(value)) => describe(value),
            Ok(Err(throw)) => format!("!? threw {}", describe(throw.value())),
            Err(payload) => {
                let message = payload
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| payload.downcast_ref::<&str>().map(|message| (*message).to_string()))
                    .unwrap_or_default();
                describe_error(&message)
            }
        }
    }

    fn operand(vm: &Vm, symbol: Gc<Symbol>, object: Gc<Object>, descriptor: &str) -> Value {
        match descriptor {
            "undefined" => return Value::UNDEFINED,
            "null" => return Value::NULL,
            "true" => return Value::TRUE,
            "false" => return Value::FALSE,
            "obj" => return Value::from_object(object),
            "sym:s" => return Value::from_symbol(symbol),
            _ => {}
        }
        let (kind, text) = descriptor.split_once(':').expect("a descriptor names its type");
        match kind {
            "n" => Value::from_f64(match text {
                "NaN" => f64::NAN,
                "Infinity" => f64::INFINITY,
                "-Infinity" => f64::NEG_INFINITY,
                _ => text.parse().expect("a number descriptor holds a number"),
            }),
            "s" => Value::from_string(PrimitiveString::create(
                vm,
                Utf16String::from_utf16(&unescape_code_units(text)),
            )),
            "b" => Value::from_bigint(BigInt::create(
                vm,
                text.parse().expect("a BigInt descriptor holds a decimal integer"),
            )),
            _ => panic!("unknown operand descriptor {descriptor}"),
        }
    }

    fn binary_operation(vm: &Vm, operator: &str, lhs: Value, rhs: Value) -> ThrowCompletionOr<Value> {
        match operator {
            "+" => add(vm, lhs, rhs),
            "-" => sub(vm, lhs, rhs),
            "*" => mul(vm, lhs, rhs),
            "/" => div(vm, lhs, rhs),
            "%" => r#mod(vm, lhs, rhs),
            "**" => exp(vm, lhs, rhs),
            "&" => bitwise_and(vm, lhs, rhs),
            "|" => bitwise_or(vm, lhs, rhs),
            "^" => bitwise_xor(vm, lhs, rhs),
            "<<" => left_shift(vm, lhs, rhs),
            ">>" => right_shift(vm, lhs, rhs),
            ">>>" => unsigned_right_shift(vm, lhs, rhs),
            "<" => less_than(vm, lhs, rhs).map(Value::from_bool),
            "<=" => less_than_equals(vm, lhs, rhs).map(Value::from_bool),
            ">" => greater_than(vm, lhs, rhs).map(Value::from_bool),
            ">=" => greater_than_equals(vm, lhs, rhs).map(Value::from_bool),
            "==" => is_loosely_equal(vm, lhs, rhs).map(Value::from_bool),
            "!=" => is_loosely_equal(vm, lhs, rhs).map(|equal| Value::from_bool(!equal)),
            "===" => Ok(Value::from_bool(is_strictly_equal(lhs, rhs))),
            "!==" => Ok(Value::from_bool(!is_strictly_equal(lhs, rhs))),
            "in" => r#in(vm, lhs, rhs),
            "instanceof" => instance_of(vm, lhs, rhs),
            _ => panic!("unknown binary operator {operator}"),
        }
    }

    fn unary_operation(vm: &Vm, operation: &str, value: Value) -> ThrowCompletionOr<Value> {
        match operation {
            "-x" => unary_minus(vm, value),
            "+x" => unary_plus(vm, value),
            "~x" => bitwise_not(vm, value),
            "!x" => Ok(Value::from_bool(!value.to_boolean())),
            "typeof x" => Ok(Value::from_string(value.typeof_(vm))),
            "ToNumeric(x), as x++ evaluates to" => value.to_numeric(vm),
            "++x" => {
                let mut values = op::IncrementValues { dst: value };
                let control = operators::increment(vm, 0, &mut values);
                assert!(control == SlowPathControl::continue_at(op::Increment::LENGTH));
                Ok(values.dst)
            }
            "--x" => {
                let mut values = op::DecrementValues { dst: value };
                let control = operators::decrement(vm, 0, &mut values);
                assert!(control == SlowPathControl::continue_at(op::Decrement::LENGTH));
                Ok(values.dst)
            }
            "ToString(x)" => {
                let string = value.to_utf16_string(vm)?;
                let primitive_string = value.to_primitive_string(vm)?;
                assert!(Utf16View::of_string(&string) == primitive_string.utf16_string_view());
                Ok(Value::from_string(PrimitiveString::create(vm, string)))
            }
            "ToPropertyKey(x)" => Ok(value.to_property_key(vm)?.to_value(vm)),
            "ToBigInt(x)" => Ok(Value::from_bigint(value.to_bigint(vm)?)),
            "ToBigInt64(x)" => {
                let result = value.to_bigint_int64(vm)?;
                Ok(Value::from_bigint(BigInt::create(vm, SignedBigInteger::from(result))))
            }
            "ToBigUint64(x)" => {
                let result = value.to_bigint_uint64(vm)?;
                Ok(Value::from_bigint(BigInt::create(vm, SignedBigInteger::from(result))))
            }
            "ToInt8(x)" => Ok(Value::from_i32(i32::from(value.to_i8(vm)?))),
            "ToUint8(x)" => Ok(Value::from_i32(i32::from(value.to_u8(vm)?))),
            "ToUint8Clamp(x)" => Ok(Value::from_i32(i32::from(value.to_u8_clamp(vm)?))),
            "ToInt16(x)" => Ok(Value::from_i32(i32::from(value.to_i16(vm)?))),
            "ToUint16(x)" => Ok(Value::from_i32(i32::from(value.to_u16(vm)?))),
            "ToInt32(x)" => Ok(Value::from_i32(value.to_i32(vm)?)),
            "ToUint32(x)" => Ok(Value::from_f64(f64::from(value.to_u32(vm)?))),
            _ => panic!("unknown unary operation {operation}"),
        }
    }

    fn create_operands<'vm>(vm: &'vm Vm, test_realm: &TestRealm<'_>) -> MarkedVec<'vm, Value> {
        let symbol = Symbol::create(vm, Some(Utf16String::from_utf8("s")), symbol::Kind::Unique);
        let object = test_realm.object();
        let operands = MarkedVec::new(vm);
        for (descriptor, _) in OPERANDS {
            operands.push(operand(vm, symbol, object, descriptor));
        }
        operands
    }

    /// What the operator computes, for an outcome that the interpreter's fast paths compute differently.
    fn operator_outcome(outcome: &str) -> &str {
        outcome
            .split_once('\u{7e}')
            .map_or(outcome, |(operator_outcome, _)| operator_outcome)
    }

    /// What a script that applies the operator computes.
    fn interpreter_outcome(outcome: &str) -> &str {
        outcome
            .split_once('\u{7e}')
            .map_or(outcome, |(_, interpreter_outcome)| interpreter_outcome)
    }

    #[test]
    fn binary_operators_match_the_cpp_runtime() {
        let vm = Vm::create();
        let test_realm = TestRealm::new(&vm);
        let operands = create_operands(&vm, &test_realm);
        assert_eq!(BINARY_OPERATOR_OUTCOMES.len(), OPERANDS.len() * OPERANDS.len());
        let mut mismatches = Vec::new();
        for &(lhs_index, rhs_index, expected_outcomes) in BINARY_OPERATOR_OUTCOMES {
            let (lhs, rhs) = (operands.get(lhs_index).unwrap(), operands.get(rhs_index).unwrap());
            for (operator, expected) in BINARY_OPERATORS.iter().zip(expected_outcomes.split('|')) {
                let expected = operator_outcome(expected);
                let actual = outcome(|| binary_operation(&vm, operator, lhs, rhs));
                if actual != expected {
                    mismatches.push(format!(
                        "{} {operator} {}: expected {expected}, got {actual}",
                        OPERANDS[lhs_index].0, OPERANDS[rhs_index].0
                    ));
                }
            }
        }
        assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
    }

    #[test]
    fn unary_operators_and_conversions_match_the_cpp_runtime() {
        let vm = Vm::create();
        let test_realm = TestRealm::new(&vm);
        let operands = create_operands(&vm, &test_realm);
        let mut mismatches = Vec::new();
        for &(index, expected_outcomes) in UNARY_OPERATION_OUTCOMES {
            let value = operands.get(index).unwrap();
            for (operation, expected) in UNARY_OPERATIONS.iter().zip(expected_outcomes.split('|')) {
                let actual = outcome(|| unary_operation(&vm, operation, value));
                if actual != expected {
                    mismatches.push(format!(
                        "{operation} of {}: expected {expected}, got {actual}",
                        OPERANDS[index].0
                    ));
                }
            }
        }
        assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
    }

    #[test]
    fn is_array_is_regexp_and_in_look_at_objects() {
        let vm = Vm::create();
        let test_realm = TestRealm::new(&vm);
        let array = Value::from_object(test_realm.array(&[Value::from_i32(7)]));
        let object = test_realm.object();
        assert!(array.is_array(&vm).must());
        assert!(!Value::from_object(object).is_array(&vm).must());
        assert!(!Value::from_i32(1).is_array(&vm).must());

        let match_key = PropertyKey::from(vm.well_known_symbols().match_);
        assert!(!Value::from_object(object).is_regexp(&vm).must());
        assert!(!Value::from_string(vm.empty_string()).is_regexp(&vm).must());
        object
            .create_data_property_or_throw(&vm, &match_key, Value::from_i32(1))
            .must();
        assert!(Value::from_object(object).is_regexp(&vm).must());
        let other_object = test_realm.object();
        other_object
            .create_data_property_or_throw(&vm, &match_key, Value::from_string(vm.empty_string()))
            .must();
        assert!(!Value::from_object(other_object).is_regexp(&vm).must());

        let element = |text: &str| Value::from_string(PrimitiveString::create_from_utf8(&vm, text));
        assert_eq!(r#in(&vm, Value::from_i32(0), array).must(), Value::TRUE);
        assert_eq!(r#in(&vm, element("0"), array).must(), Value::TRUE);
        assert_eq!(r#in(&vm, Value::from_f64(-0.0), array).must(), Value::TRUE);
        assert_eq!(r#in(&vm, Value::from_i32(1), array).must(), Value::FALSE);
        assert_eq!(r#in(&vm, element("length"), array).must(), Value::TRUE);
        assert_eq!(
            r#in(
                &vm,
                Value::from_symbol(vm.well_known_symbols().match_),
                Value::from_object(object)
            )
            .must(),
            Value::TRUE
        );
    }

    /// The error an outcome says was thrown, as "Kind: message".
    fn thrown_error(outcome: &str) -> &str {
        if let Some(error) = outcome.strip_prefix("!? ") {
            return error;
        }
        let index: usize = outcome
            .strip_prefix('!')
            .and_then(|index| index.parse().ok())
            .unwrap_or_else(|| panic!("{outcome} is not a thrown error"));
        ERRORS[index]
    }

    #[test]
    fn to_index_rejects_what_is_not_a_valid_index() {
        let vm = Vm::create();
        assert_eq!(Value::UNDEFINED.to_index(&vm).must(), 0);
        assert_eq!(Value::NULL.to_index(&vm).must(), 0);
        assert_eq!(Value::from_f64(-0.5).to_index(&vm).must(), 0);
        assert_eq!(
            Value::from_f64(9007199254740991.0).to_index(&vm).must(),
            9007199254740991
        );
        let seven = Value::from_string(PrimitiveString::create_from_utf8(&vm, " 7.9 "));
        assert_eq!(seven.to_index(&vm).must(), 7);
        for value in [
            Value::from_i32(-1),
            Value::from_f64(9007199254740992.0),
            Value::from_f64(f64::INFINITY),
        ] {
            let thrown = outcome(|| value.to_index(&vm).map(|_| Value::UNDEFINED));
            assert_eq!(
                thrown_error(&thrown),
                "RangeError: Index must be a positive integer no greater than 2^53-1"
            );
        }
    }

    #[test]
    fn helpers_box_their_results() {
        let vm = Vm::create();
        assert_eq!(operators::helper_to_boolean(Value::from_f64(f64::NAN).0), 0);
        assert_eq!(operators::helper_to_boolean(Value::from_f64(-0.5).0), 1);
        assert_eq!(operators::helper_to_boolean(Value::from_string(vm.empty_string()).0), 0);
        assert_eq!(operators::helper_math_exp(Value::from_i32(0).0), Value::from_i32(1).0);
        assert_eq!(
            Value(operators::helper_math_exp(Value::from_f64(1.0).0)).as_f64(),
            1f64.exp()
        );
        assert!(Value(operators::helper_empty_string(&vm)) == Value::from_string(vm.empty_string()));
        // The helpers keep only the low bits of the character they are given, as the C++ casts do.
        let a = Value(operators::helper_single_ascii_character_string(&vm, 0x161));
        assert!(a == Value::from_string(vm.single_ascii_character_string(b'a')));
        let ascii = Value(operators::helper_single_utf16_code_unit_string(&vm, 0x41));
        assert!(ascii == Value::from_string(vm.single_ascii_character_string(b'A')));
        let surrogate = Value(operators::helper_single_utf16_code_unit_string(&vm, 0x1_d800));
        assert_eq!(describe(surrogate), "s:\\ud800");
    }

    #[test]
    fn instanceof_needs_an_object_and_ordinary_has_instance_a_callable_constructor() {
        let vm = Vm::create();
        let test_realm = TestRealm::new(&vm);
        let object = Value::from_object(test_realm.object());
        let not_callable = outcome(|| instance_of(&vm, object, object));
        assert_eq!(
            thrown_error(&not_callable),
            "TypeError: [object Object] is not a function"
        );
        let not_an_object = outcome(|| instance_of(&vm, object, Value::from_i32(1)));
        assert_eq!(thrown_error(&not_an_object), "TypeError: 1 is not an object");
        assert_eq!(ordinary_has_instance(&vm, object, object).must(), Value::FALSE);
        assert_eq!(
            ordinary_has_instance(&vm, Value::from_i32(1), object).must(),
            Value::FALSE
        );
    }

    #[test]
    fn canonical_numeric_index_string_matches_the_cpp_runtime() {
        for &(key, expected) in CANONICAL_NUMERIC_INDEX_STRING {
            let index = canonical_numeric_index_string(
                &PropertyKey::from_utf8(key),
                CanonicalIndexMode::DetectNumericRoundtrip,
            );
            let actual = if index.is_index() {
                "index"
            } else if index.is_undefined() {
                "undefined"
            } else {
                "numeric"
            };
            assert_eq!(actual, expected, "CanonicalNumericIndexString({key:?})");
            let ignoring_roundtrip = canonical_numeric_index_string(
                &PropertyKey::from_utf8(key),
                CanonicalIndexMode::IgnoreNumericRoundtrip,
            );
            assert_eq!(ignoring_roundtrip.is_index(), index.is_index());
            assert_eq!(ignoring_roundtrip.is_undefined(), !index.is_index());
        }
    }

    fn describe_script_outcome(vm: &Vm, realm: Gc<Realm>, source: &str) -> String {
        let code_units: Vec<u16> = source.encode_utf16().collect();
        let script = Script::parse(vm, &code_units, realm).ok().expect("the script parses");
        match vm.run_script(script, None) {
            Ok(value) => describe(value),
            Err(throw) => format!("!? threw {}", describe(throw.value())),
        }
    }

    fn comparison_as_condition(outcome: &str) -> String {
        match outcome {
            "true" => "s:yes".to_string(),
            "false" => "s:no".to_string(),
            _ => unreachable!("a comparison that does not throw is a boolean"),
        }
    }

    /// The scripts that run each operation through the interpreter, with the outcome they must have. Operations that
    /// throw are left out, since the error constructors they need do not exist yet.
    fn scripts_with_expected_outcomes() -> Vec<(String, String)> {
        let mut scripts = Vec::new();
        for &(lhs_index, rhs_index, expected_outcomes) in BINARY_OPERATOR_OUTCOMES {
            let (Some(lhs), Some(rhs)) = (OPERANDS[lhs_index].1, OPERANDS[rhs_index].1) else {
                continue;
            };
            for (operator, expected) in BINARY_OPERATORS.iter().zip(expected_outcomes.split('|')) {
                let expected = interpreter_outcome(expected);
                if expected.starts_with('!') {
                    continue;
                }
                let prefix = format!("{{ let a = {lhs}; let b = {rhs}; ");
                scripts.push((format!("{prefix}a {operator} b }}"), expected.to_string()));
                if matches!(*operator, "<" | "<=" | ">" | ">=" | "==" | "!=" | "===" | "!==") {
                    scripts.push((
                        format!("{prefix}a {operator} b ? \"yes\" : \"no\" }}"),
                        comparison_as_condition(expected),
                    ));
                }
            }
        }
        for &(index, expected_outcomes) in UNARY_OPERATION_OUTCOMES {
            let Some(source) = OPERANDS[index].1 else {
                continue;
            };
            let expected: Vec<&str> = expected_outcomes.split('|').collect();
            let expected_for = |operation: &str| {
                let position = UNARY_OPERATIONS.iter().position(|name| *name == operation).unwrap();
                expected[position]
            };
            let prefix = format!("{{ let a = {source}; ");
            for (operation, script) in [
                ("-x", "-a"),
                ("+x", "+a"),
                ("~x", "~a"),
                ("typeof x", "typeof a"),
                ("ToNumeric(x), as x++ evaluates to", "a++"),
                ("ToNumeric(x), as x++ evaluates to", "a--"),
                ("++x", "++a"),
                ("--x", "--a"),
                ("++x", "a++; a"),
                ("--x", "a--; a"),
                ("ToString(x)", "`${a}`"),
                ("ToInt32(x)", "a | 0"),
                ("ToInt32(x)", "a >> 0"),
            ] {
                let expected = expected_for(operation);
                if !expected.starts_with('!') {
                    scripts.push((format!("{prefix}{script} }}"), expected.to_string()));
                }
            }
            let truthiness = if expected_for("!x") == "false" { "s:yes" } else { "s:no" };
            scripts.push((format!("{prefix}a ? \"yes\" : \"no\" }}"), truthiness.to_string()));
            if let Some(string) = expected_for("ToString(x)").strip_prefix("s:") {
                scripts.push((format!("{prefix}`<${{a}}>` }}"), format!("s:<{string}>")));
            }
        }
        scripts
    }

    #[test]
    fn operators_run_through_the_interpreter_like_the_cpp_runtime() {
        let vm = Vm::create();
        let test_realm = TestRealm::new(&vm);
        set_up_global_object(&vm, test_realm.realm);
        let scripts = scripts_with_expected_outcomes();
        assert!(scripts.len() > 10_000);
        let mismatches: Vec<String> = scripts
            .iter()
            .filter_map(|(source, expected)| {
                let actual = describe_script_outcome(&vm, test_realm.realm, source);
                (actual != *expected).then(|| format!("{source}: expected {expected}, got {actual}"))
            })
            .collect();
        assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
    }

    #[test]
    fn operators_run_through_the_interpreter_while_collecting_on_every_allocation() {
        let vm = Vm::create();
        vm.heap().set_should_collect_on_every_allocation(true);
        let test_realm = TestRealm::new(&vm);
        set_up_global_object(&vm, test_realm.realm);
        for (source, expected) in scripts_with_expected_outcomes().iter().step_by(97) {
            assert_eq!(
                describe_script_outcome(&vm, test_realm.realm, source),
                *expected,
                "{source}"
            );
        }
        let operands = create_operands(&vm, &test_realm);
        for &(lhs_index, rhs_index, expected_outcomes) in BINARY_OPERATOR_OUTCOMES.iter().step_by(13) {
            let (lhs, rhs) = (operands.get(lhs_index).unwrap(), operands.get(rhs_index).unwrap());
            for (operator, expected) in BINARY_OPERATORS.iter().zip(expected_outcomes.split('|')) {
                let expected = operator_outcome(expected);
                if !expected.starts_with('!') {
                    assert_eq!(
                        outcome(|| binary_operation(&vm, operator, lhs, rhs)),
                        expected,
                        "{} {operator} {}",
                        OPERANDS[lhs_index].0,
                        OPERANDS[rhs_index].0
                    );
                }
            }
        }
    }
}

impl core::fmt::Debug for Value {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(formatter, "Value(0x{:016x})", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_follow_the_cpp_encoding() {
        assert!(Value::from_f64(2.0).is_int32());
        assert_eq!(Value::from_f64(2.0).as_i32(), 2);
        assert!(Value::from_f64(-0.0).is_double());
        assert_eq!(Value::from_f64(f64::NAN).0, nan_box::CANON_NAN_BITS);
        assert!(Value::from_f64(0.5).is_double());
        assert!(Value::from_f64(f64::from(i32::MAX) + 1.0).is_double());
        assert!(Value::UNDEFINED.is_nullish() && Value::NULL.is_nullish() && !Value::FALSE.is_nullish());
        assert!(!Value::EMPTY.is_number() && Value::from_i32(-1).is_number());
    }

    #[test]
    fn integral_numbers_are_finite_and_whole() {
        assert!(Value::from_i32(-7).is_integral_number());
        assert!(Value::from_f64(-0.0).is_integral_number());
        assert!(Value::from_f64(1e300).is_integral_number());
        assert!(!Value::from_f64(0.5).is_integral_number());
        assert!(!Value::from_f64(f64::INFINITY).is_integral_number());
        assert!(!Value::from_f64(f64::NAN).is_integral_number());
        assert!(!Value::UNDEFINED.is_integral_number() && !Value::UNDEFINED.is_finite_number());
    }
}

/// The text and UTF-16 builders Number::toString writes its ASCII output into.
pub trait NumberStringBuilder {
    fn append_ascii(&mut self, text: &[u8]);
    fn append_repeated_ascii(&mut self, code_unit: u8, count: usize);
}

impl NumberStringBuilder for String {
    fn append_ascii(&mut self, text: &[u8]) {
        self.extend(text.iter().copied().map(char::from));
    }

    fn append_repeated_ascii(&mut self, code_unit: u8, count: usize) {
        self.extend(core::iter::repeat_n(char::from(code_unit), count));
    }
}

impl NumberStringBuilder for Vec<u16> {
    fn append_ascii(&mut self, text: &[u8]) {
        self.extend(text.iter().copied().map(u16::from));
    }

    fn append_repeated_ascii(&mut self, code_unit: u8, count: usize) {
        self.extend(core::iter::repeat_n(u16::from(code_unit), count));
    }
}

pub fn number_to_string(value: f64) -> String {
    let mut builder = String::new();
    append_number_to_string(&mut builder, value);
    builder
}

pub fn number_to_utf16_string(value: f64) -> Vec<u16> {
    let mut builder = Vec::new();
    append_number_to_string(&mut builder, value);
    builder
}

// 6.1.6.1.20 Number::toString ( x ), https://tc39.es/ecma262/#sec-numeric-types-number-tostring
// Implementation for radix = 10
pub fn append_number_to_string(builder: &mut impl NumberStringBuilder, value: f64) {
    // 1. If x is NaN, return "NaN".
    if value.is_nan() {
        builder.append_ascii(b"NaN");
        return;
    }

    // 2. If x is +0𝔽 or -0𝔽, return "0".
    if value == 0.0 {
        builder.append_ascii(b"0");
        return;
    }

    // 4. If x is +∞𝔽, return "Infinity".
    if value.is_infinite() {
        builder.append_ascii(if value > 0.0 { b"Infinity" } else { b"-Infinity" });
        return;
    }

    // 5. Let n, k, and s be integers such that k ≥ 1, radix ^ (k - 1) ≤ s < radix ^ k, 𝔽(s × radix ^ (n - k)) is x,
    //    and k is as small as possible.
    let DecimalExponentialForm {
        is_negative,
        significand,
        exponent,
    } = convert_to_decimal_exponential_form(value);
    let significand_digits = DecimalDigits::new(significand);
    let digits = significand_digits.as_bytes();
    let k = digits.len() as i32;
    let n = exponent + k;

    // 3. If x < -0𝔽, return the string-concatenation of "-" and Number::toString(-x, radix).
    if is_negative {
        builder.append_ascii(b"-");
    }

    // 6. If radix ≠ 10 or n is in the inclusive interval from -5 to 21, then
    if (-5..=21).contains(&n) {
        if n >= k {
            // a. If n ≥ k, return the k digits of s followed by n - k zeros.
            builder.append_ascii(digits);
            builder.append_repeated_ascii(b'0', (n - k) as usize);
        } else if n > 0 {
            // b. Else if n > 0, return the most significant n digits of s, ".", and the remaining k - n digits.
            builder.append_ascii(&digits[..n as usize]);
            builder.append_ascii(b".");
            builder.append_ascii(&digits[n as usize..]);
        } else {
            // c. Else, return "0.", -n zeros, and the k digits of s.
            builder.append_ascii(b"0.");
            builder.append_repeated_ascii(b'0', n.unsigned_abs() as usize);
            builder.append_ascii(digits);
        }
        return;
    }

    // 7. NOTE: In this case, the input will be represented using scientific E notation, such as 1.2e+3.
    // 9. If n < 0, let exponentSign be "-". 10. Else, let exponentSign be "+".
    let exponent_sign: &[u8] = if n < 0 { b"-" } else { b"+" };
    let exponent_digits = DecimalDigits::new(u64::from((n - 1).unsigned_abs()));

    // 11. If k is 1, return the single digit of s, "e", exponentSign, and the decimal representation of abs(n - 1).
    // 12. Return the most significant digit of s, ".", the remaining k - 1 digits, "e", exponentSign, and abs(n - 1).
    builder.append_ascii(&digits[..1]);
    if k > 1 {
        builder.append_ascii(b".");
        builder.append_ascii(&digits[1..]);
    }
    builder.append_ascii(b"e");
    builder.append_ascii(exponent_sign);
    builder.append_ascii(exponent_digits.as_bytes());
}

/// A finite, non-zero double as (-1)^is_negative × significand × 10^exponent, with the contract of
/// AK::convert_to_decimal_exponential_form (Dragonbox): the significand has as few digits as possible, and of the
/// significands of that length that round-trip, it is the one closest to the double, the even one on a tie.
struct DecimalExponentialForm {
    is_negative: bool,
    significand: u64,
    exponent: i32,
}

fn convert_to_decimal_exponential_form(value: f64) -> DecimalExponentialForm {
    let magnitude = value.abs();
    let mut shortest = AsciiBuffer::new();
    write!(shortest, "{magnitude:e}").expect("the shortest exponential form of a double fits the buffer");
    let (mantissa_text, exponent_text) = shortest
        .as_str()
        .split_once('e')
        .expect("exponential formatting writes an exponent");

    let mut significand = 0u64;
    let mut digit_count = 0i32;
    for digit in mantissa_text.bytes().filter(u8::is_ascii_digit) {
        significand = significand * 10 + u64::from(digit - b'0');
        digit_count += 1;
    }
    let exponent_of_first_digit: i32 = exponent_text
        .parse()
        .expect("exponential formatting writes a decimal exponent");
    let exponent = exponent_of_first_digit - (digit_count - 1);

    DecimalExponentialForm {
        is_negative: value.is_sign_negative(),
        significand: prefer_even_significand_on_exact_tie(magnitude, significand, exponent),
        exponent,
    }
}

/// Rust's shortest formatting finds the same significand as Dragonbox, except when the double lies exactly halfway
/// between two shortest candidates: Rust then rounds up, where Dragonbox picks the even candidate.
fn prefer_even_significand_on_exact_tie(magnitude: f64, significand: u64, exponent: i32) -> u64 {
    if significand.is_multiple_of(2) {
        return significand;
    }
    [significand - 1, significand + 1]
        .into_iter()
        .find(|&neighbour| {
            neighbour != 0
                && is_exactly_halfway_between(magnitude, significand + neighbour, exponent)
                && decimal_rounds_to(neighbour, exponent, magnitude)
        })
        .unwrap_or(significand)
}

/// Whether magnitude is exactly (sum_of_significands / 2) × 10^exponent, for an odd sum_of_significands.
fn is_exactly_halfway_between(magnitude: f64, sum_of_significands: u64, exponent: i32) -> bool {
    // 2 × magnitude is odd_binary_significand × 2^(1 + trailing_zeros + binary_exponent), and the other side is
    // sum_of_significands × 5^exponent × 2^exponent. Once a negative exponent's 5^-exponent is moved across, both are
    // an odd number times a power of two, so they are equal exactly when the powers of two and the odd numbers are.
    let bits = magnitude.to_bits();
    let biased_exponent = ((bits >> 52) & 0x7ff) as i32;
    let fraction = bits & ((1 << 52) - 1);
    let (binary_significand, binary_exponent) = if biased_exponent == 0 {
        (fraction, -1074)
    } else {
        (fraction | (1 << 52), biased_exponent - 1075)
    };

    let trailing_zeros = binary_significand.trailing_zeros();
    let odd_binary_significand = u128::from(binary_significand >> trailing_zeros);
    if 1 + trailing_zeros as i32 + binary_exponent != exponent {
        return false;
    }

    let power_of_five = 5u128.checked_pow(exponent.unsigned_abs());
    let sum_of_significands = u128::from(sum_of_significands);
    if exponent >= 0 {
        power_of_five.and_then(|power| power.checked_mul(sum_of_significands)) == Some(odd_binary_significand)
    } else {
        power_of_five.and_then(|power| power.checked_mul(odd_binary_significand)) == Some(sum_of_significands)
    }
}

fn decimal_rounds_to(significand: u64, exponent: i32, magnitude: f64) -> bool {
    let mut decimal = AsciiBuffer::new();
    write!(decimal, "{significand}e{exponent}").expect("a decimal significand and exponent fit the buffer");
    decimal.as_str().parse::<f64>() == Ok(magnitude)
}

struct AsciiBuffer {
    bytes: [u8; 32],
    length: usize,
}

impl AsciiBuffer {
    fn new() -> Self {
        Self {
            bytes: [0; 32],
            length: 0,
        }
    }

    fn as_str(&self) -> &str {
        core::str::from_utf8(&self.bytes[..self.length]).expect("only formatted ASCII is written")
    }
}

impl Write for AsciiBuffer {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        let end = self.length + text.len();
        self.bytes
            .get_mut(self.length..end)
            .ok_or(fmt::Error)?
            .copy_from_slice(text.as_bytes());
        self.length = end;
        Ok(())
    }
}

struct DecimalDigits {
    digits: [u8; 20],
    start: usize,
}

impl DecimalDigits {
    fn new(mut value: u64) -> Self {
        let mut digits = [0; 20];
        let mut start = digits.len();
        loop {
            start -= 1;
            digits[start] = b'0' + (value % 10) as u8;
            value /= 10;
            if value == 0 {
                break;
            }
        }
        Self { digits, start }
    }

    fn as_bytes(&self) -> &[u8] {
        &self.digits[self.start..]
    }
}

#[cfg(test)]
mod number_to_string_tests {
    use super::*;

    #[test]
    fn number_to_string_matches_the_cpp_runtime() {
        for &(bits, expected) in NUMBER_TO_STRING_CASES {
            let value = f64::from_bits(bits);
            assert_eq!(
                number_to_string(value),
                expected,
                "Number::toString({value:e}), bits {bits:016x}"
            );
            assert_eq!(
                number_to_utf16_string(value),
                expected.encode_utf16().collect::<Vec<_>>(),
                "Number::toString({value:e}) as UTF-16, bits {bits:016x}"
            );
        }
    }

    /// Doubles with their String() in the C++ runtime, as bit patterns so that each case is exactly one double. They
    /// include doubles exactly halfway between two shortest candidates, such as 1125899906842624.25.
    const NUMBER_TO_STRING_CASES: &[(u64, &str)] = &[
        (0x0000000000000000, "0"),
        (0x8000000000000000, "0"),
        (0x7ff8000000000000, "NaN"),
        (0x7ff0000000000000, "Infinity"),
        (0xfff0000000000000, "-Infinity"),
        (0x3ff0000000000000, "1"),
        (0xbff0000000000000, "-1"),
        (0x4000000000000000, "2"),
        (0x4024000000000000, "10"),
        (0x4059000000000000, "100"),
        (0x405ec00000000000, "123"),
        (0xc05ec00000000000, "-123"),
        (0x3fb999999999999a, "0.1"),
        (0x3fc999999999999a, "0.2"),
        (0x3fd3333333333333, "0.3"),
        (0x3fd3333333333334, "0.30000000000000004"),
        (0x3fd5555555555555, "0.3333333333333333"),
        (0x3fe5555555555555, "0.6666666666666666"),
        (0xbfd5555555555555, "-0.3333333333333333"),
        (0x3fe0000000000000, "0.5"),
        (0x3fd0000000000000, "0.25"),
        (0x3fc0000000000000, "0.125"),
        (0x3ff8000000000000, "1.5"),
        (0x444b1ae4d6e2ef50, "1e+21"),
        (0x4415af1d78b58c40, "100000000000000000000"),
        (0x3eb0c6f7a0b5ed8d, "0.000001"),
        (0x3e7ad7f29abcaf48, "1e-7"),
        (0x3e8421f5f40d8376, "1.5e-7"),
        (0x3c36b082c2148b8e, "1.23e-18"),
        (0x4340000000000000, "9007199254740992"),
        (0x4340000000000001, "9007199254740994"),
        (0x433fffffffffffff, "9007199254740991"),
        (0xc340000000000000, "-9007199254740992"),
        (0x43f0000000000000, "18446744073709552000"),
        (0x43e0000000000000, "9223372036854776000"),
        (0x41e0000000000000, "2147483648"),
        (0x41f0000000000000, "4294967296"),
        (0x41dfffffffc00000, "2147483647"),
        (0x41efffffffe00000, "4294967295"),
        (0x7fefffffffffffff, "1.7976931348623157e+308"),
        (0xffefffffffffffff, "-1.7976931348623157e+308"),
        (0x0000000000000001, "5e-324"),
        (0x8000000000000001, "-5e-324"),
        (0x0000000000000002, "1e-323"),
        (0x0000000000000003, "1.5e-323"),
        (0x0010000000000000, "2.2250738585072014e-308"),
        (0x000fffffffffffff, "2.225073858507201e-308"),
        (0x3cb0000000000000, "2.220446049250313e-16"),
        (0xc33fffffffffffff, "-9007199254740991"),
        (0x400921fb54442d18, "3.141592653589793"),
        (0x4005bf0a8b145769, "2.718281828459045"),
        (0x3ff6a09e667f3bcd, "1.4142135623730951"),
        (0x3fe62e42fefa39ef, "0.6931471805599453"),
        (0x441ac53a7e04bcda, "123456789012345680000"),
        (0x4484ea15b273b38a, "1.2345678901234568e+22"),
        (0x3ee9e0fcaf9380fc, "0.00001234"),
        (0x3eb4b3fd5942cd96, "0.000001234"),
        (0x3e808ffde1023e12, "1.234e-7"),
        (0x4310000000000001, "1125899906842624.2"),
        (0x4310000000000003, "1125899906842624.8"),
        (0x44b52d02c7e14af6, "1e+23"),
        (0x4480f0cf064dd592, "1e+22"),
        (0x3ea0c6f7a0b5ed8d, "5e-7"),
        (0x4011666666666666, "4.35"),
        (0x3fb1eb851eb851ec, "0.07"),
        (0x444b1ae4d6e2ef4f, "999999999999999900000"),
        (0x7e37e43c8800759c, "1e+300"),
        (0x01a56e1fc2f8f359, "1e-300"),
        (0x3e8091f1667f0595, "1.2345678901234566e-7"),
        (0x4450bb448ec2f608, "1.2345678901234568e+21"),
        (0x441ac53a7e04bcd9, "123456789012345670000"),
        (0x3eb4b66dc01ec6fb, "0.0000012345678901234567"),
        (0xbe7ad7f29abcaf48, "-1e-7"),
        (0xc44b1ae4d6e2ef50, "-1e+21"),
        (0x41f0000000080000, "4294967296.5"),
        (0x405edd2f1a9fbe77, "123.456"),
        (0xbeb92a737110e454, "-0.0000015"),
        (0x3eff75104d551d69, "0.00003"),
        (0x41b1de784a000000, "299792458"),
        (0x44dfe185ca57c517, "6.02214076e+23"),
        (0x390b860bde023111, "6.62607015e-34"),
        (0x0090000000000000, "5.696189077778436e-306"),
        (0x0450000000000000, "6.567258882077402e-288"),
        (0x0810000000000000, "7.571533991467358e-270"),
        (0x0bd0000000000000, "8.729384361624432e-252"),
        (0x0f90000000000000, "1.0064294952495521e-233"),
        (0x1350000000000000, "1.1603342079438231e-215"),
        (0x1710000000000000, "1.3377742608693866e-197"),
        (0x1ad0000000000000, "1.542348713665846e-179"),
        (0x1e90000000000000, "1.778206999588062e-161"),
        (0x2250000000000000, "2.0501330894674953e-143"),
        (0x2610000000000000, "2.3636425261531484e-125"),
        (0x29d0000000000000, "2.7250942976052165e-107"),
        (0x2d90000000000000, "3.141819817790545e-89"),
        (0x3150000000000000, "3.622271631530685e-71"),
        (0x3510000000000000, "4.176194859519056e-53"),
        (0x38d0000000000000, "4.81482486096809e-35"),
        (0x3c90000000000000, "5.551115123125783e-17"),
        (0x4050000000000000, "64"),
        (0x4410000000000000, "73786976294838210000"),
        (0x47d0000000000000, "8.507059173023462e+37"),
        (0x4b90000000000000, "9.807971461541689e+55"),
        (0x4f50000000000000, "1.130782121458166e+74"),
        (0x5310000000000000, "1.3037030248540711e+92"),
        (0x56d0000000000000, "1.5030672529752533e+110"),
        (0x5a90000000000000, "1.7329185588255093e+128"),
        (0x5e50000000000000, "1.997919072202235e+146"),
        (0x6210000000000000, "2.3034438628061165e+164"),
        (0x65d0000000000000, "2.6556899640838355e+182"),
        (0x6990000000000000, "3.061802069160839e+200"),
        (0x6d50000000000000, "3.530017448385272e+218"),
        (0x7110000000000000, "4.0698330278807704e+236"),
        (0x74d0000000000000, "4.692198018002938e+254"),
        (0x7890000000000000, "5.409735998829212e+272"),
        (0x7c50000000000000, "6.237000967296e+290"),
        (0x7fc0000000000000, "2.247116418577895e+307"),
        (0x7fd0000000000000, "4.49423283715579e+307"),
        (0x7fe0000000000000, "8.98846567431158e+307"),
        (0x0000000000000004, "2e-323"),
        (0x0636b0a8e891fffd, "9.999999999999996e-279"),
        (0x0636b0a8e891fffe, "9.999999999999998e-279"),
        (0x0636b0a8e891ffff, "1e-278"),
        (0x0636b0a8e8920000, "1.0000000000000001e-278"),
        (0x0636b0a8e8920001, "1.0000000000000003e-278"),
        (0x0f8fcbaa82a1611f, "9.999999999999997e-234"),
        (0x0f8fcbaa82a16120, "9.999999999999998e-234"),
        (0x0f8fcbaa82a16121, "1e-233"),
        (0x0f8fcbaa82a16122, "1.0000000000000001e-233"),
        (0x0f8fcbaa82a16123, "1.0000000000000002e-233"),
        (0x18e6470cff6546b4, "9.999999999999996e-189"),
        (0x18e6470cff6546b5, "9.999999999999998e-189"),
        (0x18e6470cff6546b6, "1e-188"),
        (0x18e6470cff6546b7, "1.0000000000000001e-188"),
        (0x18e6470cff6546b8, "1.0000000000000003e-188"),
        (0x223f37ad21436d0a, "9.999999999999997e-144"),
        (0x223f37ad21436d0b, "9.999999999999998e-144"),
        (0x223f37ad21436d0c, "1e-143"),
        (0x223f37ad21436d0d, "1.0000000000000001e-143"),
        (0x223f37ad21436d0e, "1.0000000000000002e-143"),
        (0x2b95df5ca28ef40b, "9.999999999999996e-99"),
        (0x2b95df5ca28ef40c, "9.999999999999998e-99"),
        (0x2b95df5ca28ef40d, "1e-98"),
        (0x2b95df5ca28ef40e, "1.0000000000000001e-98"),
        (0x2b95df5ca28ef40f, "1.0000000000000003e-98"),
        (0x34eea6608e29b24b, "9.999999999999998e-54"),
        (0x34eea6608e29b24c, "9.999999999999999e-54"),
        (0x34eea6608e29b24d, "1e-53"),
        (0x34eea6608e29b24e, "1.0000000000000001e-53"),
        (0x34eea6608e29b24f, "1.0000000000000003e-53"),
        (0x3e45798ee2308c38, "9.999999999999997e-9"),
        (0x3e45798ee2308c39, "9.999999999999999e-9"),
        (0x3e45798ee2308c3a, "1e-8"),
        (0x3e45798ee2308c3b, "1.0000000000000002e-8"),
        (0x3e45798ee2308c3c, "1.0000000000000004e-8"),
        (0x479e17b843576919, "9.999999999999997e+36"),
        (0x479e17b84357691a, "9.999999999999998e+36"),
        (0x479e17b84357691b, "1e+37"),
        (0x479e17b84357691c, "1.0000000000000001e+37"),
        (0x479e17b84357691d, "1.0000000000000002e+37"),
        (0x50f5159af8044460, "9.999999999999996e+81"),
        (0x50f5159af8044461, "9.999999999999998e+81"),
        (0x50f5159af8044462, "1e+82"),
        (0x50f5159af8044463, "1.0000000000000001e+82"),
        (0x50f5159af8044464, "1.0000000000000003e+82"),
        (0x5a4d8ba7f519c84d, "9.999999999999997e+126"),
        (0x5a4d8ba7f519c84e, "9.999999999999998e+126"),
        (0x5a4d8ba7f519c84f, "1e+127"),
        (0x5a4d8ba7f519c850, "1.0000000000000001e+127"),
        (0x5a4d8ba7f519c851, "1.0000000000000002e+127"),
        (0x63a4b378469b6730, "9.999999999999997e+171"),
        (0x63a4b378469b6731, "9.999999999999999e+171"),
        (0x63a4b378469b6732, "1e+172"),
        (0x63a4b378469b6733, "1.0000000000000003e+172"),
        (0x63a4b378469b6734, "1.0000000000000004e+172"),
        (0x6cfd022390f8b835, "9.999999999999997e+216"),
        (0x6cfd022390f8b836, "9.999999999999998e+216"),
        (0x6cfd022390f8b837, "1e+217"),
        (0x6cfd022390f8b838, "1.0000000000000001e+217"),
        (0x6cfd022390f8b839, "1.0000000000000002e+217"),
        (0x7654531e58a03e8a, "9.999999999999997e+261"),
        (0x7654531e58a03e8b, "9.999999999999998e+261"),
        (0x7654531e58a03e8c, "1e+262"),
        (0x7654531e58a03e8d, "1.0000000000000002e+262"),
        (0x7654531e58a03e8e, "1.0000000000000004e+262"),
        (0x7fac7b1f3cac7431, "9.999999999999997e+306"),
        (0x7fac7b1f3cac7432, "9.999999999999999e+306"),
        (0x7fac7b1f3cac7433, "1e+307"),
        (0x7fac7b1f3cac7434, "1.0000000000000001e+307"),
        (0x7fac7b1f3cac7435, "1.0000000000000002e+307"),
        (0x7fe1ccf385ebc89e, "9.999999999999996e+307"),
        (0x7fe1ccf385ebc89f, "9.999999999999998e+307"),
        (0x7fe1ccf385ebc8a0, "1e+308"),
        (0x7fe1ccf385ebc8a1, "1.0000000000000002e+308"),
        (0x7fe1ccf385ebc8a2, "1.0000000000000004e+308"),
        (0x3eb0c6f7a0b5ed8b, "9.999999999999995e-7"),
        (0x3eb0c6f7a0b5ed8c, "9.999999999999997e-7"),
        (0x3eb0c6f7a0b5ed8e, "0.0000010000000000000002"),
        (0x3eb0c6f7a0b5ed8f, "0.0000010000000000000004"),
        (0x3e7ad7f29abcaf46, "9.999999999999997e-8"),
        (0x3e7ad7f29abcaf47, "9.999999999999998e-8"),
        (0x3e7ad7f29abcaf49, "1.0000000000000001e-7"),
        (0x3e7ad7f29abcaf4a, "1.0000000000000002e-7"),
        (0x4415af1d78b58c3e, "99999999999999970000"),
        (0x4415af1d78b58c3f, "99999999999999980000"),
        (0x4415af1d78b58c41, "100000000000000020000"),
        (0x4415af1d78b58c42, "100000000000000030000"),
        (0x444b1ae4d6e2ef4e, "999999999999999700000"),
        (0x444b1ae4d6e2ef51, "1.0000000000000001e+21"),
        (0x444b1ae4d6e2ef52, "1.0000000000000003e+21"),
        (0x433ffffffffffffe, "9007199254740990"),
        (0x4340000000000002, "9007199254740996"),
        (0x432ffffffffffffe, "4503599627370495"),
        (0x432fffffffffffff, "4503599627370495.5"),
        (0x4330000000000000, "4503599627370496"),
        (0x4330000000000001, "4503599627370497"),
        (0x4330000000000002, "4503599627370498"),
        (0x434ffffffffffffe, "18014398509481980"),
        (0x434fffffffffffff, "18014398509481982"),
        (0x4350000000000000, "18014398509481984"),
        (0x4350000000000001, "18014398509481988"),
        (0x4350000000000002, "18014398509481990"),
        (0xc054e00000000000, "-83.5"),
        (0x3fc53f7ced916873, "0.166"),
        (0x406f400000000000, "250"),
        (0xc074d80000000000, "-333.5"),
        (0x3fda9fbe76c8b439, "0.416"),
        (0x407f400000000000, "500"),
        (0xc0823c0000000000, "-583.5"),
        (0x3fe54fdf3b645a1d, "0.666"),
        (0x4087700000000000, "750"),
        (0xc08a0c0000000000, "-833.5"),
        (0x3fed4fdf3b645a1d, "0.916"),
        (0x408f400000000000, "1000"),
        (0xdc1b77ae0bf34dad, "-4.9911105725155504e+135"),
        (0x2eaa5d555d5d6c79, "6.785660674745272e-84"),
        (0x733c75ed334879c4, "1.24371618519318e+247"),
        (0xe4b9c30f2a68f125, "-1.6311586497843164e+177"),
        (0xb7ac6ca72a6a68b9, "-1.6314820258654558e-40"),
        (0xbfcece69f5e25e3f, "-0.2406742525679295"),
        (0x796d177505842666, "8.057755371405623e+276"),
        (0xeffd8abb474418be, "-2.8665228437957636e+231"),
        (0x5d7089a5f7743cc2, "1.2604127373919135e+142"),
        (0x5b91ef14aeb5d478, "1.2729550572097727e+133"),
        (0xa88f69d5836da456, "-2.551213381306969e-113"),
        (0x947c80c87620c643, "-5.4186924695997535e-210"),
        (0xa190320af3143e02, "-5.066358731024768e-147"),
        (0x9848b143dc6b8901, "-1.0824224925384549e-191"),
        (0x40811f8f77ac5ceb, "547.9450524774414"),
        (0x76af82579d6c3e63, "4.9609277569007504e+263"),
        (0xf7f6ff6897c504e4, "-7.593471064170383e+269"),
        (0xbe960fc3d9827afc, "-3.287431922324738e-7"),
        (0x0ca04237a2cc8afe, "7.26671897031033e-248"),
        (0x3f85702e9c7a9965, "0.010467876577717925"),
        (0xe7be910aad5b800a, "-5.4475842471175054e+191"),
        (0x4d26ab4617261b9d, "4.6627507177823825e+63"),
        (0xd02e1a059a83e642, "-1.742766355921457e+78"),
        (0xef7e2f42a7ae6eaf, "-1.1440955667566245e+229"),
        (0x3a5956b7e9b7eb3b, "1.279279639208626e-27"),
        (0xac80803d6769e5bd, "-2.472065483180713e-94"),
        (0x362d5bfab7f4182d, "1.0044222210166761e-47"),
        (0x39374765d78e5160, "4.483352622176303e-33"),
        (0x2745a557aeae63c0, "1.6765123592528922e-119"),
        (0x5863203573878ca0, "6.028764038861866e+117"),
        (0xd66c85180967af3a, "-2.0931310924971078e+108"),
        (0x75a6eb0478c3e040, "5.5058415467340815e+258"),
        (0x16e0c14e406343aa, "1.7511360846860286e-198"),
        (0xbd2290600317b3e7, "-3.2976226244021695e-14"),
        (0xabdfee1b7fd2d1f8, "-2.335725428778002e-97"),
        (0x18f6fb43869286de, "2.0631988061609328e-188"),
        (0x79e349ad56a97777, "1.3676303143924114e+279"),
        (0xbfe2c6e0a44aa206, "-0.5867770394152678"),
        (0x591c33321e216b64, "1.8204850979289672e+121"),
        (0x18d5035c9499813a, "4.716214055847976e-189"),
        (0x0002d2485b84b1dd, "3.923662683323697e-309"),
        (0x000f74c75b693341, "2.149444564230886e-308"),
        (0x001b14aeb951a326, "3.7660475180651344e-308"),
        (0x001f9fb92684912d, "4.397847206030633e-308"),
        (0x00001420046fdd48, "1.09325590933037e-310"),
        (0x0008dc4a49c2a4e4, "1.2322053712920567e-308"),
        (0x000a2dd43ba2ceff, "1.415566910511916e-308"),
        (0x000c8aef2aa17d39, "1.7442787715464685e-308"),
        (0x00073734bf1d863a, "1.0034594417785896e-308"),
        (0x00134334065b51db, "2.6787820752561945e-308"),
        (0x0000bed5e97da30c, "1.03667795791766e-309"),
        (0x000a7646934339fd, "1.454922170792351e-308"),
        (0x0010e5e258d38d44, "2.3499540470914154e-308"),
        (0x0014dd9a0d24fa75, "2.9017232533103855e-308"),
        (0x4327d342fca93e58, "3353104562298668"),
        (0x431af02e641d397a, "1895607858318942.5"),
        (0x42ff0b1630c9a395, "546119638358585.3"),
        (0x42e3f4d5eed4e7e2, "175538257176383.06"),
        (0x42cdcce8d634ef81, "65531833772511.01"),
        (0x42b9cbb234635bf5, "28362658833243.957"),
        (0x429eef44b0202547, "8503249602569.319"),
        (0x4285775bb0f4a24a, "2950297951892.286"),
        (0x4264a4644b1cef5e, "709259057383.4802"),
        (0x4257936fec2a0b76, "405031334056.1791"),
        (0x42392d9fab5c5ee9, "108139621212.37074"),
        (0x4313c4fc43ab4fa5, "1391153075901417.2"),
        (0x431ca34d57b8b41d, "2015212981857543.2"),
        (0x4312cc54196fbf95, "1322802789216229.2"),
        (0x4314995b0a070d71, "1449528955880284.2"),
        (0x4312b38cff1f0361, "1315991934451928.2"),
        (0x43126d6132315629, "1296703450535306.2"),
        (0x431feb76e1270281, "2246155023532192.2"),
        (0x4313eac06af21b21, "1401534176593608.2"),
        (0x4312a54b4cf5f341, "1312073103277264.2"),
        (0x431b89045d872655, "1937619053300117.2"),
        (0x431a08d6d9208935, "1832017063322189.2"),
        (0x4319ae29d100d021, "1807092260287496.2"),
        (0x4311d20bdf6fddc1, "1254005759801200.2"),
        (0x43144a4f0c0c21c5, "1427800724801649.2"),
        (0x43186f3bcdb1a6c1, "1719425521445296.2"),
        (0x43129f1dabae2aad, "1310374840994475.2"),
        (0x43177916278dc319, "1651765131047110.2"),
        (0x431374b8786870e1, "1369090050104376.2"),
        (0x431defaea4dfe925, "2106576923523657.2"),
        (0x4310a589f4e802f1, "1171402891329724.2"),
        (0x4314b199f4af1bf9, "1456193581860606.2"),
        (0x43117b77eadc24cd, "1230207393925427.2"),
        (0x43152f0ee17d2ab5, "1490678867511981.2"),
        (0x4315281c08250e01, "1488768842941312.2"),
        (0x43041614c6d9cdaa, "706722253191605.2"),
        (0x4308e1b33f2db342, "875444927051368.2"),
        (0x4309f69bf5b9b1b2, "913503015089718.2"),
        (0x430b8a58c8fd5762, "968992288123628.2"),
        (0x4309fd3ef84a2c12, "914415164147074.2"),
        (0x430266669d5d7f22, "647392561704932.2"),
        (0x43043903364e4ad2, "711523186624858.2"),
        (0x4304db01fcda7faa, "733787639730165.2"),
        (0x430d0c14b3a22dda, "1022007172154811.2"),
        (0x43076c1db8b4827a, "824099921629263.2"),
        (0x42e01f9a994254e4, "141823390126759.12"),
        (0x42e48b6d4d7c2ee4, "180712534434167.12"),
        (0x42e569377d404e24, "188333173637745.12"),
        (0x42e6b2158c91b474, "199632972189091.62"),
        (0x42e7d7d14bdaee34, "209725574535025.62"),
        (0x42e661dec7bf9584, "196876842171564.12"),
        (0x42d4e4f5933f6508, "91894420667796.12"),
        (0x42d41587a7108d28, "88330810966580.62"),
        (0x42be570d183e9010, "33359230680720.062"),
        (0x42b6830db9d99ad0, "24752126810522.812"),
        (0x428989506e795240, "3509693828906.2812"),
        (0x3e85b2c8a8a06bce, "1.6166548660951684e-7"),
        (0x4414c7d8fc54cf0f, "95833854680972380000"),
        (0x3e8cbdd5ffc77376, "2.1414120955536544e-7"),
        (0x441b501f6ffe61ed, "125958886593956360000"),
        (0x3e8bf84f3701100b, "2.083924317571433e-7"),
        (0x441a4c59d336bbc0, "121279251642072430000"),
        (0x3e85d4ebf2dda025, "1.6265902885314037e-7"),
        (0x441bd76fd1af5cdd, "128396486745458690000"),
        (0x3e8eea0988a79aa4, "2.3032879810300927e-7"),
        (0x4410df7bb56f709a, "77812892390545330000"),
        (0x3e854b2f3c840fa8, "1.5865034997547052e-7"),
        (0x44194868ee2b0497, "116596570969893880000"),
        (0x3e8a7d28249d5ac9, "1.9735763806079843e-7"),
        (0x4416e94bc6f4fcfd, "105659779602804850000"),
        (0x3e89b1eae27791b5, "1.914425962055453e-7"),
        (0x4412acc54faf846b, "86122709421741750000"),
        (0x3e8f4d7c6e08cc6f, "2.3322313948276081e-7"),
        (0x441fea45cfcc6cf4, "147182548385043120000"),
        (0x3e8414c1cf59fefc, "1.4961572218987618e-7"),
        (0x441f34d0b1dfce5a, "143913700886230500000"),
    ];
}

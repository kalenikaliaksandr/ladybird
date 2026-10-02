/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

use core::cell::Cell;

use ak::Utf16FlyString;
use libjs_runtime_macros::Trace;

use crate::gc::class::{GcCell, define_cell};
use crate::interpreter::runtime_functions::unimplemented_runtime_function;
use crate::interpreter::vm::Vm;
use crate::layout::cell::{CellHeader, Gc};
use crate::layout::value::Value;
use crate::runtime::accessor::Accessor;
use crate::runtime::aggregate_error_constructor::AggregateErrorConstructor;
use crate::runtime::aggregate_error_prototype::AggregateErrorPrototype;
use crate::runtime::array_buffer_constructor::ArrayBufferConstructor;
use crate::runtime::array_buffer_prototype::ArrayBufferPrototype;
use crate::runtime::array_constructor::ArrayConstructor;
use crate::runtime::array_iterator_prototype::ArrayIteratorPrototype;
use crate::runtime::array_prototype::ArrayPrototype;
use crate::runtime::async_from_sync_iterator_prototype::AsyncFromSyncIteratorPrototype;
use crate::runtime::async_function_constructor::AsyncFunctionConstructor;
use crate::runtime::async_function_prototype::AsyncFunctionPrototype;
use crate::runtime::async_generator_function_constructor::AsyncGeneratorFunctionConstructor;
use crate::runtime::async_generator_function_prototype::AsyncGeneratorFunctionPrototype;
use crate::runtime::async_generator_prototype::AsyncGeneratorPrototype;
use crate::runtime::async_iterator_prototype::AsyncIteratorPrototype;
use crate::runtime::atomics_object::AtomicsObject;
use crate::runtime::big_int_constructor::BigIntConstructor;
use crate::runtime::big_int_prototype::BigIntPrototype;
use crate::runtime::boolean_constructor::BooleanConstructor;
use crate::runtime::boolean_prototype::BooleanPrototype;
use crate::runtime::completion::Must;
use crate::runtime::console_object::ConsoleObject;
use crate::runtime::data_view_constructor::DataViewConstructor;
use crate::runtime::data_view_prototype::DataViewPrototype;
use crate::runtime::error::ErrorKind;
use crate::runtime::error_constructor::{
    ErrorConstructor, EvalErrorConstructor, InternalErrorConstructor, RangeErrorConstructor, ReferenceErrorConstructor,
    SyntaxErrorConstructor, TypeErrorConstructor, URIErrorConstructor,
};
use crate::runtime::error_prototype::{
    ErrorPrototype, EvalErrorPrototype, InternalErrorPrototype, RangeErrorPrototype, ReferenceErrorPrototype,
    SyntaxErrorPrototype, TypeErrorPrototype, URIErrorPrototype,
};
use crate::runtime::error_types::ErrorType;
use crate::runtime::finalization_registry_constructor::FinalizationRegistryConstructor;
use crate::runtime::finalization_registry_prototype::FinalizationRegistryPrototype;
use crate::runtime::function_constructor::FunctionConstructor;
use crate::runtime::function_object::FunctionObject;
use crate::runtime::function_prototype::FunctionPrototype;
use crate::runtime::generator_function_constructor::GeneratorFunctionConstructor;
use crate::runtime::generator_function_prototype::GeneratorFunctionPrototype;
use crate::runtime::generator_prototype::GeneratorPrototype;
use crate::runtime::global_object::GlobalObject;
use crate::runtime::iterator_constructor::IteratorConstructor;
use crate::runtime::iterator_helper_prototype::IteratorHelperPrototype;
use crate::runtime::iterator_prototype::IteratorPrototype;
use crate::runtime::json_object::JSONObject;
use crate::runtime::map_constructor::MapConstructor;
use crate::runtime::map_iterator_prototype::MapIteratorPrototype;
use crate::runtime::map_prototype::MapPrototype;
use crate::runtime::math_object::MathObject;
use crate::runtime::native_function::{NativeFunction, RawNativeFunction, raw_native};
use crate::runtime::number_constructor::NumberConstructor;
use crate::runtime::number_prototype::NumberPrototype;
use crate::runtime::object::{Object, allocate_object};
use crate::runtime::object_constructor::ObjectConstructor;
use crate::runtime::object_prototype::ObjectPrototype;
use crate::runtime::primitive_string::PrimitiveString;
use crate::runtime::promise_constructor::PromiseConstructor;
use crate::runtime::promise_prototype::PromisePrototype;
use crate::runtime::property_attributes::{Attribute, PropertyAttributes};
use crate::runtime::property_key::PropertyKey;
use crate::runtime::proxy_constructor::ProxyConstructor;
use crate::runtime::realm::Realm;
use crate::runtime::reflect_object::ReflectObject;
use crate::runtime::regexp_constructor::RegExpConstructor;
use crate::runtime::regexp_prototype::RegExpPrototype;
use crate::runtime::regexp_string_iterator_prototype::RegExpStringIteratorPrototype;
use crate::runtime::set_constructor::SetConstructor;
use crate::runtime::set_iterator_prototype::SetIteratorPrototype;
use crate::runtime::set_prototype::SetPrototype;
use crate::runtime::shape::Shape;
use crate::runtime::shared_array_buffer_constructor::SharedArrayBufferConstructor;
use crate::runtime::shared_array_buffer_prototype::SharedArrayBufferPrototype;
use crate::runtime::string_constructor::StringConstructor;
use crate::runtime::string_iterator_prototype::StringIteratorPrototype;
use crate::runtime::string_prototype::StringPrototype;
use crate::runtime::symbol_constructor::SymbolConstructor;
use crate::runtime::symbol_prototype::SymbolPrototype;
use crate::runtime::typed_array::{
    BigInt64ArrayConstructor, BigInt64ArrayPrototype, BigUint64ArrayConstructor, BigUint64ArrayPrototype,
    Float16ArrayConstructor, Float16ArrayPrototype, Float32ArrayConstructor, Float32ArrayPrototype,
    Float64ArrayConstructor, Float64ArrayPrototype, Int8ArrayConstructor, Int8ArrayPrototype, Int16ArrayConstructor,
    Int16ArrayPrototype, Int32ArrayConstructor, Int32ArrayPrototype, Uint8ArrayConstructor, Uint8ArrayPrototype,
    Uint8ClampedArrayConstructor, Uint8ClampedArrayPrototype, Uint16ArrayConstructor, Uint16ArrayPrototype,
    Uint32ArrayConstructor, Uint32ArrayPrototype,
};
use crate::runtime::typed_array_constructor::TypedArrayConstructor;
use crate::runtime::typed_array_prototype::TypedArrayPrototype;
use crate::runtime::weak_map_constructor::WeakMapConstructor;
use crate::runtime::weak_map_prototype::WeakMapPrototype;
use crate::runtime::weak_ref_constructor::WeakRefConstructor;
use crate::runtime::weak_ref_prototype::WeakRefPrototype;
use crate::runtime::weak_set_constructor::WeakSetConstructor;
use crate::runtime::weak_set_prototype::WeakSetPrototype;
use crate::runtime::wrap_for_valid_iterator_prototype::WrapForValidIteratorPrototype;

/// Declares the intrinsics' slots, which all start out empty.
macro_rules! define_intrinsics {
    ($($slot:ident: $type:ty,)*) => {
        #[repr(C)]
        #[derive(Trace)]
        pub struct Intrinsics {
            header: CellHeader,
            realm: Gc<Realm>,
            $($slot: $type,)*
        }

        impl Intrinsics {
            fn new(realm: Gc<Realm>) -> Self {
                Self {
                    header: CellHeader::for_class(Self::CLASS),
                    realm,
                    $($slot: Default::default(),)*
                }
            }
        }
    };
}

define_intrinsics! {
    empty_object_shape: Cell<Option<Gc<Shape>>>,
    new_object_shape: Cell<Option<Gc<Shape>>>,

    iterator_result_object_shape: Cell<Option<Gc<Shape>>>,
    iterator_result_object_value_offset: Cell<u32>,
    iterator_result_object_done_offset: Cell<u32>,

    normal_function_prototype_shape: Cell<Option<Gc<Shape>>>,
    normal_function_prototype_constructor_offset: Cell<u32>,

    normal_function_shape: Cell<Option<Gc<Shape>>>,
    normal_function_length_offset: Cell<u32>,
    normal_function_name_offset: Cell<u32>,

    async_function_shape: Cell<Option<Gc<Shape>>>,
    generator_function_shape: Cell<Option<Gc<Shape>>>,
    async_generator_function_shape: Cell<Option<Gc<Shape>>>,
    generator_function_prototype_property_offset: Cell<u32>,

    native_function_shape: Cell<Option<Gc<Shape>>>,
    native_function_length_offset: Cell<u32>,
    native_function_name_offset: Cell<u32>,

    unmapped_arguments_object_shape: Cell<Option<Gc<Shape>>>,
    unmapped_arguments_object_length_offset: Cell<u32>,
    unmapped_arguments_object_well_known_symbol_iterator_offset: Cell<u32>,
    unmapped_arguments_object_callee_offset: Cell<u32>,

    mapped_arguments_object_shape: Cell<Option<Gc<Shape>>>,
    mapped_arguments_object_length_offset: Cell<u32>,
    mapped_arguments_object_well_known_symbol_iterator_offset: Cell<u32>,
    mapped_arguments_object_callee_offset: Cell<u32>,

    regexp_builtin_exec_array_shape: Cell<Option<Gc<Shape>>>,
    regexp_builtin_exec_array_index_offset: Cell<u32>,
    regexp_builtin_exec_array_input_offset: Cell<u32>,
    regexp_builtin_exec_array_groups_offset: Cell<u32>,

    throw_type_error_accessor: Cell<Option<Gc<Accessor>>>,

    // Not included in JS_ENUMERATE_NATIVE_OBJECTS due to missing distinct prototype
    proxy_constructor: Cell<Option<Gc<ProxyConstructor>>>,

    // Not included in JS_ENUMERATE_NATIVE_OBJECTS due to missing distinct constructor
    async_from_sync_iterator_prototype: Cell<Option<Gc<Object>>>,
    async_generator_prototype: Cell<Option<Gc<Object>>>,
    generator_prototype: Cell<Option<Gc<Object>>>,
    wrap_for_valid_iterator_prototype: Cell<Option<Gc<Object>>>,

    // Not included in JS_ENUMERATE_INTL_OBJECTS due to missing distinct constructor
    intl_segments_prototype: Cell<Option<Gc<Object>>>,

    // Global object functions
    eval_function: Cell<Option<Gc<FunctionObject>>>,
    is_finite_function: Cell<Option<Gc<FunctionObject>>>,
    is_nan_function: Cell<Option<Gc<FunctionObject>>>,
    parse_float_function: Cell<Option<Gc<FunctionObject>>>,
    parse_int_function: Cell<Option<Gc<FunctionObject>>>,
    decode_uri_function: Cell<Option<Gc<FunctionObject>>>,
    decode_uri_component_function: Cell<Option<Gc<FunctionObject>>>,
    encode_uri_function: Cell<Option<Gc<FunctionObject>>>,
    encode_uri_component_function: Cell<Option<Gc<FunctionObject>>>,
    escape_function: Cell<Option<Gc<FunctionObject>>>,
    unescape_function: Cell<Option<Gc<FunctionObject>>>,

    // Namespace/constructor object functions
    array_prototype_values_function: Cell<Option<Gc<FunctionObject>>>,
    date_constructor_now_function: Cell<Option<Gc<FunctionObject>>>,
    json_parse_function: Cell<Option<Gc<FunctionObject>>>,
    json_stringify_function: Cell<Option<Gc<FunctionObject>>>,
    object_prototype_to_string_function: Cell<Option<Gc<FunctionObject>>>,
    throw_type_error_function: Cell<Option<Gc<FunctionObject>>>,

    // JS_ENUMERATE_BUILTIN_TYPES
    aggregate_error_constructor: Cell<Option<Gc<AggregateErrorConstructor>>>,
    aggregate_error_prototype: Cell<Option<Gc<Object>>>,
    array_constructor: Cell<Option<Gc<ArrayConstructor>>>,
    array_prototype: Cell<Option<Gc<Object>>>,
    array_buffer_constructor: Cell<Option<Gc<FunctionObject>>>,
    array_buffer_prototype: Cell<Option<Gc<Object>>>,
    async_disposable_stack_constructor: Cell<Option<Gc<FunctionObject>>>,
    async_disposable_stack_prototype: Cell<Option<Gc<Object>>>,
    async_function_constructor: Cell<Option<Gc<AsyncFunctionConstructor>>>,
    async_function_prototype: Cell<Option<Gc<Object>>>,
    async_generator_function_constructor: Cell<Option<Gc<AsyncGeneratorFunctionConstructor>>>,
    async_generator_function_prototype: Cell<Option<Gc<Object>>>,
    bigint_constructor: Cell<Option<Gc<BigIntConstructor>>>,
    bigint_prototype: Cell<Option<Gc<Object>>>,
    boolean_constructor: Cell<Option<Gc<BooleanConstructor>>>,
    boolean_prototype: Cell<Option<Gc<Object>>>,
    data_view_constructor: Cell<Option<Gc<FunctionObject>>>,
    data_view_prototype: Cell<Option<Gc<Object>>>,
    date_constructor: Cell<Option<Gc<FunctionObject>>>,
    date_prototype: Cell<Option<Gc<Object>>>,
    disposable_stack_constructor: Cell<Option<Gc<FunctionObject>>>,
    disposable_stack_prototype: Cell<Option<Gc<Object>>>,
    error_constructor: Cell<Option<Gc<ErrorConstructor>>>,
    error_prototype: Cell<Option<Gc<Object>>>,
    finalization_registry_constructor: Cell<Option<Gc<FinalizationRegistryConstructor>>>,
    finalization_registry_prototype: Cell<Option<Gc<Object>>>,
    function_constructor: Cell<Option<Gc<FunctionConstructor>>>,
    function_prototype: Cell<Option<Gc<Object>>>,
    generator_function_constructor: Cell<Option<Gc<GeneratorFunctionConstructor>>>,
    generator_function_prototype: Cell<Option<Gc<Object>>>,
    iterator_constructor: Cell<Option<Gc<IteratorConstructor>>>,
    iterator_prototype: Cell<Option<Gc<Object>>>,
    map_constructor: Cell<Option<Gc<MapConstructor>>>,
    map_prototype: Cell<Option<Gc<Object>>>,
    number_constructor: Cell<Option<Gc<NumberConstructor>>>,
    number_prototype: Cell<Option<Gc<Object>>>,
    object_constructor: Cell<Option<Gc<ObjectConstructor>>>,
    object_prototype: Cell<Option<Gc<Object>>>,
    promise_constructor: Cell<Option<Gc<PromiseConstructor>>>,
    promise_prototype: Cell<Option<Gc<Object>>>,
    regexp_constructor: Cell<Option<Gc<RegExpConstructor>>>,
    regexp_prototype: Cell<Option<Gc<Object>>>,
    set_constructor: Cell<Option<Gc<SetConstructor>>>,
    set_prototype: Cell<Option<Gc<Object>>>,
    shared_array_buffer_constructor: Cell<Option<Gc<FunctionObject>>>,
    shared_array_buffer_prototype: Cell<Option<Gc<Object>>>,
    string_constructor: Cell<Option<Gc<StringConstructor>>>,
    string_prototype: Cell<Option<Gc<Object>>>,
    suppressed_error_constructor: Cell<Option<Gc<FunctionObject>>>,
    suppressed_error_prototype: Cell<Option<Gc<Object>>>,
    symbol_constructor: Cell<Option<Gc<SymbolConstructor>>>,
    symbol_prototype: Cell<Option<Gc<Object>>>,
    weak_map_constructor: Cell<Option<Gc<WeakMapConstructor>>>,
    weak_map_prototype: Cell<Option<Gc<Object>>>,
    weak_ref_constructor: Cell<Option<Gc<WeakRefConstructor>>>,
    weak_ref_prototype: Cell<Option<Gc<Object>>>,
    weak_set_constructor: Cell<Option<Gc<WeakSetConstructor>>>,
    weak_set_prototype: Cell<Option<Gc<Object>>>,
    typed_array_constructor: Cell<Option<Gc<FunctionObject>>>,
    typed_array_prototype: Cell<Option<Gc<Object>>>,
    eval_error_constructor: Cell<Option<Gc<EvalErrorConstructor>>>,
    eval_error_prototype: Cell<Option<Gc<Object>>>,
    internal_error_constructor: Cell<Option<Gc<InternalErrorConstructor>>>,
    internal_error_prototype: Cell<Option<Gc<Object>>>,
    range_error_constructor: Cell<Option<Gc<RangeErrorConstructor>>>,
    range_error_prototype: Cell<Option<Gc<Object>>>,
    reference_error_constructor: Cell<Option<Gc<ReferenceErrorConstructor>>>,
    reference_error_prototype: Cell<Option<Gc<Object>>>,
    syntax_error_constructor: Cell<Option<Gc<SyntaxErrorConstructor>>>,
    syntax_error_prototype: Cell<Option<Gc<Object>>>,
    type_error_constructor: Cell<Option<Gc<TypeErrorConstructor>>>,
    type_error_prototype: Cell<Option<Gc<Object>>>,
    uri_error_constructor: Cell<Option<Gc<URIErrorConstructor>>>,
    uri_error_prototype: Cell<Option<Gc<Object>>>,
    uint8_array_constructor: Cell<Option<Gc<FunctionObject>>>,
    uint8_array_prototype: Cell<Option<Gc<Object>>>,
    uint8_clamped_array_constructor: Cell<Option<Gc<FunctionObject>>>,
    uint8_clamped_array_prototype: Cell<Option<Gc<Object>>>,
    uint16_array_constructor: Cell<Option<Gc<FunctionObject>>>,
    uint16_array_prototype: Cell<Option<Gc<Object>>>,
    uint32_array_constructor: Cell<Option<Gc<FunctionObject>>>,
    uint32_array_prototype: Cell<Option<Gc<Object>>>,
    big_uint64_array_constructor: Cell<Option<Gc<FunctionObject>>>,
    big_uint64_array_prototype: Cell<Option<Gc<Object>>>,
    int8_array_constructor: Cell<Option<Gc<FunctionObject>>>,
    int8_array_prototype: Cell<Option<Gc<Object>>>,
    int16_array_constructor: Cell<Option<Gc<FunctionObject>>>,
    int16_array_prototype: Cell<Option<Gc<Object>>>,
    int32_array_constructor: Cell<Option<Gc<FunctionObject>>>,
    int32_array_prototype: Cell<Option<Gc<Object>>>,
    big_int64_array_constructor: Cell<Option<Gc<FunctionObject>>>,
    big_int64_array_prototype: Cell<Option<Gc<Object>>>,
    float16_array_constructor: Cell<Option<Gc<FunctionObject>>>,
    float16_array_prototype: Cell<Option<Gc<Object>>>,
    float32_array_constructor: Cell<Option<Gc<FunctionObject>>>,
    float32_array_prototype: Cell<Option<Gc<Object>>>,
    float64_array_constructor: Cell<Option<Gc<FunctionObject>>>,
    float64_array_prototype: Cell<Option<Gc<Object>>>,

    // JS_ENUMERATE_INTL_OBJECTS
    intl_collator_constructor: Cell<Option<Gc<FunctionObject>>>,
    intl_collator_prototype: Cell<Option<Gc<Object>>>,
    intl_date_time_format_constructor: Cell<Option<Gc<FunctionObject>>>,
    intl_date_time_format_prototype: Cell<Option<Gc<Object>>>,
    intl_display_names_constructor: Cell<Option<Gc<FunctionObject>>>,
    intl_display_names_prototype: Cell<Option<Gc<Object>>>,
    intl_duration_format_constructor: Cell<Option<Gc<FunctionObject>>>,
    intl_duration_format_prototype: Cell<Option<Gc<Object>>>,
    intl_list_format_constructor: Cell<Option<Gc<FunctionObject>>>,
    intl_list_format_prototype: Cell<Option<Gc<Object>>>,
    intl_locale_constructor: Cell<Option<Gc<FunctionObject>>>,
    intl_locale_prototype: Cell<Option<Gc<Object>>>,
    intl_number_format_constructor: Cell<Option<Gc<FunctionObject>>>,
    intl_number_format_prototype: Cell<Option<Gc<Object>>>,
    intl_plural_rules_constructor: Cell<Option<Gc<FunctionObject>>>,
    intl_plural_rules_prototype: Cell<Option<Gc<Object>>>,
    intl_relative_time_format_constructor: Cell<Option<Gc<FunctionObject>>>,
    intl_relative_time_format_prototype: Cell<Option<Gc<Object>>>,
    intl_segmenter_constructor: Cell<Option<Gc<FunctionObject>>>,
    intl_segmenter_prototype: Cell<Option<Gc<Object>>>,

    // JS_ENUMERATE_TEMPORAL_OBJECTS
    temporal_duration_constructor: Cell<Option<Gc<FunctionObject>>>,
    temporal_duration_prototype: Cell<Option<Gc<Object>>>,
    temporal_instant_constructor: Cell<Option<Gc<FunctionObject>>>,
    temporal_instant_prototype: Cell<Option<Gc<Object>>>,
    temporal_plain_date_constructor: Cell<Option<Gc<FunctionObject>>>,
    temporal_plain_date_prototype: Cell<Option<Gc<Object>>>,
    temporal_plain_date_time_constructor: Cell<Option<Gc<FunctionObject>>>,
    temporal_plain_date_time_prototype: Cell<Option<Gc<Object>>>,
    temporal_plain_month_day_constructor: Cell<Option<Gc<FunctionObject>>>,
    temporal_plain_month_day_prototype: Cell<Option<Gc<Object>>>,
    temporal_plain_time_constructor: Cell<Option<Gc<FunctionObject>>>,
    temporal_plain_time_prototype: Cell<Option<Gc<Object>>>,
    temporal_plain_year_month_constructor: Cell<Option<Gc<FunctionObject>>>,
    temporal_plain_year_month_prototype: Cell<Option<Gc<Object>>>,
    temporal_zoned_date_time_constructor: Cell<Option<Gc<FunctionObject>>>,
    temporal_zoned_date_time_prototype: Cell<Option<Gc<Object>>>,

    // JS_ENUMERATE_BUILTIN_NAMESPACE_OBJECTS
    atomics_object: Cell<Option<Gc<Object>>>,
    console_object: Cell<Option<Gc<ConsoleObject>>>,
    intl_object: Cell<Option<Gc<Object>>>,
    json_object: Cell<Option<Gc<Object>>>,
    math_object: Cell<Option<Gc<Object>>>,
    reflect_object: Cell<Option<Gc<Object>>>,
    temporal_object: Cell<Option<Gc<Object>>>,

    // JS_ENUMERATE_ITERATOR_PROTOTYPES
    array_iterator_prototype: Cell<Option<Gc<Object>>>,
    async_iterator_prototype: Cell<Option<Gc<Object>>>,
    intl_segment_iterator_prototype: Cell<Option<Gc<Object>>>,
    iterator_helper_prototype: Cell<Option<Gc<Object>>>,
    map_iterator_prototype: Cell<Option<Gc<Object>>>,
    regexp_string_iterator_prototype: Cell<Option<Gc<Object>>>,
    set_iterator_prototype: Cell<Option<Gc<Object>>>,
    string_iterator_prototype: Cell<Option<Gc<Object>>>,

    // JS_ENUMERATE_NATIVE_JAVASCRIPT_BACKED_ABSTRACT_OPERATIONS
    async_iterator_close_abstract_operation_function: Cell<Option<Gc<FunctionObject>>>,
    get_method_abstract_operation_function: Cell<Option<Gc<FunctionObject>>>,
    get_iterator_direct_abstract_operation_function: Cell<Option<Gc<FunctionObject>>>,
    get_iterator_from_method_abstract_operation_function: Cell<Option<Gc<FunctionObject>>>,
    iterator_complete_abstract_operation_function: Cell<Option<Gc<FunctionObject>>>,

    // JS_ENUMERATE_NATIVE_JAVASCRIPT_BACKED_ARRAY_CONSTRUCTOR_FUNCTIONS
    from_async_array_constructor_function: Cell<Option<Gc<FunctionObject>>>,

    default_collator: Cell<Option<Gc<Object>>>,
}

define_cell!(Intrinsics, Other);

/// The intrinsic in `slot`, which CreateIntrinsics creates unless it comes with a part of the runtime that does not
/// exist yet.
fn created_intrinsic<T>(slot: &Cell<Option<Gc<T>>>, name: &str) -> Gc<T> {
    slot.get()
        .unwrap_or_else(|| unimplemented_runtime_function(&format!("the realm intrinsic {name}"), 0))
}

/// Accessors for the intrinsics CreateIntrinsics creates up front, and the property offsets of its premade shapes.
macro_rules! created_intrinsics {
    (cells { $($name:ident: $type:ty => $description:literal,)* } offsets { $($offset:ident,)* }) => {
        impl Intrinsics {
            $(
                pub fn $name(&self) -> Gc<$type> {
                    created_intrinsic(&self.$name, $description)
                }
            )*
            $(
                pub fn $offset(&self) -> u32 {
                    self.$offset.get()
                }
            )*
        }
    };
}

created_intrinsics! {
    cells {
        empty_object_shape: Shape => "empty object shape",
        new_object_shape: Shape => "new object shape",
        iterator_result_object_shape: Shape => "iterator result object shape",
        normal_function_prototype_shape: Shape => "normal function prototype shape",
        normal_function_shape: Shape => "normal function shape",
        async_function_shape: Shape => "async function shape",
        generator_function_shape: Shape => "generator function shape",
        async_generator_function_shape: Shape => "async generator function shape",
        native_function_shape: Shape => "native function shape",
        unmapped_arguments_object_shape: Shape => "unmapped arguments object shape",
        mapped_arguments_object_shape: Shape => "mapped arguments object shape",
        regexp_builtin_exec_array_shape: Shape => "RegExpBuiltinExec array shape",
        throw_type_error_accessor: Accessor => "%ThrowTypeError% accessor",
        proxy_constructor: ProxyConstructor => "%Proxy%",
        async_from_sync_iterator_prototype: Object => "%AsyncFromSyncIteratorPrototype%",
        async_generator_prototype: Object => "%AsyncGeneratorPrototype%",
        generator_prototype: Object => "%GeneratorPrototype%",
        wrap_for_valid_iterator_prototype: Object => "%WrapForValidIteratorPrototype%",
        intl_segments_prototype: Object => "%IntlSegmentsPrototype%",
        eval_function: FunctionObject => "%eval%",
        is_finite_function: FunctionObject => "%isFinite%",
        is_nan_function: FunctionObject => "%isNaN%",
        parse_float_function: FunctionObject => "%parseFloat%",
        parse_int_function: FunctionObject => "%parseInt%",
        decode_uri_function: FunctionObject => "%decodeURI%",
        decode_uri_component_function: FunctionObject => "%decodeURIComponent%",
        encode_uri_function: FunctionObject => "%encodeURI%",
        encode_uri_component_function: FunctionObject => "%encodeURIComponent%",
        escape_function: FunctionObject => "%escape%",
        unescape_function: FunctionObject => "%unescape%",
        array_prototype_values_function: FunctionObject => "%Array.prototype.values%",
        date_constructor_now_function: FunctionObject => "%Date.now%",
        json_parse_function: FunctionObject => "%JSON.parse%",
        json_stringify_function: FunctionObject => "%JSON.stringify%",
        object_prototype_to_string_function: FunctionObject => "%Object.prototype.toString%",
        throw_type_error_function: FunctionObject => "%ThrowTypeError%",
        array_iterator_prototype: Object => "%ArrayIteratorPrototype%",
        async_iterator_prototype: Object => "%AsyncIteratorPrototype%",
        intl_segment_iterator_prototype: Object => "%IntlSegmentIteratorPrototype%",
        iterator_helper_prototype: Object => "%IteratorHelperPrototype%",
        map_iterator_prototype: Object => "%MapIteratorPrototype%",
        regexp_string_iterator_prototype: Object => "%RegExpStringIteratorPrototype%",
        set_iterator_prototype: Object => "%SetIteratorPrototype%",
        string_iterator_prototype: Object => "%StringIteratorPrototype%",
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

/// The lazy accessors of a constructor and its prototype, which create both the first time either is asked for, as
/// the C++ Intrinsics::snake_name_constructor() and snake_name_prototype() do.
macro_rules! builtin_type_accessors {
    ($($prototype:ident, $constructor:ident: $constructor_type:ty, $initialize:ident;)*) => {
        impl Intrinsics {
            $(
                pub fn $constructor(&self, vm: &Vm) -> Gc<$constructor_type> {
                    if self.$constructor.get().is_none() {
                        self.$initialize(vm);
                    }
                    self.$constructor.get().expect("initializing a builtin type creates its constructor")
                }

                pub fn $prototype(&self, vm: &Vm) -> Gc<Object> {
                    if self.$prototype.get().is_none() {
                        self.$initialize(vm);
                    }
                    self.$prototype.get().expect("initializing a builtin type creates its prototype")
                }
            )*
        }
    };
}

builtin_type_accessors! {
    aggregate_error_prototype, aggregate_error_constructor: AggregateErrorConstructor, initialize_aggregate_error;
    array_prototype, array_constructor: ArrayConstructor, initialize_array;
    array_buffer_prototype, array_buffer_constructor: FunctionObject, initialize_array_buffer;
    async_disposable_stack_prototype, async_disposable_stack_constructor: FunctionObject, initialize_async_disposable_stack;
    async_function_prototype, async_function_constructor: AsyncFunctionConstructor, initialize_async_function;
    async_generator_function_prototype, async_generator_function_constructor: AsyncGeneratorFunctionConstructor, initialize_async_generator_function;
    bigint_prototype, bigint_constructor: BigIntConstructor, initialize_bigint;
    boolean_prototype, boolean_constructor: BooleanConstructor, initialize_boolean;
    data_view_prototype, data_view_constructor: FunctionObject, initialize_data_view;
    date_prototype, date_constructor: FunctionObject, initialize_date;
    disposable_stack_prototype, disposable_stack_constructor: FunctionObject, initialize_disposable_stack;
    error_prototype, error_constructor: ErrorConstructor, initialize_error;
    finalization_registry_prototype, finalization_registry_constructor: FinalizationRegistryConstructor, initialize_finalization_registry;
    function_prototype, function_constructor: FunctionConstructor, initialize_function;
    generator_function_prototype, generator_function_constructor: GeneratorFunctionConstructor, initialize_generator_function;
    iterator_prototype, iterator_constructor: IteratorConstructor, initialize_iterator;
    map_prototype, map_constructor: MapConstructor, initialize_map;
    number_prototype, number_constructor: NumberConstructor, initialize_number;
    object_prototype, object_constructor: ObjectConstructor, initialize_object;
    promise_prototype, promise_constructor: PromiseConstructor, initialize_promise;
    regexp_prototype, regexp_constructor: RegExpConstructor, initialize_regexp;
    set_prototype, set_constructor: SetConstructor, initialize_set;
    shared_array_buffer_prototype, shared_array_buffer_constructor: FunctionObject, initialize_shared_array_buffer;
    string_prototype, string_constructor: StringConstructor, initialize_string;
    suppressed_error_prototype, suppressed_error_constructor: FunctionObject, initialize_suppressed_error;
    symbol_prototype, symbol_constructor: SymbolConstructor, initialize_symbol;
    weak_map_prototype, weak_map_constructor: WeakMapConstructor, initialize_weak_map;
    weak_ref_prototype, weak_ref_constructor: WeakRefConstructor, initialize_weak_ref;
    weak_set_prototype, weak_set_constructor: WeakSetConstructor, initialize_weak_set;
    typed_array_prototype, typed_array_constructor: FunctionObject, initialize_typed_array;
    eval_error_prototype, eval_error_constructor: EvalErrorConstructor, initialize_eval_error;
    internal_error_prototype, internal_error_constructor: InternalErrorConstructor, initialize_internal_error;
    range_error_prototype, range_error_constructor: RangeErrorConstructor, initialize_range_error;
    reference_error_prototype, reference_error_constructor: ReferenceErrorConstructor, initialize_reference_error;
    syntax_error_prototype, syntax_error_constructor: SyntaxErrorConstructor, initialize_syntax_error;
    type_error_prototype, type_error_constructor: TypeErrorConstructor, initialize_type_error;
    uri_error_prototype, uri_error_constructor: URIErrorConstructor, initialize_uri_error;
    uint8_array_prototype, uint8_array_constructor: FunctionObject, initialize_uint8_array;
    uint8_clamped_array_prototype, uint8_clamped_array_constructor: FunctionObject, initialize_uint8_clamped_array;
    uint16_array_prototype, uint16_array_constructor: FunctionObject, initialize_uint16_array;
    uint32_array_prototype, uint32_array_constructor: FunctionObject, initialize_uint32_array;
    big_uint64_array_prototype, big_uint64_array_constructor: FunctionObject, initialize_big_uint64_array;
    int8_array_prototype, int8_array_constructor: FunctionObject, initialize_int8_array;
    int16_array_prototype, int16_array_constructor: FunctionObject, initialize_int16_array;
    int32_array_prototype, int32_array_constructor: FunctionObject, initialize_int32_array;
    big_int64_array_prototype, big_int64_array_constructor: FunctionObject, initialize_big_int64_array;
    float16_array_prototype, float16_array_constructor: FunctionObject, initialize_float16_array;
    float32_array_prototype, float32_array_constructor: FunctionObject, initialize_float32_array;
    float64_array_prototype, float64_array_constructor: FunctionObject, initialize_float64_array;
    intl_collator_prototype, intl_collator_constructor: FunctionObject, initialize_intl_collator;
    intl_date_time_format_prototype, intl_date_time_format_constructor: FunctionObject, initialize_intl_date_time_format;
    intl_display_names_prototype, intl_display_names_constructor: FunctionObject, initialize_intl_display_names;
    intl_duration_format_prototype, intl_duration_format_constructor: FunctionObject, initialize_intl_duration_format;
    intl_list_format_prototype, intl_list_format_constructor: FunctionObject, initialize_intl_list_format;
    intl_locale_prototype, intl_locale_constructor: FunctionObject, initialize_intl_locale;
    intl_number_format_prototype, intl_number_format_constructor: FunctionObject, initialize_intl_number_format;
    intl_plural_rules_prototype, intl_plural_rules_constructor: FunctionObject, initialize_intl_plural_rules;
    intl_relative_time_format_prototype, intl_relative_time_format_constructor: FunctionObject, initialize_intl_relative_time_format;
    intl_segmenter_prototype, intl_segmenter_constructor: FunctionObject, initialize_intl_segmenter;
    temporal_duration_prototype, temporal_duration_constructor: FunctionObject, initialize_temporal_duration;
    temporal_instant_prototype, temporal_instant_constructor: FunctionObject, initialize_temporal_instant;
    temporal_plain_date_prototype, temporal_plain_date_constructor: FunctionObject, initialize_temporal_plain_date;
    temporal_plain_date_time_prototype, temporal_plain_date_time_constructor: FunctionObject, initialize_temporal_plain_date_time;
    temporal_plain_month_day_prototype, temporal_plain_month_day_constructor: FunctionObject, initialize_temporal_plain_month_day;
    temporal_plain_time_prototype, temporal_plain_time_constructor: FunctionObject, initialize_temporal_plain_time;
    temporal_plain_year_month_prototype, temporal_plain_year_month_constructor: FunctionObject, initialize_temporal_plain_year_month;
    temporal_zoned_date_time_prototype, temporal_zoned_date_time_constructor: FunctionObject, initialize_temporal_zoned_date_time;
}

/// Intrinsics::initialize_snake_name() for the builtin types whose constructor and prototype create no other
/// intrinsics: creates the prototype and the constructor, and links them with initialize_constructor().
macro_rules! initialize_builtin_types {
    ($($initialize:ident: $prototype:ident: $prototype_type:ty, $constructor:ident: $constructor_type:ty, $name:ident;)*) => {
        impl Intrinsics {
            $(
                fn $initialize(&self, vm: &Vm) {
                    assert!(self.$prototype.get().is_none());
                    assert!(self.$constructor.get().is_none());
                    let prototype = <$prototype_type>::create(vm, self.realm);
                    self.$prototype.set(Some(prototype.upcast()));
                    let constructor = <$constructor_type>::create(vm, self.realm);
                    self.$constructor.set(Some(constructor));

                    initialize_constructor(
                        vm,
                        &vm.names.$name,
                        &constructor,
                        Some(prototype.upcast()),
                        PropertyAttributes::new(Attribute::WRITABLE | Attribute::CONFIGURABLE),
                    );
                }
            )*
        }
    };
}

initialize_builtin_types! {
    initialize_aggregate_error: aggregate_error_prototype: AggregateErrorPrototype, aggregate_error_constructor: AggregateErrorConstructor, AggregateError;
    initialize_array: array_prototype: ArrayPrototype, array_constructor: ArrayConstructor, Array;
    initialize_async_function: async_function_prototype: AsyncFunctionPrototype, async_function_constructor: AsyncFunctionConstructor, AsyncFunction;
    initialize_async_generator_function: async_generator_function_prototype: AsyncGeneratorFunctionPrototype, async_generator_function_constructor: AsyncGeneratorFunctionConstructor, AsyncGeneratorFunction;
    initialize_bigint: bigint_prototype: BigIntPrototype, bigint_constructor: BigIntConstructor, BigInt;
    initialize_boolean: boolean_prototype: BooleanPrototype, boolean_constructor: BooleanConstructor, Boolean;
    initialize_error: error_prototype: ErrorPrototype, error_constructor: ErrorConstructor, Error;
    initialize_finalization_registry: finalization_registry_prototype: FinalizationRegistryPrototype, finalization_registry_constructor: FinalizationRegistryConstructor, FinalizationRegistry;
    initialize_function: function_prototype: FunctionPrototype, function_constructor: FunctionConstructor, Function;
    initialize_generator_function: generator_function_prototype: GeneratorFunctionPrototype, generator_function_constructor: GeneratorFunctionConstructor, GeneratorFunction;
    initialize_map: map_prototype: MapPrototype, map_constructor: MapConstructor, Map;
    initialize_number: number_prototype: NumberPrototype, number_constructor: NumberConstructor, Number;
    initialize_object: object_prototype: ObjectPrototype, object_constructor: ObjectConstructor, Object;
    initialize_promise: promise_prototype: PromisePrototype, promise_constructor: PromiseConstructor, Promise;
    initialize_regexp: regexp_prototype: RegExpPrototype, regexp_constructor: RegExpConstructor, RegExp;
    initialize_set: set_prototype: SetPrototype, set_constructor: SetConstructor, Set;
    initialize_string: string_prototype: StringPrototype, string_constructor: StringConstructor, String;
    initialize_symbol: symbol_prototype: SymbolPrototype, symbol_constructor: SymbolConstructor, Symbol;
    initialize_weak_map: weak_map_prototype: WeakMapPrototype, weak_map_constructor: WeakMapConstructor, WeakMap;
    initialize_weak_ref: weak_ref_prototype: WeakRefPrototype, weak_ref_constructor: WeakRefConstructor, WeakRef;
    initialize_weak_set: weak_set_prototype: WeakSetPrototype, weak_set_constructor: WeakSetConstructor, WeakSet;
    initialize_eval_error: eval_error_prototype: EvalErrorPrototype, eval_error_constructor: EvalErrorConstructor, EvalError;
    initialize_internal_error: internal_error_prototype: InternalErrorPrototype, internal_error_constructor: InternalErrorConstructor, InternalError;
    initialize_range_error: range_error_prototype: RangeErrorPrototype, range_error_constructor: RangeErrorConstructor, RangeError;
    initialize_reference_error: reference_error_prototype: ReferenceErrorPrototype, reference_error_constructor: ReferenceErrorConstructor, ReferenceError;
    initialize_syntax_error: syntax_error_prototype: SyntaxErrorPrototype, syntax_error_constructor: SyntaxErrorConstructor, SyntaxError;
    initialize_type_error: type_error_prototype: TypeErrorPrototype, type_error_constructor: TypeErrorConstructor, TypeError;
    initialize_uri_error: uri_error_prototype: URIErrorPrototype, uri_error_constructor: URIErrorConstructor, URIError;
}

/// Intrinsics::initialize_snake_name() for the builtin types whose constructor slot holds a FunctionObject.
macro_rules! initialize_builtin_function_types {
    ($($initialize:ident: $prototype:ident: $prototype_type:ty, $constructor:ident: $constructor_type:ty, $name:ident;)*) => {
        impl Intrinsics {
            $(
                fn $initialize(&self, vm: &Vm) {
                    assert!(self.$prototype.get().is_none());
                    assert!(self.$constructor.get().is_none());
                    let prototype = <$prototype_type>::create(vm, self.realm);
                    self.$prototype.set(Some(prototype.upcast()));
                    let constructor = <$constructor_type>::create(vm, self.realm);
                    self.$constructor.set(Some(constructor.upcast()));

                    initialize_constructor(
                        vm,
                        &vm.names.$name,
                        &constructor,
                        Some(prototype.upcast()),
                        PropertyAttributes::new(Attribute::WRITABLE | Attribute::CONFIGURABLE),
                    );
                }
            )*
        }
    };
}

initialize_builtin_function_types! {
    initialize_array_buffer: array_buffer_prototype: ArrayBufferPrototype, array_buffer_constructor: ArrayBufferConstructor, ArrayBuffer;
    initialize_data_view: data_view_prototype: DataViewPrototype, data_view_constructor: DataViewConstructor, DataView;
    initialize_shared_array_buffer: shared_array_buffer_prototype: SharedArrayBufferPrototype, shared_array_buffer_constructor: SharedArrayBufferConstructor, SharedArrayBuffer;
    initialize_typed_array: typed_array_prototype: TypedArrayPrototype, typed_array_constructor: TypedArrayConstructor, TypedArray;
}

/// Intrinsics::initialize_snake_name() for the typed arrays, whose prototypes and constructors extend %TypedArray%'s.
macro_rules! initialize_typed_array_types {
    ($($initialize:ident: $prototype:ident: $prototype_type:ty, $constructor:ident: $constructor_type:ty, $name:ident;)*) => {
        impl Intrinsics {
            $(
                fn $initialize(&self, vm: &Vm) {
                    assert!(self.$prototype.get().is_none());
                    assert!(self.$constructor.get().is_none());
                    let prototype = <$prototype_type>::create(vm, self.realm, self.typed_array_prototype(vm));
                    self.$prototype.set(Some(prototype.upcast()));
                    let constructor =
                        <$constructor_type>::create(vm, self.realm, self.typed_array_constructor(vm).upcast());
                    self.$constructor.set(Some(constructor.upcast()));

                    initialize_constructor(
                        vm,
                        &vm.names.$name,
                        &constructor,
                        Some(prototype.upcast()),
                        PropertyAttributes::new(Attribute::WRITABLE | Attribute::CONFIGURABLE),
                    );
                }
            )*
        }
    };
}

initialize_typed_array_types! {
    initialize_uint8_array: uint8_array_prototype: Uint8ArrayPrototype, uint8_array_constructor: Uint8ArrayConstructor, Uint8Array;
    initialize_uint8_clamped_array: uint8_clamped_array_prototype: Uint8ClampedArrayPrototype, uint8_clamped_array_constructor: Uint8ClampedArrayConstructor, Uint8ClampedArray;
    initialize_uint16_array: uint16_array_prototype: Uint16ArrayPrototype, uint16_array_constructor: Uint16ArrayConstructor, Uint16Array;
    initialize_uint32_array: uint32_array_prototype: Uint32ArrayPrototype, uint32_array_constructor: Uint32ArrayConstructor, Uint32Array;
    initialize_big_uint64_array: big_uint64_array_prototype: BigUint64ArrayPrototype, big_uint64_array_constructor: BigUint64ArrayConstructor, BigUint64Array;
    initialize_int8_array: int8_array_prototype: Int8ArrayPrototype, int8_array_constructor: Int8ArrayConstructor, Int8Array;
    initialize_int16_array: int16_array_prototype: Int16ArrayPrototype, int16_array_constructor: Int16ArrayConstructor, Int16Array;
    initialize_int32_array: int32_array_prototype: Int32ArrayPrototype, int32_array_constructor: Int32ArrayConstructor, Int32Array;
    initialize_big_int64_array: big_int64_array_prototype: BigInt64ArrayPrototype, big_int64_array_constructor: BigInt64ArrayConstructor, BigInt64Array;
    initialize_float16_array: float16_array_prototype: Float16ArrayPrototype, float16_array_constructor: Float16ArrayConstructor, Float16Array;
    initialize_float32_array: float32_array_prototype: Float32ArrayPrototype, float32_array_constructor: Float32ArrayConstructor, Float32Array;
    initialize_float64_array: float64_array_prototype: Float64ArrayPrototype, float64_array_constructor: Float64ArrayConstructor, Float64Array;
}

/// Intrinsics::initialize_snake_name() for the builtin types the runtime does not have yet.
macro_rules! unimplemented_builtin_types {
    ($($initialize:ident => $name:literal,)*) => {
        impl Intrinsics {
            $(
                fn $initialize(&self, _vm: &Vm) {
                    unimplemented_runtime_function(
                        concat!("Intrinsics::", stringify!($initialize), ", for the ", $name, " constructor and prototype"),
                        0,
                    )
                }
            )*
        }
    };
}

unimplemented_builtin_types! {
    initialize_async_disposable_stack => "AsyncDisposableStack",
    initialize_date => "Date",
    initialize_disposable_stack => "DisposableStack",
    initialize_suppressed_error => "SuppressedError",
    initialize_intl_collator => "Intl.Collator",
    initialize_intl_date_time_format => "Intl.DateTimeFormat",
    initialize_intl_display_names => "Intl.DisplayNames",
    initialize_intl_duration_format => "Intl.DurationFormat",
    initialize_intl_list_format => "Intl.ListFormat",
    initialize_intl_locale => "Intl.Locale",
    initialize_intl_number_format => "Intl.NumberFormat",
    initialize_intl_plural_rules => "Intl.PluralRules",
    initialize_intl_relative_time_format => "Intl.RelativeTimeFormat",
    initialize_intl_segmenter => "Intl.Segmenter",
    initialize_temporal_duration => "Temporal.Duration",
    initialize_temporal_instant => "Temporal.Instant",
    initialize_temporal_plain_date => "Temporal.PlainDate",
    initialize_temporal_plain_date_time => "Temporal.PlainDateTime",
    initialize_temporal_plain_month_day => "Temporal.PlainMonthDay",
    initialize_temporal_plain_time => "Temporal.PlainTime",
    initialize_temporal_plain_year_month => "Temporal.PlainYearMonth",
    initialize_temporal_zoned_date_time => "Temporal.ZonedDateTime",
}

/// The lazy accessors of the other namespace objects, the abstract operations written in JavaScript and the default
/// collator, none of which the runtime has yet.
macro_rules! unimplemented_lazy_intrinsics {
    ($($name:ident: $type:ty => $description:literal,)*) => {
        impl Intrinsics {
            $(
                pub fn $name(&self, _vm: &Vm) -> Gc<$type> {
                    if let Some(intrinsic) = self.$name.get() {
                        return intrinsic;
                    }
                    unimplemented_runtime_function(concat!("the realm intrinsic ", $description), 0)
                }
            )*
        }
    };
}

/// The lazy accessors of the namespace objects, which create the object the first time it is asked for, as the C++
/// Intrinsics::snake_name_object() does.
macro_rules! namespace_object_accessors {
    ($($name:ident: $type:ty;)*) => {
        impl Intrinsics {
            $(
                pub fn $name(&self, vm: &Vm) -> Gc<Object> {
                    if self.$name.get().is_none() {
                        self.$name.set(Some(<$type>::create(vm, self.realm).upcast()));
                    }
                    self.$name.get().expect("the namespace object was just created")
                }
            )*
        }
    };
}

namespace_object_accessors! {
    atomics_object: AtomicsObject;
    json_object: JSONObject;
    math_object: MathObject;
}

unimplemented_lazy_intrinsics! {
    intl_object: Object => "%Intl%",
    temporal_object: Object => "%Temporal%",
    async_iterator_close_abstract_operation_function: FunctionObject => "AsyncIteratorClose, written in JavaScript",
    get_method_abstract_operation_function: FunctionObject => "GetMethod, written in JavaScript",
    get_iterator_direct_abstract_operation_function: FunctionObject => "GetIteratorDirect, written in JavaScript",
    get_iterator_from_method_abstract_operation_function: FunctionObject => "GetIteratorFromMethod, written in JavaScript",
    iterator_complete_abstract_operation_function: FunctionObject => "IteratorComplete, written in JavaScript",
    from_async_array_constructor_function: FunctionObject => "%Array.fromAsync%",
    default_collator: Object => "default Intl.Collator",
}

impl Intrinsics {
    pub fn reflect_object(&self, vm: &Vm) -> Gc<Object> {
        if let Some(reflect_object) = self.reflect_object.get() {
            return reflect_object;
        }
        let reflect_object = ReflectObject::create(vm, self.realm).upcast();
        self.reflect_object.set(Some(reflect_object));
        reflect_object
    }

    pub fn console_object(&self, vm: &Vm) -> Gc<ConsoleObject> {
        if self.console_object.get().is_none() {
            self.console_object.set(Some(ConsoleObject::create(vm, self.realm)));
        }
        self.console_object.get().expect("the console object was just created")
    }
}

impl Intrinsics {
    fn initialize_iterator(&self, vm: &Vm) {
        assert!(self.iterator_prototype.get().is_none());
        assert!(self.iterator_constructor.get().is_none());
        let prototype = IteratorPrototype::create(vm, self.realm);
        self.iterator_prototype.set(Some(prototype.upcast()));
        let constructor = IteratorConstructor::create(vm, self.realm);
        self.iterator_constructor.set(Some(constructor));

        initialize_constructor(
            vm,
            &vm.names.Iterator,
            &constructor,
            None,
            PropertyAttributes::new(Attribute::WRITABLE | Attribute::CONFIGURABLE),
        );
    }
}

/// The prototypes CreateIntrinsics creates, which the object model asks the realm for without having to create them.
macro_rules! created_prototypes_of_realm {
    ($($name:ident => $slot:ident: $description:literal,)*) => {
        impl Realm {
            $(
                pub fn $name(&self) -> Gc<Object> {
                    created_intrinsic(&self.intrinsics().$slot, $description)
                }
            )*
        }
    };
}

created_prototypes_of_realm! {
    object_prototype => object_prototype: "%Object.prototype%",
    function_prototype => function_prototype: "%Function.prototype%",
    array_prototype => array_prototype: "%Array.prototype%",
    generator_function_prototype => generator_function_prototype: "%GeneratorFunction.prototype%",
    async_function_prototype => async_function_prototype: "%AsyncFunction.prototype%",
    async_generator_function_prototype => async_generator_function_prototype: "%AsyncGeneratorFunction.prototype%",
    // Aliases for the Generator and AsyncGenerator Prototype Objects used by the spec.
    generator_function_prototype_prototype => generator_prototype: "%GeneratorFunction.prototype.prototype%",
    async_generator_function_prototype_prototype => async_generator_prototype: "%AsyncGeneratorFunction.prototype.prototype%",
}

fn initialize_constructor(
    vm: &Vm,
    property_key: &PropertyKey,
    constructor: &Object,
    prototype: Option<Gc<Object>>,
    constructor_property_attributes: PropertyAttributes,
) {
    constructor.define_direct_property(
        vm,
        &vm.names.name,
        Value::from_string(PrimitiveString::create_from_fly_string(vm, property_key.as_string())),
        PropertyAttributes::new(Attribute::CONFIGURABLE),
    );
    if let Some(prototype) = prototype {
        prototype.define_direct_property(
            vm,
            &vm.names.constructor,
            Value::from_object(constructor.as_gc()),
            constructor_property_attributes,
        );
    }
}

impl Intrinsics {
    // 9.3.2 CreateIntrinsics ( realmRec ), https://tc39.es/ecma262/#sec-createintrinsics
    pub fn create(vm: &Vm, realm: Gc<Realm>) -> Gc<Intrinsics> {
        // 1. Set realmRec.[[Intrinsics]] to a new Record.
        let intrinsics = vm.heap().allocate(Intrinsics::new(realm));
        realm.set_intrinsics(intrinsics);

        // 2. Set fields of realmRec.[[Intrinsics]] with the values listed in Table 6.
        //    The field names are the names listed in column one of the table.
        //    The value of each field is a new object value fully and recursively populated
        //    with property values as defined by the specification of each object in
        //    clauses 19 through 28. All object property values are newly created object
        //    values. All values that are built-in function objects are created by performing
        //    CreateBuiltinFunction(steps, length, name, slots, realmRec, prototype)
        //    where steps is the definition of that function provided by this specification,
        //    name is the initial value of the function's "name" property, length is the
        //    initial value of the function's "length" property, slots is a list of the
        //    names, if any, of the function's specified internal slots, and prototype
        //    is the specified value of the function's [[Prototype]] internal slot. The
        //    creation of the intrinsics and their properties must be ordered to avoid
        //    any dependencies upon objects that have not yet been created.
        intrinsics.initialize_intrinsics(vm, realm);

        // 3. Perform AddRestrictedFunctionProperties(realmRec.[[Intrinsics]].[[%Function.prototype%]], realmRec).
        add_restricted_function_properties(vm, &realm.intrinsics().function_prototype(vm), realm);

        // 4. Return unused.
        intrinsics
    }

    fn initialize_intrinsics(&self, vm: &Vm, realm: Gc<Realm>) {
        let names = &vm.names;
        let attributes = PropertyAttributes::new;
        let configurable = attributes(Attribute::CONFIGURABLE);
        let writable_configurable = attributes(Attribute::WRITABLE | Attribute::CONFIGURABLE);
        let writable_configurable_enumerable =
            attributes(Attribute::WRITABLE | Attribute::CONFIGURABLE | Attribute::ENUMERABLE);
        let offset_of = |shape: Gc<Shape>, property_key: &PropertyKey| {
            shape
                .lookup(property_key)
                .expect("the premade shape has the property")
                .offset
        };

        // These are done first since other prototypes depend on their presence.
        self.empty_object_shape.set(Some(Shape::create(vm, realm)));
        let object_prototype = allocate_object(vm, ObjectPrototype::new(vm, realm));
        self.object_prototype.set(Some(object_prototype.upcast()));
        object_prototype.convert_to_prototype_if_needed(vm);
        let function_prototype = allocate_object(vm, FunctionPrototype::new(vm, realm));
        self.function_prototype.set(Some(function_prototype.upcast()));
        function_prototype.convert_to_prototype_if_needed(vm);
        let object_prototype: Gc<Object> = object_prototype.upcast();
        let function_prototype: Gc<Object> = function_prototype.upcast();

        let new_object_shape = Shape::create(vm, realm);
        new_object_shape.set_prototype_without_transition(vm, object_prototype);
        self.new_object_shape.set(Some(new_object_shape));

        // OPTIMIZATION: A lot of runtime algorithms create an "iterator result" object.
        //               We pre-bake a shape for these objects and remember the property offsets.
        //               This allows us to construct them very quickly.
        let iterator_result_object_shape = Shape::create(vm, realm);
        iterator_result_object_shape.set_prototype_without_transition(vm, object_prototype);
        iterator_result_object_shape.add_property_without_transition(
            vm,
            &names.value,
            writable_configurable_enumerable,
        );
        iterator_result_object_shape.add_property_without_transition(vm, &names.done, writable_configurable_enumerable);
        self.iterator_result_object_value_offset
            .set(offset_of(iterator_result_object_shape, &names.value));
        self.iterator_result_object_done_offset
            .set(offset_of(iterator_result_object_shape, &names.done));
        self.iterator_result_object_shape
            .set(Some(iterator_result_object_shape));

        let normal_function_prototype_shape = Shape::create(vm, realm);
        normal_function_prototype_shape.set_prototype_without_transition(vm, object_prototype);
        normal_function_prototype_shape.add_property_without_transition(vm, &names.constructor, writable_configurable);
        self.normal_function_prototype_constructor_offset
            .set(offset_of(normal_function_prototype_shape, &names.constructor));
        self.normal_function_prototype_shape
            .set(Some(normal_function_prototype_shape));

        let normal_function_shape = Shape::create(vm, realm);
        normal_function_shape.set_prototype_without_transition(vm, function_prototype);
        normal_function_shape.add_property_without_transition(vm, &names.length, configurable);
        normal_function_shape.add_property_without_transition(vm, &names.name, configurable);
        self.normal_function_length_offset
            .set(offset_of(normal_function_shape, &names.length));
        self.normal_function_name_offset
            .set(offset_of(normal_function_shape, &names.name));
        self.normal_function_shape.set(Some(normal_function_shape));

        let native_function_shape = Shape::create(vm, realm);
        native_function_shape.set_prototype_without_transition(vm, function_prototype);
        native_function_shape.add_property_without_transition(vm, &names.length, configurable);
        native_function_shape.add_property_without_transition(vm, &names.name, configurable);
        self.native_function_length_offset
            .set(offset_of(native_function_shape, &names.length));
        self.native_function_name_offset
            .set(offset_of(native_function_shape, &names.name));
        self.native_function_shape.set(Some(native_function_shape));

        let iterator = PropertyKey::from(vm.well_known_symbols().iterator);

        let unmapped_arguments_object_shape = Shape::create(vm, realm);
        unmapped_arguments_object_shape.set_prototype_without_transition(vm, object_prototype);
        unmapped_arguments_object_shape.set_has_parameter_map();
        unmapped_arguments_object_shape.add_property_without_transition(vm, &names.length, writable_configurable);
        unmapped_arguments_object_shape.add_property_without_transition(vm, &iterator, writable_configurable);
        unmapped_arguments_object_shape.add_property_without_transition(vm, &names.callee, attributes(0));
        self.unmapped_arguments_object_length_offset
            .set(offset_of(unmapped_arguments_object_shape, &names.length));
        self.unmapped_arguments_object_well_known_symbol_iterator_offset
            .set(offset_of(unmapped_arguments_object_shape, &iterator));
        self.unmapped_arguments_object_callee_offset
            .set(offset_of(unmapped_arguments_object_shape, &names.callee));
        self.unmapped_arguments_object_shape
            .set(Some(unmapped_arguments_object_shape));

        let mapped_arguments_object_shape = Shape::create(vm, realm);
        mapped_arguments_object_shape.set_prototype_without_transition(vm, object_prototype);
        mapped_arguments_object_shape.set_has_parameter_map();
        mapped_arguments_object_shape.add_property_without_transition(vm, &names.length, writable_configurable);
        mapped_arguments_object_shape.add_property_without_transition(vm, &iterator, writable_configurable);
        mapped_arguments_object_shape.add_property_without_transition(vm, &names.callee, writable_configurable);
        self.mapped_arguments_object_length_offset
            .set(offset_of(mapped_arguments_object_shape, &names.length));
        self.mapped_arguments_object_well_known_symbol_iterator_offset
            .set(offset_of(mapped_arguments_object_shape, &iterator));
        self.mapped_arguments_object_callee_offset
            .set(offset_of(mapped_arguments_object_shape, &names.callee));
        self.mapped_arguments_object_shape
            .set(Some(mapped_arguments_object_shape));

        // Normally Realm::create() takes care of this, but these are allocated via Heap::allocate().
        function_prototype.initialize(vm, realm);
        object_prototype.initialize(vm, realm);

        // JS_ENUMERATE_ITERATOR_PROTOTYPES
        assert!(self.array_iterator_prototype.get().is_none());
        self.array_iterator_prototype
            .set(Some(ArrayIteratorPrototype::create(vm, realm).upcast()));
        assert!(self.async_iterator_prototype.get().is_none());
        self.async_iterator_prototype
            .set(Some(AsyncIteratorPrototype::create(vm, realm).upcast()));
        // NB: %IntlSegmentIteratorPrototype% comes with Intl.
        assert!(self.iterator_helper_prototype.get().is_none());
        self.iterator_helper_prototype
            .set(Some(IteratorHelperPrototype::create(vm, realm).upcast()));
        assert!(self.map_iterator_prototype.get().is_none());
        self.map_iterator_prototype
            .set(Some(MapIteratorPrototype::create(vm, realm).upcast()));
        assert!(self.regexp_string_iterator_prototype.get().is_none());
        self.regexp_string_iterator_prototype
            .set(Some(RegExpStringIteratorPrototype::create(vm, realm).upcast()));
        assert!(self.set_iterator_prototype.get().is_none());
        self.set_iterator_prototype
            .set(Some(SetIteratorPrototype::create(vm, realm).upcast()));
        assert!(self.string_iterator_prototype.get().is_none());
        self.string_iterator_prototype
            .set(Some(StringIteratorPrototype::create(vm, realm).upcast()));

        // These must be initialized separately as they have no companion constructor
        self.async_from_sync_iterator_prototype
            .set(Some(AsyncFromSyncIteratorPrototype::create(vm, realm).upcast()));
        self.async_generator_prototype
            .set(Some(AsyncGeneratorPrototype::create(vm, realm).upcast()));
        self.generator_prototype
            .set(Some(GeneratorPrototype::create(vm, realm).upcast()));
        // NB: %IntlSegmentsPrototype% comes with Intl.
        self.wrap_for_valid_iterator_prototype
            .set(Some(WrapForValidIteratorPrototype::create(vm, realm).upcast()));

        // These must be initialized before allocating...
        // - AggregateErrorPrototype, which uses ErrorPrototype as its prototype
        // - AggregateErrorConstructor, which uses ErrorConstructor as its prototype
        // - AsyncFunctionConstructor, which uses FunctionConstructor as its prototype
        self.error_prototype
            .set(Some(ErrorPrototype::create(vm, realm).upcast()));
        self.error_constructor.set(Some(ErrorConstructor::create(vm, realm)));
        self.function_constructor
            .set(Some(FunctionConstructor::create(vm, realm)));

        // Not included in JS_ENUMERATE_NATIVE_OBJECTS due to missing distinct prototype
        self.proxy_constructor.set(Some(ProxyConstructor::create(vm, realm)));

        // Global object functions
        self.eval_function.set(Some(
            RawNativeFunction::create(
                vm,
                raw_native!(GlobalObject::eval),
                1,
                &names.eval,
                Some(realm),
                None,
                None,
            )
            .upcast(),
        ));
        let global_object_functions = [
            (
                &self.is_finite_function,
                raw_native!(GlobalObject::is_finite),
                1,
                &names.isFinite,
            ),
            (
                &self.is_nan_function,
                raw_native!(GlobalObject::is_nan),
                1,
                &names.isNaN,
            ),
            (
                &self.parse_float_function,
                raw_native!(GlobalObject::parse_float),
                1,
                &names.parseFloat,
            ),
            (
                &self.parse_int_function,
                raw_native!(GlobalObject::parse_int),
                2,
                &names.parseInt,
            ),
            (
                &self.decode_uri_function,
                raw_native!(GlobalObject::decode_uri),
                1,
                &names.decodeURI,
            ),
            (
                &self.decode_uri_component_function,
                raw_native!(GlobalObject::decode_uri_component),
                1,
                &names.decodeURIComponent,
            ),
            (
                &self.encode_uri_function,
                raw_native!(GlobalObject::encode_uri),
                1,
                &names.encodeURI,
            ),
            (
                &self.encode_uri_component_function,
                raw_native!(GlobalObject::encode_uri_component),
                1,
                &names.encodeURIComponent,
            ),
            (
                &self.escape_function,
                raw_native!(GlobalObject::escape),
                1,
                &names.escape,
            ),
            (
                &self.unescape_function,
                raw_native!(GlobalObject::unescape),
                1,
                &names.unescape,
            ),
        ];
        for (slot, function, length, name) in global_object_functions {
            slot.set(Some(
                RawNativeFunction::create(vm, function, length, name, Some(realm), None, None).upcast(),
            ));
        }

        self.object_constructor.set(Some(ObjectConstructor::create(vm, realm)));

        // 10.2.4.1 %ThrowTypeError% ( ), https://tc39.es/ecma262/#sec-%throwtypeerror%
        let throw_type_error_function = NativeFunction::create(
            vm,
            (),
            |vm, _| vm.throw_completion(ErrorKind::TypeError, ErrorType::RestrictedFunctionPropertiesAccess, &[]),
            0,
            &PropertyKey::from(Utf16FlyString::default()),
            Some(realm),
            None,
            None,
        );
        self.throw_type_error_function
            .set(Some(throw_type_error_function.upcast()));
        throw_type_error_function.define_direct_property(vm, &names.length, Value::from_i32(0), attributes(0));
        throw_type_error_function.define_direct_property(
            vm,
            &names.name,
            Value::from_string(vm.empty_string()),
            attributes(0),
        );
        throw_type_error_function.internal_prevent_extensions(vm).must();

        self.throw_type_error_accessor.set(Some(Accessor::create(
            vm,
            Some(throw_type_error_function.upcast()),
            Some(throw_type_error_function.upcast()),
            None,
        )));

        initialize_constructor(
            vm,
            &names.Error,
            &self.error_constructor(vm),
            Some(self.error_prototype(vm)),
            writable_configurable,
        );
        initialize_constructor(
            vm,
            &names.Function,
            &self.function_constructor(vm),
            Some(function_prototype),
            writable_configurable,
        );
        initialize_constructor(
            vm,
            &names.Object,
            &self.object_constructor(vm),
            Some(object_prototype),
            writable_configurable,
        );
        initialize_constructor(vm, &names.Proxy, &self.proxy_constructor(), None, writable_configurable);

        initialize_constructor(
            vm,
            &names.GeneratorFunction,
            &self.generator_function_constructor(vm),
            Some(self.generator_function_prototype(vm)),
            configurable,
        );
        initialize_constructor(
            vm,
            &names.AsyncGeneratorFunction,
            &self.async_generator_function_constructor(vm),
            Some(self.async_generator_function_prototype(vm)),
            configurable,
        );
        initialize_constructor(
            vm,
            &names.AsyncFunction,
            &self.async_function_constructor(vm),
            Some(self.async_function_prototype(vm)),
            configurable,
        );

        // 27.5.1.1 Generator.prototype.constructor, https://tc39.es/ecma262/#sec-generator.prototype.constructor
        self.generator_prototype().define_direct_property(
            vm,
            &names.constructor,
            Value::from_object(self.generator_function_prototype(vm)),
            configurable,
        );

        // 27.6.1.1 AsyncGenerator.prototype.constructor, https://tc39.es/ecma262/#sec-asyncgenerator-prototype-constructor
        self.async_generator_prototype().define_direct_property(
            vm,
            &names.constructor,
            Value::from_object(self.async_generator_function_prototype(vm)),
            configurable,
        );

        // OPTIMIZATION: Like normal functions, the other function kinds start from a premade shape with their own properties
        //               already in spec order, so creating one only has to store the property values.
        let create_function_shape = |prototype: Gc<Object>, has_prototype_property: bool| {
            let shape = Shape::create(vm, realm);
            shape.set_prototype_without_transition(vm, prototype);
            shape.add_property_without_transition(vm, &names.length, configurable);
            shape.add_property_without_transition(vm, &names.name, configurable);
            if has_prototype_property {
                shape.add_property_without_transition(vm, &names.prototype, attributes(Attribute::WRITABLE));
            }
            assert!(offset_of(shape, &names.length) == self.normal_function_length_offset.get());
            assert!(offset_of(shape, &names.name) == self.normal_function_name_offset.get());
            shape
        };
        self.async_function_shape
            .set(Some(create_function_shape(self.async_function_prototype(vm), false)));
        let generator_function_shape = create_function_shape(self.generator_function_prototype(vm), true);
        self.generator_function_shape.set(Some(generator_function_shape));
        let async_generator_function_shape = create_function_shape(self.async_generator_function_prototype(vm), true);
        self.async_generator_function_shape
            .set(Some(async_generator_function_shape));
        self.generator_function_prototype_property_offset
            .set(offset_of(generator_function_shape, &names.prototype));
        assert!(
            offset_of(async_generator_function_shape, &names.prototype)
                == self.generator_function_prototype_property_offset.get()
        );

        self.array_prototype_values_function.set(Some(
            self.array_prototype(vm)
                .get_without_side_effects(vm, &names.values)
                .as_function(),
        ));
        self.object_prototype_to_string_function.set(Some(
            self.object_prototype(vm)
                .get_without_side_effects(vm, &names.toString)
                .as_function(),
        ));
        self.json_parse_function.set(Some(
            self.json_object(vm)
                .get_without_side_effects(vm, &names.parse)
                .as_function(),
        ));
        self.json_stringify_function.set(Some(
            self.json_object(vm)
                .get_without_side_effects(vm, &names.stringify)
                .as_function(),
        ));
        // NB: Date.now comes with the Date builtins; until a realm has them, its intrinsic accessor stops the process.

        assert!(self.array_prototype(vm).indexed_array_like_size() == 0);
        assert!(self.object_prototype(vm).indexed_array_like_size() == 0);

        let regexp_builtin_exec_array_shape = Shape::create(vm, realm);
        regexp_builtin_exec_array_shape.set_prototype_without_transition(vm, realm.intrinsics().array_prototype(vm));
        regexp_builtin_exec_array_shape.add_property_without_transition(
            vm,
            &names.index,
            writable_configurable_enumerable,
        );
        regexp_builtin_exec_array_shape.add_property_without_transition(
            vm,
            &names.input,
            writable_configurable_enumerable,
        );
        regexp_builtin_exec_array_shape.add_property_without_transition(
            vm,
            &names.groups,
            writable_configurable_enumerable,
        );
        self.regexp_builtin_exec_array_index_offset
            .set(offset_of(regexp_builtin_exec_array_shape, &names.index));
        self.regexp_builtin_exec_array_input_offset
            .set(offset_of(regexp_builtin_exec_array_shape, &names.input));
        self.regexp_builtin_exec_array_groups_offset
            .set(offset_of(regexp_builtin_exec_array_shape, &names.groups));
        self.regexp_builtin_exec_array_shape
            .set(Some(regexp_builtin_exec_array_shape));
    }

    pub fn realm(&self) -> Gc<Realm> {
        self.realm
    }

    /// Stands in for %eval% in the unit tests, which create realms without the global functions.
    #[cfg(test)]
    pub fn set_eval_function_for_tests(&self, function: Gc<FunctionObject>) {
        self.eval_function.set(Some(function));
    }
}

// 10.2.4 AddRestrictedFunctionProperties ( F, realm ), https://tc39.es/ecma262/#sec-addrestrictedfunctionproperties
pub fn add_restricted_function_properties(vm: &Vm, function: &Object, realm: Gc<Realm>) {
    // 1. Assert: realm.[[Intrinsics]].[[%ThrowTypeError%]] exists and has been initialized.
    // NOTE: This is ensured by dereferencing the GCPtr in the getter.

    // 2. Let thrower be realm.[[Intrinsics]].[[%ThrowTypeError%]].
    let thrower = realm.intrinsics().throw_type_error_function();

    // 3. Perform ! DefinePropertyOrThrow(F, "caller", PropertyDescriptor { [[Get]]: thrower, [[Set]]: thrower, [[Enumerable]]: false, [[Configurable]]: true }).
    function.define_direct_accessor(
        vm,
        &vm.names.caller,
        Some(thrower),
        Some(thrower),
        PropertyAttributes::new(Attribute::CONFIGURABLE),
    );

    // 4. Perform ! DefinePropertyOrThrow(F, "arguments", PropertyDescriptor { [[Get]]: thrower, [[Set]]: thrower, [[Enumerable]]: false, [[Configurable]]: true }).
    function.define_direct_accessor(
        vm,
        &vm.names.arguments,
        Some(thrower),
        Some(thrower),
        PropertyAttributes::new(Attribute::CONFIGURABLE),
    );

    // 5. Return unused.
}

#[cfg(all(test, libjs_runtime_tests_with_libgc))]
mod tests {
    use super::*;
    use crate::gc::root::MarkedVec;
    use crate::runtime::abstract_operations::call;
    use crate::runtime::error::TypeError;
    use crate::runtime::object::ORDINARY_OBJECT_METHODS;
    use crate::runtime::realm::test_realm::{key, own_keys, thrown_message};
    use crate::runtime::value::number_to_string;
    use crate::utf16::Utf16View;
    use crate::utilities::initialize_realm;

    include!("../../oracle/intrinsics_table.rs");

    /// The intrinsics of INTRINSICS, by the names it gives them.
    fn named_intrinsics(vm: &Vm, realm: Gc<Realm>) -> MarkedVec<'_, (&'static str, Gc<Object>)> {
        let intrinsics = realm.intrinsics();
        let named = MarkedVec::new(vm);
        let add = |name: &'static str, object: Gc<Object>| named.push((name, object));
        add("Object.prototype", realm.object_prototype());
        add("Function.prototype", realm.function_prototype());
        add("Array.prototype", realm.array_prototype());
        add("String.prototype", intrinsics.string_prototype(vm));
        add("Number.prototype", intrinsics.number_prototype(vm));
        add("Boolean.prototype", intrinsics.boolean_prototype(vm));
        add("Symbol.prototype", intrinsics.symbol_prototype(vm));
        add("BigInt.prototype", intrinsics.bigint_prototype(vm));
        add("Error.prototype", intrinsics.error_prototype(vm));
        add("EvalError.prototype", intrinsics.eval_error_prototype(vm));
        add("InternalError.prototype", intrinsics.internal_error_prototype(vm));
        add("RangeError.prototype", intrinsics.range_error_prototype(vm));
        add("ReferenceError.prototype", intrinsics.reference_error_prototype(vm));
        add("SyntaxError.prototype", intrinsics.syntax_error_prototype(vm));
        add("TypeError.prototype", intrinsics.type_error_prototype(vm));
        add("URIError.prototype", intrinsics.uri_error_prototype(vm));
        add("AggregateError.prototype", intrinsics.aggregate_error_prototype(vm));
        add("%IteratorPrototype%", intrinsics.iterator_prototype(vm));
        add("%ArrayIteratorPrototype%", intrinsics.array_iterator_prototype());
        add("%AsyncIteratorPrototype%", intrinsics.async_iterator_prototype());
        add("%IteratorHelperPrototype%", intrinsics.iterator_helper_prototype());
        add("%MapIteratorPrototype%", intrinsics.map_iterator_prototype());
        add(
            "%RegExpStringIteratorPrototype%",
            intrinsics.regexp_string_iterator_prototype(),
        );
        add("%SetIteratorPrototype%", intrinsics.set_iterator_prototype());
        add("%StringIteratorPrototype%", intrinsics.string_iterator_prototype());
        add(
            "%WrapForValidIteratorPrototype%",
            intrinsics.wrap_for_valid_iterator_prototype(),
        );
        add(
            "%GeneratorFunction.prototype%",
            intrinsics.generator_function_prototype(vm),
        );
        add(
            "%AsyncGeneratorFunction.prototype%",
            intrinsics.async_generator_function_prototype(vm),
        );
        add("%AsyncFunction.prototype%", intrinsics.async_function_prototype(vm));
        add("%GeneratorPrototype%", intrinsics.generator_prototype());
        add("%AsyncGeneratorPrototype%", intrinsics.async_generator_prototype());
        add("Object", intrinsics.object_constructor(vm).upcast());
        add("Function", intrinsics.function_constructor(vm).upcast());
        add("Array", intrinsics.array_constructor(vm).upcast());
        add("String", intrinsics.string_constructor(vm).upcast());
        add("Number", intrinsics.number_constructor(vm).upcast());
        add("Boolean", intrinsics.boolean_constructor(vm).upcast());
        add("Symbol", intrinsics.symbol_constructor(vm).upcast());
        add("BigInt", intrinsics.bigint_constructor(vm).upcast());
        add("Error", intrinsics.error_constructor(vm).upcast());
        add("EvalError", intrinsics.eval_error_constructor(vm).upcast());
        add("InternalError", intrinsics.internal_error_constructor(vm).upcast());
        add("RangeError", intrinsics.range_error_constructor(vm).upcast());
        add("ReferenceError", intrinsics.reference_error_constructor(vm).upcast());
        add("SyntaxError", intrinsics.syntax_error_constructor(vm).upcast());
        add("TypeError", intrinsics.type_error_constructor(vm).upcast());
        add("URIError", intrinsics.uri_error_constructor(vm).upcast());
        add("AggregateError", intrinsics.aggregate_error_constructor(vm).upcast());
        add("Iterator", intrinsics.iterator_constructor(vm).upcast());
        add(
            "%GeneratorFunction%",
            intrinsics.generator_function_constructor(vm).upcast(),
        );
        add(
            "%AsyncGeneratorFunction%",
            intrinsics.async_generator_function_constructor(vm).upcast(),
        );
        add("%AsyncFunction%", intrinsics.async_function_constructor(vm).upcast());
        add("Proxy", intrinsics.proxy_constructor().upcast());
        add("%ThrowTypeError%", intrinsics.throw_type_error_function().upcast());
        add("ArrayBuffer.prototype", intrinsics.array_buffer_prototype(vm));
        add(
            "SharedArrayBuffer.prototype",
            intrinsics.shared_array_buffer_prototype(vm),
        );
        add("DataView.prototype", intrinsics.data_view_prototype(vm));
        add("%TypedArray.prototype%", intrinsics.typed_array_prototype(vm));
        add("Uint8Array.prototype", intrinsics.uint8_array_prototype(vm));
        add(
            "Uint8ClampedArray.prototype",
            intrinsics.uint8_clamped_array_prototype(vm),
        );
        add("Uint16Array.prototype", intrinsics.uint16_array_prototype(vm));
        add("Uint32Array.prototype", intrinsics.uint32_array_prototype(vm));
        add("BigUint64Array.prototype", intrinsics.big_uint64_array_prototype(vm));
        add("Int8Array.prototype", intrinsics.int8_array_prototype(vm));
        add("Int16Array.prototype", intrinsics.int16_array_prototype(vm));
        add("Int32Array.prototype", intrinsics.int32_array_prototype(vm));
        add("BigInt64Array.prototype", intrinsics.big_int64_array_prototype(vm));
        add("Float16Array.prototype", intrinsics.float16_array_prototype(vm));
        add("Float32Array.prototype", intrinsics.float32_array_prototype(vm));
        add("Float64Array.prototype", intrinsics.float64_array_prototype(vm));
        add("ArrayBuffer", intrinsics.array_buffer_constructor(vm).upcast());
        add(
            "SharedArrayBuffer",
            intrinsics.shared_array_buffer_constructor(vm).upcast(),
        );
        add("DataView", intrinsics.data_view_constructor(vm).upcast());
        add("%TypedArray%", intrinsics.typed_array_constructor(vm).upcast());
        add("Uint8Array", intrinsics.uint8_array_constructor(vm).upcast());
        add(
            "Uint8ClampedArray",
            intrinsics.uint8_clamped_array_constructor(vm).upcast(),
        );
        add("Uint16Array", intrinsics.uint16_array_constructor(vm).upcast());
        add("Uint32Array", intrinsics.uint32_array_constructor(vm).upcast());
        add("BigUint64Array", intrinsics.big_uint64_array_constructor(vm).upcast());
        add("Int8Array", intrinsics.int8_array_constructor(vm).upcast());
        add("Int16Array", intrinsics.int16_array_constructor(vm).upcast());
        add("Int32Array", intrinsics.int32_array_constructor(vm).upcast());
        add("BigInt64Array", intrinsics.big_int64_array_constructor(vm).upcast());
        add("Float16Array", intrinsics.float16_array_constructor(vm).upcast());
        add("Float32Array", intrinsics.float32_array_constructor(vm).upcast());
        add("Float64Array", intrinsics.float64_array_constructor(vm).upcast());
        add("Atomics", intrinsics.atomics_object(vm));
        add("globalThis", realm.global_object());
        named
    }

    fn utf8(string: &ak::Utf16String) -> String {
        Utf16View::of_string(string).to_utf8()
    }

    /// Describes a value the way intrinsics.js does.
    fn describe(vm: &Vm, named: &MarkedVec<'_, (&'static str, Gc<Object>)>, value: Value) -> String {
        if value.is_null() {
            return "null".to_string();
        }
        if value.is_undefined() {
            return "undefined".to_string();
        }
        if value.is_object() {
            let object = value.as_object();
            for index in 0..named.len() {
                let (name, intrinsic) = named.get(index).expect("the index is in bounds");
                if intrinsic == object {
                    return name.to_string();
                }
            }
            return if value.is_function() { "function" } else { "object" }.to_string();
        }
        if value.is_string() {
            return format!("\"{}\"", value.as_string().to_utf8());
        }
        if value.is_symbol() {
            return utf8(&value.as_symbol().descriptive_string());
        }
        if value.is_number() {
            return number_to_string(value.as_f64());
        }
        utf8(&value.to_utf16_string(vm).must())
    }

    /// The own properties of `object` as intrinsics.js describes them.
    fn describe_properties(
        vm: &Vm,
        named: &MarkedVec<'_, (&'static str, Gc<Object>)>,
        object: Gc<Object>,
    ) -> Vec<String> {
        let keys = object.internal_own_property_keys(vm).must();
        let mut properties = Vec::new();
        for index in 0..keys.len() {
            let key_value = keys.get(index).expect("the index is in bounds");
            let property_key = PropertyKey::from_value(vm, key_value).must();
            let shown_key = if key_value.is_symbol() {
                let description = utf8(
                    key_value
                        .as_symbol()
                        .description()
                        .expect("the well-known symbols have descriptions"),
                );
                format!("@@{}", description.trim_start_matches("Symbol."))
            } else {
                key_value.as_string().to_utf8()
            };
            let descriptor = object
                .internal_get_own_property(vm, &property_key)
                .must()
                .expect("an own key has a property");
            let flag = |set: Option<bool>, letter: char| if set == Some(true) { letter } else { '-' };
            let mut flags = String::new();
            let value = if let Some(value) = descriptor.value {
                flags.push(flag(descriptor.writable, 'w'));
                describe(vm, named, value)
            } else {
                let has_getter = descriptor.get.flatten().is_some();
                let has_setter = descriptor.set.flatten().is_some();
                format!(
                    "<{}{}>",
                    if has_getter { "get" } else { "" },
                    if has_setter { "set" } else { "" }
                )
            };
            flags.push(flag(descriptor.enumerable, 'e'));
            flags.push(flag(descriptor.configurable, 'c'));
            properties.push(format!("{shown_key}={value}:{flags}"));
        }
        properties
    }

    /// Whether the Rust runtime may lack a property the C++ runtime has: the built-in functions and accessors of
    /// later units, the namespace objects, and the properties of the C++ js binary's global object.
    fn may_be_missing(property: &str) -> bool {
        let (key, rest) = property.split_once('=').expect("a property has a value");
        let value = rest.rsplit_once(':').expect("a property has flags").0;
        value == "function" || value.starts_with('<') || value == "object" || key == "global"
    }

    /// The functions and accessors of other builtins come with later units, so the Rust runtime has to have at least
    /// these ones that this unit defines.
    const PROPERTIES_THE_RUST_RUNTIME_DEFINES: &[(&str, &str)] = &[
        ("Function.prototype", "caller=<getset>:-c"),
        ("Function.prototype", "arguments=<getset>:-c"),
        ("Function.prototype", "@@hasInstance=function:---"),
        ("Error.prototype", "toString=function:w-c"),
        ("Error.prototype", "stack=<getset>:-c"),
        ("Error", "isError=function:w-c"),
        ("Object.prototype", "toString=function:w-c"),
        ("Object.prototype", "__proto__=<getset>:-c"),
        ("Object", "assign=function:w-c"),
        ("Function.prototype", "bind=function:w-c"),
        ("Function.prototype", "toString=function:w-c"),
        ("globalThis", "Reflect=object:w-c"),
    ];

    fn compare_intrinsics_with_the_cpp_runtime(vm: &Vm, realm: Gc<Realm>) {
        let named = named_intrinsics(vm, realm);
        assert_eq!(named.len(), INTRINSICS.len());
        let mut mismatches = Vec::new();
        for (index, (name, expected_prototype, expected_properties)) in INTRINSICS.iter().enumerate() {
            let (actual_name, object) = named.get(index).expect("the index is in bounds");
            assert_eq!(actual_name, *name);
            let prototype = object
                .internal_get_prototype_of(vm)
                .must()
                .map_or(Value::NULL, Value::from_object);
            let actual_prototype = describe(vm, &named, prototype);
            if actual_prototype != *expected_prototype {
                mismatches.push(format!(
                    "{name}: the prototype is {actual_prototype}, not {expected_prototype}"
                ));
            }

            // The Rust runtime's properties are the C++ runtime's, in the same order, less the ones it may lack.
            let actual_properties = describe_properties(vm, &named, object);
            let mut actual = actual_properties.iter().peekable();
            for expected in *expected_properties {
                if actual.peek() == Some(&&expected.to_string()) {
                    actual.next();
                } else if !may_be_missing(expected) {
                    mismatches.push(format!("{name}: {expected} is missing or out of order"));
                }
            }
            for unexpected in actual {
                mismatches.push(format!("{name}: {unexpected} is not a property of the C++ runtime's"));
            }
            for (owner, property) in PROPERTIES_THE_RUST_RUNTIME_DEFINES {
                if owner == name && !actual_properties.iter().any(|actual| actual == property) {
                    mismatches.push(format!("{name}: {property} is missing"));
                }
            }
        }
        assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
    }

    #[test]
    fn intrinsics_have_the_prototypes_and_properties_of_the_cpp_runtime() {
        let vm = Vm::create();
        let root_execution_context = initialize_realm(&vm);
        let realm = root_execution_context.realm();
        compare_intrinsics_with_the_cpp_runtime(&vm, realm);

        let named = named_intrinsics(&vm, realm);
        assert_eq!(
            describe_properties(&vm, &named, realm.intrinsics().error_prototype(&vm)),
            [
                "name=\"Error\":w-c",
                "message=\"\":w-c",
                "toString=function:w-c",
                "stack=<getset>:-c",
                "constructor=Error:w-c"
            ]
        );
        assert_eq!(
            describe_properties(&vm, &named, realm.global_object()).join(" "),
            "eval=function:w-c isFinite=function:w-c isNaN=function:w-c parseFloat=function:w-c parseInt=function:w-c \
             decodeURI=function:w-c decodeURIComponent=function:w-c encodeURI=function:w-c \
             encodeURIComponent=function:w-c globalThis=globalThis:w-c Infinity=Infinity:--- NaN=NaN:--- \
             undefined=undefined:--- AggregateError=AggregateError:w-c Array=Array:w-c ArrayBuffer=ArrayBuffer:w-c \
             BigInt=BigInt:w-c BigInt64Array=BigInt64Array:w-c BigUint64Array=BigUint64Array:w-c Boolean=Boolean:w-c \
             DataView=DataView:w-c Error=Error:w-c EvalError=EvalError:w-c FinalizationRegistry=function:w-c \
             Float16Array=Float16Array:w-c Float32Array=Float32Array:w-c Float64Array=Float64Array:w-c \
             Function=Function:w-c Int8Array=Int8Array:w-c Int16Array=Int16Array:w-c Int32Array=Int32Array:w-c \
             Iterator=Iterator:w-c Map=function:w-c Number=Number:w-c Object=Object:w-c Promise=function:w-c \
             Proxy=Proxy:w-c RangeError=RangeError:w-c ReferenceError=ReferenceError:w-c RegExp=function:w-c \
             Set=function:w-c SharedArrayBuffer=SharedArrayBuffer:w-c String=String:w-c Symbol=Symbol:w-c \
             SyntaxError=SyntaxError:w-c TypeError=TypeError:w-c Uint8Array=Uint8Array:w-c \
             Uint8ClampedArray=Uint8ClampedArray:w-c Uint16Array=Uint16Array:w-c Uint32Array=Uint32Array:w-c \
             URIError=URIError:w-c WeakMap=function:w-c WeakRef=function:w-c WeakSet=function:w-c Atomics=Atomics:w-c \
             JSON=object:w-c Math=object:w-c Reflect=object:w-c escape=function:w-c unescape=function:w-c \
             InternalError=InternalError:w-c console=object:w-c"
        );
    }

    /// The own properties of `object` as intrinsics.js describes them, with the length and name of each function.
    fn describe_properties_with_functions(vm: &Vm, object: Gc<Object>) -> String {
        let named = MarkedVec::new(vm);
        let mut properties = describe_properties(vm, &named, object);
        let keys = object.internal_own_property_keys(vm).must();
        for (index, property) in properties.iter_mut().enumerate() {
            let key = PropertyKey::from_value(vm, keys.get(index).expect("the index is in bounds")).must();
            let value = object.get_without_side_effects(vm, &key);
            if value.is_function() {
                let function = value.as_function();
                let length = describe(vm, &named, function.get_without_side_effects(vm, &vm.names.length));
                let name = function
                    .get_without_side_effects(vm, &vm.names.name)
                    .as_string()
                    .to_utf8();
                *property = property.replacen("=function:", &format!("=function/{length}/{name}:"), 1);
            }
        }
        properties.join(" ")
    }

    #[test]
    fn math_and_json_have_the_properties_of_the_cpp_runtime() {
        let vm = Vm::create();
        let root_execution_context = initialize_realm(&vm);
        let realm = root_execution_context.realm();
        let intrinsics = realm.intrinsics();
        // What the C++ js binary describes these objects as.
        assert_eq!(
            describe_properties_with_functions(&vm, intrinsics.math_object(&vm)),
            "abs=function/1/abs:w-c random=function/0/random:w-c sqrt=function/1/sqrt:w-c floor=function/1/floor:w-c \
             ceil=function/1/ceil:w-c round=function/1/round:w-c max=function/2/max:w-c min=function/2/min:w-c \
             trunc=function/1/trunc:w-c sin=function/1/sin:w-c cos=function/1/cos:w-c tan=function/1/tan:w-c \
             pow=function/2/pow:w-c exp=function/1/exp:w-c expm1=function/1/expm1:w-c sign=function/1/sign:w-c \
             clz32=function/1/clz32:w-c acos=function/1/acos:w-c acosh=function/1/acosh:w-c asin=function/1/asin:w-c \
             asinh=function/1/asinh:w-c atan=function/1/atan:w-c atanh=function/1/atanh:w-c log1p=function/1/log1p:w-c \
             cbrt=function/1/cbrt:w-c atan2=function/2/atan2:w-c fround=function/1/fround:w-c \
             f16round=function/1/f16round:w-c hypot=function/2/hypot:w-c imul=function/2/imul:w-c \
             log=function/1/log:w-c log2=function/1/log2:w-c log10=function/1/log10:w-c sinh=function/1/sinh:w-c \
             cosh=function/1/cosh:w-c tanh=function/1/tanh:w-c sumPrecise=function/1/sumPrecise:w-c \
             E=2.718281828459045:--- LN2=0.6931471805599453:--- LN10=2.302585092994046:--- \
             LOG2E=1.4426950408889634:--- LOG10E=0.4342944819032518:--- PI=3.141592653589793:--- \
             SQRT1_2=0.7071067811865476:--- SQRT2=1.4142135623730951:--- @@toStringTag=\"Math\":--c"
        );
        assert_eq!(
            describe_properties_with_functions(&vm, intrinsics.json_object(&vm)),
            "stringify=function/3/stringify:w-c parse=function/2/parse:w-c rawJSON=function/1/rawJSON:w-c \
             isRawJSON=function/1/isRawJSON:w-c @@toStringTag=\"JSON\":--c"
        );
        let global_functions = [
            ("isFinite", intrinsics.is_finite_function(), 1),
            ("isNaN", intrinsics.is_nan_function(), 1),
            ("parseFloat", intrinsics.parse_float_function(), 1),
            ("parseInt", intrinsics.parse_int_function(), 2),
            ("decodeURI", intrinsics.decode_uri_function(), 1),
            ("decodeURIComponent", intrinsics.decode_uri_component_function(), 1),
            ("encodeURI", intrinsics.encode_uri_function(), 1),
            ("encodeURIComponent", intrinsics.encode_uri_component_function(), 1),
            ("escape", intrinsics.escape_function(), 1),
            ("unescape", intrinsics.unescape_function(), 1),
        ];
        for (name, function, length) in global_functions {
            assert!(function.get_without_side_effects(&vm, &vm.names.length) == Value::from_i32(length));
            assert_eq!(
                function
                    .get_without_side_effects(&vm, &vm.names.name)
                    .as_string()
                    .to_utf8(),
                name
            );
        }
    }

    #[test]
    fn intrinsics_survive_collecting_garbage_on_every_allocation() {
        let vm = Vm::create();
        vm.heap().set_should_collect_on_every_allocation(true);
        let root_execution_context = initialize_realm(&vm);
        compare_intrinsics_with_the_cpp_runtime(&vm, root_execution_context.realm());
        vm.heap().set_should_collect_on_every_allocation(false);
    }

    #[test]
    fn intrinsics_have_the_classes_of_the_cpp_runtime() {
        let vm = Vm::create();
        let root_execution_context = initialize_realm(&vm);
        let realm = root_execution_context.realm();
        let named = named_intrinsics(&vm, realm);
        // What the C++ runtime names the intrinsics in messages like "[object ObjectPrototype] is not a function".
        let expected_classes = [
            ("Object.prototype", "ObjectPrototype"),
            ("Function.prototype", "FunctionPrototype"),
            ("Array.prototype", "ArrayPrototype"),
            ("String.prototype", "StringPrototype"),
            ("Number.prototype", "NumberPrototype"),
            ("Boolean.prototype", "BooleanPrototype"),
            ("Symbol.prototype", "SymbolPrototype"),
            ("BigInt.prototype", "BigIntPrototype"),
            ("Error.prototype", "ErrorPrototype"),
            ("TypeError.prototype", "TypeErrorPrototype"),
            ("AggregateError.prototype", "AggregateErrorPrototype"),
            ("%IteratorPrototype%", "IteratorPrototype"),
            ("%ArrayIteratorPrototype%", "ArrayIteratorPrototype"),
            ("%AsyncIteratorPrototype%", "AsyncIteratorPrototype"),
            ("%GeneratorFunction.prototype%", "GeneratorFunctionPrototype"),
            ("%AsyncGeneratorFunction.prototype%", "AsyncGeneratorFunctionPrototype"),
            ("%AsyncFunction.prototype%", "AsyncFunctionPrototype"),
            ("%GeneratorPrototype%", "GeneratorPrototype"),
            ("%AsyncGeneratorPrototype%", "AsyncGeneratorPrototype"),
            ("%ThrowTypeError%", "CapturingNativeFunction"),
            ("Error", "ErrorConstructor"),
            ("TypeError", "TypeErrorConstructor"),
            ("Object", "ObjectConstructor"),
            ("globalThis", "GlobalObject"),
        ];
        for (name, class) in expected_classes {
            let object = (0..named.len())
                .map(|index| named.get(index).expect("the index is in bounds"))
                .find(|(intrinsic_name, _)| *intrinsic_name == name)
                .expect("the intrinsic is named")
                .1;
            assert_eq!(
                utf8(&Value::from_object(object).to_utf16_string_without_side_effects()),
                format!("[object {class}]")
            );
        }
    }

    #[test]
    fn create_intrinsics_creates_the_builtin_types_the_cpp_runtime_creates_up_front() {
        let vm = Vm::create();
        let realm = Realm::create(&vm);
        let intrinsics = Intrinsics::create(&vm, realm);
        assert!(realm.intrinsics() == intrinsics && intrinsics.realm() == realm);

        let up_front = [
            intrinsics.object_prototype.get(),
            intrinsics.function_prototype.get(),
            intrinsics.error_prototype.get(),
            intrinsics.array_prototype.get(),
            intrinsics.iterator_prototype.get(),
            intrinsics.generator_function_prototype.get(),
            intrinsics.async_generator_function_prototype.get(),
            intrinsics.async_function_prototype.get(),
        ];
        assert!(up_front.iter().all(Option::is_some));
        assert!(intrinsics.error_constructor.get().is_some() && intrinsics.function_constructor.get().is_some());
        assert!(intrinsics.object_constructor.get().is_some() && intrinsics.array_constructor.get().is_some());

        let lazily = [
            intrinsics.string_prototype.get(),
            intrinsics.number_prototype.get(),
            intrinsics.boolean_prototype.get(),
            intrinsics.symbol_prototype.get(),
            intrinsics.bigint_prototype.get(),
            intrinsics.type_error_prototype.get(),
            intrinsics.aggregate_error_prototype.get(),
        ];
        assert!(lazily.iter().all(Option::is_none));
        let type_error_prototype = intrinsics.type_error_prototype(&vm);
        assert!(intrinsics.type_error_constructor.get().is_some());
        assert!(type_error_prototype.prototype() == Some(intrinsics.error_prototype(&vm)));
        assert!(intrinsics.type_error_prototype(&vm) == type_error_prototype);

        // The intrinsics the runtime does not have yet stop the process with their names.
        assert!(
            thrown_message(|| intrinsics.temporal_duration_constructor(&vm))
                .contains("Intrinsics::initialize_temporal_duration")
        );
        assert!(Value::from_object(intrinsics.eval_function()).is_function());
        let values = realm.array_prototype().get_without_side_effects(&vm, &vm.names.values);
        assert!(values == Value::from_object(realm.array_prototype_values_function()));
    }

    #[test]
    fn the_global_object_creates_its_constructors_when_they_are_first_read() {
        let vm = Vm::create();
        let root_execution_context = initialize_realm(&vm);
        let realm = root_execution_context.realm();
        let intrinsics = realm.intrinsics();
        let global = realm.global_object();
        assert!(global.is_global_object() && global.has_intrinsic_accessors());
        assert!(global.prototype() == Some(realm.object_prototype()));
        assert!(realm.global_environment().global_this_value() == global);

        assert!(intrinsics.eval_error_constructor.get().is_none());
        let eval_error = global.get(&vm, &vm.names.EvalError).must();
        assert!(eval_error == Value::from_object(intrinsics.eval_error_constructor(&vm)));
        assert!(global.get(&vm, &vm.names.EvalError).must() == eval_error);

        // A property that is set before it is first read no longer creates its intrinsic.
        assert!(intrinsics.symbol_constructor.get().is_none());
        global.define_direct_property(
            &vm,
            &vm.names.Symbol,
            Value::from_i32(1),
            PropertyAttributes::new(Attribute::WRITABLE | Attribute::CONFIGURABLE),
        );
        assert!(global.get(&vm, &vm.names.Symbol).must() == Value::from_i32(1));
        vm.heap().collect_garbage();
        assert!(intrinsics.symbol_constructor.get().is_none());

        // [[Delete]] reads the property it deletes, which creates its intrinsic as in the C++ runtime.
        assert!(intrinsics.boolean_constructor.get().is_none());
        assert!(global.internal_delete(&vm, &vm.names.Boolean).must());
        assert!(global.get(&vm, &vm.names.Boolean).must() == Value::UNDEFINED);
        assert!(intrinsics.boolean_constructor.get().is_some());

        // The accessors of an object that dies are forgotten.
        let object = Object::create(&vm, realm, None);
        object.define_intrinsic_accessor(&vm, &key("lazy"), PropertyAttributes::new(0), |vm, realm| {
            Value::from_object(realm.intrinsics().string_prototype(vm))
        });
        assert!(object.get(&vm, &key("lazy")).must() == Value::from_object(intrinsics.string_prototype(&vm)));
        assert_eq!(own_keys(&vm, &object), "lazy");

        #[inline(never)]
        fn define_accessors_on_unreachable_objects(vm: &Vm, realm: Gc<Realm>, count: usize) {
            for _ in 0..count {
                Object::create(vm, realm, None).define_intrinsic_accessor(
                    vm,
                    &key("never_read"),
                    PropertyAttributes::new(0),
                    |_, _| unreachable!("the property is never read"),
                );
            }
        }
        let accessor_count = vm.intrinsic_accessors().borrow().len();
        define_accessors_on_unreachable_objects(&vm, realm, 64);
        assert_eq!(vm.intrinsic_accessors().borrow().len(), accessor_count + 64);
        vm.heap().collect_garbage();
        // The stack is scanned conservatively, so a stale pointer may keep one of the objects alive.
        assert!(vm.intrinsic_accessors().borrow().len() < accessor_count + 4);
    }

    #[test]
    fn object_prototype_is_an_immutable_prototype_exotic_object() {
        let vm = Vm::create();
        let root_execution_context = initialize_realm(&vm);
        let realm = root_execution_context.realm();
        let object_prototype = realm.object_prototype();
        assert!(
            object_prototype
                .class()
                .object_methods
                .is_some_and(|methods| !core::ptr::eq(methods, &raw const ORDINARY_OBJECT_METHODS))
        );
        assert!(object_prototype.internal_set_prototype_of(&vm, None).must());
        let other = Object::create(&vm, realm, None);
        assert!(!object_prototype.internal_set_prototype_of(&vm, Some(other)).must());
        assert!(object_prototype.prototype().is_none());
    }

    #[test]
    fn function_prototype_returns_undefined_and_throw_type_error_throws() {
        let vm = Vm::create();
        let root_execution_context = initialize_realm(&vm);
        let realm = root_execution_context.realm();
        let function_prototype = Value::from_object(realm.function_prototype());
        assert!(function_prototype.is_function() && !function_prototype.is_constructor());
        assert!(call(&vm, function_prototype, Value::NULL, &[Value::from_i32(1)]).must() == Value::UNDEFINED);
        assert_eq!(
            utf8(&realm.function_prototype().name_for_call_stack()),
            "(Function.prototype)"
        );

        let thrower = realm.throw_type_error_function();
        assert!(!thrower.extensible());
        let message = thrown_message(|| call(&vm, Value::from_object(thrower), Value::UNDEFINED, &[]));
        assert!(
            message.contains(
                "creating a TypeError with the message \"Restricted function properties like 'callee', 'caller' and \
                 'arguments' may not be accessed in strict mode\""
            ),
            "{message}"
        );
        let accessor = realm.throw_type_error_accessor();
        assert!(accessor.getter() == Some(thrower) && accessor.setter() == Some(thrower));

        // Function.prototype [ @@hasInstance ] is OrdinaryHasInstance.
        let has_instance = realm
            .function_prototype()
            .get(&vm, &PropertyKey::from(vm.well_known_symbols().has_instance))
            .must();
        let error_constructor = Value::from_object(realm.intrinsics().error_constructor(&vm));
        let type_error = Value::from_object(TypeError::create(&vm, realm));
        assert!(call(&vm, has_instance, error_constructor, &[type_error]).must() == Value::TRUE);
        assert!(call(&vm, has_instance, error_constructor, &[Value::from_i32(1)]).must() == Value::FALSE);
    }
}

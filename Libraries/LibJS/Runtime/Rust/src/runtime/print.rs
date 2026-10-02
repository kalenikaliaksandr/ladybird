/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! Pretty printing of values for js-rust, mirroring Libraries/LibJS/Print.cpp.

use std::collections::HashSet;
use std::io::{self, Write};

use crate::gc::root::MarkedVec;
use crate::interpreter::vm::Vm;
use crate::layout::cell::Gc;
use crate::layout::object::Object;
use crate::layout::value::Value;
use crate::runtime::array::Array;
use crate::runtime::async_generator::AsyncGenerator;
use crate::runtime::boolean_object::BooleanObject;
use crate::runtime::ecmascript_function_object::EcmascriptFunctionObject;
use crate::runtime::error::Error;
use crate::runtime::generator_object::GeneratorObject;
use crate::runtime::map::Map;
use crate::runtime::native_function::NativeFunction;
use crate::runtime::number_object::NumberObject;
use crate::runtime::promise::{Promise, PromiseState};
use crate::runtime::property_key::PropertyKey;
use crate::runtime::proxy_object::ProxyObject;
use crate::runtime::set::Set;
use crate::runtime::shared_function_instance_data::FunctionKind;
use crate::runtime::string_object::StringObject;
use crate::runtime::weak_map::WeakMap;
use crate::runtime::weak_ref::WeakRef;
use crate::runtime::weak_set::WeakSet;
use crate::utf16::Utf16View;

/// Where and how to print. A Vec<u8> stream stands in for the StringBuilder C++ can print into.
pub struct PrintContext<'a> {
    pub vm: &'a Vm,
    pub stream: &'a mut dyn Write,
    pub strip_ansi: bool,
    pub raw_strings: bool,
}

fn escape_for_string_literal(string: Utf16View<'_>) -> Vec<u16> {
    let mut builder = Vec::with_capacity(string.length_in_code_units());
    for code_unit in string.code_units() {
        let escape = match code_unit {
            0x0D => b'r',
            0x0B => b'v',
            0x0C => b'f',
            0x08 => b'b',
            0x0A => b'n',
            0x5C => b'\\',
            _ => {
                builder.push(code_unit);
                continue;
            }
        };
        builder.push(u16::from(b'\\'));
        builder.push(u16::from(escape));
    }
    builder
}

/// The GC::RootHashTable<GC::Ref<JS::Object>> of the objects printed so far. The objects stay rooted while printing
/// runs getters, so that no object allocated meanwhile can take the address of one that was printed.
struct SeenObjects<'vm> {
    objects: MarkedVec<'vm, Gc<Object>>,
    addresses: HashSet<usize>,
}

impl<'vm> SeenObjects<'vm> {
    fn new(vm: &'vm Vm) -> Self {
        Self {
            objects: MarkedVec::new(vm),
            addresses: HashSet::new(),
        }
    }

    fn contains(&self, object: Gc<Object>) -> bool {
        self.addresses.contains(&object.as_ptr().addr())
    }

    fn set(&mut self, object: Gc<Object>) {
        if self.addresses.insert(object.as_ptr().addr()) {
            self.objects.push(object);
        }
    }

    fn size(&self) -> usize {
        self.addresses.len()
    }
}

fn strip_ansi(format_string: &str) -> Vec<u8> {
    let format_string = format_string.as_bytes();
    if format_string.is_empty() {
        return Vec::new();
    }

    let mut builder = Vec::with_capacity(format_string.len());
    let mut i = 0;
    while i < format_string.len() - 1 {
        if format_string[i] == 0x1B && format_string[i + 1] == b'[' {
            while i < format_string.len() && format_string[i] != b'm' {
                i += 1;
            }
        } else {
            builder.push(format_string[i]);
        }
        i += 1;
    }
    if i < format_string.len() {
        builder.push(format_string[i]);
    }
    builder
}

/// js_out() with a format string that has no arguments. Like C++, stripping ANSI colors only applies to the format
/// string, so the arguments of a format string are written with the functions below rather than through this.
fn js_out(print_context: &mut PrintContext<'_>, format_string: &str) -> io::Result<()> {
    if print_context.strip_ansi {
        return print_context.stream.write_all(&strip_ansi(format_string));
    }
    print_context.stream.write_all(format_string.as_bytes())
}

fn js_out_argument(print_context: &mut PrintContext<'_>, argument: &str) -> io::Result<()> {
    print_context.stream.write_all(argument.as_bytes())
}

fn js_out_utf16_argument(print_context: &mut PrintContext<'_>, argument: Utf16View<'_>) -> io::Result<()> {
    print_context.stream.write_all(&argument.to_wtf8())
}

fn print_type(print_context: &mut PrintContext<'_>, name: &str) -> io::Result<()> {
    js_out(print_context, "[\x1b[36;1m")?;
    js_out_argument(print_context, name)?;
    js_out(print_context, "\x1b[0m]")
}

fn print_utf16_type(print_context: &mut PrintContext<'_>, name: Utf16View<'_>) -> io::Result<()> {
    js_out(print_context, "[\x1b[36;1m")?;
    js_out_utf16_argument(print_context, name)?;
    js_out(print_context, "\x1b[0m]")
}

fn print_separator(print_context: &mut PrintContext<'_>, first: &mut bool) -> io::Result<()> {
    js_out_argument(print_context, if *first { " " } else { ", " })?;
    *first = false;
    Ok(())
}

fn print_array(
    print_context: &mut PrintContext<'_>,
    array: Gc<Object>,
    seen_objects: &mut SeenObjects<'_>,
) -> io::Result<()> {
    js_out(print_context, "[")?;
    let mut first = true;
    let mut printed_count: usize = 0;
    let mut i: u32 = 0;
    while i < array.indexed_array_like_size() {
        if !array.indexed_has(i) {
            i += 1;
            continue;
        }
        print_separator(print_context, &mut first)?;
        let value_or_error = array.get(print_context.vm, &PropertyKey::from(i));
        // The V8 repl doesn't throw an exception here, and instead just
        // prints 'undefined'. We may choose to replicate that behavior in
        // the future, but for now lets just catch the error
        let Ok(value) = value_or_error else {
            return Ok(());
        };
        print_value(print_context, value, seen_objects)?;
        printed_count += 1;
        if printed_count > 100 && i + 1 < array.indexed_array_like_size() {
            js_out(print_context, ", ...")?;
            break;
        }
        i += 1;
    }
    if !first {
        js_out(print_context, " ")?;
    }
    js_out(print_context, "]")
}

fn print_object(
    print_context: &mut PrintContext<'_>,
    object: Gc<Object>,
    seen_objects: &mut SeenObjects<'_>,
) -> io::Result<()> {
    js_out_argument(print_context, object.class().class_name())?;
    js_out(print_context, "{")?;
    let mut first = true;
    const MAX_NUMBER_OF_NEW_OBJECTS: usize = 20; // Arbitrary limit
    let original_num_seen_objects = seen_objects.size();

    let vm = print_context.vm;
    let maybe_completion = object.enumerate_object_properties(vm, |property_key| -> Option<()> {
        // The V8 repl doesn't throw an exception on accessing properties, and instead just
        // prints 'undefined'. We may choose to replicate that behavior in
        // the future, but for now lets just catch the error
        if print_separator(print_context, &mut first).is_err() {
            return Some(());
        }
        if js_out(print_context, "\x1b[33;1m").is_err() {
            return Some(());
        }
        // NOTE: Ignore this error to always print out "reset" ANSI sequence
        let _ = print_value(print_context, property_key, seen_objects);
        if js_out(print_context, "\x1b[0m: ").is_err() {
            return Some(());
        }
        let Ok(maybe_property_key) = PropertyKey::from_value(vm, property_key) else {
            return Some(());
        };
        let Ok(value) = object.get(vm, &maybe_property_key) else {
            return Some(());
        };
        let error = print_value(print_context, value, seen_objects);
        // FIXME: Come up with a better way to structure the data so that we don't care about this limit
        if seen_objects.size() > original_num_seen_objects + MAX_NUMBER_OF_NEW_OBJECTS {
            return Some(()); // Stop once we've seen a ton of objects, to prevent spamming the console.
        }
        if error.is_err() {
            return Some(());
        }
        None
    });
    // Swallow Error/undefined from printing properties
    if !matches!(maybe_completion, Ok(None)) {
        return Ok(());
    }

    if !first {
        js_out(print_context, " ")?;
    }
    js_out(print_context, "}")
}

fn print_function(print_context: &mut PrintContext<'_>, function_object: Gc<Object>) -> io::Result<()> {
    let ecmascript_function_object = function_object.downcast::<EcmascriptFunctionObject>();
    if let Some(ecmascript_function_object) = ecmascript_function_object {
        match ecmascript_function_object.kind() {
            FunctionKind::Normal => print_type(print_context, "Function")?,
            FunctionKind::Generator => print_type(print_context, "GeneratorFunction")?,
            FunctionKind::Async => print_type(print_context, "AsyncFunction")?,
            FunctionKind::AsyncGenerator => print_type(print_context, "AsyncGeneratorFunction")?,
        }
    } else {
        print_type(print_context, function_object.class().class_name())?;
    }
    if let Some(ecmascript_function_object) = ecmascript_function_object {
        js_out(print_context, " ")?;
        js_out_utf16_argument(
            print_context,
            Utf16View::of_fly_string(&ecmascript_function_object.name()),
        )?;
    } else if let Some(native_function) = function_object.downcast::<NativeFunction>() {
        js_out(print_context, " ")?;
        js_out_utf16_argument(print_context, Utf16View::of_fly_string(&native_function.name()))?;
    }
    Ok(())
}

fn print_error(
    print_context: &mut PrintContext<'_>,
    object: Gc<Object>,
    seen_objects: &mut SeenObjects<'_>,
) -> io::Result<()> {
    let vm = print_context.vm;
    let name = object.get_without_side_effects(vm, &vm.names.name);
    let message = object.get_without_side_effects(vm, &vm.names.message);
    if name.is_accessor() || message.is_accessor() {
        // NB: The object is among the seen objects already, so this prints it as an already printed object.
        print_value(print_context, Value::from_object(object), seen_objects)?;
    } else {
        let name_string = name.to_utf16_string_without_side_effects();
        let message_string = message.to_utf16_string_without_side_effects();
        print_utf16_type(print_context, Utf16View::of_string(&name_string))?;
        if !Utf16View::of_string(&message_string).is_empty() {
            js_out(print_context, " \x1b[31;1m")?;
            js_out_utf16_argument(print_context, Utf16View::of_string(&message_string))?;
            js_out(print_context, "\x1b[0m")?;
        }
    }
    Ok(())
}

fn print_map(print_context: &mut PrintContext<'_>, map: Gc<Map>, seen_objects: &mut SeenObjects<'_>) -> io::Result<()> {
    print_type(print_context, "Map")?;
    js_out(print_context, " {")?;
    let mut first = true;
    let iterator = map.begin();
    while !iterator.is_end() {
        let entry = iterator.current();
        print_separator(print_context, &mut first)?;
        print_value(print_context, entry.key, seen_objects)?;
        js_out(print_context, " => ")?;
        print_value(print_context, entry.value, seen_objects)?;
        iterator.advance();
    }
    if !first {
        js_out(print_context, " ")?;
    }
    js_out(print_context, "}")
}

fn print_set(print_context: &mut PrintContext<'_>, set: Gc<Set>, seen_objects: &mut SeenObjects<'_>) -> io::Result<()> {
    print_type(print_context, "Set")?;
    js_out(print_context, " {")?;
    let mut first = true;
    let iterator = set.begin();
    while !iterator.is_end() {
        let value = iterator.current();
        print_separator(print_context, &mut first)?;
        print_value(print_context, value, seen_objects)?;
        iterator.advance();
    }
    if !first {
        js_out(print_context, " ")?;
    }
    js_out(print_context, "}")
}

fn print_weak_map(print_context: &mut PrintContext<'_>, weak_map: Gc<WeakMap>) -> io::Result<()> {
    print_type(print_context, "WeakMap")?;
    js_out(print_context, " (")?;
    js_out_argument(print_context, &weak_map.weak_map_size().to_string())?;
    // Note: We could tell you what's actually inside, but not in insertion order.
    js_out(print_context, ")")
}

fn print_weak_set(print_context: &mut PrintContext<'_>, weak_set: Gc<WeakSet>) -> io::Result<()> {
    print_type(print_context, "WeakSet")?;
    js_out(print_context, " (")?;
    js_out_argument(print_context, &weak_set.weak_set_size().to_string())?;
    // Note: We could tell you what's actually inside, but not in insertion order.
    js_out(print_context, ")")
}

fn print_weak_ref(
    print_context: &mut PrintContext<'_>,
    weak_ref: Gc<WeakRef>,
    seen_objects: &mut SeenObjects<'_>,
) -> io::Result<()> {
    print_type(print_context, "WeakRef")?;
    js_out(print_context, " ")?;
    let value = weak_ref.value();
    print_value(
        print_context,
        if value.is_empty() { Value::UNDEFINED } else { value },
        seen_objects,
    )
}

fn print_boolean_object(
    print_context: &mut PrintContext<'_>,
    boolean_object: Gc<BooleanObject>,
    seen_objects: &mut SeenObjects<'_>,
) -> io::Result<()> {
    print_type(print_context, "Boolean")?;
    js_out(print_context, " ")?;
    print_value(print_context, Value::from_bool(boolean_object.boolean()), seen_objects)
}

fn print_number_object(
    print_context: &mut PrintContext<'_>,
    number_object: Gc<NumberObject>,
    seen_objects: &mut SeenObjects<'_>,
) -> io::Result<()> {
    print_type(print_context, "Number")?;
    js_out(print_context, " ")?;
    print_value(print_context, Value::from_f64(number_object.number()), seen_objects)
}

fn print_promise(
    print_context: &mut PrintContext<'_>,
    promise: Gc<Promise>,
    seen_objects: &mut SeenObjects<'_>,
) -> io::Result<()> {
    print_type(print_context, "Promise")?;
    match promise.state() {
        PromiseState::Pending => {
            js_out(print_context, "\n  state: ")?;
            js_out(print_context, "\x1b[36;1mPending\x1b[0m")?;
        }
        PromiseState::Fulfilled => {
            js_out(print_context, "\n  state: ")?;
            js_out(print_context, "\x1b[32;1mFulfilled\x1b[0m")?;
            js_out(print_context, "\n  result: ")?;
            print_value(print_context, promise.result(), seen_objects)?;
        }
        PromiseState::Rejected => {
            js_out(print_context, "\n  state: ")?;
            js_out(print_context, "\x1b[31;1mRejected\x1b[0m")?;
            js_out(print_context, "\n  result: ")?;
            print_value(print_context, promise.result(), seen_objects)?;
        }
    }
    Ok(())
}

fn print_proxy_object(
    print_context: &mut PrintContext<'_>,
    proxy_object: Gc<ProxyObject>,
    seen_objects: &mut SeenObjects<'_>,
) -> io::Result<()> {
    print_type(print_context, "Proxy")?;
    js_out(print_context, "\n  target: ")?;
    print_value(print_context, Value::from_object(proxy_object.target()), seen_objects)?;
    js_out(print_context, "\n  handler: ")?;
    print_value(print_context, Value::from_object(proxy_object.handler()), seen_objects)
}

fn print_generator(print_context: &mut PrintContext<'_>, generator: Gc<Object>) -> io::Result<()> {
    print_type(print_context, generator.class().class_name())
}

fn print_async_generator(print_context: &mut PrintContext<'_>, generator: Gc<Object>) -> io::Result<()> {
    print_type(print_context, generator.class().class_name())
}

fn is_error_object(object: Gc<Object>) -> bool {
    object.is::<Error>()
}

/// &prototype == prototype.shape().realm().intrinsics().error_prototype(): whether `prototype` is the %Error.prototype%
/// of the realm it was made in.
fn is_error_prototype_of_its_realm(vm: &Vm, prototype: Gc<Object>) -> bool {
    prototype == prototype.shape().realm().intrinsics().error_prototype(vm)
}

fn print_string_object(
    print_context: &mut PrintContext<'_>,
    string_object: Gc<StringObject>,
    seen_objects: &mut SeenObjects<'_>,
) -> io::Result<()> {
    print_type(print_context, "String")?;
    js_out(print_context, " ")?;
    print_value(
        print_context,
        Value::from_string(string_object.primitive_string()),
        seen_objects,
    )
}

fn print_value(
    print_context: &mut PrintContext<'_>,
    value: Value,
    seen_objects: &mut SeenObjects<'_>,
) -> io::Result<()> {
    if value.is_empty() {
        js_out(print_context, "\x1b[34;1m<empty>\x1b[0m")?;
        return Ok(());
    }

    if value.is_object() {
        if seen_objects.contains(value.as_object()) {
            // FIXME: Maybe we should only do this for circular references,
            //        not for all reoccurring objects.
            js_out(print_context, "<already printed Object ")?;
            js_out_argument(print_context, &format!("{:#018x}", value.as_object().as_ptr().addr()))?;
            js_out(print_context, ">")?;
            return Ok(());
        }
        seen_objects.set(value.as_object());
    }

    if value.is_object() {
        let object = value.as_object();
        if object.is::<Array>() {
            return print_array(print_context, object, seen_objects);
        }
        if object.is_function() {
            return print_function(print_context, object);
        }
        // NB: Date objects are printed by print_date() here, before errors, once the runtime has them.
        if is_error_object(object) {
            return print_error(print_context, object, seen_objects);
        }

        let prototype_or_error = object.internal_get_prototype_of(print_context.vm);
        if let Ok(Some(prototype)) = prototype_or_error
            && is_error_prototype_of_its_realm(print_context.vm, prototype)
        {
            return print_error(print_context, object, seen_objects);
        }

        // NB: Print.cpp goes on to check for the classes the runtime does not have yet, which their units add here in
        //     this order, each printed by the function Print.cpp names after it: RegExpObject here, Map, Set,
        //     WeakMap, WeakSet and WeakRef below, then DataView, ProxyObject and Promise after them.
        if let Some(map) = object.downcast::<Map>() {
            return print_map(print_context, map, seen_objects);
        }
        if let Some(set) = object.downcast::<Set>() {
            return print_set(print_context, set, seen_objects);
        }
        if let Some(weak_map) = object.downcast::<WeakMap>() {
            return print_weak_map(print_context, weak_map);
        }
        if let Some(weak_set) = object.downcast::<WeakSet>() {
            return print_weak_set(print_context, weak_set);
        }
        if let Some(weak_ref) = object.downcast::<WeakRef>() {
            return print_weak_ref(print_context, weak_ref, seen_objects);
        }
        if let Some(proxy_object) = object.downcast::<ProxyObject>() {
            return print_proxy_object(print_context, proxy_object, seen_objects);
        }
        if let Some(promise) = object.downcast::<Promise>() {
            return print_promise(print_context, promise, seen_objects);
        }
        // NB: After Promise, Print.cpp checks for ArrayBuffer, then GeneratorObject below.
        if object.is::<GeneratorObject>() {
            return print_generator(print_context, object);
        }
        if object.is::<AsyncGenerator>() {
            return print_async_generator(print_context, object);
        }
        // NB: After AsyncGenerator, Print.cpp checks for the typed arrays (object.is_typed_array()), then
        //     BooleanObject, NumberObject and StringObject below.
        if let Some(boolean_object) = object.downcast::<BooleanObject>() {
            return print_boolean_object(print_context, boolean_object, seen_objects);
        }
        if let Some(number_object) = object.downcast::<NumberObject>() {
            return print_number_object(print_context, number_object, seen_objects);
        }
        if let Some(string_object) = object.downcast::<StringObject>() {
            return print_string_object(print_context, string_object, seen_objects);
        }
        // NB: Print.cpp then checks for Intl.DisplayNames, Intl.Locale, Intl.ListFormat, Intl.NumberFormat,
        //     Intl.DateTimeFormat, Intl.RelativeTimeFormat, Intl.PluralRules, Intl.Collator, Intl.Segmenter, Segments,
        //     Intl.DurationFormat, and Temporal.Duration, Temporal.Instant, Temporal.PlainDate, Temporal.PlainDateTime,
        //     Temporal.PlainMonthDay, Temporal.PlainTime, Temporal.PlainYearMonth and Temporal.ZonedDateTime.
        //     Everything else is printed as an ordinary object.
        return print_object(print_context, object, seen_objects);
    }

    if value.is_string() {
        js_out(print_context, "\x1b[32;1m")?;
    } else if value.is_number() || value.is_bigint() {
        js_out(print_context, "\x1b[35;1m")?;
    } else if value.is_boolean() || value.is_null() {
        js_out(print_context, "\x1b[33;1m")?;
    } else if value.is_undefined() {
        js_out(print_context, "\x1b[34;1m")?;
    }

    if value.is_string() && !print_context.raw_strings {
        js_out(print_context, "\"")?;
    } else if value.is_negative_zero() {
        js_out(print_context, "-")?;
    }

    let contents = value.to_utf16_string_without_side_effects();
    if value.is_string() && !print_context.raw_strings {
        let escaped = escape_for_string_literal(Utf16View::of_string(&contents));
        js_out_utf16_argument(print_context, Utf16View::Utf16(&escaped))?;
    } else {
        js_out_utf16_argument(print_context, Utf16View::of_string(&contents))?;
    }

    if value.is_string() && !print_context.raw_strings {
        js_out(print_context, "\"")?;
    }
    js_out(print_context, "\x1b[0m")
}

pub fn print(value: Value, print_context: &mut PrintContext<'_>) -> io::Result<()> {
    let mut seen_objects = SeenObjects::new(print_context.vm);
    print_value(print_context, value, &mut seen_objects)
}

#[cfg(all(test, libjs_runtime_tests_with_libgc))]
mod tests {
    use super::*;
    use crate::runtime::abstract_operations::call;
    use crate::runtime::bound_function::BoundFunction;
    use crate::runtime::completion::{Must, Throw};
    use crate::runtime::ecmascript_function_object::test_functions::function_from_script;
    use crate::runtime::global_environment::test_global_object::set_up_global_object;
    use crate::runtime::native_function::{RawNativeFunction, raw_native};
    use crate::runtime::property_attributes::{DEFAULT_ATTRIBUTES, PropertyAttributes};
    use crate::runtime::property_descriptor::PropertyDescriptor;
    use crate::runtime::realm::test_realm::{TestRealm, key};
    use crate::runtime::symbol::{Kind, Symbol};
    use crate::script::Script;

    fn printed_bytes(vm: &Vm, value: Value, strip_ansi: bool, raw_strings: bool) -> Vec<u8> {
        let mut stream = Vec::new();
        let mut print_context = PrintContext {
            vm,
            stream: &mut stream,
            strip_ansi,
            raw_strings,
        };
        print(value, &mut print_context).expect("printing into a buffer succeeds");
        stream
    }

    fn printed_with(vm: &Vm, value: Value, strip_ansi: bool, raw_strings: bool) -> String {
        String::from_utf8(printed_bytes(vm, value, strip_ansi, raw_strings)).expect("the printed value is UTF-8")
    }

    fn printed(vm: &Vm, value: Value) -> String {
        printed_with(vm, value, true, false)
    }

    /// The printed text with the address of each already printed object replaced, since addresses differ between
    /// runs and between the runtimes.
    fn without_addresses(text: &str) -> String {
        let mut result = String::new();
        let mut rest = text;
        while let Some(start) = rest.find("<already printed Object 0x") {
            let address_start = start + "<already printed Object ".len();
            result.push_str(&rest[..address_start]);
            let address_length = rest[address_start..].find('>').expect("the address ends with >");
            assert_eq!(address_length, 18, "the address is printed as 0x and 16 hex digits");
            result.push_str("ADDRESS");
            rest = &rest[address_start + address_length..];
        }
        result.push_str(rest);
        result
    }

    struct Session<'vm> {
        vm: &'vm Vm,
        test_realm: TestRealm<'vm>,
        global: Gc<Object>,
    }

    impl<'vm> Session<'vm> {
        fn new(vm: &'vm Vm) -> Self {
            let test_realm = TestRealm::with_function_intrinsics(vm);
            let global = set_up_global_object(vm, test_realm.realm);
            Self { vm, test_realm, global }
        }

        fn evaluate(&self, source: &str) -> Value {
            let source: Vec<u16> = source.encode_utf16().collect();
            let script = Script::parse(self.vm, &source, self.test_realm.realm).expect("the script parses");
            self.vm.run_script(script, None).must()
        }

        fn define_global(&self, name: &str, value: Value) {
            self.global
                .define_direct_property(self.vm, &key(name), value, DEFAULT_ATTRIBUTES);
        }

        fn function(&self, source: &str) -> Value {
            Value::from_object(function_from_script(self.vm, self.test_realm.realm, source, 0))
        }

        fn array(&self, elements: &[Value]) -> Gc<Object> {
            self.test_realm.array(elements).upcast()
        }

        /// [0, 1, ..., count - 1]
        fn integers(&self, count: i32) -> Gc<Object> {
            let array = self.array(&[]);
            for index in 0..count {
                let property_key = PropertyKey::from(u32::try_from(index).expect("the index is not negative"));
                array
                    .create_data_property_or_throw(self.vm, &property_key, Value::from_i32(index))
                    .must();
            }
            array
        }
    }

    /// Scripts the C++ js runs with -i -l, and what it prints for them. The scripts only use what the test realm has.
    const OBJECT_LITERALS: &[(&str, &str)] = &[
        (
            "({a: 1, b: \"x\", c: {d: null}, e: undefined, f: true, g: -0, h: 1.5, i: 10n, j: 0/0, k: -1/0})",
            "Object{ \"a\": 1, \"b\": \"x\", \"c\": Object{ \"d\": null }, \"e\": undefined, \"f\": true, \"g\": -0, \
             \"h\": 1.5, \"i\": 10n, \"j\": NaN, \"k\": -Infinity }",
        ),
        (
            "({a1: {}, a2: {}, a3: {}, a4: {}, a5: {}, a6: {}, a7: {}, a8: {}, a9: {}, a10: {}, a11: {}, a12: {}, \
             a13: {}, a14: {}, a15: {}, a16: {}, a17: {}, a18: {}, a19: {}, a20: {}, a21: {}, a22: {}, a23: {}, \
             a24: {}, a25: {}})",
            "Object{ \"a1\": Object{}, \"a2\": Object{}, \"a3\": Object{}, \"a4\": Object{}, \"a5\": Object{}, \
             \"a6\": Object{}, \"a7\": Object{}, \"a8\": Object{}, \"a9\": Object{}, \"a10\": Object{}, \
             \"a11\": Object{}, \"a12\": Object{}, \"a13\": Object{}, \"a14\": Object{}, \"a15\": Object{}, \
             \"a16\": Object{}, \"a17\": Object{}, \"a18\": Object{}, \"a19\": Object{}, \"a20\": Object{}, \
             \"a21\": Object{}",
        ),
        (
            "({a: {b: {c: {d: {e: {f: {g: {h: {i: {j: {k: {l: {m: {n: {o: {p: {q: {r: {s: {t: {u: {v: {w: {}}}}}}}}}}}}}}}}}}}}}}}, z: 1})",
            "Object{ \"a\": Object{ \"b\": Object{ \"c\": Object{ \"d\": Object{ \"e\": Object{ \"f\": Object{ \
             \"g\": Object{ \"h\": Object{ \"i\": Object{ \"j\": Object{ \"k\": Object{ \"l\": Object{ \"m\": Object{ \
             \"n\": Object{ \"o\": Object{ \"p\": Object{ \"q\": Object{ \"r\": Object{ \"s\": Object{ \"t\": Object{ \
             \"u\": Object{ \"v\": Object{ \"w\": Object{} } } } } } } } } } } } } } } } } } } } }",
        ),
        (
            "var o = {}; ({x: o, y: o})",
            "Object{ \"x\": Object{}, \"y\": <already printed Object ADDRESS> }",
        ),
        (
            "var o = {}; o.self = o; o",
            "Object{ \"self\": <already printed Object ADDRESS> }",
        ),
        (
            "({b: 1, 2: 2, a: 3, 1: 4})",
            "Object{ \"1\": 4, \"2\": 2, \"b\": 1, \"a\": 3 }",
        ),
        (
            "({__proto__: {inherited: 1, own: 0}, own: 2})",
            "Object{ \"own\": 2, \"inherited\": 1 }",
        ),
        (
            "({name: \"E\", message: \"m\"})",
            "Object{ \"name\": \"E\", \"message\": \"m\" }",
        ),
        (
            "({\"key with \\\"quote\\\"\\n\": 1})",
            "Object{ \"key with \"quote\"\\n\": 1 }",
        ),
        (
            "\"a\\r\\v\\f\\b\\n\\\\\\\"\\t\\u2028z\"",
            "\"a\\r\\v\\f\\b\\n\\\\\"\t\u{2028}z\"",
        ),
        ("({})", "Object{}"),
        ("-0", "-0"),
        ("10n", "10n"),
    ];

    fn print_object_literals(collect_on_every_allocation: bool) {
        let vm = Vm::create();
        let session = Session::new(&vm);
        vm.heap()
            .set_should_collect_on_every_allocation(collect_on_every_allocation);
        for &(source, expected) in OBJECT_LITERALS {
            assert_eq!(
                without_addresses(&printed(&vm, session.evaluate(source))),
                expected,
                "printing {source}"
            );
        }
        vm.heap().set_should_collect_on_every_allocation(false);
    }

    #[test]
    fn object_literals_print_like_the_cpp_js_when_collecting_on_every_allocation() {
        print_object_literals(true);
    }

    #[test]
    fn objects_print_like_the_cpp_js() {
        print_object_literals(false);

        let vm = Vm::create();
        let session = Session::new(&vm);

        let object = session.evaluate("({v: 1})").as_object();
        object.define_direct_property(&vm, &key("hidden"), Value::from_i32(1), PropertyAttributes::new(0));
        // Object.defineProperty({v: 1}, "hidden", {value: 1})
        assert_eq!(printed(&vm, Value::from_object(object)), "Object{ \"v\": 1 }");
        // Object.create(null)
        let without_prototype = Object::create(&vm, session.test_realm.realm, None);
        assert_eq!(printed(&vm, Value::from_object(without_prototype)), "Object{}");

        let reoccurring = session.evaluate("var o = {}; ({x: o, y: o})");
        let address = reoccurring
            .as_object()
            .get(&vm, &key("x"))
            .must()
            .as_object()
            .as_ptr()
            .addr();
        assert!(printed(&vm, reoccurring).ends_with(&format!("<already printed Object {address:#018x}> }}")));
    }

    #[test]
    fn arrays_print_like_the_cpp_js() {
        let vm = Vm::create();
        let session = Session::new(&vm);
        let int = Value::from_i32;

        // var a = [1,,3]; a.x = 4; a
        let holey = session.array(&[int(1), int(2), int(3)]);
        holey.delete_property_or_throw(&vm, &PropertyKey::from(1u32)).must();
        holey.define_direct_property(&vm, &key("x"), int(4), DEFAULT_ATTRIBUTES);
        assert_eq!(printed(&vm, Value::from_object(holey)), "[ 1, 3 ]");

        // [[], [[]]]
        let empty = session.array(&[]);
        let nested = session.array(&[Value::from_object(session.array(&[]))]);
        let arrays = session.array(&[Value::from_object(empty), Value::from_object(nested)]);
        assert_eq!(printed(&vm, Value::from_object(arrays)), "[ [], [ [] ] ]");
        assert_eq!(printed(&vm, Value::from_object(session.array(&[]))), "[]");

        // Array.from({length: 101}, (_, i) => i) and the same with 102 elements.
        let all_numbers = (0..=100).map(|i| i.to_string()).collect::<Vec<_>>().join(", ");
        assert_eq!(
            printed(&vm, Value::from_object(session.integers(101))),
            format!("[ {all_numbers} ]")
        );
        assert_eq!(
            printed(&vm, Value::from_object(session.integers(102))),
            format!("[ {all_numbers}, ... ]")
        );

        // [undefined, null, , 0]
        let sparse = session.array(&[Value::UNDEFINED, Value::NULL, Value::UNDEFINED, int(0)]);
        sparse.delete_property_or_throw(&vm, &PropertyKey::from(2u32)).must();
        assert_eq!(printed(&vm, Value::from_object(sparse)), "[ undefined, null, 0 ]");

        // var o = {}; [o, [o]]
        let object = Value::from_object(session.test_realm.object());
        let repeated = session.array(&[object, Value::from_object(session.array(&[object]))]);
        assert_eq!(
            without_addresses(&printed(&vm, Value::from_object(repeated))),
            "[ Object{}, [ <already printed Object ADDRESS> ] ]"
        );

        // var a = [1]; Object.defineProperty(a, 1, {get(){ throw 1 }}); a.push(3); a
        let throwing = session.array(&[int(1)]);
        let getter = RawNativeFunction::create(
            &vm,
            raw_native!(|_| Err(Throw::new(Value::from_i32(1)))),
            0,
            &key("get"),
            None,
            None,
            None,
        );
        throwing
            .define_property_or_throw(
                &vm,
                &PropertyKey::from(1u32),
                &mut PropertyDescriptor {
                    get: Some(Some(getter.upcast())),
                    ..Default::default()
                },
            )
            .must();
        throwing
            .set(
                &vm,
                &PropertyKey::from(2u32),
                int(3),
                crate::runtime::object::ShouldThrowExceptions::Yes,
            )
            .must();
        assert_eq!(printed(&vm, Value::from_object(throwing)), "[ 1, ");
    }

    #[test]
    fn functions_print_their_kind_and_name_like_the_cpp_js() {
        let vm = Vm::create();
        let session = Session::new(&vm);
        let realm = session.test_realm.realm;

        // [function foo(){}, () => 1, function* g(){}, async function a(){}, async function* ag(){}]
        let functions = session.array(&[
            session.function("(function foo(){})"),
            session.function("(() => 1)"),
            session.function("(function* g(){})"),
            session.function("(async function a(){})"),
            session.function("(async function* ag(){})"),
        ]);
        assert_eq!(
            printed(&vm, Value::from_object(functions)),
            "[ [Function] foo, [Function] , [GeneratorFunction] g, [AsyncFunction] a, [AsyncGeneratorFunction] ag ]"
        );

        // [Math.max, (function(){}).bind(null), (function f(){}).bind(null)]
        let max = RawNativeFunction::create(
            &vm,
            raw_native!(|_| Ok(Value::UNDEFINED)),
            2,
            &key("max"),
            None,
            None,
            None,
        );
        let bind = |source: &str| {
            let target = session.function(source).as_function();
            Value::from_object(BoundFunction::create(&vm, realm, target, Value::NULL, &[]).must())
        };
        let natives = session.array(&[
            Value::from_object(max),
            bind("(function(){})"),
            bind("(function f(){})"),
        ]);
        assert_eq!(
            printed(&vm, Value::from_object(natives)),
            "[ [RawNativeFunction] , [BoundFunction], [BoundFunction] ]"
        );

        // ({f: function f(){}, g: function(){}}), whose second function is named g where it is created.
        session.define_global("f", session.function("(function f(){})"));
        session.define_global("g", session.function("(function g(){})"));
        assert_eq!(
            printed(&vm, session.evaluate("({f: f, g: g})")),
            "Object{ \"f\": [Function] f, \"g\": [Function] g }"
        );

        // (function(){return arguments})(1,2) and its strict form.
        let arguments_of =
            |source: &str, arguments: &[Value]| call(&vm, session.function(source), Value::UNDEFINED, arguments).must();
        let mapped = arguments_of(
            "(function(){return arguments})",
            &[Value::from_i32(1), Value::from_i32(2)],
        );
        assert_eq!(printed(&vm, mapped), "ArgumentsObject{ \"0\": 1, \"1\": 2 }");
        let x = session.evaluate("\"x\"");
        let unmapped = arguments_of(
            "(function(){\"use strict\"; return arguments})",
            &[Value::from_i32(1), x],
        );
        assert_eq!(printed(&vm, unmapped), "Object{ \"0\": 1, \"1\": \"x\" }");
    }

    #[test]
    fn getters_run_while_printing_and_their_throws_end_it_like_the_cpp_js() {
        let vm = Vm::create();
        let session = Session::new(&vm);
        let realm = session.test_realm.realm;

        // ({get x(){ return 5 }, y: 2})
        let object = session.test_realm.object();
        object.define_native_accessor(
            &vm,
            realm,
            &key("x"),
            raw_native!(|_| Ok(Value::from_i32(5))),
            None,
            DEFAULT_ATTRIBUTES,
        );
        object.define_direct_property(&vm, &key("y"), Value::from_i32(2), DEFAULT_ATTRIBUTES);
        assert_eq!(printed(&vm, Value::from_object(object)), "Object{ \"x\": 5, \"y\": 2 }");

        // ({get x(){ throw 1 }, y: 2})
        let object = session.test_realm.object();
        object.define_native_accessor(
            &vm,
            realm,
            &key("x"),
            raw_native!(|_| Err(Throw::new(Value::from_i32(1)))),
            None,
            DEFAULT_ATTRIBUTES,
        );
        object.define_direct_property(&vm, &key("y"), Value::from_i32(2), DEFAULT_ATTRIBUTES);
        assert_eq!(printed(&vm, Value::from_object(object)), "Object{ \"x\": ");
    }

    #[test]
    fn colors_raw_strings_and_symbols_print_like_the_cpp_js() {
        let vm = Vm::create();
        let session = Session::new(&vm);

        // ({a: [1, "s", null, undefined, true, 10n, -0], f: function f(){}, o: Object.create(null)}), without -i.
        let elements = [
            Value::from_i32(1),
            session.evaluate("\"s\""),
            Value::NULL,
            Value::UNDEFINED,
            Value::TRUE,
            session.evaluate("10n"),
            Value::from_f64(-0.0),
        ];
        session.define_global("a", Value::from_object(session.array(&elements)));
        session.define_global("f", session.function("(function f(){})"));
        session.define_global(
            "o",
            Value::from_object(Object::create(&vm, session.test_realm.realm, None)),
        );
        assert_eq!(
            printed_with(&vm, session.evaluate("({a: a, f: f, o: o})"), false, false),
            "Object{ \x1b[33;1m\x1b[32;1m\"a\"\x1b[0m\x1b[0m: [ \x1b[35;1m1\x1b[0m, \x1b[32;1m\"s\"\x1b[0m, \
             \x1b[33;1mnull\x1b[0m, \x1b[34;1mundefined\x1b[0m, \x1b[33;1mtrue\x1b[0m, \x1b[35;1m10n\x1b[0m, \
             \x1b[35;1m-0\x1b[0m ], \x1b[33;1m\x1b[32;1m\"f\"\x1b[0m\x1b[0m: [\x1b[36;1mFunction\x1b[0m] f, \
             \x1b[33;1m\x1b[32;1m\"o\"\x1b[0m\x1b[0m: Object{} }"
        );

        // ({a: "x\ny", b: ["q"]}) and "x\ny", with -r.
        session.define_global("q", Value::from_object(session.array(&[session.evaluate("\"q\"")])));
        assert_eq!(
            printed_with(&vm, session.evaluate("({a: \"x\\ny\", b: q})"), true, true),
            "Object{ a: x\ny, b: [ q ] }"
        );
        assert_eq!(printed_with(&vm, session.evaluate("\"x\\ny\""), true, true), "x\ny");

        // [Symbol("q"), Symbol(), Symbol.iterator]
        let symbols = session.array(&[
            Value::from_symbol(Symbol::create(&vm, Some(ak::Utf16String::from_utf8("q")), Kind::Unique)),
            Value::from_symbol(Symbol::create(&vm, None, Kind::Unique)),
            Value::from_symbol(vm.well_known_symbols().iterator),
        ]);
        assert_eq!(
            printed(&vm, Value::from_object(symbols)),
            "[ Symbol(q), Symbol(), Symbol(Symbol.iterator) ]"
        );

        // "\ud800x" prints its lone surrogate as the three bytes AK writes for it.
        assert_eq!(
            printed_bytes(&vm, session.evaluate("\"\\ud800x\""), true, false),
            b"\"\xED\xA0\x80x\""
        );
    }

    #[test]
    fn printed_objects_stay_alive_while_getters_allocate() {
        let vm = Vm::create();
        let session = Session::new(&vm);
        let realm = session.test_realm.realm;
        vm.heap().set_should_collect_on_every_allocation(true);

        // A getter that drops the object printed before it and allocates new ones, which must not be mistaken for it.
        let object = session.test_realm.object();
        object.define_direct_property(
            &vm,
            &key("a"),
            Value::from_object(session.test_realm.object()),
            DEFAULT_ATTRIBUTES,
        );
        object.define_native_accessor(
            &vm,
            realm,
            &key("b"),
            raw_native!(|vm| {
                let this_object = vm.this_value().as_object();
                this_object.delete_property_or_throw(vm, &key("a"))?;
                let realm = vm.current_realm().expect("the getter runs in a realm");
                let mut last = Object::create(vm, realm, Some(realm.object_prototype()));
                for _ in 0..8 {
                    last = Object::create(vm, realm, Some(realm.object_prototype()));
                }
                Ok(Value::from_object(last))
            }),
            None,
            DEFAULT_ATTRIBUTES,
        );
        for index in 0..9 {
            object.define_direct_property(
                &vm,
                &key(&format!("c{index}")),
                Value::from_object(session.array(&[Value::from_object(session.test_realm.object())])),
                DEFAULT_ATTRIBUTES,
            );
        }
        let expected = format!(
            "Object{{ \"a\": Object{{}}, \"b\": Object{{}}, {} }}",
            (0..9)
                .map(|index| format!("\"c{index}\": [ Object{{}} ]"))
                .collect::<Vec<_>>()
                .join(", ")
        );
        assert_eq!(printed(&vm, Value::from_object(object)), expected);
        vm.heap().set_should_collect_on_every_allocation(false);
    }
}

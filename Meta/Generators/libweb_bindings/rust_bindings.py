# Copyright (c) 2026-present, the Ladybird developers.
#
# SPDX-License-Identifier: BSD-2-Clause

# The bindings of the interfaces that their IDL marks [ImplementedIn=Rust]. Their attributes, operations, constructor
# and interface objects are Rust code in libweb_rust, which this module writes to Bindings/RustBindings.rs, and the
# implementation of each interface is a Rust type in libweb_rust that implements the trait the bindings define for the
# interface. LibWeb's C++ bindings still create the interface object and the interface prototype object of every
# interface and expose them on the globals; for an interface implemented in Rust they call into the Rust bindings to
# initialize them and to construct objects, and they define the class of its wrappers.
#
# A wrapper is a platform object whose host data holds a GC cell of the implementation. Its wrappable slot stays
# empty, so the C++ bindings see a platform object without a C++ implementation object.

from typing import List
from typing import Optional
from typing import TextIO

from Generators.libweb_bindings.context import GenerationContext
from Generators.libweb_bindings.includes import GeneratedIncludes
from Generators.libweb_bindings.overload_resolution import operation_length
from Generators.libweb_bindings.overload_resolution import operation_overload_sets
from Generators.libweb_bindings.overload_resolution import parameter_list_length
from Generators.libweb_bindings.rust_types import UnsupportedByRustBindings
from Generators.libweb_bindings.rust_types import check_type_is_supported
from Generators.libweb_bindings.rust_types import rust_conversion_to_idl_value
from Generators.libweb_bindings.rust_types import rust_default_value
from Generators.libweb_bindings.rust_types import rust_parameter_type
from Generators.libweb_bindings.rust_types import rust_type
from Generators.libweb_bindings.wrappers import wrapper_host_class_flags
from Generators.libweb_bindings.wrappers import wrapper_host_class_hooks
from Generators.libweb_bindings.wrappers import wrapper_name
from Utils.utils import title_case_to_snake_case
from Utils.webidl_parser import Dictionary
from Utils.webidl_parser import Interface
from Utils.webidl_parser import Module

RUST_KEYWORDS = {
    "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern", "false", "fn", "for",
    "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub", "ref", "return", "self", "static",
    "struct", "super", "trait", "true", "type", "unsafe", "use", "where", "while", "abstract", "become", "box", "do",
    "final", "gen", "macro", "override", "priv", "try", "typeof", "unsized", "virtual", "yield",
}  # fmt: skip

# The extended attributes of an interface and of its members that the Rust bindings handle.
SUPPORTED_INTERFACE_EXTENDED_ATTRIBUTES = {"Exposed", "ImplementedIn", "SecureContext", "Experimental"}
SUPPORTED_MEMBER_EXTENDED_ATTRIBUTES = {"NewObject", "SameObject"}

# Names that Rust does not accept as raw identifiers either.
RUST_RESERVED_NAMES = {"self", "Self", "crate", "super"}


def is_implemented_in_rust(interface: Optional[Interface]) -> bool:
    return interface is not None and interface.extended_attributes.get("ImplementedIn") == "Rust"


def rust_identifier(name: str) -> str:
    identifier = title_case_to_snake_case(name).replace("-", "_")
    if identifier in RUST_RESERVED_NAMES:
        return f"{identifier}_"
    if identifier in RUST_KEYWORDS:
        return f"r#{identifier}"
    return identifier


def rust_local_name(name: str) -> str:
    """The local variable that holds the IDL value of a parameter, which never clashes with the generated ones."""
    return f"idl_{rust_identifier(name).removeprefix('r#')}"


def rust_module_name(interface: Interface) -> str:
    return title_case_to_snake_case(interface.name)


def rust_trait_name(interface: Interface) -> str:
    return f"{interface.name}Interface"


def rust_entry_point_name(interface: Interface, entry_point: str) -> str:
    return f"web_rust_{rust_module_name(interface)}_{entry_point}"


def wrapper_host_class_symbol(interface: Interface) -> str:
    return f"web_bindings_{wrapper_name(interface)}_host_class"


def implementation_path(interface: Interface) -> str:
    """The Rust path of the implementation, which follows from the IDL path: Encoding/TextEncoder.idl names
    crate::encoding::text_encoder::TextEncoder."""
    parts = interface.path.parts
    namespace = parts[parts.index("LibWeb") + 1]
    return f"crate::{title_case_to_snake_case(namespace)}::{rust_module_name(interface)}::{interface.name}"


def rust_dictionaries(module: Module) -> List[Dictionary]:
    return list(module.dictionaries) if is_implemented_in_rust(module.interface) else []


def cpp_dictionaries(module: Module) -> List[Dictionary]:
    return [] if is_implemented_in_rust(module.interface) else list(module.dictionaries)


def check_interface_is_supported(interface: Interface, module: Module, context: GenerationContext) -> None:
    """Raises an error that names what the Rust bindings do not handle yet."""

    def fail(message: str) -> None:
        raise UnsupportedByRustBindings(f"{interface.path}: {interface.name} is [ImplementedIn=Rust], but {message}")

    unsupported_attributes = set(interface.extended_attributes) - SUPPORTED_INTERFACE_EXTENDED_ATTRIBUTES
    if unsupported_attributes:
        fail(f"the Rust bindings do not handle its extended attributes {sorted(unsupported_attributes)}")
    if interface.is_namespace or interface.is_callback_interface:
        fail("it is not a regular interface")
    if interface.parent_name:
        fail("it inherits from another interface")
    special_operations = [
        interface.named_property_getter,
        interface.indexed_property_getter,
        interface.named_property_setter,
        interface.indexed_property_setter,
        interface.named_property_deleter,
    ]
    if any(special_operations) or interface.stringifier is not None:
        fail("it has special operations")
    if interface.iterable or interface.async_iterable or interface.maplike or interface.setlike:
        fail("it is iterable or a maplike or setlike")
    if interface.static_attributes or interface.static_operations or interface.constants:
        fail("it has static members or constants")
    if len(interface.constructors) > 1:
        fail("it has more than one constructor")
    if any(constructor.parameters for constructor in interface.constructors):
        fail("its constructor has parameters")
    if module.enumerations or module.callback_functions:
        fail("its module defines enumerations or callback functions")

    for other in context.interfaces.values():
        if other.parent_name == interface.name:
            fail(f"{other.name} inherits from it")

    def check_member_attributes(member_name: str, extended_attributes: dict) -> None:
        unsupported = set(extended_attributes) - SUPPORTED_MEMBER_EXTENDED_ATTRIBUTES
        if unsupported:
            fail(f"the Rust bindings do not handle the extended attributes {sorted(unsupported)} of {member_name}")

    # NB: operation_overload_sets() leaves out the operations marked [FIXME], which C++ defines as unimplemented.
    for operation in interface.regular_operations:
        check_member_attributes(operation.name, operation.extended_attributes)

    def check_dictionaries_are_local(idl_type, what: str) -> None:
        for nested_type in [idl_type, *idl_type.nested_types()]:
            dictionary = context.dictionaries.get(nested_type.name)
            if dictionary is not None and dictionary not in module.dictionaries:
                fail(f"{what} uses {dictionary.name}, a dictionary of another module")

    for attribute in interface.regular_attributes:
        check_member_attributes(attribute.name, attribute.extended_attributes)
        check_dictionaries_are_local(attribute.type, f"{interface.name}.{attribute.name}")
        if not attribute.readonly:
            fail(f"its attribute {attribute.name} is not readonly")
        check_type_is_supported(attribute.type, context, f"{interface.name}.{attribute.name}")

    for name, operations in operation_overload_sets(interface).items():
        if len(operations) > 1:
            fail(f"its operation {name} is overloaded")
        operation = operations[0]
        check_member_attributes(name, operation.extended_attributes)
        what = f"{interface.name}.{name}()"
        check_type_is_supported(operation.return_type, context, what)
        check_dictionaries_are_local(operation.return_type, what)
        for parameter in operation.parameters:
            if parameter.variadic:
                fail(f"{what} has a variadic parameter")
            parameter_what = f"the parameter {parameter.name} of {what}"
            unsupported = set(parameter.extended_attributes) - {"AllowShared", "AllowResizable"}
            if unsupported:
                fail(f"{parameter_what} has the extended attributes {sorted(unsupported)}")
            check_type_is_supported(parameter.type, context, parameter_what)
            check_dictionaries_are_local(parameter.type, parameter_what)
            rust_conversion_to_idl_value(
                parameter.type, parameter.extended_attributes, "value", context, parameter_what
            )
            if parameter.optional and parameter.default_value is not None:
                rust_default_value(parameter, context, parameter_what)

    for dictionary in module.dictionaries:
        if dictionary.parent_name:
            fail(f"its dictionary {dictionary.name} inherits from another dictionary")
        for member in dictionary.members:
            check_type_is_supported(member.type, context, f"{dictionary.name}.{member.name}")
            check_dictionaries_are_local(member.type, f"{dictionary.name}.{member.name}")


def check_rust_definitions_are_not_used_by_cpp(modules: List[Module]) -> None:
    """The interfaces implemented in Rust and their dictionaries only have a Rust representation, so the C++ bindings
    of other interfaces cannot convert them."""
    rust_names = set()
    for module in modules:
        if module.interface is not None and is_implemented_in_rust(module.interface):
            rust_names.add(module.interface.name)
            rust_names.update(dictionary.name for dictionary in module.dictionaries)
    if not rust_names:
        return
    for module in modules:
        if is_implemented_in_rust(module.interface):
            continue
        definitions = []
        if module.interface is not None:
            interface = module.interface
            definitions.extend(attribute.type for attribute in interface.regular_attributes)
            definitions.extend(attribute.type for attribute in interface.static_attributes)
            for operation in interface.regular_operations + interface.static_operations:
                definitions.append(operation.return_type)
                definitions.extend(parameter.type for parameter in operation.parameters)
            for constructor in interface.constructors:
                definitions.extend(parameter.type for parameter in constructor.parameters)
        for dictionary in module.dictionaries:
            if dictionary.parent_name in rust_names:
                raise UnsupportedByRustBindings(
                    f"{module.path}: {dictionary.name} inherits from {dictionary.parent_name}, which is implemented "
                    "in Rust and has no C++ representation"
                )
            definitions.extend(member.type for member in dictionary.members)
        for callback_function in module.callback_functions:
            definitions.append(callback_function.return_type)
            definitions.extend(parameter.type for parameter in callback_function.parameters)
        for idl_type in definitions:
            for nested_type in [idl_type, *idl_type.nested_types()]:
                if nested_type.name in rust_names:
                    raise UnsupportedByRustBindings(
                        f"{module.path}: uses {nested_type.name}, which is implemented in Rust and has no C++ "
                        "representation"
                    )


# C++ side


def write_cpp_declaration(out: TextIO, includes: GeneratedIncludes, interface: Interface) -> None:
    includes.add("LibJS/HostObjectABI.h")
    includes.add("LibJS/Runtime/NativeFunction.h")
    includes.add("LibJS/Runtime/Object.h")
    includes.add("LibWeb/Bindings/InterfaceObject.h")
    out.write(
        f"""// The class of the wrappers of {interface.name}, whose implementation is in Rust.
extern "C" JSHostClass const {wrapper_host_class_symbol(interface)};

struct {interface.constructor_class} {{
public:
    static void initialize(JS::Realm&, JS::NativeFunction&);
    static JS::ThrowCompletionOr<GC::Ref<JS::Object>> construct(JS::HostFunction&, JS::FunctionObject&);
}};

struct {interface.prototype_class} {{
public:
    static void initialize(JS::Realm&, JS::Object&);
}};

"""
    )


def write_cpp_implementation(
    out: TextIO, includes: GeneratedIncludes, context: GenerationContext, interface: Interface
) -> None:
    includes.add("LibJS/HostClassBuilder.h")
    includes.add("LibJS/Runtime/Error.h")
    includes.add("LibJS/Runtime/HostFunction.h")
    includes.add("LibJS/Runtime/Realm.h")
    includes.add("LibWeb/Bindings/PlatformObject.h")
    includes.add_binding(interface.implemented_name)

    initialize_prototype = rust_entry_point_name(interface, "initialize_prototype")
    initialize_constructor = rust_entry_point_name(interface, "initialize_constructor")
    construct = rust_entry_point_name(interface, "construct")
    hooks = wrapper_host_class_hooks(context, interface)
    # NB: Only regular platform objects reach here; the hooks of the other kinds need C++ implementation objects.
    assert hooks == "platform_object_hooks", f"{interface.name} needs the wrapper hooks {hooks}"
    flags = " | ".join(wrapper_host_class_flags(context, interface))
    out.write(
        f"""extern "C" {{

// The entry points of the Rust bindings of {interface.name}.
void {initialize_prototype}(JSVM*, void* realm, JSObject* prototype);
void {initialize_constructor}(JSVM*, void* realm, JSObject* constructor);
"""
    )
    if interface.constructors:
        out.write(f"JSCompletion {construct}(JSVM*, JSObject* constructor, JSObject* new_target);\n")
    out.write(
        f"""
}}

constexpr JSHostClass {wrapper_host_class_symbol(interface)} = JS::make_host_class(JS_HOST_CLASS_OBJECT, "{interface.name}"sv,
    nullptr, &{hooks}, nullptr,
    {flags});

void {interface.prototype_class}::initialize(JS::Realm& realm, JS::Object& object)
{{
    {initialize_prototype}(JS::HostABI::vm_to_abi(realm.vm()), &realm, JS::HostABI::object_to_abi(&object));
}}

void {interface.constructor_class}::initialize(JS::Realm& realm, JS::NativeFunction& object)
{{
    {initialize_constructor}(JS::HostABI::vm_to_abi(realm.vm()), &realm, JS::HostABI::object_to_abi(&object));
}}

JS::ThrowCompletionOr<GC::Ref<JS::Object>> {interface.constructor_class}::construct([[maybe_unused]] JS::HostFunction& constructor, [[maybe_unused]] JS::FunctionObject& new_target)
{{
"""
    )
    if interface.constructors:
        out.write(
            f"""    auto object = TRY(JS::HostABI::completion_from_abi<JS::Value>({construct}(JS::HostABI::vm_to_abi(constructor.vm()), JS::HostABI::object_to_abi(&constructor), JS::HostABI::object_to_abi(&new_target))));
    return GC::Ref {{ object.as_object() }};
"""
        )
    else:
        out.write(
            f'    return constructor.vm().throw_completion<JS::TypeError>(JS::ErrorType::NotAConstructor, "{interface.name}");\n'
        )
    out.write("}\n\n")


def write_ensure_web_prototype_declaration(out: TextIO) -> None:
    out.write(
        """
// The interface prototype object of an interface implemented in Rust in the realm, for the Rust bindings.
extern "C" JSObject* web_bindings_ensure_web_prototype(void* realm, u16 interface_name);
"""
    )


def write_ensure_web_prototype_implementation(out: TextIO, intrinsics: List[Interface]) -> None:
    out.write(
        """
JSObject* web_bindings_ensure_web_prototype(void* realm, u16 interface_name)
{
    [[maybe_unused]] auto& js_realm = *static_cast<JS::Realm*>(realm);
    switch (static_cast<InterfaceName>(interface_name)) {
"""
    )
    for interface in intrinsics:
        if not is_implemented_in_rust(interface):
            continue
        out.write(
            f"""    case InterfaceName::{interface.name}:
        return JS::HostABI::object_to_abi(&ensure_web_prototype<{interface.prototype_class}>(js_realm, "{interface.namespaced_name}"_utf16_fly_string));
"""
        )
    out.write(
        """    default:
        VERIFY_NOT_REACHED();
    }
}
"""
    )


# Rust side


def write_parameter_conversions(out: TextIO, interface: Interface, operation_name: str, parameters, context) -> None:
    for index, parameter in enumerate(parameters):
        what = f"the parameter {parameter.name} of {interface.name}.{operation_name}()"
        name = rust_local_name(parameter.name)
        conversion = rust_conversion_to_idl_value(
            parameter.type, parameter.extended_attributes, f"argument{index}", context, what
        )
        out.write(f"        let argument{index} = frame.argument({index});\n")
        if parameter.optional and parameter.default_value is not None:
            out.write(
                f"""        let {name} = if argument{index}.is_undefined() {{
            {rust_default_value(parameter, context, what)}
        }} else {{
            {conversion}?
        }};
"""
            )
        elif parameter.optional:
            out.write(
                f"""        let {name} = if argument{index}.is_undefined() {{
            None
        }} else {{
            Some({conversion}?)
        }};
"""
            )
        else:
            out.write(f"        let {name} = {conversion}?;\n")


def write_rust_interface(
    out: TextIO, context: GenerationContext, module: Module, interface: Interface, interface_name_value: int
) -> None:
    trait = rust_trait_name(interface)
    out.write(
        f"""
/// The bindings of {interface.name}.
pub(crate) mod {rust_module_name(interface)} {{
    #![allow(unused_imports)]

    use ak::Utf16String;
    use libjs_embedding::abi::{{JSCompletion, JSHostClass, JSObject, JSRealm, JSVM}};
    use libjs_embedding::completion::{{ThrowCompletionOr, into_abi}};
    use libjs_embedding::error::throw_type_error;
    use libjs_embedding::gc::{{CellType, Gc, Trace}};
    use libjs_embedding::layout::*;
    use libjs_embedding::native_function;
    use libjs_embedding::object::Object;
    use libjs_embedding::typed_array::TypedArray;
    use libjs_embedding::value::Value;
    use libjs_embedding::vm::{{TypeErrorRealmScope, Vm}};

    use crate::bindings::conversions::{{
        BufferSourceRules, ToJs, UsvString, to_dom_string, to_typed_array, to_usv_string,
    }};
    use crate::bindings::interface::{{
        create_dictionary_object, create_wrapper, define_attribute, define_interface_object_properties,
        define_operation, define_to_string_tag, entry_point_arguments, implementation_of, prototype_from_new_target,
        set_dictionary_member, set_interface_prototype_object_prototype, this_object,
    }};
    use crate::bindings::messages::{{bad_arg_count_many, bad_arg_count_one}};
    use crate::bindings::{{CallContext, WebIdlResult}};

    use {implementation_path(interface)} as Implementation;

    /// Web::Bindings::InterfaceName::{interface.name}.
    pub(crate) const INTERFACE_NAME: u16 = {interface_name_value};
"""
    )

    for dictionary in module.dictionaries:
        write_rust_dictionary(out, context, dictionary)

    out.write(
        f"""
    /// The attributes and operations of {interface.name}, which its implementation provides.
    pub(crate) trait {trait}: Trace + Sized + 'static {{
"""
    )
    if interface.constructors:
        out.write("        fn construct(cx: &mut CallContext) -> WebIdlResult<Self>;\n")
    for attribute in interface.regular_attributes:
        what = f"{interface.name}.{attribute.name}"
        out.write(
            f"        fn {rust_identifier(attribute.name)}(&self, cx: &mut CallContext) -> "
            f"WebIdlResult<{rust_type(attribute.type, context, what)}>;\n"
        )
    for name, operations in operation_overload_sets(interface).items():
        operation = operations[0]
        what = f"{interface.name}.{name}()"
        parameters = "".join(
            f", {rust_identifier(parameter.name)}: {rust_parameter_type(parameter, context, what)}"
            for parameter in operation.parameters
        )
        out.write(
            f"        fn {rust_identifier(name)}(&self, cx: &mut CallContext{parameters}) -> "
            f"WebIdlResult<{rust_type(operation.return_type, context, what)}>;\n"
        )
    out.write(
        f"""    }}

    static CELL_TYPE: CellType<Implementation> = CellType::new("{interface.name}");

    unsafe extern "C" {{
        static {wrapper_host_class_symbol(interface)}: JSHostClass;
    }}

    fn wrapper_class() -> &'static JSHostClass {{
        // SAFETY: LibWeb defines the class of the wrappers as constant data.
        unsafe {{ &{wrapper_host_class_symbol(interface)} }}
    }}

    /// The implementation that an attribute or an operation runs on, and the context it runs in.
    fn this_implementation(vm: Vm) -> ThrowCompletionOr<(Gc<Implementation>, CallContext)> {{
        let (object, realm) = this_object(vm, wrapper_class(), "{interface.namespaced_name}")?;
        Ok((implementation_of(object, &CELL_TYPE), CallContext::new(vm, realm)))
    }}
"""
    )

    for attribute in interface.regular_attributes:
        getter = f"{rust_identifier(attribute.name).removeprefix('r#')}_getter"
        out.write(
            f"""
    native_function!(fn {getter}(vm) {{
        let (implementation, mut cx) = this_implementation(vm)?;
        let result = {trait}::{rust_identifier(attribute.name)}(&*implementation, &mut cx)
            .map_err(|exception| exception.throw(vm))?;
        result.to_js(&cx)
    }});
"""
        )

    for name, operations in operation_overload_sets(interface).items():
        operation = operations[0]
        function = rust_identifier(name)
        out.write(
            f"""
    native_function!(fn {function.removeprefix("r#")}_function(vm) {{
        let (implementation, mut cx) = this_implementation(vm)?;
"""
        )
        required_argument_count = operation_length(operation)
        if operation.parameters:
            out.write("        let frame = vm.running_frame();\n")
        if required_argument_count == 1:
            out.write(
                f"""        if frame.argument_count() < 1 {{
            return Err(throw_type_error(vm, &bad_arg_count_one("{name}")));
        }}
"""
            )
        elif required_argument_count > 1:
            out.write(
                f"""        if frame.argument_count() < {required_argument_count} {{
            return Err(throw_type_error(vm, &bad_arg_count_many("{name}", {required_argument_count})));
        }}
"""
            )
        if operation.parameters:
            out.write("        let mut type_error_realm_scope = TypeErrorRealmScope::new(vm, cx.realm());\n")
            write_parameter_conversions(out, interface, name, operation.parameters, context)
            out.write("        type_error_realm_scope.restore();\n")
        arguments = "".join(f", {rust_local_name(parameter.name)}" for parameter in operation.parameters)
        out.write(
            f"""        let result = {trait}::{function}(&*implementation, &mut cx{arguments})
            .map_err(|exception| exception.throw(vm))?;
        result.to_js(&cx)
    }});
"""
        )

    out.write(
        f"""
    /// The steps of {interface.prototype_class}::initialize() of LibWeb's C++ bindings.
    ///
    /// # Safety
    ///
    /// The pointers must be the VM, a realm and the interface prototype object of the realm.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn {rust_entry_point_name(interface, "initialize_prototype")}(
        vm: *mut JSVM,
        realm: *mut JSRealm,
        prototype: *mut JSObject,
    ) {{
        // SAFETY: The caller passes live pointers.
        let (vm, realm, prototype) = unsafe {{ entry_point_arguments(vm, realm, prototype) }};
        set_interface_prototype_object_prototype(vm, realm, prototype);
"""
    )
    for attribute in interface.regular_attributes:
        getter = f"{rust_identifier(attribute.name).removeprefix('r#')}_getter"
        out.write(f'        define_attribute(vm, realm, prototype, "{attribute.name}", Some({getter}), None);\n')
    for name, operations in operation_overload_sets(interface).items():
        function = f"{rust_identifier(name).removeprefix('r#')}_function"
        length = operation_length(operations[0])
        out.write(f'        define_operation(vm, realm, prototype, "{name}", Some({function}), {length});\n')
    out.write(
        f"""        define_to_string_tag(vm, prototype, "{interface.namespaced_name}");
    }}

    /// The steps of {interface.constructor_class}::initialize() of LibWeb's C++ bindings.
    ///
    /// # Safety
    ///
    /// The pointers must be the VM, a realm and the interface object of the realm.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn {rust_entry_point_name(interface, "initialize_constructor")}(
        vm: *mut JSVM,
        realm: *mut JSRealm,
        constructor: *mut JSObject,
    ) {{
        // SAFETY: The caller passes live pointers.
        let (vm, realm, constructor) = unsafe {{ entry_point_arguments(vm, realm, constructor) }};
        define_interface_object_properties(vm, realm, constructor, "{interface.name}", {constructor_length(interface)}, INTERFACE_NAME);
    }}
"""
    )

    if interface.constructors:
        out.write(
            f"""
    /// The steps of {interface.constructor_class}::construct() of LibWeb's C++ bindings, which internally create a
    /// new object implementing the interface, https://webidl.spec.whatwg.org/#internally-create-a-new-object-implementing-the-interface
    ///
    /// # Safety
    ///
    /// The pointers must be the VM, the interface object and the new target of a construction.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn {rust_entry_point_name(interface, "construct")}(
        vm: *mut JSVM,
        _constructor: *mut JSObject,
        new_target: *mut JSObject,
    ) -> JSCompletion {{
        // SAFETY: The caller passes the VM and the new target.
        let (vm, new_target) = unsafe {{
            (
                Vm::from_raw(vm),
                Object::from_raw(new_target).expect("a construction has a new target"),
            )
        }};
        let construct = || -> ThrowCompletionOr<Value> {{
            let realm = vm.current_realm();
            let prototype = prototype_from_new_target(vm, new_target, INTERFACE_NAME)?;
            let mut cx = CallContext::new(vm, realm);
            let implementation =
                <Implementation as {trait}>::construct(&mut cx).map_err(|exception| exception.throw(vm))?;
            let implementation = CELL_TYPE.allocate(vm, implementation);
            Ok(create_wrapper(vm, realm, wrapper_class(), prototype, implementation).value())
        }};
        into_abi(construct())
    }}
"""
        )
    out.write("}\n")


def constructor_length(interface: Interface) -> int:
    if not interface.constructors:
        return 0
    return min(parameter_list_length(constructor.parameters) for constructor in interface.constructors)


def write_rust_dictionary(out: TextIO, context: GenerationContext, dictionary: Dictionary) -> None:
    fields = []
    conversions = []
    for member in dictionary.members:
        what = f"{dictionary.name}.{member.name}"
        member_type = rust_type(member.type, context, what)
        field = rust_identifier(member.name)
        always_present = member.required or member.default_value is not None
        if always_present:
            fields.append(f"        pub(crate) {field}: {member_type},\n")
            conversions.append(
                f'            set_dictionary_member(cx, object, "{member.name}", self.{field}.to_js(cx)?)?;\n'
            )
        else:
            fields.append(f"        pub(crate) {field}: Option<{member_type}>,\n")
            conversions.append(
                f"""            if let Some(value) = self.{field} {{
                set_dictionary_member(cx, object, "{member.name}", value.to_js(cx)?)?;
            }}
"""
            )
    out.write(
        f"""
    /// The dictionary {dictionary.name}.
    pub(crate) struct {dictionary.name} {{
{"".join(fields)}    }}

    impl ToJs for {dictionary.name} {{
        fn to_js(self, cx: &CallContext) -> ThrowCompletionOr<Value> {{
            let object = create_dictionary_object(cx);
{"".join(conversions)}            Ok(object.value())
        }}
    }}
"""
    )


def check_rust_bindings(context: GenerationContext, modules: List[Module]) -> None:
    """Raises an error for every use of [ImplementedIn] that the bindings do not handle, before anything is written."""
    check_rust_definitions_are_not_used_by_cpp(modules)
    for module in modules:
        interface = module.interface
        if interface is None:
            continue
        implemented_in = interface.extended_attributes.get("ImplementedIn")
        if implemented_in is not None and implemented_in != "Rust":
            raise UnsupportedByRustBindings(
                f"{interface.path}: {interface.name} is [ImplementedIn={implemented_in}], but the only value is Rust"
            )
        if is_implemented_in_rust(interface):
            check_interface_is_supported(interface, module, context)


def write_rust_bindings(out: TextIO, context: GenerationContext, modules: List[Module], intrinsics) -> None:
    out.write(
        """// Generated by Meta/Generators/generate_libweb_bindings.py -- DO NOT EDIT
//
// The bindings of the interfaces that are implemented in Rust.
"""
    )
    interface_name_values = {interface.name: index for index, interface in enumerate(intrinsics, start=1)}
    for module in modules:
        interface = module.interface
        if interface is None or not is_implemented_in_rust(interface):
            continue
        write_rust_interface(out, context, module, interface, interface_name_values[interface.name])

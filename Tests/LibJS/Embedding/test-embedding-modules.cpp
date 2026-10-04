/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <AK/ByteString.h>
#include <AK/Utf16FlyString.h>
#include <AK/Utf16String.h>
#include <AK/Vector.h>
#include <LibGC/Cell.h>
#include <LibGC/CellAllocator.h>
#include <LibGC/Heap.h>
#include <LibGC/Root.h>

#include "EmbeddingTest.h"

// The embedder's cells that module records hold as [[HostDefined]], like the module script LibWeb gives each module
// record, and that LoadRequestedModules hands every load of a graph, like LibWeb's fetch context.
class HostDefinedCell final : public GC::Cell {
    GC_CELL(HostDefinedCell, GC::Cell);
    GC_DECLARE_ALLOCATOR(HostDefinedCell);

public:
    explicit HostDefinedCell(StringView what_it_stands_for)
        : m_what_it_stands_for(what_it_stands_for)
    {
    }

private:
    ByteString m_what_it_stands_for;
};

GC_DEFINE_ALLOCATOR(HostDefinedCell);

// The state of a host module, like the companion cell of a WebAssembly module record: the values of its two exports.
class HostModuleData final : public GC::Cell {
    GC_CELL(HostModuleData, GC::Cell);
    GC_DECLARE_ALLOCATOR(HostModuleData);

public:
    i32 answer { 42 };
    i32 value_of_non_ascii_export { 7 };
};

GC_DEFINE_ALLOCATOR(HostModuleData);

// Collects garbage once the stack below the caller no longer holds pointers left over from earlier calls, which the
// conservative scan would treat as roots.
static NEVER_INLINE void collect_garbage()
{
    u8 volatile filler[8 * KiB];
    for (size_t i = 0; i < sizeof(filler); ++i)
        filler[i] = 0;
    GC::Heap::the().collect_garbage();
}

static JSUtf16View view_of(Utf16String const& string)
{
    return abi_view_of(string.utf16_view());
}

static ByteString byte_string_of_code_units(u16 const* code_units, size_t length)
{
    return byte_string_of({ code_units, length, false });
}

// The hooks reach the one VM of the process the way the facade's hooks reach JS::VM::the().
static EmbeddedVM* s_embedded_vm;

static bool s_hooks_reenter;
static bool s_execution_throws;
static size_t s_hook_calls;
static size_t s_importing_module_executions;

// What a hook runs before anything else while a test asks the hooks to re-enter the VM.
static constexpr StringView reentrant_script = R"~~~(
    globalThis.reentries = (globalThis.reentries ?? 0) + 1;
    [3, 1, 2].sort().map(value => ({ value }));
    Promise.resolve(1).then(() => {});
)~~~"sv;

static void reenter_if_asked()
{
    if (!s_hooks_reenter)
        return;
    VERIFY(s_embedded_vm->run(reentrant_script));
    collect_garbage();
}

static Utf16String const& answer_name()
{
    static auto const name = Utf16String::from_utf8("answer"sv);
    return name;
}

// An export name that is not ASCII, which crosses the ABI as UTF-16 code units.
static Utf16String const& non_ascii_name()
{
    static auto const name = Utf16String::from_code_point(0x540D);
    return name;
}

static Vector<Utf16String const*> exported_names()
{
    return { &answer_name(), &non_ascii_name() };
}

// Sinks take UTF-16 code units, which a string with ASCII storage does not have.
static void append_to_sink(JSStringSink const& sink, Utf16String const& string)
{
    Vector<u16> code_units;
    for (size_t i = 0; i < string.length_in_code_units(); ++i)
        code_units.append(string.code_unit_at(i));
    sink.append(sink.context, code_units.data(), code_units.size());
}

static void get_exported_names(JSModule*, JSStringSink* names)
{
    ++s_hook_calls;
    reenter_if_asked();
    for (auto const* name : exported_names())
        append_to_sink(*names, *name);
}

static void resolve_export(JSModule* module, u16 const* export_name, size_t export_name_length, JSResolvedBinding* out)
{
    ++s_hook_calls;
    reenter_if_asked();
    VERIFY(out->type == JS_RESOLVED_BINDING_NULL);
    auto name = byte_string_of_code_units(export_name, export_name_length);
    for (auto const* exported_name : exported_names()) {
        if (exported_name->to_byte_string() != name)
            continue;
        out->type = JS_RESOLVED_BINDING_BINDING_NAME;
        out->module = module;
        out->binding_name.append(out->binding_name.context, export_name, export_name_length);
        return;
    }
}

// Creates the module's environment with a binding for each export, as LibWeb does for a WebAssembly module record.
static JSCompletion initialize_environment(JSModule* module)
{
    ++s_hook_calls;
    reenter_if_asked();
    auto* vm = s_embedded_vm->vm();
    auto* environment = js_environment_new_module_environment(vm, nullptr);
    js_host_module_set_environment(module, environment);
    for (auto const* name : exported_names())
        VERIFY(js_environment_create_immutable_binding(vm, environment, view_of(*name), true).variant == JS_COMPLETION_NORMAL);
    return { 0, JS_COMPLETION_NORMAL };
}

// Fills in the bindings from the module's own state. While a test asks the hooks to re-enter the VM, it first creates
// the module's namespace, which calls the other two hooks while this one runs.
static JSCompletion execute_module(JSModule* module, JSPromiseCapability* capability)
{
    VERIFY(!capability);
    ++s_hook_calls;
    reenter_if_asked();
    auto* vm = s_embedded_vm->vm();
    if (s_execution_throws)
        return js_error_throw(vm, JS_ERROR_KIND_ERROR, ascii_view("execute_module threw"sv));
    if (s_hooks_reenter)
        VERIFY(js_module_get_module_namespace(vm, module));

    auto const& data = *static_cast<HostModuleData const*>(js_host_module_host_data(module));
    auto* environment = js_module_environment(module);
    VERIFY(js_environment_initialize_binding(vm, environment, view_of(answer_name()), int32_value(data.answer), JS_INITIALIZE_BINDING_HINT_NORMAL).variant == JS_COMPLETION_NORMAL);
    VERIFY(js_environment_initialize_binding(vm, environment, view_of(non_ascii_name()), int32_value(data.value_of_non_ascii_export), JS_INITIALIZE_BINDING_HINT_NORMAL).variant == JS_COMPLETION_NORMAL);
    return { 0, JS_COMPLETION_NORMAL };
}

static constexpr JSHostModuleHooks exporting_module_hooks {
    .get_exported_names = get_exported_names,
    .resolve_export = resolve_export,
    .initialize_environment = initialize_environment,
    .execute_module = execute_module,
};

static constexpr StringView exporting_module_class_name = "ExportingModule"sv;

static constexpr JSHostClass exporting_module_class {
    .abi_version = JS_HOST_ABI_VERSION,
    .kind = JS_HOST_CLASS_MODULE,
    .reserved = 0,
    .flags = 0,
    .name = exporting_module_class_name.characters_without_null_termination(),
    .name_length = exporting_module_class_name.length(),
    .parent = nullptr,
    .hooks = &exporting_module_hooks,
    .user_data = nullptr,
};

static JSModule* create_exporting_module(StringView filename, void* host_defined)
{
    auto data = GC::Heap::the().allocate<HostModuleData>();
    return js_host_module_create(s_embedded_vm->vm(), s_embedded_vm->realm(), &exporting_module_class, ascii_view(filename), nullptr, 0, host_defined, data.ptr());
}

static JSCompletion completion_of_module(JSModule* module)
{
    return { reinterpret_cast<uintptr_t>(module), JS_COMPLETION_NORMAL };
}

// A load that the embedder's load_imported_module hook received and finishes later, as HTML finishes one once its
// fetch completes. It keeps the records of the referrer and the payload alive, and its own copy of the request.
struct PendingModuleLoad {
    JSImportedModuleReferrer referrer;
    GC::Root<GC::Cell> referrer_record;
    JSModuleRequest* module_request { nullptr };
    ByteString specifier;
    void* host_defined { nullptr };
    JSImportedModulePayload payload;
    GC::Root<GC::Cell> payload_record;
};

struct TestModuleLoader {
    Vector<PendingModuleLoad> pending_loads;
    size_t loads_finished_inside_the_hook { 0 };

    PendingModuleLoad take(StringView specifier)
    {
        auto index = pending_loads.find_first_index_if([&](auto const& load) { return load.specifier == specifier; });
        VERIFY(index.has_value());
        return pending_loads.take(*index);
    }

    void finish(PendingModuleLoad load, JSCompletion result)
    {
        js_module_finish_loading_imported_module(s_embedded_vm->vm(), load.referrer, load.module_request, load.payload, result);
        js_module_request_destroy(load.module_request);
    }

    ~TestModuleLoader()
    {
        for (auto& load : pending_loads)
            js_module_request_destroy(load.module_request);
    }
};

// Loads the modules whose specifiers start with "./now" right away, like a module that is already in the module
// map, and keeps every other load for the test to finish.
static void load_imported_module(void* data, JSVM* vm, JSImportedModuleReferrer referrer, JSModuleRequest const* module_request, void* host_defined, JSImportedModulePayload payload)
{
    auto& loader = *static_cast<TestModuleLoader*>(data);
    auto specifier = byte_string_of(js_module_request_specifier(module_request));
    if (specifier.starts_with("./now"sv)) {
        reenter_if_asked();
        ++loader.loads_finished_inside_the_hook;
        js_module_finish_loading_imported_module(vm, referrer, module_request, payload, completion_of_module(create_exporting_module(specifier, host_defined)));
        return;
    }
    loader.pending_loads.append({
        .referrer = referrer,
        .referrer_record = static_cast<GC::Cell*>(referrer.record),
        .module_request = js_module_request_clone(module_request),
        .specifier = move(specifier),
        .host_defined = host_defined,
        .payload = payload,
        .payload_record = static_cast<GC::Cell*>(payload.record),
    });
}

static constexpr JSVmHostHooks module_loading_hooks {
    .ensure_can_add_private_element = nullptr,
    .ensure_can_compile_strings = nullptr,
    .get_code_for_eval = nullptr,
    .promise_rejection_tracker = nullptr,
    .call_job_callback = nullptr,
    .enqueue_finalization_registry_cleanup_job = nullptr,
    .enqueue_promise_job = nullptr,
    .promise_job_queue_is_empty = nullptr,
    .make_job_callback = nullptr,
    .get_import_meta_properties = nullptr,
    .finalize_import_meta = nullptr,
    .get_supported_import_attributes = nullptr,
    .load_imported_module = load_imported_module,
    .unrecognized_date_string = nullptr,
    .resize_array_buffer = nullptr,
    .grow_shared_array_buffer = nullptr,
    .system_utc_epoch_nanoseconds = nullptr,
    .on_unimplemented_property_access = nullptr,
};

// A VM with a realm whose embedder loads modules through a TestModuleLoader.
struct EmbeddedVMLoadingModules {
    NonnullOwnPtr<EmbeddedVM> embedded_vm { EmbeddedVM::create_with_realm(EmbeddedVM::process_default_heap_options) };
    TestModuleLoader loader;

    EmbeddedVMLoadingModules()
    {
        s_embedded_vm = embedded_vm.ptr();
        js_vm_set_embedder(vm(), &module_loading_hooks, &loader);
    }

    ~EmbeddedVMLoadingModules()
    {
        js_vm_set_embedder(vm(), nullptr, nullptr);
        s_embedded_vm = nullptr;
        s_hooks_reenter = false;
        s_execution_throws = false;
        s_hook_calls = 0;
        s_importing_module_executions = 0;
    }

    JSVM* vm() { return embedded_vm->vm(); }
    JSRealm* realm() { return embedded_vm->realm(); }

    JSModule* parse(StringView source, StringView filename, void* host_defined)
    {
        auto* module = js_module_parse_source_text_module(vm(), realm(), ascii_view(source), ascii_view(filename), ascii_view(""sv), host_defined, 0, nullptr);
        VERIFY(module);
        return module;
    }

    void define_global(Utf16FlyString const& name, JSObject* object)
    {
        JSPropertyKey key { name.raw_identity() };
        js_object_define_direct_property(vm(), embedded_vm->global_object(), &key, value_of_object(object), JS_ATTRIBUTE_WRITABLE | JS_ATTRIBUTE_CONFIGURABLE);
    }
};

static void collect_names(void* context, u16 const* code_units, size_t length)
{
    static_cast<Vector<ByteString>*>(context)->append(byte_string_of_code_units(code_units, length));
}

static Utf16String const& imported_name()
{
    static auto const name = Utf16String::from_utf8("imported"sv);
    return name;
}

static void importing_module_get_exported_names(JSModule*, JSStringSink* names)
{
    reenter_if_asked();
    append_to_sink(*names, imported_name());
}

static void importing_module_resolve_export(JSModule* module, u16 const* export_name, size_t export_name_length, JSResolvedBinding* out)
{
    reenter_if_asked();
    if (byte_string_of_code_units(export_name, export_name_length) != imported_name().to_byte_string())
        return;
    out->type = JS_RESOLVED_BINDING_BINDING_NAME;
    out->module = module;
    out->binding_name.append(out->binding_name.context, export_name, export_name_length);
}

static JSCompletion importing_module_initialize_environment(JSModule* module)
{
    reenter_if_asked();
    auto* vm = s_embedded_vm->vm();
    auto* environment = js_environment_new_module_environment(vm, nullptr);
    js_host_module_set_environment(module, environment);
    VERIFY(js_environment_create_immutable_binding(vm, environment, view_of(imported_name()), true).variant == JS_COMPLETION_NORMAL);
    return { 0, JS_COMPLETION_NORMAL };
}

// Reads the "value" export of "./dep.mjs" the way LibWeb's WebAssembly module records read their imports: it gets the
// imported module, resolves the export and reads the binding it resolves to from that module's environment.
static JSCompletion importing_module_execute_module(JSModule* module, JSPromiseCapability* capability)
{
    VERIFY(!capability);
    ++s_importing_module_executions;
    reenter_if_asked();
    auto* vm = s_embedded_vm->vm();

    auto* dependency_request = js_module_request_create(ascii_view("./dep.mjs"sv), nullptr, 0);
    auto* dependency = js_module_get_imported_module(module, dependency_request);
    js_module_request_destroy(dependency_request);

    Vector<ByteString> binding_names;
    JSResolvedBinding resolution { .module = nullptr, .binding_name = { .context = &binding_names, .append = collect_names }, .type = JS_RESOLVED_BINDING_NULL };
    js_module_resolve_export(vm, dependency, ascii_view("value"sv), &resolution);
    VERIFY(resolution.type == JS_RESOLVED_BINDING_BINDING_NAME);
    VERIFY(binding_names.size() == 1);

    auto value = js_environment_get_binding_value(vm, js_module_environment(resolution.module), ascii_view(binding_names.first()), true);
    if (value.variant != JS_COMPLETION_NORMAL)
        return value;
    return js_environment_initialize_binding(vm, js_module_environment(module), view_of(imported_name()), value.payload, JS_INITIALIZE_BINDING_HINT_NORMAL);
}

static constexpr JSHostModuleHooks importing_module_hooks {
    .get_exported_names = importing_module_get_exported_names,
    .resolve_export = importing_module_resolve_export,
    .initialize_environment = importing_module_initialize_environment,
    .execute_module = importing_module_execute_module,
};

static constexpr StringView importing_module_class_name = "ImportingModule"sv;

// A class of host modules that import "./dep.mjs" and export its "value" as "imported", like a WebAssembly module
// record whose module imports a value from JavaScript.
static constexpr JSHostClass importing_module_class {
    .abi_version = JS_HOST_ABI_VERSION,
    .kind = JS_HOST_CLASS_MODULE,
    .reserved = 0,
    .flags = 0,
    .name = importing_module_class_name.characters_without_null_termination(),
    .name_length = importing_module_class_name.length(),
    .parent = nullptr,
    .hooks = &importing_module_hooks,
    .user_data = nullptr,
};

struct ParserErrorThroughTheABI {
    ByteString message;
    u32 line { 0 };
    u32 column { 0 };
};

static void collect_parser_error(void* context, JSOwnedUtf16String message, u32 line, u32 column)
{
    static_cast<Vector<ParserErrorThroughTheABI>*>(context)->append({ Utf16String::adopt_raw(message).to_byte_string(), line, column });
}

TEST_CASE(a_source_text_module_imports_a_host_module_that_loads_after_the_load_hook_returns)
{
    EmbeddedVMLoadingModules embedder;
    auto* vm = embedder.vm();
    auto module_script = GC::Heap::the().allocate<HostDefinedCell>("module script"sv);
    auto fetch_context = GC::Heap::the().allocate<HostDefinedCell>("fetch context"sv);

    auto* entry = embedder.parse(R"~~~(
        import { answer, "\u540d" as name } from "./exporting.wasm";
        import data from "./data.json" with { type: "json" };
        export { answer as reexported } from "./exporting.wasm";
        globalThis.result = [answer, name, data.list.join("")].join();
    )~~~"sv,
        "entry.mjs"sv, module_script.ptr());
    EXPECT_EQ(js_module_host_defined(entry), static_cast<void*>(module_script.ptr()));
    EXPECT_EQ(js_module_class_id(entry), JS_LAYOUT_CLASS_ID_SOURCE_TEXT_MODULE);
    EXPECT(!js_host_module_host_class(entry));

    EXPECT(js_module_load_requested_modules(vm, entry, fetch_context.ptr()));

    // The frontend lists a request for each declaration that names a module, as it does for the C++ runtime.
    auto& pending_loads = embedder.loader.pending_loads;
    EXPECT_EQ(pending_loads.size(), 3u);
    EXPECT_EQ(js_module_requested_module_count(entry), 3u);
    for (size_t i = 0; i < pending_loads.size(); ++i) {
        auto const& load = pending_loads[i];
        EXPECT_EQ(load.referrer.kind, JS_IMPORTED_MODULE_REFERRER_CYCLIC_MODULE);
        EXPECT_EQ(load.referrer.record, static_cast<void*>(entry));
        EXPECT_EQ(load.host_defined, static_cast<void*>(fetch_context.ptr()));
        EXPECT_EQ(load.payload.kind, JS_IMPORTED_MODULE_PAYLOAD_GRAPH_LOADING_STATE);
        EXPECT(js_module_request_equals(load.module_request, js_module_requested_module(entry, i)));
    }
    EXPECT_EQ(pending_loads[0].specifier, "./exporting.wasm"sv);
    EXPECT_EQ(pending_loads[1].specifier, "./data.json"sv);
    EXPECT_EQ(js_module_request_attribute_count(pending_loads[1].module_request), 1u);
    auto attribute = js_module_request_attribute(pending_loads[1].module_request, 0);
    EXPECT_EQ(byte_string_of(attribute.key), "type"sv);
    EXPECT_EQ(byte_string_of(attribute.value), "json"sv);

    // Like a fetch, finishing waits for the event loop, during which garbage may be collected.
    collect_garbage();

    auto json_load = embedder.loader.take("./data.json"sv);
    auto json_module = js_module_parse_json_module(vm, embedder.realm(), ascii_view(R"({ "list": [1, 2] })"sv), ascii_view("data.json"sv));
    EXPECT_EQ(json_module.variant, JS_COMPLETION_NORMAL);
    embedder.loader.finish(move(json_load), json_module);

    auto exporting_load = embedder.loader.take("./exporting.wasm"sv);
    auto* exporting_module = create_exporting_module("exporting.wasm"sv, exporting_load.host_defined);
    embedder.loader.finish(move(exporting_load), completion_of_module(exporting_module));
    embedder.loader.finish(embedder.loader.take("./exporting.wasm"sv), completion_of_module(exporting_module));
    collect_garbage();

    EXPECT_EQ(js_module_link(vm, entry).variant, JS_COMPLETION_NORMAL);
    auto evaluation = js_module_evaluate(vm, entry);
    EXPECT(pointer_of_payload<JSPromiseCapability>(evaluation));
    EXPECT(embedder.embedded_vm->run("if (globalThis.result !== '42,7,12') throw new Error(globalThis.result);"sv));

    EXPECT_EQ(js_module_get_imported_module(entry, js_module_requested_module(entry, 0)), exporting_module);
    EXPECT_EQ(js_host_module_host_class(exporting_module), &exporting_module_class);
    EXPECT_EQ(js_module_class_id(exporting_module), JS_LAYOUT_CLASS_ID_HOST_MODULE);
    EXPECT_EQ(js_module_host_defined(exporting_module), static_cast<void*>(fetch_context.ptr()));
    EXPECT_EQ(static_cast<HostModuleData*>(js_host_module_host_data(exporting_module))->answer, 42);
    EXPECT_EQ(js_module_realm(exporting_module), embedder.realm());
    EXPECT(js_module_environment(exporting_module));

    Vector<ByteString> exported_names;
    JSStringSink names { .context = &exported_names, .append = collect_names };
    js_module_get_exported_names(vm, entry, &names);
    EXPECT_EQ(exported_names, Vector<ByteString> { "reexported" });

    Vector<ByteString> binding_names;
    JSResolvedBinding binding { .module = nullptr, .binding_name = { .context = &binding_names, .append = collect_names }, .type = JS_RESOLVED_BINDING_NULL };
    js_module_resolve_export(vm, entry, ascii_view("reexported"sv), &binding);
    EXPECT_EQ(binding.type, JS_RESOLVED_BINDING_BINDING_NAME);
    EXPECT_EQ(binding.module, exporting_module);
    EXPECT_EQ(binding_names, Vector<ByteString> { "answer" });

    embedder.define_global("namespaceObject"_utf16_fly_string, js_module_get_module_namespace(vm, exporting_module));
    EXPECT(embedder.embedded_vm->run(R"~~~(
        if (Object.keys(namespaceObject).join() !== "answer,\u540d" || namespaceObject.answer !== 42)
            throw new Error("the namespace object has the wrong exports");
    )~~~"sv));
}

TEST_CASE(the_hooks_of_a_host_module_and_the_load_hook_may_reenter_the_vm)
{
    EmbeddedVMLoadingModules embedder;
    auto* vm = embedder.vm();
    s_hooks_reenter = true;

    auto* entry = embedder.parse("import { answer } from './now.wasm'; globalThis.answer = answer;"sv, "entry.mjs"sv, nullptr);
    EXPECT(js_module_load_requested_modules(vm, entry, nullptr));
    EXPECT_EQ(embedder.loader.loads_finished_inside_the_hook, 1u);
    EXPECT(embedder.loader.pending_loads.is_empty());
    EXPECT_EQ(js_module_link(vm, entry).variant, JS_COMPLETION_NORMAL);
    EXPECT_EQ(js_module_evaluate(vm, entry).variant, JS_COMPLETION_NORMAL);
    s_hooks_reenter = false;

    // Each hook call and the load ran the reentrant script once.
    auto expected_reentries = ByteString::formatted("if (globalThis.answer !== 42 || globalThis.reentries !== {}) throw new Error(globalThis.reentries);", s_hook_calls + 1);
    EXPECT(embedder.embedded_vm->run(expected_reentries));
}

TEST_CASE(a_script_imports_a_text_module_and_a_host_module_that_throws_dynamically)
{
    EmbeddedVMLoadingModules embedder;
    auto* vm = embedder.vm();
    EXPECT(embedder.embedded_vm->run(R"~~~(
        globalThis.outcomes = [];
        import("./text.txt", { with: { type: "text" } }).then(namespace => outcomes.push(namespace.default));
        import("./throwing.wasm").catch(error => outcomes.push(error.message));
    )~~~"sv));

    auto& pending_loads = embedder.loader.pending_loads;
    EXPECT_EQ(pending_loads.size(), 2u);
    for (auto const& load : pending_loads) {
        EXPECT_EQ(load.referrer.kind, JS_IMPORTED_MODULE_REFERRER_SCRIPT);
        EXPECT(!load.host_defined);
        EXPECT_EQ(load.payload.kind, JS_IMPORTED_MODULE_PAYLOAD_PROMISE_CAPABILITY);
    }
    collect_garbage();

    auto* text_module = js_module_create_text_module(vm, embedder.realm(), ascii_view("plain text"sv), ascii_view("text.txt"sv));
    EXPECT_EQ(js_module_class_id(text_module), JS_LAYOUT_CLASS_ID_SYNTHETIC_MODULE);
    embedder.loader.finish(embedder.loader.take("./text.txt"sv), completion_of_module(text_module));
    embedder.loader.finish(embedder.loader.take("./throwing.wasm"sv), completion_of_module(create_exporting_module("throwing.wasm"sv, nullptr)));

    s_execution_throws = true;
    js_vm_run_queued_promise_jobs(vm);
    s_execution_throws = false;
    EXPECT(embedder.embedded_vm->run(R"~~~(
        if (outcomes.sort().join("|") !== "execute_module threw|plain text")
            throw new Error(outcomes.join("|"));
    )~~~"sv));
}

TEST_CASE(a_load_that_fails_rejects_the_import_and_a_parse_error_says_where_it_is)
{
    EmbeddedVMLoadingModules embedder;
    auto* vm = embedder.vm();
    EXPECT(embedder.embedded_vm->run(R"~~~(
        globalThis.outcome = "pending";
        import("./missing.mjs").catch(error => outcome = error.message);
    )~~~"sv));
    auto failure = js_error_throw(vm, JS_ERROR_KIND_TYPE_ERROR, ascii_view("Loading imported module './missing.mjs' failed."sv));
    EXPECT_EQ(failure.variant, JS_COMPLETION_THROW);
    collect_garbage();
    embedder.loader.finish(embedder.loader.take("./missing.mjs"sv), failure);
    js_vm_run_queued_promise_jobs(vm);
    EXPECT(embedder.embedded_vm->run("if (outcome !== \"Loading imported module './missing.mjs' failed.\") throw new Error(outcome);"sv));

    Vector<ParserErrorThroughTheABI> errors;
    JSParserErrorSink error_sink { .context = &errors, .append = collect_parser_error };
    auto* unparsable = js_module_parse_source_text_module(vm, embedder.realm(), ascii_view("export let = 1;"sv), ascii_view("page.html"sv), ascii_view("inline.js"sv), nullptr, 10, &error_sink);
    EXPECT(!unparsable);
    EXPECT(!errors.is_empty());
    EXPECT_EQ(errors.first().line, 10u);
    EXPECT_EQ(errors.first().column, 12u);
    EXPECT(!errors.first().message.contains("line:"sv));
}

TEST_CASE(a_json_module_that_does_not_parse_throws_a_syntax_error)
{
    EmbeddedVMLoadingModules embedder;

    // ParseJSON creates the error in the realm of the running execution context, which the embedded VM keeps running.
    auto completion = js_module_parse_json_module(embedder.vm(), embedder.realm(), ascii_view(R"({ "list": [1, 2 })"sv), ascii_view("data.json"sv));
    EXPECT_EQ(completion.variant, JS_COMPLETION_THROW);
    auto* error = object_of_value(completion.payload);
    EXPECT(error);
    embedder.define_global("jsonError"_utf16_fly_string, error);
    EXPECT(embedder.embedded_vm->run("if (!(jsonError instanceof SyntaxError)) throw new Error(String(jsonError));"sv));
}

TEST_CASE(a_host_module_reads_its_import_from_a_module_with_top_level_await_once_that_settles)
{
    EmbeddedVMLoadingModules embedder;
    auto* vm = embedder.vm();
    auto fetch_context = GC::Heap::the().allocate<HostDefinedCell>("fetch context"sv);
    s_hooks_reenter = true;

    auto* entry = embedder.parse("import { imported } from './importing.wasm'; globalThis.result = imported;"sv, "entry.mjs"sv, nullptr);
    EXPECT(js_module_load_requested_modules(vm, entry, fetch_context.ptr()));

    auto* dependency_request = js_module_request_create(ascii_view("./dep.mjs"sv), nullptr, 0);
    JSModuleRequest const* requested_modules[] = { dependency_request };
    auto* importing_module = js_host_module_create(vm, embedder.realm(), &importing_module_class, ascii_view("importing.wasm"sv), requested_modules, 1, nullptr, nullptr);
    js_module_request_destroy(dependency_request);
    EXPECT_EQ(js_module_requested_module_count(importing_module), 1u);
    EXPECT_EQ(byte_string_of(js_module_request_specifier(js_module_requested_module(importing_module, 0))), "./dep.mjs"sv);
    embedder.loader.finish(embedder.loader.take("./importing.wasm"sv), completion_of_module(importing_module));

    // The host module's own request loads through the hook as well, with the host module as the referrer.
    auto& pending_loads = embedder.loader.pending_loads;
    EXPECT_EQ(pending_loads.size(), 1u);
    EXPECT_EQ(pending_loads[0].specifier, "./dep.mjs"sv);
    EXPECT_EQ(pending_loads[0].referrer.kind, JS_IMPORTED_MODULE_REFERRER_CYCLIC_MODULE);
    EXPECT_EQ(pending_loads[0].referrer.record, static_cast<void*>(importing_module));
    EXPECT_EQ(pending_loads[0].host_defined, static_cast<void*>(fetch_context.ptr()));
    collect_garbage();

    auto* dependency = embedder.parse(R"~~~(
        export let value = "before top-level await";
        await Promise.resolve();
        value = "after top-level await";
    )~~~"sv,
        "dep.mjs"sv, nullptr);
    embedder.loader.finish(embedder.loader.take("./dep.mjs"sv), completion_of_module(dependency));

    EXPECT_EQ(js_module_link(vm, entry).variant, JS_COMPLETION_NORMAL);
    EXPECT_EQ(js_module_evaluate(vm, entry).variant, JS_COMPLETION_NORMAL);
    EXPECT_EQ(s_importing_module_executions, 0u);
    collect_garbage();

    // Once the top-level await settles, a promise job executes the host module, whose hook runs a script and collects
    // garbage while it does, and then the entry.
    js_vm_run_queued_promise_jobs(vm);
    s_hooks_reenter = false;
    EXPECT_EQ(s_importing_module_executions, 1u);
    EXPECT(embedder.embedded_vm->run("if (globalThis.result !== 'after top-level await') throw new Error(globalThis.result);"sv));
}

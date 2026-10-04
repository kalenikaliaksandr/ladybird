/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

// An embedder of the Rust runtime built on nothing but its C ABI, the way the C++ facade over it will be. It
// constructs the VM in its own storage and installs host hooks: promise jobs go to an HTML-style microtask queue that
// it drains after each script, modules load from files in tasks that run later, finalization registries queue their
// cleanup, and an agent spins this event loop for await in native code. Each realm's global object is a host object
// with the functions of the test-js runner and a few of the host's own.
//
// It runs a slice of Tests/LibJS/Runtime with test-common.js and checks that every test has the result that the Rust
// runtime's own test-js runner, test-js-runtime-rust, reports for it.

#include <AK/AnyOf.h>
#include <AK/ByteString.h>
#include <AK/HashMap.h>
#include <AK/HashTable.h>
#include <AK/JsonObject.h>
#include <AK/JsonValue.h>
#include <AK/LexicalPath.h>
#include <AK/OwnPtr.h>
#include <AK/QuickSort.h>
#include <AK/ScopeGuard.h>
#include <AK/StringBuilder.h>
#include <AK/Utf16FlyString.h>
#include <AK/Utf16String.h>
#include <AK/Vector.h>
#include <LibCore/DirIterator.h>
#include <LibCore/File.h>
#include <LibGC/CAPI.h>
#include <stdio.h>
#include <sys/stat.h>
#include <unistd.h>

#include "EmbeddingTest.h"

namespace {

// The NaN-box encodings of the values the host makes or takes apart, besides those of EmbeddingTest.h.
constexpr u64 string_value_tag = 0b010 | GC::IS_CELL_BIT;
constexpr u64 null_value_tag = 0b111 | GC::BASE_TAG;
constexpr JSValue js_null = null_value_tag << GC::TAG_SHIFT;
constexpr JSValue js_false = boolean_value_tag << GC::TAG_SHIFT;

constexpr u8 default_attributes = JS_ATTRIBUTE_WRITABLE | JS_ATTRIBUTE_ENUMERABLE | JS_ATTRIBUTE_CONFIGURABLE;
constexpr u8 hidden_attributes = JS_ATTRIBUTE_WRITABLE | JS_ATTRIBUTE_CONFIGURABLE;

u64 tag_of(JSValue value)
{
    return value >> GC::TAG_SHIFT;
}

bool is_cell(JSValue value)
{
    return (tag_of(value) & GC::IS_CELL_PATTERN) == GC::IS_CELL_PATTERN;
}

void* cell_of(JSValue value)
{
    VERIFY(is_cell(value));
    return reinterpret_cast<void*>(GC::NanBoxedValue::extract_pointer_bits(value));
}

JSPrimitiveString* string_of_value(JSValue value)
{
    if (tag_of(value) != string_value_tag)
        return nullptr;
    return static_cast<JSPrimitiveString*>(cell_of(value));
}

JSValue bool_value(bool value)
{
    return value ? js_true : js_false;
}

// A borrowed key for a name that is not an array index: the word of its AK fly string, which is what a string key of
// the runtime is. CommonPropertyNames of the facade relies on this.
JSPropertyKey key_of(Utf16FlyString const& name)
{
    return { name.raw_identity() };
}

Utf16String adopt(JSOwnedUtf16String string)
{
    return Utf16String::adopt_raw(string);
}

ByteString utf8_of(Utf16View const& view)
{
    return MUST(view.to_byte_string());
}

ByteString utf8_of_owned(JSOwnedUtf16String string)
{
    auto adopted = adopt(string);
    return utf8_of(adopted.utf16_view());
}

template<typename T>
T field_at(void const* base, size_t offset)
{
    T field;
    __builtin_memcpy(&field, static_cast<u8 const*>(base) + offset, sizeof(field));
    return field;
}

// What the facade's VM reads inline: the running execution context, its realm and its arguments, which are the last
// slots after the context's fields.
u8 const* running_context(JSVM* vm)
{
    return field_at<u8 const*>(vm, JS_LAYOUT_VM_RUNNING_EXECUTION_CONTEXT_OFFSET);
}

JSRealm* current_realm(JSVM* vm)
{
    auto const* context = running_context(vm);
    VERIFY(context);
    return field_at<JSRealm*>(context, JS_LAYOUT_EXECUTION_CONTEXT_REALM_OFFSET);
}

JSValue argument(JSVM* vm, size_t index)
{
    auto const* context = running_context(vm);
    auto slot_count = field_at<u32>(context, JS_LAYOUT_EXECUTION_CONTEXT_REGISTERS_AND_CONSTANTS_AND_LOCALS_AND_ARGUMENTS_COUNT_OFFSET);
    auto argument_count = field_at<u32>(context, JS_LAYOUT_EXECUTION_CONTEXT_ARGUMENT_COUNT_OFFSET);
    if (index >= argument_count)
        return js_undefined;
    return field_at<JSValue>(context, JS_LAYOUT_EXECUTION_CONTEXT_SIZE + (slot_count - argument_count + index) * sizeof(JSValue));
}

size_t argument_count(JSVM* vm)
{
    return field_at<u32>(running_context(vm), JS_LAYOUT_EXECUTION_CONTEXT_ARGUMENT_COUNT_OFFSET);
}

JSCompletion normal_completion(JSValue value = js_undefined)
{
    return { value, JS_COMPLETION_NORMAL };
}

JSCompletion throw_error(JSVM* vm, JSErrorKind kind, StringView message)
{
    auto utf16_message = Utf16String::from_utf8(message);
    return js_error_throw(vm, kind, abi_view_of(utf16_message.utf16_view()));
}

ErrorOr<Utf16String, JSCompletion> to_utf16_string(JSVM* vm, JSValue value)
{
    JSOwnedUtf16String string = 0;
    auto completion = js_value_to_utf16_string(vm, value, &string);
    if (completion.variant != JS_COMPLETION_NORMAL)
        return completion;
    return adopt(string);
}

ByteString describe(JSValue value)
{
    return utf8_of_owned(js_value_to_utf16_string_without_side_effects(value));
}

// Native functions are written for whichever way the target returns a C++ ThrowCompletionOr<Value>.
template<JSCompletion (*behaviour)(JSVM*)>
JSNativeFunction native_function()
{
    if constexpr (IsSame<JSNativeFunction, JSCompletion (*)(JSVM*)>)
        return behaviour;
    else
        return [](JSCompletion* result, JSVM* vm) { *result = behaviour(vm); };
}

ErrorOr<ByteString> read_file(ByteString const& path)
{
    auto file = TRY(Core::File::open(path, Core::File::OpenMode::Read));
    auto contents = TRY(file->read_until_eof());
    return ByteString { contents.bytes() };
}

bool file_exists(ByteString const& path)
{
    struct stat buffer;
    return stat(path.characters(), &buffer) == 0;
}

bool is_directory(ByteString const& path)
{
    struct stat buffer;
    return stat(path.characters(), &buffer) == 0 && S_ISDIR(buffer.st_mode);
}

// The file a module specifier names, tried with the extensions of its type, as the runtime's own loader does.
ByteString resolve_module_filename(ByteString const& filename, StringView module_type)
{
    Vector<StringView> extensions;
    if (module_type == "json"sv)
        extensions.append("json"sv);
    else
        extensions.extend({ "js"sv, "mjs"sv });
    if (!file_exists(filename)) {
        for (auto extension : extensions) {
            auto candidate = ByteString::formatted("{}.{}", filename, extension);
            if (file_exists(candidate))
                return candidate;
        }
    } else if (is_directory(filename)) {
        for (auto extension : extensions) {
            auto candidate = LexicalPath::join(filename, ByteString::formatted("index.{}", extension)).string();
            if (file_exists(candidate))
                return candidate;
        }
    }
    return filename;
}

// The syntax errors of a parse, each as C++ ParserError::to_utf16_string() formats it.
struct ParserErrors {
    Vector<ByteString> messages;

    JSParserErrorSink sink()
    {
        return { this, [](void* context, JSOwnedUtf16String message, u32 line, u32 column) {
                    auto& errors = *static_cast<ParserErrors*>(context);
                    errors.messages.append(ByteString::formatted("{} (line: {}, column: {})", utf8_of_owned(message), line, column));
                } };
    }
};

class Host;
Host* s_host = nullptr;

// A test file's realm and its execution context, which the host pushes to run code in the realm. Nothing visits the
// context while it is off the execution context stack, so a root keeps the realm alive.
struct HostRealm {
    AK_ALLOC_WITH_KMALLOC;
    AK_MAKE_NONCOPYABLE(HostRealm);
    AK_MAKE_NONMOVABLE(HostRealm);

public:
    HostRealm() = default;

    ~HostRealm()
    {
        if (realm_root)
            gc_root_destroy(realm_root);
    }

    alignas(JS_LAYOUT_EXECUTION_CONTEXT_ALIGN) u8 execution_context[JS_LAYOUT_EXECUTION_CONTEXT_SIZE] {};
    GCRoot* realm_root { nullptr };

    JSExecutionContext* context() { return reinterpret_cast<JSExecutionContext*>(execution_context); }
    JSRealm* realm() const { return field_at<JSRealm*>(execution_context, JS_LAYOUT_EXECUTION_CONTEXT_REALM_OFFSET); }
};

// A module the host loads for a load_imported_module hook call, in a task that runs after the call returned.
struct PendingModuleLoad {
    JSImportedModuleReferrer referrer;
    JSModuleRequest* module_request { nullptr };
    JSImportedModulePayload payload;
    ByteString base_filename;
    GCRoot* referrer_root { nullptr };
    GCRoot* payload_root { nullptr };
};

struct Timer {
    GCRoot* callback { nullptr };
    i64 delay { 0 };
    u64 sequence_number { 0 };
};

class Host {
public:
    Host()
        : m_embedded_vm(EmbeddedVM::create(EmbeddedVM::process_default_heap_options))
    {
        VERIFY(!s_host);
        s_host = this;
        js_vm_set_embedder(vm(), &s_hooks, this);
        JSAgent agent {
            .can_block = true,
            .spin_event_loop_until = spin_event_loop_until,
            .data = this,
        };
        js_vm_set_agent(vm(), &agent);
    }

    // The VM outlives every root of its heap.
    ~Host()
    {
        for (auto* root : m_microtasks)
            gc_root_destroy(root);
        for (auto* root : m_finalization_registry_cleanup_jobs)
            gc_root_destroy(root);
        for (auto& load : m_pending_module_loads) {
            js_module_request_destroy(load.module_request);
            gc_root_destroy(load.referrer_root);
            gc_root_destroy(load.payload_root);
        }
        for (auto& timer : m_timers)
            gc_root_destroy(timer.callback);
        for (auto& [filename, root] : m_module_map)
            gc_root_destroy(root);
        m_realm = nullptr;
        js_vm_set_agent(vm(), nullptr);
        js_vm_set_embedder(vm(), nullptr, nullptr);
        s_host = nullptr;
    }

    static Host& the()
    {
        VERIFY(s_host);
        return *s_host;
    }

    JSVM* vm() { return m_embedded_vm->vm(); }
    HostRealm& realm() { return *m_realm; }
    Vector<ByteString>& log() { return m_log; }

    // InitializeHostDefinedRealm with a host object as the global object, whose functions it defines before the
    // default global bindings, as the test-js runner's global object does in its initialize(). The new realm replaces
    // the previous one, which lives on only as long as cells of the VM hold it.
    HostRealm& create_realm()
    {
        VERIFY(!js_execution_context_running(vm()));
        auto realm = make<HostRealm>();
        auto completion = js_realm_initialize_host_defined_realm(vm(), realm->execution_context, create_global_object, this, nullptr, nullptr);
        VERIFY(completion.variant == JS_COMPLETION_NORMAL);
        realm->realm_root = gc_root_create(reinterpret_cast<GCCell*>(realm->realm()));
        VERIFY(js_execution_context_pop(vm()) == realm->context());
        m_realm = move(realm);
        return *m_realm;
    }

    // Runs `run` in the realm's execution context unless code already runs, which it then runs on top of, as HTML's
    // "prepare to run script" does.
    template<typename Callback>
    decltype(auto) run_in_realm(Callback run)
    {
        auto needs_context = js_execution_context_running(vm()) == nullptr;
        if (needs_context)
            js_execution_context_push(vm(), realm().context());
        ScopeGuard pop_context = [&] {
            if (needs_context)
                VERIFY(js_execution_context_pop(vm()) == realm().context());
        };
        return run();
    }

    JSScript* parse_script(Utf16View source, ByteString const& filename, ParserErrors& errors)
    {
        auto utf16_filename = Utf16String::from_utf8(filename);
        auto sink = errors.sink();
        auto* script = js_script_parse(vm(), realm().realm(), abi_view_of(source), abi_view_of(utf16_filename.utf16_view()), abi_view_of(utf16_filename.utf16_view()), nullptr, 1, &sink);
        if (script)
            m_filenames.set(script, filename);
        return script;
    }

    JSModule* parse_module(Utf16View source, ByteString const& filename, ParserErrors& errors)
    {
        auto utf16_filename = Utf16String::from_utf8(filename);
        auto sink = errors.sink();
        auto* module = js_module_parse_source_text_module(vm(), realm().realm(), abi_view_of(source), abi_view_of(utf16_filename.utf16_view()), abi_view_of(utf16_filename.utf16_view()), nullptr, 1, &sink);
        if (module)
            m_filenames.set(module, filename);
        return module;
    }

    // Runs a script of the realm the way HTML runs a classic script: in the realm's context, followed by a microtask
    // checkpoint and, for the host's own event loop, the tasks that loading modules queued.
    JSCompletion run_script(JSScript* script)
    {
        auto completion = run_in_realm([&] { return js_script_run(vm(), script, nullptr); });
        run_until_idle();
        return completion;
    }

    void perform_microtask_checkpoint()
    {
        if (m_performing_microtask_checkpoint)
            return;
        m_performing_microtask_checkpoint = true;
        while (!m_microtasks.is_empty()) {
            auto* job_root = m_microtasks.take_first();
            auto* job = reinterpret_cast<JSPromiseJob*>(gc_root_cell(job_root));
            run_in_realm([&] { (void)js_promise_job_run(vm(), job); });
            gc_root_destroy(job_root);
        }
        m_performing_microtask_checkpoint = false;
    }

    // One task of the event loop other than a timer: a module load. Returns false when there is none.
    bool run_one_module_load()
    {
        if (m_pending_module_loads.is_empty())
            return false;
        auto load = m_pending_module_loads.take_first();
        run_in_realm([&] { finish_module_load(load); });
        js_module_request_destroy(load.module_request);
        gc_root_destroy(load.referrer_root);
        gc_root_destroy(load.payload_root);
        perform_microtask_checkpoint();
        return true;
    }

    bool run_one_timer()
    {
        if (m_timers.is_empty())
            return false;
        size_t next = 0;
        for (size_t index = 1; index < m_timers.size(); ++index) {
            auto& timer = m_timers[index];
            auto& earliest = m_timers[next];
            if (timer.delay < earliest.delay || (timer.delay == earliest.delay && timer.sequence_number < earliest.sequence_number))
                next = index;
        }
        auto timer = m_timers.take(next);
        auto callback = value_of_object(reinterpret_cast<JSObject*>(gc_root_cell(timer.callback)));
        run_in_realm([&] { (void)js_function_call(vm(), callback, js_undefined, nullptr, 0); });
        gc_root_destroy(timer.callback);
        perform_microtask_checkpoint();
        return true;
    }

    // Runs microtasks and module loads until there are none, leaving timers for later.
    void run_until_idle()
    {
        perform_microtask_checkpoint();
        while (run_one_module_load()) { }
    }

    void run_timers()
    {
        run_until_idle();
        while (run_one_timer())
            run_until_idle();
    }

    // runQueuedFinalizationRegistryCleanupJobs(): the cleanup of the registries the hook queued, the most recently
    // queued first. One whose callback threw is cleaned up again, as the runtime's own queue does while the registry
    // still has cells to clean up; a cleanup without any is a no-op.
    void run_finalization_registry_cleanup_jobs()
    {
        while (!m_finalization_registry_cleanup_jobs.is_empty()) {
            auto* registry_root = m_finalization_registry_cleanup_jobs.take_last();
            auto* registry = reinterpret_cast<JSObject*>(gc_root_cell(registry_root));
            auto completion = run_in_realm([&] { return js_weak_finalization_registry_cleanup(vm(), registry, nullptr); });
            if (completion.variant == JS_COMPLETION_THROW)
                m_finalization_registry_cleanup_jobs.append(registry_root);
            else
                gc_root_destroy(registry_root);
        }
    }

    u64 set_timeout(JSObject* callback, i64 delay)
    {
        auto sequence_number = ++m_timer_sequence_number;
        m_timers.append({ gc_root_create(reinterpret_cast<GCCell*>(callback)), delay, sequence_number });
        return sequence_number;
    }

    ByteString filename_of(void* script_or_module) const
    {
        return m_filenames.get(script_or_module).value_or(".");
    }

    void forget_filename(void* script_or_module) { m_filenames.remove(script_or_module); }

    // The module of a file, which the host loads once per process and keeps alive, as the runtime's own loader does.
    JSModule* module_of_file(ByteString const& filename) const
    {
        auto root = m_module_map.get(filename);
        return root.has_value() ? reinterpret_cast<JSModule*>(gc_root_cell(*root)) : nullptr;
    }

    void store_module(ByteString const& filename, JSModule* module)
    {
        if (!m_module_map.contains(filename))
            m_module_map.set(filename, gc_root_create(reinterpret_cast<GCCell*>(module)));
    }

    size_t spin_count() const { return m_spin_count; }

private:
    static JSObject* create_global_object(void* context, JSRealm* realm);
    static void define_global_functions(JSVM*, JSRealm*, JSObject* global);

    void finish_module_load(PendingModuleLoad const&);

    // The hooks, which the VM calls with the host as their data.

    static void enqueue_promise_job(void* data, JSVM*, JSPromiseJob* job, JSRealm*)
    {
        static_cast<Host*>(data)->m_microtasks.append(gc_root_create(reinterpret_cast<GCCell*>(job)));
    }

    static bool promise_job_queue_is_empty(void* data, JSVM*)
    {
        return static_cast<Host*>(data)->m_microtasks.is_empty();
    }

    // Runs at the end of a garbage collection, so it only queues the cleanup.
    static void enqueue_finalization_registry_cleanup_job(void* data, JSVM*, JSObject* registry)
    {
        static_cast<Host*>(data)->m_finalization_registry_cleanup_jobs.append(gc_root_create(reinterpret_cast<GCCell*>(registry)));
    }

    static void get_supported_import_attributes(void*, JSVM*, JSStringSink* attribute_keys)
    {
        for (auto key : { u"type"sv, u"key"sv, u"key1"sv, u"key2"sv, u"default"sv }) {
            auto const* code_units = reinterpret_cast<u16 const*>(key.utf16_span().data());
            attribute_keys->append(attribute_keys->context, code_units, key.length_in_code_units());
        }
    }

    // HostLoadImportedModule: queues a task that loads the module and finishes loading it. The referrer resolves
    // relative specifiers now, as the runtime's own loader does at this point.
    static void load_imported_module(void* data, JSVM* vm, JSImportedModuleReferrer referrer, JSModuleRequest const* module_request, void*, JSImportedModulePayload payload)
    {
        auto& host = *static_cast<Host*>(data);
        ByteString base_filename;
        if (referrer.kind == JS_IMPORTED_MODULE_REFERRER_REALM) {
            auto active = js_execution_context_get_active_script_or_module(vm);
            base_filename = active.tag == JS_LAYOUT_SCRIPT_OR_MODULE_TAG_EMPTY ? ByteString { "."sv } : host.filename_of(active.cell);
        } else {
            base_filename = host.filename_of(referrer.record);
        }
        host.m_pending_module_loads.append({
            .referrer = referrer,
            .module_request = js_module_request_clone(module_request),
            .payload = payload,
            .base_filename = move(base_filename),
            .referrer_root = gc_root_create(static_cast<GCCell*>(referrer.record)),
            .payload_root = gc_root_create(static_cast<GCCell*>(payload.record)),
        });
    }

    // Agent::spin_event_loop_until() of HTML: sets the execution context stack aside, then runs tasks until the goal
    // is met, each in its realm's context.
    static void spin_event_loop_until(void* data, JSVM* vm, JSGoalCondition goal_condition, void* goal_context)
    {
        auto& host = *static_cast<Host*>(data);
        ++host.m_spin_count;
        js_execution_context_save_stack(vm);
        js_execution_context_clear_stack(vm);
        auto was_performing_microtask_checkpoint = exchange(host.m_performing_microtask_checkpoint, false);
        while (!goal_condition(goal_context)) {
            if (!host.m_microtasks.is_empty()) {
                host.perform_microtask_checkpoint();
                continue;
            }
            if (host.run_one_module_load() || host.run_one_timer())
                continue;
            VERIFY_NOT_REACHED();
        }
        host.m_performing_microtask_checkpoint = was_performing_microtask_checkpoint;
        js_execution_context_restore_stack(vm);
    }

    static constexpr JSVmHostHooks s_hooks {
        .ensure_can_add_private_element = nullptr,
        .ensure_can_compile_strings = nullptr,
        .get_code_for_eval = nullptr,
        .promise_rejection_tracker = nullptr,
        .call_job_callback = nullptr,
        .enqueue_finalization_registry_cleanup_job = enqueue_finalization_registry_cleanup_job,
        .enqueue_promise_job = enqueue_promise_job,
        .promise_job_queue_is_empty = promise_job_queue_is_empty,
        .make_job_callback = nullptr,
        .get_import_meta_properties = nullptr,
        .finalize_import_meta = nullptr,
        .get_supported_import_attributes = get_supported_import_attributes,
        .load_imported_module = load_imported_module,
        .unrecognized_date_string = nullptr,
        .resize_array_buffer = nullptr,
        .grow_shared_array_buffer = nullptr,
        .system_utc_epoch_nanoseconds = nullptr,
        .on_unimplemented_property_access = nullptr,
    };

    NonnullOwnPtr<EmbeddedVM> m_embedded_vm;
    OwnPtr<HostRealm> m_realm;
    Vector<GCRoot*> m_microtasks;
    Vector<GCRoot*> m_finalization_registry_cleanup_jobs;
    Vector<PendingModuleLoad> m_pending_module_loads;
    Vector<Timer> m_timers;
    u64 m_timer_sequence_number { 0 };
    HashMap<ByteString, GCRoot*> m_module_map;
    HashMap<void*, ByteString> m_filenames;
    Vector<ByteString> m_log;
    bool m_performing_microtask_checkpoint { false };
    size_t m_spin_count { 0 };
};

void Host::finish_module_load(PendingModuleLoad const& load)
{
    auto specifier = utf8_of(utf16_view_of(js_module_request_specifier(load.module_request)));
    ByteString module_type;
    for (size_t index = 0; index < js_module_request_attribute_count(load.module_request); ++index) {
        auto attribute = js_module_request_attribute(load.module_request, index);
        if (utf16_view_of(attribute.key) == u"type"sv)
            module_type = utf8_of(utf16_view_of(attribute.value));
    }

    auto filename = resolve_module_filename(LexicalPath::absolute_path(LexicalPath::dirname(load.base_filename), specifier), module_type);
    auto finish = [&](JSCompletion result) {
        js_module_finish_loading_imported_module(vm(), load.referrer, load.module_request, load.payload, result);
    };

    if (auto* module = module_of_file(filename)) {
        finish(normal_completion(reinterpret_cast<uintptr_t>(module)));
        return;
    }

    m_log.append(ByteString::formatted("load {}", LexicalPath::basename(filename)));
    auto contents = read_file(filename);
    if (contents.is_error()) {
        finish(throw_error(vm(), JS_ERROR_KIND_SYNTAX_ERROR, ByteString::formatted("Cannot find/open module: '{}'", specifier)));
        return;
    }
    auto source = Utf16String::from_utf8_with_replacement_character(contents.value());

    if (module_type == "json"sv) {
        auto utf16_filename = Utf16String::from_utf8(filename);
        auto completion = js_module_parse_json_module(vm(), realm().realm(), abi_view_of(source.utf16_view()), abi_view_of(utf16_filename.utf16_view()));
        if (completion.variant == JS_COMPLETION_NORMAL)
            store_module(filename, reinterpret_cast<JSModule*>(static_cast<uintptr_t>(completion.payload)));
        finish(completion);
        return;
    }

    ParserErrors errors;
    auto* module = parse_module(source.utf16_view(), filename, errors);
    if (!module) {
        finish(throw_error(vm(), JS_ERROR_KIND_SYNTAX_ERROR, errors.messages.first()));
        return;
    }
    store_module(filename, module);
    finish(normal_completion(reinterpret_cast<uintptr_t>(module)));
}

// The functions of the test-js runner, test-js.cpp's TESTJS_GLOBAL_FUNCTIONs, built on the ABI.

JSCompletion can_parse_source(JSVM* vm)
{
    auto source = to_utf16_string(vm, argument(vm, 0));
    if (source.is_error())
        return source.release_error();
    ParserErrors errors;
    auto sink = errors.sink();
    auto* script = js_script_parse(vm, current_realm(vm), abi_view_of(source.value().utf16_view()), {}, {}, nullptr, 1, &sink);
    return normal_completion(bool_value(script != nullptr));
}

JSCompletion collect_garbage(JSVM* vm)
{
    js_vm_collect_garbage(vm);
    return normal_completion();
}

JSCompletion add_engine_private_property(JSVM* vm)
{
    auto object = js_value_to_object(vm, argument(vm, 0));
    if (object.variant != JS_COMPLETION_NORMAL)
        return object;
    auto* private_symbol = js_symbol_create_private(vm);
    js_object_set_engine_private_property(vm, pointer_of_payload<JSObject>(object), private_symbol, argument(vm, 1));
    return normal_completion();
}

JSCompletion evaluate_source(JSVM* vm)
{
    auto source = to_utf16_string(vm, argument(vm, 0));
    if (source.is_error())
        return source.release_error();
    ParserErrors errors;
    auto sink = errors.sink();
    auto* script = js_script_parse(vm, current_realm(vm), abi_view_of(source.value().utf16_view()), {}, {}, nullptr, 1, &sink);
    if (!script)
        return throw_error(vm, JS_ERROR_KIND_SYNTAX_ERROR, errors.messages.first());
    return js_script_run(vm, script, nullptr);
}

// Runs the host's event loop, without timers, until the promise settles, as the runtime's own run_module() expects
// promise jobs alone to settle it.
JSObject* wait_for(JSObject* promise)
{
    auto& host = Host::the();
    while (true) {
        host.perform_microtask_checkpoint();
        if (js_promise_state(promise) != JS_PROMISE_STATE_PENDING)
            return promise;
        auto loaded_a_module = host.run_one_module_load();
        VERIFY(loaded_a_module);
    }
}

// VM::run_module() of the runtime, which test-js uses: loads the module's graph, links and evaluates it, then runs the
// promise jobs and the cleanup jobs of finalization registries.
JSCompletion evaluate_module(JSVM* vm)
{
    auto& host = Host::the();
    auto path = to_utf16_string(vm, argument(vm, 0));
    if (path.is_error())
        return path.release_error();
    auto filename = utf8_of(path.value().utf16_view());
    auto contents = read_file(filename);
    if (contents.is_error())
        return throw_error(vm, JS_ERROR_KIND_SYNTAX_ERROR, ByteString::formatted("Cannot find/open module: '{}'", filename));
    auto source = Utf16String::from_utf8_with_replacement_character(contents.value());
    ParserErrors errors;
    auto* module = host.parse_module(source.utf16_view(), filename, errors);
    if (!module)
        return throw_error(vm, JS_ERROR_KIND_SYNTAX_ERROR, errors.messages.first());

    // The entry module is in the module map before its graph loads, so that it can import itself.
    auto absolute_filename = resolve_module_filename(LexicalPath::absolute_path("."sv, filename), ""sv);
    if (!host.module_of_file(absolute_filename))
        host.store_module(absolute_filename, module);

    auto* loading = wait_for(js_promise_capability_promise(js_module_load_requested_modules(vm, module, nullptr)));
    if (js_promise_state(loading) == JS_PROMISE_STATE_REJECTED)
        return { js_promise_result(loading), JS_COMPLETION_THROW };
    auto linked = js_module_link(vm, module);
    if (linked.variant != JS_COMPLETION_NORMAL)
        return linked;
    auto evaluation = js_module_evaluate(vm, module);
    if (evaluation.variant != JS_COMPLETION_NORMAL)
        return evaluation;
    auto* evaluated = wait_for(js_promise_capability_promise(pointer_of_payload<JSPromiseCapability>(evaluation)));
    if (js_promise_state(evaluated) == JS_PROMISE_STATE_REJECTED)
        return { js_promise_result(evaluated), JS_COMPLETION_THROW };
    host.run_until_idle();
    host.run_finalization_registry_cleanup_jobs();
    return normal_completion();
}

JSCompletion run_queued_promise_jobs(JSVM*)
{
    Host::the().run_until_idle();
    return normal_completion();
}

JSCompletion clear_kept_objects(JSVM* vm)
{
    js_vm_finish_execution_generation(vm);
    return normal_completion();
}

JSCompletion run_queued_finalization_registry_cleanup_jobs(JSVM*)
{
    Host::the().run_finalization_registry_cleanup_jobs();
    return normal_completion();
}

JSCompletion unsupported(JSVM* vm)
{
    return throw_error(vm, JS_ERROR_KIND_INTERNAL_ERROR, "The end-to-end host does not support this test-js function"sv);
}

bool has_lexical_environment(void*, JSExecutionContext* context)
{
    return field_at<void*>(context, JS_LAYOUT_EXECUTION_CONTEXT_LEXICAL_ENVIRONMENT_OFFSET) != nullptr;
}

// markAsGarbage(name): resolves the name the way a script does, takes its value away from the binding and lets the
// heap collect it, through the Reference that js_environment_resolve_binding() describes.
JSCompletion mark_as_garbage(JSVM* vm)
{
    auto* name = string_of_value(argument(vm, 0));
    if (!name)
        return throw_error(vm, JS_ERROR_KIND_TYPE_ERROR, ByteString::formatted("{} is not a string", describe(argument(vm, 0))));
    auto name_view = js_string_utf16_view(name);
    auto variable_name = utf8_of(utf16_view_of(name_view));

    auto* context = js_execution_context_last_matching(vm, has_lexical_environment, nullptr);
    if (!context)
        return throw_error(vm, JS_ERROR_KIND_REFERENCE_ERROR, ByteString::formatted("'{}' is not defined", variable_name));
    auto* outer_environment = field_at<JSEnvironment*>(context, JS_LAYOUT_EXECUTION_CONTEXT_LEXICAL_ENVIRONMENT_OFFSET);

    auto resolved = js_environment_resolve_binding(vm, name_view, false, outer_environment);
    if (resolved.variant != JS_COMPLETION_NORMAL)
        return resolved;
    auto* base = reinterpret_cast<JSEnvironment*>(static_cast<uintptr_t>(resolved.payload));
    if (!base)
        return throw_error(vm, JS_ERROR_KIND_REFERENCE_ERROR, ByteString::formatted("'{}' is not defined", variable_name));

    auto value = js_environment_get_binding_value(vm, base, name_view, false);
    if (value.variant != JS_COMPLETION_NORMAL)
        return value;
    if (!js_value_can_be_held_weakly(value.payload))
        return throw_error(vm, JS_ERROR_KIND_TYPE_ERROR, ByteString::formatted("Variable with name {} cannot be held weakly", variable_name));

    auto put = js_environment_set_mutable_binding(vm, base, name_view, js_undefined, false);
    if (put.variant != JS_COMPLETION_NORMAL)
        return put;
    auto deleted = js_environment_delete_binding(vm, base, name_view);
    if (deleted.variant != JS_COMPLETION_NORMAL)
        return deleted;
    gc_heap_uproot_cell(js_vm_heap(vm), static_cast<GCCell*>(cell_of(value.payload)));
    return normal_completion();
}

JSCompletion cleanup_finalization_registry(JSVM* vm)
{
    auto registry_value = argument(vm, 0);
    auto* registry = object_of_value(registry_value);
    if (!registry || !js_object_is_subclass_of(registry, JS_LAYOUT_CLASS_ID_FINALIZATION_REGISTRY))
        return throw_error(vm, JS_ERROR_KIND_TYPE_ERROR, "Not an object of type FinalizationRegistry"sv);
    auto callback = argument(vm, 1);
    if (argument_count(vm) > 1 && !js_value_is_function(callback))
        return throw_error(vm, JS_ERROR_KIND_TYPE_ERROR, ByteString::formatted("{} is not a function", describe(callback)));
    JSJobCallback* cleanup_callback = nullptr;
    if (callback != js_undefined)
        cleanup_callback = js_realm_job_callback_create(vm, object_of_value(callback), nullptr);
    auto completion = js_weak_finalization_registry_cleanup(vm, registry, cleanup_callback);
    if (completion.variant != JS_COMPLETION_NORMAL)
        return completion;
    return normal_completion();
}

JSCompletion detach_array_buffer(JSVM* vm)
{
    auto* buffer = object_of_value(argument(vm, 0));
    if (!buffer || !js_array_buffer_is_array_buffer(buffer))
        return throw_error(vm, JS_ERROR_KIND_TYPE_ERROR, "Not an object of type ArrayBuffer"sv);
    auto completion = js_array_buffer_detach(vm, buffer, argument(vm, 1));
    if (completion.variant != JS_COMPLETION_NORMAL)
        return completion;
    return normal_completion(js_null);
}

// toUTF8Bytes(string): a Uint8Array of the string's WTF-8 bytes.
JSCompletion to_utf8_bytes(JSVM* vm)
{
    auto string = to_utf16_string(vm, argument(vm, 0));
    if (string.is_error())
        return string.release_error();
    auto bytes = utf8_of(string.value().utf16_view());
    auto created = js_typed_array_create(vm, current_realm(vm), JS_LAYOUT_TYPED_ARRAY_KIND_UINT8, bytes.length());
    if (created.variant != JS_COMPLETION_NORMAL)
        return created;
    auto* typed_array = pointer_of_payload<JSObject>(created);
    size_t byte_length = 0;
    auto* data = js_array_buffer_data(js_typed_array_viewed_array_buffer(typed_array), &byte_length);
    VERIFY(byte_length == bytes.length());
    if (byte_length > 0)
        __builtin_memcpy(data, bytes.characters(), byte_length);
    return normal_completion(value_of_object(typed_array));
}

// createDefaultTypedArray(typedArray, length): a new typed array of the kind of the given one.
JSCompletion create_default_typed_array(JSVM* vm)
{
    auto object = js_value_to_object(vm, argument(vm, 0));
    if (object.variant != JS_COMPLETION_NORMAL)
        return object;
    auto* typed_array = pointer_of_payload<JSObject>(object);
    if (!js_typed_array_is_typed_array(typed_array))
        return throw_error(vm, JS_ERROR_KIND_TYPE_ERROR, "Not an object of type TypedArray"sv);
    u64 length = 0;
    auto converted = js_value_to_index(vm, argument(vm, 1), &length);
    if (converted.variant != JS_COMPLETION_NORMAL)
        return converted;
    if (length > NumericLimits<u32>::max() || length > static_cast<u64>(NumericLimits<i32>::max()) / js_typed_array_element_size(typed_array))
        return throw_error(vm, JS_ERROR_KIND_RANGE_ERROR, "Invalid typed array length"sv);
    return js_typed_array_create(vm, current_realm(vm), js_typed_array_kind(typed_array), static_cast<u32>(length));
}

// The host's own functions.

JSCompletion host_print(JSVM* vm)
{
    StringBuilder line;
    for (size_t index = 0; index < argument_count(vm); ++index) {
        auto string = to_utf16_string(vm, argument(vm, index));
        if (string.is_error())
            return string.release_error();
        if (index > 0)
            line.append(' ');
        line.append(utf8_of(string.value().utf16_view()));
    }
    Host::the().log().append(line.to_byte_string());
    return normal_completion();
}

JSCompletion host_set_timeout(JSVM* vm)
{
    auto* callback = object_of_value(argument(vm, 0));
    if (!callback || !js_value_is_function(argument(vm, 0)))
        return throw_error(vm, JS_ERROR_KIND_TYPE_ERROR, "setTimeout() takes a function"sv);
    double delay = 0;
    auto converted = js_value_to_double(vm, argument(vm, 1), &delay);
    if (converted.variant != JS_COMPLETION_NORMAL)
        return converted;
    auto id = Host::the().set_timeout(callback, static_cast<i64>(delay));
    return normal_completion(int32_value(static_cast<i32>(id)));
}

JSCompletion report_test(JSVM*)
{
    return normal_completion();
}

struct GlobalFunction {
    StringView name;
    JSNativeFunction behaviour;
};

void Host::define_global_functions(JSVM* vm, JSRealm* realm, JSObject* global)
{
    auto global_name = "global"_utf16_fly_string;
    auto global_key = key_of(global_name);
    js_object_define_direct_property(vm, global, &global_key, value_of_object(global), JS_ATTRIBUTE_ENUMERABLE);
    auto report_test_name = "__reportTest__"_utf16_fly_string;
    auto report_test_key = key_of(report_test_name);
    js_object_define_native_function(vm, global, realm, &report_test_key, native_function<report_test>(), 2, default_attributes);

    // In the order test-js.cpp registers them. It keeps them in a HashMap, and defines them in the order of its
    // buckets, as this does.
    static Array<GlobalFunction, 16> const test_js_functions {
        GlobalFunction { "canParseSource"sv, native_function<can_parse_source>() },
        GlobalFunction { "gc"sv, native_function<collect_garbage>() },
        GlobalFunction { "addEnginePrivateProperty"sv, native_function<add_engine_private_property>() },
        GlobalFunction { "evaluateSource"sv, native_function<evaluate_source>() },
        GlobalFunction { "evaluateModule"sv, native_function<evaluate_module>() },
        GlobalFunction { "runQueuedPromiseJobs"sv, native_function<run_queued_promise_jobs>() },
        GlobalFunction { "clearKeptObjects"sv, native_function<clear_kept_objects>() },
        GlobalFunction { "runQueuedFinalizationRegistryCleanupJobs"sv, native_function<run_queued_finalization_registry_cleanup_jobs>() },
        GlobalFunction { "getWeakSetSize"sv, native_function<unsupported>() },
        GlobalFunction { "getWeakMapSize"sv, native_function<unsupported>() },
        GlobalFunction { "markAsGarbage"sv, native_function<mark_as_garbage>() },
        GlobalFunction { "cleanupFinalizationRegistry"sv, native_function<cleanup_finalization_registry>() },
        GlobalFunction { "detachArrayBuffer"sv, native_function<detach_array_buffer>() },
        GlobalFunction { "setTimeZone"sv, native_function<unsupported>() },
        GlobalFunction { "toUTF8Bytes"sv, native_function<to_utf8_bytes>() },
        GlobalFunction { "createDefaultTypedArray"sv, native_function<create_default_typed_array>() },
    };
    HashTable<Utf16FlyString> names;
    for (auto const& function : test_js_functions)
        names.set(Utf16FlyString::from_utf8(function.name));
    for (auto const& name : names) {
        for (auto const& function : test_js_functions) {
            if (Utf16FlyString::from_utf8(function.name) != name)
                continue;
            auto key = key_of(name);
            js_object_define_native_function(vm, global, realm, &key, function.behaviour, 1, default_attributes);
        }
    }

    // Not enumerable, so that the global object's keys are those of the test-js runner's.
    auto print_name = "print"_utf16_fly_string;
    auto print_key = key_of(print_name);
    js_object_define_native_function(vm, global, realm, &print_key, native_function<host_print>(), 0, hidden_attributes);
    auto set_timeout_name = "setTimeout"_utf16_fly_string;
    auto set_timeout_key = key_of(set_timeout_name);
    js_object_define_native_function(vm, global, realm, &set_timeout_key, native_function<host_set_timeout>(), 2, hidden_attributes);
}

constexpr JSHostClass global_object_class {
    .abi_version = JS_HOST_ABI_VERSION,
    .kind = JS_HOST_CLASS_OBJECT,
    .reserved = 0,
    .flags = JS_HOST_CLASS_IS_GLOBAL_OBJECT,
    .name = "HostGlobalObject",
    .name_length = 16,
    .parent = nullptr,
    .hooks = nullptr,
    .user_data = nullptr,
};

JSObject* Host::create_global_object(void* context, JSRealm* realm)
{
    auto& host = *static_cast<Host*>(context);
    auto* vm = host.vm();
    auto* global = js_host_object_create(vm, realm, &global_object_class, js_realm_intrinsic(vm, realm, JS_INTRINSIC_OBJECT_PROTOTYPE), nullptr, nullptr);
    define_global_functions(vm, realm, global);
    return global;
}

// The result of every test of a file, keyed as test-js-runtime-rust --per-file keys them, and what a test that
// failed reported.
struct FileResults {
    OrderedHashMap<ByteString, ByteString> results;
    HashMap<ByteString, ByteString> details;
};

ByteString test_root()
{
    return ByteString::formatted("{}/Tests/LibJS/Runtime", LADYBIRD_SOURCE_DIR);
}

// The per-file run of test-js: test-common.js and then the file, each in the file's own realm, then the results the
// harness recorded in __TestResults__.
void run_test_file(Host& host, ByteString const& path, FileResults& out)
{
    auto relative_path = LexicalPath::relative_path(path, test_root()).value();
    host.create_realm();
    auto* vm = host.vm();

    auto common_path = LexicalPath::join(test_root(), "test-common.js"sv).string();
    auto common_source = Utf16String::from_utf8(MUST(read_file(common_path)));
    ParserErrors common_errors;
    auto* common_script = host.parse_script(common_source.utf16_view(), common_path, common_errors);
    VERIFY(common_script);
    VERIFY(host.run_script(common_script).variant == JS_COMPLETION_NORMAL);

    auto file_source = Utf16String::from_utf8(MUST(read_file(path)));
    ParserErrors file_errors;
    auto* file_script = host.parse_script(file_source.utf16_view(), path, file_errors);
    if (!file_script) {
        // Like the runner, report no tests for a file that does not parse.
        warnln("{}: {}", relative_path, file_errors.messages.first());
        return;
    }
    auto top_level_completion = host.run_script(file_script);

    auto results_name = "__TestResults__"_utf16_fly_string;
    auto results_key = key_of(results_name);
    JSOwnedUtf16String json = 0;
    auto stringified = host.run_in_realm([&] {
        auto* global = js_realm_global_object(host.realm().realm());
        auto results = js_object_get(vm, global, &results_key);
        VERIFY(results.variant == JS_COMPLETION_NORMAL);
        return js_json_stringify(vm, results.payload, js_undefined, js_undefined, &json);
    });
    VERIFY(stringified.variant == JS_COMPLETION_NORMAL && stringified.payload == 1);
    auto parsed = MUST(JsonValue::from_string(utf8_of_owned(json)));

    parsed.as_object().for_each_member([&](String const& suite_name, JsonValue const& suite) {
        auto suite_key = suite_name == "__$$TOP_LEVEL$$__"sv ? ByteString {} : suite_name.to_byte_string();
        suite.as_object().for_each_member([&](String const& test_name, JsonValue const& test) {
            auto result = test.as_object().get_string("result"sv).value();
            auto key = ByteString::formatted("{}/{}::{}", relative_path, suite_key, test_name);
            if (result == "pass"sv) {
                out.results.set(key, "PASSED");
            } else if (result == "fail"sv) {
                out.results.set(key, "FAILED");
                out.details.set(key, test.as_object().get_string("details"sv).value_or({}).to_byte_string());
            } else if (result == "xfail"sv) {
                out.results.set(key, "XFAIL");
            } else {
                out.results.set(key, "SKIPPED");
            }
        });
    });

    if (top_level_completion.variant == JS_COMPLETION_THROW) {
        auto key = ByteString::formatted("{}/<top-level>::<top-level>", relative_path);
        out.results.set(key, "FAILED");
        out.details.set(key, describe(top_level_completion.payload));
    }

    // Whatever the file left for later runs too, though the results are in.
    host.run_timers();
    host.forget_filename(common_script);
    host.forget_filename(file_script);
}

// The test files of the slice: every .js file below these paths of the test root, as the runner finds them.
constexpr Array slice_paths {
    "builtins/AggregateError/"sv,
    "builtins/AsyncDisposableStack/"sv,
    "builtins/AsyncGenerator/"sv,
    "builtins/ArrayBuffer/"sv,
    "builtins/Error/"sv,
    "builtins/FinalizationRegistry/"sv,
    "builtins/Iterator/"sv,
    "builtins/JSON/"sv,
    "builtins/Map/"sv,
    "builtins/Promise/"sv,
    "builtins/Proxy/"sv,
    "builtins/Reflect/"sv,
    "builtins/Set/"sv,
    "builtins/TypedArray/"sv,
    "builtins/WeakRef/"sv,
    "iterators/"sv,
    "modules/"sv,
    "syntax/async-await.js"sv,
    "syntax/async-generators.js"sv,
    "syntax/dynamic-import-usage.js"sv,
    "syntax/generators.js"sv,
    "async-this-value.js"sv,
};

Vector<ByteString> slice_test_files()
{
    Vector<ByteString> files;
    Vector<ByteString> directories { test_root() };
    while (!directories.is_empty()) {
        auto directory = directories.take_last();
        Core::DirIterator entries(directory, Core::DirIterator::SkipParentAndBaseDir);
        while (entries.has_next()) {
            auto path = entries.next_full_path();
            if (is_directory(path)) {
                directories.append(path);
                continue;
            }
            if (!path.ends_with(".js"sv) || path.ends_with("test-common.js"sv))
                continue;
            auto relative_path = LexicalPath::relative_path(path, test_root()).value();
            if (any_of(slice_paths, [&](auto prefix) { return relative_path.starts_with(prefix); }))
                files.append(path);
        }
    }
    quick_sort(files);
    return files;
}

// What test-js-runtime-rust reports for the same files.
OrderedHashMap<ByteString, ByteString> results_of_the_rust_runner()
{
    StringBuilder command;
    command.appendff("'{}' --per-file", TEST_JS_RUNTIME_RUST);
    for (auto path : slice_paths)
        command.appendff(" --filter '{}'", path);
    command.appendff(" '{}' '{}'", test_root(), LexicalPath::join(test_root(), "test-common.js"sv).string());

    auto* pipe = popen(command.to_byte_string().characters(), "r");
    VERIFY(pipe);
    StringBuilder output;
    char buffer[4096];
    while (auto read = fread(buffer, 1, sizeof(buffer), pipe))
        output.append(StringView { buffer, read });
    pclose(pipe);

    OrderedHashMap<ByteString, ByteString> results;
    auto parsed = MUST(JsonValue::from_string(output.string_view().trim_whitespace()));
    parsed.as_object().get_object("results"sv)->for_each_member([&](String const& key, JsonValue const& result) {
        results.set(key.to_byte_string(), result.as_string().to_byte_string());
    });
    return results;
}

}

TEST_CASE(the_event_loop_of_the_host_runs_microtasks_then_module_loads_then_timers)
{
    auto directory = ByteString::formatted("/tmp/test-embedding-host-{}", getpid());
    VERIFY(mkdir(directory.characters(), 0700) == 0);
    auto write_file = [&](StringView name, StringView contents) {
        auto file = MUST(Core::File::open(LexicalPath::join(directory, name).string(), Core::File::OpenMode::Write));
        MUST(file->write_until_depleted(contents.bytes()));
    };
    write_file("a.mjs"sv, "import { b } from './b.mjs'; print('a evaluated with ' + b); export const a = 1;"sv);
    write_file("b.mjs"sv, "print('b evaluated'); export const b = 2; await null; print('b resumed');"sv);
    ScopeGuard remove_files = [&] {
        (void)unlink(LexicalPath::join(directory, "a.mjs"sv).string().characters());
        (void)unlink(LexicalPath::join(directory, "b.mjs"sv).string().characters());
        (void)rmdir(directory.characters());
    };

    Host host;
    host.create_realm();
    auto source = Utf16String::from_utf8(ByteString::formatted(R"~~~(
        print("script start");
        setTimeout(() => print("timeout 2"), 2);
        setTimeout(() => {{
            print("timeout 1");
            Promise.resolve().then(() => print("microtask after timeout 1"));
        }}, 1);
        Promise.resolve().then(() => print("microtask 1"));
        import("{}/a.mjs").then(namespace => print("imported a = " + namespace.a));
        (async () => {{ await null; print("async resumed"); }})();

        // DisposeResources awaits in native code, which spins the host's event loop until the promise settles.
        const stack = new AsyncDisposableStack();
        stack.defer(async () => {{ await null; print("deferred disposal"); }});
        stack.disposeAsync().then(() => print("disposed"));
        print("script end");
    )~~~",
        directory));
    ParserErrors errors;
    auto* script = host.parse_script(source.utf16_view(), "host-check.js", errors);
    VERIFY(script);
    EXPECT_EQ(host.run_script(script).variant, JS_COMPLETION_NORMAL);
    host.run_timers();

    // The spin during the script runs the microtasks queued so far and those the disposal queues, until the disposal
    // is done. The module loads, which are tasks, wait for the script to end.
    Vector<ByteString> expected {
        "script start",
        "microtask 1",
        "async resumed",
        "deferred disposal",
        "script end",
        "disposed",
        "load a.mjs",
        "load b.mjs",
        "b evaluated",
        "b resumed",
        "a evaluated with 2",
        "imported a = 1",
        "timeout 1",
        "microtask after timeout 1",
        "timeout 2",
    };
    EXPECT_EQ(host.log(), expected);
    EXPECT(host.spin_count() > 0);
}

TEST_CASE(property_keys_made_from_the_hosts_own_fly_strings_find_the_runtimes_properties)
{
    Host host;
    auto& realm = host.create_realm();
    auto* vm = host.vm();
    host.run_in_realm([&] {
        auto* array = js_array_create_from(vm, realm.realm(), nullptr, 0);
        auto length_name = "length"_utf16_fly_string;
        auto length_key = key_of(length_name);
        EXPECT_EQ(js_object_get(vm, array, &length_key).payload, int32_value(0));

        auto* name = js_string_create_from_utf8(vm, reinterpret_cast<u8 const*>("prototype"), 9);
        auto runtime_key = js_string_to_property_key(vm, name);
        auto prototype_name = "prototype"_utf16_fly_string;
        EXPECT_EQ(runtime_key.bits, key_of(prototype_name).bits);
        // The runtime's key owns a reference to the fly string, which this gives back.
        Utf16FlyString::unref_raw(runtime_key.bits);
    });
}

TEST_CASE(runtime_tests_have_the_results_of_the_rust_runner)
{
    auto test_files = slice_test_files();
    EXPECT(test_files.size() >= 150);

    VERIFY(chdir(test_root().characters()) == 0);
    FileResults host_results;
    {
        Host host;
        for (auto const& path : test_files)
            run_test_file(host, path, host_results);
    }

    auto rust_results = results_of_the_rust_runner();
    size_t mismatches = 0;
    for (auto const& [key, rust_result] : rust_results) {
        auto host_result = host_results.results.get(key);
        if (host_result == rust_result)
            continue;
        ++mismatches;
        warnln("{}: the host reports {}, test-js-runtime-rust {}", key, host_result.value_or("nothing"), rust_result);
        if (auto details = host_results.details.get(key); details.has_value())
            warnln("    {}", *details);
    }
    for (auto const& [key, host_result] : host_results.results) {
        if (rust_results.contains(key))
            continue;
        ++mismatches;
        warnln("{}: the host reports {}, test-js-runtime-rust nothing", key, host_result);
    }
    outln("{} tests in {} files, {} by the host, {} with different results", rust_results.size(), test_files.size(), host_results.results.size(), mismatches);
    EXPECT(rust_results.size() >= 1000);
    EXPECT_EQ(mismatches, 0u);
}

# The LibJS target is the C++ API that LibJS's users build against, implemented over the Rust runtime's embedding ABI.
# It links the runtime with the runtime's symbols hidden except for the embedding ABI, so every process that links LibJS
# has exactly one copy of the runtime.

add_subdirectory(Rust)

# The optimizing JIT's compiler, a pure Rust crate that only works on plain data. Its unit tests run without the rest of
# LibJS.
if (ENABLE_JS_JIT)
    test_rust_crate(MANIFEST_PATH JIT/Rust/Cargo.toml CRATE_NAME libjs_jit)
endif()

set(SOURCES
    Console.cpp
    CyclicModule.cpp
    Debugger.cpp
    DecodedBytecodeCache.cpp
    Heap/Cell.cpp
    Module.cpp
    ParserError.cpp
    Print.cpp
    Runtime/AbstractOperations.cpp
    Runtime/Agent.cpp
    Runtime/Array.cpp
    Runtime/ArrayBuffer.cpp
    Runtime/BigInt.cpp
    Runtime/BigIntObject.cpp
    Runtime/BooleanObject.cpp
    Runtime/Completion.cpp
    Runtime/ConsoleObject.cpp
    Runtime/DataView.cpp
    Runtime/Date.cpp
    Runtime/DeclarativeEnvironment.cpp
    Runtime/Environment.cpp
    Runtime/Error.cpp
    Runtime/ErrorData.cpp
    Runtime/ErrorTypes.cpp
    Runtime/ExecutionContext.cpp
    Runtime/FinalizationRegistry.cpp
    Runtime/FunctionEnvironment.cpp
    Runtime/FunctionObject.cpp
    Runtime/GlobalEnvironment.cpp
    Runtime/HostArray.cpp
    Runtime/HostFunction.cpp
    Runtime/HostModule.cpp
    Runtime/HostObject.cpp
    Runtime/Intrinsics.cpp
    Runtime/Iterator.cpp
    Runtime/JobCallback.cpp
    Runtime/JSONObject.cpp
    Runtime/Map.cpp
    Runtime/MapIterator.cpp
    Runtime/ModuleEnvironment.cpp
    Runtime/NativeFunction.cpp
    Runtime/NumberObject.cpp
    Runtime/Object.cpp
    Runtime/ObjectEnvironment.cpp
    Runtime/PrimitiveString.cpp
    Runtime/Promise.cpp
    Runtime/PromiseCapability.cpp
    Runtime/PromiseJob.cpp
    Runtime/PropertyDescriptor.cpp
    Runtime/Realm.cpp
    Runtime/Reference.cpp
    Runtime/RegExpObject.cpp
    Runtime/Set.cpp
    Runtime/SetIterator.cpp
    Runtime/StringObject.cpp
    Runtime/Symbol.cpp
    Runtime/TypedArray.cpp
    Runtime/Value.cpp
    Runtime/VM.cpp
    Script.cpp
    ScriptCompilation.cpp
    SourceCode.cpp
    SourceTextModule.cpp
    SyntaxHighlighter.cpp
    SyntheticModule.cpp
    ToolEntryPoints.cpp
)

ladybird_lib(LibJS js EXPLICIT_SYMBOL_EXPORT)

target_link_libraries(LibJS PRIVATE LibCore LibCrypto LibGC LibTextCodec)

# JS::SyntaxHighlighter is a LibSyntax highlighter.
target_link_libraries(LibJS PRIVATE LibSyntax)

# Link LibUnicode publicly to ensure ICU data (which is in libicudata.a) is available in any process using LibJS.
target_link_libraries(LibJS PUBLIC LibUnicode)

target_link_libraries(LibJS PUBLIC JSClangPlugin)

# A shared LibJS exports its own JS_API functions and the functions of the embedding ABI, which embedders written in
# Rust call. The libjs_rust build script lists them for each linker, and every other symbol of the runtime stays local.
target_link_libraries(LibJS PRIVATE libjs_rust)
if (BUILD_SHARED_LIBS)
    set(libjs_exports_directory "${CMAKE_BINARY_DIR}/Libraries/LibJS/Embedding")
    if (WIN32)
        set(libjs_exports "${libjs_exports_directory}/LibJSExports.def")
        target_link_options(LibJS PRIVATE "LINKER:/DEF:${libjs_exports}")
    elseif (APPLE)
        set(libjs_exports "${libjs_exports_directory}/LibJSExports.exp")
        target_link_options(LibJS PRIVATE "LINKER:-exported_symbols_list,${libjs_exports}")
    else()
        set(libjs_exports "${libjs_exports_directory}/LibJSExports.map")
        target_link_options(LibJS PRIVATE "LINKER:--version-script=${libjs_exports}")
    endif()
    set_property(TARGET LibJS APPEND PROPERTY LINK_DEPENDS "${libjs_exports}")
endif()

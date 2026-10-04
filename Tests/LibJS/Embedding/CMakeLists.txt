# The embedding tests run the Rust runtime through its C ABI alone. They link the runtime's static library and LibGC
# but never the C++ LibJS, whose interpreter exports the same symbols.
file(GLOB EMBEDDING_TEST_SOURCES CONFIGURE_DEPENDS "${CMAKE_CURRENT_SOURCE_DIR}/test-embedding-*.cpp")
foreach(source IN LISTS EMBEDDING_TEST_SOURCES)
    ladybird_test("${source}" LibJS/Embedding LIBS libjs_runtime_rust LibGC)
endforeach()

# The headers are C as well, which this translation unit checks against the standard without extensions.
target_sources(test-embedding-headers PRIVATE test-embedding-headers.c)
set_target_properties(test-embedding-headers PROPERTIES C_STANDARD 11 C_STANDARD_REQUIRED ON C_EXTENSIONS OFF)
if (NOT MSVC)
    set_source_files_properties(test-embedding-headers.c PROPERTIES COMPILE_OPTIONS -pedantic-errors)
endif()

# The end-to-end host runs runtime tests of Tests/LibJS and compares its results with those of the Rust runtime's
# own test-js runner.
target_compile_definitions(test-embedding-host-end-to-end PRIVATE
    LADYBIRD_SOURCE_DIR="${LADYBIRD_SOURCE_DIR}"
    TEST_JS_RUNTIME_RUST="$<TARGET_FILE:test-js-runtime-rust>")
add_dependencies(test-embedding-host-end-to-end test-js-runtime-rust)

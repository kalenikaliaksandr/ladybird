ladybird_utility(test-js-runtime-rust SOURCES test-js-runtime-rust.cpp LIBS LibJS LibMain)

ladybird_utility(test-js SOURCES test-js-main.cpp LIBS LibMain)
ladybird_windows_bin(test-js CONSOLE)
target_compile_definitions(test-js PRIVATE TEST_JS_PYTHON="${Python3_EXECUTABLE}")

add_custom_target(test-js-script DEPENDS "${CMAKE_BINARY_DIR}/bin/test-js.py")
add_custom_command(
    OUTPUT "${CMAKE_BINARY_DIR}/bin/test-js.py"
    COMMAND "${CMAKE_COMMAND}" -E copy "${ladybird_SOURCE_DIR}/Tests/LibJS/test-js.py" "${CMAKE_BINARY_DIR}/bin/test-js.py"
    DEPENDS "${ladybird_SOURCE_DIR}/Tests/LibJS/test-js.py"
)
add_dependencies(test-js test-js-runtime-rust test-js-script js-rust)
add_test(NAME test-js COMMAND test-js --js $<TARGET_FILE:js-rust> --runtime $<TARGET_FILE:test-js-runtime-rust>)
set_tests_properties(test-js PROPERTIES ENVIRONMENT "LADYBIRD_SOURCE_DIR=${LADYBIRD_SOURCE_DIR}")

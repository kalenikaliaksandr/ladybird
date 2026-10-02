# Runs a few scripts through js-rust: cmake -DJS_RUST=<path to js-rust> -P js-rust-smoke.cmake

function(run_js_rust)
    execute_process(
        COMMAND "${JS_RUST}" ${ARGN}
        OUTPUT_VARIABLE output
        ERROR_VARIABLE error
        RESULT_VARIABLE result
    )
    set(output "${output}" PARENT_SCOPE)
    set(error "${error}" PARENT_SCOPE)
    set(result "${result}" PARENT_SCOPE)
endfunction()

function(expect_result expected_output)
    run_js_rust(${ARGN})
    if (NOT result EQUAL 0 OR NOT output STREQUAL "${expected_output}\n")
        message(FATAL_ERROR "js-rust ${ARGN}: exited with '${result}', printed '${output}', expected '${expected_output}'\n${error}")
    endif()
endfunction()

expect_result("2" -i -l -c "1 + 1")
expect_result("2" -i -l -g -c "1 + 1")
expect_result("5.75" -i -l -c "1.5 * 4 - 0.25")
expect_result("\"a\\nb\"" -i -l -c "'a\\n' + 'b'")
expect_result("a" -i -l -r -c "'a'")
expect_result("31n" -i -l -c "0x1fn")

expect_result("42" -i -l -c "function f(a) { return a * 2 } f(21)")
expect_result("Object{ \"a\": [ 1, 2 ] }" -i -l -c "({a: [1, 2]})")

# An uncaught exception prints the error and its stack, and fails the run.
run_js_rust(-i -c "throw new TypeError('x')")
if (NOT result EQUAL 1 OR NOT error MATCHES "Uncaught exception: \n\\[TypeError\\] x")
    message(FATAL_ERROR "js-rust -c throw: exited with '${result}' and printed '${error}'")
endif()

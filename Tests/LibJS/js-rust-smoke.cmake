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

# A runtime function that is not implemented yet stops the process and names itself.
run_js_rust(-c "debugger")
if (result EQUAL 0 OR NOT error MATCHES "asm_slow_path_debugger")
    message(FATAL_ERROR "js-rust -c debugger: exited with '${result}' and printed '${error}'")
endif()

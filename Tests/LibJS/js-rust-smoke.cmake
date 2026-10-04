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

# Like the C++ js, js-rust has no line editor on Windows, where it runs no REPL and its debugger prompt also shows the
# prompts that libedit leaves out when the standard streams are not terminals.
if (CMAKE_HOST_WIN32)
    return()
endif()

# The REPL and the debugger prompt read the standard input with the line editor that the C++ main passes in. The REPL
# keeps its history in HOME.
set(home "${CMAKE_CURRENT_BINARY_DIR}/js-rust-smoke-home")
file(REMOVE_RECURSE "${home}")
file(MAKE_DIRECTORY "${home}")

function(run_js_rust_with_input input)
    file(WRITE "${home}/input" "${input}")
    execute_process(
        COMMAND "${CMAKE_COMMAND}" -E env "HOME=${home}" "${JS_RUST}" ${ARGN}
        INPUT_FILE "${home}/input"
        OUTPUT_VARIABLE output
        ERROR_VARIABLE error
        RESULT_VARIABLE result
    )
    set(output "${output}" PARENT_SCOPE)
    set(error "${error}" PARENT_SCOPE)
    set(result "${result}" PARENT_SCOPE)
endfunction()

run_js_rust_with_input("let x = {\na: 21 }\nx.a * 2\n" -i)
if (NOT result EQUAL 0 OR NOT output STREQUAL "undefined\n42\n" OR NOT EXISTS "${home}/.js-history")
    message(FATAL_ERROR "js-rust REPL: exited with '${result}' and printed '${output}'\n${error}")
endif()

run_js_rust_with_input(".bogus\n.continue\n" -i -l --debug -c "6 * 7")
if (NOT result EQUAL 0 OR NOT output MATCHES "^Paused [^\n]*eval[^\n]* \\(entry\\)\n42\n$"
        OR NOT error STREQUAL "Unknown debugger command '.bogus'. Enter .help for a list of commands.\n")
    message(FATAL_ERROR "js-rust --debug: exited with '${result}', printed '${output}' and '${error}'")
endif()

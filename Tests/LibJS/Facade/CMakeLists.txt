# Conformance tests of the LibJS API that LibJS's users compile against, written against that API alone. With
# LIBJS_RUNTIME=Cpp they build against the C++ runtime's LibJS, and with Rust against the LibJS facade over the Rust
# runtime, and they expect the same results from both.
ladybird_test(TestFacadeValues.cpp LibJS/Facade LIBS LibJS LibGC LibCrypto)
ladybird_test(TestFacadeValuesOnVM.cpp LibJS/Facade LIBS LibJS LibGC LibCrypto)
ladybird_test(TestFacadeVM.cpp LibJS/Facade LIBS LibJS LibGC)
ladybird_test(TestFacadeObjects.cpp LibJS/Facade LIBS LibJS LibGC)

# Where the runtimes knowingly differ, because the Rust runtime fixes a bug of the C++ one, a test expects each one's
# behavior.
if (LIBJS_RUNTIME STREQUAL "Rust")
    target_compile_definitions(TestFacadeVM PRIVATE LIBJS_TESTS_RUN_ON_THE_RUST_RUNTIME)
endif()

# Conformance tests of the LibJS API that LibJS's users compile against, written against that API alone. With
# LIBJS_RUNTIME=Cpp they build against the C++ runtime's LibJS, and with Rust against the LibJS facade over the Rust
# runtime, and they expect the same results from both.
ladybird_test(TestFacadeValues.cpp LibJS/Facade LIBS LibJS LibGC LibCrypto)
ladybird_test(TestFacadeValuesOnVM.cpp LibJS/Facade LIBS LibJS LibGC LibCrypto)
ladybird_test(TestFacadeVM.cpp LibJS/Facade LIBS LibJS LibGC)
ladybird_test(TestFacadeObjects.cpp LibJS/Facade LIBS LibJS LibGC)
ladybird_test(TestFacadeHostObjects.cpp LibJS/Facade LIBS LibJS LibGC)
ladybird_test(TestFacadeOperations.cpp LibJS/Facade LIBS LibJS LibGC LibCrypto)
ladybird_test(TestFacadeExecution.cpp LibJS/Facade LIBS LibJS LibGC)
ladybird_test(TestFacadeScripts.cpp LibJS/Facade LIBS LibJS LibGC LibCore LibThreading)
ladybird_test(TestFacadeBuffers.cpp LibJS/Facade LIBS LibJS LibGC LibCore LibCrypto)
ladybird_test(TestFacadeBuiltins.cpp LibJS/Facade LIBS LibJS LibGC LibCrypto)
ladybird_test(TestFacadeConsole.cpp LibJS/Facade LIBS LibJS LibGC)
ladybird_test(TestFacadeDebugger.cpp LibJS/Facade LIBS LibJS LibGC)
ladybird_test(TestFacadeSyntaxHighlighter.cpp LibJS/Facade LIBS LibJS LibCore LibGfx LibSyntax)
ladybird_test(TestFacadeTimeZoneCache.cpp LibJS/Facade LIBS LibJS LibGC LibUnicode)

# Where the runtimes knowingly differ, because the Rust runtime fixes a bug of the C++ one, a test expects each one's
# behavior.
if (LIBJS_RUNTIME STREQUAL "Rust")
    target_compile_definitions(TestFacadeVM PRIVATE LIBJS_TESTS_RUN_ON_THE_RUST_RUNTIME)
endif()

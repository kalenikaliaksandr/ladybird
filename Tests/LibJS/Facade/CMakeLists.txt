# Conformance tests of the LibJS API that LibJS's users compile against, written against that API alone. With
# LIBJS_RUNTIME=Cpp they build against the C++ runtime's LibJS, and with Rust against the LibJS facade over the Rust
# runtime, and they expect the same results from both.
ladybird_test(TestFacadeValues.cpp LibJS/Facade LIBS LibJS LibGC LibCrypto)

# Creates a VM, which the facade cannot do yet.
if (LIBJS_RUNTIME STREQUAL "Cpp")
    ladybird_test(TestFacadeValuesOnVM.cpp LibJS/Facade LIBS LibJS LibGC LibCrypto)
endif()

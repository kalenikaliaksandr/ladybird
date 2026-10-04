/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! Bytecode cache blobs on this runtime's types, as RustIntegration.cpp, Script.cpp and SourceTextModule.cpp use them
//! in C++: Script and Source Text Module Records materialized from a decoded blob.
//!
//! The executables made from a blob run their bytecode in place in it and keep it alive. A function's executable stays
//! in the blob until the function is first called.

use core::cell::{Ref, RefCell};
use std::collections::HashSet;
use std::rc::Rc;

use crate::bytecode::executable::Executable;
use crate::gc::root::MarkedVec;
use crate::interpreter::vm::Vm;
use crate::layout::cell::Gc;
use crate::parser_error::ParserError;
use crate::runtime::shared_function_instance_data::SharedFunctionInstanceData;
use crate::source_code::SourceCode;
use libjs_rust::bytecode_cache::{DecodedCacheBlob, DecodedCachedExecutableRecord, DecodedExecutableRecord};

/// RustIntegration::DecodedBytecodeCache: a decoded blob, which the records materialized from it share, and which is
/// validated against the source code once.
pub struct DecodedBytecodeCache {
    blob: RefCell<DecodedCacheBlob>,
}

impl DecodedBytecodeCache {
    pub fn new(blob: DecodedCacheBlob) -> Self {
        Self {
            blob: RefCell::new(blob),
        }
    }

    /// Validates the blob for source code of `source_length_in_code_units` code units, and checks nothing else after
    /// that succeeded once.
    pub fn validate(&self, source_length_in_code_units: usize) -> bool {
        self.blob
            .borrow_mut()
            .validate_for_materialization(source_length_in_code_units)
            .is_ok()
    }

    /// The blob, if it passed validation for source code of `source_length_in_code_units` code units.
    pub fn validated_blob(&self, source_length_in_code_units: usize) -> Option<Ref<'_, DecodedCacheBlob>> {
        self.validate(source_length_in_code_units).then(|| self.blob.borrow())
    }
}

/// The error a record that cannot be materialized from a bytecode cache blob reports, as in C++.
pub fn failed_to_materialize_bytecode_cache() -> Vec<ParserError> {
    vec![ParserError {
        message: "Failed to materialize bytecode cache".to_string(),
        line: 0,
        column: 0,
    }]
}

/// ExecutableBacking: what the executables of a Script or Source Text Module Record were made from, and how that
/// changes while a bytecode cache is generated for the record and installed into it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExecutableBacking {
    /// Compiled from source code on the VM's thread.
    Source,
    /// Compiled on another thread.
    HeapBytecode,
    GeneratingFreshCacheFromSource,
    GeneratingFreshCacheFromHeapBytecode,
    /// Materialized from a bytecode cache blob, or with one installed.
    MappedBytecodeCache,
}

impl ExecutableBacking {
    pub fn is_source(self) -> bool {
        matches!(self, Self::Source | Self::GeneratingFreshCacheFromSource)
    }

    pub fn is_heap_bytecode(self) -> bool {
        matches!(self, Self::HeapBytecode | Self::GeneratingFreshCacheFromHeapBytecode)
    }

    pub fn is_mapped_bytecode_cache(self) -> bool {
        self == Self::MappedBytecodeCache
    }
}

/// Every function a record has created so far: those it declares, those its executables create, and so on for the
/// executables of the functions that ran, as the record's SharedFunctionInstanceDataList holds them in C++.
pub fn functions_created_by(
    vm: &Vm,
    declared_functions: impl IntoIterator<Item = Gc<SharedFunctionInstanceData>>,
    executable: Option<Gc<Executable>>,
) -> MarkedVec<'_, Gc<SharedFunctionInstanceData>> {
    let functions = MarkedVec::new(vm);
    let mut seen = HashSet::new();
    let mut executables_to_visit: Vec<Gc<Executable>> = executable.into_iter().collect();
    let mut add_function = |function: Gc<SharedFunctionInstanceData>,
                            executables_to_visit: &mut Vec<Gc<Executable>>| {
        if seen.insert(function) {
            functions.push(function);
            executables_to_visit.extend(function.executable());
        }
    };
    for function in declared_functions {
        add_function(function, &mut executables_to_visit);
    }
    while let Some(executable) = executables_to_visit.pop() {
        for index in 0..executable.shared_function_data_count() {
            let function = executable.shared_function_data(u32::try_from(index).expect("the index fits in u32"));
            add_function(function, &mut executables_to_visit);
        }
    }
    functions
}

/// Whether none of `functions` still has an AST or bytecode compiled ahead of its first call, which a record with a
/// bytecode cache installed only compiles from the blob.
pub fn have_only_bytecode_cache_compile_inputs(functions: &MarkedVec<'_, Gc<SharedFunctionInstanceData>>) -> bool {
    functions
        .to_vec()
        .iter()
        .all(|function| !function.has_function_ast() && !function.has_precompiled_bytecode())
}

/// Creates the executable of a cached record together with the functions its bytecode creates, whose own executables
/// stay in the blob. Returns `None` if the record turns out to be malformed.
pub fn create_executable_and_its_functions(
    vm: &Vm,
    record: &DecodedExecutableRecord,
    source_code: &Rc<SourceCode>,
) -> Option<Gc<Executable>> {
    let functions = MarkedVec::new(vm);
    for function in record.functions()? {
        functions.push(SharedFunctionInstanceData::create_from_bytecode_cache(
            vm,
            &function,
            record.is_strict(),
            source_code,
        ));
    }
    Executable::create_from_bytecode_cache(vm, record, &functions, source_code)
}

/// The executable of a function whose executable stayed in a bytecode cache blob until its first call, as
/// rust_materialize_bytecode_cache_function makes it.
pub fn materialize_cached_function_executable(
    vm: &Vm,
    cached_executable: &DecodedCachedExecutableRecord,
    source_code: &Rc<SourceCode>,
) -> Option<Gc<Executable>> {
    create_executable_and_its_functions(vm, &cached_executable.decode_executable()?, source_code)
}

#[cfg(all(test, libjs_runtime_tests_with_libgc))]
pub(crate) mod tests {
    use core::cell::Cell;
    use core::ffi::c_void;

    use super::*;
    use crate::gc::foreign::ForeignCellSlot;
    use crate::layout::realm::Realm;
    use crate::layout::value::Value;
    use crate::runtime::completion::Must;
    use crate::runtime::error::test_scripts::{run_script, utf8};
    use crate::runtime::source_text_module::SourceTextModule;
    use crate::script::Script;
    use crate::utf16::Utf16View;
    use crate::utilities::initialize_realm;
    use libjs_rust::ast::ProgramType;
    use libjs_rust::bytecode_cache::{
        BytecodeCacheRuntime, DecodedDeclarationMetadata, ForeignBytecodeCacheBlobOwner, decode_blob,
    };
    use libjs_rust::compile::{FunctionPrecompileMode, compile_parsed_program_off_thread, parse};

    const SOURCE_HASH: [u8; 32] = [42; 32];

    /// The bytes of a blob as an embedder holds them, aligned like memory from an allocator, which count how often
    /// they are released.
    struct TestBlobStorage {
        words: Vec<u64>,
        releases: Rc<Cell<usize>>,
    }

    unsafe extern "C" fn release_test_blob(owner: *mut c_void) {
        // SAFETY: The owner is the boxed storage that decode_blob_copy() handed over, which is released once.
        let storage = unsafe { Box::from_raw(owner.cast::<TestBlobStorage>()) };
        storage.releases.set(storage.releases.get() + 1);
    }

    fn source_code_of(filename: &str, source: &str) -> Rc<SourceCode> {
        SourceCode::create(ak::Utf16String::from_utf8(filename), ak::Utf16String::from_utf8(source))
    }

    /// A blob of `source` compiled with all its functions for `runtime`.
    fn serialize_for(source: &str, program_type: ProgramType, runtime: BytecodeCacheRuntime) -> Vec<u8> {
        let code_units: Vec<u16> = source.encode_utf16().collect();
        let parsed = parse(&code_units, program_type, 1);
        assert!(!parsed.has_errors(), "the source parses");
        let compiled = compile_parsed_program_off_thread(parsed, code_units.len(), FunctionPrecompileMode::All);
        let blob =
            libjs_rust::bytecode_cache::serialize_compiled_program(&compiled, program_type, &SOURCE_HASH, runtime);
        compiled.discard();
        blob
    }

    fn serialize(source: &str, program_type: ProgramType) -> Vec<u8> {
        serialize_for(source, program_type, BytecodeCacheRuntime::Rust)
    }

    /// Decodes a copy of `bytes` for this runtime, and calls `inspect` with the decoded cache and the address of the
    /// copy, which `releases` counts the releases of.
    fn decode_blob_copy<R>(
        bytes: &[u8],
        program_type: ProgramType,
        releases: &Rc<Cell<usize>>,
        inspect: impl FnOnce(Option<Rc<DecodedBytecodeCache>>, *const u8) -> R,
    ) -> R {
        let mut storage = Box::new(TestBlobStorage {
            words: vec![0; bytes.len().div_ceil(size_of::<u64>())],
            releases: releases.clone(),
        });
        // SAFETY: The words have room for every byte.
        unsafe { core::ptr::copy_nonoverlapping(bytes.as_ptr(), storage.words.as_mut_ptr().cast::<u8>(), bytes.len()) };
        // SAFETY: The storage, which the owner releases, keeps the words alive and unchanged.
        let view = unsafe { core::slice::from_raw_parts(storage.words.as_ptr().cast::<u8>(), bytes.len()) };
        let base = view.as_ptr();
        let owner = ForeignBytecodeCacheBlobOwner {
            owner: Box::into_raw(storage).cast(),
            clone_owner: None,
            free_owner: release_test_blob,
        };
        // SAFETY: As above.
        let blob = unsafe { decode_blob(view, program_type, &SOURCE_HASH, BytecodeCacheRuntime::Rust, owner) };
        inspect(blob.map(|blob| Rc::new(DecodedBytecodeCache::new(blob))), base)
    }

    fn decode(bytes: &[u8], program_type: ProgramType, releases: &Rc<Cell<usize>>) -> Option<Rc<DecodedBytecodeCache>> {
        decode_blob_copy(bytes, program_type, releases, |cache, _| cache)
    }

    fn script_from_cache(
        vm: &Vm,
        realm: Gc<Realm>,
        cache: &DecodedBytecodeCache,
        source: &str,
    ) -> Result<Gc<Script>, Vec<ParserError>> {
        Script::create_from_bytecode_cache(
            vm,
            realm,
            cache,
            source_code_of("test.js", source),
            "test.js",
            ForeignCellSlot::empty(),
        )
    }

    fn parsed_script(vm: &Vm, realm: Gc<Realm>, source_code: &Rc<SourceCode>) -> Gc<Script> {
        let code_units = source_code.code().to_utf16();
        Script::create_from_parsed(
            vm,
            parse(&code_units, ProgramType::Script, 1),
            source_code.clone(),
            realm,
        )
    }

    fn run(vm: &Vm, script: Gc<Script>) -> String {
        utf8(vm.run_script(script, None).must())
    }

    fn name_of(function: Gc<SharedFunctionInstanceData>) -> String {
        Utf16View::of_fly_string(&function.name()).to_utf8()
    }

    /// The function named `name` among those created so far by the functions and executable given.
    fn function_named(
        vm: &Vm,
        declared_functions: &[Gc<SharedFunctionInstanceData>],
        executable: Gc<Executable>,
        name: &str,
    ) -> Gc<SharedFunctionInstanceData> {
        functions_created_by(vm, declared_functions.iter().copied(), Some(executable))
            .to_vec()
            .into_iter()
            .find(|function| name_of(*function) == name)
            .unwrap_or_else(|| panic!("no function named {name} was created"))
    }

    fn function_of_script(vm: &Vm, script: Gc<Script>, name: &str) -> Gc<SharedFunctionInstanceData> {
        let declared: Vec<_> = script
            .functions_to_initialize()
            .iter()
            .map(|function| function.shared_data)
            .collect();
        function_named(vm, &declared, script.cached_executable(), name)
    }

    /// Where `inner` starts in the blob that starts at `base`.
    fn offset_in_blob(base: *const u8, inner: &[u8]) -> usize {
        inner.as_ptr().addr() - base.addr()
    }

    fn validated_blob_offsets<R>(
        blob: &[u8],
        program_type: ProgramType,
        source_length: usize,
        find: impl FnOnce(&DecodedCacheBlob, *const u8) -> R,
    ) -> R {
        decode_blob_copy(blob, program_type, &Rc::default(), |cache, base| {
            let cache = cache.expect("the blob decodes");
            let blob = cache.validated_blob(source_length).expect("the blob is valid");
            find(&blob, base)
        })
    }

    fn with_byte_flipped(blob: &[u8], offset: usize) -> Vec<u8> {
        let mut corrupted = blob.to_vec();
        corrupted[offset] ^= 0xff;
        corrupted
    }

    /// Scripts that cover what the executables of a cache hold: constants of every kind, regular expressions, classes
    /// with every kind of element, template objects, generators, async functions, the arguments object, direct eval
    /// and function source text.
    const SCRIPTS: &[&str] = &[
        "var big = 123456789012345678901234567890n; var symbol = Symbol.iterator; [big * 2n, typeof symbol, null, \
         undefined, 1.5, -0, 'text', true].join()",
        "function matches(text) { const pattern = /(?<word>a+)b/gu; return [...text.matchAll(pattern)].map(match => \
         match.groups.word + match.index).join(); } matches('aab ab aaab')",
        "class Base { static count = 0; constructor(name) { this.name = name; Base.count++; } get upper() { \
         return this.name.toUpperCase(); } } class Derived extends Base { #secret = 42; static { this.tag = 'static'; } \
         ['computed' + 1]() { return this.#secret; } static create() { return new Derived('derived'); } field = \
         this.upper; } const d = Derived.create(); [d.upper, d.computed1(), Derived.tag, Base.count, d.field].join()",
        "function tag(strings) { return strings; } function same() { return tag`a${1}b`; } [same() === same(), \
         same().raw.join('|')].join()",
        "function* counter(limit) { for (let i = 0; i < limit; ++i) yield i; } [...counter(4)].join()",
        "var log = []; async function work(value) { log.push('start'); await null; log.push(value); return value * 2; } \
         work(21).then(result => log.push(result)); log.join()",
        "function mapped(a, b) { arguments[0] = 'changed'; return [a, arguments.length, eval('a + b')].join(); } \
         mapped('x', 'y')",
        "'use strict'; function outer() { let captured = 'captured'; return () => [this === undefined, captured, \
         (function inner() { return typeof this; })()].join(); } outer()()",
        "label: for (let i = 0; i < 3; ++i) { for (let j = 0; j < 3; ++j) { if (j == 1) continue label; if (i == 2) \
         break label; } } try { throw new TypeError('thrown'); } catch ({ message }) { message } finally { 'ignored' }",
        "let f = function mapped() { return 'hello'; }; [f.toString(), (x => x * 2).toString(), \
         class Point { x = 1 }.toString()].join('|')",
    ];

    #[test]
    fn cached_scripts_run_like_scripts_compiled_from_source() {
        let vm = Vm::create();
        vm.heap().set_should_collect_on_every_allocation(true);
        for source in SCRIPTS {
            let compiled_root_execution_context = initialize_realm(&vm);
            let compiled_result = utf8(run_script(&vm, compiled_root_execution_context.realm(), source).must());
            drop(compiled_root_execution_context);

            let releases = Rc::default();
            let cache = decode(&serialize(source, ProgramType::Script), ProgramType::Script, &releases)
                .expect("the blob decodes");
            let root_execution_context = initialize_realm(&vm);
            let script = script_from_cache(&vm, root_execution_context.realm(), &cache, source)
                .expect("the script materializes");
            assert_eq!(run(&vm, script), compiled_result, "{source}");
            drop(root_execution_context);
        }
    }

    #[test]
    fn a_cached_script_runs_in_place_in_the_blob_and_compiles_its_functions_from_it_on_their_first_call() {
        let vm = Vm::create();
        vm.heap().set_should_collect_on_every_allocation(true);
        let root_execution_context = initialize_realm(&vm);
        let realm = root_execution_context.realm();
        let source = "function declared(a, b) { return function nested() { return a + b; }; }\n\
                      var lazy = function lazy() { return /h(i)/.exec('hi')[1]; };\n\
                      var unused = function unused() { return 'never called'; };\n\
                      [declared(1, 2)(), lazy()].join()";
        let blob = serialize(source, ProgramType::Script);
        let releases = Rc::default();
        let cache = decode(&blob, ProgramType::Script, &releases).expect("the blob decodes");

        let script = script_from_cache(&vm, realm, &cache, source).expect("the script materializes");
        assert!(script.executable_backing().is_mapped_bytecode_cache());
        assert!(script.cached_executable().runs_in_place_in_bytecode_cache_blob());
        let lazy = function_of_script(&vm, script, "lazy");
        let declared = function_of_script(&vm, script, "declared");
        for function in [lazy, declared] {
            assert!(function.executable().is_none() && function.has_cached_bytecode());
            assert!(!function.has_function_ast() && !function.has_precompiled_bytecode());
        }

        assert_eq!(run(&vm, script), "3,i");
        for function in [lazy, declared] {
            let executable = function.executable().expect("the function compiled when it was called");
            assert!(executable.runs_in_place_in_bytecode_cache_blob());
            assert!(!function.has_cached_bytecode());
        }
        let nested = function_of_script(&vm, script, "nested");
        assert!(nested.executable().is_some());
        let unused = function_of_script(&vm, script, "unused");
        assert!(unused.executable().is_none() && unused.has_cached_bytecode());

        drop(cache);
        assert_eq!(releases.get(), 0, "the executables keep the blob alive");
        drop(root_execution_context);
        drop(vm);
        assert_eq!(releases.get(), 1, "the blob is released with the last executable");
    }

    #[test]
    fn records_made_from_one_cache_are_independent() {
        let vm = Vm::create();
        let root_execution_context = initialize_realm(&vm);
        let realm = root_execution_context.realm();
        let source =
            "var calls = (globalThis.calls ?? 0) + 1; function answer() { return 42; } [answer(), calls].join()";
        let cache = decode(
            &serialize(source, ProgramType::Script),
            ProgramType::Script,
            &Rc::default(),
        )
        .expect("the blob decodes");
        let first = script_from_cache(&vm, realm, &cache, source).expect("the script materializes");
        let second = script_from_cache(&vm, realm, &cache, source).expect("the script materializes");
        assert!(first.cached_executable() != second.cached_executable());
        assert_eq!(run(&vm, first), "42,1");
        assert_eq!(run(&vm, second), "42,2");
    }

    #[test]
    fn cached_functions_keep_their_argument_names_for_the_debugger() {
        let vm = Vm::create();
        let root_execution_context = initialize_realm(&vm);
        let realm = root_execution_context.realm();
        let source = "function inspect(first, second) { debugger; return first + second; } inspect(1, 2);";
        let cache = decode(
            &serialize(source, ProgramType::Script),
            ProgramType::Script,
            &Rc::default(),
        )
        .expect("the blob decodes");
        let cached = script_from_cache(&vm, realm, &cache, source).expect("the script materializes");
        run(&vm, cached);
        let compiled = parsed_script(&vm, realm, &source_code_of("test.js", source));
        run(&vm, compiled);

        let argument_names = |script| {
            function_of_script(&vm, script, "inspect")
                .executable()
                .expect("inspect() ran")
                .argument_variable_names
                .iter()
                .map(|name| Utf16View::of_fly_string(name).to_utf8())
                .collect::<Vec<_>>()
        };
        assert_eq!(argument_names(cached), argument_names(compiled));
        assert_eq!(argument_names(cached), ["first", "second"]);
    }

    /// A directory of module files that is removed again when the test is over.
    struct ModuleDirectory(std::path::PathBuf);

    impl ModuleDirectory {
        fn create(name: &str, files: &[(&str, &str)]) -> Self {
            let path = std::env::temp_dir().join(format!("libjs-runtime-rust-cache-{name}-{}", std::process::id()));
            std::fs::create_dir_all(&path).expect("the module directory can be created");
            for (file, contents) in files {
                std::fs::write(path.join(file), contents).expect("the module file can be written");
            }
            Self(path)
        }

        fn path_of(&self, file: &str) -> String {
            self.0.join(file).to_str().expect("the path is UTF-8").to_string()
        }
    }

    impl Drop for ModuleDirectory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn module_from_cache(
        vm: &Vm,
        realm: Gc<Realm>,
        cache: &DecodedBytecodeCache,
        filename: &str,
        source: &str,
    ) -> Result<Gc<SourceTextModule>, Vec<ParserError>> {
        SourceTextModule::create_from_bytecode_cache(
            vm,
            realm,
            filename,
            cache,
            source_code_of(filename, source),
            ForeignCellSlot::empty(),
        )
    }

    #[test]
    fn cached_modules_link_and_run_with_and_without_top_level_await() {
        let directory = ModuleDirectory::create(
            "modules",
            &[
                (
                    "source.mjs",
                    "export function pass() { return 'pass'; }\nexport let counter = 0;\nexport function increment() { \
                     counter++; }\n",
                ),
                (
                    "reexport.mjs",
                    "import { pass as renamed } from './source.mjs';\nexport { renamed as default };\n",
                ),
            ],
        );
        let entry_source = "import renamed from './reexport.mjs';\nimport * as namespace from './source.mjs';\n\
                            import { counter, increment } from './source.mjs';\nincrement();\n\
                            export default function () { return 'anonymous'; }\nexport class Exported {}\n\
                            export const awaited = await Promise.resolve('awaited');\n\
                            globalThis.result = [renamed(), Object.keys(namespace).join('/'), counter, awaited].join();\n";
        let plain_source = "import { pass } from './source.mjs';\nexport const value = pass();\n\
                            globalThis.plain = value;\n";

        let vm = Vm::create();
        let root_execution_context = initialize_realm(&vm);
        let realm = root_execution_context.realm();
        vm.heap().set_should_collect_on_every_allocation(true);
        let releases = Rc::default();

        let entry_cache = decode(
            &serialize(entry_source, ProgramType::Module),
            ProgramType::Module,
            &releases,
        )
        .expect("the blob decodes");
        let entry_filename = directory.path_of("entry.mjs");
        let entry = module_from_cache(&vm, realm, &entry_cache, &entry_filename, entry_source)
            .expect("the module materializes");
        assert!(entry.executable_backing().is_mapped_bytecode_cache());
        assert!(entry.has_top_level_await() && entry.cached_executable().is_none());
        let body = entry
            .top_level_await_shared_data()
            .and_then(|data| data.executable())
            .expect("the module has the body of its async function");
        assert!(body.runs_in_place_in_bytecode_cache_blob());
        assert_eq!(vm.run_module(entry).must(), Value::UNDEFINED);

        let plain_cache = decode(
            &serialize(plain_source, ProgramType::Module),
            ProgramType::Module,
            &releases,
        )
        .expect("the blob decodes");
        let plain_filename = directory.path_of("plain.mjs");
        let plain = module_from_cache(&vm, realm, &plain_cache, &plain_filename, plain_source)
            .expect("the module materializes");
        assert!(!plain.has_top_level_await());
        assert!(
            plain
                .cached_executable()
                .is_some_and(|executable| executable.runs_in_place_in_bytecode_cache_blob())
        );
        vm.run_module(plain).must();
        vm.heap().set_should_collect_on_every_allocation(false);

        assert_eq!(
            utf8(run_script(&vm, realm, "[globalThis.result, globalThis.plain].join(';')").must()),
            "pass,counter/increment/pass,1,awaited;pass"
        );
        drop((entry_cache, plain_cache));
        assert_eq!(releases.get(), 0, "the modules keep their blobs alive");
        drop(root_execution_context);
        drop(vm);
        assert_eq!(releases.get(), 2);
    }

    #[test]
    fn a_cached_module_resolves_a_reexported_import_to_the_module_that_declares_it() {
        let directory = ModuleDirectory::create("reexported", &[("source.mjs", "export function pass() {}\n")]);
        let source = "import { pass as renamed } from './source.mjs'; export { renamed as default };";
        let vm = Vm::create();
        let root_execution_context = initialize_realm(&vm);
        let realm = root_execution_context.realm();
        let cache = decode(
            &serialize(source, ProgramType::Module),
            ProgramType::Module,
            &Rc::default(),
        )
        .expect("the blob decodes");
        let module = module_from_cache(&vm, realm, &cache, &directory.path_of("cached.mjs"), source)
            .expect("the module materializes");
        vm.run_module(module).must();

        let resolution = module.resolve_export(&vm, &ak::Utf16FlyString::from_utf8("default"));
        let source_module = resolution.module.expect("the export resolves");
        assert!(source_module != module.upcast());
        assert_eq!(Utf16View::of_fly_string(&resolution.export_name).to_utf8(), "pass");
    }

    fn failure_message(result: Result<Gc<Script>, Vec<ParserError>>) -> String {
        let Err(errors) = result else {
            panic!("the script materialized");
        };
        assert_eq!(errors.len(), 1);
        errors[0].message.clone()
    }

    #[test]
    fn blobs_that_do_not_match_the_source_are_rejected_and_released() {
        let vm = Vm::create();
        let root_execution_context = initialize_realm(&vm);
        let realm = root_execution_context.realm();
        let source = "var before = 0;\nfunction uniquelyNamedDeclaration() { return 1; } uniquelyNamedDeclaration();";
        let declaration_start = 16u32;
        let blob = serialize(source, ProgramType::Script);
        let releases = Rc::default();
        let decode_rejects = |bytes: &[u8], program_type| decode(bytes, program_type, &releases).is_none();

        assert!(decode_rejects(&blob, ProgramType::Module));
        assert!(decode_rejects(
            &serialize_for(source, ProgramType::Script, BytecodeCacheRuntime::Cpp),
            ProgramType::Script
        ));
        assert!(decode_rejects(&blob[..blob.len() - 1], ProgramType::Script));
        let mut with_trailing_byte = blob.clone();
        with_trailing_byte.push(0);
        assert!(decode_rejects(&with_trailing_byte, ProgramType::Script));
        let mut of_other_source = blob.clone();
        let hash_offset = 8 + 4 + 2;
        of_other_source[hash_offset] ^= 1;
        assert!(decode_rejects(&of_other_source, ProgramType::Script));
        assert_eq!(releases.get(), 5);

        let source_length = source.encode_utf16().count();
        let (top_level_bytecode, declaration_bytecode) =
            validated_blob_offsets(&blob, ProgramType::Script, source_length, |blob, base| {
                let DecodedDeclarationMetadata::Script {
                    declaration_functions, ..
                } = blob.declaration_metadata()
                else {
                    panic!("the blob is a script's");
                };
                let declaration = declaration_functions[0]
                    .cached_executable()
                    .decode_executable()
                    .expect("the declaration's executable decodes");
                (
                    offset_in_blob(base, blob.program().executable().bytecode().as_slice()),
                    offset_in_blob(base, declaration.bytecode().as_slice()),
                )
            });
        let name: Vec<u8> = "uniquelyNamedDeclaration"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .chain(declaration_start.to_le_bytes())
            .collect();
        let records_of_the_declaration: Vec<usize> = blob
            .windows(name.len())
            .enumerate()
            .filter_map(|(offset, window)| (window == name).then_some(offset))
            .collect();
        assert_eq!(
            records_of_the_declaration.len(),
            1,
            "only the declaration's record names it, followed by where its source text starts"
        );
        let source_text_start = records_of_the_declaration[0] + name.len() - size_of::<u32>();
        let mut out_of_range_source_text = blob.clone();
        out_of_range_source_text[source_text_start..source_text_start + size_of::<u32>()]
            .copy_from_slice(&u32::try_from(source_length + 1).expect("fits").to_le_bytes());

        let materialization_fails = |bytes: &[u8], source: &str| {
            let cache = decode(bytes, ProgramType::Script, &releases).expect("the blob decodes");
            failure_message(script_from_cache(&vm, realm, &cache, source))
        };
        for (bytes, source) in [
            (with_byte_flipped(&blob, top_level_bytecode), source.to_string()),
            (with_byte_flipped(&blob, declaration_bytecode), source.to_string()),
            (out_of_range_source_text, source.to_string()),
            (blob.clone(), format!("{source} ")),
        ] {
            assert_eq!(
                materialization_fails(&bytes, &source),
                "Failed to materialize bytecode cache"
            );
        }
        assert_eq!(releases.get(), 9);

        let cache = decode(&blob, ProgramType::Script, &releases).expect("the blob decodes");
        let script = script_from_cache(&vm, realm, &cache, source).expect("the original blob still materializes");
        assert_eq!(run(&vm, script), "1");
    }
}

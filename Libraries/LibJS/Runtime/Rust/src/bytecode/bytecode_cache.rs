/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! Bytecode cache blobs on this runtime's types, as RustIntegration.cpp, Script.cpp and SourceTextModule.cpp use them
//! in C++: Script and Source Text Module Records materialized from a decoded blob, and blobs installed into records
//! that already run.
//!
//! The executables made from a blob run their bytecode in place in it and keep it alive. A function's executable stays
//! in the blob until the function is first called. Installing a blob into a running record matches every function the
//! record has created to a function of the blob, then gives each one that ran an executable from the blob, which takes
//! over the inline caches of the one it replaces, and each one that did not its executable in the blob.

use core::cell::{Ref, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use crate::bytecode::executable::Executable;
use crate::gc::root::MarkedVec;
use crate::interpreter::vm::Vm;
use crate::layout::cell::Gc;
use crate::parser_error::ParserError;
use crate::runtime::shared_function_instance_data::SharedFunctionInstanceData;
use crate::source_code::SourceCode;
use libjs_rust::bytecode::generator::FunctionSfdMetadata;
use libjs_rust::bytecode_cache::{
    DecodedCacheBlob, DecodedCachedExecutableRecord, DecodedExecutableRecord, DecodedFunctionRecord,
};

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

    pub fn can_generate_bytecode_cache(self) -> bool {
        matches!(self, Self::Source | Self::HeapBytecode)
    }

    pub fn can_install_generated_bytecode_cache(self) -> bool {
        matches!(
            self,
            Self::GeneratingFreshCacheFromSource | Self::GeneratingFreshCacheFromHeapBytecode
        )
    }

    /// The backing once the generation of a bytecode cache began.
    pub fn with_bytecode_cache_generation_begun(self) -> Self {
        match self {
            Self::Source => Self::GeneratingFreshCacheFromSource,
            Self::HeapBytecode => Self::GeneratingFreshCacheFromHeapBytecode,
            _ => panic!("a bytecode cache is only generated for a record that has none, once at a time"),
        }
    }

    /// The backing once the generation of a bytecode cache ended without installing it.
    pub fn with_bytecode_cache_generation_finished_without_install(self) -> Self {
        match self {
            Self::GeneratingFreshCacheFromSource => Self::Source,
            Self::GeneratingFreshCacheFromHeapBytecode => Self::HeapBytecode,
            _ => panic!("only the generation of a bytecode cache that began can finish"),
        }
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
    Executable::create_from_bytecode_cache(vm, record, &functions, source_code, None)
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

enum Replacement {
    CachedExecutable(DecodedCachedExecutableRecord),
    Executable(Gc<Executable>),
}

/// What installing a blob gives one function, once every function turned out to have its counterpart in the blob.
struct PendingFunctionInstall {
    function: Gc<SharedFunctionInstanceData>,
    replacement: Replacement,
    metadata: FunctionSfdMetadata,
}

/// Installing a bytecode cache blob into a running record: the functions the record has created, each of which must
/// match one function of the blob, and what each one gets once they all did.
pub struct BytecodeCacheInstall<'vm, 'source> {
    vm: &'vm Vm,
    source_code: &'source Rc<SourceCode>,
    existing_functions: Vec<Gc<SharedFunctionInstanceData>>,
    /// The indices of the existing functions by the source text range a blob identifies them by, in their order.
    existing_functions_by_source_text_range: HashMap<(usize, usize), Vec<usize>, foldhash::fast::RandomState>,
    matched: Vec<bool>,
    pending_installs: Vec<PendingFunctionInstall>,
    /// The executables made for functions that ran, which nothing else holds until the install commits.
    new_executables: MarkedVec<'vm, Gc<Executable>>,
}

impl<'vm, 'source> BytecodeCacheInstall<'vm, 'source> {
    /// `existing_functions` must stay alive until the install commits or is dropped, as the record does for those it
    /// created.
    pub fn new(
        vm: &'vm Vm,
        source_code: &'source Rc<SourceCode>,
        existing_functions: &MarkedVec<'_, Gc<SharedFunctionInstanceData>>,
    ) -> Self {
        let existing_functions = existing_functions.to_vec();
        let mut existing_functions_by_source_text_range: HashMap<_, Vec<_>, _> = HashMap::default();
        for (index, function) in existing_functions.iter().enumerate() {
            existing_functions_by_source_text_range
                .entry(function.bytecode_cache_source_text_range())
                .or_default()
                .push(index);
        }
        Self {
            vm,
            source_code,
            matched: vec![false; existing_functions.len()],
            existing_functions,
            existing_functions_by_source_text_range,
            pending_installs: Vec::new(),
            new_executables: MarkedVec::new(vm),
        }
    }

    fn take_matching_function(
        &mut self,
        function: &DecodedFunctionRecord,
        outer_strict: bool,
    ) -> Option<Gc<SharedFunctionInstanceData>> {
        let source_text_range = function.source_text_range();
        let index = self
            .existing_functions_by_source_text_range
            .get(&(
                source_text_range.start,
                source_text_range.end.saturating_sub(source_text_range.start),
            ))?
            .iter()
            .copied()
            .find(|&index| {
                !self.matched[index]
                    && self.existing_functions[index].matches_bytecode_cache_function(function, outer_strict)
            })?;
        self.matched[index] = true;
        Some(self.existing_functions[index])
    }

    /// Matches a function of the blob, nested in code that is strict if `outer_strict` is, to one the record created,
    /// and prepares what that one gets: an executable from the blob if it ran, which recursively matches the functions
    /// it creates, and its executable in the blob otherwise. Returns `None` if no function matches or the blob turns
    /// out to be malformed.
    pub fn prepare_function(
        &mut self,
        function: &DecodedFunctionRecord,
        outer_strict: bool,
    ) -> Option<Gc<SharedFunctionInstanceData>> {
        let existing_function = self.take_matching_function(function, outer_strict)?;
        let replacement = match existing_function.executable() {
            None => Replacement::CachedExecutable(function.cached_executable()),
            Some(existing_executable) => {
                let record = function.cached_executable().decode_executable()?;
                Replacement::Executable(self.prepare_executable(&record, Some(existing_executable))?)
            }
        };
        self.pending_installs.push(PendingFunctionInstall {
            function: existing_function,
            replacement,
            metadata: function.scope_metadata().clone(),
        });
        Some(existing_function)
    }

    /// The executable of a record of the blob, whose functions match functions the record created, and which takes over
    /// the inline caches of `replaced_executable`.
    pub fn prepare_executable(
        &mut self,
        record: &DecodedExecutableRecord,
        replaced_executable: Option<Gc<Executable>>,
    ) -> Option<Gc<Executable>> {
        let functions = MarkedVec::new(self.vm);
        for function in record.functions()? {
            functions.push(self.prepare_function(&function, record.is_strict())?);
        }
        let executable =
            Executable::create_from_bytecode_cache(self.vm, record, &functions, self.source_code, replaced_executable)?;
        self.new_executables.push(executable);
        Some(executable)
    }

    /// Gives every function what was prepared for it, if every function the record created found its counterpart in
    /// the blob. Otherwise nothing changes and this returns false.
    pub fn commit(self) -> bool {
        if !self.matched.iter().all(|matched| *matched) {
            return false;
        }
        for install in self.pending_installs {
            match install.replacement {
                Replacement::CachedExecutable(cached_executable) => install
                    .function
                    .install_cached_bytecode_executable(cached_executable, &install.metadata),
                Replacement::Executable(executable) => install
                    .function
                    .install_bytecode_cache_executable(executable, &install.metadata),
            }
        }
        drop(self.new_executables);
        true
    }
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
    use libjs_rust::compile::{FunctionPrecompileMode, compile_function, compile_parsed_program_off_thread, parse};

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

        // Only the first call of lazy() decodes its constants, and only a BigInt constant's creation parses its digits.
        let lazy_source = "var lazy = function lazy() { return 1234.5678; }; 0";
        let mut with_malformed_lazy_constant = serialize(lazy_source, ProgramType::Script);
        let lazy_number = with_malformed_lazy_constant
            .windows(size_of::<f64>())
            .position(|window| window == 1234.5678f64.to_le_bytes())
            .expect("the blob has the constant of lazy()");
        with_malformed_lazy_constant[lazy_number - 1] = 0xee;
        let big_int_source = "var big = 123456789n; big";
        let mut with_big_int_that_is_not_a_number = serialize(big_int_source, ProgramType::Script);
        let digits = with_big_int_that_is_not_a_number
            .windows(9)
            .position(|window| window == b"123456789")
            .expect("the blob has the digits of the BigInt");
        with_big_int_that_is_not_a_number[digits + 4] = b'z';

        let materialization_fails = |bytes: &[u8], source: &str| {
            let cache = decode(bytes, ProgramType::Script, &releases).expect("the blob decodes");
            failure_message(script_from_cache(&vm, realm, &cache, source))
        };
        for (bytes, source) in [
            (with_byte_flipped(&blob, top_level_bytecode), source.to_string()),
            (with_byte_flipped(&blob, declaration_bytecode), source.to_string()),
            (out_of_range_source_text, source.to_string()),
            (blob.clone(), format!("{source} ")),
            (with_malformed_lazy_constant, lazy_source.to_string()),
            (with_big_int_that_is_not_a_number, big_int_source.to_string()),
        ] {
            assert_eq!(
                materialization_fails(&bytes, &source),
                "Failed to materialize bytecode cache"
            );
        }
        assert_eq!(releases.get(), 11);

        let cache = decode(&blob, ProgramType::Script, &releases).expect("the blob decodes");
        let script = script_from_cache(&vm, realm, &cache, source).expect("the original blob still materializes");
        assert_eq!(run(&vm, script), "1");
    }

    #[test]
    fn installing_a_cache_gives_functions_that_ran_executables_from_it_with_their_inline_caches() {
        let vm = Vm::create();
        vm.heap().set_should_collect_on_every_allocation(true);
        let root_execution_context = initialize_realm(&vm);
        let realm = root_execution_context.realm();
        let source = "var tag = function tag(strings) { return strings; };\n\
                      var template = function template(use_template) { if (use_template) return tag`hello`; \
                      return null; };\n\
                      var outer = function outer() { function inner() { return 1; } return inner; };\n\
                      var Klass = class { constructor() { this.value = 1; } method() { return this.value; } };\n\
                      template(false); var innerFunction = outer(); new Klass().method();";
        let source_code = source_code_of("test.js", source);
        let script = parsed_script(&vm, realm, &source_code);
        run(&vm, script);

        let template = function_of_script(&vm, script, "template");
        let old_template_executable = template.executable().expect("template() ran");
        let old_template_cache = old_template_executable.template_object_cache(0);
        assert!(old_template_cache.cached_template_object().is_none());
        let inner = function_of_script(&vm, script, "inner");
        assert!(inner.has_function_ast() && inner.executable().is_none());
        let old_script_executable = script.cached_executable();

        let releases = Rc::default();
        let cache =
            decode(&serialize(source, ProgramType::Script), ProgramType::Script, &releases).expect("the blob decodes");
        assert!(script.try_install_bytecode_cache(&vm, &cache, &source_code));
        assert!(script.executable_backing().is_mapped_bytecode_cache());
        assert!(script.cached_executable() != old_script_executable);
        assert!(script.cached_executable().runs_in_place_in_bytecode_cache_blob());

        let new_template_executable = template.executable().expect("template() keeps an executable");
        assert!(new_template_executable != old_template_executable);
        assert!(new_template_executable.runs_in_place_in_bytecode_cache_blob());
        assert!(new_template_executable.template_object_cache(0) == old_template_cache);
        assert!(!inner.has_function_ast() && inner.has_cached_bytecode());
        let declared: Vec<_> = script
            .functions_to_initialize()
            .iter()
            .map(|function| function.shared_data)
            .collect();
        let functions = functions_created_by(&vm, declared, Some(script.cached_executable()));
        assert!(have_only_bytecode_cache_compile_inputs(&functions));

        assert!(
            !script.try_install_bytecode_cache(&vm, &cache, &source_code),
            "a cache installs once"
        );
        drop(cache);
        assert_eq!(
            utf8(
                run_script(
                    &vm,
                    realm,
                    "[template(true) === template(true), innerFunction(), new Klass().method()].join()"
                )
                .must()
            ),
            "true,1,1"
        );
        assert!(old_template_cache.cached_template_object().is_some());
        assert_eq!(releases.get(), 0);
    }

    const TEMPORARY_SHAPE_COUNT: usize = 64;

    /// Whether the first entry of each of the first property lookup caches of `executable` holds a shape.
    #[inline(never)]
    fn cached_shapes(executable: Gc<Executable>) -> Vec<bool> {
        (0..TEMPORARY_SHAPE_COUNT)
            .map(|index| {
                executable
                    .property_lookup_cache(index)
                    .first_entry()
                    .is_some_and(|entry| entry.shape.is_some())
            })
            .collect()
    }

    /// Runs the script in a frame of its own, so that no pointer to the objects it makes stays on the stack of the test,
    /// where the conservative scan would keep them alive.
    #[inline(never)]
    fn executable_of_function_that_read_temporary_objects(vm: &Vm, script: Gc<Script>) -> Gc<Executable> {
        run(vm, script);
        let executable = function_of_script(vm, script, "read").executable().expect("read() ran");
        assert!(cached_shapes(executable).iter().all(|cached| *cached));
        executable
    }

    #[test]
    fn an_executable_that_replaces_another_takes_over_no_cache_entry_for_a_cell_that_died_while_it_was_made() {
        let vm = Vm::create();
        let root_execution_context = initialize_realm(&vm);
        let realm = root_execution_context.realm();
        let parameters: Vec<String> = (0..TEMPORARY_SHAPE_COUNT).map(|index| format!("o{index}")).collect();
        let reads: Vec<String> = (0..TEMPORARY_SHAPE_COUNT).map(|index| format!("o{index}.p")).collect();
        let objects: Vec<String> = (0..TEMPORARY_SHAPE_COUNT)
            .map(|index| format!("{{ p: {index}, q{index}: 0 }}"))
            .collect();
        let source = format!(
            "var read = function read({}) {{ return [{}]; }}; read({}); 0",
            parameters.join(", "),
            reads.join(", "),
            objects.join(", ")
        );
        let source_code = source_code_of("test.js", &source);
        let script = parsed_script(&vm, realm, &source_code);
        let old_executable = executable_of_function_that_read_temporary_objects(&vm, script);

        let cache = decode(
            &serialize(&source, ProgramType::Script),
            ProgramType::Script,
            &Rc::default(),
        )
        .expect("the blob decodes");
        let blob = cache
            .validated_blob(source_code.length_in_code_units())
            .expect("the blob is valid");
        let functions = blob.program().executable().functions().expect("the functions decode");
        let [read] = functions.as_slice() else {
            panic!("the script creates one function");
        };
        let record = read.cached_executable().decode_executable().expect("read() decodes");

        // Nothing holds the objects read() saw any more, so the collection that allocating the new executable starts
        // frees their shapes.
        vm.heap().set_should_collect_on_every_allocation(true);
        let new_executable = Executable::create_from_bytecode_cache(
            &vm,
            &record,
            &MarkedVec::new(&vm),
            &source_code,
            Some(old_executable),
        )
        .expect("read() materializes");
        vm.heap().set_should_collect_on_every_allocation(false);
        let shapes_kept_by_old_executable = cached_shapes(old_executable);
        let shapes_kept_by_new_executable = cached_shapes(new_executable);
        let died = shapes_kept_by_old_executable.iter().filter(|kept| !**kept).count();
        assert!(
            died >= TEMPORARY_SHAPE_COUNT / 2,
            "only {died} of the {TEMPORARY_SHAPE_COUNT} temporary shapes died"
        );
        assert_eq!(shapes_kept_by_new_executable, shapes_kept_by_old_executable);
    }

    #[test]
    fn a_failed_install_changes_nothing() {
        let vm = Vm::create();
        let root_execution_context = initialize_realm(&vm);
        let realm = root_execution_context.realm();
        let source = "var f = function f() { return 1; }; f();";
        let source_code = source_code_of("test.js", source);
        let script = parsed_script(&vm, realm, &source_code);
        assert_eq!(run(&vm, script), "1");
        let f = function_of_script(&vm, script, "f");
        let old_executable = script.cached_executable();
        let old_function_executable = f.executable().expect("f() ran");

        let blob = serialize(source, ProgramType::Script);
        let top_level_bytecode = validated_blob_offsets(
            &blob,
            ProgramType::Script,
            source_code.length_in_code_units(),
            |blob, base| offset_in_blob(base, blob.program().executable().bytecode().as_slice()),
        );
        let corrupt = decode(
            &with_byte_flipped(&blob, top_level_bytecode),
            ProgramType::Script,
            &Rc::default(),
        )
        .expect("the blob decodes");
        assert!(!script.try_install_bytecode_cache(&vm, &corrupt, &source_code));
        let of_longer_source = decode(
            &serialize(&format!("{source} "), ProgramType::Script),
            ProgramType::Script,
            &Rc::default(),
        )
        .expect("the blob decodes");
        assert!(!script.try_install_bytecode_cache(&vm, &of_longer_source, &source_code));
        // A source of the same length whose function lies elsewhere: no function of the blob matches f().
        let other_source = "var f =  function() { return 1; }; f(); ";
        assert_eq!(other_source.len(), source.len());
        let of_other_function = decode(
            &serialize(other_source, ProgramType::Script),
            ProgramType::Script,
            &Rc::default(),
        )
        .expect("the blob decodes");
        assert!(!script.try_install_bytecode_cache(&vm, &of_other_function, &source_code));

        assert!(script.executable_backing().is_source());
        assert!(script.cached_executable() == old_executable);
        assert!(f.executable() == Some(old_function_executable));
        assert_eq!(run(&vm, script), "1");
    }

    #[test]
    fn generating_a_cache_moves_a_record_through_the_states_of_its_executable_backing() {
        let vm = Vm::create();
        let root_execution_context = initialize_realm(&vm);
        let realm = root_execution_context.realm();
        let source = "let f = function lazy() { return 1; }; f;";
        let source_code = source_code_of("test.js", source);

        let parsed = parsed_script(&vm, realm, &source_code);
        let code_units = source_code.code().to_utf16();
        let compiled = Script::create(
            &vm,
            realm,
            compile_parsed_program_off_thread(
                parse(&code_units, ProgramType::Script, 1),
                code_units.len(),
                FunctionPrecompileMode::EagerOnly,
            )
            .into_script(),
            source_code.clone(),
            "test.js",
            ForeignCellSlot::empty(),
            ExecutableBacking::HeapBytecode,
        );
        for (script, is_source) in [(parsed, true), (compiled, false)] {
            let backing_is_where_it_was_compiled = || {
                let backing = script.executable_backing();
                if is_source {
                    backing.is_source()
                } else {
                    backing.is_heap_bytecode()
                }
            };
            assert!(script.can_generate_bytecode_cache() && !script.can_install_generated_bytecode_cache());
            script.begin_bytecode_cache_generation(&vm);
            assert!(backing_is_where_it_was_compiled());
            assert!(!script.can_generate_bytecode_cache() && script.can_install_generated_bytecode_cache());
            script.finish_bytecode_cache_generation_without_install(&vm);
            assert!(backing_is_where_it_was_compiled());
            assert!(script.can_generate_bytecode_cache() && !script.can_install_generated_bytecode_cache());

            script.begin_bytecode_cache_generation(&vm);
            let lazy = function_of_script(&vm, script, "lazy");
            if !is_source {
                let (_, payload) = SharedFunctionInstanceData::uncompiled_functions_of(script.cached_executable())
                    .into_iter()
                    .find(|(function, _)| *function == lazy)
                    .expect("lazy() has its AST");
                lazy.set_precompiled_bytecode_executable(compile_function(
                    payload,
                    code_units.len(),
                    false,
                    FunctionPrecompileMode::All,
                ));
                assert!(lazy.has_precompiled_bytecode());
            }
            let cache = decode(
                &serialize(source, ProgramType::Script),
                ProgramType::Script,
                &Rc::default(),
            )
            .expect("the blob decodes");
            script.install_generated_bytecode_cache(&vm, &cache, &source_code);
            assert!(script.executable_backing().is_mapped_bytecode_cache());
            assert!(!script.can_generate_bytecode_cache() && !script.can_install_generated_bytecode_cache());
            assert!(!lazy.has_function_ast() && !lazy.has_precompiled_bytecode() && lazy.has_cached_bytecode());
        }
    }

    #[test]
    fn installing_a_cache_into_a_module_with_top_level_await_replaces_the_body_of_its_async_function() {
        let vm = Vm::create();
        let root_execution_context = initialize_realm(&vm);
        let realm = root_execution_context.realm();
        let source = "export const value = await Promise.resolve(function later() { return 'later'; });";
        let source_code = source_code_of("test.mjs", source);
        let module = SourceTextModule::parse(&vm, source_code.clone(), realm, "test.mjs").expect("the module parses");
        let top_level_await_shared_data = module.top_level_await_shared_data().expect("the module awaits");
        let old_body = top_level_await_shared_data.executable().expect("the body is compiled");

        let cache = decode(
            &serialize(source, ProgramType::Module),
            ProgramType::Module,
            &Rc::default(),
        )
        .expect("the blob decodes");
        module.begin_bytecode_cache_generation(&vm);
        assert!(module.can_install_generated_bytecode_cache());
        module.install_generated_bytecode_cache(&vm, &cache, &source_code);
        assert!(module.executable_backing().is_mapped_bytecode_cache());
        assert!(module.top_level_await_shared_data() == Some(top_level_await_shared_data));
        let new_body = top_level_await_shared_data.executable().expect("the body is replaced");
        assert!(new_body != old_body && new_body.runs_in_place_in_bytecode_cache_blob());
        assert!(!module.try_install_bytecode_cache(&vm, &cache, &source_code));

        let plain_source = "export function plain() { return 'plain'; }";
        let plain_source_code = source_code_of("plain.mjs", plain_source);
        let plain =
            SourceTextModule::parse(&vm, plain_source_code.clone(), realm, "plain.mjs").expect("the module parses");
        let awaiting_cache = decode(
            &serialize(source, ProgramType::Module),
            ProgramType::Module,
            &Rc::default(),
        )
        .expect("the blob decodes");
        assert!(!plain.try_install_bytecode_cache(&vm, &awaiting_cache, &source_code));
        let plain_cache = decode(
            &serialize(plain_source, ProgramType::Module),
            ProgramType::Module,
            &Rc::default(),
        )
        .expect("the blob decodes");
        let old_plain_body = plain.cached_executable();
        assert!(plain.try_install_bytecode_cache(&vm, &plain_cache, &plain_source_code));
        assert!(plain.cached_executable() != old_plain_body);
        assert!(
            plain
                .cached_executable()
                .is_some_and(|body| body.runs_in_place_in_bytecode_cache_blob())
        );
    }
}

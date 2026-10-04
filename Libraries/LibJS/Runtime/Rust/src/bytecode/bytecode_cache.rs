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

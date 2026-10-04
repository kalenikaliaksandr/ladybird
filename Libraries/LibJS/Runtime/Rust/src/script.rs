/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

use core::ffi::c_void;
use core::ptr::NonNull;
use std::collections::HashSet;
use std::rc::Rc;

use ak::Utf16FlyString;
use libjs_runtime_macros::Trace;

use crate::bytecode::executable::Executable;
use crate::gc::class::{GcCell, define_cell};
use crate::gc::foreign::ForeignCellSlot;
use crate::gc::gc_ref_cell::GcRefCell;
use crate::gc::root::MarkedVec;
use crate::hash_table::Utf16FlyStringHashTable;
use crate::interpreter::vm::Vm;
use crate::layout::cell::{CellHeader, Gc};
use crate::layout::realm::Realm;
use crate::layout::value::Value;
use crate::parser_error::ParserError;
use crate::runtime::completion::ThrowCompletionOr;
use crate::runtime::ecmascript_function_object::EcmascriptFunctionObject;
use crate::runtime::error::ErrorKind;
use crate::runtime::error_types::ErrorType;
use crate::runtime::global_environment::GlobalEnvironment;
use crate::runtime::module_request::LoadedModuleRequest;
use crate::runtime::private_environment::PrivateEnvironment;
use crate::runtime::shared_function_instance_data::SharedFunctionInstanceData;
use crate::source_code::SourceCode;
use libjs_rust::ast::{ProgramType, Utf16String};
use libjs_rust::compile::{CompiledScript, ParsedProgram, compile_script, parse};

/// Script::FunctionToInitialize.
#[derive(Trace)]
pub struct FunctionToInitialize {
    pub shared_data: Gc<SharedFunctionInstanceData>,
    pub name: Utf16FlyString,
}

/// Script::LexicalBinding.
#[derive(Clone, Trace)]
pub struct LexicalBinding {
    pub name: Utf16FlyString,
    pub is_constant: bool,
}

// 16.1.4 Script Records, https://tc39.es/ecma262/#sec-script-records
#[repr(C)]
#[derive(Trace)]
pub struct Script {
    header: CellHeader,
    realm: Gc<Realm>,                                    // [[Realm]]
    loaded_modules: GcRefCell<Vec<LoadedModuleRequest>>, // [[LoadedModules]]
    host_defined: ForeignCellSlot,                       // [[HostDefined]]
    executable: Gc<Executable>,
    /// What the script's functions compile themselves from when they are first called.
    #[gc(untraced)]
    source_code: Rc<SourceCode>,

    // Pre-computed global declaration instantiation data.
    // These are extracted from the AST at parse time so that GDI can run
    // without needing to walk the AST.
    lexical_names: Vec<Utf16FlyString>,
    var_names: Vec<Utf16FlyString>,
    functions_to_initialize: Vec<FunctionToInitialize>,
    #[gc(untraced)]
    declared_function_names: HashSet<Utf16FlyString>,
    var_scoped_names: Vec<Utf16FlyString>,
    annex_b_candidate_names: Vec<Utf16FlyString>,
    lexical_bindings: Vec<LexicalBinding>,
    is_strict_mode: bool,

    // Needed for potential lookups of modules.
    filename: String,
}

define_cell!(Script, Other);

fn fly_string_of(name: &Utf16String) -> Utf16FlyString {
    Utf16FlyString::from_utf16(&name.0)
}

fn fly_strings_of(names: &[Utf16String]) -> Vec<Utf16FlyString> {
    names.iter().map(fly_string_of).collect()
}

impl Script {
    // 16.1.5 ParseScript ( sourceText, realm, hostDefined ), https://tc39.es/ecma262/#sec-parse-script
    pub fn parse(vm: &Vm, source: &[u16], realm: Gc<Realm>) -> Result<Gc<Script>, Vec<ParserError>> {
        Self::parse_with_filename(vm, source, realm, "")
    }

    /// ParseScript of a script from `filename`, which the script's code reports and module loading resolves the
    /// specifiers of its dynamic imports against.
    pub fn parse_with_filename(
        vm: &Vm,
        source: &[u16],
        realm: Gc<Realm>,
        filename: &str,
    ) -> Result<Gc<Script>, Vec<ParserError>> {
        Self::parse_with_host_defined(
            vm,
            source,
            realm,
            filename,
            ak::Utf16String::default(),
            ForeignCellSlot::empty(),
            1,
        )
    }

    /// ParseScript as C++ Script::parse runs it for a host: the script's code reports `display_filename`, or
    /// `filename` if that is empty, its lines count from `line_number_offset`, and it keeps `host_defined` as its
    /// [[HostDefined]].
    pub fn parse_with_host_defined(
        vm: &Vm,
        source: &[u16],
        realm: Gc<Realm>,
        filename: &str,
        display_filename: ak::Utf16String,
        host_defined: ForeignCellSlot,
        line_number_offset: usize,
    ) -> Result<Gc<Script>, Vec<ParserError>> {
        let parsed = parse(source, ProgramType::Script, line_number_offset);
        if parsed.has_errors() {
            return Err(ParserError::all_from_parsed_program(&parsed));
        }
        let display_filename = if display_filename.is_empty() {
            ak::Utf16String::from_utf8(filename)
        } else {
            display_filename
        };
        let source_code = SourceCode::create(display_filename, ak::Utf16String::from_utf16(source));
        let source_length = source_code.length_in_code_units();
        Ok(Self::create(
            vm,
            realm,
            compile_script(parsed, source_length),
            source_code,
            filename,
            host_defined,
        ))
    }

    /// Compiles a script the caller parsed without errors from `source`.
    pub fn compile_parsed_program(vm: &Vm, parsed: ParsedProgram, source: &[u16], realm: Gc<Realm>) -> Gc<Script> {
        Self::compile_parsed_program_with_filename(vm, parsed, source, realm, ak::Utf16String::default())
    }

    /// Compiles a script the caller parsed without errors from `source`, which came from `filename`.
    pub fn compile_parsed_program_with_filename(
        vm: &Vm,
        parsed: ParsedProgram,
        source: &[u16],
        realm: Gc<Realm>,
        filename: ak::Utf16String,
    ) -> Gc<Script> {
        let source_code = SourceCode::create(filename, ak::Utf16String::from_utf16(source));
        Self::create_from_parsed(vm, parsed, source_code, realm)
    }

    /// Compiles a script the caller parsed without errors from the code of `source_code`, whose filename the
    /// script's code reports.
    pub fn create_from_parsed(
        vm: &Vm,
        parsed: ParsedProgram,
        source_code: Rc<SourceCode>,
        realm: Gc<Realm>,
    ) -> Gc<Script> {
        Self::create_from_parsed_with_filename(vm, parsed, source_code, realm, "")
    }

    /// Compiles a script the caller parsed without errors from the code of `source_code`, whose filename the
    /// script's code reports, as `filename`, which module loading resolves the specifiers of its dynamic imports
    /// against.
    pub fn create_from_parsed_with_filename(
        vm: &Vm,
        parsed: ParsedProgram,
        source_code: Rc<SourceCode>,
        realm: Gc<Realm>,
        filename: &str,
    ) -> Gc<Script> {
        assert!(parsed.program_type() == ProgramType::Script && !parsed.has_errors());
        let source_length = source_code.length_in_code_units();
        Self::create(
            vm,
            realm,
            compile_script(parsed, source_length),
            source_code,
            filename,
            ForeignCellSlot::empty(),
        )
    }

    /// The Script Record of a script compiled, on any thread, from the code of `source_code`, whose filename the
    /// script's code reports. Module loading resolves the specifiers of its dynamic imports against `filename`.
    pub fn create(
        vm: &Vm,
        realm: Gc<Realm>,
        compiled: CompiledScript,
        source_code: Rc<SourceCode>,
        filename: &str,
        host_defined: ForeignCellSlot,
    ) -> Gc<Script> {
        let CompiledScript {
            executable,
            declarations,
        } = compiled;
        let is_strict = executable.is_strict;
        let executable = Executable::create_with_source_code(vm, executable, Some(&source_code));

        // The functions stay rooted until the script that holds them is allocated.
        let rooted_shared_data = MarkedVec::with_capacity(vm, declarations.functions_to_initialize.len());
        let mut function_names = Vec::with_capacity(declarations.functions_to_initialize.len());
        for mut function in declarations.functions_to_initialize {
            rooted_shared_data.push(SharedFunctionInstanceData::create_from_pending_shared_function_data(
                vm,
                &mut function.shared_function_data,
                is_strict,
                Some(&source_code),
            ));
            function_names.push(fly_string_of(&function.name));
        }
        let functions_to_initialize: Vec<FunctionToInitialize> = rooted_shared_data
            .to_vec()
            .into_iter()
            .zip(function_names)
            .map(|(shared_data, name)| FunctionToInitialize { shared_data, name })
            .collect();
        let declared_function_names = functions_to_initialize
            .iter()
            .map(|function| function.name.clone())
            .collect();
        let lexical_bindings = declarations
            .lexical_bindings
            .iter()
            .map(|binding| LexicalBinding {
                name: fly_string_of(&binding.name),
                is_constant: binding.is_constant,
            })
            .collect();

        let script = vm.heap().allocate(Script {
            header: CellHeader::for_class(Self::CLASS),
            realm,
            loaded_modules: GcRefCell::new(Vec::new()),
            host_defined,
            executable,
            source_code,
            lexical_names: fly_strings_of(&declarations.lexical_names),
            var_names: fly_strings_of(&declarations.var_names),
            functions_to_initialize,
            declared_function_names,
            var_scoped_names: fly_strings_of(&declarations.var_scoped_names),
            annex_b_candidate_names: fly_strings_of(&declarations.annex_b_candidate_names),
            lexical_bindings,
            // NB: The C++ runtime never sets the strictness of a script it compiles, so Annex B function hoisting
            //     always runs; the frontend only collects candidates for sloppy scripts.
            is_strict_mode: false,
            filename: filename.to_string(),
        });
        drop(rooted_shared_data);
        script
    }

    pub fn realm(&self) -> Gc<Realm> {
        self.realm
    }

    pub fn loaded_modules(&self) -> &GcRefCell<Vec<LoadedModuleRequest>> {
        &self.loaded_modules
    }

    pub fn host_defined(&self) -> Option<NonNull<c_void>> {
        self.host_defined.get()
    }

    pub fn filename(&self) -> &str {
        &self.filename
    }

    pub fn cached_executable(&self) -> Gc<Executable> {
        self.executable
    }

    pub fn functions_to_initialize(&self) -> &[FunctionToInitialize] {
        &self.functions_to_initialize
    }

    // 16.1.7 GlobalDeclarationInstantiation ( script, env ), https://tc39.es/ecma262/#sec-globaldeclarationinstantiation
    pub fn global_declaration_instantiation(
        &self,
        vm: &Vm,
        global_environment: Gc<GlobalEnvironment>,
    ) -> ThrowCompletionOr<()> {
        let realm = vm.current_realm();

        // 1. Let lexNames be the LexicallyDeclaredNames of script.
        // 2. Let varNames be the VarDeclaredNames of script.
        // 3. For each element name of lexNames, do
        for name in self.lexical_names.clone() {
            // a. If env.HasLexicalDeclaration(name) is true, throw a SyntaxError exception.
            if global_environment.has_lexical_declaration(&name) {
                return vm.throw_completion(
                    ErrorKind::SyntaxError,
                    ErrorType::TopLevelVariableAlreadyDeclared,
                    &[&name],
                );
            }

            // b. Let hasRestrictedGlobal be ? HasRestrictedGlobalProperty(env, name).
            let has_restricted_global = global_environment.has_restricted_global_property(vm, &name)?;

            // d. If hasRestrictedGlobal is true, throw a SyntaxError exception.
            if has_restricted_global {
                return vm.throw_completion(ErrorKind::SyntaxError, ErrorType::RestrictedGlobalProperty, &[&name]);
            }
        }

        // 4. For each element name of varNames, do
        for name in self.var_names.clone() {
            // a. If env.HasLexicalDeclaration(name) is true, throw a SyntaxError exception.
            if global_environment.has_lexical_declaration(&name) {
                return vm.throw_completion(
                    ErrorKind::SyntaxError,
                    ErrorType::TopLevelVariableAlreadyDeclared,
                    &[&name],
                );
            }
        }

        let function_names: Vec<Utf16FlyString> = self
            .functions_to_initialize
            .iter()
            .map(|function| function.name.clone())
            .collect();

        // 5. Let varDeclarations be the VarScopedDeclarations of script.
        // 6. Let functionsToInitialize be a new empty List.
        // 7. Let declaredFunctionNames be a new empty List.
        // 8. For each element d of varDeclarations, in reverse List order, do
        for function_name in &function_names {
            // 1. Let fnDefinable be ? env.CanDeclareGlobalFunction(fn).
            let function_definable = global_environment.can_declare_global_function(vm, function_name)?;

            // 2. If fnDefinable is false, throw a TypeError exception.
            if !function_definable {
                return vm.throw_completion(
                    ErrorKind::TypeError,
                    ErrorType::CannotDeclareGlobalFunction,
                    &[function_name],
                );
            }
        }

        // 9. Let declaredVarNames be a new empty List.
        let mut declared_var_names = Utf16FlyStringHashTable::default();

        // 10. For each element d of varDeclarations, do
        for name in self.var_scoped_names.clone() {
            // 1. If vn is not an element of declaredFunctionNames, then
            if self.declared_function_names.contains(&name) {
                continue;
            }

            // a. Let vnDefinable be ? env.CanDeclareGlobalVar(vn).
            let var_definable = global_environment.can_declare_global_var(vm, &name)?;

            // b. If vnDefinable is false, throw a TypeError exception.
            if !var_definable {
                return vm.throw_completion(ErrorKind::TypeError, ErrorType::CannotDeclareGlobalVariable, &[&name]);
            }

            // c. If vn is not an element of declaredVarNames, then
            // i. Append vn to declaredVarNames.
            declared_var_names.set(name);
        }

        // 12. NOTE: Annex B.3.2.2 adds additional steps at this point.
        // 12. Let strict be IsStrict of script.
        // 13. If strict is false, then
        if !self.is_strict_mode {
            // a. Let declaredFunctionOrVarNames be the list-concatenation of declaredFunctionNames and declaredVarNames.
            // b. For each FunctionDeclaration f that is directly contained in the StatementList of a Block, CaseClause, or DefaultClause Contained within script, do
            for function_name in self.annex_b_candidate_names.clone() {
                // i. Let F be StringValue of the BindingIdentifier of f.

                // 1. If env.HasLexicalDeclaration(F) is false, then
                if global_environment.has_lexical_declaration(&function_name) {
                    continue;
                }

                // a. Let fnDefinable be ? env.CanDeclareGlobalVar(F).
                let function_definable = global_environment.can_declare_global_function(vm, &function_name)?;
                // b. If fnDefinable is true, then
                if !function_definable {
                    continue;
                }

                // ii. If declaredFunctionOrVarNames does not contain F, then
                if !self.declared_function_names.contains(&function_name)
                    && !declared_var_names.contains(&function_name)
                {
                    // i. Perform ? env.CreateGlobalVarBinding(F, false).
                    global_environment.create_global_var_binding(vm, &function_name, false)?;
                }
            }
        }

        // 14. Let privateEnv be null.
        let private_environment: Option<Gc<PrivateEnvironment>> = None;

        // 15. For each element d of lexDeclarations, do
        for binding in self.lexical_bindings.clone() {
            // i. If IsConstantDeclaration of d is true, then
            if binding.is_constant {
                // 1. Perform ? env.CreateImmutableBinding(dn, true).
                global_environment.create_immutable_binding(vm, &binding.name, true)?;
            }
            // ii. Else,
            else {
                // 1. Perform ? env.CreateMutableBinding(dn, false).
                global_environment.create_mutable_binding(vm, &binding.name, false)?;
            }
        }

        // 16. For each Parse Node f of functionsToInitialize, do
        for (function_index, function_name) in function_names.iter().enumerate() {
            // a. Let fn be the sole element of the BoundNames of f.
            // b. Let fo be InstantiateFunctionObject of f with arguments env and privateEnv.
            let function = EcmascriptFunctionObject::create_from_function_data(
                vm,
                realm.expect("GlobalDeclarationInstantiation runs in an execution context with a realm"),
                self.functions_to_initialize[function_index].shared_data,
                Some(global_environment.upcast()),
                private_environment,
            );

            // c. Perform ? env.CreateGlobalFunctionBinding(fn, fo, false).
            // NB: C++ binds function->name(), which is the name the declaration binds.
            global_environment.create_global_function_binding(
                vm,
                function_name,
                Value::from_object(function),
                false,
            )?;
        }

        // 17. For each String vn of declaredVarNames, do
        for var_name in declared_var_names.iter() {
            // a. Perform ? env.CreateGlobalVarBinding(vn, false).
            global_environment.create_global_var_binding(vm, var_name, false)?;
        }

        // 18. Return unused.
        Ok(())
    }
}

#[cfg(all(test, libjs_runtime_tests_with_libgc))]
mod tests {
    use super::*;
    use crate::layout::object::Object;
    use crate::runtime::completion::Must;
    use crate::runtime::global_environment::test_global_object::set_up_global_object;
    use crate::runtime::print::{PrintContext, print};
    use crate::runtime::property_attributes::PropertyAttributes;
    use crate::runtime::realm::test_realm::{
        TestRealm, check_that_host_defined_slots_keep_their_cells_alive, key, own_keys,
    };

    /// Runs `source` as a script of `realm` and describes its completion the way the C++ js REPL prints it, with a
    /// thrown error as its name and message.
    fn evaluate(vm: &Vm, realm: Gc<Realm>, source: &str) -> String {
        let source: Vec<u16> = source.encode_utf16().collect();
        let script = Script::parse(vm, &source, realm).expect("the script parses");
        let value = match vm.run_script(script, None) {
            Ok(value) => value,
            Err(throw) => return describe_thrown_error(vm, throw.value()),
        };
        let mut text = Vec::new();
        let mut context = PrintContext {
            vm,
            stream: &mut text,
            strip_ansi: true,
            raw_strings: false,
        };
        print(value, &mut context).expect("printing into a buffer succeeds");
        String::from_utf8(text).expect("the printed value is UTF-8")
    }

    /// "Name: message" of a thrown error.
    fn describe_thrown_error(vm: &Vm, thrown: Value) -> String {
        assert!(
            thrown.is_object() && thrown.as_object().has_error_data(),
            "the script threw a value that is not an error"
        );
        let error = thrown.as_object();
        let property = |name| {
            error
                .get_without_side_effects(vm, name)
                .to_utf16_string_without_side_effects()
        };
        let (name, message) = (property(&vm.names.name), property(&vm.names.message));
        format!(
            "{}: {}",
            crate::utf16::Utf16View::of_string(&name).to_utf8(),
            crate::utf16::Utf16View::of_string(&message).to_utf8()
        )
    }

    fn evaluate_session(vm: &Vm, realm: Gc<Realm>, session: &[(&str, &str)]) {
        for (source, expected) in session {
            assert_eq!(evaluate(vm, realm, source), *expected, "evaluating {source:?}");
        }
    }

    fn global_with_test_realm(vm: &Vm) -> (TestRealm<'_>, Gc<Object>) {
        let test_realm = TestRealm::new(vm);
        let global = set_up_global_object(vm, test_realm.realm);
        (test_realm, global)
    }

    /// Each line is a script the C++ js REPL (Build/release/bin/js -i, fed one script per line) evaluated in one
    /// realm, with what it printed.
    #[test]
    fn global_declarations_conflict_across_scripts_like_the_cpp_runtime() {
        let vm = Vm::create();
        vm.heap().set_should_collect_on_every_allocation(true);
        let (test_realm, global) = global_with_test_realm(&vm);
        evaluate_session(
            &vm,
            test_realm.realm,
            &[
                ("let x = 1", "undefined"),
                ("var x = 2", "SyntaxError: Redeclaration of top level variable 'x'"),
                (
                    "let a = 1; var x",
                    "SyntaxError: Redeclaration of top level variable 'x'",
                ),
                ("typeof a", "\"undefined\""),
                ("var v1; let x", "SyntaxError: Redeclaration of top level variable 'x'"),
                ("typeof v1", "\"undefined\""),
                ("var y = 1", "undefined"),
                ("let y = 2", "SyntaxError: Cannot declare global property 'y'"),
                ("const cst = 1", "undefined"),
                ("let cst = 2", "SyntaxError: Redeclaration of top level variable 'cst'"),
                ("const k = 1; var k2; let k3", "undefined"),
                ("k3", "undefined"),
                ("let x2 = 1; x2", "1"),
                ("x2 = 5; x2", "5"),
                ("x + y + x2", "7"),
                ("var newVar = 5", "undefined"),
            ],
        );

        // A var declaration is a non-configurable property of the global object; a lexical one is not a property.
        let k2 = global.internal_get_own_property(&vm, &key("k2")).must().unwrap();
        assert_eq!(k2.value, Some(Value::UNDEFINED));
        assert_eq!(
            (k2.writable, k2.enumerable, k2.configurable),
            (Some(true), Some(true), Some(false))
        );
        assert!(global.internal_get_own_property(&vm, &key("x2")).must().is_none());
        assert!(
            test_realm
                .realm
                .global_environment()
                .has_lexical_declaration(&Utf16FlyString::from_utf8("x2"))
        );

        global.define_direct_property(&vm, &key("nc"), Value::from_i32(1), PropertyAttributes::default());
        evaluate_session(
            &vm,
            test_realm.realm,
            &[
                ("let nc = 1", "SyntaxError: Cannot declare global property 'nc'"),
                (
                    "function nc() {}",
                    "TypeError: Cannot declare global function of name 'nc'",
                ),
            ],
        );

        assert!(global.internal_prevent_extensions(&vm).must());
        evaluate_session(
            &vm,
            test_realm.realm,
            &[
                (
                    "var brandNew",
                    "TypeError: Cannot declare global variable of name 'brandNew'",
                ),
                (
                    "function brandNewFn() {}",
                    "TypeError: Cannot declare global function of name 'brandNewFn'",
                ),
                ("var newVar; newVar", "5"),
            ],
        );
    }

    /// The C++ runtime creates the var bindings of a script in the order of its HashTable's buckets, where the spec
    /// uses declaration order; Object.keys(globalThis) in Build/release/bin/js shows it.
    #[test]
    fn global_var_bindings_are_created_in_the_cpp_hash_table_order() {
        let vm = Vm::create();
        let (test_realm, global) = global_with_test_realm(&vm);
        evaluate_session(&vm, test_realm.realm, &[("var zeta, alpha, mid", "undefined")]);
        assert_eq!(own_keys(&vm, &global), "mid,alpha,zeta");

        let (test_realm, global) = global_with_test_realm(&vm);
        evaluate_session(
            &vm,
            test_realm.realm,
            &[(
                "var zeta; var alpha; var mid; var b; var a; var c; var x1, x2, x3, x4, x5, x6, x7, x8, x9, x10;",
                "undefined",
            )],
        );
        assert_eq!(
            own_keys(&vm, &global),
            "x9,a,x8,x6,x1,mid,alpha,x10,zeta,x5,x2,x7,x3,c,b,x4"
        );
    }

    #[test]
    fn function_declarations_become_functions_on_the_global_object() {
        let vm = Vm::create();
        vm.heap().set_should_collect_on_every_allocation(true);
        let test_realm = TestRealm::with_function_intrinsics(&vm);
        let global = set_up_global_object(&vm, test_realm.realm);
        let source: Vec<u16> = "var before; function declared(a, b) {}".encode_utf16().collect();
        let script = Script::parse(&vm, &source, test_realm.realm).expect("the script parses");
        assert_eq!(script.functions_to_initialize().len(), 1);
        assert_eq!(vm.run_script(script, None).must(), Value::UNDEFINED);
        let declared = global.get(&vm, &key("declared")).must();
        assert!(declared.is_function());
        let length = declared.as_object().get(&vm, &key("length")).must();
        assert_eq!(length, Value::from_i32(2));
    }

    #[test]
    fn a_script_keeps_its_host_defined_cell_alive() {
        let vm = Vm::create();
        let test_realm = TestRealm::new(&vm);
        let source: Vec<u16> = "var declared = 1;".encode_utf16().collect();
        check_that_host_defined_slots_keep_their_cells_alive(
            &vm,
            &test_realm,
            |host_defined| {
                let source_code = SourceCode::create(ak::Utf16String::default(), ak::Utf16String::from_utf16(&source));
                let compiled = compile_script(parse(&source, ProgramType::Script, 1), source.len());
                Script::create(&vm, test_realm.realm, compiled, source_code, "", host_defined)
            },
            |script| script.host_defined(),
        );
    }

    #[test]
    fn scripts_keep_their_realm_and_declarations() {
        let vm = Vm::create();
        vm.heap().set_should_collect_on_every_allocation(true);
        let (test_realm, _global) = global_with_test_realm(&vm);
        let source: Vec<u16> = "let a = 1; const b = 2; var c = 3; var c;".encode_utf16().collect();
        let script = Script::parse(&vm, &source, test_realm.realm).expect("the script parses");
        vm.heap().collect_garbage();
        assert!(script.realm() == test_realm.realm);
        let names = |names: &[Utf16FlyString]| {
            names
                .iter()
                .map(|name| crate::utf16::Utf16View::of_fly_string(name).to_utf8())
                .collect::<Vec<_>>()
        };
        assert_eq!(names(&script.lexical_names), ["a", "b"]);
        assert_eq!(names(&script.var_names), ["c", "c"]);
        assert_eq!(names(&script.var_scoped_names), ["c", "c"]);
        assert_eq!(
            script
                .lexical_bindings
                .iter()
                .map(|binding| binding.is_constant)
                .collect::<Vec<_>>(),
            [false, true]
        );
        assert!(!script.is_strict_mode);
        assert_eq!(vm.run_script(script, None).must(), Value::UNDEFINED);
        assert_eq!(evaluate(&vm, test_realm.realm, "a + b + c"), "6");
    }

    #[test]
    fn a_script_may_run_in_an_overridden_lexical_environment() {
        let vm = Vm::create();
        let (test_realm, _global) = global_with_test_realm(&vm);
        evaluate_session(&vm, test_realm.realm, &[("let outer = 1", "undefined")]);

        let inner = crate::runtime::declarative_environment::DeclarativeEnvironment::create(
            &vm,
            Some(test_realm.realm.global_environment().upcast()),
        );
        let shadowing = Utf16FlyString::from_utf8("outer");
        inner.create_mutable_binding(&vm, &shadowing, false).must();
        inner
            .initialize_binding(
                &vm,
                &shadowing,
                Value::from_string(crate::runtime::primitive_string::PrimitiveString::create_from_utf8(
                    &vm, "shadow",
                )),
                crate::runtime::environment::InitializeBindingHint::Normal,
            )
            .must();
        // Plain reads of globals go straight to the global environment; typeof resolves through the lexical one.
        let source: Vec<u16> = "typeof outer".encode_utf16().collect();
        let script = Script::parse(&vm, &source, test_realm.realm).expect("the script parses");
        assert_eq!(
            vm.run_script(script, Some(inner.upcast())).must().as_string().to_utf8(),
            "string"
        );
        assert_eq!(evaluate(&vm, test_realm.realm, "typeof outer"), "\"number\"");
    }
}

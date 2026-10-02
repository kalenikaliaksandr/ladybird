/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

use core::cell::Cell;
use core::ops::Deref;

use libjs_runtime_macros::Trace;

use crate::gc::class::{Finalize, GcCell, define_cell};
use crate::gc::gc_ref_cell::GcRefCell;
use crate::gc::root::MarkedVec;
use crate::interpreter::vm::Vm;
use crate::layout::cell::Gc;
use crate::layout::object::Object;
use crate::layout::value::Value;
use crate::runtime::completion::{Completion, ThrowCompletionOr};
use crate::runtime::function_object::FunctionObject;
use crate::runtime::generator_object::{GeneratorObject, GeneratorState, IterationResult};
use crate::runtime::iterator::{IteratorRecord, iterator_close_all};
use crate::runtime::iterator_constructor::{ConcatIterator, ZipIterator};
use crate::runtime::iterator_prototype::{self, FlatMapIterator};
use crate::runtime::realm::Realm;

pub const ITERATOR_HELPER_BRAND: &str = "Iterator Helper";

/// IteratorHelper::Closure and IteratorHelper::AbruptClosure: the abstract closure an iterator helper runs, with what
/// it captures. The C++ runtime keeps each as a GC::Function; here the closures are functions of the files whose
/// built-ins create them, and the state that changes as the helper runs lives in cells they capture, as in C++.
#[derive(Clone, Copy, Trace)]
pub enum IteratorHelperClosure {
    Concat {
        iterables: Gc<ConcatIterator>,
    },
    Drop {
        iterated: Gc<IteratorRecord>,
        integer_limit: f64,
    },
    Filter {
        iterated: Gc<IteratorRecord>,
        predicate: Gc<FunctionObject>,
    },
    FlatMap {
        iterated: Gc<IteratorRecord>,
        flat_map_iterator: Gc<FlatMapIterator>,
        mapper: Gc<FunctionObject>,
    },
    Map {
        iterated: Gc<IteratorRecord>,
        mapper: Gc<FunctionObject>,
    },
    Take {
        iterated: Gc<IteratorRecord>,
        integer_limit: f64,
    },
    Zip {
        zip_iterator: Gc<ZipIterator>,
    },
}

impl IteratorHelperClosure {
    fn call(self, vm: &Vm, iterator: &IteratorHelper) -> ThrowCompletionOr<IterationResult> {
        match self {
            Self::Concat { iterables } => iterables.next(vm, iterator),
            Self::Drop {
                iterated,
                integer_limit,
            } => iterator_prototype::drop_closure(vm, iterator, iterated, integer_limit),
            Self::Filter { iterated, predicate } => {
                iterator_prototype::filter_closure(vm, iterator, iterated, predicate)
            }
            Self::FlatMap {
                iterated,
                flat_map_iterator,
                mapper,
            } => flat_map_iterator.next(vm, iterated, iterator, mapper),
            Self::Map { iterated, mapper } => iterator_prototype::map_closure(vm, iterator, iterated, mapper),
            Self::Take {
                iterated,
                integer_limit,
            } => iterator_prototype::take_closure(vm, iterator, iterated, integer_limit),
            Self::Zip { zip_iterator } => zip_iterator.next(vm),
        }
    }

    /// The abrupt closure, for the helpers that have one.
    fn abrupt_closure(self, vm: &Vm, completion: Completion) -> Option<ThrowCompletionOr<Value>> {
        match self {
            Self::Concat { iterables } => Some(iterables.on_abrupt_completion(vm, completion)),
            Self::FlatMap {
                iterated,
                flat_map_iterator,
                ..
            } => Some(flat_map_iterator.on_abrupt_completion(vm, iterated, completion)),
            Self::Zip { zip_iterator } => Some(zip_iterator.on_abrupt_completion(vm, completion)),
            Self::Drop { .. } | Self::Filter { .. } | Self::Map { .. } | Self::Take { .. } => None,
        }
    }
}

#[repr(C)]
#[derive(Trace)]
pub struct IteratorHelper {
    base: GeneratorObject,
    underlying_iterators: GcRefCell<Vec<Gc<IteratorRecord>>>, // [[UnderlyingIterators]]
    closure: Cell<IteratorHelperClosure>,
    #[gc(untraced)]
    counter: Cell<usize>,
}

define_cell!(IteratorHelper, Object, extends: [GeneratorObject, Object], finalize: finalize);

impl Deref for IteratorHelper {
    type Target = GeneratorObject;

    fn deref(&self) -> &GeneratorObject {
        &self.base
    }
}

impl Finalize for IteratorHelper {
    fn finalize(&self) {
        Finalize::finalize(&self.base);
        drop(self.underlying_iterators.replace(Vec::new()));
    }
}

impl IteratorHelper {
    pub fn create(
        vm: &Vm,
        realm: Gc<Realm>,
        underlying_iterators: &MarkedVec<'_, Gc<IteratorRecord>>,
        closure: IteratorHelperClosure,
    ) -> Gc<IteratorHelper> {
        let prototype = realm.intrinsics().iterator_helper_prototype();
        let running_execution_context = vm
            .running_execution_context()
            .expect("an iterator helper is created by a running built-in");
        // SAFETY: The running execution context is live.
        let execution_context = unsafe { running_execution_context.as_ref() }.copy();
        let iterator = realm.create_object(
            vm,
            IteratorHelper {
                base: GeneratorObject::new(
                    vm,
                    Self::CLASS,
                    realm,
                    Some(prototype),
                    execution_context,
                    Some(ITERATOR_HELPER_BRAND),
                    IteratorHelper::execute,
                ),
                underlying_iterators: GcRefCell::new(Vec::new()),
                closure: Cell::new(closure),
                counter: Cell::new(0),
            },
        );
        *iterator.underlying_iterators.borrow_mut() = underlying_iterators.to_vec();
        iterator
    }

    /// A copy of [[UnderlyingIterators]], which stays alive while iterators are closed.
    pub fn underlying_iterators<'vm>(&self, vm: &'vm Vm) -> MarkedVec<'vm, Gc<IteratorRecord>> {
        let underlying_iterators = MarkedVec::new(vm);
        for iterator_record in self.underlying_iterators.borrow().iter() {
            underlying_iterators.push(*iterator_record);
        }
        underlying_iterators
    }

    pub fn counter(&self) -> usize {
        self.counter.get()
    }

    pub fn increment_counter(&self) {
        self.counter.set(self.counter.get() + 1);
    }

    fn execute(generator: &GeneratorObject, vm: &Vm, completion: Completion) -> ThrowCompletionOr<IterationResult> {
        assert!(generator.is::<IteratorHelper>());
        // SAFETY: Only iterator helpers execute through this, and an IteratorHelper starts with its GeneratorObject.
        let iterator = unsafe { &*core::ptr::from_ref(generator).cast::<IteratorHelper>() };
        let result = iterator.execute_closure(vm, completion);
        vm.pop_execution_context();
        result
    }

    fn execute_closure(&self, vm: &Vm, completion: Completion) -> ThrowCompletionOr<IterationResult> {
        let closure = self.closure.get();

        if completion.is_abrupt() {
            // NB: Like the C++ runtime, this leaves the helper executing when closing its iterators throws, so that it
            //     throws on every later call.
            let abrupt_result = match closure.abrupt_closure(vm, completion) {
                Some(abrupt_result) => abrupt_result?,
                None => {
                    iterator_close_all(vm, &self.underlying_iterators(vm), completion).into_throw_completion_or()?
                }
            };

            self.set_generator_state(GeneratorState::Completed);
            return Ok(IterationResult::new(abrupt_result, true));
        }

        let result_value = closure.call(vm, self);

        let result = match result_value {
            Err(throw) => {
                self.set_generator_state(GeneratorState::Completed);
                return Err(throw);
            }
            Ok(result) => result,
        };
        self.set_generator_state(if result.done {
            GeneratorState::Completed
        } else {
            GeneratorState::SuspendedYield
        });

        Ok(result)
    }
}

#[cfg(all(test, libjs_runtime_tests_with_libgc))]
mod tests {
    use crate::interpreter::vm::Vm;
    use crate::runtime::completion::Must;
    use crate::script::Script;
    use crate::utilities::initialize_realm;

    const HELPERS: &str = r#"
function* numbers(n) { for (let i = 0; i < n; i++) yield { value: i }; }
var helpers = [
    numbers(5).map(x => ({ boxed: x.value * 2 })),
    numbers(6).filter(x => x.value % 2).map(x => x.value),
    numbers(4).flatMap(x => [{ v: x.value }, { v: -x.value }].values()).map(o => o.v),
    Iterator.concat([{ a: 1 }].values(), numbers(2)).map(x => JSON.stringify(x)),
    Iterator.zip([numbers(3), ["a", "b"]], { mode: "longest", padding: [{ p: 1 }, { p: 2 }] }).map(r => JSON.stringify(r)),
    Iterator.zipKeyed({ x: numbers(2), y: [{ q: 1 }] }, { mode: "longest" }).map(r => JSON.stringify(r)),
    numbers(10).drop(3).take(4).map(x => x.value),
    Iterator.from({ next() { return { value: { n: 1 }, done: false }; } }).take(2).map(x => x.n),
];
var garbage = [];
helpers.map(h => {
    let out = [];
    for (let v of h) {
        garbage.push({ junk: [v] });
        out.push(typeof v === "object" ? JSON.stringify(v) : String(v));
    }
    return out.join(",");
}).join(" | ")
"#;

    /// The state the closures of iterator helpers capture lives in cells the helpers trace, as do the iterators a zip
    /// pads and the iterables a concat has yet to open.
    #[test]
    fn iterator_helpers_keep_what_their_closures_capture_alive() {
        let vm = Vm::create();
        vm.heap().set_should_collect_on_every_allocation(true);
        let root_execution_context = initialize_realm(&vm);
        let source: Vec<u16> = HELPERS.encode_utf16().collect();
        let script = Script::parse(&vm, &source, root_execution_context.realm()).expect("the script parses");
        let result = vm.run_script(script, None).must();
        vm.heap().set_should_collect_on_every_allocation(false);
        assert_eq!(
            result.as_string().to_utf8(),
            "{\"boxed\":0},{\"boxed\":2},{\"boxed\":4},{\"boxed\":6},{\"boxed\":8} | 1,3,5 | 0,0,1,-1,2,-2,3,-3 | \
             {\"a\":1},{\"value\":0},{\"value\":1} | [{\"value\":0},\"a\"],[{\"value\":1},\"b\"],[{\"value\":2},{\"p\":2}] | \
             {\"x\":{\"value\":0},\"y\":{\"q\":1}},{\"x\":{\"value\":1}} | 3,4,5,6 | 1,1"
        );
    }
}

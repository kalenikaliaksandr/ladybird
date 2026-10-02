/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

use core::cell::Cell;
use core::ops::Deref;
use std::collections::HashMap;

use libjs_runtime_macros::Trace;

use crate::gc::class::{Finalize, GcCell, define_cell};
use crate::gc::gc_ref_cell::GcRefCell;
use crate::gc::visitor::{Trace, Visitor};
use crate::interpreter::vm::Vm;
use crate::layout::cell::Gc;
use crate::layout::object::Object;
use crate::layout::value::Value;
use crate::runtime::object::MayInterfereWithIndexedPropertyAccess;
use crate::runtime::realm::Realm;
use crate::runtime::value_traits::ValueTraitsKey;

#[derive(Trace)]
struct StoredEntry {
    key: Value,
    value: Value,
    insertion_id: u64,
}

impl StoredEntry {
    fn is_removed(&self) -> bool {
        self.key.is_empty()
    }
}

#[derive(Default)]
struct MapStorage {
    /// All entries in insertion order. Removed entries stay in place (as holes) until the next compaction, so the
    /// positions held by iterators stay valid. Compacting or clearing moves entries, which bumps generation.
    entries: Vec<StoredEntry>,
    indices: HashMap<ValueTraitsKey, usize, foldhash::fast::RandomState>,
    removed_entry_count: usize,
    next_insertion_id: u64,
    generation: u64,
}

// SAFETY: Visits the key and value of every entry. The keys of the indices are the keys of the live entries.
unsafe impl Trace for MapStorage {
    fn trace(&self, visitor: &mut Visitor) {
        self.entries.trace(visitor);
    }
}

impl MapStorage {
    fn index_of_first_entry_not_inserted_before(&self, insertion_id: u64) -> usize {
        // Entries are stored in insertion order, so their insertion IDs are increasing.
        self.entries.partition_point(|entry| entry.insertion_id < insertion_id)
    }

    fn compact_entries(&mut self) {
        let mut new_indices = vec![0usize; self.entries.len()];

        let mut live_entry_count = 0;
        for (i, new_index) in new_indices.iter_mut().enumerate() {
            if self.entries[i].is_removed() {
                continue;
            }
            *new_index = live_entry_count;
            self.entries.swap(live_entry_count, i);
            live_entry_count += 1;
        }
        self.entries.truncate(live_entry_count);
        if self.entries.capacity() > live_entry_count * 2 {
            self.entries.shrink_to_fit();
        }

        for index in self.indices.values_mut() {
            *index = new_indices[*index];
        }

        self.removed_entry_count = 0;
        self.generation += 1;
    }
}

/// The key and value of a Map entry.
#[derive(Clone, Copy)]
pub struct Entry {
    pub key: Value,
    pub value: Value,
}

#[repr(C)]
#[derive(Trace)]
pub struct Map {
    base: Object,
    storage: GcRefCell<MapStorage>,
}

define_cell!(Map, Object, extends: [Object], finalize: finalize);

impl Deref for Map {
    type Target = Object;

    fn deref(&self) -> &Object {
        &self.base
    }
}

impl Finalize for Map {
    fn finalize(&self) {
        Finalize::finalize(&self.base);
        drop(self.storage.replace(MapStorage::default()));
    }
}

impl Map {
    pub fn new(vm: &Vm, prototype: Gc<Object>) -> Map {
        Map {
            base: Object::new_with_prototype(vm, Self::CLASS, prototype, MayInterfereWithIndexedPropertyAccess::No),
            storage: GcRefCell::new(MapStorage::default()),
        }
    }

    pub fn create(vm: &Vm, realm: Gc<Realm>) -> Gc<Map> {
        realm.create_object(vm, Map::new(vm, realm.intrinsics().map_prototype(vm)))
    }

    // 24.1.3.1 Map.prototype.clear ( ), https://tc39.es/ecma262/#sec-map.prototype.clear
    pub fn map_clear(&self) {
        let mut storage = self.storage.borrow_mut();
        storage.entries.clear();
        storage.indices.clear();
        storage.removed_entry_count = 0;
        storage.generation += 1;
    }

    // 24.1.3.3 Map.prototype.delete ( key ), https://tc39.es/ecma262/#sec-map.prototype.delete
    pub fn map_remove(&self, key: Value) -> bool {
        let mut storage = self.storage.borrow_mut();
        let Some(index) = storage.indices.remove(&ValueTraitsKey(key)) else {
            return false;
        };

        let entry = &mut storage.entries[index];
        entry.key = Value::EMPTY;
        entry.value = Value::UNDEFINED;
        storage.removed_entry_count += 1;

        // Compact once removed entries outnumber the live ones, so removal stays amortized O(1).
        const MINIMUM_REMOVED_ENTRY_COUNT_FOR_COMPACTION: usize = 8;
        if storage.removed_entry_count >= MINIMUM_REMOVED_ENTRY_COUNT_FOR_COMPACTION
            && storage.removed_entry_count > storage.indices.len()
        {
            storage.compact_entries();
        }

        true
    }

    // 24.1.3.6 Map.prototype.get ( key ), https://tc39.es/ecma262/#sec-map.prototype.get
    pub fn map_get(&self, key: Value) -> Option<Value> {
        let storage = self.storage.borrow();
        let index = *storage.indices.get(&ValueTraitsKey(key))?;
        Some(storage.entries[index].value)
    }

    // 24.1.3.7 Map.prototype.has ( key ), https://tc39.es/ecma262/#sec-map.prototype.has
    pub fn map_has(&self, key: Value) -> bool {
        self.storage.borrow().indices.contains_key(&ValueTraitsKey(key))
    }

    // 24.1.3.9 Map.prototype.set ( key, value ), https://tc39.es/ecma262/#sec-map.prototype.set
    pub fn map_set(&self, key: Value, value: Value) {
        let mut storage = self.storage.borrow_mut();
        let new_index = storage.entries.len();
        let index = *storage.indices.entry(ValueTraitsKey(key)).or_insert(new_index);
        if index != new_index {
            storage.entries[index].value = value;
            return;
        }
        let insertion_id = storage.next_insertion_id;
        storage.next_insertion_id += 1;
        storage.entries.push(StoredEntry {
            key,
            value,
            insertion_id,
        });
    }

    pub fn map_size(&self) -> usize {
        self.storage.borrow().indices.len()
    }

    /// Calls the callback with the key and value of every entry, in insertion order.
    /// The callback must not modify the map.
    pub fn for_each_entry(&self, mut callback: impl FnMut(Value, Value)) {
        let (generation, entry_count) = {
            let storage = self.storage.borrow();
            (storage.generation, storage.entries.len())
        };
        for index in 0..entry_count {
            let (key, value) = {
                let entry = &self.storage.borrow().entries[index];
                (entry.key, entry.value)
            };
            if !key.is_empty() {
                callback(key, value);
            }
        }
        let storage = self.storage.borrow();
        assert!(generation == storage.generation && entry_count == storage.entries.len());
    }

    pub fn begin(&self) -> ConstIterator {
        ConstIterator {
            // SAFETY: Maps only exist as cells, since every way to create one allocates it.
            map: unsafe { Gc::from_ref(self) },
            index: Cell::new(0),
            generation: Cell::new(self.storage.borrow().generation),
            next_insertion_id: Cell::new(0),
            current_insertion_id: Cell::new(None),
        }
    }
}

/// An iterator that stays valid while the map is modified, with the visiting rules of the spec's index-based loops:
/// entries added during iteration are visited, removed entries are skipped, and entries that were moved by a
/// compaction or cleared are found again by their insertion ID. The C++ Map::ConstIterator, which compares equal to
/// Map::end() once is_end() is true.
#[derive(Trace)]
pub struct ConstIterator {
    map: Gc<Map>,

    /// The position of the current entry in the entries. Only meaningful while generation matches the map.
    index: Cell<usize>,
    generation: Cell<u64>,

    /// Every entry with a smaller insertion ID has already been visited or skipped.
    next_insertion_id: Cell<u64>,
    current_insertion_id: Cell<Option<u64>>,
}

impl ConstIterator {
    pub fn is_end(&self) -> bool {
        self.find_current_entry();
        self.index.get() >= self.map.storage.borrow().entries.len()
    }

    /// operator++. This moves past the entry that is_end() or current() found last, even if that entry has been
    /// removed since, so removing the current entry during iteration does not skip the entry after it.
    pub fn advance(&self) {
        if self.current_insertion_id.get().is_none() {
            self.find_current_entry();
            if self.current_insertion_id.get().is_none() {
                return;
            }
        }
        let current_insertion_id = self
            .current_insertion_id
            .take()
            .expect("the current entry was just found");
        self.next_insertion_id.set(current_insertion_id + 1);
        let storage = self.map.storage.borrow();
        let index = self.index.get();
        if self.generation.get() == storage.generation
            && index < storage.entries.len()
            && storage.entries[index].insertion_id < self.next_insertion_id.get()
        {
            self.index.set(index + 1);
        }
    }

    /// operator*.
    pub fn current(&self) -> Entry {
        self.find_current_entry();
        let storage = self.map.storage.borrow();
        let entry = &storage.entries[self.index.get()];
        Entry {
            key: entry.key,
            value: entry.value,
        }
    }

    fn find_current_entry(&self) {
        let storage = self.map.storage.borrow();
        let entries = &storage.entries;
        if self.generation.get() != storage.generation {
            self.index
                .set(storage.index_of_first_entry_not_inserted_before(self.next_insertion_id.get()));
            self.generation.set(storage.generation);
        }
        let mut index = self.index.get();
        while index < entries.len() && entries[index].is_removed() {
            index += 1;
        }
        self.index.set(index);
        if index < entries.len() {
            self.current_insertion_id.set(Some(entries[index].insertion_id));
        } else {
            self.current_insertion_id.set(None);
        }
    }
}

#[cfg(all(test, libjs_runtime_tests_with_libgc))]
mod tests {
    use super::*;
    use crate::runtime::completion::Must;
    use crate::runtime::error::test_scripts::{run_script, utf8};
    use crate::utilities::initialize_realm;

    fn visit_all(map: Gc<Map>, mut on_entry: impl FnMut(i32)) -> Vec<i32> {
        let mut visited = Vec::new();
        let iterator = map.begin();
        while !iterator.is_end() {
            let key = iterator.current().key.as_i32();
            visited.push(key);
            on_entry(key);
            iterator.advance();
        }
        visited
    }

    #[test]
    fn iterators_find_their_place_again_after_removals_compaction_and_clear() {
        let vm = Vm::create();
        let root_execution_context = initialize_realm(&vm);
        let map = Map::create(&vm, root_execution_context.realm());
        let int = Value::from_i32;
        for i in 0..20 {
            map.map_set(int(i), int(i * 10));
        }

        // Removing the current entry does not skip the next one; removing more than the live entries compacts the
        // storage under the iterator; entries added during iteration are visited.
        let visited = visit_all(map, |key| match key {
            1 => {
                map.map_remove(int(1));
            }
            2 => {
                for i in 3..15 {
                    map.map_remove(int(i));
                }
            }
            16 => {
                map.map_set(int(100), int(0));
                map.map_remove(int(17));
                map.map_set(int(0), int(1));
            }
            _ => {}
        });
        assert_eq!(visited, [0, 1, 2, 15, 16, 18, 19, 100]);
        assert_eq!(map.map_size(), 7);
        assert_eq!(map.map_get(int(0)), Some(int(1)));

        // A cleared map is visited from the entries added after the clear.
        let visited = visit_all(map, |key| {
            if key == 15 {
                map.map_clear();
                map.map_set(int(7), int(7));
                map.map_set(int(8), int(8));
            }
        });
        assert_eq!(visited, [0, 2, 15, 7, 8]);

        // An exhausted iterator sees the entries added later, as the spec's index-based loops do.
        let iterator = map.begin();
        while !iterator.is_end() {
            iterator.advance();
        }
        map.map_set(int(9), int(9));
        assert!(!iterator.is_end());
        assert_eq!(iterator.current().key, int(9));
    }

    #[test]
    fn keys_compare_with_same_value_and_hash_their_contents() {
        let vm = Vm::create();
        let root_execution_context = initialize_realm(&vm);
        let realm = root_execution_context.realm();
        let source = r#"
            var m = new Map();
            m.set("a" + "b", 1);
            m.set(NaN, 2);
            m.set(-0, 3);
            m.set(10n ** 20n, 4);
            [m.get("ab"), m.get(0 / 0), m.get(0), m.has(-0), m.get(100000000000000000000n), m.size,
             [...m.keys()].map(String)].join()
        "#;
        assert_eq!(
            utf8(run_script(&vm, realm, source).must()),
            "1,2,3,true,4,4,ab,NaN,0,100000000000000000000"
        );
    }

    const SCRIPT_RESULTS: &[(&str, &str)] = &[
        (
            "var m = new Map([[1, 'a'], [2, 'b']]); var out = []; m.forEach((v, k) => { out.push(k + v); if (k === 1) { m.delete(2); m.set(3, 'c'); } }); out.join()",
            "1a,3c",
        ),
        (
            "var s = new Set([1, 2, 3]); [...s.union(new Set(['x' + 1]))].concat([...s.intersection({ size: 10, has: v => { s.delete(3); s.add(4); return true; }, keys: () => [][Symbol.iterator]() })]).join()",
            "1,2,3,x1,1,2,4",
        ),
        (
            "JSON.stringify([Array.from(new Map([[1, {}], ['k', [2]]])), [...new Set('abca')], [...new Map([[1, 2]]).entries()]])",
            "[[[1,{}],[\"k\",[2]]],[\"a\",\"b\",\"c\"],[[1,2]]]",
        ),
        (
            "var g = Map.groupBy(['one', 'two', 'three'], s => s.length); [...g.keys()].join() + ':' + g.get(3).join()",
            "3,5:one,two",
        ),
        (
            "var w = new WeakMap(); var k = {}; w.set(k, [1]); var s = new WeakSet([k]); w.getOrInsertComputed({}, () => 1); [w.get(k)[0], s.has(k), new WeakRef(k).deref() === k].join()",
            "1,true,true",
        ),
    ];

    fn check_script_results(collect_on_every_allocation: bool) {
        let vm = Vm::create();
        vm.heap()
            .set_should_collect_on_every_allocation(collect_on_every_allocation);
        let root_execution_context = initialize_realm(&vm);
        let realm = root_execution_context.realm();
        for (source, expected) in SCRIPT_RESULTS {
            let result =
                run_script(&vm, realm, source).map_or_else(|throw| format!("threw {}", utf8(throw.value())), utf8);
            assert_eq!(result, *expected, "for {source}");
        }
        vm.heap().set_should_collect_on_every_allocation(false);
    }

    #[test]
    fn keyed_collections_behave_like_the_cpp_runtime() {
        check_script_results(false);
    }

    #[test]
    fn keyed_collections_keep_everything_alive_when_collecting_on_every_allocation() {
        check_script_results(true);
    }
}

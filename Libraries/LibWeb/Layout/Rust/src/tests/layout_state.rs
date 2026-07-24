use crate::layout_state::*;

#[test]
fn paged_store_allocates_and_gets_sparse_indices() {
    let mut store = PagedStore::<u32>::default();
    assert!(store.get(1).is_null());
    let first = store.allocate(1, 11u32);
    let distant = store.allocate(63, 22u32);
    assert_eq!(store.get(1), first);
    assert_eq!(store.get(63), distant);
    assert!(store.get(2).is_null());
    // SAFETY: Both pointers refer to initialized entries owned by store.
    unsafe {
        assert_eq!(*first, 11);
        assert_eq!(*distant, 22);
    }
}

#[test]
fn ensure_capacity_preallocates_only_the_top_level_table() {
    let mut store = PagedStore::<u32>::default();
    store.ensure_capacity(33);
    assert_eq!(store.pages.len(), 3);
    assert!(store.pages.iter().all(Option::is_none));
    store.allocate(32, 7);
    assert!(store.pages[2].is_some());
    assert!(store.pages[0].is_none());
}

#[test]
fn allocation_grows_beyond_ensured_capacity() {
    let mut store = PagedStore::default();
    store.ensure_capacity(1);
    store.allocate(80, 5u32);
    assert_eq!(store.pages.len(), 6);
    // SAFETY: The returned entry is initialized and owned by store.
    unsafe {
        assert_eq!(*store.get(80), 5);
    }
}

#[test]
fn iteration_follows_page_and_entry_order() {
    let mut store = PagedStore::default();
    store.allocate(31, 31u32);
    store.allocate(1, 1u32);
    store.allocate(17, 17u32);
    let mut values = Vec::new();
    store.for_each(|value| values.push(*value));
    assert_eq!(values, [1, 17, 31]);
}

#[test]
#[should_panic(expected = "assertion failed: entry.is_null()")]
fn duplicate_allocation_is_rejected() {
    let mut store = PagedStore::default();
    store.allocate(4, 1u32);
    store.allocate(4, 2u32);
}

/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! Columns whose published generations share storage with the column that goes on changing.
//!
//! A [`CowColumn`] keeps its rows in fixed-size chunks behind [`Arc`]s, and its chunks in groups
//! behind [`Arc`]s, under a spine behind an [`Arc`]. A column holds whole chunks, and a row it was
//! never asked to hold reads as the default. Publishing shares the spine, so a [`ColumnSnapshot`]
//! costs one reference count however large the rows are, and so does dropping it. A write copies
//! the spine, the row's group and the row's chunk only where a snapshot still shares them, so the
//! cost of a generation follows the chunks written after it was published rather than the size of
//! the column. Neither is deep-copied: a snapshot's clone shares its spine, and so does a column's
//! clone, its fork. Both are `Send` and `Sync` when the row type is: the column is written through
//! `&mut`, and a snapshot never changes.
//!
//! A column is written only through [`CowColumn::set`], [`CowColumn::set_rows`] and [`RowMut`], and
//! each compares the rows it writes with the rows a snapshot shares before copying the chunk. So a
//! write that leaves a row as it was never copies a chunk, whichever caller makes it.

use std::ops::{Deref, DerefMut, Range};
use std::sync::Arc;

/// A chunk starts on a cache line of its own, so rows do not share a line with the reference
/// counts or with another chunk.
#[derive(Clone)]
#[repr(align(64))]
struct Chunk<T, const CHUNK: usize>([T; CHUNK]);

/// How many chunks a group holds: a write after a publication copies this many chunk pointers.
const GROUP: usize = 64;

/// The chunks of a group, the column's last group holding fewer than it has room for.
struct Group<T, const CHUNK: usize>([Option<Arc<Chunk<T, CHUNK>>>; GROUP]);

impl<T, const CHUNK: usize> Clone for Group<T, CHUNK> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

type Spine<T, const CHUNK: usize> = Arc<[Arc<Group<T, CHUNK>>]>;

fn chunk_of<T, const CHUNK: usize>(spine: &Spine<T, CHUNK>, chunk_index: usize) -> Option<&Arc<Chunk<T, CHUNK>>> {
    spine.get(chunk_index / GROUP)?.0[chunk_index % GROUP].as_ref()
}

fn row_of<T, const CHUNK: usize>(spine: &Spine<T, CHUNK>, index: usize) -> Option<&T> {
    Some(&chunk_of(spine, index / CHUNK)?.0[index % CHUNK])
}

/// The chunk at `chunk_index` of `spine`, for the caller to make its own.
fn chunk_slot_mut<T, const CHUNK: usize>(
    spine: &mut Spine<T, CHUNK>,
    chunk_index: usize,
) -> &mut Option<Arc<Chunk<T, CHUNK>>> {
    let group = Arc::make_mut(&mut Arc::make_mut(spine)[chunk_index / GROUP]);
    &mut group.0[chunk_index % GROUP]
}

pub(crate) struct CowColumn<T, const CHUNK: usize> {
    spine: Spine<T, CHUNK>,
    /// The chunk the spine holds at each index, so that reaching a row does not walk the spine
    /// and the chunk's group. The spine keeps each alive, and whatever puts another chunk in a
    /// slot of the spine (`grow_to`, and `owned_row` copying one a snapshot shares) updates it.
    chunks: Vec<*mut Chunk<T, CHUNK>>,
    /// Whether each chunk is known to be unshared, with its group and the spine: the column made
    /// them writable after it last published, and nothing but publishing shares them.
    unshared: Vec<std::sync::atomic::AtomicBool>,
    /// Advanced as the column is written. See [`Self::version`].
    version: u64,
}

// SAFETY: `chunks` points only into what `spine` holds, and the column hands out its rows only
// through `&self` and `&mut self`, as the spine alone would.
unsafe impl<T: Send + Sync, const CHUNK: usize> Send for CowColumn<T, CHUNK> {}
// SAFETY: As above.
unsafe impl<T: Send + Sync, const CHUNK: usize> Sync for CowColumn<T, CHUNK> {}

/// A generation of a [`CowColumn`], as it was when published. It does not see later writes.
pub(crate) struct ColumnSnapshot<T, const CHUNK: usize> {
    spine: Spine<T, CHUNK>,
}

impl<T, const CHUNK: usize> Default for CowColumn<T, CHUNK> {
    fn default() -> Self {
        Self {
            spine: Arc::new([]),
            chunks: Vec::new(),
            unshared: Vec::new(),
            version: 0,
        }
    }
}

impl<T, const CHUNK: usize> Default for ColumnSnapshot<T, CHUNK> {
    fn default() -> Self {
        Self { spine: Arc::new([]) }
    }
}

impl<T, const CHUNK: usize> Clone for ColumnSnapshot<T, CHUNK> {
    fn clone(&self) -> Self {
        Self {
            spine: Arc::clone(&self.spine),
        }
    }
}

impl<T: Clone + Default, const CHUNK: usize> CowColumn<T, CHUNK> {
    const CHUNK_IS_NOT_EMPTY: () = assert!(CHUNK > 0);

    #[inline]
    fn chunk(&self, chunk_index: usize) -> Option<&[T; CHUNK]> {
        let chunk = *self.chunks.get(chunk_index)?;
        // SAFETY: The spine holds the chunk, and nothing writes it while `&self` is borrowed.
        Some(unsafe { &(*chunk).0 })
    }

    #[inline]
    pub(crate) fn get(&self, index: usize) -> Option<&T> {
        Some(&self.chunk(index / CHUNK)?[index % CHUNK])
    }

    /// The rows from `rows.start` up to `rows.end`, as one run of rows for each chunk they span, in
    /// order. The runs end at the first row the column does not hold.
    pub(crate) fn runs(&self, rows: Range<usize>) -> impl Iterator<Item = &[T]> {
        let mut start = rows.start;
        std::iter::from_fn(move || {
            if start >= rows.end {
                return None;
            }
            let offset = start % CHUNK;
            let run = &self.chunk(start / CHUNK)?[offset..CHUNK.min(offset + rows.end - start)];
            start += run.len();
            Some(run)
        })
    }

    /// The chunk at `chunk_index` made the column's own, copying the spine, the chunk's group and
    /// the chunk where a snapshot shares them.
    #[inline]
    fn owned_chunk(&mut self, chunk_index: usize) -> Option<&mut [T; CHUNK]> {
        let unshared = *self.unshared.get_mut(chunk_index)?.get_mut();
        self.version += 1;
        if !unshared {
            self.make_chunk_unshared(chunk_index)?;
        }
        // SAFETY: The chunk, its group and the spine have not been shared since `make_mut` in
        // `make_chunk_unshared` made them unique: only `publish` shares them, and it forgets which
        // chunks are unshared. `&mut self` keeps any other reference into the column from being live.
        Some(unsafe { &mut (*self.chunks[chunk_index]).0 })
    }

    fn make_chunk_unshared(&mut self, chunk_index: usize) -> Option<()> {
        let chunk = chunk_slot_mut(&mut self.spine, chunk_index).as_mut()?;
        Arc::make_mut(chunk);
        self.chunks[chunk_index] = Arc::as_ptr(chunk).cast_mut();
        *self.unshared[chunk_index].get_mut() = true;
        Some(())
    }

    #[inline]
    fn owned_row(&mut self, index: usize) -> Option<&mut T> {
        Some(&mut self.owned_chunk(index / CHUNK)?[index % CHUNK])
    }

    /// Whether no snapshot shares the chunk at `chunk_index`, marking the chunk so if none does.
    #[inline]
    fn owns_chunk(&mut self, chunk_index: usize) -> bool {
        *self.unshared[chunk_index].get_mut() || self.finds_chunk_unshared(chunk_index)
    }

    /// Whether no snapshot shares the chunk at `chunk_index` any more, which the column has not
    /// marked as its own since it last published, marking it so if none does.
    fn finds_chunk_unshared(&mut self, chunk_index: usize) -> bool {
        let owned = Arc::get_mut(&mut self.spine)
            .and_then(|spine| Arc::get_mut(&mut spine[chunk_index / GROUP]))
            .and_then(|group| Arc::get_mut(group.0[chunk_index % GROUP].as_mut()?))
            .is_some();
        if owned {
            *self.unshared[chunk_index].get_mut() = true;
            self.version += 1;
        }
        owned
    }

    /// Grows the column to hold at least `len` rows, the new ones default. A column never
    /// shrinks: rows are reset in place instead.
    #[inline]
    pub(crate) fn grow_to(&mut self, len: usize) {
        if self.unshared.len() * CHUNK < len {
            self.add_chunks_to_hold(len);
        }
    }

    /// A chunk of default rows, written where the `Arc` keeps them rather than moved there.
    fn default_chunk() -> Arc<Chunk<T, CHUNK>> {
        let mut chunk = Arc::<Chunk<T, CHUNK>>::new_uninit();
        let chunk_pointer = Arc::get_mut(&mut chunk).expect("a new chunk is unique").as_mut_ptr();
        // SAFETY: Nothing else reaches the new chunk, and every row of it is written before `assume_init`.
        unsafe {
            let rows = std::ptr::addr_of_mut!((*chunk_pointer).0).cast::<T>();
            for index in 0..CHUNK {
                rows.add(index).write(T::default());
            }
            chunk.assume_init()
        }
    }

    fn add_chunks_to_hold(&mut self, len: usize) {
        let () = Self::CHUNK_IS_NOT_EMPTY;
        while self.unshared.len() * CHUNK < len {
            let chunk_index = self.unshared.len();
            if chunk_index.is_multiple_of(GROUP) {
                let group = Arc::new(Group(std::array::from_fn(|_| None)));
                self.spine = self.spine.iter().cloned().chain([group]).collect();
            }
            let chunk = Self::default_chunk();
            self.chunks.push(Arc::as_ptr(&chunk).cast_mut());
            *chunk_slot_mut(&mut self.spine, chunk_index) = Some(chunk);
            self.unshared.push(std::sync::atomic::AtomicBool::new(true));
            self.version += 1;
        }
    }

    /// How far the column has been written. A snapshot reads as the column does for as long as the version stays what
    /// it was when the snapshot was published, whoever published since: every write advances it, except one to a chunk
    /// the column already made its own after it last published, whose first write did.
    pub(crate) fn version(&self) -> u64 {
        self.version
    }

    #[cfg(test)]
    pub(crate) fn shares_chunk_with(&self, other: &Self, chunk_index: usize) -> bool {
        chunk_of(&self.spine, chunk_index)
            .zip(chunk_of(&other.spine, chunk_index))
            .is_some_and(|(chunk, other_chunk)| Arc::ptr_eq(chunk, other_chunk))
    }

    /// Forgets which chunks the column owns, once something else shares them.
    fn forget_unshared(&self) {
        for unshared in &self.unshared {
            unshared.store(false, std::sync::atomic::Ordering::Relaxed);
        }
    }

    /// This generation of the column, sharing all of it with the column until the column writes.
    pub(crate) fn publish(&mut self) -> ColumnSnapshot<T, CHUNK> {
        self.forget_unshared();
        ColumnSnapshot {
            spine: Arc::clone(&self.spine),
        }
    }
}

/// A fork of a column shares all of it with the column until either writes, as a snapshot does: it costs one
/// reference count, and each side copies a chunk the other shares the first time it writes there.
impl<T: Clone + Default, const CHUNK: usize> Clone for CowColumn<T, CHUNK> {
    fn clone(&self) -> Self {
        self.forget_unshared();
        Self {
            spine: Arc::clone(&self.spine),
            chunks: self.chunks.clone(),
            unshared: self
                .unshared
                .iter()
                .map(|_| std::sync::atomic::AtomicBool::new(false))
                .collect(),
            version: self.version,
        }
    }
}

impl<T: Clone + Default + PartialEq, const CHUNK: usize> CowColumn<T, CHUNK> {
    /// Sets the row at `index`: in place in a chunk the column owns, and in a chunk a snapshot
    /// shares only if the row changes, copying the chunk then. `None` if the column does not hold
    /// the row.
    #[inline]
    pub(crate) fn set(&mut self, index: usize, value: T) -> Option<()> {
        let chunk_index = index / CHUNK;
        if !*self.unshared.get_mut(chunk_index)?.get_mut() {
            return self.set_in_unmarked_chunk(index, value);
        }
        self.version += 1;
        // SAFETY: As in `owned_chunk`, for a chunk the column marked as its own.
        unsafe { (*self.chunks[chunk_index]).0[index % CHUNK] = value };
        Some(())
    }

    fn set_in_unmarked_chunk(&mut self, index: usize, value: T) -> Option<()> {
        if *self.get(index)? != value {
            *self.owned_row(index)? = value;
        }
        Some(())
    }

    /// Sets the rows from `start` on to `values`, as [`Self::set`] sets each, one chunk at a time: a
    /// chunk a snapshot shares is copied only if a row in it changes. `None` if the column does not
    /// hold all of the rows, having set none.
    pub(crate) fn set_rows(&mut self, start: usize, values: &[T]) -> Option<()> {
        if let Some(last) = values.len().checked_sub(1) {
            self.get(start + last)?;
        }
        let mut index = start;
        let mut values = values;
        while !values.is_empty() {
            let (chunk_index, offset) = (index / CHUNK, index % CHUNK);
            let (run, rest) = values.split_at(values.len().min(CHUNK - offset));
            let rows = offset..offset + run.len();
            if *self.unshared[chunk_index].get_mut() || self.chunk(chunk_index)?[rows.clone()] != *run {
                self.owned_chunk(chunk_index)?[rows].clone_from_slice(run);
            }
            index += run.len();
            values = rest;
        }
        Some(())
    }

    /// The row at `index`, for writing. See [`RowMut`].
    pub(crate) fn row_mut(&mut self, index: usize) -> Option<RowMut<&mut Self, T, CHUNK>> {
        RowMut::new(self, index)
    }
}

/// A row of a [`CowColumn`], for writing. While a snapshot shares the row's chunk, the writes go
/// to a copy of the row, and the row is set from it when the guard drops, copying the chunk only if
/// the row changed. In a chunk the column owns, they go to the row in place. `C` is how the guard
/// holds the column: `&mut` it, or a `RefMut` of it.
pub(crate) struct RowMut<C, T, const CHUNK: usize>
where
    C: DerefMut<Target = CowColumn<T, CHUNK>>,
    T: Clone + Default + PartialEq,
{
    column: C,
    index: usize,
    staged: Option<T>,
}

impl<C, T, const CHUNK: usize> RowMut<C, T, CHUNK>
where
    C: DerefMut<Target = CowColumn<T, CHUNK>>,
    T: Clone + Default + PartialEq,
{
    /// The row at `index` of the column `column` holds. `None` if the column does not hold it.
    pub(crate) fn new(mut column: C, index: usize) -> Option<Self> {
        column.get(index)?;
        let staged = if column.owns_chunk(index / CHUNK) {
            None
        } else {
            column.get(index).cloned()
        };
        Some(Self { column, index, staged })
    }
}

impl<C, T, const CHUNK: usize> Deref for RowMut<C, T, CHUNK>
where
    C: DerefMut<Target = CowColumn<T, CHUNK>>,
    T: Clone + Default + PartialEq,
{
    type Target = T;

    #[inline]
    fn deref(&self) -> &T {
        match &self.staged {
            Some(row) => row,
            None => self.column.get(self.index).expect("the guard's row is in the column"),
        }
    }
}

impl<C, T, const CHUNK: usize> DerefMut for RowMut<C, T, CHUNK>
where
    C: DerefMut<Target = CowColumn<T, CHUNK>>,
    T: Clone + Default + PartialEq,
{
    #[inline]
    fn deref_mut(&mut self) -> &mut T {
        match &mut self.staged {
            Some(row) => row,
            None => self
                .column
                .owned_row(self.index)
                .expect("the guard's row is in the column"),
        }
    }
}

impl<C, T, const CHUNK: usize> Drop for RowMut<C, T, CHUNK>
where
    C: DerefMut<Target = CowColumn<T, CHUNK>>,
    T: Clone + Default + PartialEq,
{
    fn drop(&mut self) {
        if let Some(row) = self.staged.take() {
            self.column.set(self.index, row);
        }
    }
}

impl<T, const CHUNK: usize> ColumnSnapshot<T, CHUNK> {
    /// How many rows the snapshot has room for: every row index below it may be read.
    pub(crate) fn slot_capacity(&self) -> usize {
        self.spine.last().map_or(0, |last| {
            let in_last = last.0.iter().take_while(|chunk| chunk.is_some()).count();
            ((self.spine.len() - 1) * GROUP + in_last) * CHUNK
        })
    }

    #[inline]
    pub(crate) fn get(&self, index: usize) -> Option<&T> {
        row_of(&self.spine, index)
    }
}

/// Whether two rows hold the same shared payload: the same allocation, or two that `same_value`
/// says are equal.
pub(crate) fn same_payload<T: ?Sized>(
    a: Option<&Arc<T>>,
    b: Option<&Arc<T>>,
    same_value: impl FnOnce(&T, &T) -> bool,
) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => Arc::ptr_eq(a, b) || same_value(a, b),
        (None, None) => true,
        _ => false,
    }
}

const _: () = {
    const fn assert_send_and_sync<T: Send + Sync>() {}
    assert_send_and_sync::<CowColumn<u64, 64>>();
    assert_send_and_sync::<ColumnSnapshot<u64, 64>>();
};

#[cfg(test)]
mod tests {
    use super::*;

    type Column = CowColumn<u32, 4>;

    fn column_of(values: &[u32]) -> Column {
        let mut column = Column::default();
        column.grow_to(values.len());
        for (index, &value) in values.iter().enumerate() {
            column.set(index, value).unwrap();
        }
        column
    }

    fn rows(snapshot: &ColumnSnapshot<u32, 4>, count: usize) -> Vec<u32> {
        (0..count).map(|index| *snapshot.get(index).unwrap()).collect()
    }

    fn set(column: &mut Column, index: usize, value: u32) {
        column.set(index, value).unwrap();
    }

    fn chunk(spine: &Spine<u32, 4>, chunk_index: usize) -> &Arc<Chunk<u32, 4>> {
        chunk_of(spine, chunk_index).unwrap()
    }

    #[test]
    fn a_column_reads_back_what_was_written() {
        let mut column = column_of(&[1, 2, 3, 4, 5, 6]);
        set(&mut column, 4, 50);
        assert_eq!(
            (0..6).map(|index| *column.get(index).unwrap()).collect::<Vec<_>>(),
            [1, 2, 3, 4, 50, 6]
        );
    }

    #[test]
    fn a_column_holds_whole_chunks() {
        let mut column = column_of(&[1, 2, 3]);
        assert_eq!(column.get(3), Some(&0));
        assert_eq!(column.get(4), None);
        assert_eq!(column.set(4, 1), None);
        assert!(column.row_mut(4).is_none());
        assert_eq!(column.publish().get(4), None);
    }

    #[test]
    fn growing_fills_with_defaults() {
        let mut column = column_of(&[7]);
        column.grow_to(9);
        assert_eq!(column.get(0), Some(&7));
        assert_eq!(column.get(11), Some(&0));
        assert_eq!(column.get(12), None);
        column.grow_to(3);
        assert_eq!(column.get(11), Some(&0));
    }

    #[test]
    fn a_snapshot_keeps_its_generation_while_the_column_changes() {
        let mut column = column_of(&[1, 2, 3, 4, 5, 6]);
        let first = column.publish();
        set(&mut column, 1, 20);
        column.grow_to(7);
        set(&mut column, 6, 7);
        let second = column.publish();
        set(&mut column, 5, 60);
        assert_eq!(rows(&first, 6), [1, 2, 3, 4, 5, 6]);
        assert_eq!(rows(&second, 7), [1, 20, 3, 4, 5, 6, 7]);
        assert_eq!(rows(&column.publish(), 7), [1, 20, 3, 4, 5, 60, 7]);
    }

    #[test]
    fn a_write_copies_only_the_chunk_a_snapshot_shares_and_only_once() {
        let mut column = column_of(&[1, 2, 3, 4, 5, 6, 7, 8]);
        let snapshot = column.publish();
        set(&mut column, 5, 60);
        assert!(Arc::ptr_eq(chunk(&column.spine, 0), chunk(&snapshot.spine, 0)));
        assert!(!Arc::ptr_eq(chunk(&column.spine, 1), chunk(&snapshot.spine, 1)));
        let copied = Arc::as_ptr(chunk(&column.spine, 1));
        set(&mut column, 6, 70);
        assert_eq!(Arc::as_ptr(chunk(&column.spine, 1)), copied);
        assert_eq!(rows(&snapshot, 8), [1, 2, 3, 4, 5, 6, 7, 8]);
    }

    #[test]
    fn a_snapshot_shares_the_column_and_a_write_copies_only_its_group() {
        let mut column = column_of(&[1; 4 * GROUP + 4]);
        let snapshot = column.publish();
        assert!(Arc::ptr_eq(&column.spine, &snapshot.spine));
        assert!(Arc::ptr_eq(&snapshot.clone().spine, &snapshot.spine));
        set(&mut column, 4 * GROUP, 2);
        assert!(!Arc::ptr_eq(&column.spine, &snapshot.spine));
        assert!(Arc::ptr_eq(&column.spine[0], &snapshot.spine[0]));
        assert!(!Arc::ptr_eq(&column.spine[1], &snapshot.spine[1]));
        assert!(Arc::ptr_eq(chunk(&column.spine, 1), chunk(&snapshot.spine, 1)));
        assert_eq!(snapshot.slot_capacity(), 4 * GROUP + 4);
        assert_eq!(snapshot.get(4 * GROUP), Some(&1));
        assert_eq!(column.publish().get(4 * GROUP), Some(&2));
    }

    #[test]
    fn a_write_that_changes_nothing_copies_no_chunk() {
        let mut column = column_of(&[1, 2, 3, 4, 5, 6, 7, 8]);
        let snapshot = column.publish();
        set(&mut column, 5, 6);
        *column.row_mut(6).unwrap() = 7;
        {
            let mut row = column.row_mut(4).unwrap();
            *row = 50;
            *row = 5;
        }
        assert!(Arc::ptr_eq(chunk(&column.spine, 1), chunk(&snapshot.spine, 1)));
        *column.row_mut(6).unwrap() += 1;
        assert!(!Arc::ptr_eq(chunk(&column.spine, 1), chunk(&snapshot.spine, 1)));
        assert_eq!(rows(&snapshot, 8), [1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(rows(&column.publish(), 8), [1, 2, 3, 4, 5, 6, 8, 8]);
    }

    #[test]
    fn a_row_guard_writes_in_place_once_the_column_owns_the_chunk() {
        let mut column = column_of(&[1, 2, 3, 4]);
        let snapshot = column.publish();
        set(&mut column, 0, 10);
        let copied = Arc::as_ptr(chunk(&column.spine, 0));
        {
            let mut row = column.row_mut(1).unwrap();
            assert!(row.staged.is_none());
            *row = 20;
        }
        assert_eq!(Arc::as_ptr(chunk(&column.spine, 0)), copied);
        assert_eq!(rows(&snapshot, 4), [1, 2, 3, 4]);
        assert_eq!(rows(&column.publish(), 4), [10, 20, 3, 4]);
    }

    #[test]
    fn a_chunk_whose_snapshot_is_gone_is_written_in_place() {
        let mut column = column_of(&[1, 2, 3, 4]);
        drop(column.publish());
        let in_place = Arc::as_ptr(chunk(&column.spine, 0));
        set(&mut column, 2, 30);
        assert_eq!(Arc::as_ptr(chunk(&column.spine, 0)), in_place);
    }

    #[test]
    fn runs_split_rows_at_chunk_boundaries() {
        let column = column_of(&[0, 1, 2, 3, 4, 5, 6, 7, 8, 9]);
        let runs = |rows: Range<usize>| column.runs(rows).map(<[u32]>::to_vec).collect::<Vec<_>>();
        assert_eq!(runs(1..3), [vec![1, 2]]);
        assert_eq!(runs(2..9), [vec![2, 3], vec![4, 5, 6, 7], vec![8]]);
        assert_eq!(runs(4..8), [vec![4, 5, 6, 7]]);
        assert!(runs(5..5).is_empty());
        // The column holds whole chunks: rows up to 12 read as the default, and the runs end there.
        assert_eq!(runs(9..20), [vec![9, 0, 0]]);
    }

    #[test]
    fn setting_rows_copies_only_the_chunks_whose_rows_change() {
        let mut column = column_of(&[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11]);
        let snapshot = column.publish();
        column.set_rows(2, &[2, 3, 40, 50, 6, 7]).unwrap();
        assert!(Arc::ptr_eq(chunk(&column.spine, 0), chunk(&snapshot.spine, 0)));
        assert!(!Arc::ptr_eq(chunk(&column.spine, 1), chunk(&snapshot.spine, 1)));
        assert!(Arc::ptr_eq(chunk(&column.spine, 2), chunk(&snapshot.spine, 2)));
        assert_eq!(rows(&snapshot, 12), [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11]);
        assert_eq!(rows(&column.publish(), 12), [0, 1, 2, 3, 40, 50, 6, 7, 8, 9, 10, 11]);

        // Rows past the last chunk set nothing, not even the rows the column holds.
        assert_eq!(column.set_rows(10, &[100, 110, 120]), None);
        assert_eq!(rows(&column.publish(), 12), [0, 1, 2, 3, 40, 50, 6, 7, 8, 9, 10, 11]);
        assert_eq!(column.set_rows(12, &[]), Some(()));
    }

    #[test]
    fn a_fork_shares_the_column_until_either_side_writes() {
        let mut column = column_of(&[1, 2, 3, 4, 5, 6, 7, 8]);
        let mut fork = column.clone();
        assert!(column.shares_chunk_with(&fork, 0) && column.shares_chunk_with(&fork, 1));
        set(&mut fork, 1, 20);
        set(&mut column, 6, 70);
        assert!(!column.shares_chunk_with(&fork, 0) && !column.shares_chunk_with(&fork, 1));
        assert_eq!(rows(&column.publish(), 8), [1, 2, 3, 4, 5, 6, 70, 8]);
        assert_eq!(rows(&fork.publish(), 8), [1, 20, 3, 4, 5, 6, 7, 8]);
    }

    #[test]
    fn a_snapshot_can_be_read_on_another_thread() {
        let mut column = column_of(&[1, 2, 3, 4, 5]);
        let snapshot = column.publish();
        let reader = std::thread::spawn(move || rows(&snapshot, 5));
        set(&mut column, 0, 10);
        assert_eq!(reader.join().unwrap(), [1, 2, 3, 4, 5]);
    }
}

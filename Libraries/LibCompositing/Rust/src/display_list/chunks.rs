/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! The chunks a tape is cut into, and a tape sent as the chunks its receiver lacks.
//!
//! A producer of tapes ends a chunk where what it records next can change without what it recorded before. The tape
//! that replaces another is then mostly the same chunks, and a chunk is named by what it holds, so a receiver that
//! keeps the tape it got last needs only the chunks that tape does not have.

use super::builder::{COMMAND_ALIGNMENT, HEADER_SIZE};
use super::commands::DisplayListCommandRun;
use super::storage::write_run;
use super::validate::{ValidationError, validate_tape};
use crate::fast_hash::FastMap;
use std::hash::{BuildHasher, Hasher};

/// Names a chunk by what it holds: chunks with one id hold the same records in the same runs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct ChunkId(pub [u64; 2]);

impl ChunkId {
    /// The id of a chunk whose records can still change.
    pub const UNSET: Self = Self([0, 0]);
}

/// Where a chunk of a tape ends. Chunks cover a tape, each starting where the one before it ends, and hold whole
/// runs: a run never continues into the next chunk, even where that one begins in the same visual context.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DisplayListChunk {
    pub id: ChunkId,
    pub byte_end: u32,
    pub run_end: u32,
}

const RUN_SIZE: usize = std::mem::size_of::<DisplayListCommandRun>();

/// The id of the chunk that holds `bytes` in `runs`, which name the bytes by their offsets in the tape the chunk
/// starts at `chunk_start` of.
pub fn chunk_id(bytes: &[u8], runs: &[DisplayListCommandRun], chunk_start: u32) -> ChunkId {
    let write_runs = |hasher: &mut crate::fast_hash::FastHasher| {
        let mut run_bytes = [0; RUN_SIZE];
        for run in runs {
            write_run(
                &DisplayListCommandRun {
                    offset: run.offset - chunk_start,
                    ..*run
                },
                &mut run_bytes,
            );
            hasher.write(&run_bytes);
        }
    };
    // Two digests that differ in their seed and in the order they read the chunk in.
    let mut records_first = foldhash::fast::FixedState::with_seed(0x6368_756e_6b5f_6964).build_hasher();
    records_first.write(bytes);
    write_runs(&mut records_first);
    let mut runs_first = foldhash::fast::FixedState::with_seed(0x7461_7065_5f63_7574).build_hasher();
    write_runs(&mut runs_first);
    runs_first.write(bytes);
    ChunkId([records_first.finish(), runs_first.finish()])
}

/// The bytes and runs of each chunk of a tape, in tape order.
pub fn chunks_of<'a>(
    bytes: &'a [u8],
    runs: &'a [DisplayListCommandRun],
    chunks: &'a [DisplayListChunk],
) -> impl Iterator<Item = (&'a DisplayListChunk, &'a [u8], &'a [DisplayListCommandRun])> {
    let mut byte_start = 0;
    let mut run_start = 0;
    chunks.iter().map(move |chunk| {
        let chunk_bytes = &bytes[byte_start..chunk.byte_end as usize];
        let chunk_runs = &runs[run_start..chunk.run_end as usize];
        byte_start = chunk.byte_end as usize;
        run_start = chunk.run_end as usize;
        (chunk, chunk_bytes, chunk_runs)
    })
}

/// A tape with the chunks it is cut into, which is what an update is written from and read against.
#[derive(Clone, Copy)]
pub struct ChunkedTape<'a> {
    pub bytes: &'a [u8],
    pub runs: &'a [DisplayListCommandRun],
    pub chunks: &'a [DisplayListChunk],
}

/// Where each chunk of a tape is, by its id. Chunks that hold the same are one entry.
pub struct ChunkIndex {
    // The index of the chunk in the tape's chunk table.
    by_id: FastMap<ChunkId, u32>,
}

impl ChunkIndex {
    pub fn of(chunks: &[DisplayListChunk]) -> Self {
        let mut by_id = FastMap::default();
        for (index, chunk) in chunks.iter().enumerate() {
            by_id.entry(chunk.id).or_insert(index as u32);
        }
        Self { by_id }
    }

    fn holds(&self, id: ChunkId) -> bool {
        self.by_id.contains_key(&id)
    }

    fn chunk<'a>(&self, tape: ChunkedTape<'a>, id: ChunkId) -> Option<(&'a [u8], &'a [DisplayListCommandRun], u32)> {
        let index = *self.by_id.get(&id)? as usize;
        let (byte_start, run_start) = match index.checked_sub(1) {
            Some(previous) => (tape.chunks[previous].byte_end, tape.chunks[previous].run_end),
            None => (0, 0),
        };
        let chunk = &tape.chunks[index];
        Some((
            &tape.bytes[byte_start as usize..chunk.byte_end as usize],
            &tape.runs[run_start as usize..chunk.run_end as usize],
            byte_start,
        ))
    }
}

// An update is a count of chunks, an entry for each chunk in tape order, and then, for each chunk the update carries,
// its records followed by its runs, which name the records by their offsets in the chunk.
const UPDATE_HEADER_SIZE: usize = 8;
const ENTRY_SIZE: usize = 32;
const ENTRY_CARRIES_THE_CHUNK: u32 = 1;

struct UpdateEntry {
    id: ChunkId,
    byte_size: u32,
    run_count: u32,
    carries_the_chunk: bool,
}

impl UpdateEntry {
    fn write(&self, out: &mut [u8]) {
        out[0..8].copy_from_slice(&self.id.0[0].to_ne_bytes());
        out[8..16].copy_from_slice(&self.id.0[1].to_ne_bytes());
        out[16..20].copy_from_slice(&self.byte_size.to_ne_bytes());
        out[20..24].copy_from_slice(&self.run_count.to_ne_bytes());
        let flags = if self.carries_the_chunk {
            ENTRY_CARRIES_THE_CHUNK
        } else {
            0
        };
        out[24..28].copy_from_slice(&flags.to_ne_bytes());
        out[28..32].fill(0);
    }

    fn read(bytes: &[u8; ENTRY_SIZE]) -> Result<Self, ValidationError> {
        let u64_at = |offset: usize| u64::from_ne_bytes(bytes[offset..offset + 8].try_into().unwrap());
        let u32_at = |offset: usize| u32::from_ne_bytes(bytes[offset..offset + 4].try_into().unwrap());
        let flags = u32_at(24);
        if flags & !ENTRY_CARRIES_THE_CHUNK != 0 {
            return Err("Display list update has a chunk with unknown flags");
        }
        Ok(Self {
            id: ChunkId([u64_at(0), u64_at(8)]),
            byte_size: u32_at(16),
            run_count: u32_at(20),
            carries_the_chunk: flags & ENTRY_CARRIES_THE_CHUNK != 0,
        })
    }

    fn carried_size(&self) -> usize {
        self.byte_size as usize + self.run_count as usize * RUN_SIZE
    }
}

fn update_entries<'a>(
    tape: ChunkedTape<'a>,
    held_by_receiver: Option<&'a ChunkIndex>,
) -> impl Iterator<Item = (UpdateEntry, &'a [u8], &'a [DisplayListCommandRun])> {
    chunks_of(tape.bytes, tape.runs, tape.chunks).map(move |(chunk, bytes, runs)| {
        (
            UpdateEntry {
                id: chunk.id,
                byte_size: bytes.len() as u32,
                run_count: runs.len() as u32,
                carries_the_chunk: !held_by_receiver.is_some_and(|held| held.holds(chunk.id)),
            },
            bytes,
            runs,
        )
    })
}

/// The size of the update that takes a receiver holding the chunks of `held_by_receiver` to `tape`.
pub fn update_size(tape: ChunkedTape<'_>, held_by_receiver: Option<&ChunkIndex>) -> usize {
    UPDATE_HEADER_SIZE
        + tape.chunks.len() * ENTRY_SIZE
        + update_entries(tape, held_by_receiver)
            .filter(|(entry, _, _)| entry.carries_the_chunk)
            .map(|(entry, _, _)| entry.carried_size())
            .sum::<usize>()
}

/// Writes the update that takes a receiver holding the chunks of `held_by_receiver` to `tape` into `destination`,
/// which is `update_size` bytes.
pub fn write_update(tape: ChunkedTape<'_>, held_by_receiver: Option<&ChunkIndex>, destination: &mut [u8]) {
    let (header, rest) = destination.split_at_mut(UPDATE_HEADER_SIZE);
    header.copy_from_slice(&(tape.chunks.len() as u64).to_ne_bytes());
    let (entries, mut carried) = rest.split_at_mut(tape.chunks.len() * ENTRY_SIZE);
    let (entries, _) = entries.as_chunks_mut::<ENTRY_SIZE>();
    for ((entry, bytes, runs), out) in update_entries(tape, held_by_receiver).zip(entries) {
        entry.write(out);
        if !entry.carries_the_chunk {
            continue;
        }
        let (chunk, after_chunk) = carried.split_at_mut(entry.carried_size());
        let (records, run_records) = chunk.split_at_mut(bytes.len());
        records.copy_from_slice(bytes);
        let chunk_start = runs.first().map_or(0, |run| run.offset);
        let (run_records, _) = run_records.as_chunks_mut::<RUN_SIZE>();
        for (run, out) in runs.iter().zip(run_records) {
            write_run(
                &DisplayListCommandRun {
                    offset: run.offset - chunk_start,
                    ..*run
                },
                out,
            );
        }
        carried = after_chunk;
    }
    debug_assert!(carried.is_empty());
}

/// A tape put together from an update.
pub struct ReceivedTape {
    /// The records, in words so that they are aligned for every command.
    pub words: Vec<u64>,
    pub byte_count: usize,
    pub runs: Vec<DisplayListCommandRun>,
    pub chunks: Vec<DisplayListChunk>,
}

impl ReceivedTape {
    fn bytes_mut(&mut self) -> &mut [u8] {
        // SAFETY: The words hold at least `byte_count` initialized bytes, and any byte is a valid u8.
        unsafe { ak::slice_from_raw_parts_mut(self.words.as_mut_ptr().cast(), self.byte_count) }
    }
}

/// Puts a tape together from `update`, which its receiver has copied out of the memory the sender can write to, and
/// `held`, the tape the update was written against. Each chunk the update carries is checked as a tape of its own;
/// the others are copied from `held`, which passed the same checks when it arrived.
pub fn receive_update(
    update: &[u8],
    held: Option<(ChunkedTape<'_>, &ChunkIndex)>,
) -> Result<ReceivedTape, ValidationError> {
    const TRUNCATED: ValidationError = "Display list update is smaller than what it says it holds";
    let (header, rest) = update.split_at_checked(UPDATE_HEADER_SIZE).ok_or(TRUNCATED)?;
    let chunk_count = usize::try_from(u64::from_ne_bytes(header.try_into().unwrap())).map_err(|_| TRUNCATED)?;
    let entries_size = chunk_count.checked_mul(ENTRY_SIZE).ok_or(TRUNCATED)?;
    let (entry_records, mut carried) = rest.split_at_checked(entries_size).ok_or(TRUNCATED)?;
    let mut entries = Vec::new();
    entries
        .try_reserve_exact(chunk_count)
        .map_err(|_| "Display list update is too large to receive")?;
    let mut byte_count = 0usize;
    let mut run_count = 0usize;
    for record in entry_records.as_chunks::<ENTRY_SIZE>().0 {
        let entry = UpdateEntry::read(record)?;
        let size = entry.byte_size as usize;
        // Every chunk holds a record, and every run of it holds one.
        if size < HEADER_SIZE
            || !size.is_multiple_of(COMMAND_ALIGNMENT)
            || entry.run_count == 0
            || entry.run_count as usize > size / HEADER_SIZE
        {
            return Err("Display list update has a chunk of an invalid size");
        }
        byte_count += size;
        run_count += entry.run_count as usize;
        if u32::try_from(byte_count).is_err() {
            return Err("Display list update describes a tape that is too large");
        }
        entries.push(entry);
    }

    let mut words = Vec::new();
    let mut runs = Vec::new();
    let mut chunks = Vec::new();
    if words.try_reserve_exact(byte_count.div_ceil(8)).is_err()
        || runs.try_reserve_exact(run_count).is_err()
        || chunks.try_reserve_exact(chunk_count).is_err()
    {
        return Err("Display list update is too large to receive");
    }
    words.resize(byte_count.div_ceil(8), 0);
    let mut tape = ReceivedTape {
        words,
        byte_count,
        runs,
        chunks,
    };
    let mut chunk_start = 0usize;
    for entry in &entries {
        let size = entry.byte_size as usize;
        let chunk_end = chunk_start + size;
        if entry.carries_the_chunk {
            let (chunk, after_chunk) = carried.split_at_checked(entry.carried_size()).ok_or(TRUNCATED)?;
            carried = after_chunk;
            let (records, run_records) = chunk.split_at(size);
            tape.bytes_mut()[chunk_start..chunk_end].copy_from_slice(records);
            let chunk_runs = validate_tape(&tape.bytes_mut()[chunk_start..chunk_end], run_records)?;
            tape.runs
                .extend(chunk_runs.into_iter().map(|run| DisplayListCommandRun {
                    offset: run.offset + chunk_start as u32,
                    ..run
                }));
        } else {
            let (held_bytes, held_runs, held_start) = held
                .and_then(|(held_tape, index)| index.chunk(held_tape, entry.id))
                .ok_or("Display list update names a chunk its receiver does not hold")?;
            if held_bytes.len() != size || held_runs.len() != entry.run_count as usize {
                return Err("Display list update names a chunk its receiver holds with another size");
            }
            tape.bytes_mut()[chunk_start..chunk_end].copy_from_slice(held_bytes);
            tape.runs.extend(held_runs.iter().map(|run| DisplayListCommandRun {
                offset: run.offset - held_start + chunk_start as u32,
                ..*run
            }));
        }
        tape.chunks.push(DisplayListChunk {
            id: entry.id,
            byte_end: chunk_end as u32,
            run_end: tape.runs.len() as u32,
        });
        chunk_start = chunk_end;
    }
    Ok(tape)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::display_list::builder::{DisplayListBuilder, RecordedDisplayList};
    use crate::display_list::commands::{ClipNodeIndex, ContextRef, EffectNodeIndex, FillRect, SpatialNodeIndex};
    use libgfx_rust::{Color, CompositingAndBlendingOperator, IntRect};

    fn context(spatial: u32) -> ContextRef {
        ContextRef {
            spatial: SpatialNodeIndex(spatial),
            clip: ClipNodeIndex::NONE,
            effect: EffectNodeIndex::NONE,
        }
    }

    fn fill_rect(y: i32) -> FillRect {
        FillRect {
            rect: IntRect::new(0, y, 10, 10),
            color: Color::default(),
            compositing_and_blending_operator: CompositingAndBlendingOperator::Normal,
            background_color_animation_effect: EffectNodeIndex::NONE,
        }
    }

    // A tape of one chunk for each of `rows`, each a rect at that row in one context and one in another.
    fn tape_of_rows(rows: &[i32]) -> RecordedDisplayList {
        let mut builder = DisplayListBuilder::new();
        for &row in rows {
            builder.append(&fill_rect(row), &[], context(1));
            builder.append(&fill_rect(row + 1), &[], context(2));
            builder.end_chunk();
        }
        builder.finish()
    }

    fn chunked(tape: &RecordedDisplayList) -> ChunkedTape<'_> {
        ChunkedTape {
            bytes: &tape.bytes,
            runs: &tape.command_runs,
            chunks: &tape.chunks,
        }
    }

    fn update_between(tape: &RecordedDisplayList, held: Option<&RecordedDisplayList>) -> Vec<u8> {
        let index = held.map(|held| ChunkIndex::of(&held.chunks));
        let mut update = vec![0xff; update_size(chunked(tape), index.as_ref())];
        write_update(chunked(tape), index.as_ref(), &mut update);
        update
    }

    fn received(update: &[u8], held: Option<&RecordedDisplayList>) -> Result<ReceivedTape, ValidationError> {
        let index = held.map(|held| ChunkIndex::of(&held.chunks));
        receive_update(update, held.map(chunked).zip(index.as_ref()))
    }

    fn assert_is(received: &mut ReceivedTape, tape: &RecordedDisplayList) {
        assert_eq!(received.bytes_mut(), &tape.bytes[..]);
        assert_eq!(received.runs, tape.command_runs);
        assert_eq!(received.chunks, tape.chunks);
    }

    #[test]
    fn a_chunk_is_named_by_its_records_and_runs_wherever_it_is_in_a_tape() {
        let tape = tape_of_rows(&[10, 20, 10]);
        assert_eq!(tape.chunks.len(), 3);
        assert_eq!(tape.chunks[0].id, tape.chunks[2].id);
        assert_ne!(tape.chunks[0].id, tape.chunks[1].id);
        assert_ne!(tape.chunks[0].id, ChunkId::UNSET);

        // The same records in another visual context are another chunk.
        let mut builder = DisplayListBuilder::new();
        builder.append(&fill_rect(10), &[], context(1));
        builder.append(&fill_rect(11), &[], context(3));
        assert_ne!(builder.finish().chunks[0].id, tape.chunks[0].id);
    }

    #[test]
    fn an_update_to_a_receiver_that_holds_nothing_carries_every_chunk() {
        let tape = tape_of_rows(&[10, 20, 30]);
        let update = update_between(&tape, None);
        assert_is(&mut received(&update, None).unwrap(), &tape);
    }

    #[test]
    fn an_update_carries_only_the_chunks_the_receiver_lacks() {
        let held = tape_of_rows(&[10, 20, 30, 40]);
        // One chunk gone, one new, and the rest in another order.
        let tape = tape_of_rows(&[40, 10, 50, 20]);
        let update = update_between(&tape, Some(&held));
        let one_chunk = tape.chunks[0].byte_end as usize + tape.chunks[0].run_end as usize * RUN_SIZE;
        assert_eq!(update.len(), UPDATE_HEADER_SIZE + 4 * ENTRY_SIZE + one_chunk);
        assert_is(&mut received(&update, Some(&held)).unwrap(), &tape);
    }

    #[test]
    fn an_update_against_a_tape_the_receiver_does_not_hold_is_refused() {
        let held = tape_of_rows(&[10, 20]);
        let tape = tape_of_rows(&[10, 30]);
        let update = update_between(&tape, Some(&held));
        assert_eq!(
            received(&update, None).err(),
            Some("Display list update names a chunk its receiver does not hold")
        );
        let other = tape_of_rows(&[20, 30]);
        assert_eq!(
            received(&update, Some(&other)).err(),
            Some("Display list update names a chunk its receiver does not hold")
        );
    }

    #[test]
    fn a_truncated_or_malformed_update_is_refused() {
        let tape = tape_of_rows(&[10, 20]);
        let update = update_between(&tape, None);
        for size in 0..update.len() {
            assert!(
                received(&update[..size], None).is_err(),
                "an update cut to {size} bytes"
            );
        }
        // A chunk that says it holds more runs than it has records for.
        let mut too_many_runs = update.clone();
        too_many_runs[UPDATE_HEADER_SIZE + 20..UPDATE_HEADER_SIZE + 24].copy_from_slice(&1000u32.to_ne_bytes());
        assert!(received(&too_many_runs, None).is_err());
        // A chunk whose records do not validate.
        let mut broken_record = update.clone();
        broken_record[UPDATE_HEADER_SIZE + 2 * ENTRY_SIZE] = 0xff;
        assert!(received(&broken_record, None).is_err());
        // A count of chunks the update has no room for.
        let mut too_many_chunks = update;
        too_many_chunks[..8].copy_from_slice(&u64::MAX.to_ne_bytes());
        assert!(received(&too_many_chunks, None).is_err());
    }

    #[test]
    fn an_empty_tape_is_an_update_of_no_chunks() {
        let tape = DisplayListBuilder::new().finish();
        let update = update_between(&tape, None);
        assert_eq!(update.len(), UPDATE_HEADER_SIZE);
        assert_is(&mut received(&update, None).unwrap(), &tape);
    }
}

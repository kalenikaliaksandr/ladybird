/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! The chunks a tape is cut into.
//!
//! A producer of tapes ends a chunk where what it records next can change without what it recorded before. The tape
//! that replaces another is then mostly the same chunks, a recording reuses whole chunks of the one before it, and a
//! chunk is named by what it holds, so two tapes can be told apart chunk by chunk.

use super::builder::{for_each_command, has_inline_clip_in_its_own_space, inline_clips_of, read_command};
use super::commands::{DisplayListCommandRun, DisplayListCommandType, PaintScrollBar, SpatialNodeIndex};
use super::storage::write_run;
use std::hash::{BuildHasher, Hasher};

/// Names a chunk by what it holds: chunks with one id hold the same records in the same runs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct ChunkId(pub [u64; 2]);

impl ChunkId {
    /// The id of a chunk whose records can still change.
    pub const UNSET: Self = Self([0, 0]);
}

/// A stretch of whole records with the runs that cover them, which name the records by their offsets from where the
/// chunk starts. A chunk is immutable once named, so that every holder of one name draws the same.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Chunk {
    pub id: ChunkId,
    pub records: Vec<u8>,
    pub runs: Vec<DisplayListCommandRun>,
}

impl Chunk {
    /// The chunk that holds `records` in `runs`, named by them.
    pub fn named(records: Vec<u8>, runs: Vec<DisplayListCommandRun>) -> Self {
        Self {
            id: chunk_id(&records, &runs, 0),
            records,
            runs,
        }
    }
}

/// How many of the latest display lists of a context both ends of a connection keep the chunks of.
pub const DISPLAY_LISTS_WHOSE_CHUNKS_ARE_KEPT: usize = 4;

/// The chunks both ends of a connection keep for one context: those its latest display lists name. A sender sends a
/// display list's chunks that are not kept, and both ends take in each list they send or receive in the same order,
/// so both always keep the same chunks without telling each other.
#[derive(Debug, Default)]
pub struct KeptChunks {
    latest_lists: std::collections::VecDeque<Box<[ChunkId]>>,
    lists_naming: std::collections::HashMap<ChunkId, u32>,
}

impl KeptChunks {
    pub fn keeps(&self, id: ChunkId) -> bool {
        self.lists_naming.contains_key(&id)
    }

    /// Takes in the display list the context presents next, which names `chunks`, and answers the chunks that no list
    /// still kept names.
    pub fn take_in(&mut self, chunks: &[ChunkId]) -> Vec<ChunkId> {
        let mut named_once: Vec<ChunkId> = chunks.to_vec();
        named_once.sort_unstable_by_key(|id| id.0);
        named_once.dedup();
        for id in &named_once {
            *self.lists_naming.entry(*id).or_default() += 1;
        }
        self.latest_lists.push_back(named_once.into_boxed_slice());
        let mut dropped = Vec::new();
        while self.latest_lists.len() > DISPLAY_LISTS_WHOSE_CHUNKS_ARE_KEPT {
            let oldest = self.latest_lists.pop_front().expect("more lists than are kept");
            for id in oldest {
                let count = self.lists_naming.get_mut(&id).expect("a kept list names a kept chunk");
                *count -= 1;
                if *count == 0 {
                    self.lists_naming.remove(&id);
                    dropped.push(id);
                }
            }
        }
        dropped
    }
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

/// What a record depends on outside the visual context of its run, which a raster space that does not move with it
/// cannot raster it under.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReachOutsideChunk {
    ScrollbarThumbFollowsScrollOffsetOf(SpatialNodeIndex),
    InlineClipInSpaceOf(SpatialNodeIndex),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TopLevelRecordReachingOutsideItsChunk {
    pub record_offset: u32,
    pub run_index: u32,
    pub reach: ReachOutsideChunk,
}

pub fn top_level_records_reaching_outside_their_chunk(
    bytes: &[u8],
    runs: &[DisplayListCommandRun],
) -> Vec<TopLevelRecordReachingOutsideItsChunk> {
    let mut records = Vec::new();
    for (run_index, run) in runs.iter().enumerate() {
        let run_start = run.offset as usize;
        for_each_command(
            &bytes[run_start..run_start + run.size as usize],
            |header, offset_in_run, payload| {
                let mut note = |reach| {
                    records.push(TopLevelRecordReachingOutsideItsChunk {
                        record_offset: (run_start + offset_in_run) as u32,
                        run_index: run_index as u32,
                        reach,
                    });
                };
                if header.command_type == DisplayListCommandType::PaintScrollBar {
                    note(ReachOutsideChunk::ScrollbarThumbFollowsScrollOffsetOf(
                        read_command::<PaintScrollBar>(payload).scroll_node_index,
                    ));
                }
                if header.inline_clip_count != 0 && has_inline_clip_in_its_own_space(header, payload) {
                    for clip in inline_clips_of(header, payload).filter(|clip| clip.has_own_space) {
                        note(ReachOutsideChunk::InlineClipInSpaceOf(clip.space));
                    }
                }
            },
        );
    }
    records
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::display_list::builder::{DisplayListBuilder, PendingInlineClip, RecordedDisplayList};
    use crate::display_list::commands::{ClipNodeIndex, ContextRef, EffectNodeIndex, FillRect, SpatialNodeIndex};
    use libgfx_rust::{Color, CompositingAndBlendingOperator, FloatRect, IntRect};

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

    #[test]
    fn a_chunk_is_kept_while_one_of_the_latest_lists_names_it() {
        let id = |value: u64| ChunkId([value, value]);
        let mut kept = KeptChunks::default();
        assert!(kept.take_in(&[id(1), id(2), id(1)]).is_empty());
        assert!(kept.keeps(id(1)) && kept.keeps(id(2)));
        for _ in 1..DISPLAY_LISTS_WHOSE_CHUNKS_ARE_KEPT {
            assert!(kept.take_in(&[id(2), id(3)]).is_empty());
        }
        // The list that named the first chunk twice leaves, and the one it named alone goes with it.
        assert_eq!(kept.take_in(&[id(3)]), vec![id(1)]);
        assert!(!kept.keeps(id(1)) && kept.keeps(id(2)) && kept.keeps(id(3)));
        for _ in 1..DISPLAY_LISTS_WHOSE_CHUNKS_ARE_KEPT {
            kept.take_in(&[id(3)]);
        }
        assert!(!kept.keeps(id(2)));
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
    fn records_that_reach_outside_their_chunk_are_found_with_what_they_reach() {
        let mut builder = DisplayListBuilder::new();
        builder.append(&fill_rect(0), &[], context(1));
        let paint_scrollbar_offset = builder.byte_size() as u32;
        builder.append(
            &PaintScrollBar {
                scroll_node_index: SpatialNodeIndex(5),
                gutter_rect: IntRect::new(90, 0, 10, 100),
                thumb_rect: IntRect::new(90, 0, 10, 20),
                track_rect: IntRect::new(90, 0, 10, 100),
                expanded_gutter_rect: IntRect::new(86, 0, 14, 100),
                scroll_size: 0.5,
                thumb_color: Color::default(),
                thumb_outline_color: Color::default(),
                track_color: Color::default(),
                vertical: true,
            },
            &[],
            context(1),
        );
        let own_space_clip_offset = builder.byte_size() as u32;
        let clip = PendingInlineClip::intersecting_float_rect(FloatRect::new(0.0, 0.0, 5.0, 5.0));
        builder.append_with_inline_clips(
            &fill_rect(20),
            &[],
            context(2),
            &[clip.in_space(SpatialNodeIndex(4)), clip.in_space(SpatialNodeIndex(6))],
        );
        builder.append_with_inline_clips(&fill_rect(30), &[], context(2), &[clip]);
        let chunk = builder.take_chunk();

        let record = |record_offset, run_index, reach| TopLevelRecordReachingOutsideItsChunk {
            record_offset,
            run_index,
            reach,
        };
        assert_eq!(
            top_level_records_reaching_outside_their_chunk(&chunk.records, &chunk.runs),
            [
                record(
                    paint_scrollbar_offset,
                    0,
                    ReachOutsideChunk::ScrollbarThumbFollowsScrollOffsetOf(SpatialNodeIndex(5))
                ),
                record(
                    own_space_clip_offset,
                    1,
                    ReachOutsideChunk::InlineClipInSpaceOf(SpatialNodeIndex(4))
                ),
                record(
                    own_space_clip_offset,
                    1,
                    ReachOutsideChunk::InlineClipInSpaceOf(SpatialNodeIndex(6))
                ),
            ]
        );
    }
}

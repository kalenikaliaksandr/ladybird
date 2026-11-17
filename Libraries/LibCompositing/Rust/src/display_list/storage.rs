/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! A display list as the chunks it is cut into, in paint order: chunks another process sent that passed validation, or
//! the chunks of a recording this process made, with the visual context tree it was recorded against.

use super::builder::{HEADER_SIZE, RecordedDisplayList, for_each_command, read_header};
use super::chunks::{
    Chunk, ChunkId, DisplayListChunk, TopLevelRecordReachingOutsideItsChunk, chunk_id, chunks_of,
    top_level_records_reaching_outside_their_chunk,
};
use super::commands::{ContextRef, DisplayListCommandHeader, DisplayListCommandRun, DisplayListCommandType};
use super::effect_clip_plan::EffectClipPlan;
use super::placement::RunGroups;
use super::summary::{DisplayListSummary, IndexedRecord, summarize, summary_of_pieces};
use super::tile_keys::{PlacedChunk, RecordTable, TileKey};
use super::validate::validate_tape;
use crate::visual_context::VisualContextTree;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

// How many placements of its records in raster spaces a chunk keeps: those of the spaces it shows in, and those of a
// moment before, as a scroll that comes back places them again.
const PLACEMENTS_KEPT_PER_CHUNK: usize = 4;

// Record bytes in memory aligned for every command struct, which a Vec<u8> does not promise.
struct AlignedBytes {
    words: Vec<u64>,
    size: usize,
}

impl AlignedBytes {
    fn try_zeroed(size: usize) -> Option<Self> {
        let mut words = Vec::new();
        words.try_reserve_exact(size.div_ceil(8)).ok()?;
        words.resize(size.div_ceil(8), 0);
        Some(Self { words, size })
    }

    fn copied(bytes: &[u8]) -> Self {
        let mut copy = Self::try_zeroed(bytes.len()).expect("a recorded chunk fits in memory");
        copy.as_mut_slice().copy_from_slice(bytes);
        copy
    }

    fn as_slice(&self) -> &[u8] {
        // SAFETY: The words hold at least `size` initialized bytes.
        unsafe { ak::slice_from_raw_parts(self.words.as_ptr().cast(), self.size) }
    }

    fn as_mut_slice(&mut self) -> &mut [u8] {
        // SAFETY: The words hold at least `size` initialized bytes, which this borrows uniquely.
        unsafe { ak::slice_from_raw_parts_mut(self.words.as_mut_ptr().cast(), self.size) }
    }
}

/// Where a record is in a display list: the chunk that holds it, by its place among the chunks of the list, and its
/// offset in that chunk. Records are painted in the order of their positions.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ListPosition {
    pub chunk: u32,
    pub offset: u32,
}

impl ListPosition {
    pub const fn new(chunk: u32, offset: u32) -> Self {
        Self { chunk, offset }
    }
}

/// The records of a display list from `begin` up to `end`, which can lie in a later chunk than `begin`. Every record of
/// the chunks between the two is in the span.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ListSpan {
    pub begin: ListPosition,
    pub end: ListPosition,
}

impl ListSpan {
    pub fn contains(&self, position: ListPosition) -> bool {
        self.begin <= position && position < self.end
    }
}

/// Names a chunk of a list in a way that outlives the list: by its id, and by how many chunks before it in the list
/// have that id, which tells apart the chunks of one id that a list names more than once.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ChunkName {
    pub id: ChunkId,
    pub occurrence: u32,
}

/// A chunk that display lists hold, named by what it holds: its records in memory aligned for every command struct,
/// and its runs, which name the records by their offsets from where the chunk starts. Every list that names a chunk
/// shares it, and what is known of the chunk alone is worked out once, for all of them.
pub struct ListChunk {
    pub id: ChunkId,
    records: AlignedBytes,
    runs: Vec<DisplayListCommandRun>,
    records_reaching_outside: OnceLock<Vec<TopLevelRecordReachingOutsideItsChunk>>,
    summary: OnceLock<DisplayListSummary>,
    record_table: OnceLock<RecordTable>,
    // The latest placements of the chunk's records in raster spaces, the one used last at the end.
    placements: Mutex<Vec<Arc<PlacedChunk>>>,
}

impl ListChunk {
    fn new(records: AlignedBytes, runs: Vec<DisplayListCommandRun>) -> Self {
        Self {
            id: chunk_id(records.as_slice(), &runs, 0),
            records,
            runs,
            records_reaching_outside: OnceLock::new(),
            summary: OnceLock::new(),
            record_table: OnceLock::new(),
            placements: Mutex::new(Vec::new()),
        }
    }

    /// The chunk of a copy of `records`, which `runs` name by their offsets from `chunk_start`.
    pub fn copied(records: &[u8], runs: &[DisplayListCommandRun], chunk_start: u32) -> Self {
        Self::new(
            AlignedBytes::copied(records),
            runs.iter()
                .map(|run| DisplayListCommandRun {
                    offset: run.offset - chunk_start,
                    ..*run
                })
                .collect(),
        )
    }

    pub fn records(&self) -> &[u8] {
        self.records.as_slice()
    }

    pub fn runs(&self) -> &[DisplayListCommandRun] {
        &self.runs
    }

    pub fn records_reaching_outside(&self) -> &[TopLevelRecordReachingOutsideItsChunk] {
        self.records_reaching_outside
            .get_or_init(|| top_level_records_reaching_outside_their_chunk(self.records(), &self.runs))
    }

    pub fn summary(&self) -> &DisplayListSummary {
        self.summary.get_or_init(|| summarize(self.records(), self.runs()))
    }

    pub fn record_table(&self) -> &RecordTable {
        self.record_table
            .get_or_init(|| RecordTable::new(self.records(), self.runs()))
    }

    /// The placement of the chunk's records that `key` names, which `place` makes unless the chunk keeps it, and
    /// whether it was made now.
    pub(crate) fn placement(&self, key: TileKey, place: impl FnOnce() -> PlacedChunk) -> (Arc<PlacedChunk>, bool) {
        let mut placements = self.placements.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(index) = placements.iter().position(|placed| placed.key() == key) {
            let placed = placements.remove(index);
            placements.push(placed.clone());
            return (placed, false);
        }
        let placed = Arc::new(place());
        if placements.len() == PLACEMENTS_KEPT_PER_CHUNK {
            placements.remove(0);
        }
        placements.push(placed.clone());
        (placed, true)
    }

    /// The header and the payload of the record at `offset`.
    pub fn record_at(&self, offset: u32) -> (DisplayListCommandHeader, &[u8]) {
        let records = self.records();
        let offset = offset as usize;
        let header = read_header(&records[offset..]);
        let payload_start = offset + HEADER_SIZE;
        (
            header,
            &records[payload_start..payload_start + header.payload_size as usize],
        )
    }
}

pub struct DisplayListStorage {
    // In paint order.
    chunks: Vec<Arc<ListChunk>>,
    // Per chunk, the index of its first run among the runs of the list, and the count of those runs last.
    first_runs_of_chunks: Vec<u32>,
    // The tree a list this process recorded was recorded against, which it replays against and goes
    // to the compositor with. A list from another process comes without one.
    visual_context_tree: Option<Arc<VisualContextTree>>,
    // The placement of the runs' clips and effects. The list is replayed against trees of one structural epoch only, so
    // one plan serves every replay.
    effect_clip_plan: OnceLock<EffectClipPlan>,
    // Like the plan above, decided by the structure of the trees the list is replayed against, which one epoch fixes.
    run_groups: OnceLock<RunGroups>,
    summary: OnceLock<DisplayListSummary>,
    chunk_names: OnceLock<Arc<[ChunkName]>>,
}

impl DisplayListStorage {
    fn new(chunks: Vec<Arc<ListChunk>>, visual_context_tree: Option<Arc<VisualContextTree>>) -> Arc<Self> {
        let mut first_runs_of_chunks = Vec::with_capacity(chunks.len() + 1);
        let mut run_count = 0u32;
        for chunk in &chunks {
            first_runs_of_chunks.push(run_count);
            run_count += chunk.runs().len() as u32;
        }
        first_runs_of_chunks.push(run_count);
        Arc::new(Self {
            chunks,
            first_runs_of_chunks,
            visual_context_tree,
            effect_clip_plan: OnceLock::new(),
            run_groups: OnceLock::new(),
            summary: OnceLock::new(),
            chunk_names: OnceLock::new(),
        })
    }

    /// The list of `chunks`, which another process sent, in paint order.
    pub fn of_chunks(chunks: Vec<Arc<ListChunk>>) -> Result<Arc<Self>, &'static str> {
        let run_count = chunks.iter().map(|chunk| chunk.runs().len()).sum::<usize>();
        if u32::try_from(chunks.len()).is_err() || u32::try_from(run_count).is_err() {
            return Err("Display list has more chunks or runs than a list can hold");
        }
        Ok(Self::new(chunks, None))
    }

    /// A list this process recorded against `visual_context_tree`.
    pub fn recorded(recorded: &RecordedDisplayList, visual_context_tree: Arc<VisualContextTree>) -> Arc<Self> {
        Self::new(
            chunks_of_recording(&recorded.bytes, &recorded.command_runs, &recorded.chunks),
            Some(visual_context_tree),
        )
    }

    /// An empty list, recorded against a tree with no node but the visual viewport.
    pub fn empty() -> Arc<Self> {
        Self::new(Vec::new(), Some(Arc::new(VisualContextTree::create_identity())))
    }

    /// The list of what `builder` recorded so far, cut into the chunks it ended, as tests build lists.
    #[cfg(test)]
    pub(crate) fn of_builder(builder: &super::builder::DisplayListBuilder) -> Arc<Self> {
        Self::new(
            chunks_of_recording(builder.bytes(), builder.command_runs(), builder.chunks()),
            None,
        )
    }

    pub fn chunks(&self) -> &[Arc<ListChunk>] {
        &self.chunks
    }

    pub fn chunk(&self, index: u32) -> &ListChunk {
        &self.chunks[index as usize]
    }

    /// The name of each chunk of the list, by its place in the list.
    pub fn chunk_names(&self) -> Arc<[ChunkName]> {
        self.chunk_names
            .get_or_init(|| {
                let mut occurrences: HashMap<ChunkId, u32> = HashMap::new();
                self.chunks
                    .iter()
                    .map(|chunk| {
                        let occurrence = occurrences.entry(chunk.id).or_default();
                        let name = ChunkName {
                            id: chunk.id,
                            occurrence: *occurrence,
                        };
                        *occurrence += 1;
                        name
                    })
                    .collect()
            })
            .clone()
    }

    pub fn visual_context_tree(&self) -> Option<&Arc<VisualContextTree>> {
        self.visual_context_tree.as_ref()
    }

    pub fn byte_size(&self) -> usize {
        self.chunks.iter().map(|chunk| chunk.records().len()).sum()
    }

    pub fn run_count(&self) -> usize {
        *self
            .first_runs_of_chunks
            .last()
            .expect("the run counts end with the total") as usize
    }

    /// The runs of the list in paint order.
    pub fn runs(&self) -> impl Iterator<Item = &DisplayListCommandRun> + Clone {
        self.chunks.iter().flat_map(|chunk| chunk.runs().iter())
    }

    /// The index among the runs of the list of the first run of `chunk`.
    pub fn first_run_of_chunk(&self, chunk: u32) -> usize {
        self.first_runs_of_chunks[chunk as usize] as usize
    }

    /// The chunk that holds the run at `run_index` among the runs of the list, and the run's index in the chunk.
    pub fn chunk_of_run(&self, run_index: usize) -> (u32, usize) {
        let chunk = self
            .first_runs_of_chunks
            .partition_point(|&first_run| first_run as usize <= run_index)
            - 1;
        (chunk as u32, run_index - self.first_runs_of_chunks[chunk] as usize)
    }

    pub fn run(&self, run_index: usize) -> &DisplayListCommandRun {
        let (chunk, run_in_chunk) = self.chunk_of_run(run_index);
        &self.chunks[chunk as usize].runs()[run_in_chunk]
    }

    /// Where the first record of the run at `run_index` is.
    pub fn start_of_run(&self, run_index: usize) -> ListPosition {
        let (chunk, run_in_chunk) = self.chunk_of_run(run_index);
        ListPosition::new(chunk, self.chunks[chunk as usize].runs()[run_in_chunk].offset)
    }

    /// Where the records of the run at `run_index` end.
    pub fn end_of_run(&self, run_index: usize) -> ListPosition {
        let (chunk, run_in_chunk) = self.chunk_of_run(run_index);
        let run = &self.chunks[chunk as usize].runs()[run_in_chunk];
        ListPosition::new(chunk, run.offset + run.size)
    }

    /// The span of every record of the list.
    pub fn whole_span(&self) -> ListSpan {
        let end = match self.chunks.last() {
            Some(last) => ListPosition::new(self.chunks.len() as u32 - 1, last.records().len() as u32),
            None => ListPosition::default(),
        };
        ListSpan {
            begin: ListPosition::default(),
            end,
        }
    }

    /// The chunks that `span` reaches into, each with the offsets of the part of it in the span.
    pub fn parts_of_span(&self, span: ListSpan) -> impl Iterator<Item = (u32, u32, u32)> + '_ {
        let chunk_end = (span.end.chunk as usize + 1).min(self.chunks.len()) as u32;
        (span.begin.chunk..chunk_end.max(span.begin.chunk)).filter_map(move |chunk| {
            let begin = if chunk == span.begin.chunk {
                span.begin.offset
            } else {
                0
            };
            let end = if chunk == span.end.chunk {
                span.end.offset
            } else {
                self.chunks[chunk as usize].records().len() as u32
            };
            (begin < end).then_some((chunk, begin, end))
        })
    }

    /// The header and the payload of the record at `position`.
    pub fn record_at(&self, position: ListPosition) -> (DisplayListCommandHeader, &[u8]) {
        self.chunks[position.chunk as usize].record_at(position.offset)
    }

    pub fn summary(&self) -> &DisplayListSummary {
        self.summary.get_or_init(|| {
            summary_of_pieces(
                self.chunks
                    .iter()
                    .enumerate()
                    .map(|(index, chunk)| (chunk.summary(), self.first_runs_of_chunks[index], index as u32)),
            )
        })
    }

    pub fn effect_clip_plan(&self, tree: &VisualContextTree) -> &EffectClipPlan {
        self.effect_clip_plan.get_or_init(|| {
            EffectClipPlan::new(tree, self.runs()).expect("a display list's runs name live nodes of its tree")
        })
    }

    pub fn run_groups(&self, tree: &VisualContextTree) -> &RunGroups {
        self.run_groups.get_or_init(|| RunGroups::new(tree, self.runs()))
    }

    /// Visits the compositor metadata commands of the runs that hold any, with the context of their run and their
    /// payload, in paint order.
    pub fn for_each_compositor_metadata(&self, mut visit: impl FnMut(ContextRef, DisplayListCommandType, &[u8])) {
        for chunk in &self.chunks {
            let records = chunk.records();
            for run in chunk.runs().iter().filter(|run| run.has_compositor_metadata) {
                for_each_command(
                    &records[run.offset as usize..(run.offset + run.size) as usize],
                    |header, _, payload| {
                        if header.command_type.is_compositor_metadata() {
                            visit(run.context, header.command_type, payload);
                        }
                    },
                );
            }
        }
    }

    /// Visits indexed top-level records with their run's context, their header and their payload.
    pub fn for_each_indexed_record(
        &self,
        records: &[IndexedRecord],
        mut visit: impl FnMut(ContextRef, &DisplayListCommandHeader, &[u8]),
    ) {
        for record in records {
            let (header, payload) = self.record_at(record.position());
            visit(self.run(record.run_index as usize).context, &header, payload);
        }
    }
}

/// The chunks of a recorded tape, which `chunks` cuts it into, each a copy. What the tape holds past its last chunk is a
/// chunk of its own.
pub fn chunks_of_recording(
    bytes: &[u8],
    runs: &[DisplayListCommandRun],
    chunks: &[DisplayListChunk],
) -> Vec<Arc<ListChunk>> {
    let mut list_chunks = Vec::with_capacity(chunks.len() + 1);
    let (mut covered_bytes, mut covered_runs) = (0usize, 0usize);
    for (chunk, chunk_bytes, chunk_runs) in chunks_of(bytes, runs, chunks) {
        if !chunk_runs.is_empty() {
            list_chunks.push(Arc::new(ListChunk::copied(
                chunk_bytes,
                chunk_runs,
                covered_bytes as u32,
            )));
        }
        covered_bytes = chunk.byte_end as usize;
        covered_runs = chunk.run_end as usize;
    }
    if covered_runs < runs.len() {
        list_chunks.push(Arc::new(ListChunk::copied(
            &bytes[covered_bytes..],
            &runs[covered_runs..],
            covered_bytes as u32,
        )));
    }
    list_chunks
}

// Writes a run field by field, so its padding is written too.
pub(super) fn write_run(run: &DisplayListCommandRun, out: &mut [u8]) {
    use std::mem::offset_of;
    out.fill(0);
    let mut put = |offset: usize, bytes: &[u8]| out[offset..offset + bytes.len()].copy_from_slice(bytes);
    let context = offset_of!(DisplayListCommandRun, context);
    let ink_bounds = offset_of!(DisplayListCommandRun, ink_bounds);
    put(offset_of!(DisplayListCommandRun, offset), &run.offset.to_ne_bytes());
    put(offset_of!(DisplayListCommandRun, size), &run.size.to_ne_bytes());
    put(
        context + offset_of!(ContextRef, spatial),
        &run.context.spatial.0.to_ne_bytes(),
    );
    put(
        context + offset_of!(ContextRef, clip),
        &run.context.clip.0.to_ne_bytes(),
    );
    put(
        context + offset_of!(ContextRef, effect),
        &run.context.effect.0.to_ne_bytes(),
    );
    put(
        ink_bounds + offset_of!(libgfx_rust::IntRect, x),
        &run.ink_bounds.x.to_ne_bytes(),
    );
    put(
        ink_bounds + offset_of!(libgfx_rust::IntRect, y),
        &run.ink_bounds.y.to_ne_bytes(),
    );
    put(
        ink_bounds + offset_of!(libgfx_rust::IntRect, width),
        &run.ink_bounds.width.to_ne_bytes(),
    );
    put(
        ink_bounds + offset_of!(libgfx_rust::IntRect, height),
        &run.ink_bounds.height.to_ne_bytes(),
    );
    put(
        offset_of!(DisplayListCommandRun, has_compositor_metadata),
        &[u8::from(run.has_compositor_metadata)],
    );
}

/// The size of one chunk packed for transport: its records, and then its runs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PackedChunkSize {
    pub record_bytes: u64,
    pub run_count: u64,
}

impl PackedChunkSize {
    pub fn of(chunk: &Chunk) -> Self {
        Self {
            record_bytes: chunk.records.len() as u64,
            run_count: chunk.runs.len() as u64,
        }
    }

    fn in_bytes(self) -> Option<(usize, usize)> {
        let record_bytes = usize::try_from(self.record_bytes).ok()?;
        let run_bytes = usize::try_from(self.run_count)
            .ok()?
            .checked_mul(std::mem::size_of::<DisplayListCommandRun>())?;
        Some((record_bytes, run_bytes))
    }
}

/// The size of `chunks` packed for transport one after another.
pub fn packed_size_of_chunks(chunks: &[&Chunk]) -> usize {
    chunks
        .iter()
        .map(|chunk| chunk.records.len() + chunk.runs.len() * std::mem::size_of::<DisplayListCommandRun>())
        .sum()
}

/// Writes `chunks` packed for transport one after another to `destination`, which is
/// `packed_size_of_chunks(chunks)` bytes long.
pub fn write_packed_chunks(chunks: &[&Chunk], destination: &mut [u8]) {
    assert_eq!(destination.len(), packed_size_of_chunks(chunks));
    let mut rest = destination;
    for chunk in chunks {
        let (records, after_records) = rest.split_at_mut(chunk.records.len());
        records.copy_from_slice(&chunk.records);
        let (runs, after_runs) =
            after_records.split_at_mut(chunk.runs.len() * std::mem::size_of::<DisplayListCommandRun>());
        let (run_destinations, _) = runs.as_chunks_mut::<{ std::mem::size_of::<DisplayListCommandRun>() }>();
        for (run, out) in chunk.runs.iter().zip(run_destinations) {
            write_run(run, out);
        }
        rest = after_runs;
    }
}

/// Copies the chunks packed one after another in the `packed_size_in_bytes` bytes at `packed`, whose sizes `sizes` lists
/// in order, out of memory that another process can still write to, checks each copy and names it by what it holds.
///
/// # Safety
/// `packed` must address `packed_size_in_bytes` readable bytes, or be null with a size of zero.
pub unsafe fn receive_packed_chunks_from_shared_memory(
    packed: *const u8,
    packed_size_in_bytes: usize,
    sizes: &[PackedChunkSize],
) -> Result<Vec<ListChunk>, &'static str> {
    let mut chunks = Vec::new();
    chunks
        .try_reserve_exact(sizes.len())
        .map_err(|_| "Display list has too many chunks to receive")?;
    let mut offset = 0usize;
    for size in sizes {
        let (record_bytes, run_bytes) = size.in_bytes().ok_or("Display list chunk sizes overflow")?;
        let end = offset
            .checked_add(record_bytes)
            .and_then(|end| end.checked_add(run_bytes))
            .ok_or("Display list chunk sizes overflow")?;
        if end > packed_size_in_bytes {
            return Err("Display list chunks exceed the buffer that holds them");
        }
        // Every run holds at least one record, so more runs than headers cannot describe the chunk.
        if run_bytes / std::mem::size_of::<DisplayListCommandRun>() > record_bytes / super::builder::HEADER_SIZE {
            return Err("Display list chunk has more runs than its records allow");
        }
        let mut records = AlignedBytes::try_zeroed(record_bytes).ok_or("Display list chunk is too large to receive")?;
        let mut packed_runs = Vec::new();
        packed_runs
            .try_reserve_exact(run_bytes)
            .map_err(|_| "Display list chunk is too large to receive")?;
        packed_runs.resize(run_bytes, 0);
        if end > offset {
            // SAFETY: The caller guarantees `packed` addresses `packed_size_in_bytes` bytes, which hold `offset..end`,
            // and both copies have room for their part.
            unsafe {
                std::ptr::copy_nonoverlapping(packed.add(offset), records.as_mut_slice().as_mut_ptr(), record_bytes);
                std::ptr::copy_nonoverlapping(packed.add(offset + record_bytes), packed_runs.as_mut_ptr(), run_bytes);
            }
        }
        let runs = validate_tape(records.as_slice(), &packed_runs)?;
        chunks.push(ListChunk::new(records, runs));
        offset = end;
    }
    if offset != packed_size_in_bytes {
        return Err("Display list chunks do not fill the buffer that holds them");
    }
    Ok(chunks)
}

/// Copies the chunks packed one after another in `packed`, whose sizes `sizes` lists in order, out, checks each copy and
/// names it by what it holds.
pub fn receive_packed_chunks(packed: &[u8], sizes: &[PackedChunkSize]) -> Result<Vec<ListChunk>, &'static str> {
    // SAFETY: The slice addresses its bytes.
    unsafe { receive_packed_chunks_from_shared_memory(packed.as_ptr(), packed.len(), sizes) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::display_list::builder::DisplayListBuilder;
    use crate::display_list::commands::{EffectNodeIndex, FillRect};
    use libgfx_rust::{Color, CompositingAndBlendingOperator, IntRect};

    fn fill_rect(y: i32) -> FillRect {
        FillRect {
            rect: IntRect::new(0, y, 10, 10),
            color: Color::default(),
            compositing_and_blending_operator: CompositingAndBlendingOperator::Normal,
            background_color_animation_effect: EffectNodeIndex::NONE,
        }
    }

    #[test]
    fn chunks_packed_for_transport_come_back_as_the_chunks_of_the_list_they_make() {
        use crate::display_list::chunks::ReachOutsideChunk;
        use crate::display_list::commands::{CanvasId, ClipNodeIndex, DrawCanvas, PaintScrollBar, SpatialNodeIndex};
        use libgfx_rust::ScalingMode;
        let context = |spatial| ContextRef {
            spatial: SpatialNodeIndex(spatial),
            clip: ClipNodeIndex::NONE,
            effect: EffectNodeIndex::NONE,
        };
        let mut builder = DisplayListBuilder::new();
        builder.append(&fill_rect(0), &[], context(1));
        builder.append(
            &DrawCanvas {
                dst_rect: IntRect::new(0, 0, 30, 30),
                canvas_id: CanvasId(4),
                scaling_mode: ScalingMode::NearestNeighbor,
            },
            &[],
            context(1),
        );
        let first = builder.take_chunk();
        builder.append(&fill_rect(10), &[], context(1));
        builder.append(&fill_rect(20), &[], context(2));
        let canvas_offset_in_second_chunk = builder.byte_size() as u32;
        builder.append(
            &DrawCanvas {
                dst_rect: IntRect::new(40, 0, 30, 30),
                canvas_id: CanvasId(3),
                scaling_mode: ScalingMode::NearestNeighbor,
            },
            &[],
            context(2),
        );
        let scrollbar_offset_in_second_chunk = builder.byte_size() as u32;
        builder.append(
            &PaintScrollBar {
                scroll_node_index: SpatialNodeIndex(3),
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
            context(2),
        );
        let second = builder.take_chunk();
        let chunks = [&first, &second];
        let mut packed = vec![0; packed_size_of_chunks(&chunks)];
        write_packed_chunks(&chunks, &mut packed);
        let sizes: Vec<PackedChunkSize> = chunks.iter().map(|chunk| PackedChunkSize::of(chunk)).collect();

        let received = receive_packed_chunks(&packed, &sizes).unwrap();
        for (received, sent) in received.iter().zip(chunks) {
            assert_eq!(received.id, sent.id);
            assert_eq!(received.records(), &sent.records[..]);
            assert_eq!(received.runs(), &sent.runs[..]);
        }
        let received: Vec<Arc<ListChunk>> = received.into_iter().map(Arc::new).collect();
        let list = DisplayListStorage::of_chunks(received).unwrap();
        // The chunks of a recording of the same chunks are the same, and so is what is known of them.
        let tape = RecordedDisplayList::of_chunks(chunks);
        let recorded = DisplayListStorage::recorded(&tape, Arc::new(VisualContextTree::create_identity()));
        assert_eq!(list.summary(), recorded.summary());
        let ids = |list: &DisplayListStorage| list.chunks().iter().map(|chunk| chunk.id).collect::<Vec<_>>();
        assert_eq!(ids(&list), [first.id, second.id]);
        assert_eq!(ids(&recorded), [first.id, second.id]);
        assert_eq!(list.run_count(), 3);
        assert_eq!(list.chunk_of_run(0), (0, 0));
        assert_eq!(list.chunk_of_run(2), (1, 1));
        assert_eq!(list.start_of_run(2), ListPosition::new(1, second.runs[1].offset));
        // What the list knows of its records is where they are in their chunks.
        assert_eq!(
            list.summary().live_content_records[1].record.position(),
            ListPosition::new(1, canvas_offset_in_second_chunk)
        );
        assert_eq!(list.summary().live_content_records[1].record.run_index, 2);
        assert!(list.chunk(0).records_reaching_outside().is_empty());
        assert_eq!(
            list.chunk(1).records_reaching_outside(),
            [TopLevelRecordReachingOutsideItsChunk {
                record_offset: scrollbar_offset_in_second_chunk,
                run_index: 1,
                reach: ReachOutsideChunk::ScrollbarThumbFollowsScrollOffsetOf(SpatialNodeIndex(3)),
            }]
        );

        // Sizes that leave bytes over or claim more than the buffer holds are refused.
        assert!(receive_packed_chunks(&packed, &sizes[..1]).is_err());
        assert!(receive_packed_chunks(&packed[..packed.len() - 8], &sizes).is_err());
    }

    #[test]
    fn a_recorded_list_survives_the_recorder_and_can_be_released_on_another_thread() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<RecordedDisplayList>();
        assert_send_sync::<DisplayListStorage>();
        let mut builder = DisplayListBuilder::new();
        builder.append(&fill_rect(0), &[], ContextRef::default());
        let mut recorded = Arc::new(builder.finish());
        let records = recorded.bytes.clone();
        let storage = DisplayListStorage::recorded(&recorded, Arc::new(VisualContextTree::create_identity()));
        Arc::make_mut(&mut recorded).bytes[0] = 9;
        assert_eq!(storage.chunk(0).records(), &records[..]);
        drop(recorded);
        std::thread::spawn(move || {
            assert_eq!(storage.chunk(0).records(), &records[..]);
            drop(storage);
        })
        .join()
        .unwrap();
    }
}

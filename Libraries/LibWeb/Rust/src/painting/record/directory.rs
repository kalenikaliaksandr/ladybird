/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! The complete output directory for one frame. Offsets are never repaired in an older
//! generation. Empty occurrences keep their order; skipped ones have no reusable payload.

use super::program::PaintProgram;
use crate::layout::used_values::FfiCssPixelPoint;
use crate::painting::display_list::commands::{ContextRef, SpatialNodeIndex};
use std::rc::Rc;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct OutputPoint {
    pub commands: u32,
    pub hits: u32,
}

impl OutputPoint {
    fn relocated(self, old_base: Self, new_base: Self) -> Self {
        Self {
            commands: new_base
                .commands
                .checked_add(self.commands - old_base.commands)
                .expect("display list exceeds u32"),
            hits: new_base
                .hits
                .checked_add(self.hits - old_base.hits)
                .expect("hit-test list exceeds u32"),
        }
    }
}

#[derive(Clone, Copy, Default, PartialEq)]
pub(crate) struct OwnerInputs {
    pub position: FfiCssPixelPoint,
    pub own_context: ContextRef,
    pub descendants_context: ContextRef,
    pub scroll_node: SpatialNodeIndex,
}

#[derive(Clone, Copy)]
struct BlockingRegions {
    operation: u32,
    count: u32,
}

#[derive(Default)]
pub(crate) struct PaintDirectory {
    pub boundaries: Vec<OutputPoint>,
    recorded: Vec<u64>,
    pub refresh: Vec<u32>,
    blocking_regions: Vec<BlockingRegions>,
}

pub(crate) struct FramePaintCache {
    pub program: Rc<PaintProgram>,
    pub directory: Rc<PaintDirectory>,
    pub owner_inputs: Rc<Vec<OwnerInputs>>,
    pub record_gen: u32,
    pub topology_revision: u64,
    pub geometry_revision: u64,
}

const _: () = assert!(std::mem::size_of::<OutputPoint>() == 8);
const _: () = assert!(std::mem::size_of::<OwnerInputs>() == 36);

impl FramePaintCache {
    pub(crate) fn metadata_bytes(&self) -> usize {
        std::mem::size_of::<Self>() + self.program.retained_bytes() + self.directory.retained_bytes()
            + std::mem::size_of::<Vec<OwnerInputs>>() + self.owner_inputs.capacity() * std::mem::size_of::<OwnerInputs>()
            // The three Rc owners contain strong and weak reference counters.
            + 6 * std::mem::size_of::<usize>()
    }

    pub(crate) fn counts(&self) -> (usize, usize, usize) {
        (
            self.program.ops.len(),
            self.program.scopes.len(),
            self.program.owners.len(),
        )
    }
}

impl PaintDirectory {
    fn retained_bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            + self.boundaries.capacity() * std::mem::size_of::<OutputPoint>()
            + self.recorded.capacity() * std::mem::size_of::<u64>()
            + self.refresh.capacity() * std::mem::size_of::<u32>()
            + self.blocking_regions.capacity() * std::mem::size_of::<BlockingRegions>()
    }

    pub fn new(operations: usize) -> Self {
        let mut directory = Self {
            boundaries: Vec::with_capacity(operations + 1),
            recorded: Vec::with_capacity(operations.div_ceil(64)),
            ..Default::default()
        };
        directory.boundaries.push(OutputPoint::default());
        directory
    }

    pub fn recorded(&self, operation: u32) -> bool {
        self.recorded
            .get(operation as usize / 64)
            .is_some_and(|bits| bits & (1 << (operation % 64)) != 0)
    }

    pub fn needs_refresh(&self, begin: u32, end: u32) -> bool {
        let first = self.refresh.partition_point(|&index| index < begin);
        self.refresh.get(first).is_some_and(|&index| index < end)
    }

    pub fn blocking_count(&self, begin: u32, end: u32) -> u32 {
        let first = self.blocking_regions.partition_point(|entry| entry.operation < begin);
        self.blocking_regions[first..]
            .iter()
            .take_while(|entry| entry.operation < end)
            .map(|entry| entry.count)
            .sum()
    }

    pub fn append(&mut self, end: OutputPoint, recorded: bool, refresh: bool, blocking_count: u32) {
        let operation = (self.boundaries.len() - 1) as u32;
        let previous = self.boundaries.last().unwrap();
        assert!(end.commands >= previous.commands && end.hits >= previous.hits);
        self.boundaries.push(end);
        if operation.is_multiple_of(64) {
            self.recorded.push(0);
        }
        if recorded {
            self.recorded[operation as usize / 64] |= 1 << (operation % 64);
        }
        if refresh {
            self.refresh.push(operation);
        }
        if blocking_count != 0 {
            self.blocking_regions.push(BlockingRegions {
                operation,
                count: blocking_count,
            });
        }
    }

    pub fn append_skipped(&mut self, count: u32, at: OutputPoint) {
        if count == 0 {
            return;
        }
        let previous = self.boundaries.last().unwrap();
        assert!(at.commands >= previous.commands && at.hits >= previous.hits);
        self.boundaries.resize(self.boundaries.len() + count as usize, at);
        self.recorded.resize((self.boundaries.len() - 1).div_ceil(64), 0);
    }

    fn append_recorded_range(&mut self, source: &Self, mut from: u32, mut to: u32, mut count: u32) {
        self.recorded.resize(((to + count) as usize).div_ceil(64), 0);
        while count != 0 {
            let source_shift = from % 64;
            let destination_shift = to % 64;
            let length = count.min(64 - destination_shift);
            let mut bits = source.recorded[from as usize / 64] >> source_shift;
            if source_shift + length > 64 {
                bits |= source.recorded[from as usize / 64 + 1] << (64 - source_shift);
            }
            bits &= u64::MAX >> (64 - length);
            self.recorded[to as usize / 64] |= bits << destination_shift;
            from += length;
            to += length;
            count -= length;
        }
    }

    pub fn append_source(&mut self, source: &Self, begin: u32, end: u32) {
        if begin == end {
            return;
        }
        let destination_begin = (self.boundaries.len() - 1) as u32;
        let new_base = *self.boundaries.last().unwrap();
        let old_base = source.boundaries[begin as usize];
        // Source endpoints are monotonic. Checking the last relocated endpoint
        // proves that adding these deltas cannot overflow anywhere in the range.
        let new_end = source.boundaries[end as usize].relocated(old_base, new_base);
        let command_delta = new_base.commands.wrapping_sub(old_base.commands);
        let hit_delta = new_base.hits.wrapping_sub(old_base.hits);
        self.boundaries.reserve((end - begin) as usize);
        self.boundaries.extend(
            source.boundaries[begin as usize + 1..end as usize]
                .iter()
                .map(|point| OutputPoint {
                    commands: point.commands.wrapping_add(command_delta),
                    hits: point.hits.wrapping_add(hit_delta),
                }),
        );
        self.boundaries.push(new_end);
        self.append_recorded_range(source, begin, destination_begin, end - begin);
        let first = source.refresh.partition_point(|&operation| operation < begin);
        for &operation in source.refresh[first..].iter().take_while(|&&operation| operation < end) {
            self.refresh.push(destination_begin + operation - begin);
        }
        let first = source.blocking_regions.partition_point(|entry| entry.operation < begin);
        for entry in source.blocking_regions[first..]
            .iter()
            .take_while(|entry| entry.operation < end)
        {
            self.blocking_regions.push(BlockingRegions {
                operation: destination_begin + entry.operation - begin,
                count: entry.count,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn growth_rebases_the_next_generation_without_changing_the_source() {
        let mut source = PaintDirectory::new(4);
        source.append(OutputPoint { commands: 16, hits: 1 }, true, false, 0);
        source.append(OutputPoint { commands: 16, hits: 1 }, true, false, 0);
        source.append(OutputPoint { commands: 32, hits: 2 }, true, true, 1);
        source.append(OutputPoint { commands: 48, hits: 3 }, true, false, 0);
        let mut next = PaintDirectory::new(4);
        next.append_source(&source, 0, 1);
        next.append(OutputPoint { commands: 48, hits: 4 }, true, false, 0);
        next.append_source(&source, 2, 4);
        assert_eq!(
            next.boundaries,
            [
                OutputPoint::default(),
                OutputPoint { commands: 16, hits: 1 },
                OutputPoint { commands: 48, hits: 4 },
                OutputPoint { commands: 64, hits: 5 },
                OutputPoint { commands: 80, hits: 6 },
            ]
        );
        assert_eq!(source.boundaries[2], OutputPoint { commands: 16, hits: 1 });
        assert!(next.needs_refresh(2, 3));
        assert_eq!(next.blocking_count(0, 4), 1);
    }

    #[test]
    fn skipped_and_recorded_empty_occurrences_remain_distinct_after_copying() {
        let mut source = PaintDirectory::new(70);
        source.append_skipped(65, OutputPoint::default());
        source.append(OutputPoint::default(), true, false, 0);
        let mut next = PaintDirectory::new(70);
        next.append_source(&source, 0, 66);
        assert!(!next.recorded(64));
        assert!(next.recorded(65));
        assert!(next.boundaries.iter().all(|point| *point == OutputPoint::default()));
    }

    #[test]
    #[should_panic(expected = "display list exceeds u32")]
    fn bulk_copy_checks_the_relocated_range_for_overflow() {
        let mut source = PaintDirectory::new(1);
        source.append(
            OutputPoint {
                commands: u32::MAX,
                hits: 0,
            },
            true,
            false,
            0,
        );
        let mut next = PaintDirectory::new(2);
        next.append(OutputPoint { commands: 16, hits: 0 }, true, false, 0);
        next.append_source(&source, 0, 1);
    }

    #[test]
    fn copied_ranges_preserve_availability_at_every_word_alignment() {
        let available = |index: u32| !index.is_multiple_of(3) && !index.is_multiple_of(5);
        let mut source = PaintDirectory::new(256);
        for index in 0..256 {
            source.append(
                OutputPoint {
                    commands: (index + 1) * 16,
                    hits: (index + 1) / 3,
                },
                available(index),
                false,
                0,
            );
        }
        for prefix in 0..64 {
            for begin in 0..64 {
                for count in [0, 1, 63, 64, 65, 129] {
                    let mut next = PaintDirectory::new((prefix + count + 3) as usize);
                    for index in 0..prefix {
                        next.append(
                            OutputPoint {
                                commands: (index + 1) * 8,
                                hits: (index + 1) / 5,
                            },
                            index % 2 != 0,
                            false,
                            0,
                        );
                    }
                    next.append_source(&source, begin, begin + count);
                    for index in 0..prefix {
                        assert_eq!(next.recorded(index), index % 2 != 0);
                    }
                    for index in 0..count {
                        assert_eq!(next.recorded(prefix + index), available(begin + index));
                        assert_eq!(
                            next.boundaries[(prefix + index + 1) as usize],
                            OutputPoint {
                                commands: prefix * 8 + (index + 1) * 16,
                                hits: prefix / 5 + (begin + index + 1) / 3 - begin / 3,
                            },
                        );
                    }
                    let end = *next.boundaries.last().unwrap();
                    next.append_skipped(2, end);
                    next.append(end, true, false, 0);
                    assert!(!next.recorded(prefix + count));
                    assert!(!next.recorded(prefix + count + 1));
                    assert!(next.recorded(prefix + count + 2));
                    assert_eq!(next.boundaries.len(), (prefix + count + 4) as usize);
                }
            }
        }
    }
}

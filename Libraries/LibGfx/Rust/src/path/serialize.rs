/*
 * Copyright 2018 Google LLC
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-3-Clause
 */
// Source: skia/src/core/SkPath_serial.cpp and skia/src/core/SkPathData.cpp (valid_path_data).

//! The byte form of a path. It is the form the compositor reads, so it must stay the same.
//!
//! The bytes are a 32-bit header, the counts of points, conic weights and verbs as 32-bit
//! values, the points, the conic weights and the verbs, then zeros up to a multiple of four
//! bytes. The header has the version in the low 8 bits, the fill type in the next 2 bits and the
//! kind of data in the high 4 bits. All values are in the byte order of the machine.

use super::{PathFillType, PathVerb, PathView, bounds};
use crate::geometry::FloatPoint;

const VERSION_WITH_VERBS_REVERSED: u32 = 4;
const CURRENT_VERSION: u32 = 5;
const FILL_TYPE_SHIFT: u32 = 8;
const KIND_SHIFT: u32 = 28;
const KIND_GENERAL: u32 = 0;

pub(super) fn write(view: PathView<'_>) -> Vec<u8> {
    let header = ((view.fill_type as u32) << FILL_TYPE_SHIFT) | (KIND_GENERAL << KIND_SHIFT) | CURRENT_VERSION;
    let size = 16 + view.points.len() * 8 + view.conic_weights.len() * 4 + view.verbs.len();
    let mut bytes = Vec::with_capacity(size.next_multiple_of(4));
    bytes.extend_from_slice(&header.to_ne_bytes());
    for count in [view.points.len(), view.conic_weights.len(), view.verbs.len()] {
        bytes.extend_from_slice(&(count as i32).to_ne_bytes());
    }
    for point in view.points {
        bytes.extend_from_slice(&point.x.to_ne_bytes());
        bytes.extend_from_slice(&point.y.to_ne_bytes());
    }
    for weight in view.conic_weights {
        bytes.extend_from_slice(&weight.to_ne_bytes());
    }
    bytes.extend(view.verbs.iter().map(|verb| *verb as u8));
    bytes.resize(bytes.len().next_multiple_of(4), 0);
    bytes
}

struct Reader<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> Reader<'a> {
    fn skip(&mut self, count: usize) -> Option<&'a [u8]> {
        let end = self.position.checked_add(count)?;
        let skipped = self.bytes.get(self.position..end)?;
        self.position = end;
        Some(skipped)
    }

    fn read_u32(&mut self) -> Option<u32> {
        let bytes = self.skip(4)?;
        Some(u32::from_ne_bytes(bytes.try_into().ok()?))
    }

    fn skip_count(&mut self, count: u32, element_size: usize) -> Option<&'a [u8]> {
        self.skip((count as usize).checked_mul(element_size)?)
    }

    // The callers give bytes that start at an address that is a multiple of four.
    fn skip_to_alignment(&mut self) -> Option<()> {
        self.skip(self.position.next_multiple_of(4) - self.position).map(|_| ())
    }
}

/// The result of reading a path that is not valid.
enum Failure {
    /// The bytes are not a path. This gives an empty path with the default fill type.
    NotAPath,
    /// The bytes are a path with data that is not valid. This gives an empty path with the fill
    /// type from the header.
    InvalidData(PathFillType),
}

fn floats(bytes: &[u8]) -> impl Iterator<Item = f32> + '_ {
    bytes.as_chunks::<4>().0.iter().map(|chunk| f32::from_ne_bytes(*chunk))
}

fn is_valid_sequence(verbs: &[u8], point_count: usize, conic_weights: &[f32]) -> bool {
    let Some((&first, rest)) = verbs.split_first() else {
        return point_count == 0 && conic_weights.is_empty();
    };
    if first != PathVerb::Move as u8 {
        return false;
    }
    let mut previous = PathVerb::Move;
    let mut expected_point_count = 1usize;
    let mut expected_conic_weight_count = 0usize;
    for &value in rest {
        let Some(verb) = PathVerb::from_u8(value) else {
            return false;
        };
        // A move must not follow a move, and only a move can follow a close.
        if (previous == PathVerb::Move && verb == PathVerb::Move)
            || (previous == PathVerb::Close && verb != PathVerb::Move)
        {
            return false;
        }
        expected_point_count += verb.point_count();
        expected_conic_weight_count += usize::from(verb == PathVerb::Conic);
        previous = verb;
    }
    point_count == expected_point_count
        && conic_weights.len() == expected_conic_weight_count
        && conic_weights.iter().all(|weight| *weight >= 0.0 && weight.is_finite())
}

fn read_path(bytes: &[u8], receive: &mut dyn FnMut(PathView<'_>)) -> Result<(), Failure> {
    let mut reader = Reader { bytes, position: 0 };
    let header = reader.read_u32().ok_or(Failure::NotAPath)?;
    let version = header & 0xFF;
    if version != CURRENT_VERSION && version != VERSION_WITH_VERBS_REVERSED {
        return Err(Failure::NotAPath);
    }
    // A rounded rectangle has its own form, which nothing here writes. Reject it.
    if (header >> KIND_SHIFT) & 0xF != KIND_GENERAL {
        return Err(Failure::NotAPath);
    }
    let fill_type = PathFillType::from_bits(header >> FILL_TYPE_SHIFT);

    let point_count = reader.read_u32().ok_or(Failure::NotAPath)?;
    let conic_weight_count = reader.read_u32().ok_or(Failure::NotAPath)?;
    let verb_count = reader.read_u32().ok_or(Failure::NotAPath)?;
    let point_bytes = reader.skip_count(point_count, 8).ok_or(Failure::NotAPath)?;
    let conic_weight_bytes = reader.skip_count(conic_weight_count, 4).ok_or(Failure::NotAPath)?;
    let verb_bytes = reader.skip_count(verb_count, 1).ok_or(Failure::NotAPath)?;
    reader.skip_to_alignment().ok_or(Failure::NotAPath)?;

    if verb_count == 0 {
        if point_count == 0 && conic_weight_count == 0 {
            receive(PathView::empty(fill_type));
            return Ok(());
        }
        return Err(Failure::NotAPath);
    }

    let mut verb_values = verb_bytes.to_vec();
    if version == VERSION_WITH_VERBS_REVERSED {
        verb_values.reverse();
    }
    let points: Vec<FloatPoint> = floats(point_bytes)
        .collect::<Vec<_>>()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|&[x, y]| FloatPoint { x, y })
        .collect();
    let conic_weights: Vec<f32> = floats(conic_weight_bytes).collect();
    if !is_valid_sequence(&verb_values, points.len(), &conic_weights) {
        return Err(Failure::InvalidData(fill_type));
    }
    let verbs: Vec<PathVerb> = verb_values
        .iter()
        .map(|value| PathVerb::from_u8(*value).expect("The verbs are valid"))
        .collect();
    if bounds::trimmed_bounds(&points, &verbs).is_none() {
        return Err(Failure::InvalidData(fill_type));
    }
    receive(PathView {
        verbs: &verbs,
        points: &points,
        conic_weights: &conic_weights,
        fill_type,
    });
    Ok(())
}

/// Reads a path, and gives it to `receive`. Bytes that are not a valid path give an empty path.
pub(super) fn read(bytes: &[u8], mut receive: impl FnMut(PathView<'_>)) {
    match read_path(bytes, &mut receive) {
        Ok(()) => {}
        Err(Failure::NotAPath) => receive(PathView::empty(PathFillType::Winding)),
        Err(Failure::InvalidData(fill_type)) => receive(PathView::empty(fill_type)),
    }
}

/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

use super::commands::*;
use crate::painting::display_list::ffi_bytes::FfiBytes;

pub const COMMAND_ALIGNMENT: usize = 16;
pub const HEADER_SIZE: usize = std::mem::size_of::<DisplayListCommandHeader>();

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CommandRange {
    pub offset: u32,
    pub size: u32,
}

impl CommandRange {
    pub const fn is_empty(self) -> bool {
        self.size == 0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AppendContext {
    pub context_index: VisualContextIndex,
    pub context_geometry_only: bool,
    pub has_empty_effective_clip: bool,
}

#[derive(Default)]
pub struct DisplayListBuilder {
    bytes: Vec<u8>,
    save_nesting_level: i32,
}

impl DisplayListBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            bytes: Vec::with_capacity(capacity),
            save_nesting_level: 0,
        }
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }

    pub fn byte_size(&self) -> usize {
        self.bytes.len()
    }

    pub fn save_nesting_level(&self) -> i32 {
        self.save_nesting_level
    }

    pub fn append<C: DisplayListCommand>(&mut self, command: &C, inline_data: &[u8], context: AppendContext) -> bool {
        if !context.context_geometry_only && context.has_empty_effective_clip {
            return false;
        }
        debug_assert_eq!(self.bytes.len() % COMMAND_ALIGNMENT, 0);
        let payload_size = std::mem::size_of::<C>() + inline_data.len();
        let record_size = HEADER_SIZE + payload_size;
        let trailing_padding = record_size.next_multiple_of(COMMAND_ALIGNMENT) - record_size;
        let header = DisplayListCommandHeader {
            command_type: C::COMMAND_TYPE,
            payload_size: u32::try_from(payload_size + trailing_padding).expect("display list payload exceeds u32"),
            context_index: context.context_index,
            context_geometry_only: context.context_geometry_only,
            has_bounding_rect: command.bounding_rect().is_some(),
            is_clip: command.is_clip(),
            bounding_rect: command.bounding_rect().unwrap_or_default(),
        };
        let start = self.bytes.len();
        self.bytes
            .resize(start + HEADER_SIZE + payload_size + trailing_padding, 0);
        header.write_ffi_bytes(&mut self.bytes[start..start + HEADER_SIZE]);
        let payload_start = start + HEADER_SIZE;
        command.write_ffi_bytes(&mut self.bytes[payload_start..payload_start + std::mem::size_of::<C>()]);
        let inline_start = payload_start + std::mem::size_of::<C>();
        self.bytes[inline_start..inline_start + inline_data.len()].copy_from_slice(inline_data);
        self.save_nesting_level += C::COMMAND_TYPE.nesting_level_change();
        true
    }

    pub fn append_command_range(
        &mut self,
        source: &[u8],
        range: CommandRange,
        recorded_context_index: VisualContextIndex,
        current_context_index: VisualContextIndex,
    ) -> u32 {
        // Captures are save/restore balanced (verified at capture end), so splicing never shifts the save nesting level.
        debug_assert_eq!(self.bytes.len() % COMMAND_ALIGNMENT, 0);
        debug_assert_eq!(range.size as usize % COMMAND_ALIGNMENT, 0);
        let destination_offset = self.bytes.len();
        if range.is_empty() {
            return u32::try_from(destination_offset).expect("display list exceeds u32");
        }
        let source_range = &source[range.offset as usize..(range.offset + range.size) as usize];
        self.bytes.extend_from_slice(source_range);
        if recorded_context_index != current_context_index {
            set_command_sequence_visual_context(&mut self.bytes[destination_offset..], current_context_index);
        }
        u32::try_from(destination_offset).expect("display list exceeds u32")
    }
}

pub fn read_header(bytes: &[u8]) -> DisplayListCommandHeader {
    assert!(bytes.len() >= HEADER_SIZE);
    let cursor = HeaderReader { bytes };
    DisplayListCommandHeader {
        command_type: DisplayListCommandType::from_u8(
            cursor.u8_at(std::mem::offset_of!(DisplayListCommandHeader, command_type)),
        )
        .expect("invalid display list command type"),
        payload_size: cursor.u32_at(std::mem::offset_of!(DisplayListCommandHeader, payload_size)),
        context_index: VisualContextIndex(
            cursor.usize_at(std::mem::offset_of!(DisplayListCommandHeader, context_index)),
        ),
        context_geometry_only: cursor.bool_at(std::mem::offset_of!(DisplayListCommandHeader, context_geometry_only)),
        has_bounding_rect: cursor.bool_at(std::mem::offset_of!(DisplayListCommandHeader, has_bounding_rect)),
        is_clip: cursor.bool_at(std::mem::offset_of!(DisplayListCommandHeader, is_clip)),
        bounding_rect: {
            let base = std::mem::offset_of!(DisplayListCommandHeader, bounding_rect);
            libgfx_rust::IntRect {
                x: cursor.i32_at(base),
                y: cursor.i32_at(base + 4),
                width: cursor.i32_at(base + 8),
                height: cursor.i32_at(base + 12),
            }
        },
    }
}

struct HeaderReader<'a> {
    bytes: &'a [u8],
}

impl HeaderReader<'_> {
    fn u8_at(&self, offset: usize) -> u8 {
        self.bytes[offset]
    }
    fn bool_at(&self, offset: usize) -> bool {
        self.bytes[offset] != 0
    }
    fn u32_at(&self, offset: usize) -> u32 {
        u32::from_ne_bytes(self.bytes[offset..offset + 4].try_into().unwrap())
    }
    fn i32_at(&self, offset: usize) -> i32 {
        i32::from_ne_bytes(self.bytes[offset..offset + 4].try_into().unwrap())
    }
    fn usize_at(&self, offset: usize) -> usize {
        usize::from_ne_bytes(
            self.bytes[offset..offset + std::mem::size_of::<usize>()]
                .try_into()
                .unwrap(),
        )
    }
}

pub fn for_each_command(bytes: &[u8], mut callback: impl FnMut(DisplayListCommandHeader, &[u8])) {
    let mut offset = 0;
    while offset < bytes.len() {
        let header = read_header(&bytes[offset..]);
        offset += HEADER_SIZE;
        let payload = &bytes[offset..offset + header.payload_size as usize];
        offset += header.payload_size as usize;
        callback(header, payload);
    }
    assert_eq!(offset, bytes.len());
}

fn set_command_sequence_visual_context(bytes: &mut [u8], context_index: VisualContextIndex) {
    let mut offset = 0;
    while offset < bytes.len() {
        let header = read_header(&bytes[offset..]);
        let field_offset = offset + std::mem::offset_of!(DisplayListCommandHeader, context_index);
        context_index
            .write_ffi_bytes(&mut bytes[field_offset..field_offset + std::mem::size_of::<VisualContextIndex>()]);
        offset += HEADER_SIZE + header.payload_size as usize;
    }
    assert_eq!(offset, bytes.len());
}

#[cfg(test)]
mod tests {
    use super::*;
    use libgfx_rust::*;

    #[test]
    fn records_are_sixteen_byte_aligned_and_padding_is_zeroed() {
        let mut builder = DisplayListBuilder::new();
        let context = AppendContext {
            context_index: VisualContextIndex(3),
            context_geometry_only: false,
            has_empty_effective_clip: false,
        };
        assert!(builder.append(
            &FillRect {
                rect: IntRect::new(1, 2, 3, 4),
                color: Color(0xff00ff00)
            },
            &[],
            context
        ));
        assert_eq!(builder.byte_size() % COMMAND_ALIGNMENT, 0);
        let mut seen = 0;
        for_each_command(builder.bytes(), |header, payload| {
            assert_eq!(header.command_type, DisplayListCommandType::FillRect);
            assert_eq!(header.context_index, VisualContextIndex(3));
            assert!(header.has_bounding_rect);
            assert_eq!(header.bounding_rect, IntRect::new(1, 2, 3, 4));
            assert_eq!(payload.len() as u32, header.payload_size);
            assert!(payload[std::mem::size_of::<FillRect>()..].iter().all(|byte| *byte == 0));
            seen += 1;
        });
        assert_eq!(seen, 1);
    }

    #[test]
    fn empty_effective_clip_drops_non_geometry_commands() {
        let mut builder = DisplayListBuilder::new();
        let clipped = AppendContext {
            context_index: VisualContextIndex(1),
            context_geometry_only: false,
            has_empty_effective_clip: true,
        };
        assert!(!builder.append(
            &FillRect {
                rect: IntRect::default(),
                color: Color(0)
            },
            &[],
            clipped
        ));
        let geometry_only = AppendContext {
            context_geometry_only: true,
            ..clipped
        };
        assert!(builder.append(
            &FillRect {
                rect: IntRect::default(),
                color: Color(0)
            },
            &[],
            geometry_only
        ));
    }

    #[test]
    fn spliced_ranges_are_rewritten_only_when_the_context_moved() {
        let mut source = DisplayListBuilder::new();
        let context = AppendContext {
            context_index: VisualContextIndex(2),
            context_geometry_only: false,
            has_empty_effective_clip: false,
        };
        source.append(&Save::default(), &[], context);
        source.append(&Restore::default(), &[], context);
        let range = CommandRange {
            offset: 0,
            size: source.byte_size() as u32,
        };

        let mut same = DisplayListBuilder::new();
        same.append_command_range(source.bytes(), range, VisualContextIndex(2), VisualContextIndex(2));
        assert_eq!(same.bytes(), source.bytes());

        let mut moved = DisplayListBuilder::new();
        moved.append_command_range(source.bytes(), range, VisualContextIndex(2), VisualContextIndex(7));
        for_each_command(moved.bytes(), |header, _| {
            assert_eq!(header.context_index, VisualContextIndex(7));
        });
    }

    #[test]
    fn nesting_level_follows_save_and_restore() {
        let mut builder = DisplayListBuilder::new();
        let context = AppendContext {
            context_index: VisualContextIndex(0),
            context_geometry_only: false,
            has_empty_effective_clip: false,
        };
        builder.append(&SaveLayer::default(), &[], context);
        assert_eq!(builder.save_nesting_level(), 1);
        builder.append(&Restore::default(), &[], context);
        assert_eq!(builder.save_nesting_level(), 0);
    }
}

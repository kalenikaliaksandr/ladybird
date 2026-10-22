/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! The tape of a 2D canvas: the records that a recorder writes for the commands of the canvas, and the entry points
//! through which the compositor checks and reads a tape that it received.

use super::builder::{append_record, for_each_command};
use super::commands::*;
use super::ffi_bytes::FfiBytes;
use super::validate::{ValidationError, validate_canvas_2d_tape};
use libgfx_rust::{
    AffineTransform, CapStyle, Color, CompositingAndBlendingOperator, FloatPoint, FloatRect, IntRect, JoinStyle,
    ScalingMode, WindingRule,
};
use libipc_rust::wire::{Decode, DecodeError, Decoder, Encode, EncodeError, Encoder};
use std::ffi::c_void;

/// The stops of a gradient, as parallel arrays of colors and positions.
#[derive(Clone, Copy, Debug, Default)]
pub struct ColorStops<'a> {
    pub colors: &'a [Color],
    pub positions: &'a [f32],
}

/// The fill or stroke style of a draw. A pattern names its image by the id it travels with beside the tape.
#[derive(Clone, Copy, Debug)]
pub enum Paint<'a> {
    Color(Color),
    LinearGradient {
        start_point: FloatPoint,
        end_point: FloatPoint,
        color_stops: ColorStops<'a>,
    },
    RadialGradient {
        start_center: FloatPoint,
        start_radius: f32,
        end_center: FloatPoint,
        end_radius: f32,
        color_stops: ColorStops<'a>,
    },
    ConicGradient {
        center: FloatPoint,
        start_angle: f32,
        color_stops: ColorStops<'a>,
    },
    Pattern {
        image: Option<ImageFrameResourceId>,
        repetition: Canvas2DPatternRepetition,
        transform: Option<AffineTransform>,
    },
}

/// How a draw composites: the serialized filter graph of the context if it has one, the global alpha, and the
/// compositing and blending operator.
#[derive(Clone, Copy, Debug)]
pub struct Compositing<'a> {
    pub filter: Option<&'a [u8]>,
    pub global_alpha: f32,
    pub compositing_and_blending_operator: CompositingAndBlendingOperator,
}

impl Default for Compositing<'_> {
    fn default() -> Self {
        Self {
            filter: None,
            global_alpha: 1.0,
            compositing_and_blending_operator: CompositingAndBlendingOperator::SourceOver,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Stroke<'a> {
    pub thickness: f32,
    pub cap_style: CapStyle,
    pub join_style: JoinStyle,
    pub miter_limit: f32,
    pub dash_array: &'a [f32],
    pub dash_offset: f32,
}

// The data a record carries after its command struct. Its spans are offsets into the payload, which starts with the
// struct, at a multiple of the command alignment.
struct InlineData {
    struct_size: usize,
    bytes: Vec<u8>,
}

impl InlineData {
    fn new<C: DisplayListCommand>() -> Self {
        Self {
            struct_size: std::mem::size_of::<C>(),
            bytes: Vec::new(),
        }
    }

    fn append(&mut self, data: &[u8], alignment: usize) -> DisplayListDataSpan {
        if data.is_empty() {
            return DisplayListDataSpan::default();
        }
        let offset = (self.struct_size + self.bytes.len()).next_multiple_of(alignment);
        self.bytes.resize(offset - self.struct_size, 0);
        self.bytes.extend_from_slice(data);
        DisplayListDataSpan {
            offset: u32::try_from(offset).expect("canvas 2D record exceeds u32"),
            size: u32::try_from(data.len()).expect("canvas 2D record exceeds u32"),
        }
    }

    fn append_objects<T: FfiBytes>(&mut self, objects: &[T]) -> DisplayListDataSpan {
        let size = std::mem::size_of::<T>();
        let mut bytes = vec![0u8; std::mem::size_of_val(objects)];
        for (object, out) in objects.iter().zip(bytes.chunks_exact_mut(size)) {
            object.write_ffi_bytes(out);
        }
        self.append(&bytes, std::mem::align_of::<T>())
    }

    fn append_paint(&mut self, paint: &Paint) -> Canvas2DPaint {
        let mut record = Canvas2DPaint::default();
        let append_color_stops = |inline_data: &mut Self, color_stops: &ColorStops| {
            debug_assert_eq!(color_stops.colors.len(), color_stops.positions.len());
            DisplayListGradientColorStops {
                colors: inline_data.append_objects(color_stops.colors),
                positions: inline_data.append_objects(color_stops.positions),
                repeating: false,
            }
        };
        match *paint {
            Paint::Color(color) => {
                record.kind = Canvas2DPaintKind::Color;
                record.color = color;
            }
            Paint::LinearGradient {
                start_point,
                end_point,
                color_stops,
            } => {
                record.kind = Canvas2DPaintKind::LinearGradient;
                record.start_point = start_point;
                record.end_point = end_point;
                record.color_stops = append_color_stops(self, &color_stops);
            }
            Paint::RadialGradient {
                start_center,
                start_radius,
                end_center,
                end_radius,
                color_stops,
            } => {
                record.kind = Canvas2DPaintKind::RadialGradient;
                record.start_point = start_center;
                record.start_radius = start_radius;
                record.end_point = end_center;
                record.end_radius = end_radius;
                record.color_stops = append_color_stops(self, &color_stops);
            }
            Paint::ConicGradient {
                center,
                start_angle,
                color_stops,
            } => {
                record.kind = Canvas2DPaintKind::ConicGradient;
                record.start_point = center;
                record.start_angle = start_angle;
                record.color_stops = append_color_stops(self, &color_stops);
            }
            Paint::Pattern {
                image,
                repetition,
                transform,
            } => {
                record.kind = Canvas2DPaintKind::Pattern;
                record.pattern_has_image = image.is_some();
                record.pattern_image_id = image.unwrap_or_default();
                record.pattern_repetition = repetition;
                record.pattern_transform = transform.into();
            }
        }
        record
    }

    fn append_compositing(&mut self, compositing: &Compositing) -> Canvas2DCompositing {
        Canvas2DCompositing {
            filter: compositing
                .filter
                .map_or_else(DisplayListDataSpan::default, |filter| self.append(filter, 1)),
            has_filter: compositing.filter.is_some(),
            global_alpha: compositing.global_alpha,
            compositing_and_blending_operator: compositing.compositing_and_blending_operator,
        }
    }

    fn append_path(&mut self, path: &[u8]) -> DisplayListDataSpan {
        self.append(path, std::mem::align_of::<u32>())
    }
}

/// Writes the records of the commands of a 2D canvas to a tape, which a recorder hands on and clears.
#[derive(Default)]
pub struct TapeWriter {
    bytes: Vec<u8>,
}

impl TapeWriter {
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn clear(&mut self) {
        self.bytes.clear();
    }

    fn append<C: DisplayListCommand>(&mut self, command: &C, inline_data: &InlineData) {
        append_record(&mut self.bytes, command, &inline_data.bytes);
    }

    fn append_plain<C: DisplayListCommand>(&mut self, command: &C) {
        append_record(&mut self.bytes, command, &[]);
    }

    /// `path` is a serialized Gfx::Path.
    pub fn fill_path(
        &mut self,
        path: &[u8],
        paint: &Paint,
        winding_rule: WindingRule,
        blur_radius: f32,
        compositing: &Compositing,
    ) {
        let mut inline_data = InlineData::new::<Canvas2DFillPath>();
        let command = Canvas2DFillPath {
            path_data: inline_data.append_path(path),
            paint: inline_data.append_paint(paint),
            winding_rule,
            blur_radius,
            compositing: inline_data.append_compositing(compositing),
        };
        self.append(&command, &inline_data);
    }

    /// `path` is a serialized Gfx::Path.
    pub fn stroke_path(
        &mut self,
        path: &[u8],
        paint: &Paint,
        stroke: &Stroke,
        blur_radius: f32,
        compositing: &Compositing,
    ) {
        let mut inline_data = InlineData::new::<Canvas2DStrokePath>();
        let command = Canvas2DStrokePath {
            path_data: inline_data.append_path(path),
            paint: inline_data.append_paint(paint),
            thickness: stroke.thickness,
            cap_style: stroke.cap_style,
            join_style: stroke.join_style,
            miter_limit: stroke.miter_limit,
            dash_array: inline_data.append_objects(stroke.dash_array),
            dash_offset: stroke.dash_offset,
            blur_radius,
            compositing: inline_data.append_compositing(compositing),
        };
        self.append(&command, &inline_data);
    }

    pub fn draw_glyph_run(
        &mut self,
        font_id: FontResourceId,
        glyphs: &[DisplayListGlyph],
        translation: FloatPoint,
        paint: &Paint,
        compositing: &Compositing,
    ) {
        let mut inline_data = InlineData::new::<Canvas2DDrawGlyphRun>();
        let command = Canvas2DDrawGlyphRun {
            font_id,
            glyphs: inline_data.append_objects(glyphs),
            translation,
            paint: inline_data.append_paint(paint),
            compositing: inline_data.append_compositing(compositing),
        };
        self.append(&command, &inline_data);
    }

    pub fn clear_rect(&mut self, rect: FloatRect, color: Color) {
        self.append_plain(&Canvas2DClearRect { rect, color });
    }

    pub fn draw_image_frame(
        &mut self,
        frame_id: ImageFrameResourceId,
        dst_rect: FloatRect,
        src_rect: IntRect,
        scaling_mode: ScalingMode,
        compositing: &Compositing,
    ) {
        let mut inline_data = InlineData::new::<Canvas2DDrawImageFrame>();
        let command = Canvas2DDrawImageFrame {
            frame_id,
            dst_rect,
            src_rect,
            scaling_mode,
            compositing: inline_data.append_compositing(compositing),
        };
        self.append(&command, &inline_data);
    }

    pub fn draw_canvas(
        &mut self,
        source_canvas_id: CanvasId,
        dst_rect: FloatRect,
        src_rect: IntRect,
        scaling_mode: ScalingMode,
        compositing: &Compositing,
    ) {
        let mut inline_data = InlineData::new::<Canvas2DDrawCanvas>();
        let command = Canvas2DDrawCanvas {
            source_canvas_id,
            dst_rect,
            src_rect,
            scaling_mode,
            compositing: inline_data.append_compositing(compositing),
        };
        self.append(&command, &inline_data);
    }

    pub fn draw_video_frame(
        &mut self,
        video_sink_handle: u64,
        generation: u64,
        dst_rect: FloatRect,
        src_rect: IntRect,
        scaling_mode: ScalingMode,
        compositing: &Compositing,
    ) {
        let mut inline_data = InlineData::new::<Canvas2DDrawVideoFrame>();
        let command = Canvas2DDrawVideoFrame {
            video_sink_handle,
            generation,
            dst_rect,
            src_rect,
            scaling_mode,
            compositing: inline_data.append_compositing(compositing),
        };
        self.append(&command, &inline_data);
    }

    pub fn set_transform(&mut self, transform: AffineTransform) {
        self.append_plain(&Canvas2DSetTransform { transform });
    }

    pub fn save(&mut self) {
        self.append_plain(&Canvas2DSave::default());
    }

    pub fn restore(&mut self) {
        self.append_plain(&Canvas2DRestore::default());
    }

    /// `path` is a serialized Gfx::Path.
    pub fn clip_path(&mut self, path: &[u8], winding_rule: WindingRule) {
        let mut inline_data = InlineData::new::<Canvas2DClipPath>();
        let command = Canvas2DClipPath {
            path_data: inline_data.append_path(path),
            winding_rule,
        };
        self.append(&command, &inline_data);
    }

    pub fn reset(&mut self) {
        self.append_plain(&Canvas2DReset::default());
    }

    pub fn clear_canvas(&mut self, color: Color) {
        self.append_plain(&Canvas2DClearCanvas { color });
    }
}

/// Calls `visit` with the type and the payload of each record of a tape that passed `canvas_2d_segments_validate`, in
/// order.
///
/// # Safety
/// `visit` runs synchronously and reads the payload only while it runs.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn canvas_2d_tape_for_each_command(
    tape: ak::Span<'_, u8>,
    context: *mut c_void,
    visit: unsafe extern "C" fn(*mut c_void, DisplayListCommandType, ak::Span<'_, u8>),
) {
    for_each_command(tape.as_slice(), |header, _, payload| {
        debug_assert!(header.command_type.is_canvas_2d());
        // SAFETY: The caller reads the payload synchronously.
        unsafe { visit(context, header.command_type, ak::Span::new(payload)) };
    });
}

/// The records that one canvas recorded in a row, and whether the canvas presented after them.
#[derive(Debug, PartialEq, Eq)]
pub struct Canvas2DSegment {
    pub canvas_id: CanvasId,
    pub tape: Vec<u8>,
    pub present: bool,
}

/// The canvas 2D commands that all canvases of a WebContent connection recorded, in order, as segments. Replaying the
/// segments strictly in order guarantees that a command that draws another canvas (DrawCanvas) sees all of that
/// canvas's earlier commands, without a flush when it is recorded.
#[derive(Debug, Default)]
pub struct Canvas2DSegments {
    segments: Vec<Canvas2DSegment>,
    command_count: usize,
}

impl Canvas2DSegments {
    pub fn append_command(&mut self, canvas_id: CanvasId, record: &[u8]) {
        // A presented segment must keep its position in the stream, so recording after a present marker starts a new
        // segment for the same canvas.
        let continues_last = self
            .segments
            .last()
            .is_some_and(|last| last.canvas_id == canvas_id && !last.present);
        if !continues_last {
            self.segments.push(Canvas2DSegment {
                canvas_id,
                tape: Vec::new(),
                present: false,
            });
        }
        if let Some(last) = self.segments.last_mut() {
            last.tape.extend_from_slice(record);
        }
        self.command_count += 1;
    }

    pub fn record_present(&mut self, canvas_id: CanvasId) {
        if let Some(last) = self.segments.last_mut()
            && last.canvas_id == canvas_id
        {
            last.present = true;
            return;
        }
        self.segments.push(Canvas2DSegment {
            canvas_id,
            tape: Vec::new(),
            present: true,
        });
    }

    pub fn is_empty(&self) -> bool {
        self.segments.is_empty()
    }

    /// How many commands were appended, which a decoded stream does not know.
    pub fn command_count(&self) -> usize {
        self.command_count
    }

    pub fn segments(&self) -> &[Canvas2DSegment] {
        &self.segments
    }

    /// Checks the tapes of segments that came from another process.
    pub fn validate(&self) -> Result<(), ValidationError> {
        self.segments
            .iter()
            .try_for_each(|segment| validate_canvas_2d_tape(&segment.tape))
    }
}

impl Encode for Canvas2DSegment {
    fn encode(&self, encoder: &mut Encoder) -> Result<(), EncodeError> {
        encoder.encode(&self.canvas_id.0)?;
        encoder.encode(self.tape.as_slice())?;
        encoder.encode(&self.present)
    }
}

impl Decode for Canvas2DSegment {
    const MIN_ENCODED_SIZE: usize = 8 + 4 + 1;

    fn decode(decoder: &mut Decoder<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            canvas_id: CanvasId(decoder.decode()?),
            tape: decoder.decode()?,
            present: decoder.decode()?,
        })
    }
}

/// Segments go as a vector of them, each its canvas, its tape as a byte buffer, and whether it presents. The tapes are
/// not checked when they are decoded: the receiver checks them with `validate()`.
impl Encode for Canvas2DSegments {
    fn encode(&self, encoder: &mut Encoder) -> Result<(), EncodeError> {
        encoder.encode(self.segments.as_slice())
    }
}

impl Decode for Canvas2DSegments {
    const MIN_ENCODED_SIZE: usize = 4;

    fn decode(decoder: &mut Decoder<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            segments: decoder.decode()?,
            command_count: 0,
        })
    }
}

/// # Safety
///
/// `segments` must come from `canvas_2d_segments_create`, `canvas_2d_segments_take` or
/// `canvas_2d_segments_ipc_decode`, and not be destroyed yet.
unsafe fn segments_from_handle<'a>(segments: *const c_void) -> &'a Canvas2DSegments {
    // SAFETY: Guaranteed by the caller.
    unsafe { &*segments.cast::<Canvas2DSegments>() }
}

/// # Safety
///
/// As for `segments_from_handle`, and nothing else may use `segments` meanwhile.
unsafe fn segments_from_handle_mut<'a>(segments: *mut c_void) -> &'a mut Canvas2DSegments {
    // SAFETY: Guaranteed by the caller.
    unsafe { &mut *segments.cast::<Canvas2DSegments>() }
}

fn segments_into_handle(segments: Canvas2DSegments) -> *mut c_void {
    Box::into_raw(Box::new(segments)).cast()
}

/// Makes an empty stream of segments, which the caller destroys with `canvas_2d_segments_destroy`.
#[unsafe(no_mangle)]
pub extern "C" fn canvas_2d_segments_create() -> *mut c_void {
    segments_into_handle(Canvas2DSegments::default())
}

/// # Safety
///
/// `segments` must be a live handle, which this destroys.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn canvas_2d_segments_destroy(segments: *mut c_void) {
    // SAFETY: Guaranteed by the caller.
    drop(unsafe { Box::from_raw(segments.cast::<Canvas2DSegments>()) });
}

/// # Safety
///
/// `segments` must be a live handle that nothing else uses meanwhile.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn canvas_2d_segments_append_command(
    segments: *mut c_void,
    canvas_id: u64,
    record: ak::Span<'_, u8>,
) {
    // SAFETY: Guaranteed by the caller.
    unsafe { segments_from_handle_mut(segments) }.append_command(CanvasId(canvas_id), record.as_slice());
}

/// # Safety
///
/// `segments` must be a live handle that nothing else uses meanwhile.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn canvas_2d_segments_record_present(segments: *mut c_void, canvas_id: u64) {
    // SAFETY: Guaranteed by the caller.
    unsafe { segments_from_handle_mut(segments) }.record_present(CanvasId(canvas_id));
}

/// # Safety
///
/// `segments` must be a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn canvas_2d_segments_is_empty(segments: *const c_void) -> bool {
    // SAFETY: Guaranteed by the caller.
    unsafe { segments_from_handle(segments) }.is_empty()
}

/// # Safety
///
/// `segments` must be a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn canvas_2d_segments_command_count(segments: *const c_void) -> usize {
    // SAFETY: Guaranteed by the caller.
    unsafe { segments_from_handle(segments) }.command_count()
}

/// Moves the segments of `segments` into a new handle, which the caller destroys, and leaves `segments` empty.
///
/// # Safety
///
/// `segments` must be a live handle that nothing else uses meanwhile.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn canvas_2d_segments_take(segments: *mut c_void) -> *mut c_void {
    // SAFETY: Guaranteed by the caller.
    segments_into_handle(std::mem::take(unsafe { segments_from_handle_mut(segments) }))
}

/// Checks the tapes of segments that came from another process. Returns false with a static error message, whose
/// address and size it writes, if a tape does not pass.
///
/// # Safety
///
/// `segments` must be a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn canvas_2d_segments_validate(
    segments: *const c_void,
    error: &mut *const u8,
    error_size: &mut usize,
) -> bool {
    // SAFETY: Guaranteed by the caller.
    match unsafe { segments_from_handle(segments) }.validate() {
        Ok(()) => true,
        Err(message) => {
            *error = message.as_ptr();
            *error_size = message.len();
            false
        }
    }
}

/// Calls `visit` with the canvas, the tape and whether it presents of each segment, in order.
///
/// # Safety
///
/// `segments` must be a live handle, and `visit` runs synchronously and reads the tape only while it runs.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn canvas_2d_segments_for_each(
    segments: *const c_void,
    context: *mut c_void,
    visit: unsafe extern "C" fn(*mut c_void, u64, ak::Span<'_, u8>, bool),
) {
    // SAFETY: Guaranteed by the caller.
    for segment in unsafe { segments_from_handle(segments) }.segments() {
        // SAFETY: The caller reads the tape synchronously.
        unsafe {
            visit(
                context,
                segment.canvas_id.0,
                ak::Span::new(&segment.tape),
                segment.present,
            );
        }
    }
}

/// Encodes `segments` into the message that the C++ `IPC::Encoder` at `encoder` builds, through `append`. Answers
/// whether they encoded.
///
/// # Safety
///
/// `segments` must be a live handle, and `append` must append to `encoder` before it returns.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn canvas_2d_segments_ipc_encode(
    segments: *const c_void,
    encoder: *mut c_void,
    append: unsafe extern "C" fn(*mut c_void, *const u8, usize),
) -> bool {
    // SAFETY: Guaranteed by the caller.
    unsafe { crate::ipc::encode_into_cpp(segments_from_handle(segments), encoder, append) }
}

/// Decodes segments from the start of `bytes`, the rest of a message that a C++ `IPC::Decoder` reads, and sets
/// `consumed` to how many bytes they took. Returns a handle the caller destroys, or null when the bytes do not start
/// with segments. Their tapes are not checked yet.
///
/// # Safety
///
/// `bytes` must address `length` readable bytes for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn canvas_2d_segments_ipc_decode(
    bytes: *const u8,
    length: usize,
    consumed: &mut usize,
) -> *mut c_void {
    // SAFETY: The caller guarantees `bytes` addresses `length` readable bytes.
    let bytes = unsafe { ak::slice_from_raw_parts(bytes, length) };
    match crate::ipc::decode_from_cpp::<Canvas2DSegments>(bytes, consumed) {
        Some(segments) => segments_into_handle(segments),
        None => std::ptr::null_mut(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::display_list::builder::{HEADER_SIZE, read_command, read_header};
    use crate::display_list::nested_records::span_bytes;
    use crate::display_list::storage::write_run;
    use crate::display_list::validate::validate_tape;

    fn records(tape: &[u8]) -> Vec<(DisplayListCommandType, Vec<u8>)> {
        let mut records = Vec::new();
        for_each_command(tape, |header, _, payload| {
            records.push((header.command_type, payload.to_vec()));
        });
        records
    }

    fn linear_gradient<'a>(colors: &'a [Color], positions: &'a [f32]) -> Paint<'a> {
        Paint::LinearGradient {
            start_point: FloatPoint { x: 1.0, y: 2.0 },
            end_point: FloatPoint { x: 3.0, y: 4.0 },
            color_stops: ColorStops { colors, positions },
        }
    }

    #[test]
    fn a_fill_carries_its_path_its_paint_and_its_filter_inline() {
        let colors = [Color(0xff00_00ff), Color(0xffff_0000)];
        let positions = [0.0, 1.0];
        let path = [1u8, 2, 3, 4, 5];
        let filter = [9u8, 8, 7];
        let mut writer = TapeWriter::default();
        writer.fill_path(
            &path,
            &linear_gradient(&colors, &positions),
            WindingRule::EvenOdd,
            0.0,
            &Compositing {
                filter: Some(&filter),
                global_alpha: 0.5,
                compositing_and_blending_operator: CompositingAndBlendingOperator::Multiply,
            },
        );
        assert_eq!(validate_canvas_2d_tape(writer.bytes()), Ok(()));
        let header = read_header(writer.bytes());
        assert!(!header.has_bounding_rect);
        assert_eq!(header.inline_clip_count, 0);
        let records = records(writer.bytes());
        assert_eq!(records.len(), 1);
        let (command_type, payload) = &records[0];
        assert_eq!(*command_type, DisplayListCommandType::Canvas2DFillPath);
        let command = read_command::<Canvas2DFillPath>(payload);
        assert_eq!(span_bytes(payload, command.path_data), path);
        assert_eq!(command.paint.kind, Canvas2DPaintKind::LinearGradient);
        assert_eq!(command.paint.end_point, FloatPoint { x: 3.0, y: 4.0 });
        let stop_colors: Vec<u32> = span_bytes(payload, command.paint.color_stops.colors)
            .as_chunks::<4>()
            .0
            .iter()
            .map(|bytes| u32::from_ne_bytes(*bytes))
            .collect();
        assert_eq!(stop_colors, [0xff00_00ff, 0xffff_0000]);
        assert_eq!(command.paint.color_stops.positions.size, 8);
        assert_eq!(command.paint.color_stops.positions.offset % 4, 0);
        assert_eq!(command.winding_rule, WindingRule::EvenOdd);
        assert!(command.compositing.has_filter);
        assert_eq!(span_bytes(payload, command.compositing.filter), filter);
        assert_eq!(command.compositing.global_alpha, 0.5);
        assert_eq!(
            command.compositing.compositing_and_blending_operator,
            CompositingAndBlendingOperator::Multiply
        );
    }

    #[test]
    fn records_follow_each_other_in_recording_order() {
        let mut writer = TapeWriter::default();
        writer.save();
        writer.set_transform(AffineTransform {
            values: [2.0, 0.0, 0.0, 2.0, 5.0, 6.0],
        });
        writer.clear_rect(FloatRect::new(0.0, 0.0, 10.0, 10.0), Color(0));
        writer.clip_path(&[4, 4, 4, 4], WindingRule::Nonzero);
        writer.draw_glyph_run(
            FontResourceId(7),
            &[DisplayListGlyph {
                position: FloatPoint { x: 1.0, y: 0.0 },
                glyph_id: 42,
            }],
            FloatPoint { x: 10.0, y: 20.0 },
            &Paint::Color(Color(0xff00_ff00)),
            &Compositing::default(),
        );
        writer.restore();
        writer.reset();
        writer.clear_canvas(Color(0xff00_0000));
        assert_eq!(validate_canvas_2d_tape(writer.bytes()), Ok(()));
        let types: Vec<_> = records(writer.bytes())
            .into_iter()
            .map(|(command_type, _)| command_type)
            .collect();
        assert_eq!(
            types,
            [
                DisplayListCommandType::Canvas2DSave,
                DisplayListCommandType::Canvas2DSetTransform,
                DisplayListCommandType::Canvas2DClearRect,
                DisplayListCommandType::Canvas2DClipPath,
                DisplayListCommandType::Canvas2DDrawGlyphRun,
                DisplayListCommandType::Canvas2DRestore,
                DisplayListCommandType::Canvas2DReset,
                DisplayListCommandType::Canvas2DClearCanvas,
            ]
        );
        assert_eq!(writer.bytes().len() % 8, 0);
    }

    #[test]
    fn a_draw_without_a_filter_carries_no_filter_bytes() {
        let mut writer = TapeWriter::default();
        writer.draw_image_frame(
            ImageFrameResourceId(3),
            FloatRect::new(0.0, 0.0, 4.0, 4.0),
            IntRect::new(0, 0, 2, 2),
            ScalingMode::NearestNeighbor,
            &Compositing::default(),
        );
        let records = records(writer.bytes());
        let command = read_command::<Canvas2DDrawImageFrame>(&records[0].1);
        assert!(!command.compositing.has_filter);
        assert!(command.compositing.filter.is_empty());
        assert_eq!(command.frame_id, ImageFrameResourceId(3));
        assert_eq!(
            records[0].1.len(),
            size_of::<Canvas2DDrawImageFrame>().next_multiple_of(8)
        );
    }

    #[test]
    fn a_pattern_without_an_image_says_so() {
        let mut writer = TapeWriter::default();
        writer.stroke_path(
            &[1],
            &Paint::Pattern {
                image: None,
                repetition: Canvas2DPatternRepetition::RepeatY,
                transform: Some(AffineTransform {
                    values: [1.0, 0.0, 0.0, 1.0, 3.0, 4.0],
                }),
            },
            &Stroke {
                thickness: 2.0,
                cap_style: CapStyle::Round,
                join_style: JoinStyle::Bevel,
                miter_limit: 10.0,
                dash_array: &[1.0, 2.0],
                dash_offset: 0.5,
            },
            3.0,
            &Compositing::default(),
        );
        assert_eq!(validate_canvas_2d_tape(writer.bytes()), Ok(()));
        let records = records(writer.bytes());
        let command = read_command::<Canvas2DStrokePath>(&records[0].1);
        assert!(!command.paint.pattern_has_image);
        assert_eq!(command.paint.pattern_repetition, Canvas2DPatternRepetition::RepeatY);
        assert_eq!(command.paint.pattern_transform.get().unwrap().values[4], 3.0);
        assert_eq!(command.dash_array.size, 8);
        assert_eq!(command.blur_radius, 3.0);
    }

    #[test]
    fn a_tape_holds_only_the_commands_of_its_kind() {
        let mut writer = TapeWriter::default();
        writer.save();
        let mut run_bytes = vec![0u8; size_of::<DisplayListCommandRun>()];
        write_run(
            &DisplayListCommandRun {
                offset: 0,
                size: u32::try_from(writer.bytes().len()).unwrap(),
                ..Default::default()
            },
            &mut run_bytes,
        );
        assert_eq!(
            validate_tape(writer.bytes(), &run_bytes),
            Err("Display list holds a command of the canvas 2D stream")
        );
        let mut page_records = Vec::new();
        append_record(
            &mut page_records,
            &FillRect {
                rect: IntRect::new(0, 0, 1, 1),
                color: Color(0),
                compositing_and_blending_operator: CompositingAndBlendingOperator::Normal,
                background_color_animation_effect: EffectNodeIndex::NONE,
            },
            &[],
        );
        assert_eq!(
            validate_canvas_2d_tape(&page_records),
            Err("Canvas 2D stream holds a command of a display list")
        );
    }

    #[test]
    fn a_global_alpha_outside_0_to_1_is_rejected() {
        for (global_alpha, valid) in [
            (0.0, true),
            (1.0, true),
            (-0.5, false),
            (1.5, false),
            (f32::NAN, false),
            (f32::INFINITY, false),
        ] {
            let mut writer = TapeWriter::default();
            writer.fill_path(
                &[1],
                &Paint::Color(Color(0xff00_0000)),
                WindingRule::Nonzero,
                0.0,
                &Compositing {
                    global_alpha,
                    ..Compositing::default()
                },
            );
            let expected = if valid {
                Ok(())
            } else {
                Err("Canvas 2D stream command has a global alpha outside 0 to 1")
            };
            assert_eq!(validate_canvas_2d_tape(writer.bytes()), expected);
        }
    }

    #[test]
    fn a_command_with_a_bounding_rect_is_rejected() {
        let mut writer = TapeWriter::default();
        writer.save();
        writer.clear_rect(FloatRect::new(0.0, 0.0, 1.0, 1.0), Color(0));
        let mut tape = writer.bytes().to_vec();
        let second_record = read_header(&tape).payload_size as usize + HEADER_SIZE;
        tape[second_record + std::mem::offset_of!(DisplayListCommandHeader, has_bounding_rect)] = 1;
        assert_eq!(
            validate_canvas_2d_tape(&tape),
            Err("Canvas 2D stream command has a bounding rect")
        );
    }

    #[test]
    fn a_span_outside_its_payload_is_rejected() {
        let mut writer = TapeWriter::default();
        writer.clip_path(&[1, 2, 3, 4], WindingRule::Nonzero);
        let mut tape = writer.bytes().to_vec();
        let size_offset = HEADER_SIZE
            + std::mem::offset_of!(Canvas2DClipPath, path_data)
            + std::mem::offset_of!(DisplayListDataSpan, size);
        tape[size_offset..size_offset + 4].copy_from_slice(&1000u32.to_ne_bytes());
        assert_eq!(
            validate_canvas_2d_tape(&tape),
            Err("Display list span exceeds its command payload")
        );
        let mut tape = writer.bytes().to_vec();
        tape[HEADER_SIZE + std::mem::offset_of!(Canvas2DClipPath, winding_rule)] = 7;
        assert_eq!(
            validate_canvas_2d_tape(&tape),
            Err("Display list command has an invalid enum value")
        );
    }

    fn record(build: impl FnOnce(&mut TapeWriter)) -> Vec<u8> {
        let mut writer = TapeWriter::default();
        build(&mut writer);
        writer.bytes().to_vec()
    }

    fn segment(canvas_id: u64, tape: Vec<u8>, present: bool) -> Canvas2DSegment {
        Canvas2DSegment {
            canvas_id: CanvasId(canvas_id),
            tape,
            present,
        }
    }

    #[test]
    fn segments_split_where_the_canvas_changes_or_presents() {
        let save = record(TapeWriter::save);
        let clear = record(|writer| writer.clear_canvas(Color(0xff00_00ff)));
        let mut segments = Canvas2DSegments::default();
        segments.append_command(CanvasId(1), &save);
        segments.append_command(CanvasId(1), &clear);
        segments.append_command(CanvasId(2), &save);
        segments.record_present(CanvasId(2));
        // Recording after a present starts a new segment, even for the same canvas.
        segments.append_command(CanvasId(2), &clear);
        // A present for a canvas other than the last one's is a segment of its own.
        segments.record_present(CanvasId(1));
        assert_eq!(segments.command_count(), 4);
        assert_eq!(
            segments.segments(),
            [
                segment(1, [save.clone(), clear.clone()].concat(), false),
                segment(2, save, true),
                segment(2, clear, false),
                segment(1, Vec::new(), true),
            ]
        );

        let taken = std::mem::take(&mut segments);
        assert!(segments.is_empty());
        assert_eq!(segments.command_count(), 0);
        assert_eq!(taken.segments().len(), 4);
    }

    #[test]
    fn segments_go_over_ipc_as_a_vector_of_canvas_tape_and_present() {
        let clear = record(|writer| writer.clear_canvas(Color(0xff00_00ff)));
        let mut segments = Canvas2DSegments::default();
        segments.append_command(CanvasId(7), &clear);
        segments.record_present(CanvasId(7));
        segments.record_present(CanvasId(9));

        let mut encoder = Encoder::new();
        encoder.encode(&segments).unwrap();
        let (message, attachments) = encoder.finish();
        assert!(attachments.is_empty());

        // As C++ encoded a Vector<Canvas2DCommandStreamSegment>: the count, then each canvas ID, tape and present flag.
        let mut expected = 2u32.to_ne_bytes().to_vec();
        expected.extend_from_slice(&7u64.to_ne_bytes());
        expected.extend_from_slice(&(clear.len() as u32).to_ne_bytes());
        expected.extend_from_slice(&clear);
        expected.push(1);
        expected.extend_from_slice(&9u64.to_ne_bytes());
        expected.extend_from_slice(&0u32.to_ne_bytes());
        expected.push(1);
        assert_eq!(message, expected);

        let mut decoder = Decoder::new(&message, std::iter::empty());
        let decoded: Canvas2DSegments = decoder.decode().unwrap();
        assert!(decoder.is_at_end());
        assert_eq!(decoded.segments(), segments.segments());
        assert_eq!(decoded.validate(), Ok(()));
    }

    #[test]
    fn malformed_segments_are_refused() {
        let mut segments = Canvas2DSegments::default();
        segments.append_command(CanvasId(1), &record(TapeWriter::save));
        let mut encoder = Encoder::new();
        encoder.encode(&segments).unwrap();
        let (message, _) = encoder.finish();

        // Cut short.
        assert!(
            Decoder::new(&message[..message.len() - 1], std::iter::empty())
                .decode::<Canvas2DSegments>()
                .is_err()
        );

        // A present flag that is not a boolean.
        let mut bad_flag = message.clone();
        *bad_flag.last_mut().unwrap() = 2;
        assert_eq!(
            Decoder::new(&bad_flag, std::iter::empty())
                .decode::<Canvas2DSegments>()
                .err(),
            Some(DecodeError::InvalidBoolean(2))
        );

        // A tape that decodes but does not validate.
        let mut not_a_tape = Canvas2DSegments::default();
        not_a_tape.append_command(CanvasId(1), &[1, 2, 3]);
        let mut encoder = Encoder::new();
        encoder.encode(&not_a_tape).unwrap();
        let (message, _) = encoder.finish();
        let decoded: Canvas2DSegments = Decoder::new(&message, std::iter::empty()).decode().unwrap();
        assert_eq!(decoded.validate(), Err("Canvas 2D stream tape has an invalid size"));
    }
}

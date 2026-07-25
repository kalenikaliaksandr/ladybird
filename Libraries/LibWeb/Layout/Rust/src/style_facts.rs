/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

use crate::abort_on_panic;
use crate::css_pixels::CssPixels;
use crate::display::FfiDisplay;
use std::ffi::c_void;
use std::sync::OnceLock;

pub const STYLE_GROUP_COUNT: usize = 23;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum FfiSizeKind {
    Auto,
    Px,
    Percentage,
    Calc,
    MinContent,
    MaxContent,
    FitContent,
    None_,
}

/// A computed CSS size value with no Rust-owned allocation.
///
/// `kind` is an `FfiSizeKind`. `fraction` is used for Percentage, `px` for Px,
/// and `calc` for Calc. FitContent uses the matching payload for its optional
/// inner length-percentage; an all-zero payload is its keyword/zero form. A
/// non-null calc handle points to retained Rust-owned calculated style-value
/// data; the caller must balance every returned handle with
/// `ladybird_layout_release_calc_handle`.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct FfiSizeValue {
    pub kind: u8,
    pub px: CssPixels,
    pub fraction: f64,
    pub calc: *const c_void,
    pub contains_percentage: bool,
    pub contains_anchor_function: bool,
}

impl FfiSizeValue {
    pub(crate) fn auto_value() -> Self {
        Self {
            kind: FfiSizeKind::Auto as u8,
            px: CssPixels::default(),
            fraction: 0.0,
            calc: std::ptr::null(),
            contains_percentage: false,
            contains_anchor_function: false,
        }
    }

    pub(crate) fn px_value(px: CssPixels) -> Self {
        Self {
            kind: FfiSizeKind::Px as u8,
            px,
            fraction: 0.0,
            calc: std::ptr::null(),
            contains_percentage: false,
            contains_anchor_function: false,
        }
    }

    #[cfg(test)]
    fn with_kind(kind: FfiSizeKind) -> Self {
        Self {
            kind: kind as u8,
            px: CssPixels::default(),
            fraction: 0.0,
            calc: std::ptr::null(),
            contains_percentage: false,
            contains_anchor_function: false,
        }
    }

    #[cfg(test)]
    pub(crate) fn px(px: CssPixels) -> Self {
        Self::px_value(px)
    }

    #[cfg(test)]
    pub(crate) fn percentage(fraction: f64) -> Self {
        Self {
            fraction,
            ..Self::with_kind(FfiSizeKind::Percentage)
        }
    }

    pub(crate) fn release_calc_handle(self) {
        if !self.calc.is_null() {
            // SAFETY: Every non-null handle in a style snapshot was retained
            // by LayoutRustBridge and is released exactly once when the
            // per-pass Rust layout state is dropped.
            unsafe {
                ladybird_layout_release_calc_handle(self.calc);
            }
        }
    }

    pub(crate) fn kind(self) -> FfiSizeKind {
        assert!(self.kind <= FfiSizeKind::None_ as u8);
        // SAFETY: The range check above covers every repr(u8) variant.
        unsafe { std::mem::transmute(self.kind) }
    }

    pub(crate) fn is_auto(self) -> bool {
        self.kind() == FfiSizeKind::Auto
    }

    pub(crate) fn is_length(self) -> bool {
        self.kind() == FfiSizeKind::Px
    }

    pub(crate) fn is_percentage(self) -> bool {
        self.kind() == FfiSizeKind::Percentage
    }

    pub(crate) fn is_length_percentage(self) -> bool {
        matches!(
            self.kind(),
            FfiSizeKind::Px | FfiSizeKind::Percentage | FfiSizeKind::Calc
        )
    }

    pub(crate) fn is_min_content(self) -> bool {
        self.kind() == FfiSizeKind::MinContent
    }

    pub(crate) fn is_max_content(self) -> bool {
        self.kind() == FfiSizeKind::MaxContent
    }

    pub(crate) fn is_fit_content(self) -> bool {
        self.kind() == FfiSizeKind::FitContent
    }

    pub(crate) fn is_none(self) -> bool {
        self.kind() == FfiSizeKind::None_
    }

    pub(crate) fn is_intrinsic_sizing_constraint(self) -> bool {
        matches!(
            self.kind(),
            FfiSizeKind::MinContent | FfiSizeKind::MaxContent | FfiSizeKind::FitContent
        )
    }

    pub(crate) fn to_px(self, reference: CssPixels) -> CssPixels {
        match self.kind() {
            FfiSizeKind::Px => self.px,
            FfiSizeKind::Percentage => truncated_css_pixels(reference.to_double() * self.fraction),
            FfiSizeKind::Calc => resolve_calc(self.calc, reference),
            FfiSizeKind::FitContent if !self.calc.is_null() => resolve_calc(self.calc, reference),
            FfiSizeKind::FitContent if self.contains_percentage => {
                truncated_css_pixels(reference.to_double() * self.fraction)
            }
            FfiSizeKind::FitContent => self.px,
            FfiSizeKind::Auto | FfiSizeKind::MinContent | FfiSizeKind::MaxContent | FfiSizeKind::None_ => {
                CssPixels::default()
            }
        }
    }
}

fn truncated_css_pixels(value: f64) -> CssPixels {
    if value.is_nan() {
        return CssPixels::default();
    }
    let raw = (value * 64.0).trunc();
    CssPixels::from_raw(raw.clamp(i32::MIN as f64, i32::MAX as f64) as i32)
}

fn resolve_calc(calc: *const c_void, percentage_basis: CssPixels) -> CssPixels {
    assert!(!calc.is_null());
    // Pinned to C++ LengthUnit::Px; LayoutRustBridge.cpp static-asserts it.
    const LENGTH_UNIT_PX: u8 = 29;
    let context = CssFfiCalcResolutionContext {
        basis_kind: 3,
        basis_value: percentage_basis.to_double(),
        basis_unit: LENGTH_UNIT_PX,
        length_resolution_context: std::ptr::null(),
        callback_context: std::ptr::null_mut(),
        resolve_non_math_function: no_non_math_function,
        resolve_channel_keyword: no_channel_keyword,
        random_base_value: no_random_base_value,
        absolutize_random_sharing: no_absolutized_random_sharing,
        resolve_length: no_fallback_length,
    };
    // SAFETY: The handle is retained by this state and the context contains
    // the same no-host-callback setup used by the Phase B parity hook.
    let result = unsafe { rust_calc_resolve(calc, &raw const context, true) };
    assert!(result.resolved);
    CssPixels::nearest_value_for(result.value)
}

/// Every computed-value field layout can read. Complex entries have no byte
/// offset and are decoded by C++ on first use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum FfiStyleField {
    Width,
    Height,
    MinWidth,
    MinHeight,
    MaxWidth,
    MaxHeight,
    MarginTop,
    MarginRight,
    MarginBottom,
    MarginLeft,
    PaddingTop,
    PaddingRight,
    PaddingBottom,
    PaddingLeft,
    InsetTop,
    InsetRight,
    InsetBottom,
    InsetLeft,
    PositionAnchor,
    BorderTopWidth,
    BorderRightWidth,
    BorderBottomWidth,
    BorderLeftWidth,
    BorderTopStyle,
    BorderRightStyle,
    BorderBottomStyle,
    BorderLeftStyle,
    Position,
    Float,
    Clear,
    WritingMode,
    Direction,
    TextAlign,
    TextJustify,
    WhiteSpaceCollapse,
    TextWrapMode,
    VerticalAlign,
    LineHeight,
    FontSize,
    Font,
    BoxSizing,
    BoxSizingForAspectRatio,
    OverflowX,
    OverflowY,
    TextOverflow,
    FlexDirection,
    FlexWrap,
    FlexGrow,
    FlexShrink,
    FlexBasis,
    Order,
    AlignItems,
    AlignSelf,
    AlignContent,
    JustifyContent,
    JustifyItems,
    JustifySelf,
    RowGap,
    ColumnGap,
    AspectRatio,
    Appearance,
    BorderCollapse,
    BorderSpacingHorizontal,
    BorderSpacingVertical,
    CaptionSide,
    TableLayout,
    ColumnWidth,
    ColumnCount,
    Containment,
    ContainerType,
    ContentVisibility,
    Visibility,
    WordBreak,
    ZIndex,
    FontVariantEmoji,
    LetterSpacing,
    WordSpacing,
    UnicodeBidi,
    TextTransform,
    TextIndent,
    TabSize,
    GridAutoFlowRow,
    GridAutoFlowDense,
    X,
    Y,
    UserSelect,
    Opacity,
    Isolation,
    MixBlendMode,
    TransformStyle,
    Perspective,
    ListStylePosition,
    TextDecorationStyle,
    Count,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum FfiStyleFieldEncoding {
    Lazy,
    U8,
    Bool,
    I32,
    F32,
    F64,
    CssPixels,
}

#[derive(Clone, Copy)]
#[repr(C)]
pub struct FfiStyleFieldSchema {
    pub field: FfiStyleField,
    pub group_index: u8,
    pub offset: u32,
    pub group_size: u32,
    pub encoding: FfiStyleFieldEncoding,
}

#[derive(Clone, Copy)]
#[repr(C)]
pub struct FfiStylePayloads {
    /// Borrowed pointers to every `ComputedValues` group payload. A
    /// `NodeWithStyle` keeps its immutable `ComputedValues` alive, and style
    /// replacement cannot run during the synchronous layout pass, so these
    /// pointers remain valid until the pass returns to C++.
    pub groups: [*const c_void; STYLE_GROUP_COUNT],
}

impl Default for FfiStylePayloads {
    fn default() -> Self {
        Self {
            groups: [std::ptr::null(); STYLE_GROUP_COUNT],
        }
    }
}

pub(crate) const STYLE_FIELD_COUNT: usize = FfiStyleField::Count as usize;

struct StyleSchema([Option<FfiStyleFieldSchema>; STYLE_FIELD_COUNT]);

// SAFETY: Schema entries contain only immutable integers and enum values.
unsafe impl Send for StyleSchema {}
unsafe impl Sync for StyleSchema {}

static STYLE_SCHEMA: OnceLock<StyleSchema> = OnceLock::new();

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rust_layout_register_style_schema(entries: *const FfiStyleFieldSchema, count: usize) {
    abort_on_panic(|| {
        let entries = unsafe { std::slice::from_raw_parts(entries, count) };
        let mut schema = [None; STYLE_FIELD_COUNT];
        for entry in entries {
            let index = entry.field as usize;
            assert!(index < STYLE_FIELD_COUNT);
            assert!((entry.group_index as usize) < STYLE_GROUP_COUNT);
            assert!(schema[index].is_none(), "duplicate style schema field");
            let width = match entry.encoding {
                FfiStyleFieldEncoding::Lazy => 0,
                FfiStyleFieldEncoding::U8 | FfiStyleFieldEncoding::Bool => 1,
                FfiStyleFieldEncoding::I32 | FfiStyleFieldEncoding::F32 | FfiStyleFieldEncoding::CssPixels => 4,
                FfiStyleFieldEncoding::F64 => 8,
            };
            assert!(entry.offset as usize + width <= entry.group_size as usize);
            schema[index] = Some(*entry);
        }
        assert!(schema.iter().all(Option::is_some));
        assert!(
            STYLE_SCHEMA.set(StyleSchema(schema)).is_ok(),
            "style schema registered twice"
        );
        crate::ffi_stats::bump(crate::ffi_stats::FfiOp::StyleSchemaRegistration);
    });
}

fn schema_for(field: FfiStyleField) -> FfiStyleFieldSchema {
    STYLE_SCHEMA.get().expect("style schema used before registration").0[field as usize]
        .expect("missing style schema field")
}

#[derive(Clone, Copy)]
#[repr(C)]
pub struct FfiDecodedStyleValue {
    pub size: FfiSizeValue,
    pub has_value: bool,
    pub bool_value: bool,
    pub u8_value: u8,
    pub i32_value: i32,
    pub css_pixels_value: CssPixels,
    pub f64_value: f64,
    pub f64_value_2: f64,
    pub pointer: *const c_void,
    pub f32_value: f32,
    pub f32_value_2: f32,
    pub f32_value_3: f32,
    pub f32_value_4: f32,
    /// A transferred Utf16FlyString reference, when non-zero.
    pub retained_name: usize,
}

impl Default for FfiDecodedStyleValue {
    fn default() -> Self {
        Self {
            size: FfiSizeValue::auto_value(),
            has_value: false,
            bool_value: false,
            u8_value: 0,
            i32_value: 0,
            css_pixels_value: CssPixels::default(),
            f64_value: 0.0,
            f64_value_2: 0.0,
            pointer: std::ptr::null(),
            f32_value: 0.0,
            f32_value_2: 0.0,
            f32_value_3: 0.0,
            f32_value_4: 0.0,
            retained_name: 0,
        }
    }
}

pub type FfiDecodeStyleFieldCallback =
    unsafe extern "C" fn(*mut c_void, *const c_void, FfiStyleField) -> FfiDecodedStyleValue;

#[derive(Clone, Copy)]
pub(crate) struct StyleReader {
    payloads: FfiStylePayloads,
}

impl StyleReader {
    pub(crate) fn new(payloads: FfiStylePayloads) -> Self {
        Self { payloads }
    }

    fn address(self, field: FfiStyleField, encoding: FfiStyleFieldEncoding) -> *const u8 {
        let schema = schema_for(field);
        assert_eq!(schema.encoding, encoding);
        let payload = self.payloads.groups[schema.group_index as usize];
        assert!(!payload.is_null());
        unsafe { (payload as *const u8).add(schema.offset as usize) }
    }

    pub(crate) fn u8(self, field: FfiStyleField) -> u8 {
        unsafe { self.address(field, FfiStyleFieldEncoding::U8).read_unaligned() }
    }

    pub(crate) fn bool(self, field: FfiStyleField) -> bool {
        let value = unsafe { self.address(field, FfiStyleFieldEncoding::Bool).read_unaligned() };
        assert!(value <= 1);
        value != 0
    }

    pub(crate) fn i32(self, field: FfiStyleField) -> i32 {
        unsafe { (self.address(field, FfiStyleFieldEncoding::I32) as *const i32).read_unaligned() }
    }

    pub(crate) fn f32(self, field: FfiStyleField) -> f32 {
        unsafe { (self.address(field, FfiStyleFieldEncoding::F32) as *const f32).read_unaligned() }
    }

    pub(crate) fn f64(self, field: FfiStyleField) -> f64 {
        unsafe { (self.address(field, FfiStyleFieldEncoding::F64) as *const f64).read_unaligned() }
    }

    pub(crate) fn css_pixels(self, field: FfiStyleField) -> CssPixels {
        let raw = unsafe { (self.address(field, FfiStyleFieldEncoding::CssPixels) as *const i32).read_unaligned() };
        CssPixels::from_raw(raw)
    }

    fn payload(self, field: FfiStyleField) -> *const c_void {
        let schema = schema_for(field);
        assert_eq!(schema.encoding, FfiStyleFieldEncoding::Lazy);
        let payload = self.payloads.groups[schema.group_index as usize];
        assert!(!payload.is_null());
        payload
    }
}

pub(crate) struct LazyStyleCache {
    values: Vec<(FfiStyleField, FfiDecodedStyleValue)>,
}

impl LazyStyleCache {
    pub(crate) fn new() -> Self {
        Self { values: Vec::new() }
    }

    fn decode(
        &mut self,
        reader: StyleReader,
        context: *mut c_void,
        callback: FfiDecodeStyleFieldCallback,
        field: FfiStyleField,
    ) -> FfiDecodedStyleValue {
        if let Some((_, value)) = self.values.iter().find(|(cached, _)| *cached == field) {
            return *value;
        }
        crate::ffi_stats::bump(crate::ffi_stats::FfiOp::StyleLazyDecode);
        let value = unsafe { callback(context, reader.payload(field), field) };
        self.values.push((field, value));
        value
    }

    pub(crate) fn replace_size(&mut self, field: FfiStyleField, value: FfiSizeValue) {
        if let Some((_, decoded)) = self.values.iter_mut().find(|(cached, _)| *cached == field) {
            decoded.size.release_calc_handle();
            decoded.size = value;
            return;
        }
        self.values.push((
            field,
            FfiDecodedStyleValue {
                size: value,
                ..FfiDecodedStyleValue::default()
            },
        ));
    }
}

impl Drop for LazyStyleCache {
    fn drop(&mut self) {
        for (_, value) in &self.values {
            value.size.release_calc_handle();
            if value.retained_name != 0 {
                unsafe {
                    ladybird_layout_release_anchor_name_handle(value.retained_name);
                }
            }
        }
    }
}

/// A Rust-only view over a node's immutable computed-value group payloads.
///
/// Plain fields below are copied directly from the payload bytes using the
/// registered C++ schema. Methods decode complex C++ values on first use and
/// read the per-node Rust cache thereafter.
#[derive(Clone, Copy)]
pub(crate) struct StyleValues {
    reader: StyleReader,
    cache: *mut LazyStyleCache,
    callback_context: *mut c_void,
    decode_callback: FfiDecodeStyleFieldCallback,

    pub display: FfiDisplay,
    pub border_top_width: CssPixels,
    pub border_right_width: CssPixels,
    pub border_bottom_width: CssPixels,
    pub border_left_width: CssPixels,
    pub border_top_style: u8,
    pub border_right_style: u8,
    pub border_bottom_style: u8,
    pub border_left_style: u8,
    pub position: u8,
    pub float_: u8,
    pub clear: u8,
    pub writing_mode: u8,
    pub direction: u8,
    pub text_align: u8,
    pub text_justify: u8,
    pub white_space_collapse: u8,
    pub text_wrap_mode: u8,
    pub line_height: CssPixels,
    pub font_size: CssPixels,
    pub box_sizing: u8,
    pub overflow_x: u8,
    pub overflow_y: u8,
    pub text_overflow: u8,
    pub flex_direction: u8,
    pub flex_wrap: u8,
    pub flex_grow: f64,
    pub flex_shrink: f64,
    pub order: i32,
    pub align_items: u8,
    pub align_self: u8,
    pub align_content: u8,
    pub justify_content: u8,
    pub justify_items: u8,
    pub justify_self: u8,
    pub appearance: u8,
    pub border_collapse: u8,
    pub border_spacing_horizontal: CssPixels,
    pub border_spacing_vertical: CssPixels,
    pub caption_side: u8,
    pub table_layout: u8,
    pub content_visibility: u8,
    pub visibility: u8,
    pub word_break: u8,
    pub font_variant_emoji: u8,
    pub letter_spacing: CssPixels,
    pub word_spacing: CssPixels,
    pub unicode_bidi: u8,
    pub text_transform: u8,
    pub grid_auto_flow_row: bool,
    pub grid_auto_flow_dense: bool,
    pub user_select: u8,
    pub opacity: f64,
    pub isolation: u8,
    pub mix_blend_mode: u8,
    pub transform_style: u8,
    pub list_style_position: u8,
    pub text_decoration_style: u8,
    vertical_align_override: u16,
}

impl StyleValues {
    pub(crate) fn new(
        payloads: FfiStylePayloads,
        display: FfiDisplay,
        cache: *mut LazyStyleCache,
        callback_context: *mut c_void,
        decode_callback: FfiDecodeStyleFieldCallback,
    ) -> Self {
        let reader = StyleReader::new(payloads);
        Self {
            reader,
            cache,
            callback_context,
            decode_callback,
            display,
            border_top_width: reader.css_pixels(FfiStyleField::BorderTopWidth),
            border_right_width: reader.css_pixels(FfiStyleField::BorderRightWidth),
            border_bottom_width: reader.css_pixels(FfiStyleField::BorderBottomWidth),
            border_left_width: reader.css_pixels(FfiStyleField::BorderLeftWidth),
            border_top_style: reader.u8(FfiStyleField::BorderTopStyle),
            border_right_style: reader.u8(FfiStyleField::BorderRightStyle),
            border_bottom_style: reader.u8(FfiStyleField::BorderBottomStyle),
            border_left_style: reader.u8(FfiStyleField::BorderLeftStyle),
            position: reader.u8(FfiStyleField::Position),
            float_: reader.u8(FfiStyleField::Float),
            clear: reader.u8(FfiStyleField::Clear),
            writing_mode: reader.u8(FfiStyleField::WritingMode),
            direction: reader.u8(FfiStyleField::Direction),
            text_align: reader.u8(FfiStyleField::TextAlign),
            text_justify: reader.u8(FfiStyleField::TextJustify),
            white_space_collapse: reader.u8(FfiStyleField::WhiteSpaceCollapse),
            text_wrap_mode: reader.u8(FfiStyleField::TextWrapMode),
            line_height: reader.css_pixels(FfiStyleField::LineHeight),
            font_size: reader.css_pixels(FfiStyleField::FontSize),
            box_sizing: reader.u8(FfiStyleField::BoxSizing),
            overflow_x: reader.u8(FfiStyleField::OverflowX),
            overflow_y: reader.u8(FfiStyleField::OverflowY),
            text_overflow: reader.u8(FfiStyleField::TextOverflow),
            flex_direction: reader.u8(FfiStyleField::FlexDirection),
            flex_wrap: reader.u8(FfiStyleField::FlexWrap),
            flex_grow: reader.f64(FfiStyleField::FlexGrow),
            flex_shrink: reader.f64(FfiStyleField::FlexShrink),
            order: reader.i32(FfiStyleField::Order),
            align_items: reader.u8(FfiStyleField::AlignItems),
            align_self: reader.u8(FfiStyleField::AlignSelf),
            align_content: reader.u8(FfiStyleField::AlignContent),
            justify_content: reader.u8(FfiStyleField::JustifyContent),
            justify_items: reader.u8(FfiStyleField::JustifyItems),
            justify_self: reader.u8(FfiStyleField::JustifySelf),
            appearance: reader.u8(FfiStyleField::Appearance),
            border_collapse: reader.u8(FfiStyleField::BorderCollapse),
            border_spacing_horizontal: reader.css_pixels(FfiStyleField::BorderSpacingHorizontal),
            border_spacing_vertical: reader.css_pixels(FfiStyleField::BorderSpacingVertical),
            caption_side: reader.u8(FfiStyleField::CaptionSide),
            table_layout: reader.u8(FfiStyleField::TableLayout),
            content_visibility: reader.u8(FfiStyleField::ContentVisibility),
            visibility: reader.u8(FfiStyleField::Visibility),
            word_break: reader.u8(FfiStyleField::WordBreak),
            font_variant_emoji: reader.u8(FfiStyleField::FontVariantEmoji),
            letter_spacing: reader.css_pixels(FfiStyleField::LetterSpacing),
            word_spacing: reader.css_pixels(FfiStyleField::WordSpacing),
            unicode_bidi: reader.u8(FfiStyleField::UnicodeBidi),
            text_transform: reader.u8(FfiStyleField::TextTransform),
            grid_auto_flow_row: reader.bool(FfiStyleField::GridAutoFlowRow),
            grid_auto_flow_dense: reader.bool(FfiStyleField::GridAutoFlowDense),
            user_select: reader.u8(FfiStyleField::UserSelect),
            opacity: reader.f32(FfiStyleField::Opacity) as f64,
            isolation: reader.u8(FfiStyleField::Isolation),
            mix_blend_mode: reader.u8(FfiStyleField::MixBlendMode),
            transform_style: reader.u8(FfiStyleField::TransformStyle),
            list_style_position: reader.u8(FfiStyleField::ListStylePosition),
            text_decoration_style: reader.u8(FfiStyleField::TextDecorationStyle),
            vertical_align_override: u16::MAX,
        }
    }

    fn decoded(self, field: FfiStyleField) -> FfiDecodedStyleValue {
        unsafe { &mut *self.cache }.decode(self.reader, self.callback_context, self.decode_callback, field)
    }

    pub(crate) fn with_vertical_align_keyword(mut self, keyword: u8) -> Self {
        self.vertical_align_override = keyword as u16;
        self
    }

    pub(crate) fn vertical_align_is_keyword(self) -> bool {
        self.vertical_align_override != u16::MAX || self.decoded(FfiStyleField::VerticalAlign).has_value
    }

    pub(crate) fn vertical_align_keyword(self) -> u8 {
        if self.vertical_align_override != u16::MAX {
            self.vertical_align_override as u8
        } else {
            self.decoded(FfiStyleField::VerticalAlign).u8_value
        }
    }

    pub(crate) fn vertical_align_value(self) -> FfiSizeValue {
        self.decoded(FfiStyleField::VerticalAlign).size
    }

    pub(crate) fn has_position_anchor(self) -> bool {
        self.decoded(FfiStyleField::PositionAnchor).has_value
    }

    pub(crate) fn position_anchor_name(self) -> usize {
        self.decoded(FfiStyleField::PositionAnchor).retained_name
    }

    pub(crate) fn first_available_font(self) -> *const c_void {
        self.decoded(FfiStyleField::Font).pointer
    }

    pub(crate) fn font_ascent(self) -> f32 {
        self.decoded(FfiStyleField::Font).f32_value
    }

    pub(crate) fn font_descent(self) -> f32 {
        self.decoded(FfiStyleField::Font).f32_value_2
    }

    pub(crate) fn font_x_height(self) -> f32 {
        self.decoded(FfiStyleField::Font).f32_value_3
    }

    pub(crate) fn font_pixel_size(self) -> f32 {
        self.decoded(FfiStyleField::Font).f32_value_4
    }

    pub(crate) fn box_sizing_for_aspect_ratio(self) -> u8 {
        self.decoded(FfiStyleField::BoxSizingForAspectRatio).u8_value
    }

    pub(crate) fn flex_basis_is_content(self) -> bool {
        self.decoded(FfiStyleField::FlexBasis).has_value
    }

    pub(crate) fn flex_basis(self) -> FfiSizeValue {
        self.decoded(FfiStyleField::FlexBasis).size
    }

    pub(crate) fn has_aspect_ratio(self) -> bool {
        self.decoded(FfiStyleField::AspectRatio).has_value
    }

    pub(crate) fn aspect_ratio_width(self) -> f64 {
        self.decoded(FfiStyleField::AspectRatio).f64_value
    }

    pub(crate) fn aspect_ratio_height(self) -> f64 {
        self.decoded(FfiStyleField::AspectRatio).f64_value_2
    }

    pub(crate) fn aspect_ratio_is_degenerate(self) -> bool {
        self.decoded(FfiStyleField::AspectRatio).bool_value
    }

    pub(crate) fn has_column_count(self) -> bool {
        self.decoded(FfiStyleField::ColumnCount).has_value
    }

    pub(crate) fn column_count(self) -> i32 {
        self.decoded(FfiStyleField::ColumnCount).i32_value
    }

    pub(crate) fn containment_bits(self) -> u8 {
        self.decoded(FfiStyleField::Containment).u8_value
    }

    pub(crate) fn container_type_bits(self) -> u8 {
        self.decoded(FfiStyleField::ContainerType).u8_value
    }

    pub(crate) fn has_z_index(self) -> bool {
        self.decoded(FfiStyleField::ZIndex).has_value
    }

    pub(crate) fn z_index(self) -> i32 {
        self.decoded(FfiStyleField::ZIndex).i32_value
    }

    pub(crate) fn text_indent_each_line(self) -> bool {
        self.decoded(FfiStyleField::TextIndent).has_value
    }

    pub(crate) fn text_indent_hanging(self) -> bool {
        self.decoded(FfiStyleField::TextIndent).bool_value
    }

    pub(crate) fn tab_size_is_number(self) -> bool {
        self.decoded(FfiStyleField::TabSize).has_value
    }

    pub(crate) fn tab_size(self) -> CssPixels {
        self.decoded(FfiStyleField::TabSize).css_pixels_value
    }

    pub(crate) fn tab_size_number(self) -> f64 {
        self.decoded(FfiStyleField::TabSize).f64_value
    }

    pub(crate) fn has_perspective(self) -> bool {
        self.decoded(FfiStyleField::Perspective).has_value
    }

    pub(crate) fn perspective(self) -> CssPixels {
        self.decoded(FfiStyleField::Perspective).css_pixels_value
    }
}

macro_rules! size_accessors {
    ($($name:ident => $field:ident,)+) => {
        impl StyleValues {
            $(pub(crate) fn $name(self) -> FfiSizeValue {
                self.decoded(FfiStyleField::$field).size
            })+
        }
    };
}

size_accessors! {
    width => Width,
    height => Height,
    min_width => MinWidth,
    min_height => MinHeight,
    max_width => MaxWidth,
    max_height => MaxHeight,
    margin_top => MarginTop,
    margin_right => MarginRight,
    margin_bottom => MarginBottom,
    margin_left => MarginLeft,
    padding_top => PaddingTop,
    padding_right => PaddingRight,
    padding_bottom => PaddingBottom,
    padding_left => PaddingLeft,
    inset_top => InsetTop,
    inset_right => InsetRight,
    inset_bottom => InsetBottom,
    inset_left => InsetLeft,
    row_gap => RowGap,
    column_gap => ColumnGap,
    column_width => ColumnWidth,
    text_indent => TextIndent,
    x => X,
    y => Y,
}

#[cfg(not(test))]
unsafe extern "C" {
    fn ladybird_layout_release_calc_handle(handle: *const c_void);
    fn ladybird_layout_release_anchor_name_handle(raw: usize);
}

#[cfg(test)]
unsafe fn ladybird_layout_release_calc_handle(_handle: *const c_void) {}
#[cfg(test)]
unsafe fn ladybird_layout_release_anchor_name_handle(_raw: usize) {}

#[derive(Clone, Copy)]
#[repr(C)]
struct CssFfiNumericType {
    has_exponent: [bool; 7],
    exponents: [i32; 7],
    has_percent_hint: bool,
    percent_hint: u8,
    valid: bool,
}

#[derive(Clone, Copy)]
#[repr(C)]
struct CssFfiResolvedCalc {
    resolved: bool,
    value: f64,
    numeric_type: CssFfiNumericType,
}

#[derive(Clone, Copy)]
#[repr(C)]
struct CssFfiCalcResolutionContext {
    basis_kind: u8,
    basis_value: f64,
    basis_unit: u8,
    length_resolution_context: *const c_void,
    callback_context: *mut c_void,
    resolve_non_math_function: unsafe extern "C" fn(*mut c_void, *const c_void) -> *const c_void,
    resolve_channel_keyword: unsafe extern "C" fn(*mut c_void, u8, *mut f64) -> bool,
    random_base_value: unsafe extern "C" fn(*mut c_void, *const c_void, *mut f64) -> bool,
    absolutize_random_sharing: unsafe extern "C" fn(*mut c_void, *const c_void) -> *const c_void,
    resolve_length: unsafe extern "C" fn(*mut c_void, f64, u8, *mut f64) -> bool,
}

#[cfg(not(test))]
unsafe extern "C" {
    fn rust_calc_resolve(
        calculated: *const c_void,
        context: *const CssFfiCalcResolutionContext,
        apply_censoring_and_clamping: bool,
    ) -> CssFfiResolvedCalc;
}

#[cfg(test)]
unsafe fn rust_calc_resolve(
    _calculated: *const c_void,
    _context: *const CssFfiCalcResolutionContext,
    _apply_censoring_and_clamping: bool,
) -> CssFfiResolvedCalc {
    unreachable!("the CSS static library is not linked into layout Rust unit tests")
}

unsafe extern "C" fn no_non_math_function(_context: *mut c_void, _value: *const c_void) -> *const c_void {
    std::ptr::null()
}

unsafe extern "C" fn no_channel_keyword(_context: *mut c_void, _channel: u8, _out: *mut f64) -> bool {
    false
}

unsafe extern "C" fn no_random_base_value(_context: *mut c_void, _value: *const c_void, _out: *mut f64) -> bool {
    false
}

unsafe extern "C" fn no_absolutized_random_sharing(_context: *mut c_void, _value: *const c_void) -> *const c_void {
    std::ptr::null()
}

unsafe extern "C" fn no_fallback_length(_context: *mut c_void, _value: f64, _unit: u8, _out: *mut f64) -> bool {
    false
}

/// Resolves a snapshot calc handle through the CSS crate's existing
/// `rust_calc_resolve` export. This parity hook only supports already
/// absolutized px/percentage calculations, which need no host callbacks.
#[unsafe(no_mangle)]
pub extern "C" fn rust_layout_resolve_calc_handle_for_parity(calc: *const c_void, percentage_basis_raw: i32) -> i32 {
    abort_on_panic(|| {
        // Pinned to C++ LengthUnit::Px; LayoutRustBridge.cpp static-asserts it.
        resolve_calc(calc, CssPixels::from_raw(percentage_basis_raw)).raw_value()
    })
}

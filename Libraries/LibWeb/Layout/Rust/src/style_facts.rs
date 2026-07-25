/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

use crate::abort_on_panic;
use crate::css_pixels::CssPixels;
use crate::display::FfiDisplay;
use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::mem::size_of;
use std::ops::Deref;
use std::sync::OnceLock;

pub const STYLE_GROUP_COUNT: usize = 22;

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
/// data. The per-node decode cache balances it through either the CSS crate or
/// LayoutRustBridge according to `calc_is_rust_retained`.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct FfiSizeValue {
    pub kind: u8,
    pub px: CssPixels,
    pub fraction: f64,
    pub calc: *const c_void,
    /// True when `calc` was retained directly through the CSS Rust crate.
    /// False denotes a handle registered by LayoutRustBridge.
    pub calc_is_rust_retained: bool,
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
            calc_is_rust_retained: false,
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
            calc_is_rust_retained: false,
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
            calc_is_rust_retained: false,
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
            // SAFETY: Every non-null handle was retained either directly by
            // the CSS Rust crate or by LayoutRustBridge, and is released
            // exactly once when the per-pass Rust layout state is dropped.
            unsafe {
                if self.calc_is_rust_retained {
                    css_rust_style_value_release(self.calc);
                } else {
                    ladybird_layout_release_calc_handle(self.calc);
                }
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
    let context = px_calc_resolution_context(percentage_basis);
    // SAFETY: The handle is retained by this state and the context contains
    // the same no-host-callback setup used by the Phase B parity hook.
    let result = unsafe { rust_calc_resolve(calc, &raw const context, true) };
    assert!(result.resolved);
    CssPixels::nearest_value_for(result.value)
}

/// Every computed-value field layout can read. Complex entries have no byte
/// offset; Rust-visible payloads are interpreted in-crate and the remainder
/// are populated by one lazy C++ batch per node.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
// NB: Some variants are only constructed by C++ through the FFI.
#[allow(dead_code)]
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
    BorderCollapse,
    BorderSpacingHorizontal,
    BorderSpacingVertical,
    CaptionSide,
    TableLayout,
    ColumnWidth,
    ColumnCount,
    Containment,
    Visibility,
    LetterSpacing,
    WordSpacing,
    UnicodeBidi,
    TextIndent,
    TabSize,
    GridAutoFlowRow,
    GridAutoFlowDense,
    X,
    Y,
    Count,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum FfiStyleFieldEncoding {
    Lazy,
    U8,
    Bool,
    I32,
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

#[derive(Clone, Copy)]
#[repr(C)]
pub struct FfiStyleSnapshot {
    pub payloads: FfiStylePayloads,
    pub display: FfiDisplay,
}

impl Default for FfiStyleSnapshot {
    fn default() -> Self {
        Self {
            payloads: FfiStylePayloads::default(),
            display: FfiDisplay::block(),
        }
    }
}

impl Default for FfiStylePayloads {
    fn default() -> Self {
        Self {
            groups: [std::ptr::null(); STYLE_GROUP_COUNT],
        }
    }
}

pub(crate) const STYLE_FIELD_COUNT: usize = FfiStyleField::Count as usize;

struct StyleSchema([FfiStyleFieldSchema; STYLE_FIELD_COUNT]);

// SAFETY: Schema entries contain only immutable integers and enum values.
unsafe impl Send for StyleSchema {}
unsafe impl Sync for StyleSchema {}

static STYLE_SCHEMA: OnceLock<StyleSchema> = OnceLock::new();

/// Offsets into the CSS crate's `StyleValueData` representation needed for
/// scalar computed length-percentage values. C++ registers these from the
/// generated CSS header, keeping this crate independent of the full enum.
#[derive(Clone, Copy)]
#[repr(C)]
pub struct FfiStyleValueSchema {
    pub style_value_size: u32,
    pub tag_offset: u32,
    pub length_value_offset: u32,
    pub length_unit_offset: u32,
    pub percentage_value_offset: u32,
    pub length_tag: u8,
    pub percentage_tag: u8,
    pub calculated_tag: u8,
}

struct StyleValueSchema {
    fields: FfiStyleValueSchema,
    /// Indexed by the generated CSS `LengthUnit` value. NaN marks a
    /// non-absolute unit; finite entries are the ratio to CSS px.
    absolute_length_to_px_ratios: Box<[f64]>,
}

// SAFETY: The schema contains only immutable integers and floating-point
// conversion ratios.
unsafe impl Send for StyleValueSchema {}
unsafe impl Sync for StyleValueSchema {}

static STYLE_VALUE_SCHEMA: OnceLock<StyleValueSchema> = OnceLock::new();

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rust_layout_register_style_schema(entries: *const FfiStyleFieldSchema, count: usize) {
    abort_on_panic(|| {
        let entries = unsafe { std::slice::from_raw_parts(entries, count) };
        assert_eq!(entries.len(), STYLE_FIELD_COUNT);
        let mut schema = [entries[0]; STYLE_FIELD_COUNT];
        let mut present = [false; STYLE_FIELD_COUNT];
        for entry in entries {
            let index = entry.field as usize;
            assert!(index < STYLE_FIELD_COUNT);
            assert!((entry.group_index as usize) < STYLE_GROUP_COUNT);
            assert!(!present[index], "duplicate style schema field");
            let width = match entry.encoding {
                FfiStyleFieldEncoding::Lazy => 0,
                FfiStyleFieldEncoding::U8 | FfiStyleFieldEncoding::Bool => 1,
                FfiStyleFieldEncoding::I32 | FfiStyleFieldEncoding::CssPixels => 4,
                FfiStyleFieldEncoding::F64 => 8,
            };
            assert!(entry.offset as usize + width <= entry.group_size as usize);
            schema[index] = *entry;
            present[index] = true;
        }
        assert!(present.iter().all(|present| *present));
        assert!(
            STYLE_SCHEMA.set(StyleSchema(schema)).is_ok(),
            "style schema registered twice"
        );
        crate::ffi_stats::bump(crate::ffi_stats::FfiOp::StyleSchemaRegistration);
    });
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rust_layout_register_style_value_schema(
    fields: FfiStyleValueSchema,
    absolute_length_to_px_ratios: *const f64,
    ratio_count: usize,
) {
    abort_on_panic(|| {
        assert!(fields.tag_offset < fields.style_value_size);
        assert!(fields.length_value_offset as usize + size_of::<f64>() <= fields.style_value_size as usize);
        assert!(fields.length_unit_offset < fields.style_value_size);
        assert!(fields.percentage_value_offset as usize + size_of::<f64>() <= fields.style_value_size as usize);
        assert!(ratio_count > 0);
        let ratios = unsafe { std::slice::from_raw_parts(absolute_length_to_px_ratios, ratio_count) };
        assert!(
            STYLE_VALUE_SCHEMA
                .set(StyleValueSchema {
                    fields,
                    absolute_length_to_px_ratios: ratios.into(),
                })
                .is_ok(),
            "style-value schema registered twice"
        );
    });
}

fn style_schema() -> &'static StyleSchema {
    STYLE_SCHEMA.get().expect("style schema used before registration")
}

/// Every field whose immutable payload still requires C++ interpretation.
/// One callback populates the whole value on the first residual read for a
/// node. Anchor inset entries are populated only when the corresponding
/// Rust-visible anchor handle is non-null.
#[derive(Clone, Copy)]
#[repr(C)]
pub struct FfiResidualStyleValues {
    pub anchor_inset_top: FfiSizeValue,
    pub anchor_inset_right: FfiSizeValue,
    pub anchor_inset_bottom: FfiSizeValue,
    pub anchor_inset_left: FfiSizeValue,
    pub position_anchor_has_value: bool,
    /// A transferred Utf16FlyString reference, when non-zero.
    pub position_anchor_retained_name: usize,
    pub vertical_align_is_keyword: bool,
    pub vertical_align_keyword: u8,
    pub vertical_align_value: FfiSizeValue,
    pub first_available_font: *const c_void,
    pub font_ascent: f32,
    pub font_descent: f32,
    pub font_x_height: f32,
    pub box_sizing_for_aspect_ratio: u8,
    pub column_width: FfiSizeValue,
    pub column_count_has_value: bool,
    pub column_count: i32,
    pub containment_bits: u8,
    pub text_indent: FfiSizeValue,
    pub text_indent_each_line: bool,
    pub text_indent_hanging: bool,
    pub tab_size_is_number: bool,
    pub tab_size: CssPixels,
    pub tab_size_number: f64,
}

impl Default for FfiResidualStyleValues {
    fn default() -> Self {
        Self {
            anchor_inset_top: FfiSizeValue::auto_value(),
            anchor_inset_right: FfiSizeValue::auto_value(),
            anchor_inset_bottom: FfiSizeValue::auto_value(),
            anchor_inset_left: FfiSizeValue::auto_value(),
            position_anchor_has_value: false,
            position_anchor_retained_name: 0,
            vertical_align_is_keyword: false,
            vertical_align_keyword: 0,
            vertical_align_value: FfiSizeValue::auto_value(),
            first_available_font: std::ptr::null(),
            font_ascent: 0.0,
            font_descent: 0.0,
            font_x_height: 0.0,
            box_sizing_for_aspect_ratio: 0,
            column_width: FfiSizeValue::auto_value(),
            column_count_has_value: false,
            column_count: 0,
            containment_bits: 0,
            text_indent: FfiSizeValue::auto_value(),
            text_indent_each_line: false,
            text_indent_hanging: false,
            tab_size_is_number: false,
            tab_size: CssPixels::default(),
            tab_size_number: 0.0,
        }
    }
}

impl FfiResidualStyleValues {
    fn release_handles(self) {
        self.anchor_inset_top.release_calc_handle();
        self.anchor_inset_right.release_calc_handle();
        self.anchor_inset_bottom.release_calc_handle();
        self.anchor_inset_left.release_calc_handle();
        self.vertical_align_value.release_calc_handle();
        self.column_width.release_calc_handle();
        self.text_indent.release_calc_handle();
        if self.position_anchor_retained_name != 0 {
            unsafe {
                ladybird_layout_release_anchor_name_handle(self.position_anchor_retained_name);
            }
        }
    }
}

pub type FfiDecodeResidualStyleCallback =
    unsafe extern "C" fn(*mut c_void, *const FfiStylePayloads) -> FfiResidualStyleValues;

#[derive(Clone, Copy)]
struct StyleReader {
    payloads: FfiStylePayloads,
    schema: &'static StyleSchema,
}

impl StyleReader {
    fn new(payloads: FfiStylePayloads, schema: &'static StyleSchema) -> Self {
        Self { payloads, schema }
    }

    #[inline]
    fn address(&self, field: FfiStyleField, encoding: FfiStyleFieldEncoding) -> *const u8 {
        let schema = &self.schema.0[field as usize];
        debug_assert_eq!(schema.encoding, encoding);
        let payload = self.payloads.groups[schema.group_index as usize];
        debug_assert!(!payload.is_null());
        unsafe { (payload as *const u8).add(schema.offset as usize) }
    }

    #[inline]
    fn u8(&self, field: FfiStyleField) -> u8 {
        unsafe { self.address(field, FfiStyleFieldEncoding::U8).read_unaligned() }
    }

    #[inline]
    fn bool(&self, field: FfiStyleField) -> bool {
        let value = unsafe { self.address(field, FfiStyleFieldEncoding::Bool).read_unaligned() };
        debug_assert!(value <= 1);
        value != 0
    }

    #[inline]
    fn i32(&self, field: FfiStyleField) -> i32 {
        unsafe { (self.address(field, FfiStyleFieldEncoding::I32) as *const i32).read_unaligned() }
    }

    #[inline]
    fn f64(&self, field: FfiStyleField) -> f64 {
        unsafe { (self.address(field, FfiStyleFieldEncoding::F64) as *const f64).read_unaligned() }
    }

    #[inline]
    fn css_pixels(&self, field: FfiStyleField) -> CssPixels {
        let raw = unsafe { (self.address(field, FfiStyleFieldEncoding::CssPixels) as *const i32).read_unaligned() };
        CssPixels::from_raw(raw)
    }

    #[inline]
    fn payload(&self, field: FfiStyleField) -> *const c_void {
        let schema = &self.schema.0[field as usize];
        debug_assert_eq!(schema.encoding, FfiStyleFieldEncoding::Lazy);
        let payload = self.payloads.groups[schema.group_index as usize];
        debug_assert!(!payload.is_null());
        payload
    }

    #[inline]
    fn typed_payload<T>(&self, field: FfiStyleField) -> *const T {
        let schema = &self.schema.0[field as usize];
        debug_assert_eq!(schema.group_size as usize, size_of::<T>());
        self.payload(field).cast()
    }
}

const SIZE_FIELD_COUNT: usize = 25;

#[inline]
fn size_field_index(field: FfiStyleField) -> usize {
    match field {
        FfiStyleField::Width => 0,
        FfiStyleField::Height => 1,
        FfiStyleField::MinWidth => 2,
        FfiStyleField::MinHeight => 3,
        FfiStyleField::MaxWidth => 4,
        FfiStyleField::MaxHeight => 5,
        FfiStyleField::MarginTop => 6,
        FfiStyleField::MarginRight => 7,
        FfiStyleField::MarginBottom => 8,
        FfiStyleField::MarginLeft => 9,
        FfiStyleField::PaddingTop => 10,
        FfiStyleField::PaddingRight => 11,
        FfiStyleField::PaddingBottom => 12,
        FfiStyleField::PaddingLeft => 13,
        FfiStyleField::InsetTop => 14,
        FfiStyleField::InsetRight => 15,
        FfiStyleField::InsetBottom => 16,
        FfiStyleField::InsetLeft => 17,
        FfiStyleField::FlexBasis => 18,
        FfiStyleField::RowGap => 19,
        FfiStyleField::ColumnGap => 20,
        FfiStyleField::ColumnWidth => 21,
        FfiStyleField::TextIndent => 22,
        FfiStyleField::X => 23,
        FfiStyleField::Y => 24,
        _ => unreachable!(),
    }
}

pub(crate) struct StyleDecodeCache {
    reader: StyleReader,
    sizes: [Cell<FfiSizeValue>; SIZE_FIELD_COUNT],
    size_presence: Cell<u32>,
    residual: RefCell<Option<FfiResidualStyleValues>>,
}

impl StyleDecodeCache {
    fn new(reader: StyleReader) -> Self {
        Self {
            reader,
            sizes: std::array::from_fn(|_| Cell::new(FfiSizeValue::auto_value())),
            size_presence: Cell::new(0),
            residual: RefCell::new(None),
        }
    }

    #[inline]
    fn cached_size(&self, field: FfiStyleField) -> Option<FfiSizeValue> {
        let index = size_field_index(field);
        (self.size_presence.get() & (1 << index) != 0).then(|| self.sizes[index].get())
    }

    #[inline]
    fn cache_size(&self, field: FfiStyleField, value: FfiSizeValue) -> FfiSizeValue {
        let index = size_field_index(field);
        debug_assert_eq!(self.size_presence.get() & (1 << index), 0);
        self.sizes[index].set(value);
        self.size_presence.set(self.size_presence.get() | (1 << index));
        value
    }

    fn cached_residual(&self) -> Option<FfiResidualStyleValues> {
        *self.residual.borrow()
    }

    fn cache_residual(&self, value: FfiResidualStyleValues) {
        let mut residual = self.residual.borrow_mut();
        assert!(residual.is_none());
        *residual = Some(value);
    }

    pub(crate) fn replace_size(&self, field: FfiStyleField, value: FfiSizeValue) {
        let index = size_field_index(field);
        let presence = self.size_presence.get();
        if presence & (1 << index) != 0 {
            self.sizes[index].replace(value).release_calc_handle();
        } else {
            self.sizes[index].set(value);
            self.size_presence.set(presence | (1 << index));
        }
    }
}

impl Drop for StyleDecodeCache {
    fn drop(&mut self) {
        let presence = self.size_presence.get();
        for (index, value) in self.sizes.iter().enumerate() {
            if presence & (1 << index) != 0 {
                value.get().release_calc_handle();
            }
        }
        if let Some(residual) = *self.residual.get_mut() {
            residual.release_handles();
        }
    }
}

fn decode_length_percentage(handle: &crate::display::computed_value_types::ComputedStyleValueHandle) -> FfiSizeValue {
    let pointer = handle.pointer;
    assert!(!pointer.is_null());
    let schema = STYLE_VALUE_SCHEMA
        .get()
        .expect("style-value schema used before registration");
    let fields = schema.fields;
    let bytes = pointer.cast::<u8>();
    let tag = unsafe { bytes.add(fields.tag_offset as usize).read_unaligned() };
    if tag == fields.length_tag {
        let value = unsafe {
            bytes
                .add(fields.length_value_offset as usize)
                .cast::<f64>()
                .read_unaligned()
        };
        let unit = unsafe { bytes.add(fields.length_unit_offset as usize).read_unaligned() };
        let ratio = *schema
            .absolute_length_to_px_ratios
            .get(unit as usize)
            .expect("length unit outside registered schema");
        assert!(ratio.is_finite(), "computed length is not absolute");
        return FfiSizeValue::px_value(CssPixels::nearest_value_for(value * ratio));
    }
    if tag == fields.percentage_tag {
        let value = unsafe {
            bytes
                .add(fields.percentage_value_offset as usize)
                .cast::<f64>()
                .read_unaligned()
        };
        return FfiSizeValue {
            kind: FfiSizeKind::Percentage as u8,
            px: CssPixels::default(),
            // Match Percentage::as_fraction(); the multiplication order is
            // observable for some f64 inputs.
            fraction: value * 0.01,
            calc: std::ptr::null(),
            calc_is_rust_retained: false,
            contains_percentage: true,
            contains_anchor_function: false,
        };
    }
    assert_eq!(tag, fields.calculated_tag);
    let root = unsafe { rust_calc_root_from_calculated(pointer) };
    assert!(!root.is_null());
    let contains_percentage = unsafe { css_rust_calc_node_contains_percentage(root) };
    let contains_anchor_function = unsafe { css_rust_calc_contains_anchor(pointer) };
    let retained = unsafe { css_rust_style_value_retain(pointer) };
    assert!(!retained.is_null());
    FfiSizeValue {
        kind: FfiSizeKind::Calc as u8,
        px: CssPixels::default(),
        fraction: 0.0,
        calc: retained,
        calc_is_rust_retained: true,
        contains_percentage,
        contains_anchor_function,
    }
}

fn decode_computed_size(value: &crate::display::computed_value_types::ComputedSize) -> FfiSizeValue {
    use crate::display::computed_value_types::ComputedSizeKind;

    match value.kind {
        ComputedSizeKind::Auto => FfiSizeValue::auto_value(),
        ComputedSizeKind::Calculated => decode_length_percentage(&value.value),
        ComputedSizeKind::Length => {
            let result = decode_length_percentage(&value.value);
            assert_eq!(result.kind(), FfiSizeKind::Px);
            result
        }
        ComputedSizeKind::Percentage => {
            let result = decode_length_percentage(&value.value);
            assert_eq!(result.kind(), FfiSizeKind::Percentage);
            result
        }
        ComputedSizeKind::MinContent => FfiSizeValue {
            kind: FfiSizeKind::MinContent as u8,
            ..FfiSizeValue::auto_value()
        },
        ComputedSizeKind::MaxContent => FfiSizeValue {
            kind: FfiSizeKind::MaxContent as u8,
            ..FfiSizeValue::auto_value()
        },
        ComputedSizeKind::FitContent => {
            if value.value.pointer.is_null() {
                FfiSizeValue {
                    kind: FfiSizeKind::FitContent as u8,
                    ..FfiSizeValue::auto_value()
                }
            } else {
                let mut result = decode_length_percentage(&value.value);
                result.kind = FfiSizeKind::FitContent as u8;
                result
            }
        }
        ComputedSizeKind::None => FfiSizeValue {
            kind: FfiSizeKind::None_ as u8,
            ..FfiSizeValue::auto_value()
        },
    }
}

fn decode_length_percentage_or_auto(
    value: &crate::display::computed_value_types::ComputedLengthPercentageOrAuto,
) -> FfiSizeValue {
    if value.is_auto {
        FfiSizeValue::auto_value()
    } else {
        decode_length_percentage(&value.value)
    }
}

#[derive(Clone, Copy)]
pub(crate) struct DecodedStyleScalars {
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
    pub border_collapse: u8,
    pub border_spacing_horizontal: CssPixels,
    pub border_spacing_vertical: CssPixels,
    pub caption_side: u8,
    pub table_layout: u8,
    pub visibility: u8,
    pub letter_spacing: CssPixels,
    pub word_spacing: CssPixels,
    pub unicode_bidi: u8,
    pub grid_auto_flow_row: bool,
    pub grid_auto_flow_dense: bool,
}

pub(crate) struct StyleDecodeValues {
    scalars: DecodedStyleScalars,
    cache: StyleDecodeCache,
}

impl StyleDecodeValues {
    pub(crate) fn new(payloads: FfiStylePayloads, display: FfiDisplay) -> Self {
        let reader = StyleReader::new(payloads, style_schema());
        Self {
            scalars: DecodedStyleScalars::decode(&reader, display),
            cache: StyleDecodeCache::new(reader),
        }
    }

    pub(crate) fn replace_size(&self, field: FfiStyleField, value: FfiSizeValue) {
        self.cache.replace_size(field, value);
    }
}

/// A thin Rust-only view over a node's immutable computed-value group
/// payloads.
///
/// Plain fields are decoded once into `DecodedStyleScalars`. Rust-owned
/// complex payloads are interpreted in-crate; the first read of any genuinely
/// C++-owned field fetches the one residual batch and caches it for the node.
#[derive(Clone, Copy)]
pub(crate) struct StyleValues<'a> {
    scalars: &'a DecodedStyleScalars,
    cache: &'a StyleDecodeCache,
    callback_context: *mut c_void,
    decode_residual_callback: FfiDecodeResidualStyleCallback,
    vertical_align_override: u16,
}

impl DecodedStyleScalars {
    fn decode(reader: &StyleReader, display: FfiDisplay) -> Self {
        Self {
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
            border_collapse: reader.u8(FfiStyleField::BorderCollapse),
            border_spacing_horizontal: reader.css_pixels(FfiStyleField::BorderSpacingHorizontal),
            border_spacing_vertical: reader.css_pixels(FfiStyleField::BorderSpacingVertical),
            caption_side: reader.u8(FfiStyleField::CaptionSide),
            table_layout: reader.u8(FfiStyleField::TableLayout),
            visibility: reader.u8(FfiStyleField::Visibility),
            letter_spacing: reader.css_pixels(FfiStyleField::LetterSpacing),
            word_spacing: reader.css_pixels(FfiStyleField::WordSpacing),
            unicode_bidi: reader.u8(FfiStyleField::UnicodeBidi),
            grid_auto_flow_row: reader.bool(FfiStyleField::GridAutoFlowRow),
            grid_auto_flow_dense: reader.bool(FfiStyleField::GridAutoFlowDense),
        }
    }
}

impl<'a> StyleValues<'a> {
    #[inline]
    pub(crate) fn new(
        values: &'a StyleDecodeValues,
        callback_context: *mut c_void,
        decode_residual_callback: FfiDecodeResidualStyleCallback,
    ) -> Self {
        Self {
            scalars: &values.scalars,
            cache: &values.cache,
            callback_context,
            decode_residual_callback,
            vertical_align_override: u16::MAX,
        }
    }

    fn residual(self) -> FfiResidualStyleValues {
        if let Some(value) = self.cache.cached_residual() {
            return value;
        }
        crate::ffi_stats::bump(crate::ffi_stats::FfiOp::StyleResidualBatchDecode);
        // SAFETY: The payload snapshot and bridge callback remain live for
        // the synchronous layout pass.
        let value =
            unsafe { (self.decode_residual_callback)(self.callback_context, &raw const self.cache.reader.payloads) };
        if let Some(existing) = self.cache.cached_residual() {
            value.release_handles();
            existing
        } else {
            self.cache.cache_residual(value);
            value
        }
    }

    fn surround(self, field: FfiStyleField) -> *const crate::display::computed_value_types::SurroundValues {
        self.cache.reader.typed_payload(field)
    }

    fn inset_has_anchor(self, field: FfiStyleField) -> bool {
        let values = unsafe { &*self.surround(field) };
        let handle = match field {
            FfiStyleField::InsetTop => &values.top_anchor_inset,
            FfiStyleField::InsetRight => &values.right_anchor_inset,
            FfiStyleField::InsetBottom => &values.bottom_anchor_inset,
            FfiStyleField::InsetLeft => &values.left_anchor_inset,
            _ => unreachable!(),
        };
        !handle.pointer.is_null()
    }

    fn direct_size(self, field: FfiStyleField) -> FfiSizeValue {
        use crate::display::computed_value_types::{AlignmentValues, SVGResetValues, SizingValues};

        match field {
            FfiStyleField::Width
            | FfiStyleField::Height
            | FfiStyleField::MinWidth
            | FfiStyleField::MinHeight
            | FfiStyleField::MaxWidth
            | FfiStyleField::MaxHeight => {
                let values = unsafe { &*self.cache.reader.typed_payload::<SizingValues>(field) };
                decode_computed_size(match field {
                    FfiStyleField::Width => &values.width,
                    FfiStyleField::Height => &values.height,
                    FfiStyleField::MinWidth => &values.min_width,
                    FfiStyleField::MinHeight => &values.min_height,
                    FfiStyleField::MaxWidth => &values.max_width,
                    FfiStyleField::MaxHeight => &values.max_height,
                    _ => unreachable!(),
                })
            }
            FfiStyleField::MarginTop
            | FfiStyleField::MarginRight
            | FfiStyleField::MarginBottom
            | FfiStyleField::MarginLeft
            | FfiStyleField::PaddingTop
            | FfiStyleField::PaddingRight
            | FfiStyleField::PaddingBottom
            | FfiStyleField::PaddingLeft
            | FfiStyleField::InsetTop
            | FfiStyleField::InsetRight
            | FfiStyleField::InsetBottom
            | FfiStyleField::InsetLeft => {
                let values = unsafe { &*self.surround(field) };
                decode_length_percentage_or_auto(match field {
                    FfiStyleField::MarginTop => &values.margin.top,
                    FfiStyleField::MarginRight => &values.margin.right,
                    FfiStyleField::MarginBottom => &values.margin.bottom,
                    FfiStyleField::MarginLeft => &values.margin.left,
                    FfiStyleField::PaddingTop => &values.padding.top,
                    FfiStyleField::PaddingRight => &values.padding.right,
                    FfiStyleField::PaddingBottom => &values.padding.bottom,
                    FfiStyleField::PaddingLeft => &values.padding.left,
                    FfiStyleField::InsetTop => &values.inset.top,
                    FfiStyleField::InsetRight => &values.inset.right,
                    FfiStyleField::InsetBottom => &values.inset.bottom,
                    FfiStyleField::InsetLeft => &values.inset.left,
                    _ => unreachable!(),
                })
            }
            FfiStyleField::FlexBasis => {
                let values = unsafe { &*self.cache.reader.typed_payload::<AlignmentValues>(field) };
                if values.flex_basis.is_content {
                    FfiSizeValue::auto_value()
                } else {
                    decode_computed_size(&values.flex_basis.size)
                }
            }
            FfiStyleField::RowGap | FfiStyleField::ColumnGap => {
                let values = unsafe { &*self.cache.reader.typed_payload::<AlignmentValues>(field) };
                let gap = if field == FfiStyleField::RowGap {
                    &values.row_gap
                } else {
                    &values.column_gap
                };
                if gap.is_normal {
                    FfiSizeValue::auto_value()
                } else {
                    decode_length_percentage(&gap.value)
                }
            }
            FfiStyleField::X | FfiStyleField::Y => {
                let values = unsafe { &*self.cache.reader.typed_payload::<SVGResetValues>(field) };
                decode_length_percentage(if field == FfiStyleField::X {
                    &values.x
                } else {
                    &values.y
                })
            }
            _ => unreachable!(),
        }
    }

    fn size_value(self, field: FfiStyleField) -> FfiSizeValue {
        if let Some(value) = self.cache.cached_size(field) {
            return value;
        }
        let residual = match field {
            FfiStyleField::InsetTop
            | FfiStyleField::InsetRight
            | FfiStyleField::InsetBottom
            | FfiStyleField::InsetLeft
                if self.inset_has_anchor(field) =>
            {
                let values = self.residual();
                Some(match field {
                    FfiStyleField::InsetTop => values.anchor_inset_top,
                    FfiStyleField::InsetRight => values.anchor_inset_right,
                    FfiStyleField::InsetBottom => values.anchor_inset_bottom,
                    FfiStyleField::InsetLeft => values.anchor_inset_left,
                    _ => unreachable!(),
                })
            }
            FfiStyleField::ColumnWidth => Some(self.residual().column_width),
            FfiStyleField::TextIndent => Some(self.residual().text_indent),
            _ => None,
        };
        residual.unwrap_or_else(|| {
            let value = self.direct_size(field);
            self.cache.cache_size(field, value)
        })
    }

    pub(crate) fn with_vertical_align_keyword(mut self, keyword: u8) -> Self {
        self.vertical_align_override = keyword as u16;
        self
    }

    pub(crate) fn vertical_align_is_keyword(self) -> bool {
        self.vertical_align_override != u16::MAX || self.residual().vertical_align_is_keyword
    }

    pub(crate) fn vertical_align_keyword(self) -> u8 {
        if self.vertical_align_override != u16::MAX {
            self.vertical_align_override as u8
        } else {
            self.residual().vertical_align_keyword
        }
    }

    pub(crate) fn vertical_align_value(self) -> FfiSizeValue {
        self.residual().vertical_align_value
    }

    pub(crate) fn has_position_anchor(self) -> bool {
        self.residual().position_anchor_has_value
    }

    pub(crate) fn position_anchor_name(self) -> usize {
        self.residual().position_anchor_retained_name
    }

    pub(crate) fn first_available_font(self) -> *const c_void {
        self.residual().first_available_font
    }

    pub(crate) fn font_ascent(self) -> f32 {
        self.residual().font_ascent
    }

    pub(crate) fn font_descent(self) -> f32 {
        self.residual().font_descent
    }

    pub(crate) fn font_x_height(self) -> f32 {
        self.residual().font_x_height
    }

    pub(crate) fn box_sizing_for_aspect_ratio(self) -> u8 {
        self.residual().box_sizing_for_aspect_ratio
    }

    pub(crate) fn flex_basis_is_content(self) -> bool {
        let values = unsafe {
            &*self
                .cache
                .reader
                .typed_payload::<crate::display::computed_value_types::AlignmentValues>(FfiStyleField::FlexBasis)
        };
        values.flex_basis.is_content
    }

    pub(crate) fn flex_basis(self) -> FfiSizeValue {
        self.size_value(FfiStyleField::FlexBasis)
    }

    pub(crate) fn has_column_count(self) -> bool {
        self.residual().column_count_has_value
    }

    pub(crate) fn column_count(self) -> i32 {
        self.residual().column_count
    }

    pub(crate) fn containment_bits(self) -> u8 {
        self.residual().containment_bits
    }

    pub(crate) fn text_indent_each_line(self) -> bool {
        self.residual().text_indent_each_line
    }

    pub(crate) fn text_indent_hanging(self) -> bool {
        self.residual().text_indent_hanging
    }

    pub(crate) fn tab_size_is_number(self) -> bool {
        self.residual().tab_size_is_number
    }

    pub(crate) fn tab_size(self) -> CssPixels {
        self.residual().tab_size
    }

    pub(crate) fn tab_size_number(self) -> f64 {
        self.residual().tab_size_number
    }
}

impl Deref for StyleValues<'_> {
    type Target = DecodedStyleScalars;

    fn deref(&self) -> &Self::Target {
        self.scalars
    }
}

macro_rules! size_accessors {
    ($($name:ident => $field:ident,)+) => {
        impl StyleValues<'_> {
            $(pub(crate) fn $name(self) -> FfiSizeValue {
                self.size_value(FfiStyleField::$field)
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
    pub(crate) fn ladybird_layout_release_anchor_name_handle(raw: usize);
    #[link_name = "rust_calc_contains_anchor"]
    fn css_rust_calc_contains_anchor(calculated: *const c_void) -> bool;
    #[link_name = "rust_calc_node_contains_percentage"]
    fn css_rust_calc_node_contains_percentage(node: *const c_void) -> bool;
    #[link_name = "rust_style_value_release"]
    fn css_rust_style_value_release(value: *const c_void);
    #[link_name = "rust_style_value_retain"]
    fn css_rust_style_value_retain(value: *const c_void) -> *const c_void;
}

#[cfg(test)]
unsafe fn ladybird_layout_release_calc_handle(_handle: *const c_void) {}
#[cfg(test)]
pub(crate) unsafe fn ladybird_layout_release_anchor_name_handle(_raw: usize) {}
#[cfg(test)]
unsafe fn css_rust_calc_contains_anchor(_calculated: *const c_void) -> bool {
    false
}
#[cfg(test)]
unsafe fn css_rust_calc_node_contains_percentage(_node: *const c_void) -> bool {
    false
}
#[cfg(test)]
unsafe fn css_rust_style_value_release(_value: *const c_void) {}
#[cfg(test)]
unsafe fn css_rust_style_value_retain(value: *const c_void) -> *const c_void {
    value
}

#[derive(Clone, Copy)]
#[repr(C)]
pub(crate) struct CssFfiNumericType {
    pub(crate) has_exponent: [bool; 7],
    pub(crate) exponents: [i32; 7],
    pub(crate) has_percent_hint: bool,
    pub(crate) percent_hint: u8,
    pub(crate) valid: bool,
}

#[derive(Clone, Copy)]
#[repr(C)]
pub(crate) struct CssFfiResolvedCalc {
    pub(crate) resolved: bool,
    pub(crate) value: f64,
    pub(crate) numeric_type: CssFfiNumericType,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
// NB: Some variants are only constructed by the CSS crate through the FFI.
#[allow(dead_code)]
pub(crate) enum CssFfiCalcExternalResolutionKind {
    NonMathFunction,
    Channel,
    RandomSharing,
    Length,
}

#[derive(Clone, Copy)]
#[repr(C)]
pub(crate) struct CssFfiCalcExternalResolution {
    pub(crate) kind: CssFfiCalcExternalResolutionKind,
    pub(crate) source: *const c_void,
    pub(crate) input_value: f64,
    pub(crate) unit_or_channel: u8,
    pub(crate) has_number: bool,
    pub(crate) number: f64,
    pub(crate) resolved_node: *const c_void,
    pub(crate) resolved_style_value: *const c_void,
}

#[derive(Clone, Copy)]
#[repr(C)]
pub(crate) struct CssFfiCalcExternalResolutions {
    pub(crate) resolutions: *mut CssFfiCalcExternalResolution,
    pub(crate) resolution_count: usize,
    pub(crate) storage: *mut c_void,
}

#[derive(Clone, Copy)]
#[repr(C)]
pub(crate) struct CssFfiCalcResolutionContext {
    pub(crate) basis_kind: u8,
    pub(crate) basis_value: f64,
    pub(crate) basis_unit: u8,
    pub(crate) length_resolution_context: *const c_void,
    pub(crate) external_resolutions: *const CssFfiCalcExternalResolution,
    pub(crate) external_resolution_count: usize,
}

#[cfg(not(test))]
unsafe extern "C" {
    fn rust_calc_root_from_calculated(calculated: *const c_void) -> *const c_void;
    fn rust_calc_external_resolutions(
        root: *const c_void,
        basis_kind: u8,
        basis_value: f64,
        basis_unit: u8,
    ) -> CssFfiCalcExternalResolutions;
    fn rust_calc_external_resolutions_release(storage: *mut c_void);
    pub(crate) fn rust_calc_resolve(
        calculated: *const c_void,
        context: *const CssFfiCalcResolutionContext,
        apply_censoring_and_clamping: bool,
    ) -> CssFfiResolvedCalc;
}

#[cfg(test)]
pub(crate) unsafe fn rust_calc_resolve(
    _calculated: *const c_void,
    _context: *const CssFfiCalcResolutionContext,
    _apply_censoring_and_clamping: bool,
) -> CssFfiResolvedCalc {
    CssFfiResolvedCalc {
        resolved: false,
        value: 0.0,
        numeric_type: CssFfiNumericType {
            has_exponent: [false; 7],
            exponents: [0; 7],
            has_percent_hint: false,
            percent_hint: 0,
            valid: false,
        },
    }
}

#[cfg(test)]
unsafe fn rust_calc_root_from_calculated(_calculated: *const c_void) -> *const c_void {
    std::ptr::null()
}

#[cfg(test)]
unsafe fn rust_calc_external_resolutions(
    _root: *const c_void,
    _basis_kind: u8,
    _basis_value: f64,
    _basis_unit: u8,
) -> CssFfiCalcExternalResolutions {
    CssFfiCalcExternalResolutions {
        resolutions: std::ptr::null_mut(),
        resolution_count: 0,
        storage: std::ptr::null_mut(),
    }
}

#[cfg(test)]
unsafe fn rust_calc_external_resolutions_release(_storage: *mut c_void) {}

// Pinned to C++ LengthUnit::Px; LayoutRustBridge.cpp static-asserts it.
pub(crate) const LENGTH_UNIT_PX: u8 = 29;

pub(crate) fn px_calc_resolution_context(percentage_basis: CssPixels) -> CssFfiCalcResolutionContext {
    CssFfiCalcResolutionContext {
        basis_kind: 3,
        basis_value: percentage_basis.to_double(),
        basis_unit: LENGTH_UNIT_PX,
        length_resolution_context: std::ptr::null(),
        external_resolutions: std::ptr::null(),
        external_resolution_count: 0,
    }
}

pub(crate) unsafe fn resolve_calc_with_external_resolutions(
    calculated: *const c_void,
    percentage_basis: CssPixels,
    callback_context: *mut c_void,
    resolve_non_math_function: Option<unsafe extern "C" fn(*mut c_void, *const c_void) -> *const c_void>,
) -> CssFfiResolvedCalc {
    let mut context = px_calc_resolution_context(percentage_basis);
    let root = unsafe { rust_calc_root_from_calculated(calculated) };
    let external =
        unsafe { rust_calc_external_resolutions(root, context.basis_kind, context.basis_value, context.basis_unit) };
    context.external_resolutions = external.resolutions;
    context.external_resolution_count = external.resolution_count;
    if let Some(resolve_non_math_function) = resolve_non_math_function
        && external.resolution_count > 0
    {
        for resolution in unsafe { std::slice::from_raw_parts_mut(external.resolutions, external.resolution_count) } {
            if resolution.kind == CssFfiCalcExternalResolutionKind::NonMathFunction {
                resolution.resolved_node = unsafe { resolve_non_math_function(callback_context, resolution.source) };
            }
        }
    }
    let result = unsafe { rust_calc_resolve(calculated, &raw const context, true) };
    unsafe {
        rust_calc_external_resolutions_release(external.storage);
    }
    result
}

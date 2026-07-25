/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

use crate::css_pixels::CssPixels;
use crate::geometry::{AvailableSize, AvailableSpace, FfiContainingBlockConstraints, FfiLayoutInput};
use crate::layout_state::state_mut;
use crate::style_facts::StyleValues;
use crate::used_values::{FfiCssPixelPoint, UsedValuesCore};
use std::ffi::c_void;

use super::{FfiLayoutFcCallbacks, FormattingContextInstance};

#[derive(Clone, Copy, Debug, Default, PartialEq)]
#[repr(C)]
pub struct FfiFloatPoint {
    pub x: f32,
    pub y: f32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
#[repr(C)]
pub struct FfiFloatRect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
#[repr(C)]
pub struct FfiAffineTransform {
    pub a: f32,
    pub b: f32,
    pub c: f32,
    pub d: f32,
    pub e: f32,
    pub f: f32,
}

impl Default for FfiAffineTransform {
    fn default() -> Self {
        Self {
            a: 1.0,
            b: 0.0,
            c: 0.0,
            d: 1.0,
            e: 0.0,
            f: 0.0,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
#[repr(C)]
pub struct FfiSvgComputedTransforms {
    pub viewbox_transform: FfiAffineTransform,
    pub svg_transform: FfiAffineTransform,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
#[repr(C)]
pub struct FfiSvgViewBox {
    pub min_x: f64,
    pub min_y: f64,
    pub width: f64,
    pub height: f64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
#[repr(C)]
pub struct FfiSvgNumberPercentage {
    pub value: f32,
    pub is_percentage: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
#[repr(C)]
pub struct FfiSvgElementFacts {
    pub is_document_element: bool,
    pub document_is_decoded_svg: bool,
    pub is_fit_to_view_box: bool,
    pub is_svg_svg_element: bool,
    pub is_container_element: bool,
    pub is_graphics_box: bool,
    pub is_geometry_box: bool,
    pub is_text_box: bool,
    pub is_text_path_box: bool,
    pub is_image_box: bool,
    pub is_foreign_object_box: bool,
    pub is_mask_box: bool,
    pub is_clip_box: bool,
    pub is_pattern_box: bool,
    pub has_active_view_box: bool,
    pub active_view_box: FfiSvgViewBox,
    pub has_own_view_box: bool,
    pub preserve_aspect_ratio_align: u8,
    pub preserve_aspect_ratio_meet_or_slice: u8,
    pub element_transform: FfiAffineTransform,
    pub visible_stroke_width: f32,
    pub content_units: u8,
    pub pattern_units: u8,
    pub pattern_width: FfiSvgNumberPercentage,
    pub pattern_height: FfiSvgNumberPercentage,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
#[repr(C)]
pub struct FfiSvgPathRequest {
    pub viewport_width: CssPixels,
    pub viewport_height: CssPixels,
    pub current_text_position: FfiFloatPoint,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
#[repr(C)]
pub struct FfiSvgPathResult {
    pub path_handle: *mut c_void,
    pub bounding_box: FfiFloatRect,
    pub text_position_for_children: FfiFloatPoint,
}

pub(crate) const PRESERVE_ASPECT_RATIO_NONE: u8 = 0;
const PRESERVE_ASPECT_RATIO_X_MIN_Y_MIN: u8 = 1;
const PRESERVE_ASPECT_RATIO_X_MID_Y_MIN: u8 = 2;
const PRESERVE_ASPECT_RATIO_X_MAX_Y_MIN: u8 = 3;
const PRESERVE_ASPECT_RATIO_X_MIN_Y_MID: u8 = 4;
pub(crate) const PRESERVE_ASPECT_RATIO_X_MID_Y_MID: u8 = 5;
const PRESERVE_ASPECT_RATIO_X_MAX_Y_MID: u8 = 6;
const PRESERVE_ASPECT_RATIO_X_MIN_Y_MAX: u8 = 7;
pub(crate) const PRESERVE_ASPECT_RATIO_X_MID_Y_MAX: u8 = 8;
pub(crate) const PRESERVE_ASPECT_RATIO_X_MAX_Y_MAX: u8 = 9;
pub(crate) const MEET_OR_SLICE_MEET: u8 = 0;
pub(crate) const MEET_OR_SLICE_SLICE: u8 = 1;
const SVG_UNITS_OBJECT_BOUNDING_BOX: u8 = 0;
const SVG_UNITS_USER_SPACE_ON_USE: u8 = 1;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct CssPixelRect {
    x: CssPixels,
    y: CssPixels,
    width: CssPixels,
    height: CssPixels,
}

impl CssPixelRect {
    fn inflate(&mut self, width: CssPixels, height: CssPixels) {
        self.x -= width / 2;
        self.width += width;
        self.y -= height / 2;
        self.height += height;
    }
}

impl FfiAffineTransform {
    fn is_identity(self) -> bool {
        self.a == 1.0 && self.b == 0.0 && self.c == 0.0 && self.d == 1.0 && self.e == 0.0 && self.f == 0.0
    }

    fn is_identity_or_translation(self) -> bool {
        self.a == 1.0 && self.b == 0.0 && self.c == 0.0 && self.d == 1.0
    }

    pub(crate) fn multiply(&mut self, other: Self) {
        if other.is_identity() {
            return;
        }
        let this = *self;
        self.a = other.a * this.a + other.b * this.c;
        self.b = other.a * this.b + other.b * this.d;
        self.c = other.c * this.a + other.d * this.c;
        self.d = other.c * this.b + other.d * this.d;
        self.e = other.e * this.a + other.f * this.c + this.e;
        self.f = other.e * this.b + other.f * this.d + this.f;
    }

    pub(crate) fn translated(mut self, x: f32, y: f32) -> Self {
        if self.is_identity_or_translation() {
            self.e += x;
            self.f += y;
            return self;
        }
        self.e += x * self.a + y * self.c;
        self.f += x * self.b + y * self.d;
        self
    }

    pub(crate) fn scaled(mut self, x: f32, y: f32) -> Self {
        self.a *= x;
        self.b *= x;
        self.c *= y;
        self.d *= y;
        self
    }

    fn map_point(self, point: FfiFloatPoint) -> FfiFloatPoint {
        FfiFloatPoint {
            x: self.a * point.x + self.c * point.y + self.e,
            y: self.b * point.x + self.d * point.y + self.f,
        }
    }

    pub(crate) fn map_rect(self, rect: FfiFloatRect) -> FfiFloatRect {
        if self.is_identity() {
            return rect;
        }
        if self.is_identity_or_translation() {
            return FfiFloatRect {
                x: rect.x + self.e,
                y: rect.y + self.f,
                ..rect
            };
        }

        let top_left = self.map_point(FfiFloatPoint { x: rect.x, y: rect.y });
        let top_right = self.map_point(FfiFloatPoint {
            x: rect.x + rect.width,
            y: rect.y,
        });
        let bottom_right = self.map_point(FfiFloatPoint {
            x: rect.x + rect.width,
            y: rect.y + rect.height,
        });
        let bottom_left = self.map_point(FfiFloatPoint {
            x: rect.x,
            y: rect.y + rect.height,
        });
        let left = top_left.x.min(top_right.x).min(bottom_right.x.min(bottom_left.x));
        let top = top_left.y.min(top_right.y).min(bottom_right.y.min(bottom_left.y));
        let right = top_left.x.max(top_right.x).max(bottom_right.x.max(bottom_left.x));
        let bottom = top_left.y.max(top_right.y).max(bottom_right.y.max(bottom_left.y));
        FfiFloatRect {
            x: left,
            y: top,
            width: right - left,
            height: bottom - top,
        }
    }

    fn x_scale(self) -> f32 {
        (self.a * self.a + self.b * self.b).sqrt()
    }
}

fn css_pixels_from_f32(value: f32) -> CssPixels {
    if value.is_nan() {
        return CssPixels::default();
    }
    let scaled = value * 64.0;
    if scaled >= i32::MAX as f32 {
        return CssPixels::from_raw(i32::MAX);
    }
    if scaled <= i32::MIN as f32 {
        return CssPixels::from_raw(i32::MIN);
    }
    CssPixels::from_raw(scaled.round_ties_even() as i32)
}

fn float_rect_to_css_pixels(rect: FfiFloatRect) -> CssPixelRect {
    CssPixelRect {
        x: css_pixels_from_f32(rect.x),
        y: css_pixels_from_f32(rect.y),
        width: css_pixels_from_f32(rect.width),
        height: css_pixels_from_f32(rect.height),
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct ViewBoxTransform {
    pub(crate) offset: FfiCssPixelPoint,
    pub(crate) scale_factor_x: f64,
    pub(crate) scale_factor_y: f64,
}

// https://svgwg.org/svg2-draft/coords.html#PreserveAspectRatioAttribute
pub(crate) fn scale_and_align_viewbox_content(
    align: u8,
    meet_or_slice: u8,
    view_box: FfiSvgViewBox,
    viewbox_scale_x: f64,
    viewbox_scale_y: f64,
    content_size: (CssPixels, CssPixels),
    has_definite_inline_size: bool,
) -> ViewBoxTransform {
    if align == PRESERVE_ASPECT_RATIO_NONE {
        // Do not force uniform scaling. Scale the graphic content of the given element non-uniformly
        // if necessary such that the element's bounding box exactly matches the SVG viewport rectangle.
        return ViewBoxTransform {
            scale_factor_x: viewbox_scale_x,
            scale_factor_y: viewbox_scale_y,
            ..Default::default()
        };
    }

    let scale = match meet_or_slice {
        // meet (the default) - Scale the graphic such that:
        // - aspect ratio is preserved
        // - the entire ‘viewBox’ is visible within the SVG viewport
        // - the ‘viewBox’ is scaled up as much as possible, while still meeting the other criteria
        MEET_OR_SLICE_MEET => viewbox_scale_x.min(viewbox_scale_y),
        // slice - Scale the graphic such that:
        // aspect ratio is preserved
        // the entire SVG viewport is covered by the ‘viewBox’
        // the ‘viewBox’ is scaled down as much as possible, while still meeting the other criteria
        MEET_OR_SLICE_SLICE => viewbox_scale_x.max(viewbox_scale_y),
        _ => panic!("invalid preserveAspectRatio meet-or-slice value"),
    };
    let mut result = ViewBoxTransform {
        scale_factor_x: scale,
        scale_factor_y: scale,
        ..Default::default()
    };

    // Handle X alignment:
    if has_definite_inline_size {
        match align {
            // Align the <min-x> of the element's ‘viewBox’ with the smallest X value of the SVG viewport.
            PRESERVE_ASPECT_RATIO_X_MIN_Y_MIN
            | PRESERVE_ASPECT_RATIO_X_MIN_Y_MID
            | PRESERVE_ASPECT_RATIO_X_MIN_Y_MAX => {}
            // Align the midpoint X value of the element's ‘viewBox’ with the midpoint X value of the SVG viewport.
            PRESERVE_ASPECT_RATIO_X_MID_Y_MIN
            | PRESERVE_ASPECT_RATIO_X_MID_Y_MID
            | PRESERVE_ASPECT_RATIO_X_MID_Y_MAX => {
                result.offset.x =
                    (content_size.0 - CssPixels::nearest_value_for(view_box.width * result.scale_factor_x)) / 2;
            }
            // Align the <min-x>+<width> of the element's ‘viewBox’ with the maximum X value of the SVG viewport.
            PRESERVE_ASPECT_RATIO_X_MAX_Y_MIN
            | PRESERVE_ASPECT_RATIO_X_MAX_Y_MID
            | PRESERVE_ASPECT_RATIO_X_MAX_Y_MAX => {
                result.offset.x = content_size.0 - CssPixels::nearest_value_for(view_box.width * result.scale_factor_x);
            }
            _ => panic!("invalid preserveAspectRatio alignment value"),
        }
    }

    // This intentionally checks inline-size definiteness, matching the C++
    // algorithm being ported.
    if has_definite_inline_size {
        match align {
            // Align the <min-y> of the element's ‘viewBox’ with the smallest Y value of the SVG viewport.
            PRESERVE_ASPECT_RATIO_X_MIN_Y_MIN
            | PRESERVE_ASPECT_RATIO_X_MID_Y_MIN
            | PRESERVE_ASPECT_RATIO_X_MAX_Y_MIN => {}
            // Align the midpoint Y value of the element's ‘viewBox’ with the midpoint Y value of the SVG viewport.
            PRESERVE_ASPECT_RATIO_X_MIN_Y_MID
            | PRESERVE_ASPECT_RATIO_X_MID_Y_MID
            | PRESERVE_ASPECT_RATIO_X_MAX_Y_MID => {
                result.offset.y =
                    (content_size.1 - CssPixels::nearest_value_for(view_box.height * result.scale_factor_y)) / 2;
            }
            // Align the <min-y>+<height> of the element's ‘viewBox’ with the maximum Y value of the SVG viewport.
            PRESERVE_ASPECT_RATIO_X_MIN_Y_MAX
            | PRESERVE_ASPECT_RATIO_X_MID_Y_MAX
            | PRESERVE_ASPECT_RATIO_X_MAX_Y_MAX => {
                result.offset.y =
                    content_size.1 - CssPixels::nearest_value_for(view_box.height * result.scale_factor_y);
            }
            _ => panic!("invalid preserveAspectRatio alignment value"),
        }
    }

    result
}

pub(super) struct SvgFormattingContext {
    state: *mut c_void,
    box_: *mut c_void,
    rust_context_handle: *mut c_void,
    layout_mode: u8,
    callbacks: FfiLayoutFcCallbacks,
    parent_viewbox_transform: FfiAffineTransform,
    parent_svg_transform: Option<FfiAffineTransform>,
    available_space: Option<AvailableSpace>,
    quirks_mode_percentage_basis_block_size: Option<CssPixels>,
    current_viewbox_transform: FfiAffineTransform,
    viewport_width: CssPixels,
    viewport_height: CssPixels,
    current_text_position: FfiFloatPoint,
}

impl SvgFormattingContext {
    pub(super) fn new(
        state: *mut c_void,
        box_: *mut c_void,
        rust_context_handle: *mut c_void,
        layout_mode: u8,
        callbacks: FfiLayoutFcCallbacks,
    ) -> Self {
        Self::new_nested(
            state,
            box_,
            rust_context_handle,
            layout_mode,
            callbacks,
            FfiAffineTransform::default(),
            None,
        )
    }

    fn new_nested(
        state: *mut c_void,
        box_: *mut c_void,
        rust_context_handle: *mut c_void,
        layout_mode: u8,
        callbacks: FfiLayoutFcCallbacks,
        parent_viewbox_transform: FfiAffineTransform,
        parent_svg_transform: Option<FfiAffineTransform>,
    ) -> Self {
        Self {
            state,
            box_,
            rust_context_handle,
            layout_mode,
            callbacks,
            parent_viewbox_transform,
            parent_svg_transform,
            available_space: None,
            quirks_mode_percentage_basis_block_size: None,
            current_viewbox_transform: FfiAffineTransform::default(),
            viewport_width: CssPixels::default(),
            viewport_height: CssPixels::default(),
            current_text_position: FfiFloatPoint::default(),
        }
    }

    fn first_child(&self, node: *mut c_void) -> *mut c_void {
        // SAFETY: SVG traversal is synchronous and all layout nodes remain
        // owned by the C++ layout tree for the pass.
        unsafe { (self.callbacks.navigation.first_child)(self.callbacks.navigation.context, node) }
    }

    fn next_sibling(&self, node: *mut c_void) -> *mut c_void {
        // SAFETY: See first_child().
        unsafe { (self.callbacks.navigation.next_sibling)(self.callbacks.navigation.context, node) }
    }

    fn parent(&self, node: *mut c_void) -> *mut c_void {
        // SAFETY: See first_child().
        unsafe { (self.callbacks.navigation.parent)(self.callbacks.navigation.context, node) }
    }

    fn svg_facts(&self, node: *mut c_void) -> FfiSvgElementFacts {
        // SAFETY: The callback snapshots plain data from a live node and
        // returns no borrowed storage.
        unsafe { (self.callbacks.build_svg_facts)(self.callbacks.context, node) }
    }

    fn style_facts(&self, node: *mut c_void) -> StyleValues {
        state_mut(self.state).style_facts(&self.callbacks, node)
    }

    fn used_values(&self, node: *mut c_void) -> *mut UsedValuesCore {
        state_mut(self.state).used_values(&self.callbacks, node)
    }

    fn create_used_values(&self, node: *mut c_void) -> *mut UsedValuesCore {
        // SVG descendants deliberately carry no percentage basis.
        // SVG layout resolves percentages against the SVG viewport, not a CSS containing
        // block, so boxes inside the SVG subtree carry no percentage basis.
        let used = state_mut(self.state).create_used_values(
            &self.callbacks,
            node,
            crate::geometry::FfiContainingBlockConstraints::default(),
        );
        assert!(!used.is_null());
        used
    }

    fn computed_transforms(&self, node: *mut c_void) -> Option<FfiSvgComputedTransforms> {
        let index = state_mut(self.state).box_facts(&self.callbacks, node).layout_index;
        if let Some(transforms) = state_mut(self.state)
            .used_values_rare_data(index)
            .and_then(|data| data.computed_svg_transforms)
        {
            return Some(transforms);
        }
        let mut transforms = FfiSvgComputedTransforms::default();
        // SAFETY: `transforms` is writable POD storage and the callback reads
        // only paintable geometry retained by the C++ layout node.
        let has_transforms = unsafe {
            (self.callbacks.read_paintable_svg_transforms)(self.callbacks.context, node, &raw mut transforms)
        };
        has_transforms.then_some(transforms)
    }

    fn set_computed_transforms(&self, node: *mut c_void, transforms: FfiSvgComputedTransforms) {
        state_mut(self.state)
            .used_values_rare_data_for_node_mut(&self.callbacks, node)
            .computed_svg_transforms = Some(transforms);
    }

    fn place_child(&self, node: *mut c_void, x: CssPixels, y: CssPixels) {
        super::place_child(self.state, &self.callbacks, node, FfiCssPixelPoint { x, y });
    }

    fn for_each_child(&self, node: *mut c_void, mut callback: impl FnMut(*mut c_void)) {
        let mut child = self.first_child(node);
        while !child.is_null() {
            let next = self.next_sibling(child);
            callback(child);
            child = next;
        }
    }

    fn first_child_matching(
        &self,
        node: *mut c_void,
        predicate: impl Fn(FfiSvgElementFacts) -> bool,
    ) -> Option<*mut c_void> {
        let mut result = None;
        self.for_each_child(node, |child| {
            if result.is_none() && predicate(self.svg_facts(child)) {
                result = Some(child);
            }
        });
        result
    }

    pub(super) fn run(&mut self, input: FfiLayoutInput) {
        // NOTE: SVG doesn't have a "formatting context" in the spec, but this is the most
        //       obvious way to drive SVG layout in our engine at the moment.
        let facts = self.svg_facts(self.box_);
        let used_pointer = self.used_values(self.box_);
        // SAFETY: The state owns the used-values core for the full pass.
        let used = unsafe { &mut *used_pointer };

        if facts.is_document_element && !facts.document_is_decoded_svg && !used.has_content_offset {
            // Overwrite the content width/height with the styled node width/height (from <svg width height ...>)
            //
            // NOTE: If a height had not been provided by the svg element, it was set to the height of the container
            let style = self.style_facts(self.box_);
            if style.width().is_length() {
                used.set_content_inline_size(style.width().to_px(CssPixels::default()));
            }
            if style.height().is_length() {
                used.set_content_block_size(style.height().to_px(CssPixels::default()));
            }
            // FIXME: In SVG 2, length can also be a percentage. We'll need to support that.
        }

        // NOTE: We consider all SVG root elements to have definite size in both axes.
        //       I'm not sure if this is good or bad, but our viewport transform logic depends on it.
        used.has_definite_inline_size = true;
        used.has_definite_block_size = true;

        let mut active_view_box = facts.has_active_view_box.then_some(facts.active_view_box);
        // https://svgwg.org/svg2-draft/coords.html#ViewBoxAttribute
        if let Some(view_box) = active_view_box {
            if view_box.width < 0.0 || view_box.height < 0.0 {
                // A negative value for <width> or <height> is an error and invalidates the ‘viewBox’ attribute.
                active_view_box = None;
            } else if view_box.width == 0.0 || view_box.height == 0.0 {
                // A value of zero disables rendering of the element.
                return;
            }
        }

        if state_mut(self.state)
            .box_facts(&self.callbacks, self.box_)
            .is_svg_svg_box
            && self.computed_transforms(self.box_).is_none()
            && let Some(mut svg_transform) = self.parent_svg_transform
        {
            svg_transform.multiply(facts.element_transform);
            self.set_computed_transforms(
                self.box_,
                FfiSvgComputedTransforms {
                    viewbox_transform: self.parent_viewbox_transform,
                    svg_transform,
                },
            );
        }

        self.current_viewbox_transform = self.parent_viewbox_transform;
        if let Some(view_box) = active_view_box {
            // FIXME: This should allow just one of width or height to be specified.
            // E.g. We should be able to layout <svg width="100%"> where height is unspecified/auto.
            let scale_width = if used.has_definite_inline_size() {
                used.content_inline_size.to_double() / view_box.width
            } else {
                1.0
            };
            let scale_height = if used.has_definite_block_size() {
                used.content_block_size.to_double() / view_box.height
            } else {
                1.0
            };
            // The initial value for preserveAspectRatio is xMidYMid meet.
            // This allows mask and clipPath elements to be scaled in the x and y directions independently to match the target size.
            let transform = scale_and_align_viewbox_content(
                facts.preserve_aspect_ratio_align,
                facts.preserve_aspect_ratio_meet_or_slice,
                view_box,
                scale_width,
                scale_height,
                (used.content_inline_size, used.content_block_size),
                used.has_definite_inline_size(),
            );
            self.current_viewbox_transform = self
                .current_viewbox_transform
                .translated(
                    transform.offset.x.raw_value() as f32 / 64.0,
                    transform.offset.y.raw_value() as f32 / 64.0,
                )
                .scaled(transform.scale_factor_x as f32, transform.scale_factor_y as f32)
                .translated(-view_box.min_x as f32, -view_box.min_y as f32);
        }

        // NOTE: Calculate viewport dimensions BEFORE scaling the content by m_parent_viewbox_transform.
        // For userSpaceOnUse clips (which have no viewBox), we need the unscaled content dimensions,
        // not the final pixel dimensions. Otherwise, nested clips compound the scale incorrectly.
        self.viewport_width = active_view_box.map_or_else(
            || {
                if used.has_definite_inline_size() {
                    used.content_inline_size
                } else {
                    CssPixels::default()
                }
            },
            |view_box| CssPixels::nearest_value_for(view_box.width),
        );
        self.viewport_height = active_view_box.map_or_else(
            || {
                if used.has_definite_block_size() {
                    used.content_block_size
                } else {
                    CssPixels::default()
                }
            },
            |view_box| CssPixels::nearest_value_for(view_box.height),
        );
        self.available_space = Some(input.available_space);
        self.quirks_mode_percentage_basis_block_size = input
            .containing_block_constraints
            .has_quirks_mode_percentage_basis_block_size
            .then_some(
                input
                    .containing_block_constraints
                    .quirks_mode_percentage_basis_block_size,
            );

        let mut svg_transform_for_children = self.parent_svg_transform.unwrap_or_default();
        if let Some(transforms) = self.computed_transforms(self.box_) {
            svg_transform_for_children = transforms.svg_transform;
        }

        let mut child = self.first_child(self.box_);
        while !child.is_null() {
            let next = self.next_sibling(child);
            if state_mut(self.state).box_facts(&self.callbacks, child).is_box {
                self.layout_svg_element(child, input, svg_transform_for_children);
            }
            child = next;
        }
    }

    fn layout_svg_element(
        &mut self,
        child: *mut c_void,
        input: FfiLayoutInput,
        parent_svg_transform: FfiAffineTransform,
    ) {
        let facts = self.svg_facts(child);
        if facts.is_fit_to_view_box {
            self.layout_nested_viewport(child, parent_svg_transform);
        } else if facts.is_foreign_object_box
            && state_mut(self.state)
                .box_facts(&self.callbacks, child)
                .is_block_container
        {
            let child_used_pointer = self.create_used_values(child);
            let style = self.style_facts(child);
            let available_space = self.available_space.unwrap();
            let rect = CssPixelRect {
                x: style.x().to_px(available_space.inline_size.to_px_or_zero()),
                y: style.y().to_px(available_space.block_size.to_px_or_zero()),
                width: style.width().to_px(available_space.inline_size.to_px_or_zero()),
                height: style.height().to_px(available_space.block_size.to_px_or_zero()),
            };

            let mut svg_transform = parent_svg_transform;
            svg_transform.multiply(facts.element_transform);
            self.set_computed_transforms(
                child,
                FfiSvgComputedTransforms {
                    viewbox_transform: self.current_viewbox_transform,
                    svg_transform,
                },
            );
            let mut to_css_pixels_transform = self.current_viewbox_transform;
            to_css_pixels_transform.multiply(svg_transform);
            let transformed_rect = float_rect_to_css_pixels(to_css_pixels_transform.map_rect(FfiFloatRect {
                x: rect.x.raw_value() as f32 / 64.0,
                y: rect.y.raw_value() as f32 / 64.0,
                width: rect.width.raw_value() as f32 / 64.0,
                height: rect.height.raw_value() as f32 / 64.0,
            }));
            // SAFETY: The child core was just created and remains live.
            let child_used = unsafe { &mut *child_used_pointer };
            child_used.set_content_inline_size(transformed_rect.width);
            child_used.set_content_block_size(transformed_rect.height);

            let child_input = FfiLayoutInput {
                available_space: AvailableSpace {
                    inline_size: AvailableSize::definite(child_used.content_inline_size),
                    block_size: AvailableSize::definite(child_used.content_block_size),
                },
                containing_block_constraints: FfiContainingBlockConstraints {
                    has_quirks_mode_percentage_basis_block_size: self.quirks_mode_percentage_basis_block_size.is_some(),
                    quirks_mode_percentage_basis_block_size: self
                        .quirks_mode_percentage_basis_block_size
                        .unwrap_or_default(),
                    ..Default::default()
                },
                has_content_box_position_in_bfc_root: false,
                content_box_position_in_bfc_root: FfiCssPixelPoint::default(),
                has_table_grid_min_border_box_block_size: false,
                table_grid_min_border_box_block_size: CssPixels::default(),
            };
            assert!(
                super::layout_inside_child(self.rust_context_handle, child, self.layout_mode, child_input, false,)
                    .is_some()
            );

            // Masks and clips may use this offset for objectBoundingBox units.
            self.place_child(child, transformed_rect.x, transformed_rect.y);
            if let Some(mask) = self.first_child_matching(child, |facts| facts.is_mask_box) {
                self.layout_mask_or_clip(mask);
            }
            if let Some(clip) = self.first_child_matching(child, |facts| facts.is_clip_box) {
                self.layout_mask_or_clip(clip);
            }
            // The old SVG formatter's stack-local BFC dies at the end of this
            // branch; discard the Rust-owned equivalent at the same point.
            super::discard_child_layout(self.rust_context_handle, child);
        } else if facts.is_graphics_box {
            self.layout_graphics_element(child, input, parent_svg_transform);
        }
    }

    fn layout_nested_viewport(&mut self, viewport: *mut c_void, parent_svg_transform: FfiAffineTransform) {
        // Layout for a nested SVG viewport.
        // https://svgwg.org/svg2-draft/coords.html#EstablishingANewSVGViewport.
        let used_pointer = self.create_used_values(viewport);
        let style = self.style_facts(viewport);
        let facts = self.svg_facts(viewport);
        let nested_viewport_x = style.x().to_px(self.viewport_width);
        let nested_viewport_y = style.y().to_px(self.viewport_height);
        // The value auto for width and height on the ‘svg’ element is treated as 100%.
        // https://svgwg.org/svg2-draft/geometry.html#Sizing
        let nested_viewport_width = if style.width().is_auto() {
            self.viewport_width
        } else {
            style.width().to_px(self.viewport_width)
        };
        let nested_viewport_height = if style.height().is_auto() {
            self.viewport_height
        } else {
            style.height().to_px(self.viewport_height)
        };

        if state_mut(self.state)
            .box_facts(&self.callbacks, viewport)
            .is_svg_svg_box
            && self.computed_transforms(viewport).is_none()
        {
            // https://svgwg.org/svg2-draft/coords.html#EstablishingANewSVGViewport
            // Including an svg element inside SVG content creates a new SVG viewport into which all contained graphics are
            // drawn; this implicitly establishes both a new viewport coordinate system and a new user coordinate system.
            let mut svg_transform = parent_svg_transform;
            svg_transform.multiply(facts.element_transform);
            self.set_computed_transforms(
                viewport,
                FfiSvgComputedTransforms {
                    viewbox_transform: self.current_viewbox_transform,
                    svg_transform,
                },
            );
        }

        let mut content_offset = FfiCssPixelPoint {
            x: nested_viewport_x,
            y: nested_viewport_y,
        };
        let mut content_inline_size = nested_viewport_width;
        let mut content_block_size = nested_viewport_height;
        let mut parent_viewbox_transform = self.current_viewbox_transform;
        if facts.is_svg_svg_element && facts.has_own_view_box {
            // FIXME: Avoid converting SVG box to floats.
            let mapped_rect = self.current_viewbox_transform.map_rect(FfiFloatRect {
                x: nested_viewport_x.raw_value() as f32 / 64.0,
                y: nested_viewport_y.raw_value() as f32 / 64.0,
                width: nested_viewport_width.raw_value() as f32 / 64.0,
                height: nested_viewport_height.raw_value() as f32 / 64.0,
            });
            content_offset = FfiCssPixelPoint {
                x: css_pixels_from_f32(mapped_rect.x),
                y: css_pixels_from_f32(mapped_rect.y),
            };
            content_inline_size = css_pixels_from_f32(mapped_rect.width);
            content_block_size = css_pixels_from_f32(mapped_rect.height);
            parent_viewbox_transform = FfiAffineTransform::default();
        }

        // SAFETY: The viewport core was just created and remains live.
        let used = unsafe { &mut *used_pointer };
        used.set_content_inline_size(content_inline_size);
        used.set_content_block_size(content_block_size);
        used.has_definite_inline_size = true;
        used.has_definite_block_size = true;
        if facts.has_own_view_box {
            self.place_child(viewport, content_offset.x, content_offset.y);
        }

        let mut nested_context = Self::new_nested(
            self.state,
            viewport,
            self.rust_context_handle,
            self.layout_mode,
            self.callbacks,
            parent_viewbox_transform,
            Some(parent_svg_transform),
        );
        nested_context.run(FfiLayoutInput {
            available_space: self.available_space.unwrap(),
            containing_block_constraints: FfiContainingBlockConstraints {
                has_quirks_mode_percentage_basis_block_size: self.quirks_mode_percentage_basis_block_size.is_some(),
                quirks_mode_percentage_basis_block_size: self
                    .quirks_mode_percentage_basis_block_size
                    .unwrap_or_default(),
                ..Default::default()
            },
            has_content_box_position_in_bfc_root: false,
            content_box_position_in_bfc_root: FfiCssPixelPoint::default(),
            has_table_grid_min_border_box_block_size: false,
            table_grid_min_border_box_block_size: CssPixels::default(),
        });

        if !facts.has_own_view_box {
            let mapped_rect = self.current_viewbox_transform.map_rect(FfiFloatRect {
                x: nested_viewport_x.raw_value() as f32 / 64.0,
                y: nested_viewport_y.raw_value() as f32 / 64.0,
                width: nested_viewport_width.raw_value() as f32 / 64.0,
                height: nested_viewport_height.raw_value() as f32 / 64.0,
            });
            // Reborrow after recursive layout to avoid keeping a Rust
            // reference across callbacks.
            let used = unsafe { &mut *used_pointer };
            used.set_content_inline_size(css_pixels_from_f32(mapped_rect.width));
            used.set_content_block_size(css_pixels_from_f32(mapped_rect.height));
            self.place_child(
                viewport,
                css_pixels_from_f32(mapped_rect.x),
                css_pixels_from_f32(mapped_rect.y),
            );
        }
    }

    fn layout_graphics_element(
        &mut self,
        graphics_box: *mut c_void,
        input: FfiLayoutInput,
        parent_svg_transform: FfiAffineTransform,
    ) {
        self.create_used_values(graphics_box);
        let facts = self.svg_facts(graphics_box);
        let mut svg_transform = parent_svg_transform;
        svg_transform.multiply(facts.element_transform);
        self.set_computed_transforms(
            graphics_box,
            FfiSvgComputedTransforms {
                viewbox_transform: self.current_viewbox_transform,
                svg_transform,
            },
        );

        // https://svgwg.org/svg2-draft/struct.html#GroupsOverview
        // container element
        // An element which can have graphics elements and other container elements as child elements.
        // Specifically: ‘a’, ‘clipPath’, ‘defs’, ‘g’, ‘marker’, ‘mask’, ‘pattern’, ‘svg’, ‘switch’ and ‘symbol’.
        if facts.is_container_element {
            // https://svgwg.org/svg2-draft/struct.html#Groups
            // 5.2. Grouping: the ‘g’ element
            // The ‘g’ element is a container element for grouping together related graphics elements.
            self.layout_container_element(graphics_box, input, svg_transform);
        } else if facts.is_image_box {
            self.layout_image_element(graphics_box);
        } else {
            // Assume this is a path-like element.
            self.layout_path_like_element(graphics_box, input);
        }

        if let Some(mask) = self.first_child_matching(graphics_box, |facts| facts.is_mask_box) {
            self.layout_mask_or_clip(mask);
        }
        if let Some(clip) = self.first_child_matching(graphics_box, |facts| facts.is_clip_box) {
            self.layout_mask_or_clip(clip);
        }
        let mut child = self.first_child(graphics_box);
        while !child.is_null() {
            let next = self.next_sibling(child);
            if self.svg_facts(child).is_pattern_box {
                self.layout_mask_or_clip(child);
            }
            child = next;
        }
    }

    fn layout_path_like_element(&mut self, graphics_box: *mut c_void, input: FfiLayoutInput) {
        let transforms = self
            .computed_transforms(graphics_box)
            .expect("SVG graphics box must have computed transforms");
        let mut to_css_pixels_transform = self.current_viewbox_transform;
        to_css_pixels_transform.multiply(transforms.svg_transform);

        let facts = self.svg_facts(graphics_box);
        // SAFETY: The callback computes geometry synchronously and transfers
        // one retained path handle into the result.
        let result = unsafe {
            (self.callbacks.compute_svg_path)(
                self.callbacks.context,
                graphics_box,
                FfiSvgPathRequest {
                    viewport_width: self.viewport_width,
                    viewport_height: self.viewport_height,
                    current_text_position: self.current_text_position,
                },
            )
        };
        assert!(!result.path_handle.is_null());

        if facts.is_text_box {
            self.current_text_position = result.text_position_for_children;
            // <text> and <tspan> elements can contain more text elements.
            let mut child = self.first_child(graphics_box);
            while !child.is_null() {
                let next = self.next_sibling(child);
                let child_facts = self.svg_facts(child);
                if child_facts.is_text_box || child_facts.is_text_path_box {
                    self.layout_graphics_element(child, input, transforms.svg_transform);
                }
                child = next;
            }
        }

        self.current_text_position = FfiFloatPoint {
            x: result.bounding_box.x + result.bounding_box.width,
            y: result.bounding_box.y + result.bounding_box.height,
        };
        let mut transformed_bounding_box =
            float_rect_to_css_pixels(to_css_pixels_transform.map_rect(result.bounding_box));
        // Stroke increases the path's size by stroke_width/2 per side.
        let stroke_width = css_pixels_from_f32(facts.visible_stroke_width * self.current_viewbox_transform.x_scale());
        transformed_bounding_box.inflate(stroke_width, stroke_width);

        let used_pointer = self.used_values(graphics_box);
        // SAFETY: This node's used values were created by the caller.
        let used = unsafe { &mut *used_pointer };
        used.set_content_inline_size(transformed_bounding_box.width);
        used.set_content_block_size(transformed_bounding_box.height);
        self.place_child(graphics_box, transformed_bounding_box.x, transformed_bounding_box.y);
        used.has_definite_inline_size = true;
        used.has_definite_block_size = true;
        state_mut(self.state)
            .used_values_rare_data_for_node_mut(&self.callbacks, graphics_box)
            .computed_svg_path = Some(crate::layout_state::RetainedLayoutHandle::new(
            result.path_handle,
            self.callbacks.context,
            self.callbacks.release_svg_path,
        ));
    }

    fn layout_image_element(&self, image_box: *mut c_void) {
        let transforms = self
            .computed_transforms(image_box)
            .expect("SVG image box must have computed transforms");
        let mut to_css_pixels_transform = self.current_viewbox_transform;
        to_css_pixels_transform.multiply(transforms.svg_transform);
        // SAFETY: The callback returns a POD bounding box for the live image
        // node and requested viewport.
        let source = unsafe {
            (self.callbacks.svg_image_bounding_box)(
                self.callbacks.context,
                image_box,
                self.viewport_width,
                self.viewport_height,
            )
        };
        let bounding_box = float_rect_to_css_pixels(to_css_pixels_transform.map_rect(source));
        let used_pointer = self.used_values(image_box);
        // SAFETY: This node's used values were created by the caller.
        let used = unsafe { &mut *used_pointer };
        used.set_content_inline_size(bounding_box.width);
        used.set_content_block_size(bounding_box.height);
        self.place_child(image_box, bounding_box.x, bounding_box.y);
        used.has_definite_inline_size = true;
        used.has_definite_block_size = true;
    }

    fn layout_mask_or_clip(&mut self, resource: *mut c_void) {
        let facts = self.svg_facts(resource);
        assert!(facts.is_mask_box || facts.is_clip_box || facts.is_pattern_box);
        // FIXME: Somehow limit <clipPath> contents to: shape elements, <text>, and <use>.
        let used_pointer = self.create_used_values(resource);
        let mut parent_viewbox_transform = self.current_viewbox_transform;

        if facts.is_pattern_box && facts.has_active_view_box {
            if facts.pattern_units == SVG_UNITS_USER_SPACE_ON_USE {
                let width = if facts.pattern_width.is_percentage {
                    facts.pattern_width.value * (self.viewport_width.raw_value() as f32 / 64.0)
                } else {
                    facts.pattern_width.value
                };
                let height = if facts.pattern_height.is_percentage {
                    facts.pattern_height.value * (self.viewport_height.raw_value() as f32 / 64.0)
                } else {
                    facts.pattern_height.value
                };
                let used = unsafe { &mut *used_pointer };
                used.set_content_inline_size(css_pixels_from_f32(width));
                used.set_content_block_size(css_pixels_from_f32(height));
            } else {
                let parent = self.parent(resource);
                assert!(!parent.is_null());
                let parent_used_pointer = self.used_values(parent);
                let parent_used = unsafe { &*parent_used_pointer };
                let used = unsafe { &mut *used_pointer };
                used.set_content_inline_size(CssPixels::nearest_value_for(
                    facts.pattern_width.value as f64 * parent_used.content_inline_size.to_double(),
                ));
                used.set_content_block_size(CssPixels::nearest_value_for(
                    facts.pattern_height.value as f64 * parent_used.content_block_size.to_double(),
                ));
                parent_viewbox_transform = FfiAffineTransform::default().translated(
                    parent_used.content_offset.x.raw_value() as f32 / 64.0,
                    parent_used.content_offset.y.raw_value() as f32 / 64.0,
                );
            }
        } else if facts.content_units == SVG_UNITS_OBJECT_BOUNDING_BOX {
            let parent = self.parent(resource);
            assert!(!parent.is_null());
            let parent_used_pointer = self.used_values(parent);
            let parent_used = unsafe { &*parent_used_pointer };
            let used = unsafe { &mut *used_pointer };
            used.set_content_inline_size(parent_used.content_inline_size);
            used.set_content_block_size(parent_used.content_block_size);
            // https://svgwg.org/svg2-draft/pservers.html#PatternElementPatternContentUnitsAttribute
            parent_viewbox_transform = FfiAffineTransform::default().translated(
                parent_used.content_offset.x.raw_value() as f32 / 64.0,
                parent_used.content_offset.y.raw_value() as f32 / 64.0,
            );
            if facts.is_pattern_box {
                parent_viewbox_transform = parent_viewbox_transform.scaled(
                    parent_used.content_inline_size.raw_value() as f32 / 64.0,
                    parent_used.content_block_size.raw_value() as f32 / 64.0,
                );
            }
        } else {
            let used = unsafe { &mut *used_pointer };
            used.set_content_inline_size(self.viewport_width);
            used.set_content_block_size(self.viewport_height);
        }

        let used = unsafe { &mut *used_pointer };
        used.has_definite_inline_size = true;
        used.has_definite_block_size = true;
        // Pretend masks/clips are a viewport so we can scale the contents depending on the `contentUnits`.
        let mut nested_context = Self::new_nested(
            self.state,
            resource,
            self.rust_context_handle,
            self.layout_mode,
            self.callbacks,
            parent_viewbox_transform,
            Some(FfiAffineTransform::default()),
        );
        nested_context.run(FfiLayoutInput {
            available_space: self.available_space.unwrap(),
            containing_block_constraints: FfiContainingBlockConstraints {
                has_quirks_mode_percentage_basis_block_size: self.quirks_mode_percentage_basis_block_size.is_some(),
                quirks_mode_percentage_basis_block_size: self
                    .quirks_mode_percentage_basis_block_size
                    .unwrap_or_default(),
                ..Default::default()
            },
            has_content_box_position_in_bfc_root: false,
            content_box_position_in_bfc_root: FfiCssPixelPoint::default(),
            has_table_grid_min_border_box_block_size: false,
            table_grid_min_border_box_block_size: CssPixels::default(),
        });

        let used = unsafe { &mut *used_pointer };
        let mapped_rect = parent_viewbox_transform.map_rect(FfiFloatRect {
            x: 0.0,
            y: 0.0,
            width: used.content_inline_size.raw_value() as f32 / 64.0,
            height: used.content_block_size.raw_value() as f32 / 64.0,
        });
        used.set_content_inline_size(css_pixels_from_f32(mapped_rect.width));
        used.set_content_block_size(css_pixels_from_f32(mapped_rect.height));
        self.place_child(
            resource,
            css_pixels_from_f32(mapped_rect.x),
            css_pixels_from_f32(mapped_rect.y),
        );
    }

    fn layout_container_element(
        &mut self,
        container: *mut c_void,
        input: FfiLayoutInput,
        container_svg_transform: FfiAffineTransform,
    ) {
        let mut has_points = false;
        let mut min_x = CssPixels::default();
        let mut min_y = CssPixels::default();
        let mut max_x = CssPixels::default();
        let mut max_y = CssPixels::default();
        let mut child = self.first_child(container);
        while !child.is_null() {
            let next = self.next_sibling(child);
            let facts = self.svg_facts(child);
            // Masks/clips/patterns do not change the bounding box of their parents.
            if state_mut(self.state).box_facts(&self.callbacks, child).is_box
                && !facts.is_mask_box
                && !facts.is_clip_box
                && !facts.is_pattern_box
            {
                self.layout_svg_element(child, input, container_svg_transform);
                let child_used_pointer = self.used_values(child);
                let child_used = unsafe { &*child_used_pointer };
                let left = child_used.content_offset.x;
                let top = child_used.content_offset.y;
                let right = left + child_used.content_inline_size;
                let bottom = top + child_used.content_block_size;
                if has_points {
                    min_x = min_x.min(left);
                    min_y = min_y.min(top);
                    max_x = max_x.max(right);
                    max_y = max_y.max(bottom);
                } else {
                    min_x = left;
                    min_y = top;
                    max_x = right;
                    max_y = bottom;
                    has_points = true;
                }
            }
            child = next;
        }

        let used_pointer = self.used_values(container);
        let used = unsafe { &mut *used_pointer };
        used.set_content_inline_size(max_x - min_x);
        used.set_content_block_size(max_y - min_y);
        self.place_child(container, min_x, min_y);
        used.has_definite_inline_size = true;
        used.has_definite_block_size = true;
    }
}

pub(super) fn run(instance: &mut FormattingContextInstance, input: FfiLayoutInput) {
    instance.svg_context.as_mut().unwrap().run(input);
}

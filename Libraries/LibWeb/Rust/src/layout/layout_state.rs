/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

const PAGE_BITS: usize = 4;
const PAGE_SIZE: usize = 1 << PAGE_BITS;
const PAGE_MASK: usize = PAGE_SIZE - 1;
const PAGE_TABLE_BITS: usize = 10;
const PAGE_TABLE_FANOUT: usize = 1 << PAGE_TABLE_BITS;
const PAGE_TABLE_MASK: usize = PAGE_TABLE_FANOUT - 1;
const ADDRESSABLE_SLOT_INDEX_COUNT: usize = 1 << (2 * PAGE_TABLE_BITS + PAGE_BITS);

type Page<T> = [OnceCell<T>; PAGE_SIZE];
type PageTable<T> = [OnceCell<Box<Page<T>>>; PAGE_TABLE_FANOUT];
type PageTableDirectory<T> = [OnceCell<Box<PageTable<T>>>; PAGE_TABLE_FANOUT];

pub(crate) struct PagedStore<T> {
    page_table_directory: OnceCell<Box<PageTableDirectory<T>>>,
}

impl<T> Default for PagedStore<T> {
    fn default() -> Self {
        Self {
            page_table_directory: OnceCell::new(),
        }
    }
}

impl<T> PagedStore<T> {
    fn empty_level<Entry, const FANOUT: usize>() -> Box<[OnceCell<Entry>; FANOUT]> {
        Box::new([const { OnceCell::new() }; FANOUT])
    }

    fn split_index(index: u32) -> (usize, usize, usize) {
        let index = index as usize;
        (
            index >> (PAGE_TABLE_BITS + PAGE_BITS),
            (index >> PAGE_BITS) & PAGE_TABLE_MASK,
            index & PAGE_MASK,
        )
    }

    #[inline]
    pub(crate) fn get(&self, index: u32) -> Option<&T> {
        let (directory_index, page_table_index, entry_index) = Self::split_index(index);
        self.page_table_directory
            .get()?
            .get(directory_index)?
            .get()?[page_table_index]
            .get()?[entry_index]
            .get()
    }

    pub(crate) fn allocate(&self, index: u32, value: T) -> &T {
        assert!((index as usize) < ADDRESSABLE_SLOT_INDEX_COUNT);
        let (directory_index, page_table_index, entry_index) = Self::split_index(index);
        let page_table =
            self.page_table_directory.get_or_init(Self::empty_level)[directory_index].get_or_init(Self::empty_level);
        let entry = &page_table[page_table_index].get_or_init(Self::empty_level)[entry_index];
        let entry_was_vacant = entry.set(value).is_ok();
        assert!(entry_was_vacant, "PagedStore index {index} allocated twice");
        entry.get().unwrap()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StaticPositionAlignment {
    Start,
    Center,
    End,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct StaticPositionRect {
    pub(crate) rect: LogicalRect,
    pub(crate) inline_alignment: StaticPositionAlignment,
    pub(crate) block_alignment: StaticPositionAlignment,
    pub(crate) alignment_derives_from_own_computed_values: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AbsposAxisMode {
    StaticPosition,
    InsetFromRect,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AbsposAlignment {
    Baseline,
    Center,
    End,
    Normal,
    Safe,
    SelfEnd,
    SelfStart,
    SpaceAround,
    SpaceBetween,
    SpaceEvenly,
    Start,
    Stretch,
    Unsafe,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct AbsposContainingBlockInfo {
    pub(crate) rect: LogicalRect,
    pub(crate) inline_axis_mode: AbsposAxisMode,
    pub(crate) block_axis_mode: AbsposAxisMode,
    pub(crate) inline_alignment: Option<AbsposAlignment>,
    pub(crate) block_alignment: Option<AbsposAlignment>,
    pub(crate) derives_from_own_computed_values: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct AbsposLayoutInputs {
    pub(crate) static_position_rect: StaticPositionRect,
    pub(crate) containing_block_info: AbsposContainingBlockInfo,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(C)]
pub struct FfiTableCellCoordinates {
    pub row_index: usize,
    pub column_index: usize,
    pub row_span: usize,
    pub column_span: usize,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(C)]
pub struct FfiCommittedBoxMetrics {
    pub content_offset: crate::layout::FfiCssPixelPoint,
    pub content_inline_size: crate::layout::CssPixels,
    pub content_block_size: crate::layout::CssPixels,
    pub margin_left: crate::layout::CssPixels,
    pub margin_right: crate::layout::CssPixels,
    pub margin_top: crate::layout::CssPixels,
    pub margin_bottom: crate::layout::CssPixels,
    pub border_left: crate::layout::CssPixels,
    pub border_right: crate::layout::CssPixels,
    pub border_top: crate::layout::CssPixels,
    pub border_bottom: crate::layout::CssPixels,
    pub padding_left: crate::layout::CssPixels,
    pub padding_right: crate::layout::CssPixels,
    pub padding_top: crate::layout::CssPixels,
    pub padding_bottom: crate::layout::CssPixels,
    pub inset_left: crate::layout::CssPixels,
    pub inset_right: crate::layout::CssPixels,
    pub inset_top: crate::layout::CssPixels,
    pub inset_bottom: crate::layout::CssPixels,
    pub containing_line_box_index: usize,
    pub has_containing_line_box_index: bool,
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct InlineAncestorChainRelativeOffset {
    pub(crate) offset_x: crate::layout::CssPixels,
    pub(crate) offset_y: crate::layout::CssPixels,
    pub(crate) found_fragmented_inline_node: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(C)]
pub struct FfiCommitNodeResult {
    pub paintable: *mut c_void,
    pub paintable_for_children: *mut c_void,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(C)]
pub struct FfiCommitPosition {
    pub parent_paintable: *mut c_void,
    pub insert_before_paintable: *mut c_void,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(C)]
pub struct FfiPaintableGeometry {
    pub content_inline_size: crate::layout::CssPixels,
    pub content_block_size: crate::layout::CssPixels,
    pub content_offset: crate::layout::FfiCssPixelPoint,
    pub svg_viewport_size: crate::layout::FfiCssPixelSize,
    pub margin_left: crate::layout::CssPixels,
    pub margin_right: crate::layout::CssPixels,
    pub margin_top: crate::layout::CssPixels,
    pub margin_bottom: crate::layout::CssPixels,
    pub border_left: crate::layout::CssPixels,
    pub border_right: crate::layout::CssPixels,
    pub border_top: crate::layout::CssPixels,
    pub border_bottom: crate::layout::CssPixels,
    pub padding_left: crate::layout::CssPixels,
    pub padding_right: crate::layout::CssPixels,
    pub padding_top: crate::layout::CssPixels,
    pub padding_bottom: crate::layout::CssPixels,
    pub inset_left: crate::layout::CssPixels,
    pub inset_right: crate::layout::CssPixels,
    pub inset_top: crate::layout::CssPixels,
    pub inset_bottom: crate::layout::CssPixels,
}

#[derive(Clone, Copy)]
#[repr(C)]
pub struct FfiCommitSink {
    pub context: *mut c_void,
    pub begin_commit: unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void) -> FfiCommitPosition,
    pub finish_commit: unsafe extern "C" fn(*mut c_void),
    pub prepare_node: unsafe extern "C" fn(*mut c_void, *mut c_void, bool) -> *mut c_void,
    pub set_box_metrics: unsafe extern "C" fn(*mut c_void, *mut c_void, FfiCommittedBoxMetrics),
    pub set_override_borders: unsafe extern "C" fn(*mut c_void, *mut c_void, FfiBordersData),
    pub set_table_cell_coordinates: unsafe extern "C" fn(*mut c_void, *mut c_void, FfiTableCellCoordinates),
    pub begin_line_data: unsafe extern "C" fn(*mut c_void, *mut c_void) -> bool,
    pub begin_line: unsafe extern "C" fn(*mut c_void, FfiLineRecord),
    pub emit_fragment: unsafe extern "C" fn(*mut c_void, FfiCommittedFragment),
    pub emit_inline_box_piece: unsafe extern "C" fn(*mut c_void, FfiInlineBoxPiece),
    pub finish_line_data: unsafe extern "C" fn(*mut c_void),
    pub set_computed_svg_transforms: unsafe extern "C" fn(*mut c_void, *mut c_void, crate::layout::FfiSvgComputedTransforms),
    pub set_svg_viewport_size: unsafe extern "C" fn(*mut c_void, *mut c_void, crate::layout::FfiCssPixelSize),
    pub set_computed_svg_path: unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void),
    pub set_grid_layout_data: unsafe extern "C" fn(*mut c_void, *mut c_void, *const FfiGridLayoutData),
    pub set_flex_layout_data: unsafe extern "C" fn(*mut c_void, *mut c_void, *const FfiFlexLayoutData),
    pub set_used_grid_tracks:
        unsafe extern "C" fn(*mut c_void, *mut c_void, *const FfiUsedGridTrackList, *const FfiUsedGridTrackList),
    pub finish_node:
        unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void, *mut c_void, *mut c_void) -> FfiCommitNodeResult,
    pub assign_inline_box_geometry: unsafe extern "C" fn(*mut c_void, *mut c_void),
}

pub(crate) type ReleaseRetainedLayoutHandle = unsafe extern "C" fn(*mut c_void, *mut c_void);

pub(crate) struct RetainedLayoutHandle {
    handle: *mut c_void,
    callback_context: *mut c_void,
    release: ReleaseRetainedLayoutHandle,
}

impl RetainedLayoutHandle {
    pub(crate) fn new(
        handle: *mut c_void,
        callback_context: *mut c_void,
        release: ReleaseRetainedLayoutHandle,
    ) -> Self {
        assert!(!handle.is_null());
        Self {
            handle,
            callback_context,
            release,
        }
    }

    pub(crate) fn take(&mut self) -> *mut c_void {
        let handle = self.handle;
        self.handle = null_mut();
        handle
    }
}

impl Drop for RetainedLayoutHandle {
    fn drop(&mut self) {
        if self.handle.is_null() {
            return;
        }
        // SAFETY: Each retained layout handle is returned with one ownership
        // unit by its creating callback and is either transferred to the
        // commit sink or released exactly once with the paired callback.
        unsafe {
            (self.release)(self.callback_context, self.handle);
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PendingAbsposChild {
    pub(crate) child_box: Node,
    /// The formatting-context root whose run tail lays this child out.
    pub(crate) target: Node,
    /// The box whose content space the static position rect is expressed
    /// in. Starts as the producer's container; escape normalization and
    /// absorb hops fold offsets into the rect and rename this box so every
    /// consumption walk stays within records the draining run owns.
    pub(crate) effective_birth: Node,
    pub(crate) static_position_rect: StaticPositionRect,
    pub(crate) containing_block_info_override: Option<AbsposContainingBlockInfo>,
    /// The relative-positioned inline whose first/last-line rect forms this
    /// child's containing block, when one does; the producing inline run
    /// stamps the rect while the entry sits in its builder, and it travels
    /// with the static rect from then on.
    pub(crate) inline_containing_block: Node,
    pub(crate) inline_containing_block_rect: Option<PhysicalRect>,
}

/// A pass-scoped lens over one node's facts. Pure classification reads the
/// arena NodeData directly; style-derived answers read the per-slot style
/// snapshot; content-derived answers read the kind-gated lazy fact stores.
/// Nothing is materialized per node, so every answer is as live as its source.
#[derive(Clone, Copy)]
pub(crate) struct NodeFacts<'pass> {
    state: &'pass LayoutState,
    callbacks: &'pass FfiLayoutFcCallbacks,
    node: Node,
}

impl<'pass> NodeFacts<'pass> {
    fn data(&self) -> &'pass NodeData {
        self.callbacks.node_data(self.node)
    }

    fn parent_data(&self) -> Option<&'pass NodeData> {
        let parent = self.data().parent;
        (!parent.is_invalid()).then(|| self.callbacks.node_data(parent))
    }

    fn style(&self) -> StyleValues<'pass> {
        self.state.style_facts(self.callbacks, self.node)
    }

    fn computed_values_view_if_styled(&self) -> Option<ComputedValuesView<'pass>> {
        self.callbacks.computed_values_view_if_styled(self.node)
    }

    fn parent_computed_values_view_if_styled(&self) -> Option<ComputedValuesView<'pass>> {
        let parent = self.data().parent;
        if parent.is_invalid() {
            return None;
        }
        self.callbacks.computed_values_view_if_styled(parent)
    }

    fn replaced_content(&self) -> crate::layout::FfiReplacedContentFacts {
        self.state.replaced_content_facts(self.callbacks, self.node)
    }

    pub(crate) fn is_text_node(&self) -> bool {
        crate::layout::kind_is_text(self.data().kind)
    }

    pub(crate) fn is_break_node(&self) -> bool {
        self.data().kind == NodeKind::BreakNode
    }

    pub(crate) fn is_box(&self) -> bool {
        crate::layout::kind_is_box(self.data().kind)
    }

    pub(crate) fn is_block_container(&self) -> bool {
        crate::layout::kind_is_block_container(self.data().kind)
    }

    pub(crate) fn is_replaced_box(&self) -> bool {
        crate::layout::kind_is_replaced_box(self.data().kind)
    }

    pub(crate) fn is_replaced_box_with_children(&self) -> bool {
        let data = self.data();
        crate::layout::kind_is_replaced_box(data.kind) && crate::layout::node_can_have_children(data)
    }

    pub(crate) fn is_floating(&self) -> bool {
        self.computed_values_view_if_styled().is_some_and(|style| style.is_floating())
    }

    pub(crate) fn is_absolutely_positioned(&self) -> bool {
        self.computed_values_view_if_styled()
            .is_some_and(|style| style.is_absolutely_positioned())
    }

    pub(crate) fn is_relatively_positioned(&self) -> bool {
        self.computed_values_view_if_styled()
            .is_some_and(|style| style.position() == crate::css::css_enums::positioning::RELATIVE)
    }

    pub(crate) fn is_in_flow(&self) -> bool {
        !crate::layout::node_is_out_of_flow(self.data(), self.computed_values_view_if_styled())
    }

    pub(crate) fn is_floating_or_absolutely_positioned(&self) -> bool {
        self.computed_values_view_if_styled()
            .is_some_and(|style| style.is_floating() || style.is_absolutely_positioned())
    }

    pub(crate) fn is_inline(&self) -> bool {
        crate::layout::kind_is_text(self.data().kind)
            || self
                .computed_values_view_if_styled()
                .is_some_and(|style| style.display().is_inline_outside())
    }

    pub(crate) fn is_atomic_inline(&self) -> bool {
        let data = self.data();
        crate::layout::has_flag(data, NodeFlag::IsReplacedElement)
            || data.kind == NodeKind::ListItemMarkerBox
            || self.computed_values_view_if_styled().is_some_and(|style| {
                let display = style.display();
                display.is_inline_outside() && !display.is_flow_inside()
            })
    }

    pub(crate) fn has_box_model_metrics(&self) -> bool {
        !matches!(
            self.data().kind,
            NodeKind::Unset
                | NodeKind::Node
                | NodeKind::NodeWithStyle
                | NodeKind::GeneratedTextNode
                | NodeKind::TextNode
                | NodeKind::TextSliceNode
        )
    }

    pub(crate) fn is_fragmented_inline(&self) -> bool {
        let data = self.data();
        data.kind == NodeKind::InlineNode
            || (data.kind == NodeKind::ListItemBox
                && self.computed_values_view_if_styled().is_some_and(|style| {
                    let display = style.display();
                    display.is_inline_outside() && display.is_flow_inside()
                }))
    }

    /// A box whose committed geometry is its own single box record; a
    /// fragmented inline's committed geometry is its line pieces instead.
    pub(crate) fn is_non_fragmented_box(&self) -> bool {
        self.is_box() && !self.is_fragmented_inline()
    }

    pub(crate) fn is_inline_flow_interrupting_block(&self) -> bool {
        if !self.has_box_model_metrics() {
            return false;
        }
        let data = self.data();
        let Some(parent) = self.parent_data() else {
            return false;
        };
        let parent_is_inline_flow = self.parent_computed_values_view_if_styled().is_some_and(|style| {
            let display = style.display();
            display.is_inline_outside() && display.is_flow_inside()
        });
        if !parent_is_inline_flow {
            return false;
        }
        let style = self.computed_values_view_if_styled();
        if style.is_some_and(|style| style.display().is_inline_outside())
            || crate::layout::node_is_out_of_flow(data, style)
        {
            return false;
        }
        let display = self.display();
        if display.is_contents() {
            return false;
        }
        if display.is_internal_table() || display.is_table_caption() {
            return false;
        }
        if parent.kind == NodeKind::SVGForeignObjectBox {
            return false;
        }
        if crate::layout::kind_is_svg_box(data.kind) || data.kind == NodeKind::SVGForeignObjectBox {
            return false;
        }
        if data.kind == NodeKind::SVGSVGBox
            && (crate::layout::kind_is_svg_box(parent.kind) || parent.kind == NodeKind::SVGSVGBox)
        {
            return false;
        }
        if crate::layout::kind_is_replaced_box(parent.kind) && crate::layout::node_can_have_children(parent) {
            return false;
        }
        true
    }

    pub(crate) fn is_list_item_marker_box(&self) -> bool {
        self.data().kind == NodeKind::ListItemMarkerBox
    }

    pub(crate) fn is_list_item_box(&self) -> bool {
        self.data().kind == NodeKind::ListItemBox
    }

    pub(crate) fn is_svg_mask_box(&self) -> bool {
        self.data().kind == NodeKind::SVGMaskBox
    }

    pub(crate) fn is_svg_clip_box(&self) -> bool {
        self.data().kind == NodeKind::SVGClipBox
    }

    pub(crate) fn is_flow_layout_participant(&self) -> bool {
        self.is_box()
            && !self.is_absolutely_positioned()
            && !self.is_list_item_marker_box()
            && !self.is_svg_mask_box()
            && !self.is_svg_clip_box()
    }

    pub(crate) fn display_before_box_type_transformation_is_block_outside(&self) -> bool {
        self.computed_values_view_if_styled()
            .is_some_and(|style| style.display_before_box_type_transformation().is_block_outside())
    }

    pub(crate) fn inline_axis_is_reverse(&self) -> bool {
        let style = self.style();
        match style.writing_mode() {
            writing_mode::HORIZONTAL_TB
            | writing_mode::VERTICAL_RL
            | writing_mode::VERTICAL_LR
            | writing_mode::SIDEWAYS_RL => style.direction() == 1,
            writing_mode::SIDEWAYS_LR => style.direction() == 0,
            _ => unreachable!("invalid writing mode"),
        }
    }

    pub(crate) fn has_dom_node(&self) -> bool {
        !crate::layout::has_flag(self.data(), NodeFlag::Anonymous)
    }

    pub(crate) fn is_generated_for_pseudo_element(&self) -> bool {
        self.data().generated_for != 0
    }

    pub(crate) fn children_are_inline(&self) -> bool {
        crate::layout::has_flag(self.data(), NodeFlag::ChildrenAreInline)
    }

    pub(crate) fn is_anonymous(&self) -> bool {
        crate::layout::has_flag(self.data(), NodeFlag::Anonymous)
    }

    pub(crate) fn has_anchor_names(&self) -> bool {
        crate::layout::has_flag(self.data(), NodeFlag::HasAnchorNames)
    }

    pub(crate) fn can_have_children(&self) -> bool {
        crate::layout::node_can_have_children(self.data())
    }

    pub(crate) fn has_replaced_element_table_display_adjustment(&self) -> bool {
        crate::layout::has_flag(self.data(), NodeFlag::IsReplacedElement)
            && self
                .computed_values_view_if_styled()
                .is_some_and(|style| {
                    let display = style.display_before_box_type_transformation();
                    display.is_table_inside() || display.is_internal_table() || display.is_table_caption()
                })
    }

    pub(crate) fn creates_block_formatting_context(&self) -> bool {
        crate::layout::node_creates_block_formatting_context(
            self.data(),
            self.computed_values_view_if_styled(),
            self.parent_computed_values_view_if_styled(),
        )
    }

    pub(crate) fn is_editing_host(&self) -> bool {
        crate::layout::has_flag(self.data(), NodeFlag::IsEditingHost)
    }

    pub(crate) fn produces_line_box_fragment_when_empty(&self) -> bool {
        crate::layout::has_flag(self.data(), NodeFlag::ProducesLineBoxFragmentWhenEmpty)
    }

    pub(crate) fn uses_button_layout(&self) -> bool {
        crate::layout::has_flag(self.data(), NodeFlag::UsesButtonLayout)
    }

    pub(crate) fn vertical_align_applies(&self) -> bool {
        let data = self.data();
        crate::layout::kind_is_box(data.kind)
            && !crate::layout::has_flag(data, NodeFlag::IsFlexItem)
            && !crate::layout::has_flag(data, NodeFlag::IsGridItem)
    }

    pub(crate) fn is_html_input_element(&self) -> bool {
        crate::layout::has_flag(self.data(), NodeFlag::IsHtmlInputElement)
    }

    pub(crate) fn is_fieldset_box(&self) -> bool {
        self.data().kind == NodeKind::FieldSetBox
    }

    pub(crate) fn rendered_legend(&self) -> Node {
        let data = self.data();
        if data.kind != NodeKind::FieldSetBox {
            return Node::INVALID;
        }
        let mut child = data.first_child;
        while !child.is_invalid() {
            let child_data = self.callbacks.node_data(child);
            if child_data.kind == NodeKind::LegendBox
                && !crate::layout::node_is_out_of_flow(child_data, self.callbacks.computed_values_view_if_styled(child))
            {
                return child;
            }
            child = child_data.next_sibling;
        }
        Node::INVALID
    }

    pub(crate) fn list_item_marker(&self) -> Node {
        if !self.is_list_item_box() {
            return Node::INVALID;
        }
        // Outside markers are direct children. Inside markers participate in
        // an inline run and can therefore move below anonymous block wrappers.
        // Walk those wrappers, but not nested authored or generated list items.
        let mut candidate = self.data().first_child;
        while !candidate.is_invalid() {
            let data = self.callbacks.node_data(candidate);
            if data.kind == NodeKind::ListItemMarkerBox {
                return candidate;
            }
            if data.kind == NodeKind::BlockContainer
                && crate::layout::has_flag(data, NodeFlag::Anonymous)
                && data.generated_for == 0
                && !data.first_child.is_invalid()
            {
                candidate = data.first_child;
                continue;
            }
            let mut current_data = data;
            loop {
                if !current_data.next_sibling.is_invalid() {
                    candidate = current_data.next_sibling;
                    break;
                }
                if current_data.parent == self.node {
                    return Node::INVALID;
                }
                current_data = self.callbacks.node_data(current_data.parent);
            }
        }
        Node::INVALID
    }

    pub(crate) fn list_marker_is_inside(&self) -> bool {
        crate::layout::has_flag(self.data(), NodeFlag::ListMarkerIsInside)
    }

    pub(crate) fn has_auto_content_width(&self) -> bool {
        self.replaced_content().has_auto_content_width
    }

    pub(crate) fn auto_content_width(&self) -> crate::layout::CssPixels {
        self.replaced_content().auto_content_width
    }

    pub(crate) fn has_auto_content_height(&self) -> bool {
        self.replaced_content().has_auto_content_height
    }

    pub(crate) fn auto_content_height(&self) -> crate::layout::CssPixels {
        self.replaced_content().auto_content_height
    }

    pub(crate) fn has_auto_content_aspect_ratio(&self) -> bool {
        self.replaced_content().auto_content_aspect_ratio_denominator != crate::layout::CssPixels::default()
    }

    pub(crate) fn auto_content_aspect_ratio_numerator(&self) -> crate::layout::CssPixels {
        self.replaced_content().auto_content_aspect_ratio_numerator
    }

    pub(crate) fn auto_content_aspect_ratio_denominator(&self) -> crate::layout::CssPixels {
        self.replaced_content().auto_content_aspect_ratio_denominator
    }

    pub(crate) fn has_auto_content_box_size(&self) -> bool {
        crate::layout::node_has_auto_content_box_size(self.data())
    }

    pub(crate) fn node_has_size_containment(&self) -> bool {
        let display = self.display();
        if display.is_table_inside() || display.is_internal_table() {
            return false;
        }
        let style = self.style();
        style.has_size_containment() || style.is_size_container()
    }

    pub(crate) fn has_preferred_aspect_ratio(&self) -> bool {
        self.preferred_aspect_ratio().is_some()
    }

    pub(crate) fn preferred_aspect_ratio(&self) -> Option<PixelFraction> {
        let style = self.style();
        if !self.node_has_size_containment() && style.aspect_ratio_uses_natural_when_available() {
            let replaced = self.replaced_content();
            if replaced.auto_content_aspect_ratio_denominator != crate::layout::CssPixels::default() {
                return Some(PixelFraction {
                    numerator: replaced.auto_content_aspect_ratio_numerator,
                    denominator: replaced.auto_content_aspect_ratio_denominator,
                });
            }
        }
        let (numerator, denominator) = style.css_preferred_aspect_ratio();
        (denominator != crate::layout::CssPixels::default()).then_some(PixelFraction { numerator, denominator })
    }

    pub(crate) fn has_default_preferred_width(&self) -> bool {
        self.replaced_content().has_default_preferred_width
    }

    pub(crate) fn default_preferred_width(&self) -> crate::layout::CssPixels {
        self.replaced_content().default_preferred_width
    }

    pub(crate) fn has_default_preferred_height(&self) -> bool {
        self.replaced_content().has_default_preferred_height
    }

    pub(crate) fn default_preferred_height(&self) -> crate::layout::CssPixels {
        self.replaced_content().default_preferred_height
    }

    pub(crate) fn initial_containing_block_inline_size(&self) -> crate::layout::CssPixels {
        self.callbacks.initial_containing_block_inline_size
    }

    pub(crate) fn is_scroll_container(&self) -> bool {
        if self.data().kind == NodeKind::Viewport {
            return true;
        }
        let style = self.style();
        let overflow_value_makes_box_a_scroll_container =
            |overflow: u8| matches!(overflow, overflow::AUTO | overflow::HIDDEN | overflow::SCROLL);
        overflow_value_makes_box_a_scroll_container(style.overflow_x())
            || overflow_value_makes_box_a_scroll_container(style.overflow_y())
    }

    pub(crate) fn display(&self) -> crate::layout::FfiDisplay {
        if self.data().style.is_null() {
            return crate::layout::FfiDisplay::block();
        }
        self.state.style_facts(self.callbacks, self.node).display()
    }

    pub(crate) fn is_svg_box(&self) -> bool {
        crate::layout::kind_is_svg_box(self.data().kind)
    }

    pub(crate) fn is_svg_svg_box(&self) -> bool {
        self.data().kind == NodeKind::SVGSVGBox
    }

    pub(crate) fn is_table_box(&self) -> bool {
        self.display().is_table_inside()
    }

    pub(crate) fn is_table_wrapper(&self) -> bool {
        self.data().kind == NodeKind::TableWrapper
    }

    pub(crate) fn is_table_row_group(&self) -> bool {
        self.display().is_table_row_group()
    }

    pub(crate) fn is_table_header_group(&self) -> bool {
        self.display().is_table_header_group()
    }

    pub(crate) fn is_table_footer_group(&self) -> bool {
        self.display().is_table_footer_group()
    }

    pub(crate) fn is_table_row_group_kind(&self) -> bool {
        self.display().is_table_row_group_kind()
    }

    pub(crate) fn is_table_row(&self) -> bool {
        self.display().is_table_row()
    }

    pub(crate) fn is_table_cell(&self) -> bool {
        self.display().is_table_cell()
    }

    pub(crate) fn is_table_column_group(&self) -> bool {
        self.display().is_table_column_group()
    }

    pub(crate) fn is_table_column(&self) -> bool {
        self.display().is_table_column()
    }

    pub(crate) fn is_table_caption(&self) -> bool {
        self.display().is_table_caption()
    }

    pub(crate) fn is_viewport(&self) -> bool {
        self.data().kind == NodeKind::Viewport
    }

    pub(crate) fn document_in_quirks_mode(&self) -> bool {
        self.callbacks.document_in_quirks_mode
    }

    pub(crate) fn is_in_user_agent_shadow_tree(&self) -> bool {
        crate::layout::has_flag(self.data(), NodeFlag::IsInUserAgentShadowTree)
    }

    pub(crate) fn is_html_html_element(&self) -> bool {
        crate::layout::has_flag(self.data(), NodeFlag::IsHtmlHtmlElement)
    }

    pub(crate) fn is_html_body_element(&self) -> bool {
        crate::layout::has_flag(self.data(), NodeFlag::IsBody)
    }
}

pub(crate) struct LayoutState {
    anchor_inset_store: AnchorInsetStore,
    purpose: LayoutStatePurpose,
}

/// A formatting-context run's record scope: the run registers every record
/// it creates plus the root record its parent handed it, and every lookup
/// routes through here, so a run can only reach records it provably owns.
/// The scope OWNS its records — when the last scope handle drops at run
/// completion, the run's interior records die with it; only records shared
/// onward (the roots parents hand to children) outlive it.
/// One placement as the scope witnessed it: the offset handed to
/// place_child and the record's metrics at that moment, immutable from
/// then on because the seal forbids post-placement metric writes.
#[derive(Clone, Copy)]
pub(crate) struct PlacedGeometry {
    pub(crate) content_offset: FfiCssPixelPoint,
    pub(crate) metrics: BoxMetrics,
}

pub(crate) struct RunRecords {
    root: Node,
    map: RefCell<HashMap<u32, std::rc::Rc<UsedValues>>>,
    /// Every placement under this scope. Scope-level on purpose: nested
    /// SVG contexts share one scope across separate fragment builders, so
    /// a per-builder structure cannot answer placement questions the scope
    /// can. Grows into the run workspace as readers move off the records.
    placements: RefCell<HashMap<u32, PlacedGeometry>>,
}

impl RunRecords {
    pub(crate) fn new(root: Node, root_used: std::rc::Rc<UsedValues>) -> Self {
        let records = Self::new_unrooted(root);
        records.register(root, root_used);
        records
    }

    /// An entry scope starts empty: it creates its own viewport and
    /// materialized-ancestor records before spawning the root run.
    pub(crate) fn new_unrooted(root: Node) -> Self {
        Self {
            root,
            map: RefCell::new(HashMap::new()),
            placements: RefCell::new(HashMap::new()),
        }
    }

    pub(crate) fn note_placement(&self, node: Node, geometry: PlacedGeometry) {
        let previous = self.placements.borrow_mut().insert(node.slot_index(), geometry);
        assert!(
            previous.is_none(),
            "slot {} placed twice in the scope rooted at slot {}",
            node.slot_index(),
            self.root.slot_index()
        );
    }

    pub(crate) fn slot_is_placed_in_scope(&self, node: Node) -> bool {
        self.placements.borrow().contains_key(&node.slot_index())
    }

    pub(crate) fn placement_of(&self, node: Node) -> Option<PlacedGeometry> {
        self.placements.borrow().get(&node.slot_index()).copied()
    }

    pub(crate) fn register(&self, node: Node, used: std::rc::Rc<UsedValues>) {
        // A record can arrive already placed — materialized roots adopt the
        // previous paintable's committed geometry as their placement — and
        // the scope's placements mirror the placement facts of the records
        // it holds.
        if used.has_content_offset.get() {
            self.placements.borrow_mut().insert(
                node.slot_index(),
                PlacedGeometry {
                    content_offset: used.content_offset.get(),
                    metrics: BoxMetrics::capture_from_record(&used),
                },
            );
        }
        let previous = self.map.borrow_mut().insert(node.slot_index(), used);
        assert!(
            previous.is_none(),
            "slot {} registered twice in the run rooted at slot {}",
            node.slot_index(),
            self.root.slot_index()
        );
    }

    pub(crate) fn create_used_values(
        &self,
        state: &LayoutState,
        callbacks: &FfiLayoutFcCallbacks,
        node: Node,
        constraints: ContainingBlockConstraints,
    ) -> std::rc::Rc<UsedValues> {
        let used = state.create_used_values(callbacks, node, constraints);
        self.register(node, used.clone());
        used
    }

    #[track_caller]
    pub(crate) fn used_values(&self, node: Node) -> std::rc::Rc<UsedValues> {
        let caller = std::panic::Location::caller();
        self.used_values_if_owned(node).unwrap_or_else(|| {
            panic!(
                "the run rooted at slot {} does not own the record for slot {} (read at {caller})",
                self.root.slot_index(),
                node.slot_index(),
            )
        })
    }

    /// Probes that legally meet records outside this run's scope (seeding a
    /// drain host's own root into the placement index, the SVG payload
    /// refresh walking deposited child fragments, place_child's
    /// containing-block probe) use this instead of the panicking lookup;
    /// not-owned is a legal answer there, never an error.
    pub(crate) fn used_values_if_owned(&self, node: Node) -> Option<std::rc::Rc<UsedValues>> {
        self.map.borrow().get(&node.slot_index()).cloned()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LayoutStatePurpose {
    Commit,
    Measurement,
}

impl Default for LayoutState {
    fn default() -> Self {
        Self::new(LayoutStatePurpose::Commit)
    }
}

#[derive(Clone, Default)]
pub(crate) struct LineData {
    pub(crate) line_boxes: Vec<LineBoxData>,
    pub(crate) inline_box_pieces: Vec<InlineBoxPieceData>,
}

#[derive(Default)]
pub(crate) struct UsedValuesRareData {
    pub(crate) table_cell_coordinates: Option<FfiTableCellCoordinates>,
    pub(crate) computed_svg_path: Option<RetainedLayoutHandle>,
    pub(crate) computed_svg_transforms: Option<crate::layout::FfiSvgComputedTransforms>,
    pub(crate) svg_viewport_size: Option<crate::layout::FfiCssPixelSize>,
    pub(crate) grid_layout_data: Option<OwnedGridLayoutData>,
    pub(crate) flex_layout_data: Option<OwnedFlexLayoutData>,
    pub(crate) used_grid_tracks: Option<OwnedUsedGridTracks>,
    pub(crate) override_borders_data: Option<FfiBordersData>,
    pub(crate) abspos_layout_inputs: Option<AbsposLayoutInputs>,
}

impl LayoutState {
    pub(crate) fn new(purpose: LayoutStatePurpose) -> Self {
        Self {
            anchor_inset_store: AnchorInsetStore::default(),
            purpose,
        }
    }

    pub(crate) fn is_measurement(&self) -> bool {
        self.purpose == LayoutStatePurpose::Measurement
    }

    #[inline]
    pub(crate) fn node_facts<'pass>(
        &'pass self,
        callbacks: &'pass FfiLayoutFcCallbacks,
        node: Node,
    ) -> NodeFacts<'pass> {
        NodeFacts {
            state: self,
            callbacks,
            node,
        }
    }

    pub(crate) fn create_used_values(
        &self,
        callbacks: &FfiLayoutFcCallbacks,
        node: Node,
        constraints: ContainingBlockConstraints,
    ) -> std::rc::Rc<UsedValues> {
        let metrics = self.initial_box_metrics_for_node(callbacks, node, constraints);
        let used = UsedValues::default();
        used.has_definite_inline_size.set(metrics.has_definite_inline_size);
        used.has_definite_block_size.set(metrics.has_definite_block_size);
        used.content_inline_size.set(metrics.content_inline_size);
        used.content_block_size.set(metrics.content_block_size);
        std::rc::Rc::new(used)
    }

    /// The style-derived initial state of a box before any layout: the
    /// definiteness decision and the initially definite sizes. This is the
    /// baseline every run root starts from; parents layer their inputs on
    /// top of it.
    pub(crate) fn initial_box_metrics_for_node(
        &self,
        callbacks: &FfiLayoutFcCallbacks,
        node: Node,
        constraints: ContainingBlockConstraints,
    ) -> BoxMetrics {
        assert!(!node.is_invalid());
        let facts = self.node_facts(callbacks, node);

        let style = self.style_facts(callbacks, node);
        let percentage_basis_inline_size = constraints.percentage_basis_inline_size;
        let percentage_basis_block_size = constraints.percentage_basis_block_size;

        // NOTE: In the code below, we decide if `node` has definite inline
        // and/or block size. This attempts to cover all the *general* cases
        // where CSS considers sizes to be definite. If `node` has definite
        // values for min/max-width or min/max-height and a definite preferred
        // size in the same axis, we clamp the preferred size here as well.
        //
        // There are additional cases where CSS considers values to be
        // definite. We model all of those by considering sizes definite once
        // they are assigned through set_content_inline_size() or
        // set_content_block_size().
        let mut metrics = BoxMetrics::default();

        #[derive(Clone, Copy)]
        enum Axis {
            Inline,
            Block,
        }

        let containing_block_size_for_axis = |axis: Axis| match axis {
            Axis::Inline => percentage_basis_inline_size.unwrap_or_default(),
            Axis::Block => percentage_basis_block_size.unwrap_or_default(),
        };
        let containing_block_has_definite_size = |axis: Axis| match axis {
            Axis::Inline => percentage_basis_inline_size.is_some(),
            Axis::Block => percentage_basis_block_size.is_some(),
        };

        let adjust_for_box_sizing = |unadjusted: crate::layout::CssPixels, computed_size: &ComputedSize, axis: Axis| {
            // box-sizing: content-box and automatic sizes need no
            // adjustment.
            if style.box_sizing() == box_sizing::CONTENT_BOX || computed_size.is_auto() {
                return unadjusted;
            }

            // box-sizing: border-box subtracts the relevant border and
            // padding. Block-axis padding percentages also resolve against
            // the containing block's inline size.
            let inline_basis = percentage_basis_inline_size.unwrap_or_default();
            let border_and_padding = match axis {
                Axis::Inline => {
                    style.border_left_width()
                        + style.padding_left().to_px(inline_basis)
                        + style.border_right_width()
                        + style.padding_right().to_px(inline_basis)
                }
                Axis::Block => {
                    style.border_top_width()
                        + style.padding_top().to_px(inline_basis)
                        + style.border_bottom_width()
                        + style.padding_bottom().to_px(inline_basis)
                }
            };
            unadjusted - border_and_padding
        };

        let parent = callbacks.parent(node);
        let parent_facts = (!parent.is_invalid()).then(|| self.node_facts(callbacks, parent));
        let is_definite_size = |size: &ComputedSize, axis: Axis| -> Option<crate::layout::CssPixels> {
            // A definite size can be determined without performing
            // layout: a length, an initial-containing-block size, or a
            // percentage/formula resolved solely against definite sizes.
            if size.is_auto() {
                // The inline size of a non-flex-item block is definite when
                // it is auto and its containing block has a definite inline
                // size. This is the stretch-fit case from css-sizing-3.
                // Replaced boxes remain content-based until layout.
                if matches!(axis, Axis::Inline)
                    && !facts.is_replaced_box()
                    && !facts.is_floating()
                    && !facts.is_absolutely_positioned()
                    && facts.display().is_block_outside()
                    && parent_facts.is_some_and(|parent| {
                        !parent.is_floating()
                            && (parent.display().is_flow_root_inside() || parent.display().is_flow_inside())
                    })
                    && containing_block_has_definite_size(Axis::Inline)
                {
                    // The subtracted edges are this box's creation-time
                    // values, which are always still zero at this point;
                    // the subtraction shape is preserved verbatim from the
                    // cell-based derivation, and dropping it is a separate
                    // decision.
                    let available = containing_block_size_for_axis(Axis::Inline);
                    return Some(clamp_to_max_dimension_value(
                        available
                            - metrics.margin.left
                            - metrics.margin.right
                            - metrics.padding.left
                            - metrics.padding.right
                            - metrics.border.left
                            - metrics.border.right,
                    ));
                }
                return None;
            }

            if !size.is_length_percentage() {
                return None;
            }
            if size.contains_percentage() && !containing_block_has_definite_size(axis) {
                return None;
            }
            let basis = if size.contains_percentage() {
                containing_block_size_for_axis(axis)
            } else {
                crate::layout::CssPixels::default()
            };
            Some(clamp_to_max_dimension_value(adjust_for_box_sizing(
                size.to_px(basis),
                size,
                axis,
            )))
        };

        let min_inline_size = is_definite_size(style.min_width(), Axis::Inline);
        let max_inline_size = is_definite_size(style.max_width(), Axis::Inline);
        let min_block_size = is_definite_size(style.min_height(), Axis::Block);
        let max_block_size = is_definite_size(style.max_height(), Axis::Block);
        let mut content_inline_size = is_definite_size(style.width(), Axis::Inline);
        let mut content_block_size = is_definite_size(style.height(), Axis::Block);

        metrics.has_definite_inline_size = content_inline_size.is_some();
        metrics.has_definite_block_size = content_block_size.is_some();
        if let Some(size) = content_inline_size.as_mut() {
            if let Some(minimum) = min_inline_size {
                *size = clamp_to_max_dimension_value((*size).max(minimum));
            }
            if let Some(maximum) = max_inline_size {
                *size = clamp_to_max_dimension_value((*size).min(maximum));
            }
        }
        if let Some(size) = content_block_size.as_mut() {
            if let Some(minimum) = min_block_size {
                *size = clamp_to_max_dimension_value((*size).max(minimum));
            }
            if let Some(maximum) = max_block_size {
                *size = clamp_to_max_dimension_value((*size).min(maximum));
            }
        }
        metrics.content_inline_size = content_inline_size.unwrap_or_default();
        metrics.content_block_size = content_block_size.unwrap_or_default();

        metrics
    }

    pub(crate) fn populate_from_paintable(
        &self,
        callbacks: &FfiLayoutFcCallbacks,
        node: Node,
        paintable: *mut c_void,
    ) -> Option<std::rc::Rc<UsedValues>> {
        let mut geometry = FfiPaintableGeometry::default();
        let found =
            unsafe {
                (callbacks.read_paintable_geometry)(callbacks.context, callbacks.shell(node), paintable, &raw mut geometry)
            };
        if !found {
            return None;
        }

        // Skip normal node initialization: resolving computed sizes requires
        // percentage bases, and every resulting geometry field is replaced by
        // the previous paintable's committed value immediately.
        let used = UsedValues::default();
        used.set_content_inline_size(geometry.content_inline_size);
        used.set_content_block_size(geometry.content_block_size);
        used.has_definite_inline_size.set(true);
        used.has_definite_block_size.set(true);
        used.content_offset.set(geometry.content_offset);
        used.margin_left.set(geometry.margin_left);
        used.margin_right.set(geometry.margin_right);
        used.margin_top.set(geometry.margin_top);
        used.margin_bottom.set(geometry.margin_bottom);
        used.border_left.set(geometry.border_left);
        used.border_right.set(geometry.border_right);
        used.border_top.set(geometry.border_top);
        used.border_bottom.set(geometry.border_bottom);
        used.padding_left.set(geometry.padding_left);
        used.padding_right.set(geometry.padding_right);
        used.padding_top.set(geometry.padding_top);
        used.padding_bottom.set(geometry.padding_bottom);
        used.inset_left.set(geometry.inset_left);
        used.inset_right.set(geometry.inset_right);
        used.inset_top.set(geometry.inset_top);
        used.inset_bottom.set(geometry.inset_bottom);
        // Materialization is this box's placement: the previous paintable's
        // committed geometry is final from the moment it is adopted.
        used.has_content_offset.set(true);
        used.seal_committed_box_metrics();

        if self.node_facts(callbacks, node).is_svg_svg_box() {
            used.rare_data_mut().svg_viewport_size = Some(geometry.svg_viewport_size);
        }
        Some(std::rc::Rc::new(used))
    }

    pub(crate) fn set_box_is_grid_item(&self, callbacks: &FfiLayoutFcCallbacks, node: Node, is_grid_item: bool) {
        callbacks.arena().set_node_flag(node, NodeFlag::IsGridItem, is_grid_item);
    }

    pub(crate) fn set_box_is_flex_item(&self, callbacks: &FfiLayoutFcCallbacks, node: Node, is_flex_item: bool) {
        callbacks.arena().set_node_flag(node, NodeFlag::IsFlexItem, is_flex_item);
    }

    pub(crate) fn style_facts<'pass>(
        &'pass self,
        callbacks: &FfiLayoutFcCallbacks,
        node: Node,
    ) -> StyleValues<'pass> {
        StyleValues::new(
            callbacks.style_payloads(node),
            &self.anchor_inset_store,
            callbacks.slot_index(node),
        )
    }

    pub(crate) fn replace_resolved_anchor_insets(
        &self,
        callbacks: &FfiLayoutFcCallbacks,
        node: Node,
        resolved: crate::layout::FfiResolvedAnchorInsets,
    ) {
        let slot_index = callbacks.slot_index(node);
        let replace = |field: InsetField, is_auto: bool, px: crate::layout::CssPixels| {
            self.anchor_inset_store
                .set_override(slot_index, field, ResolvedInsetOverride { is_auto, px });
        };
        if resolved.resolves_top {
            replace(InsetField::Top, resolved.top_is_auto, resolved.top);
        }
        if resolved.resolves_right {
            replace(InsetField::Right, resolved.right_is_auto, resolved.right);
        }
        if resolved.resolves_bottom {
            replace(InsetField::Bottom, resolved.bottom_is_auto, resolved.bottom);
        }
        if resolved.resolves_left {
            replace(InsetField::Left, resolved.left_is_auto, resolved.left);
        }
    }

    pub(crate) fn replaced_content_facts(
        &self,
        callbacks: &FfiLayoutFcCallbacks,
        node: Node,
    ) -> crate::layout::FfiReplacedContentFacts {
        callbacks.arena().replaced_content_facts(node, || {
            let data = callbacks.node_data(node);
            // Size containment gives any box an auto content box size of zero,
            // so size-contained boxes join the replaced kinds in fetching real
            // facts.
            let size_containment_may_apply = crate::layout::kind_is_box(data.kind) && !data.style.is_null() && {
                let style = self.style_facts(callbacks, node);
                style.has_size_containment() || style.is_size_container()
            };
            if crate::layout::node_may_have_replaced_content_facts(data) || size_containment_may_apply {
                // SAFETY: The callback table and node are supplied by the live
                // C++ formatting-context shim and remain valid for this layout
                // pass.
                unsafe { (callbacks.build_replaced_content_facts)(callbacks.context, callbacks.shell(node)) }
            } else {
                crate::layout::FfiReplacedContentFacts::default()
            }
        })
    }

    pub(crate) fn text_chunks(
        &self,
        callbacks: &FfiLayoutFcCallbacks,
        node: Node,
        should_wrap_lines: bool,
        should_respect_linebreaks: bool,
        unidirectional_ltr: bool,
    ) -> &'static [TextChunk] {
        let parent_style = self.style_facts(callbacks, callbacks.parent(node));
        let key = crate::layout::layout_node_arena::TextChunkCacheKey {
            should_wrap_lines,
            should_respect_linebreaks,
            unidirectional_ltr,
            white_space_collapse: parent_style.white_space_collapse(),
            word_break: parent_style.word_break(),
            font_variant_emoji: parent_style.font_variant_emoji(),
            font_cascade_list: parent_style.font_cascade_list(),
        };
        let text = &callbacks.text_content(node).text;
        callbacks.arena().text_chunks(node, key, || {
            chunk_text(TextChunkInputs {
                text,
                font_cascade_list: key.font_cascade_list,
                white_space_collapse: key.white_space_collapse,
                word_break: key.word_break,
                font_variant_emoji: key.font_variant_emoji,
                should_wrap_lines,
                should_respect_linebreaks,
                unidirectional_ltr,
            })
        })
    }

    /// Accumulates relative-position insets from a chain of inline-flow
    /// ancestors, starting at first_ancestor and walking up until stop_at or
    /// the first ancestor that is not inline-flow.
    pub(crate) fn accumulated_relative_insets_from_inline_ancestor_chain(
        &self,
        records: &RunRecords,
        callbacks: &FfiLayoutFcCallbacks,
        first_ancestor: Node,
        stop_at: Node,
    ) -> InlineAncestorChainRelativeOffset {
        let mut result = InlineAncestorChainRelativeOffset::default();
        let mut ancestor = first_ancestor;
        while !ancestor.is_invalid() && ancestor != stop_at {
            let facts = self.node_facts(callbacks, ancestor);
            if !facts.has_box_model_metrics() {
                break;
            }
            let display = facts.display();
            if !display.is_inline_outside() || !display.is_flow_inside() {
                break;
            }
            result.found_fragmented_inline_node |= facts.is_fragmented_inline();
            if facts.is_relatively_positioned() {
                // A relatively positioned inline-flow ancestor reachable from a
                // committed fragment or piece was entered by its inline
                // formatting context this pass, which created its used values
                // and resolved its insets.
                let used = records.used_values(ancestor);
                result.offset_x += used.inset_left.get();
                result.offset_y += used.inset_top.get();
            }
            ancestor = callbacks.parent(ancestor);
        }
        result
    }

    /// The line box index to record for atomic inlines whose containing line
    /// survived line post-processing.
    /// Converts a placement's line coordinate into the committed line box
    /// index, validated against the containing block's final line data. A
    /// block placed while interrupting inline content has a coordinate whose
    /// fragment joins the line only after the placement.
    fn resolve_containing_line_box_index(
        &self,
        records: &RunRecords,
        callbacks: &FfiLayoutFcCallbacks,
        node: Node,
        containing_block: Node,
        coordinate: Option<LineBoxFragmentCoordinate>,
        placed_offset: FfiCssPixelPoint,
    ) -> Option<usize> {
        let coordinate = coordinate?;
        let facts = self.node_facts(callbacks, node);
        if !facts.is_non_fragmented_box() {
            return None;
        }
        assert!(!containing_block.is_invalid());
        let containing_block_used = records.used_values(containing_block);
        let data = containing_block_used.line_data_ref()?;
        let line = data.line_boxes.get(coordinate.line_box_index)?;
        if let Some(fragment) = line.fragments.get(coordinate.fragment_index) {
            let (x, y) = fragment.offset();
            debug_assert_eq!(
                crate::layout::FfiCssPixelPoint { x, y },
                placed_offset,
                "stored line fragment offset diverged from the placed offset (is_block_outside={})",
                facts.display().is_block_outside()
            );
        }
        Some(coordinate.line_box_index)
    }

    fn commit_subtree(
        &self,
        node: Node,
        parent_paintable: *mut c_void,
        insert_before_paintable: *mut c_void,
        callbacks: &FfiLayoutFcCallbacks,
        sink: &FfiCommitSink,
        commit_index: &std::collections::HashMap<u32, &crate::layout::FragmentLink>,
    ) {
        let slot_index = callbacks.slot_index(node);
        let entry = commit_index.get(&slot_index).copied();
        if let Some(link) = entry {
            callbacks.set_saved_abspos_layout_inputs(node, link.abspos_layout_inputs);
        }
        // SAFETY: The C++ sink owns paintables and copies every plain-data
        // input synchronously.
        let node_shell = callbacks.shell(node);
        let paintable = unsafe { (sink.prepare_node)(sink.context, node_shell, entry.is_some()) };

        let mut has_pending_inline_box_geometry = false;
        if let Some(link) = entry
            && !paintable.is_null()
        {
            let fragment = &link.fragment;
            // SAFETY: Every callback below copies its plain-data argument or
            // consumes one retained handle synchronously.
            unsafe {
                (sink.set_box_metrics)(
                    sink.context,
                    paintable,
                    FfiCommittedBoxMetrics {
                        content_offset: link.offset,
                        content_inline_size: fragment.content_inline_size,
                        content_block_size: fragment.content_block_size,
                        margin_left: fragment.margin_left,
                        margin_right: fragment.margin_right,
                        margin_top: fragment.margin_top,
                        margin_bottom: fragment.margin_bottom,
                        border_left: fragment.border_left,
                        border_right: fragment.border_right,
                        border_top: fragment.border_top,
                        border_bottom: fragment.border_bottom,
                        padding_left: fragment.padding_left,
                        padding_right: fragment.padding_right,
                        padding_top: fragment.padding_top,
                        padding_bottom: fragment.padding_bottom,
                        inset_left: link.inset_left,
                        inset_right: link.inset_right,
                        inset_top: link.inset_top,
                        inset_bottom: link.inset_bottom,
                        containing_line_box_index: link.containing_line_box_index.unwrap_or(0),
                        has_containing_line_box_index: link.containing_line_box_index.is_some(),
                    },
                );
            }

            unsafe {
                if let Some(borders) = fragment.override_borders_data {
                    (sink.set_override_borders)(sink.context, paintable, borders);
                }
                if let Some(coordinates) = fragment.table_cell_coordinates {
                    (sink.set_table_cell_coordinates)(sink.context, paintable, coordinates);
                }
            }

            if let Some(line_data) = &fragment.line_data {
                // SAFETY: The sink keeps one line accumulator live between
                // begin_line_data() and finish_line_data().
                let accepts_lines = unsafe { (sink.begin_line_data)(sink.context, paintable) };
                if accepts_lines {
                    let line_sink = FfiLineSinkCallbacks {
                        context: sink.context,
                        begin_line: sink.begin_line,
                        emit_fragment: sink.emit_fragment,
                        emit_inline_box_piece: sink.emit_inline_box_piece,
                    };
                    push_line_data(line_data, fragment.content_inline_size, callbacks, line_sink);
                    unsafe {
                        (sink.finish_line_data)(sink.context);
                    }
                    has_pending_inline_box_geometry = !line_data.inline_box_pieces.is_empty();
                }
            }

            unsafe {
                if let Some(transforms) = fragment.computed_svg_transforms {
                    (sink.set_computed_svg_transforms)(sink.context, paintable, transforms);
                }
                if let Some(viewport_size) = fragment.svg_viewport_size {
                    (sink.set_svg_viewport_size)(sink.context, paintable, viewport_size);
                }
                if let Some(mut path) = fragment.computed_svg_path.take() {
                    (sink.set_computed_svg_path)(sink.context, paintable, path.take());
                }
            }
            if let Some(data) = &fragment.grid_layout_data {
                data.with_ffi_view(|view| {
                    // SAFETY: The Rust-owned nested vectors remain live
                    // while the commit sink copies this borrowed view.
                    unsafe { (sink.set_grid_layout_data)(sink.context, paintable, view) };
                });
            }
            if let Some(data) = &fragment.flex_layout_data {
                data.with_ffi_view(|view| {
                    // SAFETY: The Rust-owned lines and items remain live
                    // while the commit sink copies this borrowed view.
                    unsafe { (sink.set_flex_layout_data)(sink.context, paintable, view) };
                });
            }
            if let Some(tracks) = &fragment.used_grid_tracks {
                tracks.with_ffi_views(|columns, rows| {
                    // SAFETY: Both Rust-owned track lists remain live
                    // while the commit sink copies these borrowed views.
                    unsafe { (sink.set_used_grid_tracks)(sink.context, paintable, columns, rows) };
                });
            }
        }

        // SAFETY: Wiring uses only live layout and paintable pointers for this
        // synchronous commit.
        let result = unsafe {
            (sink.finish_node)(
                sink.context,
                node_shell,
                paintable,
                parent_paintable,
                insert_before_paintable,
            )
        };
        assert_eq!(result.paintable, paintable);

        let mut child = callbacks.first_child(node);
        while !child.is_invalid() {
            let next = callbacks.next_sibling(child);
            self.commit_subtree(
                child,
                result.paintable_for_children,
                null_mut(),
                callbacks,
                sink,
                commit_index,
            );
            child = next;
        }

        if has_pending_inline_box_geometry {
            // Inline box geometry unites this block's piece rects with the box
            // models of its descendant inline paintables, which exist only now
            // that the whole subtree has committed.
            unsafe { (sink.assign_inline_box_geometry)(sink.context, paintable) };
        }
    }

    pub(crate) fn commit_replacing(
        &self,
        root: Node,
        paintable_to_replace: *mut c_void,
        callbacks: &FfiLayoutFcCallbacks,
        sink: &FfiCommitSink,
        commit_index: &std::collections::HashMap<u32, &crate::layout::FragmentLink>,
    ) {
        // SAFETY: The sink retains the replaced paintable, detaches it, and
        // returns borrowed insertion pointers that stay live until
        // finish_commit().
        let position = unsafe { (sink.begin_commit)(sink.context, callbacks.shell(root), paintable_to_replace) };
        self.commit_subtree(
            root,
            position.parent_paintable,
            position.insert_before_paintable,
            callbacks,
            sink,
            commit_index,
        );
        unsafe {
            (sink.finish_commit)(sink.context);
        }
    }
}


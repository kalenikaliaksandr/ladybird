use crate::node_data::NodeSlotId;
use crate::tree_builder::{
    FfiComputedContentType, FfiElementLayoutFacts, FfiElementLayoutKind, FfiFirstLetterCodePointFacts,
    FfiFirstLetterTextCallbacks, FfiPrincipalBoxPlacement, FfiPrincipalBoxPlacementFacts, FfiPrincipalNodeEntryFacts,
    FfiPseudoElement, FfiPseudoElementDecision, FfiPseudoElementFacts, FfiReplacedElementDisplayAdjustment,
    FirstLetterTextHost, PrincipalBoxGenerationDecision, SvgEntryDecision, TopLayerEntryDecision, TreeBuilderContext,
    adjusted_table_display_for_replaced_element, display_contents_text_needs_style_wrapper, element_layout_kind,
    find_first_letter_in_text, principal_box_generation_decision, principal_box_placement_decision,
    principal_node_entry_decision, pseudo_element_decision,
};
use std::ffi::c_void;

unsafe extern "C" fn text_length(context: *mut c_void) -> usize {
    // SAFETY: Test callers pass a valid `Vec<u16>` as the callback context.
    unsafe { (&*context.cast::<Vec<u16>>()).len() }
}

unsafe extern "C" fn code_point_at(context: *mut c_void, index: usize) -> u32 {
    // SAFETY: Test callers pass a valid `Vec<u16>` and an in-bounds index.
    unsafe { (&*context.cast::<Vec<u16>>())[index] as u32 }
}

unsafe extern "C" fn next_grapheme_boundary(_: *mut c_void, index: usize) -> usize {
    index + 1
}

unsafe extern "C" fn code_point_facts(_: *mut c_void, code_point: u32) -> FfiFirstLetterCodePointFacts {
    FfiFirstLetterCodePointFacts {
        is_space_separator: code_point == b' ' as u32,
        is_punctuation: matches!(code_point, 0x21 | 0x22 | 0x27..=0x2f | 0x3a | 0x3b | 0x3f | 0x40),
        is_letter: matches!(code_point, 0x41..=0x5a | 0x61..=0x7a),
        is_number: matches!(code_point, 0x30..=0x39),
        is_symbol: matches!(code_point, 0x24 | 0x2b | 0x3c..=0x3e | 0x5e | 0x60 | 0x7c | 0x7e),
        is_open_punctuation: matches!(code_point, 0x28 | 0x5b | 0x7b),
        is_dash_punctuation: code_point == 0x2d,
    }
}

fn first_letter_target(text: &str, preserves_segment_breaks: bool) -> crate::tree_builder::FfiFirstLetterTarget {
    let mut text = text.encode_utf16().collect::<Vec<_>>();
    let callbacks = FfiFirstLetterTextCallbacks {
        context: (&raw mut text).cast(),
        code_unit_length: text_length,
        code_point_at,
        next_grapheme_boundary,
        code_point_facts,
    };
    find_first_letter_in_text(&FirstLetterTextHost { callbacks: &callbacks }, preserves_segment_breaks)
}

#[test]
fn replaced_table_display_adjustments() {
    assert_eq!(
        adjusted_table_display_for_replaced_element(true, true, false, false),
        FfiReplacedElementDisplayAdjustment::Block
    );
    assert_eq!(
        adjusted_table_display_for_replaced_element(true, false, false, false),
        FfiReplacedElementDisplayAdjustment::Inline
    );
    assert_eq!(
        adjusted_table_display_for_replaced_element(false, false, true, false),
        FfiReplacedElementDisplayAdjustment::Inline
    );
    assert_eq!(
        adjusted_table_display_for_replaced_element(false, false, false, true),
        FfiReplacedElementDisplayAdjustment::Inline
    );
    assert_eq!(
        adjusted_table_display_for_replaced_element(false, false, false, false),
        FfiReplacedElementDisplayAdjustment::None
    );
}

#[test]
fn first_letter_text_pattern() {
    let target = first_letter_target("  Hello", false);
    assert!(target.found);
    assert_eq!((target.letter_start, target.letter_end), (2, 3));

    let target = first_letter_target("\") A", false);
    assert!(target.found);
    assert_eq!((target.letter_start, target.letter_end), (0, 4));

    let target = first_letter_target("H!ello", false);
    assert!(target.found);
    assert_eq!((target.letter_start, target.letter_end), (0, 2));

    let target = first_letter_target("H-ello", false);
    assert!(target.found);
    assert_eq!((target.letter_start, target.letter_end), (0, 1));

    assert!(!first_letter_target("\nHello", true).found);
}

#[test]
fn tree_builder_state_tracks_ancestors_and_quotes() {
    let mut state = crate::tree_builder::TreeBuilderState::default();
    let parent = NodeSlotId { index: 42 };
    state.ancestor_stack.push(parent);
    assert_eq!(state.ancestor_stack.len(), 1);
    assert_eq!(state.current_parent(), parent);
    assert_eq!(state.ancestor_stack[0], parent);

    state.quote_nesting_level = 3;
    assert_eq!(state.quote_nesting_level, 3);

    assert!(state.ancestor_stack.pop().is_some());
    assert_eq!(state.ancestor_stack.len(), 0);
}

#[test]
fn pseudo_element_box_generation_decisions() {
    let decide = |pseudo_element,
                  content_type,
                  display_is_none,
                  display_is_contents,
                  display_is_list_item,
                  has_content_replacement,
                  originating_layout_node_is_list_item,
                  normal_marker_has_content| {
        pseudo_element_decision(FfiPseudoElementFacts {
            has_style: true,
            pseudo_element,
            content_type,
            display_is_none,
            display_is_contents,
            display_is_list_item,
            has_content_replacement,
            originating_layout_node_is_list_item,
            normal_marker_has_content,
        })
    };

    assert_eq!(
        decide(
            FfiPseudoElement::Before,
            FfiComputedContentType::Normal,
            false,
            false,
            false,
            false,
            false,
            false
        ),
        FfiPseudoElementDecision::None
    );
    assert_eq!(
        decide(
            FfiPseudoElement::Marker,
            FfiComputedContentType::Normal,
            false,
            false,
            false,
            false,
            true,
            true
        ),
        FfiPseudoElementDecision::NormalMarker
    );
    assert_eq!(
        decide(
            FfiPseudoElement::Other,
            FfiComputedContentType::List,
            false,
            false,
            false,
            true,
            false,
            false
        ),
        FfiPseudoElementDecision::ContentReplacement
    );
    assert_eq!(
        decide(
            FfiPseudoElement::Other,
            FfiComputedContentType::List,
            false,
            true,
            false,
            true,
            false,
            false
        ),
        FfiPseudoElementDecision::Contents
    );
    assert_eq!(
        decide(
            FfiPseudoElement::Other,
            FfiComputedContentType::List,
            false,
            false,
            true,
            true,
            false,
            false
        ),
        FfiPseudoElementDecision::Box
    );
}

#[test]
fn principal_node_entry_decisions() {
    let mut facts = FfiPrincipalNodeEntryFacts {
        must_create_subtree: false,
        needs_layout_tree_update: false,
        document_needs_full_layout_tree_update: false,
        is_document: false,
        has_layout_node: true,
        is_element: true,
        is_text: false,
        rendered_in_top_layer: false,
        layout_node_is_attached: true,
        is_svg_container: false,
        requires_svg_container: false,
    };
    let mut context = TreeBuilderContext::default();
    let decision = principal_node_entry_decision(facts, &context);
    assert!(!decision.should_create_layout_node);
    assert_eq!(decision.top_layer, TopLayerEntryDecision::Continue);
    assert_eq!(decision.svg, SvgEntryDecision::Continue);

    facts.rendered_in_top_layer = true;
    facts.layout_node_is_attached = false;
    let decision = principal_node_entry_decision(facts, &context);
    assert_eq!(decision.top_layer, TopLayerEntryDecision::SkipAndRequestZoneRebuild);

    facts.rendered_in_top_layer = false;
    facts.requires_svg_container = true;
    let decision = principal_node_entry_decision(facts, &context);
    assert_eq!(decision.svg, SvgEntryDecision::Skip);

    facts.must_create_subtree = true;
    facts.is_svg_container = true;
    context.has_svg_root = false;
    let decision = principal_node_entry_decision(facts, &context);
    assert!(decision.should_create_layout_node);
    assert_eq!(decision.svg, SvgEntryDecision::EnterSvgRoot);
}

#[test]
fn specialized_element_layout_kinds() {
    let mut facts = FfiElementLayoutFacts {
        has_content_replacement: false,
        is_svg_mask_element: false,
        is_svg_clip_path_element: false,
        is_svg_pattern_element: false,
    };
    assert_eq!(element_layout_kind(facts, false, false), FfiElementLayoutKind::Normal);

    facts.has_content_replacement = true;
    assert_eq!(
        element_layout_kind(facts, false, false),
        FfiElementLayoutKind::ContentReplacement
    );
    facts.has_content_replacement = false;
    facts.is_svg_mask_element = true;
    assert_eq!(element_layout_kind(facts, true, false), FfiElementLayoutKind::SvgMask);

    facts.is_svg_mask_element = false;
    facts.is_svg_pattern_element = true;
    assert_eq!(
        element_layout_kind(facts, false, true),
        FfiElementLayoutKind::SvgPattern
    );
}

#[test]
fn principal_box_generation_and_placement_decisions() {
    assert_eq!(
        principal_box_generation_decision(true, true, false),
        PrincipalBoxGenerationDecision::Suppress
    );
    assert_eq!(
        principal_box_generation_decision(true, false, true),
        PrincipalBoxGenerationDecision::DisplayContents
    );
    assert_eq!(
        principal_box_generation_decision(false, false, false),
        PrincipalBoxGenerationDecision::PrincipalBox
    );

    let mut facts = FfiPrincipalBoxPlacementFacts {
        must_create_subtree: false,
        should_create_layout_node: true,
        has_old_layout_node: true,
        old_layout_node_is_attached: true,
        old_and_new_layout_nodes_are_same: false,
        has_current_rebuild_root: false,
        is_document: false,
        is_element: true,
        rendered_in_top_layer: true,
    };
    let decision = principal_box_placement_decision(facts, false, true);
    assert_eq!(decision.placement, FfiPrincipalBoxPlacement::ReplaceExisting);
    assert!(decision.start_rebuild_root);
    assert!(decision.create_backdrop);
    assert!(decision.clear_layout_top_layer_for_descendants);

    facts.has_old_layout_node = false;
    facts.old_layout_node_is_attached = false;
    let decision = principal_box_placement_decision(facts, true, true);
    assert_eq!(decision.placement, FfiPrincipalBoxPlacement::AppendSvg);
    assert!(decision.mark_update_escaped_rebuild_roots);
}

#[test]
fn display_contents_text_style_wrapper_decisions() {
    assert!(!display_contents_text_needs_style_wrapper(false, true, false, false));
    assert!(display_contents_text_needs_style_wrapper(true, true, false, true));
    assert!(!display_contents_text_needs_style_wrapper(true, true, true, true));
    assert!(display_contents_text_needs_style_wrapper(true, true, true, false));
}

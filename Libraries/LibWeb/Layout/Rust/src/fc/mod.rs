/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

use crate::abort_on_panic;
use crate::box_facts::FfiLayoutBoxFacts;
#[cfg(test)]
use crate::display::FfiDisplay;
use crate::ffi_stats::{FfiOp, bump};

const NO_FORMATTING_CONTEXT: u8 = u8::MAX;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum FfiFormattingContextType {
    Block,
    Inline,
    Flex,
    Grid,
    Table,
    Svg,
    ReplacedWithChildren,
    AbsposReplay,
    InternalReplaced,
    InternalDummy,
}

fn formatting_context_type_created_by_box(facts: FfiLayoutBoxFacts) -> Option<FfiFormattingContextType> {
    if facts.is_svg_svg_box {
        return Some(FfiFormattingContextType::Svg);
    }
    if facts.is_replaced_box_with_children {
        return Some(FfiFormattingContextType::ReplacedWithChildren);
    }
    if facts.is_replaced_box {
        return Some(FfiFormattingContextType::InternalReplaced);
    }
    if !facts.can_have_children {
        return None;
    }

    let display = facts.display;
    if facts.has_replaced_element_table_display_adjustment {
        return Some(if facts.is_block_container {
            FfiFormattingContextType::Block
        } else {
            FfiFormattingContextType::InternalReplaced
        });
    }
    if display.is_flex_inside() {
        return Some(FfiFormattingContextType::Flex);
    }
    if display.is_table_inside() {
        return Some(FfiFormattingContextType::Table);
    }
    if display.is_grid_inside() {
        return Some(FfiFormattingContextType::Grid);
    }
    if display.is_math_inside() {
        return Some(FfiFormattingContextType::Block);
    }
    if facts.creates_block_formatting_context {
        return Some(FfiFormattingContextType::Block);
    }
    if facts.children_are_inline
        || display.is_table_column()
        || display.is_table_row_group()
        || display.is_table_header_group()
        || display.is_table_footer_group()
        || display.is_table_row()
        || display.is_table_column_group()
    {
        return None;
    }
    if !display.is_flow_inside() {
        return Some(FfiFormattingContextType::InternalDummy);
    }
    None
}

#[unsafe(no_mangle)]
pub extern "C" fn rust_layout_owns_fc_type(_fc_type: u8) -> bool {
    abort_on_panic(|| false)
}

#[unsafe(no_mangle)]
pub extern "C" fn rust_layout_formatting_context_type_for_box(facts: FfiLayoutBoxFacts) -> u8 {
    abort_on_panic(|| {
        bump(FfiOp::FcTypeDecision);
        formatting_context_type_created_by_box(facts)
            .map(|type_| type_ as u8)
            .unwrap_or(NO_FORMATTING_CONTEXT)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::css_enums::{display_inside, display_internal, display_outside};

    fn base() -> FfiLayoutBoxFacts {
        FfiLayoutBoxFacts {
            is_box: true,
            is_block_container: true,
            can_have_children: true,
            display: FfiDisplay::block(),
            ..Default::default()
        }
    }

    #[test]
    fn table_flex_grid_and_math_branches() {
        for (inside, expected) in [
            (display_inside::TABLE, FfiFormattingContextType::Table),
            (display_inside::FLEX, FfiFormattingContextType::Flex),
            (display_inside::GRID, FfiFormattingContextType::Grid),
            (display_inside::MATH, FfiFormattingContextType::Block),
        ] {
            let facts = FfiLayoutBoxFacts {
                display: FfiDisplay::outside_and_inside(display_outside::BLOCK, inside, false),
                ..base()
            };
            assert_eq!(formatting_context_type_created_by_box(facts), Some(expected));
        }
    }

    #[test]
    fn svg_and_replaced_branches_precede_display() {
        assert_eq!(
            formatting_context_type_created_by_box(FfiLayoutBoxFacts {
                is_svg_svg_box: true,
                ..base()
            }),
            Some(FfiFormattingContextType::Svg)
        );
        assert_eq!(
            formatting_context_type_created_by_box(FfiLayoutBoxFacts {
                is_replaced_box_with_children: true,
                ..base()
            }),
            Some(FfiFormattingContextType::ReplacedWithChildren)
        );
        assert_eq!(
            formatting_context_type_created_by_box(FfiLayoutBoxFacts {
                is_replaced_box: true,
                ..base()
            }),
            Some(FfiFormattingContextType::InternalReplaced)
        );
    }

    #[test]
    fn block_and_none_branches() {
        assert_eq!(
            formatting_context_type_created_by_box(FfiLayoutBoxFacts {
                creates_block_formatting_context: true,
                ..base()
            }),
            Some(FfiFormattingContextType::Block)
        );
        assert_eq!(
            formatting_context_type_created_by_box(FfiLayoutBoxFacts {
                children_are_inline: true,
                ..base()
            }),
            None
        );
        assert_eq!(
            formatting_context_type_created_by_box(FfiLayoutBoxFacts {
                can_have_children: false,
                ..base()
            }),
            None
        );
    }

    #[test]
    fn table_parts_and_unknown_non_flow_inside_match_cpp() {
        let row = FfiLayoutBoxFacts {
            display: FfiDisplay::internal(display_internal::TABLE_ROW),
            ..base()
        };
        assert_eq!(formatting_context_type_created_by_box(row), None);

        let ruby = FfiLayoutBoxFacts {
            display: FfiDisplay::outside_and_inside(display_outside::BLOCK, display_inside::RUBY, false),
            ..base()
        };
        assert_eq!(
            formatting_context_type_created_by_box(ruby),
            Some(FfiFormattingContextType::InternalDummy)
        );
    }

    #[test]
    fn rust_owns_no_formatting_context_types_yet() {
        for type_ in 0..=u8::MAX {
            assert!(!rust_layout_owns_fc_type(type_));
        }
    }
}

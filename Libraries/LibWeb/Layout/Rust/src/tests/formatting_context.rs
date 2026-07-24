use crate::box_facts::FfiLayoutBoxFacts;
use crate::css_enums::{display_inside, display_internal, display_outside};
use crate::display::FfiDisplay;
use crate::formatting_context::*;

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
fn rust_owns_native_formatting_contexts_and_abspos_replay() {
    for type_ in 0..=u8::MAX {
        assert_eq!(
            rust_layout_owns_fc_type(type_),
            type_ == FfiFormattingContextType::Block as u8
                || type_ == FfiFormattingContextType::Flex as u8
                || type_ == FfiFormattingContextType::Table as u8
                || type_ == FfiFormattingContextType::Grid as u8
                || type_ == FfiFormattingContextType::Svg as u8
                || type_ == FfiFormattingContextType::AbsposReplay as u8
        );
    }
}

mod abspos {
    use crate::css_pixels::CssPixels;
    use crate::formatting_context::abspos::*;
    use crate::geometry::{LogicalOffset, LogicalRect, LogicalSize};
    use crate::layout_state::{FfiStaticPositionAlignment, FfiStaticPositionRect};
    use crate::used_values::{FfiCssPixelPoint, UsedValuesCore};

    fn px(value: i64) -> CssPixels {
        CssPixels::from_integer(value)
    }

    fn static_rect(
        x: i64,
        y: i64,
        width: i64,
        height: i64,
        inline_alignment: FfiStaticPositionAlignment,
        block_alignment: FfiStaticPositionAlignment,
    ) -> FfiStaticPositionRect {
        FfiStaticPositionRect {
            rect: LogicalRect {
                offset: LogicalOffset {
                    inline_offset: px(x),
                    block_offset: px(y),
                },
                size: LogicalSize {
                    inline_size: px(width),
                    block_size: px(height),
                },
            },
            inline_alignment,
            block_alignment,
            alignment_derives_from_own_computed_values: false,
        }
    }

    #[test]
    fn aligned_static_offsets_cover_center_and_end() {
        let offset = aligned_static_offset(
            static_rect(
                10,
                20,
                100,
                80,
                FfiStaticPositionAlignment::Center,
                FfiStaticPositionAlignment::End,
            ),
            px(20),
            px(10),
        );
        assert_eq!(offset.inline_offset, px(50));
        assert_eq!(offset.block_offset, px(90));
    }

    #[test]
    fn static_position_merge_translation_subtracts_the_actual_chain() {
        let translated = translate_static_position_between_chains(
            static_rect(
                10,
                20,
                1,
                1,
                FfiStaticPositionAlignment::Start,
                FfiStaticPositionAlignment::Start,
            ),
            FfiCssPixelPoint { x: px(40), y: px(50) },
            FfiCssPixelPoint { x: px(15), y: px(12) },
        );
        assert_eq!(translated.rect.offset.inline_offset, px(35));
        assert_eq!(translated.rect.offset.block_offset, px(58));
    }

    #[test]
    fn anchor_rect_uses_border_box_and_containing_block_padding() {
        let anchor = UsedValuesCore {
            content_inline_size: px(30),
            content_block_size: px(20),
            border_left: px(2),
            border_right: px(3),
            border_top: px(4),
            border_bottom: px(5),
            padding_left: px(6),
            padding_right: px(7),
            padding_top: px(8),
            padding_bottom: px(9),
            ..Default::default()
        };
        let containing_block = UsedValuesCore {
            padding_left: px(11),
            padding_top: px(13),
            ..Default::default()
        };

        let rect = anchor_rect_from_geometry(&anchor, &containing_block, FfiCssPixelPoint { x: px(100), y: px(200) });
        assert_eq!(
            rect,
            PhysicalRect {
                x: px(103),
                y: px(201),
                width: px(48),
                height: px(46),
            }
        );
    }

    #[test]
    fn non_replaced_axis_equation_solves_size_and_clamps_negative_size() {
        let size = solve_abspos_axis_for(
            px(200),
            None,
            true,
            Some(px(10)),
            Some(px(5)),
            px(2),
            px(4),
            None,
            px(6),
            px(3),
            Some(px(7)),
            Some(px(11)),
        );
        assert_eq!(size, px(152));

        let clamped = solve_abspos_axis_for(
            px(20),
            None,
            true,
            Some(px(10)),
            Some(px(5)),
            px(2),
            px(4),
            None,
            px(6),
            px(3),
            Some(px(7)),
            Some(px(11)),
        );
        assert_eq!(clamped, px(0));
    }

    #[test]
    fn non_replaced_over_constrained_axis_recomputes_the_end_inset() {
        let end = solve_abspos_axis_for(
            px(200),
            Some(px(30)),
            false,
            Some(px(10)),
            Some(px(5)),
            px(0),
            px(0),
            Some(px(100)),
            px(0),
            px(0),
            Some(px(7)),
            Some(px(30)),
        );
        assert_eq!(end, px(78));
    }

    #[test]
    fn replaced_axis_covers_auto_margins_static_position_and_over_constraint() {
        let inline_behavior = ReplacedAxisBehavior {
            clear_auto_margins_if_start_is_auto: true,
            clear_negative_auto_margins: true,
        };
        let centered = solve_replaced_axis(px(200), Some(px(10)), Some(px(20)), None, None, px(0), inline_behavior);
        assert_eq!(centered.margin_start, px(85));
        assert_eq!(centered.margin_end, px(85));

        let static_position = solve_replaced_axis(px(200), None, None, None, None, px(30), inline_behavior);
        assert_eq!(static_position.start, px(30));
        assert_eq!(static_position.end, px(170));
        assert_eq!(static_position.margin_start, px(0));
        assert_eq!(static_position.margin_end, px(0));

        let over_constrained = solve_replaced_axis(
            px(200),
            Some(px(10)),
            Some(px(20)),
            Some(px(5)),
            Some(px(6)),
            px(0),
            inline_behavior,
        );
        assert_eq!(over_constrained.end, px(179));
    }

    #[test]
    fn replaced_variants_preserve_the_cpp_negative_margin_difference() {
        let inline = solve_replaced_axis(
            px(50),
            Some(px(40)),
            Some(px(20)),
            None,
            None,
            px(0),
            ReplacedAxisBehavior {
                clear_auto_margins_if_start_is_auto: true,
                clear_negative_auto_margins: true,
            },
        );
        assert_eq!((inline.margin_start, inline.margin_end), (px(0), px(0)));
        assert_eq!(inline.end, px(10));

        let block = solve_replaced_axis(
            px(50),
            Some(px(40)),
            Some(px(20)),
            None,
            None,
            px(0),
            ReplacedAxisBehavior {
                clear_auto_margins_if_start_is_auto: false,
                clear_negative_auto_margins: false,
            },
        );
        assert_eq!((block.margin_start, block.margin_end), (px(-5), px(-5)));
        assert_eq!(block.end, px(20));
    }
}

mod sizing {
    use crate::css_pixels::CssPixels;
    use crate::formatting_context::sizing::*;
    use crate::geometry::AvailableSize;

    fn px(value: i64) -> CssPixels {
        CssPixels::from_integer(value)
    }

    #[test]
    fn cyclic_percentage_rules_match_property_axis_and_replaced_status() {
        assert_eq!(
            cyclic_percentage_intrinsic_contribution(
                false,
                true,
                AvailableSize::min_content(),
                CyclicPercentageSizeProperty::PreferredOrMaxSize,
            ),
            CyclicPercentageIntrinsicContribution::TreatAsInitialValue
        );
        assert_eq!(
            cyclic_percentage_intrinsic_contribution(
                true,
                true,
                AvailableSize::min_content(),
                CyclicPercentageSizeProperty::PreferredOrMaxSize,
            ),
            CyclicPercentageIntrinsicContribution::ResolveAsZero
        );
        assert_eq!(
            cyclic_percentage_intrinsic_contribution(
                false,
                true,
                AvailableSize::max_content(),
                CyclicPercentageSizeProperty::MinSize,
            ),
            CyclicPercentageIntrinsicContribution::ResolveAsZero
        );
        assert_eq!(
            cyclic_percentage_intrinsic_contribution(
                true,
                false,
                AvailableSize::min_content(),
                CyclicPercentageSizeProperty::PreferredOrMaxSize,
            ),
            CyclicPercentageIntrinsicContribution::NotCyclic
        );
    }

    #[test]
    fn border_box_adjustment_preserves_cpp_operation_order_and_floor() {
        assert_eq!(
            subtract_border_box_adjustment(px(100), px(3), px(5), px(7), px(11)),
            px(74)
        );
        assert_eq!(
            subtract_border_box_adjustment(px(10), px(3), px(5), px(7), px(11)),
            CssPixels::default()
        );
    }

    #[test]
    fn aspect_ratio_transfer_accounts_for_border_box_edges() {
        let ratio = PixelFraction {
            numerator: px(2),
            denominator: px(1),
        };
        assert_eq!(
            content_block_size_from_aspect_ratio_values(px(80), ratio, true, px(10), px(10), px(5), px(5),),
            px(40)
        );
        assert_eq!(
            content_inline_size_from_aspect_ratio_values(px(40), ratio, true, px(10), px(10), px(5), px(5),),
            px(80)
        );
    }
}

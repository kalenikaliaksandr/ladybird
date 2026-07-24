/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

use crate::css_enums::{align_content, align_items, align_self, justify_content, justify_items, justify_self};
use crate::css_pixels::CssPixels;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Alignment {
    Normal,
    Start,
    End,
    Center,
    Stretch,
    Baseline,
    SelfStart,
    SelfEnd,
    Safe,
    Unsafe,
    SpaceBetween,
    SpaceAround,
    SpaceEvenly,
}

pub(crate) fn inline_item_alignment(justify_self_value: u8, container_justify_items: u8) -> Alignment {
    match justify_self_value {
        justify_self::AUTO => match container_justify_items {
            justify_items::BASELINE => Alignment::Baseline,
            justify_items::CENTER => Alignment::Center,
            justify_items::END | justify_items::FLEX_END | justify_items::RIGHT => Alignment::End,
            justify_items::FLEX_START | justify_items::START | justify_items::LEFT => Alignment::Start,
            justify_items::NORMAL | justify_items::LEGACY => Alignment::Normal,
            justify_items::SAFE => Alignment::Safe,
            justify_items::SELF_END => Alignment::SelfEnd,
            justify_items::SELF_START => Alignment::SelfStart,
            justify_items::STRETCH => Alignment::Stretch,
            justify_items::UNSAFE => Alignment::Unsafe,
            _ => unreachable!("invalid justify-items value"),
        },
        justify_self::BASELINE => Alignment::Baseline,
        justify_self::CENTER => Alignment::Center,
        justify_self::END | justify_self::FLEX_END | justify_self::RIGHT => Alignment::End,
        justify_self::FLEX_START | justify_self::START | justify_self::LEFT => Alignment::Start,
        justify_self::NORMAL => Alignment::Normal,
        justify_self::SAFE => Alignment::Safe,
        justify_self::SELF_END => Alignment::SelfEnd,
        justify_self::SELF_START => Alignment::SelfStart,
        justify_self::STRETCH => Alignment::Stretch,
        justify_self::UNSAFE => Alignment::Unsafe,
        _ => unreachable!("invalid justify-self value"),
    }
}

pub(crate) fn block_item_alignment(align_self_value: u8, container_align_items: u8) -> Alignment {
    match align_self_value {
        align_self::AUTO => match container_align_items {
            align_items::BASELINE => Alignment::Baseline,
            align_items::CENTER => Alignment::Center,
            align_items::END | align_items::FLEX_END => Alignment::End,
            align_items::FLEX_START | align_items::START => Alignment::Start,
            align_items::NORMAL => Alignment::Normal,
            align_items::SAFE => Alignment::Safe,
            align_items::SELF_END => Alignment::SelfEnd,
            align_items::SELF_START => Alignment::SelfStart,
            align_items::STRETCH => Alignment::Stretch,
            align_items::UNSAFE => Alignment::Unsafe,
            _ => unreachable!("invalid align-items value"),
        },
        align_self::BASELINE => Alignment::Baseline,
        align_self::CENTER => Alignment::Center,
        align_self::END | align_self::FLEX_END => Alignment::End,
        align_self::FLEX_START | align_self::START => Alignment::Start,
        align_self::NORMAL => Alignment::Normal,
        align_self::SAFE => Alignment::Safe,
        align_self::SELF_END => Alignment::SelfEnd,
        align_self::SELF_START => Alignment::SelfStart,
        align_self::STRETCH => Alignment::Stretch,
        align_self::UNSAFE => Alignment::Unsafe,
        _ => unreachable!("invalid align-self value"),
    }
}

pub(crate) fn inline_content_alignment(value: u8) -> Alignment {
    match value {
        justify_content::NORMAL => Alignment::Normal,
        justify_content::START | justify_content::FLEX_START | justify_content::LEFT => Alignment::Start,
        justify_content::END | justify_content::FLEX_END | justify_content::RIGHT => Alignment::End,
        justify_content::CENTER => Alignment::Center,
        justify_content::SPACE_BETWEEN => Alignment::SpaceBetween,
        justify_content::SPACE_AROUND => Alignment::SpaceAround,
        justify_content::SPACE_EVENLY => Alignment::SpaceEvenly,
        justify_content::STRETCH => Alignment::Stretch,
        _ => unreachable!("invalid justify-content value"),
    }
}

pub(crate) fn block_content_alignment(value: u8) -> Alignment {
    match value {
        align_content::NORMAL => Alignment::Normal,
        align_content::START | align_content::FLEX_START => Alignment::Start,
        align_content::END | align_content::FLEX_END => Alignment::End,
        align_content::CENTER => Alignment::Center,
        align_content::SPACE_BETWEEN => Alignment::SpaceBetween,
        align_content::SPACE_AROUND => Alignment::SpaceAround,
        align_content::SPACE_EVENLY => Alignment::SpaceEvenly,
        align_content::STRETCH => Alignment::Stretch,
        _ => unreachable!("invalid align-content value"),
    }
}

pub(crate) fn content_start_offset(
    alignment: Alignment,
    container_size: CssPixels,
    tracks_and_gaps_size: CssPixels,
) -> CssPixels {
    let free_space = container_size - tracks_and_gaps_size;
    // CSS Align's automatic overflow alignment is unsafe for grid content
    // alignment, so preserve negative free space here.
    match alignment {
        Alignment::Center => free_space / 2,
        Alignment::SpaceAround | Alignment::SpaceEvenly => CssPixels::default().max(free_space) / 2,
        Alignment::End => free_space,
        _ => CssPixels::default(),
    }
}

pub(crate) fn distributed_gap_size(
    alignment: Alignment,
    container_size: CssPixels,
    track_size_sum: CssPixels,
    gap_count: usize,
    minimum_gap: CssPixels,
) -> CssPixels {
    if gap_count == 0 {
        return CssPixels::default();
    }
    let available = CssPixels::default().max(container_size - track_size_sum);
    let distributed = match alignment {
        Alignment::SpaceBetween => available / gap_count,
        Alignment::SpaceAround => available / gap_count.saturating_add(1),
        Alignment::SpaceEvenly => available / gap_count.saturating_add(2),
        _ => CssPixels::default(),
    };
    distributed.max(minimum_gap)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ItemAlignment {
    pub(crate) margin_start: CssPixels,
    pub(crate) margin_end: CssPixels,
    pub(crate) size: CssPixels,
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn align_item(
    original_size: CssPixels,
    size_is_auto: bool,
    is_replaced: bool,
    containing_block_size: CssPixels,
    margin_box_start: CssPixels,
    margin_box_end: CssPixels,
    used_margin_start: CssPixels,
    used_margin_end: CssPixels,
    margin_start_is_auto: bool,
    margin_end_is_auto: bool,
    alignment: Alignment,
) -> ItemAlignment {
    let mut result = ItemAlignment {
        margin_start: used_margin_start,
        margin_end: used_margin_end,
        size: original_size,
    };
    // https://drafts.csswg.org/css-grid/#auto-margins
    // Auto margins in either axis absorb positive free space prior to alignment via the box alignment
    // properties, thereby disabling the effects of any self-alignment properties in that axis.
    // Overflowing grid items resolve their auto margins to zero and overflow as specified by their box
    // alignment properties.
    let margin_space = containing_block_size - result.size - margin_box_start - margin_box_end;
    let absorbed = CssPixels::default().max(margin_space);
    if margin_start_is_auto && margin_end_is_auto {
        result.margin_start = absorbed / 2;
        result.margin_end = absorbed / 2;
    } else if margin_start_is_auto {
        result.margin_start = absorbed;
    } else if margin_end_is_auto {
        result.margin_end = absorbed;
    } else if size_is_auto && !is_replaced {
        result.size += margin_space;
    }

    // If auto margins absorbed positive free space, alignment properties have no effect in this dimension.
    if (margin_start_is_auto || margin_end_is_auto) && margin_space > CssPixels::default() {
        return result;
    }

    let alignment_space = containing_block_size - original_size - margin_box_start - margin_box_end;
    match alignment {
        Alignment::Center => {
            result.margin_start += alignment_space / 2;
            result.margin_end += alignment_space / 2;
            result.size = original_size;
        }
        Alignment::Baseline | Alignment::Start => {
            result.margin_end += alignment_space;
            result.size = original_size;
        }
        Alignment::End => {
            result.margin_start += alignment_space;
            result.size = original_size;
        }
        _ => {}
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn px(value: i64) -> CssPixels {
        CssPixels::from_integer(value)
    }

    #[test]
    fn auto_margins_absorb_positive_space_before_self_alignment() {
        let aligned = align_item(
            px(40),
            false,
            false,
            px(100),
            px(0),
            px(0),
            px(0),
            px(0),
            true,
            true,
            Alignment::End,
        );
        assert_eq!(aligned.margin_start, px(30));
        assert_eq!(aligned.margin_end, px(30));
        assert_eq!(aligned.size, px(40));
    }

    #[test]
    fn overflowing_center_alignment_preserves_negative_free_space() {
        let aligned = align_item(
            px(120),
            false,
            false,
            px(100),
            px(0),
            px(0),
            px(0),
            px(0),
            false,
            false,
            Alignment::Center,
        );
        assert_eq!(aligned.margin_start, px(-10));
        assert_eq!(aligned.margin_end, px(-10));
    }

    #[test]
    fn content_distribution_uses_cpp_gap_denominators() {
        assert_eq!(
            distributed_gap_size(Alignment::SpaceBetween, px(300), px(180), 2, px(10)),
            px(60)
        );
        assert_eq!(
            distributed_gap_size(Alignment::SpaceAround, px(300), px(180), 2, px(10)),
            px(40)
        );
        assert_eq!(
            distributed_gap_size(Alignment::SpaceEvenly, px(300), px(180), 2, px(10)),
            px(30)
        );
    }
}

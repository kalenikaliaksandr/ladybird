use crate::css_enums::justify_content;
use crate::css_pixels::CssPixels;
use crate::formatting_context::flex::*;
use std::collections::HashMap;

fn px(value: i64) -> CssPixels {
    CssPixels::from_integer(value)
}

#[derive(Clone, Copy)]
struct FixtureItem {
    base: CssPixels,
    hypothetical: CssPixels,
    min: CssPixels,
    max: CssPixels,
    factor: f64,
    target: CssPixels,
    frozen: bool,
}

fn resolve_growing_fixture(container: CssPixels, items: &mut [FixtureItem]) {
    for item in items.iter_mut() {
        item.target = item.base;
        item.frozen = item.factor == 0.0 || item.base > item.hypothetical;
        if item.frozen {
            item.target = item.hypothetical;
        }
    }
    while items.iter().any(|item| !item.frozen) {
        let occupied = items.iter().fold(CssPixels::default(), |sum, item| {
            sum + if item.frozen { item.target } else { item.base }
        });
        let free = container - occupied;
        let factor_sum: f64 = items.iter().filter(|item| !item.frozen).map(|item| item.factor).sum();
        for item in items.iter_mut().filter(|item| !item.frozen) {
            item.target = item.base + free.scaled(item.factor / factor_sum);
        }
        let mut violation = CssPixels::default();
        let mut min_violations = vec![false; items.len()];
        let mut max_violations = vec![false; items.len()];
        for (index, item) in items.iter_mut().enumerate() {
            if item.frozen {
                continue;
            }
            let original = item.target;
            item.target = css_clamp(original, item.min, item.max).max(CssPixels::default());
            min_violations[index] = item.target > original;
            max_violations[index] = item.target < original;
            violation += item.target - original;
        }
        for (index, item) in items.iter_mut().enumerate() {
            if item.frozen {
                continue;
            }
            item.frozen = violation == CssPixels::default()
                || (violation > CssPixels::default() && min_violations[index])
                || (violation < CssPixels::default() && max_violations[index]);
        }
    }
}

fn main_distribution(
    justify: u8,
    reverse: bool,
    container: CssPixels,
    used: CssPixels,
    remaining: Option<CssPixels>,
    count: usize,
) -> (CssPixels, CssPixels) {
    let mut initial = CssPixels::default();
    let mut between = CssPixels::default();
    match justify {
        justify_content::CENTER => {
            initial = (container - used) / 2;
            if reverse {
                initial = container - initial;
            }
        }
        justify_content::SPACE_BETWEEN => {
            if reverse {
                initial = container;
            }
            if let Some(free) = remaining
                && count > 1
            {
                between = (free / (count - 1)).max(CssPixels::default());
            }
        }
        justify_content::SPACE_AROUND => {
            if let Some(free) = remaining {
                between = (free / count).max(CssPixels::default());
            }
            initial = if reverse { container - between / 2 } else { between / 2 };
        }
        _ => {}
    }
    (initial, between)
}

#[test]
fn flexible_length_loop_refreezes_after_a_max_violation() {
    let mut items = [
        FixtureItem {
            base: px(50),
            hypothetical: px(50),
            min: px(0),
            max: px(80),
            factor: 1.0,
            target: px(0),
            frozen: false,
        },
        FixtureItem {
            base: px(50),
            hypothetical: px(50),
            min: px(0),
            max: px(500),
            factor: 1.0,
            target: px(0),
            frozen: false,
        },
    ];
    resolve_growing_fixture(px(300), &mut items);
    assert_eq!(items[0].target, px(80));
    assert_eq!(items[1].target, px(220));
    assert!(items.iter().all(|item| item.frozen));
}

#[test]
fn main_alignment_distribution_handles_reverse_and_negative_space() {
    assert_eq!(
        main_distribution(justify_content::CENTER, true, px(100), px(40), Some(px(60)), 2),
        (px(70), px(0))
    );
    assert_eq!(
        main_distribution(
            justify_content::SPACE_BETWEEN,
            false,
            px(100),
            px(120),
            Some(px(-20)),
            3
        ),
        (px(0), px(0))
    );
    assert_eq!(
        main_distribution(justify_content::SPACE_AROUND, false, px(100), px(40), Some(px(60)), 3),
        (px(10), px(20))
    );
}

#[test]
fn order_modified_buckets_keep_document_order_within_equal_orders() {
    let mut buckets = HashMap::new();
    buckets.insert(2, vec!['a', 'c']);
    buckets.insert(1, vec!['b']);
    let collect = |reverse| {
        order_modified_keys(&buckets, reverse)
            .into_iter()
            .flat_map(|key| buckets[&key].iter().copied())
            .collect::<Vec<_>>()
    };
    assert_eq!(collect(false), ['b', 'a', 'c']);
    assert_eq!(collect(true), ['a', 'c', 'b']);
}

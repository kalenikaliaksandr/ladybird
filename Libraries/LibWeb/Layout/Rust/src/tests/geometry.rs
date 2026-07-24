use crate::css_pixels::CssPixels;
use crate::geometry::*;

fn px(raw: i32) -> CssPixels {
    CssPixels::from_raw(raw)
}

#[test]
fn equality_compares_both_tag_and_stored_value() {
    assert_eq!(AvailableSize::definite(px(17)), AvailableSize::definite(px(17)));
    assert_ne!(AvailableSize::definite(px(17)), AvailableSize::definite(px(18)));
    assert_ne!(AvailableSize::min_content(), AvailableSize::definite(px(0)));
    assert_ne!(AvailableSize::indefinite(), AvailableSize::max_content());
}

#[test]
fn available_size_less_than_compares_the_stored_pixel_value() {
    let sizes = [
        AvailableSize::definite(px(-64)),
        AvailableSize::definite(px(64)),
        AvailableSize::min_content(),
        AvailableSize::max_content(),
        AvailableSize::indefinite(),
    ];
    for left in sizes {
        for right in sizes {
            assert_eq!(left.less_than(right), left.value < right.value);
        }
    }
}

#[test]
fn mixed_comparisons_match_cpp_special_values() {
    let values = [px(i32::MIN), px(-1), px(0), px(1), px(i32::MAX)];
    for value in values {
        assert!(!AvailableSize::max_content().pixels_greater_than(value));
        assert!(!AvailableSize::indefinite().pixels_greater_than(value));
        assert!(AvailableSize::min_content().pixels_greater_than(value));

        assert!(AvailableSize::max_content().pixels_less_than(value));
        assert!(AvailableSize::indefinite().pixels_less_than(value));
        assert!(!AvailableSize::min_content().pixels_less_than(value));

        assert!(!AvailableSize::max_content().less_than_pixels(value));
        assert!(!AvailableSize::indefinite().less_than_pixels(value));
        assert!(AvailableSize::min_content().less_than_pixels(value));
    }
}

use crate::css_pixels::CssPixels;
use crate::formatting_context::svg::*;
use crate::used_values::FfiCssPixelPoint;

fn px(value: i64) -> CssPixels {
    CssPixels::from_integer(value)
}

#[test]
fn meet_and_slice_choose_uniform_scale_and_align() {
    let view_box = FfiSvgViewBox {
        width: 100.0,
        height: 100.0,
        ..Default::default()
    };
    let meet = scale_and_align_viewbox_content(
        PRESERVE_ASPECT_RATIO_X_MID_Y_MID,
        MEET_OR_SLICE_MEET,
        view_box,
        2.0,
        1.0,
        (px(200), px(100)),
        true,
    );
    assert_eq!(meet.scale_factor_x, 1.0);
    assert_eq!(meet.scale_factor_y, 1.0);
    assert_eq!(meet.offset.x, px(50));
    assert_eq!(meet.offset.y, px(0));

    let slice = scale_and_align_viewbox_content(
        PRESERVE_ASPECT_RATIO_X_MAX_Y_MAX,
        MEET_OR_SLICE_SLICE,
        view_box,
        2.0,
        1.0,
        (px(200), px(100)),
        true,
    );
    assert_eq!(slice.scale_factor_x, 2.0);
    assert_eq!(slice.scale_factor_y, 2.0);
    assert_eq!(slice.offset.x, px(0));
    assert_eq!(slice.offset.y, px(-100));
}

#[test]
fn none_keeps_non_uniform_scale_and_zero_offset() {
    let transform = scale_and_align_viewbox_content(
        PRESERVE_ASPECT_RATIO_NONE,
        MEET_OR_SLICE_MEET,
        FfiSvgViewBox {
            width: 10.0,
            height: 20.0,
            ..Default::default()
        },
        3.0,
        4.0,
        (px(30), px(80)),
        true,
    );
    assert_eq!(transform.scale_factor_x, 3.0);
    assert_eq!(transform.scale_factor_y, 4.0);
    assert_eq!(transform.offset, FfiCssPixelPoint::default());
}

#[test]
fn affine_multiplication_and_rect_mapping_match_gfx_order() {
    let mut transform = FfiAffineTransform::default().translated(10.0, 20.0).scaled(2.0, 3.0);
    transform.multiply(FfiAffineTransform {
        a: 1.0,
        b: 0.0,
        c: 0.0,
        d: 1.0,
        e: 4.0,
        f: 5.0,
    });
    let rect = transform.map_rect(FfiFloatRect {
        x: 1.0,
        y: 2.0,
        width: 3.0,
        height: 4.0,
    });
    assert_eq!(
        rect,
        FfiFloatRect {
            x: 20.0,
            y: 41.0,
            width: 6.0,
            height: 12.0,
        }
    );
}

#[test]
fn y_alignment_preserves_inline_definiteness_quirk() {
    let transform = scale_and_align_viewbox_content(
        PRESERVE_ASPECT_RATIO_X_MID_Y_MAX,
        MEET_OR_SLICE_MEET,
        FfiSvgViewBox {
            width: 100.0,
            height: 50.0,
            ..Default::default()
        },
        1.0,
        1.0,
        (px(100), px(100)),
        false,
    );
    assert_eq!(transform.offset.y, CssPixels::default());
}

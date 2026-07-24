use crate::css_pixels::CssPixels;
use crate::style_facts::*;

#[test]
fn pure_size_values_keep_inactive_payloads_zeroed() {
    let px = FfiSizeValue::px(CssPixels::from_raw(-65));
    assert_eq!(px.kind, FfiSizeKind::Px as u8);
    assert_eq!(px.px.raw_value(), -65);
    assert_eq!(px.fraction, 0.0);
    assert!(px.calc.is_null());

    let percentage = FfiSizeValue::percentage(-0.25);
    assert_eq!(percentage.kind, FfiSizeKind::Percentage as u8);
    assert_eq!(percentage.px.raw_value(), 0);
    assert_eq!(percentage.fraction, -0.25);
    assert!(percentage.calc.is_null());
}

#[test]
fn size_kind_values_are_pinned() {
    assert_eq!(FfiSizeKind::Auto as u8, 0);
    assert_eq!(FfiSizeKind::Px as u8, 1);
    assert_eq!(FfiSizeKind::Percentage as u8, 2);
    assert_eq!(FfiSizeKind::Calc as u8, 3);
    assert_eq!(FfiSizeKind::MinContent as u8, 4);
    assert_eq!(FfiSizeKind::MaxContent as u8, 5);
    assert_eq!(FfiSizeKind::FitContent as u8, 6);
    assert_eq!(FfiSizeKind::None_ as u8, 7);
}

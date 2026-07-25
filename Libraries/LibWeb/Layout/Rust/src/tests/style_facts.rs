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

#[test]
fn registered_schema_reads_synthetic_payloads() {
    #[repr(C)]
    struct SyntheticPayload {
        byte: u8,
        boolean: bool,
        integer: i32,
        double: f64,
        pixels: i32,
    }

    let mut schema = [FfiStyleFieldSchema {
        field: FfiStyleField::Width,
        group_index: 0,
        offset: 0,
        group_size: size_of::<SyntheticPayload>() as u32,
        encoding: FfiStyleFieldEncoding::Lazy,
    }; STYLE_FIELD_COUNT];
    for (index, entry) in schema.iter_mut().enumerate() {
        entry.field = unsafe { std::mem::transmute::<u8, FfiStyleField>(index as u8) };
    }
    for (field, offset, encoding) in [
        (
            FfiStyleField::Position,
            std::mem::offset_of!(SyntheticPayload, byte),
            FfiStyleFieldEncoding::U8,
        ),
        (
            FfiStyleField::GridAutoFlowRow,
            std::mem::offset_of!(SyntheticPayload, boolean),
            FfiStyleFieldEncoding::Bool,
        ),
        (
            FfiStyleField::Order,
            std::mem::offset_of!(SyntheticPayload, integer),
            FfiStyleFieldEncoding::I32,
        ),
        (
            FfiStyleField::FlexGrow,
            std::mem::offset_of!(SyntheticPayload, double),
            FfiStyleFieldEncoding::F64,
        ),
        (
            FfiStyleField::FontSize,
            std::mem::offset_of!(SyntheticPayload, pixels),
            FfiStyleFieldEncoding::CssPixels,
        ),
    ] {
        let entry = &mut schema[field as usize];
        entry.offset = offset as u32;
        entry.encoding = encoding;
    }

    unsafe {
        rust_layout_register_style_schema(schema.as_ptr(), schema.len());
    }
    let payload = SyntheticPayload {
        byte: 17,
        boolean: true,
        integer: -123,
        double: 2.5,
        pixels: -65,
    };
    let pointer = std::ptr::from_ref(&payload).cast();
    let reader = StyleReader::new(FfiStylePayloads {
        groups: [pointer; STYLE_GROUP_COUNT],
    });
    assert_eq!(reader.u8(FfiStyleField::Position), 17);
    assert!(reader.bool(FfiStyleField::GridAutoFlowRow));
    assert_eq!(reader.i32(FfiStyleField::Order), -123);
    assert_eq!(reader.f64(FfiStyleField::FlexGrow), 2.5);
    assert_eq!(reader.css_pixels(FfiStyleField::FontSize).raw_value(), -65);
}

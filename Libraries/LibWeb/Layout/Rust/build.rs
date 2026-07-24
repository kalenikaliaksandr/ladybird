/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

use std::env;
use std::error::Error;
use std::path::{Path, PathBuf};

fn generate_ffi_header(
    config: cbindgen::Config,
    sources: &[PathBuf],
    out_dir: &Path,
    ffi_out_dir: &Path,
    header: &Path,
) {
    let builder = sources
        .iter()
        .fold(cbindgen::Builder::new().with_config(config), |builder, source| {
            builder.with_src(source)
        });
    builder.generate().map_or_else(
        |error| match error {
            cbindgen::Error::ParseSyntaxError { .. } => {}
            other => panic!("{other:?}"),
        },
        |bindings| {
            let output_header = out_dir.join(header);
            std::fs::create_dir_all(output_header.parent().unwrap()).unwrap();
            bindings.write_to_file(output_header);
            if ffi_out_dir != out_dir {
                let ffi_header = ffi_out_dir.join(header);
                std::fs::create_dir_all(ffi_header.parent().unwrap()).unwrap();
                bindings.write_to_file(ffi_header);
            }
        },
    );
}

fn main() -> Result<(), Box<dyn Error>> {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR")?);
    let out_dir = PathBuf::from(env::var("OUT_DIR")?);

    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=cbindgen.toml");
    println!("cargo:rerun-if-env-changed=FFI_OUTPUT_DIR");
    println!("cargo:rerun-if-changed=src");

    let ffi_out_dir = env::var("FFI_OUTPUT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| out_dir.clone());
    let base_config = cbindgen::Config::from_file(manifest_dir.join("cbindgen.toml"))?;

    let tree_builder_sources = [
        manifest_dir.join("../../../RustAllocator.rs"),
        manifest_dir.join("src/tree_builder.rs"),
    ];
    generate_ffi_header(
        base_config.clone(),
        &tree_builder_sources,
        &out_dir,
        &ffi_out_dir,
        Path::new("Layout/TreeBuilderRustFFI.h"),
    );

    let mut layout_config = base_config;
    // CssPixels is a single i32 fixed-point value shared from the CSS Rust
    // source. C++ already has the matching CSSPixels class, so expose its raw
    // representation in the generated ABI rather than generating a second
    // C++ pixel type in the RustFFI namespace.
    layout_config.export.exclude.push("CssPixels".to_string());
    layout_config.export.exclude.push("rust_calc_resolve".to_string());
    layout_config
        .export
        .rename
        .insert("CssPixels".to_string(), "int32_t".to_string());
    layout_config.export.include = vec![
        "AvailableSpace".to_string(),
        "LogicalSize".to_string(),
        "LogicalOffset".to_string(),
        "LogicalRect".to_string(),
        "FfiContainingBlockConstraints".to_string(),
        "FfiLayoutInput".to_string(),
        "FfiLayoutBoxFacts".to_string(),
        "FfiLayoutNavCallbacks".to_string(),
        "FfiLayoutFcCallbacks".to_string(),
        "FfiBorderData".to_string(),
        "FfiBorderDataWithElementKind".to_string(),
        "FfiBordersData".to_string(),
        "FfiTableBoxFacts".to_string(),
        "FfiChildLayoutResult".to_string(),
        "FfiMeasuredCellContent".to_string(),
        "FfiCaptionLayoutResult".to_string(),
        "FfiMeasurementState".to_string(),
        "FfiIntrinsicSizeCacheKey".to_string(),
        "FfiIntrinsicSizeCacheKind".to_string(),
        "FfiFlexAxis".to_string(),
        "FfiFlexSizeProperty".to_string(),
        "FfiFlexLayoutItemRect".to_string(),
        "FfiFlexLayoutItem".to_string(),
        "FfiFlexLayoutLine".to_string(),
        "FfiFlexLayoutData".to_string(),
        "FfiDisplay".to_string(),
        "FfiFormattingContextType".to_string(),
        "FfiSizeKind".to_string(),
        "FfiSizeValue".to_string(),
        "FfiStyleFacts".to_string(),
    ];
    generate_ffi_header(
        layout_config,
        &[
            manifest_dir.join("src/used_values.rs"),
            manifest_dir.join("src/layout_state.rs"),
            manifest_dir.join("src/geometry.rs"),
            manifest_dir.join("src/ffi_stats.rs"),
            manifest_dir.join("src/style_facts.rs"),
            manifest_dir.join("src/box_facts.rs"),
            manifest_dir.join("../../CSS/Rust/src/display.rs"),
            manifest_dir.join("src/fc/mod.rs"),
        ],
        &out_dir,
        &ffi_out_dir,
        Path::new("Layout/LayoutRustFFI.h"),
    );

    Ok(())
}

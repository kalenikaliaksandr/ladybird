/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! Reads the items of the embedding ABI from the crate's sources: the functions it exports, and the C types and
//! constants their signatures can name. build.rs writes the export lists of the LibJS library from them, and the build
//! script of the libjs_embedding crate writes the Rust declarations of the ABI from them.

use std::fs;
use std::path::{Path, PathBuf};

pub struct EmbeddingItems {
    /// Every exported js_* function, in a stable order.
    pub functions: Vec<syn::ItemFn>,
    /// Every struct, enum, union and type alias, which can be more than one per name under different cfgs.
    pub types: Vec<syn::Item>,
    /// The names that opaque_c_types! declares.
    pub opaque_types: Vec<syn::Ident>,
    /// Every public constant whose name starts with JS_.
    pub constants: Vec<syn::ItemConst>,
}

/// The files outside src/embedding whose types and constants the ABI names.
const OTHER_SOURCES: &[&str] = &[
    "src/layout/host_class.rs",
    "src/layout/host_object.rs",
    "src/gc/capi.rs",
];

/// The sources the items come from, which a build script reruns on.
pub fn sources(crate_directory: &Path) -> Vec<PathBuf> {
    let mut sources = Vec::new();
    collect_rust_files(&crate_directory.join("src/embedding"), &mut sources);
    sources.sort();
    sources.extend(OTHER_SOURCES.iter().map(|source| crate_directory.join(source)));
    sources
}

fn collect_rust_files(directory: &Path, files: &mut Vec<PathBuf>) {
    let entries = fs::read_dir(directory).unwrap_or_else(|error| panic!("read {}: {error}", directory.display()));
    for entry in entries {
        let path = entry.expect("read a directory entry").path();
        if path.is_dir() {
            collect_rust_files(&path, files);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            files.push(path);
        }
    }
}

pub fn read(crate_directory: &Path) -> EmbeddingItems {
    let mut items = EmbeddingItems {
        functions: Vec::new(),
        types: Vec::new(),
        opaque_types: Vec::new(),
        constants: Vec::new(),
    };
    for source in sources(crate_directory) {
        let text = fs::read_to_string(&source).unwrap_or_else(|error| panic!("read {}: {error}", source.display()));
        let file = syn::parse_file(&text).unwrap_or_else(|error| panic!("parse {}: {error}", source.display()));
        let is_embedding_source = source.starts_with(crate_directory.join("src/embedding"));
        for item in file.items {
            if is_test_only(item_attributes(&item)) {
                continue;
            }
            match item {
                syn::Item::Fn(function) if is_embedding_source && is_exported_function(&function) => {
                    items.functions.push(function);
                }
                syn::Item::Struct(_) | syn::Item::Enum(_) | syn::Item::Union(_) | syn::Item::Type(_) => {
                    items.types.push(item);
                }
                syn::Item::Const(constant)
                    if matches!(constant.vis, syn::Visibility::Public(_))
                        && constant.ident.to_string().starts_with("JS_") =>
                {
                    items.constants.push(constant);
                }
                syn::Item::Macro(invocation) if invocation.mac.path.is_ident("opaque_c_types") => {
                    let names = invocation
                        .mac
                        .parse_body_with(syn::punctuated::Punctuated::<syn::Ident, syn::Token![,]>::parse_terminated)
                        .unwrap_or_else(|error| panic!("parse opaque_c_types! in {}: {error}", source.display()));
                    items.opaque_types.extend(names);
                }
                _ => {}
            }
        }
    }
    items
}

impl EmbeddingItems {
    /// The names of the exported functions, each once.
    pub fn function_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .functions
            .iter()
            .map(|function| function.sig.ident.to_string())
            .collect();
        names.sort();
        names.dedup();
        names
    }
}

fn item_attributes(item: &syn::Item) -> &[syn::Attribute] {
    match item {
        syn::Item::Fn(item) => &item.attrs,
        syn::Item::Struct(item) => &item.attrs,
        syn::Item::Enum(item) => &item.attrs,
        syn::Item::Union(item) => &item.attrs,
        syn::Item::Type(item) => &item.attrs,
        syn::Item::Const(item) => &item.attrs,
        syn::Item::Mod(item) => &item.attrs,
        _ => &[],
    }
}

fn is_test_only(attributes: &[syn::Attribute]) -> bool {
    attributes.iter().any(|attribute| {
        attribute.path().is_ident("cfg")
            && attribute
                .parse_args::<syn::Ident>()
                .is_ok_and(|predicate| predicate == "test")
    })
}

fn is_exported_function(function: &syn::ItemFn) -> bool {
    let has_no_mangle = function.attrs.iter().any(|attribute| {
        attribute.path().is_ident("no_mangle")
            || (attribute.path().is_ident("unsafe")
                && attribute
                    .parse_args::<syn::Ident>()
                    .is_ok_and(|argument| argument == "no_mangle"))
    });
    let is_extern_c = function
        .sig
        .abi
        .as_ref()
        .and_then(|abi| abi.name.as_ref())
        .is_some_and(|name| name.value() == "C");
    has_no_mangle && is_extern_c && function.sig.ident.to_string().starts_with("js_")
}

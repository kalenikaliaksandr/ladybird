/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! Reads the functions that the embedding ABI exports from the crate's sources, which build.rs writes the export lists
//! of the LibJS library from.

use std::fs;
use std::path::{Path, PathBuf};

pub struct EmbeddingItems {
    /// Every exported js_* function, in a stable order.
    pub functions: Vec<syn::ItemFn>,
}

/// The sources the functions come from.
pub fn sources(crate_directory: &Path) -> Vec<PathBuf> {
    let mut sources = Vec::new();
    collect_rust_files(&crate_directory.join("src/embedding"), &mut sources);
    sources.sort();
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
    let mut items = EmbeddingItems { functions: Vec::new() };
    for source in sources(crate_directory) {
        let text = fs::read_to_string(&source).unwrap_or_else(|error| panic!("read {}: {error}", source.display()));
        let file = syn::parse_file(&text).unwrap_or_else(|error| panic!("parse {}: {error}", source.display()));
        for item in file.items {
            if let syn::Item::Fn(function) = item
                && !is_test_only(&function.attrs)
                && is_exported_function(&function)
            {
                items.functions.push(function);
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

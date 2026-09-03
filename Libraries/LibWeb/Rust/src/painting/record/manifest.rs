/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! Resources a recording produced or referenced, published to the C++ host once the
//! recording has returned.
//!
//! Fonts and nested display lists carry ids the recorder can read or mint without the host:
//! a `Gfx::Font` owns its process-unique id, and nested display lists take theirs from the
//! counter `Painting::DisplayList` mints from. The recorder writes those ids into the display
//! list bytes and lists the resources here; the host registers them with its
//! `DisplayListResourceStorage` before anything scans the bytes for references.

use crate::painting::display_list::builder::RecordedDisplayList;
use crate::painting::display_list::commands::DisplayListResourceId;
use crate::painting::host::FfiMaskDisplayListRegistration;
use crate::painting::visual_context::VisualContextTree;
use libgfx_rust::font::{FontRef, RetainedFont};
use std::collections::HashMap;

unsafe extern "C" {
    /// Mints an id from the counter `Painting::DisplayList` uses, so ids minted here never
    /// collide with lists the host creates.
    fn ladybird_web_display_list_allocate_id() -> u64;
}

/// A nested recording the parent's bytes reference by `id`.
pub(crate) struct NestedDisplayListEntry {
    pub(crate) id: DisplayListResourceId,
    pub(crate) recorded: RecordedDisplayList,
    pub(crate) tree: VisualContextTree,
    pub(crate) mask_registrations: Vec<FfiMaskDisplayListRegistration>,
}

#[derive(Default)]
pub struct ResourceManifest {
    /// Keyed by the font's own id, which is what the display list bytes name it by.
    fonts: HashMap<u64, RetainedFont>,
    nested_display_lists: Vec<NestedDisplayListEntry>,
}

impl ResourceManifest {
    /// The id the display list names `font` by; lists the font on first use.
    pub(crate) fn register_font(&mut self, font: &RetainedFont) -> u64 {
        // SAFETY: `font` holds a strong reference, so the object is live for this call.
        let id = unsafe { FontRef::from_raw(font.as_raw()) }.id();
        self.fonts.entry(id).or_insert_with(|| {
            // SAFETY: As above; the manifest's own reference keeps the font live until the host
            // has registered it.
            unsafe { RetainedFont::retain(font.as_raw()) }
        });
        id
    }

    /// Lists a finished nested recording under a freshly minted id and returns that id.
    pub(crate) fn publish_nested_display_list(
        &mut self,
        recorded: RecordedDisplayList,
        tree: VisualContextTree,
        mask_registrations: Vec<FfiMaskDisplayListRegistration>,
    ) -> DisplayListResourceId {
        // SAFETY: A relaxed atomic increment on the host side; no other state is touched.
        let id = DisplayListResourceId(unsafe { ladybird_web_display_list_allocate_id() });
        self.nested_display_lists.push(NestedDisplayListEntry {
            id,
            recorded,
            tree,
            mask_registrations,
        });
        id
    }

    pub(crate) fn fonts(&self) -> impl Iterator<Item = &RetainedFont> {
        self.fonts.values()
    }

    pub(crate) fn into_nested_display_lists(self) -> Vec<NestedDisplayListEntry> {
        self.nested_display_lists
    }
}

/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FcRunCacheMode {
    Disabled,
    Enabled,
    /// Hits do not replay: the real layout runs and the entry is verified
    /// against it, panicking on any divergence.
    Shadow,
}

fn fc_run_cache_mode_from_environment() -> FcRunCacheMode {
    static MODE: std::sync::OnceLock<FcRunCacheMode> = std::sync::OnceLock::new();
    *MODE.get_or_init(|| match std::env::var("LADYBIRD_FC_RUN_CACHE").as_deref() {
        Ok("1") => FcRunCacheMode::Enabled,
        Ok("shadow") => FcRunCacheMode::Shadow,
        _ => FcRunCacheMode::Disabled,
    })
}

fn clone_rare_data_without_resources(rare: &UsedValuesRareData) -> UsedValuesRareData {
    assert!(
        rare.computed_svg_path.is_none(),
        "SVG payload writes make a run uncacheable before its rare data is cloned"
    );
    UsedValuesRareData {
        table_cell_coordinates: rare.table_cell_coordinates,
        computed_svg_path: None,
        computed_svg_transforms: rare.computed_svg_transforms,
        svg_viewport_size: rare.svg_viewport_size,
        grid_layout_data: rare.grid_layout_data.clone(),
        flex_layout_data: rare.flex_layout_data.clone(),
        used_grid_tracks: rare.used_grid_tracks.clone(),
        override_borders_data: rare.override_borders_data,
        abspos_layout_inputs: rare.abspos_layout_inputs,
    }
}

/// The run root's record at body end, before the participation finalize
/// hooks: a hit replays it and re-runs those hooks against the live parent.
/// Lazy-cell presence is part of the state — commit payloads distinguish an
/// initialized-but-empty cell from an absent one.
struct CachedRootRecordState {
    cells: UsedValuesCellState,
    line_data: Option<LineData>,
    rare_data: Option<UsedValuesRareData>,
}

impl CachedRootRecordState {
    fn capture(used: &UsedValues) -> Self {
        Self {
            cells: UsedValuesCellState::capture(used),
            line_data: used.line_data.get().map(|cell| cell.borrow().clone()),
            rare_data: used.rare_data.get().map(|cell| clone_rare_data_without_resources(&cell.borrow())),
        }
    }

    fn replay_into(&self, used: &UsedValues) {
        self.cells.replay_into(used);
        if let Some(line_data) = &self.line_data {
            *used.line_data_cell().borrow_mut() = line_data.clone();
        }
        if let Some(rare_data) = &self.rare_data {
            *used.rare_data_mut() = clone_rare_data_without_resources(rare_data);
        }
    }
}

/// The complete identity of a memoizable run. Input totality is by
/// construction: every spawned participation kind either declares its
/// pre-run root state through the input or is probe-proven to arrive
/// with the pure style baseline, so no raw record capture is needed.
#[derive(Clone, Copy, PartialEq)]
struct FcRunCacheKey {
    fc_type: FfiFormattingContextType,
    input: LayoutInput,
}

/// What must still be true for a stored entry to be replayed: the slot
/// holds the same box (generation), nothing in its subtree was invalidated
/// (the fragment cache epoch, whose bump walk has no propagation
/// boundary), and the viewport is unchanged (viewport-relative styles do
/// not necessarily funnel through per-node invalidation).
#[derive(Clone, Copy, PartialEq, Eq)]
struct FcRunCacheValidity {
    slot_generation: u8,
    fragment_cache_epoch: u16,
    viewport: (i32, i32),
}

struct FcRunCacheEntry {
    key: FcRunCacheKey,
    validity: FcRunCacheValidity,
    atomic_intrinsic_replay: Option<(CssPixels, DerivedBaselines)>,
    result: ChildLayoutResult,
    body_end_root_state: CachedRootRecordState,
    pending: PendingRunResult,
    /// Keeps every font referenced by the cached line data alive: glyph
    /// runs borrow raw font pointers, and a paint-only style change can
    /// drop the owning computed values without touching any layout epoch.
    retained_fonts: Vec<libgfx_rust::font::RetainedFont>,
}

/// Per-document store of completed run results, one entry per slot,
/// surviving across layout passes on the node arena.
#[derive(Default)]
pub(crate) struct FcRunCacheArenaStore {
    viewport: Cell<(i32, i32)>,
    entries: RefCell<Vec<Option<std::rc::Rc<FcRunCacheEntry>>>>,
}

impl FcRunCacheArenaStore {
    fn note_viewport_size(&self, inline_size_raw: i32, block_size_raw: i32) {
        self.viewport.set((inline_size_raw, block_size_raw));
    }

    pub(crate) fn remove_entry(&self, slot: u32) {
        if let Some(entry) = self.entries.borrow_mut().get_mut(slot as usize) {
            *entry = None;
        }
    }

    /// A matching entry stays stored — hits hand out shared handles — while
    /// a stale entry is evicted on sight, releasing its tree and fonts.
    fn matching(&self, slot: u32, validity: FcRunCacheValidity, key: &FcRunCacheKey) -> Option<std::rc::Rc<FcRunCacheEntry>> {
        let mut entries = self.entries.borrow_mut();
        let stored = entries.get_mut(slot as usize)?;
        let entry = stored.as_ref()?;
        if entry.validity != validity {
            *stored = None;
            return None;
        }
        if entry.key != *key {
            return None;
        }
        Some(entry.clone())
    }

    fn store(&self, slot: u32, entry: std::rc::Rc<FcRunCacheEntry>) {
        let mut entries = self.entries.borrow_mut();
        if entries.len() <= slot as usize {
            entries.resize_with(slot as usize + 1, || None);
        }
        entries[slot as usize] = Some(entry);
    }
}

fn retain_line_data_fonts(line_data: &LineData, fonts: &mut Vec<*const c_void>) {
    for line in &line_data.line_boxes {
        for fragment in &line.fragments {
            if !fragment.first_available_font.is_null() {
                fonts.push(fragment.first_available_font);
            }
            if let Some(glyphs) = &fragment.glyphs
                && !glyphs.font.is_null()
            {
                fonts.push(glyphs.font);
            }
        }
    }
}

fn collect_fonts_if_tree_is_cacheable(links: &[FragmentLink], fonts: &mut Vec<*const c_void>) -> bool {
    for link in links {
        let fragment = &link.fragment;
        // A deposited SVG subtree can carry path handles even though the
        // caching run itself never wrote SVG payloads; commit moves them
        // out, so a tree holding any cannot be re-emitted.
        if fragment.carries_svg_path() {
            return false;
        }
        if let Some(line_data) = &fragment.line_data {
            retain_line_data_fonts(line_data, fonts);
        }
        if !collect_fonts_if_tree_is_cacheable(&fragment.children, fonts) {
            return false;
        }
    }
    true
}

fn run_validity(run: &FormattingContextRun) -> FcRunCacheValidity {
    let data = run.state.node_facts(&run.callbacks, run.box_).data();
    FcRunCacheValidity {
        slot_generation: data.slot_generation,
        fragment_cache_epoch: data.fragment_cache_epoch,
        viewport: run.callbacks.arena().fc_run_cache_store().viewport.get(),
    }
}

/// A run's cache interaction, decided at probe time and concluded at the
/// run tail. Uncacheable classes bypass entirely: measurement and
/// intrinsic-sizing runs produce no fragments; float roots read the parent
/// block formatting context's pending margin state, which the key cannot
/// carry; anchor()-positioned roots read resolved insets from the
/// pass-scoped anchor inset store, which the key cannot see either — and
/// changing the anchor never invalidates the consumer's subtree; pass
/// entries and internal runs are not spawned child runs; devtools
/// collection emits per-run callbacks a replay would skip.
enum FcRunCacheAttempt {
    Bypass,
    Store {
        key: Box<FcRunCacheKey>,
        shadow_entry: Option<std::rc::Rc<FcRunCacheEntry>>,
    },
}

impl FcRunCacheAttempt {
    /// Err carries the entry the caller must replay instead of running.
    fn probe(
        run: &FormattingContextRun,
        fc_type: FfiFormattingContextType,
        input: &LayoutInput,
    ) -> Result<Self, std::rc::Rc<FcRunCacheEntry>> {
        let mode = fc_run_cache_mode_from_environment();
        if mode == FcRunCacheMode::Disabled
            || run.fragments.is_none()
            || run.should_collect_devtools_layout_data
            || matches!(
                input.participation,
                ParticipationInParentFormattingContext::Float | ParticipationInParentFormattingContext::Root
            )
            || fc_type.is_internal()
        {
            return Ok(Self::Bypass);
        }
        let data = run.state.node_facts(&run.callbacks, run.box_).data();
        if crate::layout::has_flag(data, NodeFlag::InsetsUseAnchorFunctions) {
            return Ok(Self::Bypass);
        }
        let key = Box::new(FcRunCacheKey { fc_type, input: *input });
        let store = run.callbacks.arena().fc_run_cache_store();
        match store.matching(run.box_.slot_index(), run_validity(run), &key) {
            Some(entry) if mode == FcRunCacheMode::Shadow => Ok(Self::Store {
                key,
                shadow_entry: Some(entry),
            }),
            Some(entry) => Err(entry),
            None => Ok(Self::Store { key, shadow_entry: None }),
        }
    }

    /// Whether the run should capture its root record at body end. SVG
    /// payload writes disqualify the run right here: the path handle is
    /// move-only, and this predicate runs before take_pending_result
    /// consumes the builder's flag.
    fn wants_body_end_root_state(&self, run: &FormattingContextRun) -> bool {
        matches!(self, Self::Store { .. })
            && run
                .fragments
                .as_ref()
                .is_some_and(|fragments| !fragments.has_svg_payload_writes())
    }

    fn conclude_run(
        self,
        run: &FormattingContextRun,
        atomic_intrinsic_replay: Option<(CssPixels, DerivedBaselines)>,
        result: &ChildLayoutResult,
        body_end_root_state: Option<CachedRootRecordState>,
        fragments: Option<&PendingRunResult>,
    ) {
        let Self::Store { key, shadow_entry } = self else {
            return;
        };
        let (Some(body_end_root_state), Some(pending)) = (body_end_root_state, fragments) else {
            return;
        };
        let mut fonts = Vec::new();
        if let Some(line_data) = &body_end_root_state.line_data {
            retain_line_data_fonts(line_data, &mut fonts);
        }
        if !collect_fonts_if_tree_is_cacheable(&pending.root_children, &mut fonts)
            || !collect_fonts_if_tree_is_cacheable(&pending.late_attachments, &mut fonts)
        {
            return;
        }
        fonts.sort_unstable();
        fonts.dedup();
        let entry = FcRunCacheEntry {
            key: *key,
            validity: run_validity(run),
            atomic_intrinsic_replay,
            result: *result,
            body_end_root_state,
            pending: pending.clone(),
            retained_fonts: fonts
                .into_iter()
                // SAFETY: every collected font pointer is live during the
                // pass that produced the line data now being cached.
                .map(|font| unsafe { libgfx_rust::font::RetainedFont::retain(font) })
                .collect(),
        };
        if let Some(cached) = shadow_entry {
            verify_cached_entry_against_fresh_run(run.box_.slot_index(), &cached, &entry);
        }
        run.callbacks
            .arena()
            .fc_run_cache_store()
            .store(run.box_.slot_index(), std::rc::Rc::new(entry));
    }
}

/// Replaces a run that hit the cache: replay the root record, re-run the
/// participation finalize against the live parent, seal, and reissue the
/// cached tree. The returned scope holds only the handed root — exactly
/// what a skipped run may expose, and the cop enforces it.
fn replay_run_from_cache(
    run: &FormattingContextRun,
    input: &LayoutInput,
    parent_block: Option<&BlockFormattingContext>,
    entry: &FcRunCacheEntry,
) -> RunOutputs {
    let root_used = run.records.used_values(run.box_);
    entry.body_end_root_state.replay_into(&root_used);
    let result = entry.result;
    finalize_run_root_participation(run, input, parent_block, &result, entry.atomic_intrinsic_replay);
    root_used.seal_own_metrics();
    run.outputs(result, Some(entry.pending.clone()))
}

fn verify_cached_entry_against_fresh_run(root_slot: u32, cached: &FcRunCacheEntry, fresh: &FcRunCacheEntry) {
    assert!(
        cached.atomic_intrinsic_replay == fresh.atomic_intrinsic_replay,
        "run cache shadow: intrinsic replay decision diverged for slot {root_slot}"
    );
    assert!(
        cached.result == fresh.result,
        "run cache shadow: child layout result diverged for slot {root_slot}"
    );
    assert!(
        cached.body_end_root_state.cells == fresh.body_end_root_state.cells,
        "run cache shadow: body-end root record diverged for slot {root_slot}\ncached: {:?}\nfresh: {:?}",
        cached.body_end_root_state.cells,
        fresh.body_end_root_state.cells,
    );
    let cached_fonts: Vec<_> = cached.retained_fonts.iter().map(|font| font.as_raw()).collect();
    let fresh_fonts: Vec<_> = fresh.retained_fonts.iter().map(|font| font.as_raw()).collect();
    assert!(
        cached_fonts == fresh_fonts,
        "run cache shadow: referenced fonts diverged for slot {root_slot}"
    );
    assert_line_data_matches(
        root_slot,
        cached.body_end_root_state.line_data.as_ref(),
        fresh.body_end_root_state.line_data.as_ref(),
    );
    assert_rare_data_matches(
        root_slot,
        cached.body_end_root_state.rare_data.as_ref(),
        fresh.body_end_root_state.rare_data.as_ref(),
    );
    assert_pending_results_match(root_slot, &cached.pending, &fresh.pending);
}

fn assert_line_data_matches(root_slot: u32, cached: Option<&LineData>, fresh: Option<&LineData>) {
    assert!(
        cached.is_some() == fresh.is_some(),
        "run cache shadow: root line data presence diverged for slot {root_slot}"
    );
    let (Some(cached), Some(fresh)) = (cached, fresh) else {
        return;
    };
    assert!(
        cached.line_boxes.len() == fresh.line_boxes.len()
            && cached.inline_box_pieces.len() == fresh.inline_box_pieces.len()
            && cached.line_boxes.iter().zip(&fresh.line_boxes).all(|(cached_line, fresh_line)| {
                cached_line.fragments.len() == fresh_line.fragments.len()
                    && cached_line.block_end == fresh_line.block_end
                    && cached_line.baseline == fresh_line.baseline
            }),
        "run cache shadow: root line data diverged for slot {root_slot}"
    );
}

fn assert_rare_data_matches(root_slot: u32, cached: Option<&UsedValuesRareData>, fresh: Option<&UsedValuesRareData>) {
    assert!(
        cached.is_some() == fresh.is_some(),
        "run cache shadow: root rare data presence diverged for slot {root_slot}"
    );
    let (Some(cached), Some(fresh)) = (cached, fresh) else {
        return;
    };
    assert!(
        cached.table_cell_coordinates == fresh.table_cell_coordinates
            && cached.override_borders_data == fresh.override_borders_data
            && cached.abspos_layout_inputs == fresh.abspos_layout_inputs
            && cached.grid_layout_data.is_some() == fresh.grid_layout_data.is_some()
            && cached.flex_layout_data.is_some() == fresh.flex_layout_data.is_some()
            && cached.used_grid_tracks.is_some() == fresh.used_grid_tracks.is_some(),
        "run cache shadow: root rare data diverged for slot {root_slot}"
    );
}

fn sorted_by_key<T: Clone, K: Ord>(items: &[T], key: impl Fn(&T) -> K) -> Vec<T> {
    let mut sorted = items.to_vec();
    sorted.sort_by_key(|item| key(item));
    sorted
}

fn assert_pending_results_match(root_slot: u32, cached: &PendingRunResult, fresh: &PendingRunResult) {
    // Escapes are collected partly from the frames map sweep, whose order is
    // not deterministic between runs; consumption sorts registrations into
    // tree order, so the oracle compares order-insensitively.
    let escape_order = |entry: &PendingAbsposChild| (entry.child_box.slot_index(), entry.target.slot_index());
    let cached_escapes = sorted_by_key(&cached.escaped_abspos, escape_order);
    let fresh_escapes = sorted_by_key(&fresh.escaped_abspos, escape_order);
    assert!(
        cached_escapes == fresh_escapes,
        "run cache shadow: escaped abspos registrations diverged for slot {root_slot}\ncached: {cached_escapes:#?}\nfresh: {fresh_escapes:#?}"
    );
    let candidate_order = |candidate: &AnchorCandidate| (candidate.node.slot_index(), candidate.effective_birth.slot_index());
    let cached_candidates = sorted_by_key(&cached.escaped_anchor_candidates, candidate_order);
    let fresh_candidates = sorted_by_key(&fresh.escaped_anchor_candidates, candidate_order);
    assert!(
        cached_candidates == fresh_candidates,
        "run cache shadow: escaped anchor candidates diverged for slot {root_slot}"
    );
    assert_link_lists_match(root_slot, &cached.root_children, &fresh.root_children);
    assert_link_lists_match(root_slot, &cached.late_attachments, &fresh.late_attachments);
}

fn assert_link_lists_match(root_slot: u32, cached: &[FragmentLink], fresh: &[FragmentLink]) {
    assert!(
        cached.len() == fresh.len(),
        "run cache shadow: fragment child count diverged for slot {root_slot}"
    );
    for (cached_link, fresh_link) in cached.iter().zip(fresh) {
        assert_links_match(root_slot, cached_link, fresh_link);
    }
}

fn assert_links_match(root_slot: u32, cached: &FragmentLink, fresh: &FragmentLink) {
    let matches = cached.offset == fresh.offset
        && cached.placement_content_offset == fresh.placement_content_offset
        && cached.inset_left == fresh.inset_left
        && cached.inset_right == fresh.inset_right
        && cached.inset_top == fresh.inset_top
        && cached.inset_bottom == fresh.inset_bottom
        && cached.containing_line_box_index == fresh.containing_line_box_index
        && cached.abspos_layout_inputs == fresh.abspos_layout_inputs
        && cached.fragment.shadow_comparable_state_matches(&fresh.fragment);
    assert!(
        matches,
        "run cache shadow: fragment for slot {} diverged under run root slot {root_slot}",
        fresh.fragment.node.slot_index()
    );
    assert_link_lists_match(root_slot, &cached.fragment.children, &fresh.fragment.children);
}

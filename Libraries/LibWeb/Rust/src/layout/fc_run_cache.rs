/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FcRunCacheMode {
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

/// Every plain-value cell of a UsedValues record. Captured twice per cached
/// run: at probe time it joins the cache key, because container runs hand
/// their roots sizes, definiteness, and cell padding through the record
/// rather than through LayoutInput; at body end it becomes the state a hit
/// replays into the freshly handed root record.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct UsedValuesCellState {
    content_inline_size: CssPixels,
    content_block_size: CssPixels,
    margin_left: CssPixels,
    margin_right: CssPixels,
    margin_top: CssPixels,
    margin_bottom: CssPixels,
    border_left: CssPixels,
    border_right: CssPixels,
    border_top: CssPixels,
    border_bottom: CssPixels,
    padding_left: CssPixels,
    padding_right: CssPixels,
    padding_top: CssPixels,
    padding_bottom: CssPixels,
    inset_left: CssPixels,
    inset_right: CssPixels,
    inset_top: CssPixels,
    inset_bottom: CssPixels,
    has_definite_inline_size: bool,
    has_definite_block_size: bool,
    uses_collapsing_borders_model: bool,
    inline_size_constraint: SizeConstraint,
    block_size_constraint: SizeConstraint,
    has_content_offset: bool,
    content_offset: FfiCssPixelPoint,
    has_first_baseline: bool,
    first_baseline: CssPixels,
    has_last_baseline: bool,
    last_baseline: CssPixels,
}

impl UsedValuesCellState {
    fn capture(used: &UsedValues) -> Self {
        Self {
            content_inline_size: used.content_inline_size.get(),
            content_block_size: used.content_block_size.get(),
            margin_left: used.margin_left.get(),
            margin_right: used.margin_right.get(),
            margin_top: used.margin_top.get(),
            margin_bottom: used.margin_bottom.get(),
            border_left: used.border_left.get(),
            border_right: used.border_right.get(),
            border_top: used.border_top.get(),
            border_bottom: used.border_bottom.get(),
            padding_left: used.padding_left.get(),
            padding_right: used.padding_right.get(),
            padding_top: used.padding_top.get(),
            padding_bottom: used.padding_bottom.get(),
            inset_left: used.inset_left.get(),
            inset_right: used.inset_right.get(),
            inset_top: used.inset_top.get(),
            inset_bottom: used.inset_bottom.get(),
            has_definite_inline_size: used.has_definite_inline_size.get(),
            has_definite_block_size: used.has_definite_block_size.get(),
            uses_collapsing_borders_model: used.uses_collapsing_borders_model.get(),
            inline_size_constraint: used.inline_size_constraint.get(),
            block_size_constraint: used.block_size_constraint.get(),
            has_content_offset: used.has_content_offset.get(),
            content_offset: used.content_offset.get(),
            has_first_baseline: used.has_first_baseline.get(),
            first_baseline: used.first_baseline.get(),
            has_last_baseline: used.has_last_baseline.get(),
            last_baseline: used.last_baseline.get(),
        }
    }

    fn replay_into(&self, used: &UsedValues) {
        used.content_inline_size.set(self.content_inline_size);
        used.content_block_size.set(self.content_block_size);
        used.margin_left.set(self.margin_left);
        used.margin_right.set(self.margin_right);
        used.margin_top.set(self.margin_top);
        used.margin_bottom.set(self.margin_bottom);
        used.border_left.set(self.border_left);
        used.border_right.set(self.border_right);
        used.border_top.set(self.border_top);
        used.border_bottom.set(self.border_bottom);
        used.padding_left.set(self.padding_left);
        used.padding_right.set(self.padding_right);
        used.padding_top.set(self.padding_top);
        used.padding_bottom.set(self.padding_bottom);
        used.inset_left.set(self.inset_left);
        used.inset_right.set(self.inset_right);
        used.inset_top.set(self.inset_top);
        used.inset_bottom.set(self.inset_bottom);
        used.has_definite_inline_size.set(self.has_definite_inline_size);
        used.has_definite_block_size.set(self.has_definite_block_size);
        used.uses_collapsing_borders_model.set(self.uses_collapsing_borders_model);
        used.inline_size_constraint.set(self.inline_size_constraint);
        used.block_size_constraint.set(self.block_size_constraint);
        used.has_content_offset.set(self.has_content_offset);
        used.content_offset.set(self.content_offset);
        used.has_first_baseline.set(self.has_first_baseline);
        used.first_baseline.set(self.first_baseline);
        used.has_last_baseline.set(self.has_last_baseline);
        used.last_baseline.set(self.last_baseline);
    }
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
pub(crate) struct CachedRootRecordState {
    cells: UsedValuesCellState,
    line_data: Option<LineData>,
    rare_data: Option<UsedValuesRareData>,
}

impl CachedRootRecordState {
    pub(crate) fn capture(used: &UsedValues) -> Self {
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

pub(crate) struct FcRunCacheEntry {
    fc_type: FfiFormattingContextType,
    input: LayoutInput,
    root_input_cells: UsedValuesCellState,
    atomic_intrinsic_replay: Option<(CssPixels, DerivedBaselines)>,
    result: ChildLayoutResult,
    body_end_root_state: CachedRootRecordState,
    pending: PendingRunResult,
}

pub(crate) struct FcRunCache {
    mode: FcRunCacheMode,
    entries: RefCell<std::collections::HashMap<u32, FcRunCacheEntry>>,
}

impl Default for FcRunCache {
    fn default() -> Self {
        Self {
            mode: fc_run_cache_mode_from_environment(),
            entries: RefCell::new(std::collections::HashMap::new()),
        }
    }
}

impl FcRunCache {
    fn take_matching(
        &self,
        slot: u32,
        fc_type: FfiFormattingContextType,
        input: &LayoutInput,
        root_input_cells: &UsedValuesCellState,
    ) -> Option<FcRunCacheEntry> {
        let mut entries = self.entries.borrow_mut();
        let entry = entries.get(&slot)?;
        if entry.fc_type != fc_type || entry.input != *input || entry.root_input_cells != *root_input_cells {
            return None;
        }
        entries.remove(&slot)
    }

    fn store(&self, slot: u32, entry: FcRunCacheEntry) {
        self.entries.borrow_mut().insert(slot, entry);
    }
}

/// A run's cache interaction, decided at probe time and concluded at the
/// run tail. Uncacheable classes bypass entirely: measurement and
/// intrinsic-sizing runs produce no fragments; float roots read the parent
/// block formatting context's pending margin state, which the key cannot
/// carry; pass entries and internal-replaced runs are not spawned child
/// runs; devtools collection emits per-run callbacks a replay would skip.
pub(crate) enum FcRunCacheAttempt {
    Bypass,
    Miss { root_input_cells: UsedValuesCellState },
    Hit { entry: FcRunCacheEntry },
    ShadowHit { entry: FcRunCacheEntry, root_input_cells: UsedValuesCellState },
}

impl FcRunCacheAttempt {
    pub(crate) fn probe(
        run: &FormattingContextRun,
        fc_type: FfiFormattingContextType,
        input: &LayoutInput,
        should_collect_devtools_layout_data: bool,
    ) -> Self {
        let cache = &run.state.fc_run_cache;
        if cache.mode == FcRunCacheMode::Disabled
            || run.fragments.is_none()
            || should_collect_devtools_layout_data
            || matches!(
                input.participation,
                ParticipationInParentFormattingContext::Float | ParticipationInParentFormattingContext::Root
            )
            || matches!(
                fc_type,
                FfiFormattingContextType::InternalReplaced | FfiFormattingContextType::InternalDummy
            )
        {
            return Self::Bypass;
        }
        let root_input_cells = UsedValuesCellState::capture(&run.records.used_values(run.box_));
        match cache.take_matching(run.box_.slot_index(), fc_type, input, &root_input_cells) {
            Some(entry) => match cache.mode {
                FcRunCacheMode::Enabled => Self::Hit { entry },
                FcRunCacheMode::Shadow => Self::ShadowHit { entry, root_input_cells },
                FcRunCacheMode::Disabled => unreachable!(),
            },
            None => Self::Miss { root_input_cells },
        }
    }

    /// Whether the run should capture its root record at body end. SVG
    /// payload writes disqualify the run right here: the path handle is
    /// move-only, and this predicate runs before take_pending_result
    /// consumes the builder's flag.
    pub(crate) fn wants_body_end_root_state(&self, run: &FormattingContextRun) -> bool {
        if !matches!(self, Self::Miss { .. } | Self::ShadowHit { .. }) {
            return false;
        }
        run.fragments
            .as_ref()
            .is_some_and(|fragments| !fragments.has_svg_payload_writes())
    }

    #[expect(clippy::too_many_arguments)]
    pub(crate) fn conclude_run(
        self,
        run: &FormattingContextRun,
        fc_type: FfiFormattingContextType,
        input: &LayoutInput,
        atomic_intrinsic_replay: Option<(CssPixels, DerivedBaselines)>,
        result: &ChildLayoutResult,
        body_end_root_state: Option<CachedRootRecordState>,
        fragments: Option<&PendingRunResult>,
    ) {
        let (root_input_cells, shadow_entry) = match self {
            Self::Bypass => return,
            Self::Hit { .. } => unreachable!("a replayed run never reaches the run tail"),
            Self::Miss { root_input_cells } => (root_input_cells, None),
            Self::ShadowHit { entry, root_input_cells } => (root_input_cells, Some(entry)),
        };
        let (Some(body_end_root_state), Some(pending)) = (body_end_root_state, fragments) else {
            return;
        };
        let entry = FcRunCacheEntry {
            fc_type,
            input: *input,
            root_input_cells,
            atomic_intrinsic_replay,
            result: *result,
            body_end_root_state,
            pending: pending.clone(),
        };
        if let Some(cached) = shadow_entry {
            verify_cached_entry_against_fresh_run(run.box_, &cached, &entry);
        }
        run.state.fc_run_cache.store(run.box_.slot_index(), entry);
    }
}

/// Replaces a run that hit the cache: replay the root record, re-run the
/// participation finalize against the live parent, seal, and reissue the
/// cached tree. The returned scope holds only the handed root — exactly
/// what a skipped run may expose, and the cop enforces it.
pub(crate) fn replay_run_from_cache(
    run: &FormattingContextRun,
    input: &LayoutInput,
    parent_block: Option<&BlockFormattingContext>,
    entry: FcRunCacheEntry,
) -> RunOutputs {
    let root_used = run.records.used_values(run.box_);
    entry.body_end_root_state.replay_into(&root_used);
    let result = entry.result;
    finalize_run_root_participation(run, input, parent_block, &result, entry.atomic_intrinsic_replay);
    root_used.seal_own_metrics();
    let pending = entry.pending.clone();
    run.state.fc_run_cache.store(run.box_.slot_index(), entry);
    RunOutputs {
        result,
        fragments: Some(pending),
        records: run.records.clone(),
    }
}

fn verify_cached_entry_against_fresh_run(root: Node, cached: &FcRunCacheEntry, fresh: &FcRunCacheEntry) {
    let root_slot = root.slot_index();
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
        "run cache shadow: body-end root record diverged for slot {root_slot}"
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
    match (cached, fresh) {
        (None, None) => {}
        (Some(cached), Some(fresh)) => {
            assert!(
                cached.line_boxes.len() == fresh.line_boxes.len()
                    && cached.inline_box_pieces.len() == fresh.inline_box_pieces.len()
                    && cached
                        .line_boxes
                        .iter()
                        .zip(&fresh.line_boxes)
                        .all(|(cached_line, fresh_line)| {
                            cached_line.fragments.len() == fresh_line.fragments.len()
                                && cached_line.block_end == fresh_line.block_end
                                && cached_line.baseline == fresh_line.baseline
                        }),
                "run cache shadow: root line data diverged for slot {root_slot}"
            );
        }
        _ => panic!("run cache shadow: root line data presence diverged for slot {root_slot}"),
    }
}

fn assert_rare_data_matches(root_slot: u32, cached: Option<&UsedValuesRareData>, fresh: Option<&UsedValuesRareData>) {
    match (cached, fresh) {
        (None, None) => {}
        (Some(cached), Some(fresh)) => {
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
        _ => panic!("run cache shadow: root rare data presence diverged for slot {root_slot}"),
    }
}

fn assert_pending_results_match(root_slot: u32, cached: &PendingRunResult, fresh: &PendingRunResult) {
    assert!(
        cached.escaped_abspos == fresh.escaped_abspos,
        "run cache shadow: escaped abspos registrations diverged for slot {root_slot}"
    );
    assert!(
        cached.escaped_anchor_candidates == fresh.escaped_anchor_candidates,
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
    let cached_fragment = &cached.fragment;
    let fresh_fragment = &fresh.fragment;
    let matches = cached_fragment.node == fresh_fragment.node
        && cached.offset == fresh.offset
        && cached.placement_content_offset == fresh.placement_content_offset
        && cached.inset_left == fresh.inset_left
        && cached.inset_right == fresh.inset_right
        && cached.inset_top == fresh.inset_top
        && cached.inset_bottom == fresh.inset_bottom
        && cached.containing_line_box_index == fresh.containing_line_box_index
        && cached.abspos_layout_inputs == fresh.abspos_layout_inputs
        && cached_fragment.content_inline_size == fresh_fragment.content_inline_size
        && cached_fragment.content_block_size == fresh_fragment.content_block_size
        && cached_fragment.margin_left == fresh_fragment.margin_left
        && cached_fragment.margin_right == fresh_fragment.margin_right
        && cached_fragment.margin_top == fresh_fragment.margin_top
        && cached_fragment.margin_bottom == fresh_fragment.margin_bottom
        && cached_fragment.border_left == fresh_fragment.border_left
        && cached_fragment.border_right == fresh_fragment.border_right
        && cached_fragment.border_top == fresh_fragment.border_top
        && cached_fragment.border_bottom == fresh_fragment.border_bottom
        && cached_fragment.padding_left == fresh_fragment.padding_left
        && cached_fragment.padding_right == fresh_fragment.padding_right
        && cached_fragment.padding_top == fresh_fragment.padding_top
        && cached_fragment.padding_bottom == fresh_fragment.padding_bottom
        && cached_fragment.table_cell_coordinates == fresh_fragment.table_cell_coordinates
        && cached_fragment.override_borders_data == fresh_fragment.override_borders_data
        && cached_fragment.line_data.is_some() == fresh_fragment.line_data.is_some()
        && cached_fragment.grid_layout_data.is_some() == fresh_fragment.grid_layout_data.is_some()
        && cached_fragment.flex_layout_data.is_some() == fresh_fragment.flex_layout_data.is_some()
        && cached_fragment.used_grid_tracks.is_some() == fresh_fragment.used_grid_tracks.is_some()
        && cached_fragment.computed_svg_transforms == fresh_fragment.computed_svg_transforms
        && cached_fragment.svg_viewport_size == fresh_fragment.svg_viewport_size;
    assert!(
        matches,
        "run cache shadow: fragment for slot {} diverged under run root slot {root_slot}",
        fresh_fragment.node.slot_index()
    );
    assert_link_lists_match(root_slot, &cached_fragment.children, &fresh_fragment.children);
}

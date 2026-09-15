/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! Recording consumes a packed program and writes one destination tape. Every source span
//! belongs to the preceding frame's complete directory; no row carries a historical address.

use super::avc_reuse::AvcReuseFilter;
use super::cache::{CaptureKind, CaptureSite};
use super::directory::{FramePaintCache, OutputPoint, OwnerInputs, PaintDirectory};
use super::program::{PaintAction, PaintOp, PaintProgram, ProgramUpdate};
use super::trace::{Action, Observer, Operation};
use super::{PaintPhase, PaintRecorder};
use crate::painting::display_list::builder::{CommandRange, RecordedDisplayList};
use crate::painting::display_list::commands::ContextRef;
use crate::painting::paint_order_plan::{PaintProducer, PaintScopeKind};
use std::rc::Rc;
use std::sync::Arc;

fn reads_root_canvas(action: PaintAction) -> bool {
    matches!(
        action,
        PaintAction::Produce(
            PaintProducer::DrawBoxPhase(PaintPhase::Background)
                | PaintProducer::Svg(_)
                | PaintProducer::SvgBoxForeground
        )
    )
}

pub(super) struct PackedRecording {
    update: ProgramUpdate,
    directory: PaintDirectory,
    shared_directory: Option<Rc<PaintDirectory>>,
    owner_inputs: Rc<Vec<OwnerInputs>>,
    topology_revision: u64,
    geometry_revision: u64,
    avc_reuse: AvcReuseFilter,
}

impl PackedRecording {
    pub fn new(
        update: ProgramUpdate,
        source: Option<&FramePaintCache>,
        topology_revision: u64,
        geometry_revision: u64,
        avc_reuse: AvcReuseFilter,
    ) -> Self {
        let owner_inputs = match source {
            Some(source) if Rc::ptr_eq(&source.program, &update.program) => source.owner_inputs.clone(),
            _ => Rc::new(
                update
                    .program
                    .owners
                    .iter()
                    .map(|owner| {
                        source
                            .and_then(|source| {
                                source
                                    .program
                                    .owner_index(owner.row)
                                    .map(|index| source.owner_inputs[index as usize])
                            })
                            .unwrap_or_default()
                    })
                    .collect(),
            ),
        };
        Self {
            update,
            directory: PaintDirectory::default(),
            shared_directory: None,
            owner_inputs,
            topology_revision,
            geometry_revision,
            avc_reuse,
        }
    }

    pub fn finish(self, record_gen: u32) -> FramePaintCache {
        FramePaintCache {
            program: self.update.program,
            directory: self.shared_directory.unwrap_or_else(|| Rc::new(self.directory)),
            owner_inputs: self.owner_inputs,
            record_gen,
            topology_revision: self.topology_revision,
            geometry_revision: self.geometry_revision,
        }
    }
}

impl<O: Observer> PaintRecorder<'_, O> {
    fn packed(&self) -> &PackedRecording {
        self.packed.as_ref().unwrap()
    }
    fn packed_mut(&mut self) -> &mut PackedRecording {
        self.packed.as_mut().unwrap()
    }

    pub(super) fn record_packed_program(&mut self) -> Option<Arc<RecordedDisplayList>> {
        let program = self.packed().update.program.clone();
        if let Some(dirty) = self.plan_sparse_update(&program) {
            self.record_sparse_program(&program, &dirty);
            return None;
        }
        self.record_program_producer(program.ops[0]);
        let canvas_end = self.output_point();
        if let Some(shared) = self.try_share_complete_frame(&program, canvas_end) {
            return Some(shared);
        }
        self.packed_mut().directory = PaintDirectory::new(program.ops.len());
        self.packed_mut().directory.append(canvas_end, true, false, 0);
        self.record_program_scope(&program, 0);
        let inspector = program.ops.len() as u32 - 1;
        self.record_program_operation(&program, inspector);
        assert_eq!(self.packed().directory.boundaries.len(), program.ops.len() + 1);
        None
    }

    fn plan_sparse_update(&self, program: &Rc<PaintProgram>) -> Option<Vec<u32>> {
        if !self.source_permits_reuse() {
            return None;
        }
        let source = self.command_cache_source.as_ref()?.paint_cache.as_ref()?;
        let pending = self.layout_arena.pending_paint_rows();
        if pending.requires_validation
            || !Rc::ptr_eq(program, &source.program)
            || source.geometry_revision != self.packed().geometry_revision
            || pending.rows.is_empty()
                && source.directory.refresh.is_empty()
                && self.packed().avc_reuse.rejected_operations().is_empty()
                && self.cache_compatibility.root_background
        {
            return None;
        }
        let mut dirty = Vec::new();
        dirty.extend(
            self.changed_root_background_operations(program)
                .filter(|&index| source.directory.recorded(index)),
        );
        for dirty_row in &pending.rows {
            let row = dirty_row.row;
            let Some(owner) = program.owner_index(row) else {
                if dirty_row.descendants {
                    continue;
                }
                return None;
            };
            for &index in program.uses(owner) {
                let op = program.ops[index as usize];
                match op.action {
                    PaintAction::BeginScope if !dirty_row.descendants => {
                        let scope = program.scopes[op.scope as usize];
                        let parent_active = scope.parent == super::program::NO_INDEX
                            || source.directory.recorded(program.scopes[scope.parent as usize].begin);
                        if scope.establishes_context
                            && parent_active
                            && self.stacking_context_is_painted(row) != source.directory.recorded(index)
                        {
                            return None;
                        }
                    }
                    PaintAction::Produce(producer) if source.directory.recorded(index) => {
                        let reads_descendants =
                            matches!(producer, PaintProducer::Svg(_) | PaintProducer::SvgBoxForeground)
                                || producer == PaintProducer::ScrollMetadata
                                    && self.inputs.uncaptured.is_recording_async_scrolling_metadata;
                        if !dirty_row.descendants || reads_descendants {
                            dirty.push(index);
                        }
                    }
                    _ => {}
                }
            }
        }
        dirty.extend_from_slice(&source.directory.refresh);
        // Sparse recording keeps the source program's operation indices. A changed
        // AVC dependency schedules its producer even when its layout row stayed clean.
        dirty.extend_from_slice(self.packed().avc_reuse.rejected_operations());
        dirty.push(0); // Canvas and inspector content have independent frame inputs.
        dirty.push(program.ops.len() as u32 - 1);
        dirty.sort_unstable();
        dirty.dedup();
        Some(dirty)
    }

    fn record_sparse_program(&mut self, program: &PaintProgram, dirty: &[u32]) {
        self.packed_mut().directory = PaintDirectory::new(program.ops.len());
        let mut cursor = 0;
        let mut open_scopes = Vec::new();
        let mut touched_scopes = Vec::new();
        for &index in dirty {
            if index != cursor {
                self.copy_program_interval(cursor, index);
            }
            self.trace_sparse_path(
                program,
                program.ops[index as usize].scope,
                &mut open_scopes,
                &mut touched_scopes,
            );
            self.record_program_operation(program, index);
            cursor = index + 1;
        }
        if cursor != program.ops.len() as u32 {
            self.copy_program_interval(cursor, program.ops.len() as u32);
        }
        self.trace_sparse_path(program, super::program::NO_INDEX, &mut open_scopes, &mut touched_scopes);
        assert_eq!(self.packed().directory.boundaries.len(), program.ops.len() + 1);
        if O::ENABLED {
            for scope in touched_scopes {
                let entry = program.scopes[scope as usize];
                let from = self.packed().directory.boundaries[entry.begin as usize];
                let to = self.packed().directory.boundaries[entry.end as usize];
                let site = self.program_scope_site(program, scope);
                self.log_command_byte_capture_for_verification(
                    site.paintable,
                    site.kind,
                    CommandRange {
                        offset: from.commands,
                        size: to.commands - from.commands,
                    },
                    false,
                );
                self.log_hit_test_item_capture_for_verification(
                    site.paintable,
                    site.kind,
                    from.hits as usize,
                    (to.hits - from.hits) as usize,
                    false,
                );
            }
        }
    }

    fn trace_sparse_path(&self, program: &PaintProgram, mut scope: u32, open: &mut Vec<u32>, touched: &mut Vec<u32>) {
        if !O::ENABLED {
            return;
        }
        let mut path = smallvec::SmallVec::<[u32; 16]>::new();
        while scope != super::program::NO_INDEX {
            path.push(scope);
            scope = program.scopes[scope as usize].parent;
        }
        path.reverse();
        let common = open.iter().zip(&path).take_while(|(a, b)| a == b).count();
        while open.len() > common {
            self.observer.observe(|log| log.end(false));
            open.pop();
        }
        for &scope in &path[common..] {
            let site = self.program_scope_site(program, scope);
            self.observer
                .observe(|log| log.begin(Operation::Capture(site), Action::Recompose));
            open.push(scope);
            touched.push(scope);
        }
    }

    fn output_point(&self) -> OutputPoint {
        OutputPoint {
            commands: u32::try_from(self.recorder.byte_size()).expect("display list exceeds u32"),
            hits: u32::try_from(self.list.items.len()).expect("hit-test list exceeds u32"),
        }
    }

    fn prepared_owner(&mut self, owner: u32) -> OwnerInputs {
        let row = self.packed().update.program.owners[owner as usize].row;
        let inputs = OwnerInputs {
            position: self.current_absolute_position(row),
            own_context: self.own_context(row),
            descendants_context: self.for_descendants_context(row),
            scroll_node: self.data(row).own_scroll_node_index,
        };
        if self.packed().owner_inputs[owner as usize] != inputs {
            Rc::make_mut(&mut self.packed_mut().owner_inputs)[owner as usize] = inputs;
        }
        inputs
    }

    fn source_permits_reuse(&self) -> bool {
        self.command_cache_source.as_ref().is_some_and(|source| {
            source
                .paint_cache
                .as_ref()
                .is_some_and(|cache| cache.record_gen == self.completed_record_gen)
                && !self.all_paint_caches_dirty
                && self.packed().avc_reuse.permits_any_reuse()
                && self
                    .cache_compatibility
                    .allows_subtree(CaptureKind::PaintedAsStackingContext)
        })
    }

    fn reads_changed_root_background(&self, program: &PaintProgram, index: u32) -> bool {
        if self.cache_compatibility.root_background {
            return false;
        }
        let op = program.ops[index as usize];
        program.owners[op.owner as usize].row == self.inputs.uncaptured.root_background_source.root_layout_node
            && reads_root_canvas(op.action)
    }

    fn changed_root_background_operations<'p>(&self, program: &'p PaintProgram) -> impl Iterator<Item = u32> + 'p {
        let owner = (!self.cache_compatibility.root_background)
            .then(|| program.owner_index(self.inputs.uncaptured.root_background_source.root_layout_node))
            .flatten();
        owner
            .into_iter()
            .flat_map(|owner| program.uses(owner))
            .copied()
            .filter(|&index| reads_root_canvas(program.ops[index as usize].action))
    }

    fn scope_source_interval(&mut self, program: &PaintProgram, scope: u32) -> Option<(u32, u32)> {
        if !self.source_permits_reuse() {
            return None;
        }
        let entry = program.scopes[scope as usize];
        let interval = self.packed().update.source_interval(entry.begin, entry.end)?;
        if !self.packed().avc_reuse.permits_interval(interval.0, interval.1) {
            return None;
        }
        if let Some(source) = self
            .command_cache_source
            .as_ref()
            .and_then(|source| source.paint_cache.as_ref())
            && self
                .changed_root_background_operations(&source.program)
                .any(|index| interval.0 <= index && index < interval.1)
        {
            return None;
        }
        let row = program.owners[entry.owner as usize].row;
        let cache = self.layout_arena.paintable_paint_cache(row);
        if cache.is_self_dirty_since(self.completed_record_gen)
            || cache.has_dirty_descendants_since(self.completed_record_gen)
        {
            return None;
        }
        drop(cache);
        let inputs = self.prepared_owner(entry.owner);
        let source = self.command_cache_source.as_ref()?.paint_cache.as_ref()?;
        let source_op = source.program.ops[interval.0 as usize];
        if source_op.action != PaintAction::BeginScope
            || !source.directory.recorded(interval.0)
            || source.directory.needs_refresh(interval.0, interval.1)
            || source.owner_inputs[source_op.owner as usize] != inputs
        {
            return None;
        }
        Some(interval)
    }

    fn try_share_complete_frame(
        &mut self,
        program: &PaintProgram,
        canvas_end: OutputPoint,
    ) -> Option<Arc<RecordedDisplayList>> {
        if self.has_inspector_overlays() {
            return None;
        }
        let (begin, end) = self.scope_source_interval(program, 0)?;
        let source = self.command_cache_source.clone()?;
        let cache = source.paint_cache.as_ref()?;
        if !Rc::ptr_eq(&cache.program, &self.packed().update.program)
            || cache.directory.boundaries[begin as usize] != canvas_end
            || cache.directory.boundaries[end as usize].commands as usize != source.display_list.bytes.len()
            || canvas_end.hits != 0
            || self.recorder.bytes() != &source.display_list.bytes[..canvas_end.commands as usize]
        {
            return None;
        }
        let items = self.item_cache_source.as_ref()?.items.clone();
        if cache.geometry_revision == self.packed().geometry_revision {
            self.list.items = items;
        } else {
            for item in items.iter() {
                self.append_spliced_hit_test_item(item);
            }
        }
        self.blocking_wheel_event_region_count += cache.directory.blocking_count(begin, end);
        self.packed_mut().shared_directory = Some(cache.directory.clone());
        let site = self.program_scope_site(program, 0);
        self.log_command_byte_capture_for_verification(
            site.paintable,
            site.kind,
            CommandRange {
                offset: canvas_end.commands,
                size: source.display_list.bytes.len() as u32 - canvas_end.commands,
            },
            true,
        );
        self.log_hit_test_item_capture_for_verification(site.paintable, site.kind, 0, self.list.items.len(), true);
        self.observer
            .observe(|log| log.leaf(Operation::Capture(site), Action::Reuse, false));
        Some(source.display_list.clone())
    }

    fn program_scope_site(&self, program: &PaintProgram, scope: u32) -> CaptureSite {
        let entry = program.scopes[scope as usize];
        CaptureSite {
            paintable: program.owners[entry.owner as usize].row,
            kind: match entry.kind {
                PaintScopeKind::PaintedAsStackingContext => CaptureKind::PaintedAsStackingContext,
                PaintScopeKind::Descendants(phase) => CaptureKind::DescendantSubtreePhase(phase),
            },
        }
    }

    fn record_program_scope(&mut self, program: &PaintProgram, scope: u32) {
        let entry = program.scopes[scope as usize];
        let site = self.program_scope_site(program, scope);
        let start = self.output_point();
        if let Some((begin, end)) = self.scope_source_interval(program, scope) {
            self.copy_program_interval(begin, end);
            self.log_scope_result(site, start, true);
            self.observer
                .observe(|log| log.leaf(Operation::Capture(site), Action::Reuse, false));
            return;
        }
        self.prepared_owner(entry.owner);
        self.trace_scope(Operation::Capture(site), Action::Walk, |this| {
            let active = !entry.establishes_context || this.stacking_context_is_painted(site.paintable);
            this.packed_mut().directory.append(start, active, false, 0);
            if !active {
                this.packed_mut()
                    .directory
                    .append_skipped(entry.end - entry.begin - 1, start);
                return;
            }
            let mut index = entry.begin + 1;
            while index < entry.end - 1 {
                let op = program.ops[index as usize];
                match op.action {
                    PaintAction::BeginScope => {
                        this.record_program_scope(program, op.scope);
                        index = program.scopes[op.scope as usize].end;
                    }
                    PaintAction::Produce(_) => {
                        this.record_program_operation(program, index);
                        index += 1;
                    }
                    PaintAction::EndScope => unreachable!("child scopes consume their closing operation"),
                }
            }
            let end = this.output_point();
            this.packed_mut().directory.append(end, true, false, 0);
        });
        self.log_scope_result(site, start, false);
    }

    fn log_scope_result(&mut self, site: CaptureSite, start: OutputPoint, reused: bool) {
        let end = self.output_point();
        self.log_command_byte_capture_for_verification(
            site.paintable,
            site.kind,
            CommandRange {
                offset: start.commands,
                size: end.commands - start.commands,
            },
            reused,
        );
        self.log_hit_test_item_capture_for_verification(
            site.paintable,
            site.kind,
            start.hits as usize,
            (end.hits - start.hits) as usize,
            reused,
        );
    }

    fn producer_source(&mut self, program: &PaintProgram, index: u32) -> Option<u32> {
        if !self.source_permits_reuse() {
            return None;
        }
        let op = program.ops[index as usize];
        if self.reads_changed_root_background(program, index) {
            return None;
        }
        if matches!(
            op.action,
            PaintAction::Produce(PaintProducer::Canvas | PaintProducer::InspectorOverlays)
        ) {
            return None;
        }
        let source_index = self.packed().update.source_op(index)?;
        if !self.packed().avc_reuse.permits_interval(source_index, source_index + 1) {
            return None;
        }
        let row = program.owners[op.owner as usize].row;
        if self
            .layout_arena
            .paintable_paint_cache(row)
            .is_self_dirty_since(self.completed_record_gen)
        {
            return None;
        }
        // Scrolling metadata reads descendant snap areas and overflow, even when the
        // scroller's own box geometry and style did not change.
        let reads_descendants = matches!(
            op.action,
            PaintAction::Produce(PaintProducer::Svg(_) | PaintProducer::SvgBoxForeground)
        ) || op.action == PaintAction::Produce(PaintProducer::ScrollMetadata)
            && self.inputs.uncaptured.is_recording_async_scrolling_metadata;
        if reads_descendants
            && self
                .layout_arena
                .paintable_paint_cache(row)
                .has_dirty_descendants_since(self.completed_record_gen)
        {
            return None;
        }
        let inputs = self.prepared_owner(op.owner);
        let source = self.command_cache_source.as_ref()?.paint_cache.as_ref()?;
        let old_op = source.program.ops[source_index as usize];
        if old_op.action != op.action
            || !source.directory.recorded(source_index)
            || source.directory.needs_refresh(source_index, source_index + 1)
            || source.owner_inputs[old_op.owner as usize] != inputs
        {
            return None;
        }
        Some(source_index)
    }

    fn record_program_operation(&mut self, program: &PaintProgram, index: u32) {
        let op = program.ops[index as usize];
        if let Some(source_index) = self.producer_source(program, index) {
            let start = self.output_point();
            self.copy_program_interval(source_index, source_index + 1);
            self.log_reused_producer(program, op, start);
            return;
        }
        self.prepared_owner(op.owner);
        let before = self.uncacheable_paint_generation;
        let blocking = self.blocking_wheel_event_region_count;
        self.record_program_producer(op);
        let end = self.output_point();
        let refresh = self.uncacheable_paint_generation != before;
        let blocking_count = self.blocking_wheel_event_region_count - blocking;
        self.packed_mut().directory.append(end, true, refresh, blocking_count);
    }

    fn copy_program_interval(&mut self, begin: u32, end: u32) {
        debug_assert!(self.packed().avc_reuse.permits_interval(begin, end));
        let source = self
            .command_cache_source
            .clone()
            .expect("reuse requires a source frame");
        let cache = source.paint_cache.as_ref().unwrap();
        let from = cache.directory.boundaries[begin as usize];
        let to = cache.directory.boundaries[end as usize];
        self.recorder.append_cached_command_range_verbatim(
            &source.display_list,
            CommandRange {
                offset: from.commands,
                size: to.commands - from.commands,
            },
        );
        let items = self.item_cache_source.as_ref().unwrap().items.clone();
        let items = &items[from.hits as usize..to.hits as usize];
        if cache.geometry_revision == self.packed().geometry_revision {
            self.list.append_copies_of(items);
        } else {
            for item in items {
                self.append_spliced_hit_test_item(item);
            }
        }
        self.blocking_wheel_event_region_count += cache.directory.blocking_count(begin, end);
        self.packed_mut().directory.append_source(&cache.directory, begin, end);
    }

    fn log_reused_producer(&mut self, program: &PaintProgram, op: PaintOp, start: OutputPoint) {
        if !O::ENABLED {
            return;
        }
        let row = program.owners[op.owner as usize].row;
        let end = self.output_point();
        match op.action {
            PaintAction::Produce(PaintProducer::DrawBoxPhase(phase)) => {
                if self.base_paint_facts(row).paint_phase_mask & phase.bit() == 0 {
                    self.observer.observe(|log| {
                        log.leaf(
                            Operation::Capture(CaptureSite {
                                paintable: row,
                                kind: CaptureKind::BoxPhase(phase),
                            }),
                            Action::Skip,
                            true,
                        );
                    });
                } else {
                    self.log_command_byte_capture_for_verification(
                        row,
                        CaptureKind::BoxPhase(phase),
                        CommandRange {
                            offset: start.commands,
                            size: end.commands - start.commands,
                        },
                        true,
                    );
                }
            }
            PaintAction::Produce(PaintProducer::HitTestPhase(phase)) => self
                .log_hit_test_item_capture_for_verification(
                    row,
                    CaptureKind::BoxPhase(phase),
                    start.hits as usize,
                    (end.hits - start.hits) as usize,
                    true,
                ),
            PaintAction::Produce(PaintProducer::ScrollMetadata) => self.observer.observe(|log| {
                log.leaf(
                    Operation::Producer(Some(row), "scroll-metadata"),
                    Action::Reuse,
                    start.commands == end.commands,
                );
            }),
            _ => {}
        }
    }

    fn record_program_producer(&mut self, op: PaintOp) {
        let program = self.packed().update.program.clone();
        let row = program.owners[op.owner as usize].row;
        let PaintAction::Produce(producer) = op.action else {
            unreachable!()
        };
        debug_assert!(self.recorder.is_producer_boundary());
        let context = match producer {
            PaintProducer::DrawBoxPhase(phase) | PaintProducer::HitTestPhase(phase) => {
                self.context_for_phase(row, phase)
            }
            PaintProducer::Canvas | PaintProducer::InspectorOverlays => ContextRef::default(),
            _ => self.own_context(row),
        };
        self.recorder.set_accumulated_visual_context(context);
        self.execute_fresh_producer(row, producer);
        debug_assert!(self.recorder.is_producer_boundary());
        self.recorder.set_accumulated_visual_context(ContextRef::default());
    }
}

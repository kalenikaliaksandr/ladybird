/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! The compositor's requests to record a context's display list again around new rects, which it sends as its scrolling
//! nears the edges of what the display list records. They arrive on the render clock's thread, and each context keeps
//! the newest: a later request replaces one that no frame took yet. Only a context the process made takes requests.
//!
//! The Paint thread services the requests beside the event loop, in a job posted as one arrives: it records the last
//! committed frame of the context again around the rects asked for, which it keeps for that (see
//! [`crate::render_state::RetainedCommits`]), and presents the recording as a refresh. A request waits while a committed
//! frame of the context waits for the Paint thread to present it, which takes the newest request where it records, or
//! posts the job once it presented. Where no frame can be refreshed, the Paint thread asks the host for a frame of the
//! context, whose commit takes the context's newest request and records around its rects where they hold what the
//! commit shows. Meanwhile, the frames a clock lane of the context records around the scroll positions beside the event
//! loop record the newest request's rects as they are, and once one that records every rect the request asks for
//! presented, the request waits for no other frame. A test may hold the requests, which then wait until it releases
//! them.
//!
//! Each context also keeps what the display list it holds records, which the Paint thread notes as it presents one: the
//! frames recorded beside the event loop record no less of the scroll nodes a request does not name, and the main
//! thread checks the scroll positions it moves its scroll nodes to against it. A frame that keeps the display list the
//! compositor has, whose scroll positions show what that list leaves out, which a refresh presented after the main
//! thread checked them may make it, asks for a recording around them as a request of its own.

use crate::css::css_pixels::CssPixelPoint;
use crate::css::style::fast_hash::FastMap;
use crate::painting::ffi::FfiScrollNodeContentRect;
use crate::painting::record::interest::PresentedInterest;
use crate::painting::visual_context::interest::interest_rect_within_unbounded_edges;
use libgfx_rust::IntRect;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

unsafe extern "C" {
    /// Has the main thread ask the host for a frame of the compositor context, whose commit takes the context's newest
    /// request, and answers whether it could: before the process attaches a render clock channel, or once its main
    /// thread is gone, it cannot. Any thread may call it.
    safe fn web_render_clock_ask_host_for_interest_refresh(context: u64) -> bool;
}

/// A scroll node as the compositor names it.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct ScrollNodeName {
    scrollable_node_id: i64,
    kind: u8,
    pseudo_element_type: u8,
}

impl ScrollNodeName {
    fn of(rect: &FfiScrollNodeContentRect) -> Self {
        Self {
            scrollable_node_id: rect.scrollable_node_id,
            kind: rect.scroll_node_kind as u8,
            pseudo_element_type: rect.pseudo_element_type,
        }
    }
}

/// A request to record a context's display list around a rect of each scroll node it names, in the device pixels of
/// the node's content at scroll offset zero.
pub(crate) struct InterestRequest {
    // The compositor's id for the request, which the display list that answers it names. A test's request, and one a
    // frame that keeps the compositor's display list asks for, have none: the compositor never hears of them.
    compositor_request_id: Option<u64>,
    rects_by_scroll_node: FastMap<ScrollNodeName, IntRect>,
}

impl InterestRequest {
    fn new(compositor_request_id: Option<u64>, rects: &[FfiScrollNodeContentRect]) -> Self {
        Self {
            compositor_request_id,
            rects_by_scroll_node: rects
                .iter()
                .map(|rect| {
                    (
                        ScrollNodeName::of(rect),
                        interest_rect_within_unbounded_edges(rect.rect),
                    )
                })
                .collect(),
        }
    }

    /// A test's request, in an id space of its own, for the rects of the scroll nodes they name.
    #[cfg(test)]
    pub(crate) fn new_for_testing(rects: &[FfiScrollNodeContentRect]) -> Self {
        Self::new(None, rects)
    }

    /// The rect the request asks for of the scroll node `scroll_node` names, whatever its own rect.
    pub(crate) fn rect_requested_for(&self, scroll_node: &FfiScrollNodeContentRect) -> Option<IntRect> {
        self.rects_by_scroll_node.get(&ScrollNodeName::of(scroll_node)).copied()
    }

    /// Whether a display list that records `recorded` of the scroll nodes it records only part of records every rect
    /// the request asks for: of each scroll node the request names, the whole rect asked for. A scroll node the list
    /// records whole holds any rect.
    pub(crate) fn is_recorded_by(&self, recorded: &[FfiScrollNodeContentRect]) -> bool {
        recorded.iter().all(|recorded| {
            self.rect_requested_for(recorded)
                .is_none_or(|requested| requested.is_empty() || recorded.rect.contains_rect(requested))
        })
    }

    /// The compositor's id for the request, where a display list that records `recorded` answers it (see
    /// [`Self::is_recorded_by`]).
    pub(crate) fn compositor_request_answered_by(&self, recorded: &[FfiScrollNodeContentRect]) -> Option<u64> {
        self.compositor_request_id.filter(|_| self.is_recorded_by(recorded))
    }

    /// The compositor's id for the request, which a display list that answers it names, or none for a test's request.
    pub(crate) fn compositor_request_id(&self) -> Option<u64> {
        self.compositor_request_id
    }

    fn is_for_testing(&self) -> bool {
        self.compositor_request_id.is_none()
    }
}

#[derive(Default)]
struct ContextInterestRequests {
    context_id: u64,
    // The newest request no frame took or answered yet.
    newest_request: Option<Arc<InterestRequest>>,
    // The newest id the compositor sent: a request it sent before arrives too late.
    newest_compositor_request_id: u64,
    // Whether the host was asked for a frame of the context, and has yet to run the task that asks for it.
    host_asked: bool,
    // How many committed frames of the context the render owner handed the Paint thread that it has yet to present.
    commits_on_paint_queue: u32,
    // What the display list the context holds records, as the Paint thread presented it last.
    presented: Option<PresentedInterest>,
}

impl ContextInterestRequests {
    /// Whether the Paint thread services the context's newest request: one waits, and no frame the host was asked for or
    /// committed takes it first.
    fn waits_for_paint_thread(&self) -> bool {
        self.newest_request.is_some() && !self.host_asked && self.commits_on_paint_queue == 0
    }
}

struct InterestRefreshRegistry {
    contexts: Vec<ContextInterestRequests>,
    held_for_testing: bool,
    serviced_request_count: u64,
    discarded_refresh_recording_count: u64,
    // Whether a job that services requests waits to run on the Paint thread.
    service_queued: bool,
}

impl InterestRefreshRegistry {
    const fn new() -> Self {
        Self {
            contexts: Vec::new(),
            held_for_testing: false,
            serviced_request_count: 0,
            discarded_refresh_recording_count: 0,
            service_queued: false,
        }
    }

    fn context(&mut self, context_id: u64) -> Option<&mut ContextInterestRequests> {
        self.contexts
            .iter_mut()
            .find(|context| context.context_id == context_id)
    }

    fn register_context(&mut self, context_id: u64) {
        if self.context(context_id).is_none() {
            self.contexts.push(ContextInterestRequests {
                context_id,
                ..ContextInterestRequests::default()
            });
        }
    }

    fn forget_context(&mut self, context_id: u64) {
        self.contexts.retain(|context| context.context_id != context_id);
    }

    /// Forgets every request, and what the display lists the contexts held record: they came from a compositor that is
    /// gone, and the next one numbers its own requests from the start, and holds no display list yet.
    fn forget_requests(&mut self) {
        for context in &mut self.contexts {
            context.newest_request = None;
            context.newest_compositor_request_id = 0;
            context.presented = None;
        }
    }

    /// Takes `request` for the context, and answers whether to post a job that services requests to the Paint thread.
    fn receive(&mut self, context_id: u64, request: InterestRequest) -> bool {
        let Some(context) = self.context(context_id) else {
            return false;
        };
        if let Some(request_id) = request.compositor_request_id {
            if request_id <= context.newest_compositor_request_id {
                return false;
            }
            context.newest_compositor_request_id = request_id;
        }
        context.newest_request = Some(Arc::new(request));
        self.queue_service()
    }

    /// Answers whether to post a job that services requests to the Paint thread: one waits for it, none is queued yet,
    /// and no test holds the requests. Notes the job queued where it does.
    fn queue_service(&mut self) -> bool {
        if self.held_for_testing
            || self.service_queued
            || !self
                .contexts
                .iter()
                .any(ContextInterestRequests::waits_for_paint_thread)
        {
            return false;
        }
        self.service_queued = true;
        true
    }

    /// The job that services requests runs on the Paint thread: answers the contexts whose newest request it services,
    /// each with that request. A request that arrives from here on posts another job.
    fn requests_to_service(&mut self) -> Vec<(u64, Arc<InterestRequest>)> {
        self.service_queued = false;
        if self.held_for_testing {
            return Vec::new();
        }
        self.contexts
            .iter()
            .filter(|context| context.waits_for_paint_thread())
            .filter_map(|context| Some((context.context_id, context.newest_request.clone()?)))
            .collect()
    }

    /// Asks the host for a frame for the context's newest request, unless the host was asked already, and answers whether
    /// to call it.
    fn ask_host(&mut self, context_id: u64) -> bool {
        let Some(context) = self.context(context_id) else {
            return false;
        };
        if context.newest_request.is_none() || context.host_asked {
            return false;
        }
        context.host_asked = true;
        self.serviced_request_count += 1;
        true
    }

    fn note_commit_queued(&mut self, context_id: u64) {
        if let Some(context) = self.context(context_id) {
            context.commits_on_paint_queue += 1;
        }
    }

    /// Notes that the Paint thread is done with a committed frame of the context, and answers whether to post a job that
    /// services requests to it.
    fn note_commit_left_paint_queue(&mut self, context_id: u64) -> bool {
        if let Some(context) = self.context(context_id) {
            context.commits_on_paint_queue = context.commits_on_paint_queue.saturating_sub(1);
        }
        self.queue_service()
    }

    fn take_newest_request(&mut self, context_id: u64) -> Option<Arc<InterestRequest>> {
        self.context(context_id)?.newest_request.take()
    }

    #[cfg(test)]
    fn newest_request(&mut self, context_id: u64) -> Option<Arc<InterestRequest>> {
        self.context(context_id)?.newest_request.clone()
    }

    /// Takes a request of its own for the context, which a frame that keeps the display list the compositor has makes
    /// for `rects`, where no request waits, and answers whether to post a job that services requests to the Paint thread.
    /// A request that waits is serviced first, after which the compositor asks for what its recording leaves out.
    fn receive_request_of_frame_that_keeps_display_list(
        &mut self,
        context_id: u64,
        rects: &[FfiScrollNodeContentRect],
    ) -> bool {
        let Some(context) = self.context(context_id) else {
            return false;
        };
        if context.newest_request.is_some() || rects.is_empty() {
            return false;
        }
        context.newest_request = Some(Arc::new(InterestRequest::new(None, rects)));
        self.queue_service()
    }

    // A later request the frame that answered `request` did not see still waits.
    fn forget_answered_request(&mut self, context_id: u64, request: &Arc<InterestRequest>) {
        if let Some(context) = self.context(context_id)
            && context
                .newest_request
                .as_ref()
                .is_some_and(|newest| Arc::ptr_eq(newest, request))
        {
            context.newest_request = None;
        }
    }

    /// The host could not be asked for the frame `service` asked for.
    fn host_ask_failed(&mut self, context_id: u64) {
        if let Some(context) = self.context(context_id) {
            context.host_asked = false;
            self.serviced_request_count = self.serviced_request_count.saturating_sub(1);
        }
    }

    /// The task that asks the host for a frame of the context runs: answers whether a request still waits for the frame,
    /// which a frame committed since may have taken.
    fn host_task_runs(&mut self, context_id: u64) -> bool {
        let Some(context) = self.context(context_id) else {
            return false;
        };
        context.host_asked = false;
        context.newest_request.is_some()
    }

    /// Lets the requests a test held go, and answers whether to post a job that services them to the Paint thread.
    fn release_for_testing(&mut self) -> bool {
        self.held_for_testing = false;
        self.queue_service()
    }

    /// Forgets what a test did: the requests it made, its hold, and the counts of requests serviced and refresh recordings
    /// discarded.
    fn reset_for_testing(&mut self) {
        for context in &mut self.contexts {
            if context
                .newest_request
                .as_ref()
                .is_some_and(|request| request.is_for_testing())
            {
                context.newest_request = None;
            }
        }
        self.held_for_testing = false;
        self.serviced_request_count = 0;
        self.discarded_refresh_recording_count = 0;
    }
}

static REGISTRY: Mutex<InterestRefreshRegistry> = Mutex::new(InterestRefreshRegistry::new());

fn registry() -> MutexGuard<'static, InterestRefreshRegistry> {
    REGISTRY.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Posts a job that services the requests that wait to the Paint thread, where `queued` says to.
fn post_service(queued: bool) {
    if queued {
        crate::paint_stage::post_interest_refresh();
    }
}

/// Posts a job that services the requests that wait to the Paint thread, unless none waits for it, or one is queued.
pub(crate) fn service_waiting_interest_requests() {
    let queued = registry().queue_service();
    post_service(queued);
}

/// On the Paint thread, as the job that services requests runs: the contexts whose newest request it services, each with
/// that request.
pub(crate) fn interest_requests_to_service() -> Vec<(u64, Arc<InterestRequest>)> {
    registry().requests_to_service()
}

/// Asks the host for a frame of the compositor context `context_id`, whose commit takes the context's newest request,
/// unless it was asked already.
pub(crate) fn ask_host_for_interest_refresh(context_id: u64) {
    // NB: The ask goes outside the registry's lock, which a failed ask takes again.
    let asks = registry().ask_host(context_id);
    if asks && !web_render_clock_ask_host_for_interest_refresh(context_id) {
        registry().host_ask_failed(context_id);
    }
}

/// Notes that the Paint thread presented a display list to the compositor context `context_id`, which records what
/// `presented` says.
pub(crate) fn note_interest_presented(context_id: u64, presented: PresentedInterest) {
    if let Some(context) = registry().context(context_id) {
        context.presented = Some(presented);
    }
}

/// The newest request of the compositor context `context_id` that no committed frame took yet, and what the display list
/// the context holds records, as the Paint thread presented it last, read together.
pub(crate) fn interest_requested_and_presented(
    context_id: u64,
) -> (Option<Arc<InterestRequest>>, Option<PresentedInterest>) {
    let mut registry = registry();
    let Some(context) = registry.context(context_id) else {
        return (None, None);
    };
    (context.newest_request.clone(), context.presented.clone())
}

/// On the Paint thread, once a frame the main thread committed that keeps the display list the compositor context
/// `context_id` holds presented, with the scroll positions `own_offsets` of a render state whose visual context tree has
/// `structural_epoch`: asks for a recording around what they show that the display list leaves out, which a refresh
/// presented after the main thread checked them may leave out.
pub(crate) fn ask_for_what_kept_display_list_leaves_out(
    context_id: u64,
    structural_epoch: u64,
    own_offsets: &[CssPixelPoint],
) {
    let mut registry = registry();
    let Some(presented) = registry
        .context(context_id)
        .and_then(|context| context.presented.clone())
    else {
        return;
    };
    let rects = presented.rects_around_what_scroll_positions_show_that_it_leaves_out(structural_epoch, own_offsets);
    let queued = registry.receive_request_of_frame_that_keeps_display_list(context_id, &rects);
    drop(registry);
    post_service(queued);
}

/// Notes that a frame presented on the Paint thread, a refresh, or a display list the compositor has, records every rect
/// of `request`, a request of the compositor context `context_id`: no other frame needs to take it.
pub(crate) fn note_interest_request_serviced(context_id: u64, request: &Arc<InterestRequest>) {
    let mut registry = registry();
    registry.forget_answered_request(context_id, request);
    registry.serviced_request_count += 1;
}

/// Notes that the Paint thread recorded a refresh it did not present, which left the request to the host.
pub(crate) fn note_interest_refresh_recording_discarded() {
    registry().discarded_refresh_recording_count += 1;
}

/// A committed frame of a compositor context that the render owner handed the Paint thread, until the Paint thread is
/// done with it: the context's requests wait for it meanwhile, as it takes the newest where it records, and refreshes the
/// frame it presents after.
pub(crate) struct CommitOnPaintQueue {
    context_id: u64,
}

impl CommitOnPaintQueue {
    pub(crate) fn note(context_id: u64) -> Self {
        registry().note_commit_queued(context_id);
        Self { context_id }
    }
}

impl Drop for CommitOnPaintQueue {
    fn drop(&mut self) {
        let queued = registry().note_commit_left_paint_queue(self.context_id);
        post_service(queued);
    }
}

/// Has a committed frame of the compositor context `context_id` take the context's newest request, which it records
/// around.
pub(crate) fn take_newest_interest_request(context_id: u64) -> Option<Arc<InterestRequest>> {
    registry().take_newest_request(context_id)
}

/// Forgets `request`, where it is still the newest request of the compositor context `context_id`: a clock lane's frame
/// that records every rect it asks for presented, so no other frame needs to take it any more.
pub(crate) fn forget_interest_request_answered_by_clock_lane(context_id: u64, request: &Arc<InterestRequest>) {
    registry().forget_answered_request(context_id, request);
}

/// Has the compositor context `context_id`, which the process just made, take requests.
#[unsafe(no_mangle)]
pub extern "C" fn interest_refresh_register_context(context_id: u64) {
    registry().register_context(context_id);
}

/// Forgets the compositor context `context_id`, which the process destroyed, with its requests and the committed frame
/// the Paint thread keeps to refresh.
#[unsafe(no_mangle)]
pub extern "C" fn interest_refresh_forget_context(context_id: u64) {
    registry().forget_context(context_id);
    crate::render_state::forget_retained_commit_of_context(context_id);
}

/// Takes the compositor's request `request_id` to record the display list of the context `context_id` again around
/// `rects`, unless it sent a later one already. On the render clock's thread.
///
/// # Safety
///
/// `rects` must point to `rect_count` rects, or be null with a count of zero.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn interest_refresh_receive_request(
    context_id: u64,
    request_id: u64,
    rects: *const FfiScrollNodeContentRect,
    rect_count: usize,
) {
    // SAFETY: Guaranteed by the caller.
    let rects = unsafe { ak::slice_from_raw_parts(rects, rect_count) };
    let queued = registry().receive(context_id, InterestRequest::new(Some(request_id), rects));
    post_service(queued);
}

/// Forgets the requests of every context, which a compositor that is gone sent, and the committed frames the Paint
/// thread keeps to refresh for it. On the render clock's thread.
#[unsafe(no_mangle)]
pub extern "C" fn interest_refresh_forget_requests() {
    registry().forget_requests();
    crate::render_state::forget_retained_commits();
}

/// The main thread runs the task that asks the host for a frame of the context `context_id`: answers whether a request
/// still waits for that frame.
#[unsafe(no_mangle)]
pub extern "C" fn interest_refresh_host_task_runs(context_id: u64) -> bool {
    registry().host_task_runs(context_id)
}

/// Takes a test's request to record the display list of the context `context_id` again around `rects`, as a request
/// from the compositor, in an id space of its own: the compositor never hears of it.
///
/// # Safety
///
/// As for [`interest_refresh_receive_request`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn interest_refresh_inject_for_testing(
    context_id: u64,
    rects: *const FfiScrollNodeContentRect,
    rect_count: usize,
) {
    // SAFETY: Guaranteed by the caller.
    let rects = unsafe { ak::slice_from_raw_parts(rects, rect_count) };
    let queued = registry().receive(context_id, InterestRequest::new(None, rects));
    post_service(queued);
}

/// Holds the requests of every context, which wait until the test releases them.
#[unsafe(no_mangle)]
pub extern "C" fn interest_refresh_hold_for_testing() {
    registry().held_for_testing = true;
}

/// Lets the requests the test held go, servicing those that wait.
#[unsafe(no_mangle)]
pub extern "C" fn interest_refresh_release_for_testing() {
    let queued = registry().release_for_testing();
    post_service(queued);
}

/// How many requests were serviced since the test began, once the Paint thread ran the jobs handed to it before, where
/// a test holds no recording on it.
#[unsafe(no_mangle)]
pub extern "C" fn interest_refresh_serviced_count_for_testing() -> u64 {
    if !crate::painting::recording_slot::recording_is_held_for_testing() {
        crate::paint_stage::paint_thread().run(|| ());
    }
    registry().serviced_request_count
}

/// How many refreshes the Paint thread recorded but did not present since the test began, once it ran the jobs handed to
/// it before, where a test holds no recording on it.
#[unsafe(no_mangle)]
pub extern "C" fn interest_refresh_discarded_recording_count_for_testing() -> u64 {
    if !crate::painting::recording_slot::recording_is_held_for_testing() {
        crate::paint_stage::paint_thread().run(|| ());
    }
    registry().discarded_refresh_recording_count
}

/// Forgets the requests a test made, its hold, and the counts of requests serviced and refresh recordings discarded, as
/// the next test begins.
#[unsafe(no_mangle)]
pub extern "C" fn interest_refresh_reset_for_testing() {
    registry().reset_for_testing();
}

#[cfg(test)]
mod ffi_test_stubs {
    #[unsafe(no_mangle)]
    extern "C" fn web_render_clock_ask_host_for_interest_refresh(_: u64) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::painting::display_list::commands::CompositorScrollNodeKind;

    const CONTEXT: u64 = 7;

    fn rect_of_element(scrollable_node_id: i64, rect: IntRect) -> FfiScrollNodeContentRect {
        FfiScrollNodeContentRect {
            scrollable_node_id,
            scroll_node_kind: CompositorScrollNodeKind::Element,
            pseudo_element_type: 0,
            rect,
        }
    }

    fn registry_with_context() -> InterestRefreshRegistry {
        let mut registry = InterestRefreshRegistry::new();
        registry.register_context(CONTEXT);
        registry
    }

    fn compositor_request(request_id: u64, top: i32) -> InterestRequest {
        InterestRequest::new(Some(request_id), &[rect_of_element(1, IntRect::new(0, top, 800, 1200))])
    }

    fn newest_request_id(registry: &mut InterestRefreshRegistry) -> Option<u64> {
        registry
            .take_newest_request(CONTEXT)
            .and_then(|request| request.compositor_request_id)
    }

    fn requests_to_service(registry: &mut InterestRefreshRegistry) -> Vec<Option<u64>> {
        registry
            .requests_to_service()
            .into_iter()
            .map(|(context_id, request)| {
                assert_eq!(context_id, CONTEXT);
                request.compositor_request_id
            })
            .collect()
    }

    #[test]
    fn a_context_keeps_the_newest_request_the_compositor_sent() {
        let mut registry = registry_with_context();
        assert!(registry.receive(CONTEXT, compositor_request(2, 0)));
        // The job that services requests is queued already, and services the newest.
        assert!(!registry.receive(CONTEXT, compositor_request(3, 600)));
        // A request sent before the newest arrives too late.
        assert!(!registry.receive(CONTEXT, compositor_request(1, 1200)));
        assert!(!registry.receive(CONTEXT, compositor_request(3, 1200)));
        assert_eq!(requests_to_service(&mut registry), vec![Some(3)]);

        // The job asks the host for a frame, whose commit takes the newest request.
        assert!(registry.ask_host(CONTEXT));
        assert!(!registry.ask_host(CONTEXT));
        assert_eq!(registry.serviced_request_count, 1);
        assert!(!registry.receive(CONTEXT, compositor_request(4, 1800)));
        assert!(registry.requests_to_service().is_empty());
        assert!(registry.host_task_runs(CONTEXT));
        assert_eq!(newest_request_id(&mut registry), Some(4));
        assert!(!registry.host_task_runs(CONTEXT));
    }

    #[test]
    fn a_committed_frame_the_paint_thread_has_yet_to_present_takes_requests_first() {
        let mut registry = registry_with_context();
        registry.note_commit_queued(CONTEXT);
        assert!(!registry.receive(CONTEXT, compositor_request(1, 0)));
        assert!(registry.requests_to_service().is_empty());
        // The frame recorded without taking the request, which the job posted once it presented services.
        assert!(registry.note_commit_left_paint_queue(CONTEXT));
        assert_eq!(requests_to_service(&mut registry), vec![Some(1)]);
        // A frame that took the request leaves nothing to service.
        registry.note_commit_queued(CONTEXT);
        assert!(!registry.receive(CONTEXT, compositor_request(2, 0)));
        assert!(registry.take_newest_request(CONTEXT).is_some());
        assert!(!registry.note_commit_left_paint_queue(CONTEXT));
    }

    #[test]
    fn a_request_a_clock_lane_answered_waits_no_more() {
        let mut registry = registry_with_context();
        registry.receive(CONTEXT, compositor_request(1, 0));
        let answered = registry.newest_request(CONTEXT).expect("a request waits");
        registry.receive(CONTEXT, compositor_request(2, 600));
        // The lane's frame recorded around the request it read, which a later one replaced.
        registry.forget_answered_request(CONTEXT, &answered);
        let newest = registry.newest_request(CONTEXT).expect("the later request waits");
        assert_eq!(newest.compositor_request_id, Some(2));
        registry.forget_answered_request(CONTEXT, &newest);
        assert!(registry.newest_request(CONTEXT).is_none());
        assert!(!registry.host_task_runs(CONTEXT));
    }

    #[test]
    fn a_frame_that_keeps_the_display_list_asks_only_where_no_request_waits() {
        let mut registry = registry_with_context();
        let rects = [rect_of_element(1, IntRect::new(0, 4200, 800, 2400))];
        assert!(registry.receive_request_of_frame_that_keeps_display_list(CONTEXT, &rects));
        assert_eq!(requests_to_service(&mut registry), vec![None]);
        assert!(!registry.receive_request_of_frame_that_keeps_display_list(CONTEXT, &[]));
        // A request that waits is serviced first.
        assert!(registry.receive(CONTEXT, compositor_request(1, 0)));
        assert!(!registry.receive_request_of_frame_that_keeps_display_list(CONTEXT, &rects));
        assert_eq!(newest_request_id(&mut registry), Some(1));
    }

    #[test]
    fn only_a_context_the_process_made_takes_requests() {
        let mut registry = registry_with_context();
        assert!(!registry.receive(CONTEXT + 1, compositor_request(1, 0)));
        registry.forget_context(CONTEXT);
        assert!(!registry.receive(CONTEXT, compositor_request(1, 0)));
        assert!(registry.requests_to_service().is_empty());
    }

    #[test]
    fn a_new_compositor_numbers_its_requests_from_the_start() {
        let mut registry = registry_with_context();
        registry.receive(CONTEXT, compositor_request(5, 0));
        registry.forget_requests();
        assert!(registry.requests_to_service().is_empty());
        assert!(registry.receive(CONTEXT, compositor_request(1, 0)));
        assert_eq!(requests_to_service(&mut registry), vec![Some(1)]);
    }

    #[test]
    fn held_requests_wait_until_released() {
        let mut registry = registry_with_context();
        registry.held_for_testing = true;
        assert!(!registry.receive(CONTEXT, compositor_request(1, 0)));
        assert!(!registry.receive(
            CONTEXT,
            InterestRequest::new(None, &[rect_of_element(1, IntRect::new(0, 0, 10, 10))])
        ));
        assert!(registry.requests_to_service().is_empty());
        assert!(registry.release_for_testing());
        assert_eq!(requests_to_service(&mut registry), vec![None]);
        assert!(registry.ask_host(CONTEXT));
        assert_eq!(registry.serviced_request_count, 1);
        registry.host_ask_failed(CONTEXT);
        assert_eq!(registry.serviced_request_count, 0);
        assert!(registry.release_for_testing());

        // The test's request replaced the compositor's, and the next test forgets it.
        registry.reset_for_testing();
        assert!(registry.take_newest_request(CONTEXT).is_none());
        assert_eq!(registry.serviced_request_count, 0);
    }

    #[test]
    fn a_list_answers_a_request_where_it_records_every_rect_asked_for() {
        let request = InterestRequest::new(
            Some(9),
            &[
                rect_of_element(1, IntRect::new(0, 1000, 800, 2000)),
                rect_of_element(2, IntRect::new(0, 0, 0, 0)),
                rect_of_element(3, IntRect::new(0, 0, 400, 400)),
            ],
        );
        let recorded_around = |rect_of_first: IntRect| {
            vec![
                rect_of_element(1, rect_of_first),
                // An empty rect asks for nothing, which any list records.
                rect_of_element(2, IntRect::new(0, 0, 0, 0)),
                rect_of_element(4, IntRect::new(0, 0, 0, 0)),
            ]
        };
        // The list records the third scroll node whole.
        assert_eq!(
            request.compositor_request_answered_by(&recorded_around(IntRect::new(0, 512, 1024, 2560))),
            Some(9)
        );
        assert_eq!(
            request.compositor_request_answered_by(&recorded_around(IntRect::new(0, 1024, 1024, 2048))),
            None
        );
        assert_eq!(request.compositor_request_answered_by(&[]), Some(9));
        // A test's request is recorded alike, but the compositor never hears of it.
        let test_request = InterestRequest::new(None, &[rect_of_element(1, IntRect::new(0, 1000, 800, 2000))]);
        assert!(test_request.is_recorded_by(&recorded_around(IntRect::new(0, 512, 1024, 2560))));
        assert!(!test_request.is_recorded_by(&recorded_around(IntRect::new(0, 1024, 1024, 2048))));
        assert_eq!(
            test_request.compositor_request_answered_by(&recorded_around(IntRect::new(0, 512, 1024, 2560))),
            None
        );
    }

    #[test]
    fn a_request_names_scroll_nodes_by_kind_and_pseudo_element() {
        let pseudo_element = FfiScrollNodeContentRect {
            scroll_node_kind: CompositorScrollNodeKind::PseudoElement,
            pseudo_element_type: 3,
            ..rect_of_element(1, IntRect::new(0, 0, 100, 100))
        };
        let request = InterestRequest::new(Some(1), &[pseudo_element]);
        assert_eq!(
            request.rect_requested_for(&rect_of_element(1, IntRect::default())),
            None
        );
        assert_eq!(
            request.rect_requested_for(&FfiScrollNodeContentRect {
                rect: IntRect::default(),
                ..pseudo_element
            }),
            Some(IntRect::new(0, 0, 100, 100))
        );
    }
}

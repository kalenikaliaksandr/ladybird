/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! The compositor's requests to record a context's display list again around new rects, which it sends as its scrolling
//! nears the edges of what the display list records. They arrive on the render clock's thread, and each context keeps
//! the newest: a later request replaces one that no frame took yet. Only a context the process made takes requests.
//!
//! Servicing a request asks the host for a frame of the context, whose commit takes the context's newest request and
//! records around its rects where they hold what the commit shows. A test may hold the requests, which then wait until
//! it releases them.

use crate::css::style::fast_hash::FastMap;
use crate::painting::ffi::FfiScrollNodeContentRect;
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
    // The compositor's id for the request, which the display list that answers it names. A test's request has none: the
    // compositor never hears of it.
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

    /// The rect the request asks for of the scroll node `scroll_node` names, whatever its own rect.
    pub(crate) fn rect_requested_for(&self, scroll_node: &FfiScrollNodeContentRect) -> Option<IntRect> {
        self.rects_by_scroll_node.get(&ScrollNodeName::of(scroll_node)).copied()
    }

    /// The compositor's id for the request, where a display list that records `recorded` of the scroll nodes it records
    /// only part of answers it: of each scroll node the request names, it records the whole rect asked for. A scroll
    /// node the list records whole holds any rect.
    pub(crate) fn compositor_request_answered_by(&self, recorded: &[FfiScrollNodeContentRect]) -> Option<u64> {
        let records_every_requested_rect = recorded.iter().all(|recorded| {
            self.rect_requested_for(recorded)
                .is_none_or(|requested| requested.is_empty() || recorded.rect.contains_rect(requested))
        });
        self.compositor_request_id.filter(|_| records_every_requested_rect)
    }

    fn is_for_testing(&self) -> bool {
        self.compositor_request_id.is_none()
    }
}

#[derive(Default)]
struct ContextInterestRequests {
    context_id: u64,
    // The newest request no committed frame took yet.
    newest_request: Option<Arc<InterestRequest>>,
    // The newest id the compositor sent: a request it sent before arrives too late.
    newest_compositor_request_id: u64,
    // Whether the host was asked for a frame of the context, and has yet to run the task that asks for it.
    host_asked: bool,
}

struct InterestRefreshRegistry {
    contexts: Vec<ContextInterestRequests>,
    held_for_testing: bool,
    serviced_request_count: u64,
}

impl InterestRefreshRegistry {
    const fn new() -> Self {
        Self {
            contexts: Vec::new(),
            held_for_testing: false,
            serviced_request_count: 0,
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

    /// Forgets every request: they came from a compositor that is gone, and the next one numbers its own from the start.
    fn forget_requests(&mut self) {
        for context in &mut self.contexts {
            context.newest_request = None;
            context.newest_compositor_request_id = 0;
        }
    }

    /// Takes `request` for the context, and answers the context to ask the host for a frame of, if any.
    fn receive(&mut self, context_id: u64, request: InterestRequest) -> Option<u64> {
        let context = self.context(context_id)?;
        if let Some(request_id) = request.compositor_request_id {
            if request_id <= context.newest_compositor_request_id {
                return None;
            }
            context.newest_compositor_request_id = request_id;
        }
        context.newest_request = Some(Arc::new(request));
        self.service(context_id)
    }

    /// Asks the host for a frame for the context's newest request, unless no request waits, the host was asked already,
    /// or a test holds the requests. Answers the context to ask the host for a frame of, if any.
    fn service(&mut self, context_id: u64) -> Option<u64> {
        if self.held_for_testing {
            return None;
        }
        let context = self.context(context_id)?;
        if context.newest_request.is_none() || context.host_asked {
            return None;
        }
        context.host_asked = true;
        self.serviced_request_count += 1;
        Some(context_id)
    }

    fn take_newest_request(&mut self, context_id: u64) -> Option<Arc<InterestRequest>> {
        self.context(context_id)?.newest_request.take()
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

    /// Lets the requests a test held go, and answers the contexts to ask the host for frames of.
    fn release_for_testing(&mut self) -> Vec<u64> {
        self.held_for_testing = false;
        let waiting: Vec<u64> = self
            .contexts
            .iter()
            .filter(|context| context.newest_request.is_some())
            .map(|context| context.context_id)
            .collect();
        waiting
            .into_iter()
            .filter_map(|context_id| self.service(context_id))
            .collect()
    }

    /// Forgets what a test did: the requests it made, its hold, and the count of requests serviced.
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
    }
}

static REGISTRY: Mutex<InterestRefreshRegistry> = Mutex::new(InterestRefreshRegistry::new());

fn registry() -> MutexGuard<'static, InterestRefreshRegistry> {
    REGISTRY.lock().unwrap_or_else(PoisonError::into_inner)
}

// Called outside the registry's lock, which a failed ask takes again.
fn ask_host(context_to_ask: Option<u64>) {
    if let Some(context_id) = context_to_ask
        && !web_render_clock_ask_host_for_interest_refresh(context_id)
    {
        registry().host_ask_failed(context_id);
    }
}

/// Has a committed frame of the compositor context `context_id` take the context's newest request, which it records
/// around.
pub(crate) fn take_newest_interest_request(context_id: u64) -> Option<Arc<InterestRequest>> {
    registry().take_newest_request(context_id)
}

/// Has the compositor context `context_id`, which the process just made, take requests.
#[unsafe(no_mangle)]
pub extern "C" fn interest_refresh_register_context(context_id: u64) {
    registry().register_context(context_id);
}

/// Forgets the compositor context `context_id`, which the process destroyed, with its requests.
#[unsafe(no_mangle)]
pub extern "C" fn interest_refresh_forget_context(context_id: u64) {
    registry().forget_context(context_id);
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
    let context_to_ask = registry().receive(context_id, InterestRequest::new(Some(request_id), rects));
    ask_host(context_to_ask);
}

/// Forgets the requests of every context, which a compositor that is gone sent. On the render clock's thread.
#[unsafe(no_mangle)]
pub extern "C" fn interest_refresh_forget_requests() {
    registry().forget_requests();
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
    let context_to_ask = registry().receive(context_id, InterestRequest::new(None, rects));
    ask_host(context_to_ask);
}

/// Holds the requests of every context, which wait until the test releases them.
#[unsafe(no_mangle)]
pub extern "C" fn interest_refresh_hold_for_testing() {
    registry().held_for_testing = true;
}

/// Lets the requests the test held go, servicing those that wait.
#[unsafe(no_mangle)]
pub extern "C" fn interest_refresh_release_for_testing() {
    let contexts_to_ask = registry().release_for_testing();
    for context_id in contexts_to_ask {
        ask_host(Some(context_id));
    }
}

/// How many requests were serviced since the test began.
#[unsafe(no_mangle)]
pub extern "C" fn interest_refresh_serviced_count_for_testing() -> u64 {
    registry().serviced_request_count
}

/// Forgets the requests a test made, its hold, and the count of requests serviced, as the next test begins.
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

    #[test]
    fn a_context_keeps_the_newest_request_the_compositor_sent() {
        let mut registry = registry_with_context();
        assert_eq!(registry.receive(CONTEXT, compositor_request(2, 0)), Some(CONTEXT));
        // The host was asked already, and the frame it commits takes the newest request.
        assert_eq!(registry.receive(CONTEXT, compositor_request(3, 600)), None);
        // A request sent before the newest arrives too late.
        assert_eq!(registry.receive(CONTEXT, compositor_request(1, 1200)), None);
        assert_eq!(registry.receive(CONTEXT, compositor_request(3, 1200)), None);
        assert_eq!(newest_request_id(&mut registry), Some(3));
        assert_eq!(registry.serviced_request_count, 1);

        // Once the host's task ran, a later request asks for another frame.
        assert!(!registry.host_task_runs(CONTEXT));
        assert_eq!(registry.receive(CONTEXT, compositor_request(4, 1800)), Some(CONTEXT));
        assert!(registry.host_task_runs(CONTEXT));
        assert_eq!(newest_request_id(&mut registry), Some(4));
    }

    #[test]
    fn only_a_context_the_process_made_takes_requests() {
        let mut registry = registry_with_context();
        assert_eq!(registry.receive(CONTEXT + 1, compositor_request(1, 0)), None);
        registry.forget_context(CONTEXT);
        assert_eq!(registry.receive(CONTEXT, compositor_request(1, 0)), None);
        assert_eq!(registry.serviced_request_count, 0);
    }

    #[test]
    fn a_new_compositor_numbers_its_requests_from_the_start() {
        let mut registry = registry_with_context();
        registry.receive(CONTEXT, compositor_request(5, 0));
        registry.host_task_runs(CONTEXT);
        registry.forget_requests();
        assert!(registry.take_newest_request(CONTEXT).is_none());
        assert_eq!(registry.receive(CONTEXT, compositor_request(1, 0)), Some(CONTEXT));
        assert_eq!(newest_request_id(&mut registry), Some(1));
    }

    #[test]
    fn held_requests_wait_until_released() {
        let mut registry = registry_with_context();
        registry.held_for_testing = true;
        assert_eq!(registry.receive(CONTEXT, compositor_request(1, 0)), None);
        assert_eq!(
            registry.receive(
                CONTEXT,
                InterestRequest::new(None, &[rect_of_element(1, IntRect::new(0, 0, 10, 10))])
            ),
            None
        );
        assert_eq!(registry.serviced_request_count, 0);
        assert_eq!(registry.release_for_testing(), vec![CONTEXT]);
        assert_eq!(registry.serviced_request_count, 1);
        registry.host_ask_failed(CONTEXT);
        assert_eq!(registry.serviced_request_count, 0);
        assert_eq!(registry.release_for_testing(), vec![CONTEXT]);

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
        assert_eq!(
            InterestRequest::new(None, &[]).compositor_request_answered_by(&[]),
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

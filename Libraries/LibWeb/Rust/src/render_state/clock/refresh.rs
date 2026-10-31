/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! Refreshes: the Paint thread keeps the last committed frame of each navigable whose recording left content out by
//! interest, with a fork of the recorder state that recorded it and the seal it presented with, and records that frame
//! again around the rects the compositor asks for, beside the event loop, presenting each such recording as a refresh
//! (see [`crate::painting::interest_refresh`]).
//!
//! A refresh re-presents a frame the render owner sampled, and reads nothing of the host: its recording borrows the
//! frame the Paint thread keeps, whose rows and paint state are copy-on-write snapshots and which leases the style
//! records its rows name alone (see [`PublishedFrame::retain`]). It copies clean output from the last recording of the
//! frame presented, shares that recording's hit-test list, and keeps the inputs the commit recorded with but for what
//! it leaves out. What it records never reaches the host, whose next commit copies from the frame the host committed.
//!
//! Only the Paint thread reaches what it keeps, through [`Presenting`], in the order of its jobs: the job of a
//! navigable's next committed frame replaces it once it presented that frame, or drops it where that frame cannot be
//! refreshed, and the job of a committed frame that keeps the display list the compositor has keeps it. A committed
//! frame that waits for the host to render its SVG images replaces it in its own job, and is refreshed only from the
//! job that presents it once the host rendered them. A refresh that recorded but did not present drops it, as its
//! recorder state no longer describes what the compositor shows, and so does one that no longer can present. It goes
//! before its document's render state does, with its compositor context, and with the connection to the compositor.

use super::super::owner::DocumentId;
use super::ClockTicks;
use crate::layout::node_data::NodeSlotId;
use crate::paint_stage::{Presenting, post_to_retained_commits};
use crate::painting::interest_refresh::{
    InterestRequest, ask_host_for_interest_refresh, interest_requests_to_service,
    note_interest_refresh_recording_discarded, note_interest_request_serviced,
};
use crate::painting::paint_state::PendingRecording;
use crate::painting::presentation::Presentation;
use crate::painting::published_frame::{PublishedFrame, RetainedFrame};
use crate::painting::record::publish::{
    publish_to_presenter, publish_with_vector_images, renders_vector_images, take_in_published_output,
};
use crate::painting::record::recorder_state::RecorderState;
use crate::painting::record::vector_images::VectorImageRenderRequest;
use crate::painting::record::{RecordingInputs, RecordingOutput};
use crate::painting::recording_slot::{
    RecordingAnswer, record_frame, times_recordings, would_draw_vector_image_not_drawn,
};
use std::sync::Arc;

/// The last committed frame of each navigable that refreshes record again, which only the Paint thread reaches.
#[derive(Default)]
pub(crate) struct RetainedCommits {
    commits: Vec<RetainedCommit>,
    /// The committed frames that wait for the host to render their SVG images, which refreshes record again once they
    /// presented.
    awaiting_vector_images: Vec<KeptCommit>,
}

/// What the Paint thread keeps of a committed frame of a navigable to refresh it, from before the frame presents: all but
/// the recorder state and the seal its presentation leaves.
pub(super) struct KeptCommit {
    context_id: u64,
    document: DocumentId,
    ticks: Arc<ClockTicks>,
    frame: RetainedFrame,
    viewport: NodeSlotId,
    /// The inputs the commit's recording recorded with, which a refresh records with but around the rects asked for.
    inputs: RecordingInputs,
}

/// What the Paint thread keeps of the last committed frame of a navigable to refresh it, once it presented.
pub(crate) struct RetainedCommit {
    kept: KeptCommit,
    /// A fork of the recorder state as the commit's recording left it, which copies clean output from the last recording
    /// of the frame presented.
    recorder: RecorderState,
    /// The SVG images the commit's display list draws, with the display lists the host rendered them as, which the
    /// presenter keeps until the next commit.
    commit_vector_image_display_lists: Vec<(VectorImageRenderRequest, u64)>,
    /// The presenter, and the seal the last recording of the frame presented with.
    presentation: Presentation,
}

enum Refreshed {
    /// It presented, recording every rect of the request it was for, or not.
    Presented { records_request: bool },
    /// The display list the compositor has records every rect of the request already.
    AlreadyRecorded,
    /// A recording of the host's own adds to the resource storage the presenter presents with: the refresh waits for the
    /// hold of the clock lanes to be released.
    WaitsForRelease,
    /// A clock lane's frame shows over the committed one, and the lane's next tick, which is queued for it where the lane
    /// only follows the pointer, records around the request.
    LeftToLane,
    /// The frame can no longer be refreshed: the host's next frame records around the request.
    LeftToHost,
}

impl RetainedCommits {
    fn take(&mut self, context_id: u64) -> Option<RetainedCommit> {
        let index = self
            .commits
            .iter()
            .position(|commit| commit.kept.context_id == context_id)?;
        Some(self.commits.swap_remove(index))
    }

    fn keep(&mut self, commit: RetainedCommit) {
        debug_assert!(
            self.commits
                .iter()
                .all(|retained| retained.kept.context_id != commit.kept.context_id),
            "the Paint thread keeps one committed frame of a navigable"
        );
        self.commits.push(commit);
    }

    /// Has `kept`, what the Paint thread keeps of the frame a commit of the compositor context `context_id` recorded,
    /// take the place of what it kept of the context's last committed frame, or drops that where there is none:
    /// refreshes record it again from now on where it presented, as `answer` answers, and once it presented where it
    /// waits for the host to render its SVG images.
    pub(super) fn replace(&mut self, context_id: u64, kept: Option<KeptCommit>, answer: &RecordingAnswer) {
        self.forget_context(context_id);
        let Some(kept) = kept else {
            return;
        };
        if answer.waits_for_vector_images() {
            self.awaiting_vector_images.push(kept);
        } else if let Some(commit) = kept.presented(answer) {
            self.keep(commit);
        }
    }

    /// Notes that the committed frame of the compositor context `context_id` that waited for the host to render its SVG
    /// images presented, as `answer` answers: refreshes record it again from now on.
    pub(crate) fn note_vector_image_frame_presented(&mut self, context_id: u64, answer: &RecordingAnswer) {
        let Some(index) = self
            .awaiting_vector_images
            .iter()
            .position(|commit| commit.context_id == context_id)
        else {
            return;
        };
        if let Some(commit) = self.awaiting_vector_images.swap_remove(index).presented(answer) {
            self.keep(commit);
        }
    }

    /// Notes that `committed` presented a committed frame of the compositor context `context_id` that keeps the display
    /// list the compositor has: the refreshes of the frame kept of the context start from it.
    pub(super) fn note_unrecorded_commit(&mut self, context_id: u64, committed: &Presentation) {
        if let Some(commit) = self
            .commits
            .iter_mut()
            .find(|commit| commit.kept.context_id == context_id)
        {
            commit.presentation.take_generation_of(committed);
        }
    }

    fn forget_document(&mut self, document: DocumentId) {
        self.commits.retain(|commit| commit.kept.document != document);
        self.awaiting_vector_images.retain(|commit| commit.document != document);
    }

    fn forget_context(&mut self, context_id: u64) {
        self.commits.retain(|commit| commit.kept.context_id != context_id);
        self.awaiting_vector_images
            .retain(|commit| commit.context_id != context_id);
    }

    fn forget_all(&mut self) {
        self.commits.clear();
        self.awaiting_vector_images.clear();
    }
}

impl KeptCommit {
    /// What the Paint thread keeps to refresh `frame`, the frame a commit of the document `ticks` belong to recorded with
    /// `inputs` for the compositor context `context_id`: none where the frame leases no style records of its rows of its
    /// own.
    pub(super) fn of(
        frame: PublishedFrame,
        viewport: NodeSlotId,
        inputs: RecordingInputs,
        ticks: &Arc<ClockTicks>,
        context_id: u64,
    ) -> Option<Self> {
        Some(Self {
            context_id,
            document: ticks.document,
            ticks: Arc::clone(ticks),
            frame: frame.retain()?,
            viewport,
            inputs,
        })
    }

    /// What the Paint thread keeps to refresh the frame, which presented as `answer` answers: none where its recording
    /// published nothing.
    fn presented(self, answer: &RecordingAnswer) -> Option<RetainedCommit> {
        let started = times_recordings().then(std::time::Instant::now);
        let (recorder, presentation) = answer.refresh_recorder()?;
        let commit_vector_image_display_lists = recorder
            .published_recording
            .as_ref()?
            .vector_image_display_lists
            .clone();
        if let Some(started) = started {
            eprintln!(
                "PAINT_RETAIN thread={} time={} µs",
                std::thread::current().name().unwrap_or("?"),
                started.elapsed().as_micros()
            );
        }
        Some(RetainedCommit {
            kept: self,
            recorder,
            commit_vector_image_display_lists,
            presentation,
        })
    }
}

impl RetainedCommit {
    /// Records the frame again around the rects of `request`, and presents it as a refresh, where the frame still shows.
    fn refresh(&mut self, request: &Arc<InterestRequest>, presenting: &mut Presenting) -> Refreshed {
        let kept = &self.kept;
        if kept.ticks.is_held() {
            return Refreshed::WaitsForRelease;
        }
        let Some(mut presentation) = self.presentation.for_refresh() else {
            if self.presentation.shows_clock_frame() && kept.ticks.lane_records_interest_requests() {
                return Refreshed::LeftToLane;
            }
            return Refreshed::LeftToHost;
        };
        // The compositor has the display list the last recording of the frame presented, which a refresh seals from.
        let published = self
            .recorder
            .published_recording
            .as_ref()
            .expect("a refresh copies from the last recording of its frame");
        if request.is_recorded_by(&published.recorded_interest.scroll_node_rects) {
            if let Some(request_id) = request.compositor_request_id() {
                presenting.answer_interest_refresh_request(kept.context_id, request_id);
            }
            return Refreshed::AlreadyRecorded;
        }
        let inputs = kept.inputs.for_interest_refresh(Arc::clone(request));
        // Only the host renders an SVG image no recording of the frame drew. The boxes whose facts hold one, and the boxes
        // that paint their inline pieces, decide before recording; an image another box draws, such as an SVG <image>
        // its SVG root draws, is caught after recording below.
        let published = Arc::clone(published);
        let commit_vector_image_display_lists = &self.commit_vector_image_display_lists;
        if would_draw_vector_image_not_drawn(kept.frame.get(), &mut self.recorder, kept.viewport, &inputs, |image| {
            rendered_vector_image_display_list_id(image, &published, commit_vector_image_display_lists).is_some()
        }) {
            return Refreshed::LeftToHost;
        }
        let (pending, _) = record_frame(kept.frame.get(), &mut self.recorder, kept.viewport, false, inputs);
        let output = match renders_vector_images(&pending) {
            false => publish_to_presenter(pending, &self.recorder, &mut presentation.presenter),
            true => {
                let Some(display_list_ids) = self.drawn_vector_image_display_lists(&pending) else {
                    note_interest_refresh_recording_discarded();
                    return Refreshed::LeftToHost;
                };
                publish_with_vector_images(pending, &self.recorder, &mut presentation.presenter, &display_list_ids)
            }
        };
        if !presentation.present(&output, presenting) {
            note_interest_refresh_recording_discarded();
            return Refreshed::LeftToHost;
        }
        let records_request = request.is_recorded_by(&output.recorded_interest.scroll_node_rects);
        take_in_published_output(&mut self.recorder, &mut None, output, true, |_, _| {});
        self.presentation = presentation;
        Refreshed::Presented { records_request }
    }

    /// The ids of the display lists the host rendered the SVG images of `pending` as, one for each, which the presenter
    /// keeps while the commit's display list or the last refresh's draws them, or none where one of the images is new to
    /// both.
    fn drawn_vector_image_display_lists(&self, pending: &PendingRecording) -> Option<Vec<u64>> {
        let published = self.recorder.published_recording.as_deref()?;
        pending
            .recording
            .resources
            .vector_image_render_requests
            .iter()
            .map(|request| {
                rendered_vector_image_display_list_id(request, published, &self.commit_vector_image_display_lists)
            })
            .collect()
    }
}

/// The display list the host rendered `request` as, where the frame's last presented recording or its commit drew it.
fn rendered_vector_image_display_list_id(
    request: &VectorImageRenderRequest,
    published: &RecordingOutput,
    commit_vector_image_display_lists: &[(VectorImageRenderRequest, u64)],
) -> Option<u64> {
    published
        .vector_image_display_lists
        .iter()
        .chain(commit_vector_image_display_lists)
        .find_map(|(drawn, id)| (drawn == request).then_some(*id))
}

/// Services the compositor's requests to record display lists around new rects that wait for the Paint thread: refreshes
/// the frame kept of each context whose request waits, or asks the host for a frame where none can be refreshed.
pub(crate) fn service_interest_requests(presenting: &mut Presenting) {
    for (context_id, request) in interest_requests_to_service() {
        let Some(mut commit) = presenting.retained_commits().take(context_id) else {
            ask_host_for_interest_refresh(context_id);
            continue;
        };
        match commit.refresh(&request, presenting) {
            Refreshed::Presented { records_request } => {
                presenting.retained_commits().keep(commit);
                match records_request {
                    true => note_interest_request_serviced(context_id, &request),
                    false => ask_host_for_interest_refresh(context_id),
                }
            }
            Refreshed::AlreadyRecorded => {
                presenting.retained_commits().keep(commit);
                note_interest_request_serviced(context_id, &request);
            }
            Refreshed::WaitsForRelease | Refreshed::LeftToLane => presenting.retained_commits().keep(commit),
            Refreshed::LeftToHost => ask_host_for_interest_refresh(context_id),
        }
    }
}

/// Drops what the Paint thread keeps of the last committed frame of the compositor context `context_id`, which the
/// process destroyed.
pub(crate) fn forget_retained_commit_of_context(context_id: u64) {
    post_to_retained_commits(move |commits| commits.forget_context(context_id));
}

/// Drops what the Paint thread keeps of every committed frame, which a compositor that is gone shows.
pub(crate) fn forget_retained_commits() {
    post_to_retained_commits(RetainedCommits::forget_all);
}

/// Drops what the Paint thread keeps of the committed frames of `document`, whose render state goes, then has the render
/// owner run `then`, which drops the state: the frames read the style records of its engine.
pub(in crate::render_state) fn forget_retained_commits_of_document_then(
    document: DocumentId,
    then: impl FnOnce() + Send + 'static,
) {
    // A test's documents present nothing.
    if cfg!(test) {
        return then();
    }
    post_to_retained_commits(move |commits| {
        commits.forget_document(document);
        super::super::post_to_render_side(then);
    });
}

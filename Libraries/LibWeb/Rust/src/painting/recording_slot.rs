/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! What a document keeps of its display list recordings from one to the next, and the recording
//! that runs on it.
//!
//! A [`RecordingJob`] owns the frame it records, the inputs it records with and the recorder state
//! it records with, and returns what it recorded with that state in a [`RecordingAnswer`]. None of
//! them names the layout arena, and the job runs on the Paint thread: the host either waits for it,
//! or commits a rendering update, whose frame the render owner samples and hands the Paint thread
//! beside the event loop, and takes its answer in once it has finished. The recording of a
//! committed frame owns its navigable's presenter, presents the frame it recorded itself, or leaves it
//! for the host to present once it rendered its SVG images, and hands the frame back to the commit's
//! Paint job, which keeps it to refresh where it can once it presented, and lets go of it otherwise
//! (see [`crate::render_state::RetainedCommits`]). The slot is
//! the document host's, on the host's thread: only the recording entries, the publication and the
//! trace reach it, through [`crate::render_state::DocumentHost::recording`].

use crate::layout::node_data::NodeSlotId;
use crate::layout::{LayoutNodeArena, RowsVersion};
use crate::paint_stage::Presenting;
use crate::painting::ffi::FfiFlightBlocker;
use crate::painting::hit_test::HitTestList;
use crate::painting::interest_refresh::CommitOnPaintQueue;
use crate::painting::paint_read::PaintSource;
use crate::painting::paint_state::{PendingRecording, PendingRecordingTrace};
use crate::painting::presentation::Presentation;
use crate::painting::presentation::VectorImageResources;
use crate::painting::published_frame::PublishedFrame;
use crate::painting::record::inputs::InterestInput;
use crate::painting::record::publish::PresentedOutput;
use crate::painting::record::recorder_state::RecorderState;
use crate::painting::record::vector_images::VectorImageRenderRequest;
use crate::painting::record::{RecordingInputs, RecordingOutput};
use crate::render_state::{LockstepProof, SampledFrame, TaskBoundary};
use crate::stage_thread::InFlight;
#[cfg(not(test))]
use crate::stage_thread::{ParkedJob, StopWord};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};

/// A display list recording: the frame it records, which it drops before it returns unless it
/// presented what it recorded of it or leaves that for the host to present, the rows version that
/// frame was frozen at, the recorder state it records with, which it returns in its answer, and for
/// the recording of a committed frame, what presents the frame it recorded.
pub(crate) struct RecordingJob {
    frame: PublishedFrame,
    rows_version: RowsVersion,
    recorder: RecorderState,
    viewport: NodeSlotId,
    trace_recordings: bool,
    presentation: Option<Presentation>,
}

/// What a recording answers: the recorder state it was handed, what it made of what it recorded at
/// which rows version, and the presentation it was handed, if any.
pub(crate) struct RecordingAnswer {
    recorder: RecorderState,
    recorded: Recorded,
    rows_version: RowsVersion,
    trace: Option<PendingRecordingTrace>,
    presentation: Option<Presentation>,
}

/// What a recording made of what it recorded: nothing, where the document's viewport had no box to
/// record, a recording for the host to publish, one it published and presented itself, nothing, for a
/// committed frame that keeps the display list the compositor has, which it presented, or a committed
/// frame that waits for its SVG images.
enum Recorded {
    Nothing,
    Pending(PendingRecording),
    Presented {
        output: PresentedOutput,
        publishes_recording: bool,
    },
    PresentedUnrecorded,
    /// A recording of a committed frame that renders SVG images, which only the host renders: the frame waits for
    /// them, with the presentation.
    NeedsVectorImages(SampledFrame<VectorImageFrame>),
}

/// A committed frame whose recording renders SVG images, with the presentation that presents it once the host has
/// rendered them, and the rows version its frame was frozen at.
pub(crate) struct VectorImageFrame {
    pending: PendingRecording,
    presentation: Presentation,
    rows_version: RowsVersion,
    /// The clock lane the frame starts once it is presented.
    lane: Option<crate::render_state::LaneDelivery>,
    /// What holds the compositor's requests back until the frame is presented.
    commit_on_paint_queue: Option<CommitOnPaintQueue>,
}

impl VectorImageFrame {
    /// The SVG images the frame renders.
    pub(crate) fn requests(&self) -> &[VectorImageRenderRequest] {
        &self.pending.recording.resources.vector_image_render_requests
    }

    /// Presents the frame, recorded with `recorder`, with `resources`, the SVG images the host rendered, whose display
    /// lists are `display_list_ids`, one for each of the frame's requests, beside the event loop.
    pub(crate) fn present(
        self,
        recorder: RecorderState,
        display_list_ids: &[u64],
        resources: VectorImageResources,
        presenting: &mut Presenting,
    ) -> RecordingAnswer {
        let Self {
            pending,
            mut presentation,
            rows_version,
            lane,
            commit_on_paint_queue,
        } = self;
        presentation
            .presenter
            .take_vector_image_resources(resources, display_list_ids);
        let context_id = presentation.compositor_context_id();
        let publishes_recording = pending.publishes_recording;
        let output = crate::painting::record::publish::publish_with_vector_images(
            pending,
            &recorder,
            &mut presentation.presenter,
            display_list_ids,
        );
        presentation.present(&output, presenting);
        let recorded = Recorded::Presented {
            output: PresentedOutput::of(output),
            publishes_recording,
        };
        let answer = RecordingAnswer {
            recorder,
            recorded,
            rows_version,
            trace: None,
            presentation: Some(presentation),
        };
        presenting
            .retained_commits()
            .note_vector_image_frame_presented(context_id, &answer);
        if let Some(lane) = lane {
            lane.deliver(&answer, presenting.retained_commits());
        }
        drop(commit_on_paint_queue);
        answer
    }
}

// The host waits for a recording only where it needs the recorder back.
impl crate::stage_thread::Flown for RecordingAnswer {
    type JoinRight = LockstepProof;
}

// A job reads nothing the document goes on writing, and an answer holds nothing of the document.
const _: () = {
    const fn assert_send<T: Send + 'static>() {}
    assert_send::<RecordingJob>();
    assert_send::<RecordingAnswer>();
};

impl RecordingAnswer {
    /// The answer of a committed frame the render owner found nothing to record of, which gives back
    /// the recorder state and presentation the commit took.
    pub(crate) fn nothing_recorded(recorder: RecorderState, presentation: Option<Presentation>) -> Self {
        Self {
            recorder,
            recorded: Recorded::Nothing,
            rows_version: RowsVersion::default(),
            trace: None,
            presentation,
        }
    }

    /// Holds a recording of a committed frame that renders SVG images on the Paint thread, as `sampled`, the frame the
    /// render owner sampled, until the host has rendered them: the answer gives back the recorder state alone.
    pub(crate) fn hold_vector_images(
        self,
        sampled: impl FnOnce(VectorImageFrame) -> SampledFrame<VectorImageFrame>,
    ) -> Self {
        let Self {
            recorder,
            recorded,
            rows_version,
            trace,
            presentation,
        } = self;
        // A recording handed a presentation presented its frame, unless it renders SVG images.
        let (recorded, presentation) = match (recorded, presentation) {
            (Recorded::Pending(pending), Some(presentation)) => (
                Recorded::NeedsVectorImages(sampled(VectorImageFrame {
                    pending,
                    presentation,
                    rows_version,
                    lane: None,
                    commit_on_paint_queue: None,
                })),
                None,
            ),
            unchanged => unchanged,
        };
        Self {
            recorder,
            recorded,
            rows_version,
            trace,
            presentation,
        }
    }

    pub(crate) fn waits_for_vector_images(&self) -> bool {
        matches!(self.recorded, Recorded::NeedsVectorImages(_))
    }

    /// Hands `lane` the pieces of the clock lane of the committed frame this answers, once it presented: at once, or
    /// once the frame that waits for its SVG images is. `retained_commits` are what the Paint thread keeps to refresh.
    pub(crate) fn start_clock_lane(
        &mut self,
        lane: crate::render_state::LaneDelivery,
        retained_commits: &crate::render_state::RetainedCommits,
    ) {
        match &mut self.recorded {
            Recorded::NeedsVectorImages(frame) => frame.get_mut().lane = Some(lane),
            _ => lane.deliver(self, retained_commits),
        }
    }

    /// Keeps `commit_on_paint_queue` until the committed frame this answers is presented: once the frame that waits for
    /// its SVG images is, or at once.
    pub(crate) fn hold_commit_on_paint_queue(&mut self, commit_on_paint_queue: CommitOnPaintQueue) {
        if let Recorded::NeedsVectorImages(frame) = &mut self.recorded {
            frame.get_mut().commit_on_paint_queue = Some(commit_on_paint_queue);
        }
    }

    /// What the clock lane of the committed frame this answers records and presents with, once the frame presented: a
    /// fork of the recorder state as the recording left it, with what it published, a seal of frames that start from
    /// the frame, and the recording the lane's fork of the render state takes in, if one published. A frame that keeps
    /// the display list a refresh of the frame kept in `retained_commits` presented starts its lane from the refreshes'
    /// recorder state instead. None where the frame presented nothing a lane starts from.
    pub(crate) fn clock_lane_recorder(
        &self,
        retained_commits: &crate::render_state::RetainedCommits,
    ) -> Option<(RecorderState, Presentation, Option<Arc<RecordingOutput>>)> {
        let presentation = self.presentation.as_ref()?;
        let (recorder, lane_presentation, output) = match &self.recorded {
            Recorded::PresentedUnrecorded => match presentation.for_clock_lane() {
                Some(lane_presentation) => (self.recorder.fork(), lane_presentation, None),
                None => {
                    let (recorder, lane_presentation) =
                        retained_commits.clock_lane_of_refresh_chain(presentation, &self.recorder)?;
                    (recorder, lane_presentation, None)
                }
            },
            Recorded::Presented {
                output,
                publishes_recording: true,
            } => (
                fork_after(self.recorder.fork(), output),
                presentation.for_clock_lane()?,
                Some(output.output.clone()),
            ),
            _ => return None,
        };
        Some((recorder, lane_presentation, output))
    }

    /// What refreshes of the committed frame this answers record and present with, once the frame presented a recording
    /// that publishes: a fork of the recorder state as the recording left it, with what it published and the absolute
    /// rects it computed, and what the refreshes seal theirs from.
    pub(crate) fn refresh_recorder(&self) -> Option<(RecorderState, Presentation)> {
        let Recorded::Presented {
            output,
            publishes_recording: true,
        } = &self.recorded
        else {
            return None;
        };
        let recorder = fork_after(self.recorder.fork_keeping_absolute_rects(), output);
        Some((recorder, self.presentation.as_ref()?.for_refreshes()))
    }

    /// The answer of a committed frame that records nothing, which it presented, and gives back the recorder state and
    /// presentation the commit took.
    pub(crate) fn presented_unrecorded(recorder: RecorderState, presentation: Presentation) -> Self {
        Self {
            recorder,
            recorded: Recorded::PresentedUnrecorded,
            rows_version: RowsVersion::default(),
            trace: None,
            presentation: Some(presentation),
        }
    }
}

/// `fork`, a fork of the recorder state a recording that published `published` recorded with, as it left the state,
/// which copies from what it published.
fn fork_after(mut fork: RecorderState, published: &PresentedOutput) -> RecorderState {
    fork.published_hit_test_items = Some(published.published_hit_test_items());
    fork.published_recording = Some(published.output.clone());
    fork
}

impl RecordingJob {
    /// The recording of `frozen`, the frame of `viewport` frozen for it, with `recorder`, which presents what it
    /// recorded with `presentation`, if any.
    pub(crate) fn new(
        frozen: FrozenFrame,
        recorder: RecorderState,
        viewport: NodeSlotId,
        presentation: Option<Presentation>,
    ) -> Self {
        Self {
            frame: frozen.frame,
            rows_version: frozen.rows_version,
            recorder,
            viewport,
            trace_recordings: frozen.trace_recordings,
            presentation,
        }
    }

    /// Records the frame with `inputs` on the Paint thread, and waits for what it recorded.
    pub(crate) fn run_on_paint_thread(self, inputs: RecordingInputs) -> RecordingAnswer {
        if cfg!(test) {
            return self.run(inputs, None).0;
        }
        release_held_recording_for_testing();
        crate::paint_stage::paint_thread().run(|| self.run(inputs, None).0)
    }

    /// Records the frame with `inputs` on the Paint thread beside the host, and presents it with the job's
    /// presentation, if any. Answers the frame as well, where it presented what it recorded of it, or leaves that for
    /// the host to present once it rendered its SVG images.
    pub(crate) fn run_beside_host(
        self,
        inputs: RecordingInputs,
        presenting: &mut Presenting,
    ) -> (RecordingAnswer, Option<PublishedFrame>) {
        self.run(inputs, Some(presenting))
    }

    /// Records the frame with `inputs`, and presents it where the job has a presentation and runs beside the host
    /// with `presenting`, in which case it answers the frame as well, or leaves it for the host to present. It takes no
    /// main thread token, so nothing it calls can reach the host.
    fn run(
        self,
        inputs: RecordingInputs,
        presenting: Option<&mut Presenting>,
    ) -> (RecordingAnswer, Option<PublishedFrame>) {
        let Self {
            frame,
            rows_version,
            mut recorder,
            viewport,
            trace_recordings,
            mut presentation,
        } = self;
        let (pending, trace) = record_frame(&frame, &mut recorder, viewport, trace_recordings, inputs);
        // A recording that renders an SVG image the recording published last did not draw leaves its frame for the host
        // to present, which renders it.
        let vector_image_display_lists = match crate::painting::record::publish::renders_vector_images(&pending) {
            false => Some(None),
            true => {
                crate::painting::record::publish::published_vector_image_display_lists(&pending, &recorder).map(Some)
            }
        };
        let (recorded, presented_frame) = match (presentation.as_mut(), presenting, vector_image_display_lists) {
            (Some(presentation), Some(presenting), Some(vector_image_display_lists)) => {
                let publishes_recording = pending.publishes_recording;
                let (output, _) = match vector_image_display_lists {
                    Some(display_list_ids) => {
                        present_with_vector_images(presentation, pending, &recorder, presenting, &display_list_ids)
                    }
                    None => present(presentation, pending, &recorder, presenting),
                };
                let recorded = Recorded::Presented {
                    output: PresentedOutput::of(output),
                    publishes_recording,
                };
                (recorded, Some(frame))
            }
            (Some(_), Some(_), None) => (Recorded::Pending(pending), Some(frame)),
            _ => {
                drop(frame);
                (Recorded::Pending(pending), None)
            }
        };
        let answer = RecordingAnswer {
            recorder,
            recorded,
            rows_version,
            trace,
            presentation,
        };
        (answer, presented_frame)
    }
}

/// The test hold on recordings: a test arms it for the next recording that flies, which then reads nothing of its frame
/// until the test releases it, so that the test writes the document beside a recording that has read nothing yet.
enum RecordingHold {
    Idle,
    Armed,
    Holding,
    // A layout held is never handed to the StyleLayout thread until it goes, so that the thread runs the jobs of other
    // documents meanwhile, as one the collector finalizes asks of it.
    #[cfg(not(test))]
    HoldingLayout {
        _parked: ParkedJob,
    },
}

static RECORDING_HOLD: Mutex<RecordingHold> = Mutex::new(RecordingHold::Idle);
static RECORDING_HOLD_RELEASED: Condvar = Condvar::new();

fn recording_hold() -> MutexGuard<'static, RecordingHold> {
    RECORDING_HOLD.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Whether a test holds a recording that flies.
pub(crate) fn recording_is_held_for_testing() -> bool {
    matches!(*recording_hold(), RecordingHold::Holding)
}

/// Whether the recording about to fly is the one the test hold is armed for.
pub(crate) fn take_recording_hold_for_testing() -> bool {
    let mut hold = recording_hold();
    if !matches!(*hold, RecordingHold::Armed) {
        return false;
    }
    *hold = RecordingHold::Holding;
    true
}

/// Submits `job`, the layout of a frame, to the StyleLayout thread, or parks it where the test hold is armed for it.
/// Answers the flight and whether the hold took it.
#[cfg(not(test))]
pub(crate) fn submit_layout<R: Send + 'static>(
    job: impl FnOnce(&StopWord) -> R + Send + 'static,
) -> (InFlight<R>, bool) {
    let thread = crate::stage_thread::style_layout_thread();
    let mut hold = recording_hold();
    if !matches!(*hold, RecordingHold::Armed) {
        return (thread.submit(job), false);
    }
    let (flight, parked) = thread.park(job);
    *hold = RecordingHold::HoldingLayout { _parked: parked };
    (flight, true)
}

pub(crate) fn wait_while_recording_is_held_for_testing() {
    let _released = RECORDING_HOLD_RELEASED
        .wait_while(recording_hold(), |hold| matches!(hold, RecordingHold::Holding))
        .unwrap_or_else(PoisonError::into_inner);
}

/// Holds the next recording that flies before it reads its frame, until
/// [`release_held_recording_for_testing`] or a wait for it.
pub(crate) fn hold_next_recording_for_testing() {
    *recording_hold() = RecordingHold::Armed;
}

/// Lets the recording held for the test go, or disarms the hold no recording has taken yet.
pub(crate) fn release_held_recording_for_testing() {
    *recording_hold() = RecordingHold::Idle;
    RECORDING_HOLD_RELEASED.notify_all();
}

fn painting_diagnostic_is_on(variable: &str) -> bool {
    std::env::var_os(variable).is_some_and(|value| !value.is_empty() && value != "0")
}

/// Whether each recording tells how long it took, what for, and how much it recorded, and the Paint thread how long
/// keeping a committed frame to refresh took.
pub(crate) fn times_recordings() -> bool {
    static TIMES_RECORDINGS: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *TIMES_RECORDINGS.get_or_init(|| painting_diagnostic_is_on("LADYBIRD_RUST_PAINTING_TIMING"))
}

/// Whether a timed recording that publishes also counts the producers it left out, a walk of the whole paint-order tree
/// that delays its present.
fn counts_culled_producers() -> bool {
    static COUNTS_CULLED_PRODUCERS: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *COUNTS_CULLED_PRODUCERS.get_or_init(|| painting_diagnostic_is_on("LADYBIRD_RUST_PAINTING_CULLED_PRODUCERS"))
}

/// Records `frame` of the document's `viewport` with `inputs` and `recorder`, which keeps the inputs of a recording that
/// publishes, and answers what it recorded, with the trace it left where `trace_recordings` asks for one. What it
/// answers reads nothing more of the frame, which the caller may drop at once.
pub(crate) fn record_frame(
    frame: &PublishedFrame,
    recorder: &mut RecorderState,
    viewport: NodeSlotId,
    trace_recordings: bool,
    inputs: RecordingInputs,
) -> (PendingRecording, Option<PendingRecordingTrace>) {
    let RecorderState {
        published_recording,
        published_hit_test_items,
        paint_order_tree,
        scratch,
        absolute_rects,
        published_inputs,
    } = recorder;
    let source = PaintSource::new(frame, absolute_rects);
    let started = times_recordings().then(std::time::Instant::now);
    let recording = crate::painting::record::traversal::record_display_list(
        &source,
        scratch,
        paint_order_tree,
        viewport,
        &inputs,
        published_recording.clone(),
        published_hit_test_items.clone(),
        trace_recordings,
    );
    if let Some(started) = started {
        let elapsed = started.elapsed();
        // Only a recording that publishes leaves the paint-order tree describing what it recorded.
        let culled_producers = match inputs.publishes_recording && counts_culled_producers() {
            true => paint_order_tree.culled_producer_counts().to_string(),
            false => "-".into(),
        };
        eprintln!(
            "PAINT_RECORD thread={} purpose={} interest={} bytes={} culled={} time={} µs",
            std::thread::current().name().unwrap_or("?"),
            inputs.purpose.name(),
            inputs.interest.name(),
            recording.output.display_list.bytes.len(),
            culled_producers,
            elapsed.as_micros()
        );
    }
    let trace = trace_recordings.then_some(PendingRecordingTrace {
        viewport,
        should_paint_overlay: inputs.should_paint_overlay,
    });
    let svg_paint_resources = frame.svg_paint_resources().clone();
    let publishes_recording = inputs.publishes_recording;
    if publishes_recording {
        // Recordings made with these inputs later, such as clock lane frames, are no test's recordings, and head for no
        // smooth scroll's destination: the recording that took them does. A clock lane's frame records around the
        // newest request and the display list it replaces, which it reads itself.
        let interest = match inputs.interest {
            InterestInput::AroundScrollPositionsForTesting => InterestInput::WholeDocument,
            InterestInput::AroundScrollPositions { .. } => InterestInput::AroundScrollPositions {
                requested: None,
                smooth_scroll_destinations: Arc::new([]),
            },
            InterestInput::AsRequested { .. } => InterestInput::AsRequested {
                requested: None,
                presented: None,
            },
            ref interest => interest.clone(),
        };
        *published_inputs = Some(RecordingInputs { interest, ..inputs });
    }
    (
        PendingRecording {
            recording,
            publishes_recording,
            svg_paint_resources,
        },
        trace,
    )
}

/// See [`crate::painting::record::traversal::would_draw_vector_image_not_drawn`]. It leaves the recorder state as it was,
/// but for the absolute rects it computed.
pub(crate) fn would_draw_vector_image_not_drawn(
    frame: &PublishedFrame,
    recorder: &mut RecorderState,
    viewport: NodeSlotId,
    inputs: &RecordingInputs,
    drawn: impl Fn(&VectorImageRenderRequest) -> bool,
) -> bool {
    let RecorderState {
        published_recording,
        paint_order_tree,
        scratch,
        absolute_rects,
        ..
    } = recorder;
    let source = PaintSource::new(frame, absolute_rects);
    crate::painting::record::traversal::would_draw_vector_image_not_drawn(
        &source,
        scratch,
        paint_order_tree,
        viewport,
        inputs,
        published_recording.as_ref(),
        drawn,
    )
}

/// Presents `pending`, which renders no SVG image and was recorded with `recorder`, with `presentation`, beside the event
/// loop, and answers its output, and whether the frame was presented (see [`Presentation::present`]).
pub(crate) fn present(
    presentation: &mut Presentation,
    pending: PendingRecording,
    recorder: &RecorderState,
    presenting: &mut Presenting,
) -> (RecordingOutput, bool) {
    let output = crate::painting::record::publish::publish_to_presenter(pending, recorder, &mut presentation.presenter);
    let presented = presentation.present(&output, presenting);
    (output, presented)
}

/// Presents `pending`, which renders SVG images and was recorded with `recorder`, with `presentation`, beside the event
/// loop, drawing them as the display lists `display_list_ids` names, which the presenter holds, and answers its output,
/// and whether the frame was presented.
pub(crate) fn present_with_vector_images(
    presentation: &mut Presentation,
    pending: PendingRecording,
    recorder: &RecorderState,
    presenting: &mut Presenting,
    display_list_ids: &[u64],
) -> (RecordingOutput, bool) {
    let output = crate::painting::record::publish::publish_with_vector_images(
        pending,
        recorder,
        &mut presentation.presenter,
        display_list_ids,
    );
    let presented = presentation.present(&output, presenting);
    (output, presented)
}

/// What the host knows that freezing a document's frame for a recording reads.
pub(crate) struct FrameInputs {
    pub(crate) viewport: NodeSlotId,
    pub(crate) css_viewport_rect: crate::css::css_pixels::CssPixelRect,
    pub(crate) publishes_recording: bool,
    /// The canvas rect the root background painted in the recording published last, if any.
    pub(crate) published_root_background_canvas_rect: Option<crate::css::css_pixels::CssPixelRect>,
    pub(crate) hit_test_item_capacity_hint: usize,
}

/// A document's frame, frozen for a recording, with what the recording reads beside it, and the rows version it was
/// frozen at.
pub(crate) struct FrozenFrame {
    pub(crate) frame: crate::painting::published_frame::PublishedFrame,
    pub(crate) tree_inputs: crate::painting::host::FfiVisualContextTreeInputs,
    pub(crate) root_background_source: crate::painting::host::RootBackgroundSource,
    pub(crate) trace_recordings: bool,
    pub(crate) rows_version: crate::layout::RowsVersion,
}

/// Freezes the frame of the document whose arena `arena` is for a recording of its viewport, or none where the
/// viewport has no box to paint.
pub(crate) fn freeze_recording_frame(arena: &mut LayoutNodeArena, inputs: FrameInputs) -> Option<FrozenFrame> {
    // Recording reads overflow, and reading overflow never measures it.
    arena.measure_scrollable_overflow();
    if !arena.paintable_row_is_populated(inputs.viewport) || arena.stacking_context_entries(inputs.viewport).is_none() {
        return None;
    }
    // The root background paints the union of the viewport and the root's overflow, so it is the
    // one output a viewport move can change. Drop its caches before the frame is published instead
    // of treating the viewport position as a frame-wide input.
    if let Some(published_canvas_rect) = inputs.published_root_background_canvas_rect {
        let root = arena
            .paint_state()
            .root_background_source
            .expect("a recording follows paint preparation")
            .root_layout_node;
        let canvas_rect = crate::painting::record::paint::background_resolution::root_background_canvas_rect(
            &arena.paintable_rows(),
            root,
            inputs.css_viewport_rect,
        );
        if canvas_rect != published_canvas_rect {
            arena.push_paint_damage(root, crate::painting::record::damage::PaintDamage::DRAW_BACKGROUND);
        }
    }
    if inputs.publishes_recording {
        arena.note_publishing_paint_recording_started();
    }
    // The recording reads the document as it is now: what the host writes after this goes to the next frame.
    let frame = arena.freeze_frame(inputs.hit_test_item_capacity_hint);
    let rows_version = arena.rows_version();
    let paint_state = arena.paint_state();
    Some(FrozenFrame {
        frame,
        rows_version,
        tree_inputs: paint_state
            .visual_context
            .last_tree_inputs
            .expect("a recording follows a visual context update"),
        root_background_source: paint_state
            .root_background_source
            .expect("a recording follows paint preparation"),
        trace_recordings: paint_state.trace_recordings,
    })
}

/// The right of a rendering update's frame to fly beside the event loop: the document had no
/// flight blocker. Only [`FlightLicense::for_blocker`] mints one.
pub(crate) struct FlightLicense {
    _private: (),
}

impl FlightLicense {
    /// The license of a recording whose document has `blocker`, if it has none.
    pub(crate) fn for_blocker(blocker: FfiFlightBlocker) -> Option<Self> {
        (blocker == FfiFlightBlocker::None).then_some(Self { _private: () })
    }
}

/// The reason the host waits for its document's recording in flight: a recording it starts, or the
/// recording it reads, needs the recorder state the one in flight took.
pub(crate) struct RecordingNeedsItsRecorder {
    _private: (),
}

pub(crate) const RECORDING_NEEDS_ITS_RECORDER: RecordingNeedsItsRecorder = RecordingNeedsItsRecorder { _private: () };

/// Where the recorder state a document's recordings record with is.
#[expect(
    clippy::large_enum_variant,
    reason = "a document has one slot, and its recorder state moves to and from its recordings without an allocation"
)]
enum Recorder {
    /// With the document, for its next recording.
    Here(RecorderState),
    /// With the recording in flight, whose answer brings it back.
    InFlight(InFlight<RecordingAnswer>),
}

impl Default for Recorder {
    fn default() -> Self {
        Self::Here(RecorderState::default())
    }
}

/// How the recording in flight of a document landed.
pub(crate) enum RecordingLanding {
    NoneInFlight,
    StillInFlight,
    /// The recording landed: what it recorded is pending for the host to publish and present, or the
    /// recording presented it itself. It gives back the presentation it was handed, if any.
    Landed(Option<Presentation>),
    /// The recording found the document's viewport had no box to record, and gives back the presentation
    /// it was handed, if any.
    NothingRecorded(Option<Presentation>),
    /// The committed frame recorded nothing, and presented the display list the compositor has; it gives
    /// back the presentation it was handed.
    PresentedUnrecorded(Option<Presentation>),
    /// The committed frame's recording renders SVG images, which the host renders for the frame (see
    /// [`RecordingSlot::take_vector_image_frame`]); it keeps the presentation.
    NeedsVectorImages,
    /// The recording landed after the host wrote the document's rows. What it recorded stands as
    /// the compositor's frame, whether it presented it or the host does, but not its hit-test list,
    /// which names boxes that may be gone: the document keeps none.
    LandedBehindRows(Option<Presentation>),
}

/// What takes in a recording that presented itself: its output, whether the document's hit-test list
/// changed, and whether the recording publishes.
pub(crate) type TakeInPresented<'a> = &'a mut dyn FnMut(Arc<RecordingOutput>, bool, bool);

/// A recording the host publishes, with what its publication writes of the document's recordings.
pub(crate) struct Publication<'a> {
    pub(crate) pending: PendingRecording,
    /// Whether the recording landed after the host wrote the document's rows, so that the hit-test
    /// list it made names boxes that may be gone.
    pub(crate) behind_rows: bool,
    pub(crate) recorder: &'a mut RecorderState,
    pub(crate) hit_test_list: &'a mut Option<HitTestList>,
}

/// A recording that landed for the host to publish, and whether it landed behind the rows.
struct PendingPublication {
    pending: PendingRecording,
    behind_rows: bool,
}

/// What a document keeps of its recordings: the recording its host is to publish, the trace that
/// recording left, the recorder state the next recording records with, and the hit-test list of the
/// last recording published, which hit testing reads.
#[derive(Default)]
pub(crate) struct RecordingSlot {
    pending_recording: Option<PendingPublication>,
    /// The committed frame that waits for the host to render its SVG images.
    vector_image_frame: Option<SampledFrame<VectorImageFrame>>,
    pending_recording_trace: Option<PendingRecordingTrace>,
    recorder: Recorder,
    hit_test_list: Option<HitTestList>,
}

impl RecordingSlot {
    pub(crate) fn has_pending_recording(&self) -> bool {
        self.pending_recording.is_some()
    }

    /// The pending recording, for the host to publish, with the recorder state it recorded with and
    /// the hit-test list it replaces. A recording is pending only once it has landed.
    pub(crate) fn take_publication(&mut self) -> Option<Publication<'_>> {
        let Recorder::Here(recorder) = &mut self.recorder else {
            return None;
        };
        let PendingPublication { pending, behind_rows } = self.pending_recording.take()?;
        Some(Publication {
            pending,
            behind_rows,
            recorder,
            hit_test_list: &mut self.hit_test_list,
        })
    }

    /// The trace the pending recording left, for the host to read.
    pub(crate) fn take_pending_recording_trace(&mut self) -> Option<PendingRecordingTrace> {
        self.pending_recording_trace.take()
    }

    /// Whether a recording of the document is in flight.
    pub(crate) fn has_recording_in_flight(&self) -> bool {
        matches!(self.recorder, Recorder::InFlight(_))
    }

    /// The hit-test list of the last recording published, if any was.
    pub(crate) fn hit_test_list(&mut self) -> &mut Option<HitTestList> {
        &mut self.hit_test_list
    }

    /// How many items the next recording's hit-test list may hold, going by the last one's.
    pub(crate) fn hit_test_item_capacity_hint(&self) -> usize {
        self.hit_test_list.as_ref().map_or(0, |list| list.items.len())
    }

    /// The recorder state, for a recording to take until it answers. The host takes a recording in
    /// flight in before it records again.
    pub(crate) fn take_recorder(&mut self) -> RecorderState {
        match std::mem::take(&mut self.recorder) {
            Recorder::Here(recorder) => recorder,
            Recorder::InFlight(_) => panic!("the host takes its recording in flight in before it records again"),
        }
    }

    /// Gives back the recorder state a recording took, where it recorded nothing.
    pub(crate) fn give_back_recorder(&mut self, recorder: RecorderState) {
        self.recorder = Recorder::Here(recorder);
    }

    /// Takes in what a recording answered: a recording pending for the host to publish, or one the
    /// recording presented itself, which `take_in` takes in. `behind_rows` says the host wrote the
    /// document's rows since the frame it recorded. Answers the presentation the recording gives back.
    fn land(
        &mut self,
        answer: RecordingAnswer,
        behind_rows: bool,
        take_in: TakeInPresented<'_>,
    ) -> Option<Presentation> {
        let RecordingAnswer {
            mut recorder,
            recorded,
            trace,
            presentation,
            ..
        } = answer;
        self.pending_recording_trace = trace;
        match recorded {
            Recorded::Nothing | Recorded::PresentedUnrecorded => {}
            Recorded::NeedsVectorImages(frame) => self.vector_image_frame = Some(frame),
            Recorded::Pending(pending) => self.pending_recording = Some(PendingPublication { pending, behind_rows }),
            Recorded::Presented {
                output,
                publishes_recording,
            } => crate::painting::record::publish::take_in_presented_output(
                &mut recorder,
                &mut self.hit_test_list,
                output,
                publishes_recording,
                |output, hit_test_list_changed| {
                    take_in(output, hit_test_list_changed || behind_rows, publishes_recording);
                },
            ),
        }
        self.recorder = Recorder::Here(recorder);
        if behind_rows {
            self.hit_test_list = None;
        }
        presentation
    }

    /// Takes in what a recording the host waited for answered, leaving it pending for the host to
    /// publish.
    pub(crate) fn accept_recording_answer(&mut self, answer: RecordingAnswer) {
        let presentation = self.land(answer, false, &mut |_, _, _| {
            unreachable!("a recording the host waits for presents nothing itself")
        });
        debug_assert!(
            presentation.is_none(),
            "a recording the host waits for presents nothing"
        );
    }

    /// Lets the recording `flight` fly with the recorder state.
    pub(crate) fn fly(&mut self, flight: InFlight<RecordingAnswer>) {
        debug_assert!(
            !self.has_recording_in_flight() && !self.has_pending_recording(),
            "a document records one frame at a time"
        );
        self.recorder = Recorder::InFlight(flight);
    }

    /// Takes the recording in flight in at `boundary`, where it has finished. `rows_stand` answers
    /// whether the document's rows are still at the version of the frame it recorded.
    pub(crate) fn take_finished_recording_in(
        &mut self,
        boundary: &TaskBoundary,
        rows_stand: impl FnOnce(RowsVersion) -> bool,
        take_in: TakeInPresented<'_>,
    ) -> RecordingLanding {
        let Recorder::InFlight(flight) = std::mem::take(&mut self.recorder) else {
            return RecordingLanding::NoneInFlight;
        };
        match flight.try_take(boundary) {
            Ok(answer) => self.landing(answer, rows_stand, take_in),
            Err(flight) => {
                self.recorder = Recorder::InFlight(flight);
                RecordingLanding::StillInFlight
            }
        }
    }

    /// Waits for the recording in flight, and the frame it presents, and takes it in (see
    /// [`Self::take_finished_recording_in`]).
    pub(crate) fn join_recording_in_flight(
        &mut self,
        rows_stand: impl FnOnce(RowsVersion) -> bool,
        take_in: TakeInPresented<'_>,
    ) -> RecordingLanding {
        let Recorder::InFlight(flight) = std::mem::take(&mut self.recorder) else {
            return RecordingLanding::NoneInFlight;
        };
        release_held_recording_for_testing();
        let answer = flight.join(LockstepProof::for_reason(&RECORDING_NEEDS_ITS_RECORDER));
        self.landing(answer, rows_stand, take_in)
    }

    fn landing(
        &mut self,
        answer: RecordingAnswer,
        rows_stand: impl FnOnce(RowsVersion) -> bool,
        take_in: TakeInPresented<'_>,
    ) -> RecordingLanding {
        match answer.recorded {
            Recorded::Nothing => return RecordingLanding::NothingRecorded(self.land(answer, false, take_in)),
            Recorded::PresentedUnrecorded => {
                return RecordingLanding::PresentedUnrecorded(self.land(answer, false, take_in));
            }
            Recorded::NeedsVectorImages(_) => {
                let presentation = self.land(answer, false, take_in);
                debug_assert!(presentation.is_none(), "the frame keeps the presentation");
                return RecordingLanding::NeedsVectorImages;
            }
            Recorded::Pending(_) | Recorded::Presented { .. } => {}
        }
        let rows_stand = rows_stand(answer.rows_version);
        let presentation = self.land(answer, !rows_stand, take_in);
        if rows_stand {
            RecordingLanding::Landed(presentation)
        } else {
            RecordingLanding::LandedBehindRows(presentation)
        }
    }

    /// The committed frame that waits for the host to render its SVG images, with the recorder state, which goes with
    /// it to the Paint thread again: the host hands the frame back to the slot to fly with.
    pub(crate) fn take_vector_image_frame(&mut self) -> Option<(SampledFrame<VectorImageFrame>, RecorderState)> {
        let frame = self.vector_image_frame.take()?;
        Some((frame, self.take_recorder()))
    }

    /// Drops the pending recording unpublished. A recording that publishes wrote the retained
    /// paint-order tree, so the next one copies nothing from the published recording.
    pub(crate) fn discard_pending_recording(&mut self) {
        self.pending_recording_trace = None;
        if let Some(PendingPublication { pending, .. }) = self.pending_recording.take()
            && pending.publishes_recording
            && let Recorder::Here(recorder) = &mut self.recorder
        {
            recorder.forget_published_recording();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::LayoutNodeArena;
    use crate::painting::record::{RecordingOutput, RecordingResult};
    use std::sync::Arc;

    /// The slot's recorder state, or none while a recording in flight has it.
    fn recorder_of(slot: &mut RecordingSlot) -> Option<&mut RecorderState> {
        match &mut slot.recorder {
            Recorder::Here(recorder) => Some(recorder),
            Recorder::InFlight(_) => None,
        }
    }

    fn pending(publishes_recording: bool) -> PendingRecording {
        PendingRecording {
            recording: RecordingResult {
                output: RecordingOutput::default(),
                resources: Default::default(),
            },
            publishes_recording,
            svg_paint_resources: Default::default(),
        }
    }

    fn answer(recorder: RecorderState, recorded: Recorded) -> RecordingAnswer {
        RecordingAnswer {
            recorder,
            recorded,
            rows_version: RowsVersion::default(),
            trace: None,
            presentation: None,
        }
    }

    /// Leaves `pending` in the slot as a recording's answer would.
    fn accept_pending(slot: &mut RecordingSlot, pending: PendingRecording) {
        let recorder = slot.take_recorder();
        slot.accept_recording_answer(answer(recorder, Recorded::Pending(pending)));
    }

    #[test]
    fn the_slot_lends_its_recorder_state_to_a_job_and_takes_it_back_with_the_answer() {
        let mut engine = crate::css::style::StyleEngine::new();
        let mut arena = LayoutNodeArena::new();
        arena.set_style_engine(crate::css::style::StyleEngineHandle::from_raw(&raw mut engine));
        let mut slot = RecordingSlot::default();
        let published = Arc::new(RecordingOutput::default());
        recorder_of(&mut slot).unwrap().published_recording = Some(published.clone());

        let job = RecordingJob {
            frame: arena.freeze_frame(0),
            rows_version: RowsVersion::default(),
            recorder: slot.take_recorder(),
            viewport: NodeSlotId::INVALID,
            trace_recordings: false,
            presentation: None,
        };
        assert!(recorder_of(&mut slot).unwrap().published_recording.is_none());
        let RecordingJob { recorder, .. } = job;
        slot.accept_recording_answer(answer(recorder, Recorded::Pending(pending(true))));
        assert!(Arc::ptr_eq(
            recorder_of(&mut slot).unwrap().published_recording.as_ref().unwrap(),
            &published
        ));
        assert!(slot.has_pending_recording());
    }

    #[test]
    fn a_discarded_publishing_recording_forgets_its_source() {
        let mut slot = RecordingSlot::default();
        let published = Arc::new(RecordingOutput::default());
        recorder_of(&mut slot).unwrap().published_recording = Some(published.clone());

        accept_pending(&mut slot, pending(false));
        slot.discard_pending_recording();
        assert!(!slot.has_pending_recording());
        assert!(recorder_of(&mut slot).unwrap().published_recording.is_some());

        accept_pending(&mut slot, pending(true));
        slot.discard_pending_recording();
        assert!(recorder_of(&mut slot).unwrap().published_recording.is_none());
    }

    /// A flight of the slot's recorder state that answers it back with `recorded`.
    fn flight_of(slot: &mut RecordingSlot, recorded: Recorded) -> InFlight<RecordingAnswer> {
        let recorder = slot.take_recorder();
        crate::paint_stage::paint_thread().submit(move |_| answer(recorder, recorded))
    }

    fn take_in_nothing(_: Arc<RecordingOutput>, _: bool, _: bool) {
        panic!("the recording presented nothing itself");
    }

    #[test]
    fn a_recording_in_flight_has_the_recorder_state_until_it_lands() {
        let mut slot = RecordingSlot::default();
        let published = Arc::new(RecordingOutput::default());
        recorder_of(&mut slot).unwrap().published_recording = Some(published.clone());
        let flight = flight_of(&mut slot, Recorded::Pending(pending(true)));
        slot.fly(flight);
        assert!(slot.has_recording_in_flight());
        assert!(recorder_of(&mut slot).is_none());
        assert!(
            slot.take_publication().is_none(),
            "nothing is pending before the recording lands"
        );

        assert!(matches!(
            slot.join_recording_in_flight(|_| true, &mut take_in_nothing),
            RecordingLanding::Landed(None)
        ));
        assert!(!slot.has_recording_in_flight());
        let publication = slot.take_publication().unwrap();
        assert!(Arc::ptr_eq(
            publication.recorder.published_recording.as_ref().unwrap(),
            &published
        ));
    }

    #[test]
    fn a_recording_that_presented_itself_lands_its_output_as_the_published_one() {
        let mut slot = RecordingSlot::default();
        let flight = flight_of(
            &mut slot,
            Recorded::Presented {
                output: PresentedOutput::of(RecordingOutput::default()),
                publishes_recording: true,
            },
        );
        slot.fly(flight);
        let mut taken_in = None;
        let landing = loop {
            match slot.take_finished_recording_in(
                &TaskBoundary::for_test(),
                |_| true,
                &mut |output, _, publishes_recording| {
                    taken_in = Some((output, publishes_recording));
                },
            ) {
                RecordingLanding::StillInFlight => crate::paint_stage::paint_thread().run(|| ()),
                landing => break landing,
            }
        };
        assert!(matches!(landing, RecordingLanding::Landed(None)));
        assert!(!slot.has_pending_recording(), "the host publishes nothing more");
        let (output, publishes_recording) = taken_in.expect("the output is taken in");
        assert!(publishes_recording);
        assert!(Arc::ptr_eq(
            recorder_of(&mut slot).unwrap().published_recording.as_ref().unwrap(),
            &output
        ));
        assert!(slot.hit_test_list().is_some());
    }

    #[test]
    fn a_recording_that_lands_behind_the_rows_keeps_no_hit_test_list() {
        let mut slot = RecordingSlot::default();
        let flight = flight_of(
            &mut slot,
            Recorded::Presented {
                output: PresentedOutput::of(RecordingOutput::default()),
                publishes_recording: false,
            },
        );
        slot.fly(flight);
        let mut hit_test_list_changed = false;
        let landing = slot.join_recording_in_flight(|_| false, &mut |_, changed, _| hit_test_list_changed = changed);
        assert!(matches!(landing, RecordingLanding::LandedBehindRows(None)));
        assert!(
            hit_test_list_changed,
            "the hit-test list the document read is no longer current"
        );
        assert!(slot.hit_test_list().is_none());

        let flight = flight_of(&mut slot, Recorded::Pending(pending(false)));
        slot.fly(flight);
        let landing = slot.join_recording_in_flight(|_| false, &mut take_in_nothing);
        assert!(matches!(landing, RecordingLanding::LandedBehindRows(None)));
        let publication = slot.take_publication().expect("the host presents what it did not");
        assert!(publication.behind_rows);
    }
}

# Moving the compositor to a separate process

This is a first design iteration for moving the current LibWeb compositor
thread out of the WebContent process and into a dedicated helper process.

The proposal keeps layout, style, DOM, JavaScript, and display list recording
in WebContent. The new Compositor process owns display list replay, async
scrolling state, viewport scrollbar interaction, backing stores, and frame
presentation to the UI process.

## Current architecture

The compositor is currently implemented as `Web::Compositor::CompositorThread`
in `Libraries/LibWeb/Compositor/CompositorThread.cpp`.

Each non-SVG `HTML::Navigable` owns a `CompositorThread` member. The top-level
traversable registers itself for presentation to the UI process. Child
navigables render into compositor surfaces owned by their parent compositor.

On each rendering update, `HTML::Navigable` records a display list and a scroll
state snapshot:

- `Navigable::record_display_list_and_scroll_state()` records display lists,
  creates `DisplayListResourceTransaction`s, and sends display list or scroll
  state updates to the compositor thread.
- `Navigable::paint_next_frame()` asks the compositor thread to present the
  current frame. Child navigables wait for their compositor frame to complete
  before their parent records and presents.
- Async scroll offsets produced by the compositor are adopted back into the DOM
  before rendering-update observers run.

There is already a compositor-specific IPC side channel between LibWebView and
WebContent:

- `Libraries/LibWebView/WebContentClient.cpp` creates a paired compositor
  transport for each `WebContentClient`.
- `Services/WebContent/ConnectionFromClient.cpp` receives the transport through
  `connect_to_compositor()` and starts a `CompositorIPC` thread.
- The UI process sends wheel and mouse events over this channel so async
  scrolling can bypass the WebContent main thread.
- The compositor thread sends backing store allocation and `did_paint`
  notifications back to the UI process through callbacks installed by that IPC
  thread.
- The UI process acknowledges presented bitmap IDs with `ready_to_paint()`, so
  the compositor does not reuse a backing store while the UI may still be
  painting it.

This means the UI already has a useful direct path to "the compositor", but the
endpoint currently lives inside the WebContent process.

## Goals

- Move rasterization, display list replay, async scroll hit testing, viewport
  scrollbar handling, and backing store ownership out of WebContent.
- Keep the UI-to-compositor input path direct. Wheel and compositor-scrollbar
  events should not route through the WebContent main thread.
- Preserve the existing WebContent rendering model: WebContent records display
  lists and scroll state snapshots; the compositor replays them.
- Preserve nested navigable composition for the first implementation.
- Keep CPU painting and GPU painting as backend choices, with CPU painting as
  the initial deterministic bring-up path.
- Make process lifetime and crash behavior explicit.
- Add serialization and validation at the process boundary instead of moving
  C++ object pointers across it.

## Non-goals

- Replacing display lists with a new scene graph.
- Moving DOM, style, layout, JavaScript, image decoding, or network loading to
  the Compositor process.
- Fully solving cross-WebContent iframe composition in the first step. The
  shared Compositor process should make that possible without another process
  model redesign, but the first implementation can still limit which contexts
  are composed together.
- Android service integration. Android is out of scope for this work.
- Defining or tightening OS sandboxing, privilege reduction, or filesystem and
  network policy for the Compositor process.
- Making the compositor IPC protocol stable across builds.
- Compositing browser chrome in the new process.

## Proposed process model

The first implementation should create one Compositor process per Browser/UI
process, shared by all WebContent processes in that browser instance.

This is a larger first step than one Compositor process per WebContent process,
but it gives us the process topology we want long-term:

- Out-of-process iframe composition can be built by connecting more WebContent
  processes to the same compositor, instead of replacing the compositor process
  model later.
- GPU backend state and UI backing store ownership are centralized in one
  process for the browser instance.
- Page presentation becomes independent from any single WebContent process.
- The UI-to-compositor input path remains direct and does not route through a
  WebContent main thread.

This should not be system-wide across independent browser instances. Each
Browser/UI process launches and monitors its own Compositor process.

The first-phase process graph should look like this:

```text
UI / LibWebView  <------- UI compositor IPC ------->  Compositor
      |                                                    ^  ^
      | WebContent IPC                                     |  |
      +----> WebContent / LibWeb ---- compositor IPC ------+  |
      |                                                       |
      | WebContent IPC                                        |
      +----> WebContent / LibWeb ---- compositor IPC ---------+
```

The UI process launches and monitors the Compositor process, just like other
helper processes. Each WebContent process receives its own compositor transport
from the UI process and uses it for display-list, scroll-state, lifecycle, and
screenshot messages. The UI keeps its own compositor transport for input and
presentation.

## Ownership boundaries

### UI process

The UI process owns:

- `ViewImplementation` and platform widget painting.
- Input event collection and routing.
- Shared backing store import and widget invalidation.
- `ready_to_paint()` acknowledgements.
- `PresentationId` allocation and UI-visible presentation registration.
- Presentation visibility and active-presentation updates.
- Compositor process launching, monitoring, and teardown.

The UI process should continue to receive:

- `did_allocate_backing_stores(presentation_id, front_id, front_image, back_id,
  back_image)`
- `did_paint(presentation_id, content_rect, bitmap_id)`

The existing `CompositorServer.ipc` and `CompositorClient.ipc` messages are a
good starting point for this UI-facing protocol, but they should move out of
`Services/WebContent/` when the real server becomes the Compositor process.

### WebContent process

WebContent owns:

- DOM, JavaScript, style, layout, paint tree, and display list recording.
- Display list resource collection.
- The UI-issued presentation binding it was handed during connection brokering.
- Scroll offset adoption into DOM state.
- Promise resolution for async scroll operations.
- Page and navigable lifecycle.

WebContent should not own:

- The Skia display-list player used for page presentation.
- Backing stores presented to the UI process.
- Async wheel hit-test state once that state has been sent to the compositor.
- Viewport scrollbar hover/capture state.

WebContent will still need a small compositor client object with the same shape
as the current `CompositorThread` API, because `HTML::Navigable` calls into
that API directly today.

### Compositor process

The Compositor process owns:

- One or more compositor contexts. A compositor context is the process-local
  replacement for one current `CompositorThread` instance.
- UI-registered presentation mappings from `PresentationId` to a top-level
  compositor context.
- UI-provided presentation visibility and active-presentation state used by the
  scheduler.
- The cached display list for each context.
- The compositor-side `DisplayListResourceStorage`.
- The Skia CPU or GPU backend context.
- The async scroll tree and pending async scroll updates.
- The `BackingStoreManager` for top-level contexts.
- Child-to-parent compositor surfaces for nested navigables.

The first implementation should link the Compositor process against LibWeb and
reuse existing painting and compositor types directly. This keeps the initial
patch stack focused on the process boundary, IPC, serialization, and lifetime
rules. Splitting painting/compositor code into a smaller library can happen
after the remote process is working.

The Compositor process must validate sizes, IDs, enum values, and payload
consistency before replaying or allocating from any serialized display list
data.

## Compositor contexts

The process split should introduce an explicit `CompositorContextId`.

Current code uses one `CompositorThread` object per non-SVG navigable, with the
top-level context registered by page ID. That object identity cannot cross a
process boundary. Instead:

- The Compositor process assigns a `WebContentConnectionId` to each WebContent
  connection and returns it to the UI process during connection brokering.
- WebContent allocates a connection-local `CompositorContextId` for every
  non-SVG navigable, or asks the Compositor process to allocate one.
- The UI process allocates each `PresentationId` and an opaque
  `PresentationCapability` before a top-level context can present to the UI.
- The UI process registers and unregisters the UI-visible presentation mapping
  in the Compositor process. WebContent does not mint UI-visible presentation
  identities.
- The UI process sends the matching `PresentationId` and
  `PresentationCapability` to the owning WebContent process over the normal
  UI-to-WebContent control channel.
- A top-level WebContent context can attach to a UI-visible presentation only by
  sending the UI-issued `PresentationId` and `PresentationCapability` in its
  `PresentToUI` mode.
- Child navigables register contexts without UI presentation.
- A child context can publish into a compositor surface owned by a parent
  context:

```text
PresentationMode =
    PresentToUI {
        presentation_id,
        presentation_capability
    }
  | PublishToCompositorSurface {
        target_context_id,
        surface_id
    }
```

This preserves child-to-parent surface composition while replacing the
in-process `CompositorThread* target` pointer. In the remote path, child frame
completion is an internal scheduler dependency of the parent raster job instead
of something WebContent synchronously waits on before parent presentation.

The exact ID spelling can change during implementation, but the shared process
must not assume that `page_id`, `CompositorSurfaceId`, or resource IDs are
globally unique across WebContent connections unless the protocol makes them
so. Internally, the Compositor process should key state by connection identity
plus context or resource ID.

`page_id` remains useful for callbacks to a specific WebContent connection, but
it must not be used as the UI-facing presentation identity.

## Connection and presentation handshake

The UI process brokers both the WebContent compositor transport and the
presentation binding:

1. UI creates a paired compositor transport for a WebContent process.
2. UI sends the Compositor endpoint to the Compositor process with
   `connect_web_content()`.
3. Compositor accepts the endpoint, creates a `WebContentConnectionId`, stores
   the connection, and returns that ID to UI.
4. UI allocates a `PresentationId` and an unguessable `PresentationCapability`
   for the top-level page/view that this WebContent process may present.
5. UI registers the tuple `(PresentationId, WebContentConnectionId,
   PresentationCapability)` with the Compositor process.
6. UI sends the WebContent endpoint plus `(PresentationId,
   PresentationCapability)` to the WebContent process over the normal
   UI-to-WebContent setup path.
7. WebContent creates its top-level compositor context with
   `PresentToUI { presentation_id, presentation_capability }`.

The Compositor process must reject `PresentToUI` until step 5 has completed,
and WebContent must not invent or reuse a presentation binding that UI did not
send to it. If a tab is moved to another WebContent process, UI unregisters the
old presentation mapping or registers a new tuple for the new
`WebContentConnectionId` before handing the new binding to WebContent.

## IPC shape

The Compositor process needs two logical protocols.

### UI-to-Compositor protocol

This is the current WebContent compositor side channel, but served by the
Compositor process:

```text
connect_web_content(web_content_compositor_transport)
    => web_content_connection_id
register_presentation(presentation_id, web_content_connection_id,
                      presentation_capability)
unregister_presentation(presentation_id)
set_presentation_visibility(presentation_id, is_visible)
set_active_presentation(ui_view_id, optional_presentation_id)

async_scroll_by(presentation_id, position, delta_in_device_pixels) => handled
mouse_event(presentation_id, event) => handled
ready_to_paint(presentation_id, bitmap_id)

did_allocate_backing_stores(presentation_id, front_id, front_image, back_id,
                            back_image)
did_paint(presentation_id, content_rect, bitmap_id)
```

The synchronous `async_scroll_by()` and `mouse_event()` calls are latency
sensitive. They should remain direct UI-to-Compositor calls.

The UI process owns the lifetime of each registered presentation. It should
connect the WebContent process to the Compositor process, receive the assigned
`WebContentConnectionId`, register the presentation, and only then send the
presentation binding to WebContent. It unregisters the presentation when the
tab/view is closed, retargeted, or the owning WebContent connection exits.
Unregistering a presentation drops its backing stores and rejects future UI
input for that `PresentationId`.

The Compositor process should only bind `PresentationId` to a WebContent
context when both sides agree: UI has registered
`(presentation_id, web_content_connection_id, presentation_capability)`, and
that WebContent connection creates or updates a context with
`PresentToUI { presentation_id, presentation_capability }`.

The UI can choose the numeric `PresentationId` from existing LibWebView
view/page state during bring-up, but the ID sent over IPC must be explicitly
registered by UI and globally unique within the browser instance. A
WebContent-local `page_id` is not acceptable as the presentation address.

The UI process also owns presentation scheduling state. It sends
`set_presentation_visibility()` when a tab/view becomes visible or hidden, and
`set_active_presentation()` when a UI view's selected presentation changes.
`ui_view_id` is allocated by UI and only scopes the active-presentation choice;
it is not a WebContent or page identity. Passing no presentation ID clears the
active presentation for that UI view.

### WebContent-to-Compositor protocol

This is new. It replaces direct calls from `HTML::Navigable` to
`CompositorThread`.

Lifecycle messages:

```text
create_context(context_id, page_id, presentation_mode, player_type)
destroy_context(context_id)
set_presentation_mode(context_id, presentation_mode)
stop_presenting_to_client(context_id)
viewport_size_updated(context_id, viewport_size, is_top_level, resize_state)
```

For a top-level context, `presentation_mode` must be
`PresentToUI { presentation_id, presentation_capability }` using values issued
by UI and delivered to this WebContent process during connection brokering.
`page_id` is only used for callbacks to the same WebContent connection.

Rendering messages:

```text
update_display_list(context_id, display_list, resource_transaction, scroll_state)
update_scroll_state(context_id, scroll_state)
present_frame(context_id, viewport_rect) => frame_id
wait_for_frame(context_id, frame_id) => ()
```

Async scrolling messages:

```text
invalidate_wheel_event_listener_state(context_id, generation)
enqueue_async_scroll_by(context_id, expected_document_id, position, delta,
                        viewport_rect, tracking) => accepted, operation_id
should_defer_async_scroll_offset_adoption(context_id) => bool
should_defer_main_thread_present_for_async_scroll(context_id) => bool
take_pending_async_scroll_updates(context_id) => offsets, completed_operation_ids
```

Surface and screenshot messages:

```text
update_compositor_surface(context_id, surface_id, shared_image)
clear_compositor_surface(context_id, surface_id)
request_screenshot(context_id, request_id, size)
```

Video messages:

```text
update_yuv_video_frame(context_id, source_id, frame)
clear_video_frame(context_id, source_id)
```

These messages update the current frame for a compositor-side
`VideoFrameSource`. They are intentionally separate from display list updates:
the display list owns where and how the video source is drawn, while the video
message owns the source's current decoded frame.

Compositor-to-WebContent callbacks:

```text
schedule_rendering_update(page_id)
did_finish_screenshot(request_id, shared_image)
did_fail_screenshot(request_id)
```

The WebContent-facing `page_id` is scoped to the WebContent connection that
receives the callback.

Screenshot requests are completed only through the
`did_finish_screenshot()`/`did_fail_screenshot()` callbacks. The `request_id`
correlates the callback with the original `request_screenshot()` message.

The compositor should never make a blocking call into WebContent. If it needs
WebContent to adopt scroll offsets or record a fresh display list, it sends an
async notification and WebContent schedules its normal rendering update.

## Display list and resource transfer

The main implementation work is making display lists and resources explicitly
serializable.

The current in-process resource transaction contains C++ objects:

- `NonnullRefPtr<Gfx::Font const>`
- `Gfx::DecodedImageFrame`
- `NonnullRefPtr<VideoFrameSource const>`
- `NonnullRefPtr<DisplayList const>`

Those objects cannot be sent to another process as-is. The remote protocol
should preserve the existing resource IDs but replace each object with a
transport payload.

Suggested wire fields and payloads:

```text
DisplayList IPC fields {
    id
    payload_buffer
    command_bytes_offset
    command_bytes_size
    accumulated_visual_context_tree_offset
    accumulated_visual_context_tree_size
    async_scrolling_metadata
}

SerializedAccumulatedVisualContextTree {
    nodes
}

SerializedVisualContextNode {
    parent_index
    data
}

SerializedVisualContextData =
    Scroll {
        scroll_frame_index
        is_sticky
    }
  | Clip {
        rect
        corner_radii
    }
  | Transform {
        matrix
        origin
    }
  | Perspective {
        matrix
    }
  | ClipPath {
        path
        bounding_rect
        fill_rule
    }
  | Effects {
        opacity
        blend_mode
        serialized_filter
    }
  | ScrollCompensation {
        scroll_frame_index
    }

SerializedFontResource {
    font_id
    font_data_buffer
    font_data_size
    ttc_index
    point_width
    point_height
    variations
    shape_features
}

SerializedImageFrameResource {
    image_frame_id
    shared_image
    color_space
}

SerializedVideoFrameSourceResource {
    source_id
}

SerializedYUVVideoFrame {
    source_id
    frame_sequence_id
    timestamp
    duration
    size
    bit_depth
    subsampling
    cicp
    color_space
    y_plane_buffer
    y_plane_size
    u_plane_buffer
    u_plane_size
    v_plane_buffer
    v_plane_size
}
```

For fonts, send exact font bytes through shared memory rather than asking the
Compositor process to find the same system or web font by family name. Display
list glyph IDs are only meaningful for the exact typeface used when recording.
Sending font bytes also avoids duplicating WebContent's font lookup and
selection behavior in the Compositor process.

The serialized font resource should include the shared font-data buffer, the
valid byte length within that buffer, the TTC index when applicable, and the
metadata needed to reconstruct the `Gfx::Font` used by the display list. The
receiving resource store should insert the reconstructed font under the
transmitted `FontResourceId` instead of using the process-local `Gfx::Font::id()`.
We should deduplicate shared font-data buffers within a transaction when
multiple `Gfx::Font` resources use the same typeface at different sizes.

For images, use `Gfx::SharedImage` or `Gfx::ShareableBitmap` and preserve the
original `ImageFrameResourceId`. The receiving resource store should insert the
decoded frame under the transmitted ID instead of using the process-local
`Gfx::DecodedImageFrame::id()`.

For nested display lists, encode the underlying `DisplayList` directly and send
it as a resource in the same transaction. The important invariant is that
command bytes never reference a resource ID that is missing from the
compositor-side storage after applying the transaction.

The `AccumulatedVisualContextTree` should be serialized as an ordered vector of
nodes into the same display-list payload buffer that carries the display list
command bytes. The two sections are addressed by explicit offsets and sizes.
The tree section excludes the implicit sentinel at `VisualContextIndex { 0 }`;
the receiver reconstructs the tree by calling
`AccumulatedVisualContextTree::create()` and appending each serialized node in
order. That preserves the exact `VisualContextIndex` values used by display list
command headers and async scrolling metadata: serialized node array index
`N - 1` reconstructs tree index `N`.

Only `parent_index` and `VisualContextData` should cross IPC. Do not serialize
`depth` or `has_empty_effective_clip`; both are derived by
`AccumulatedVisualContextTree::append()`. Recomputing them on the receiving side
keeps the cached tree metadata consistent with the node data that will actually
be replayed.

The visual context tree section and command byte section must be decoded and
applied atomically from the same shared payload buffer. If either section is out
of bounds, if the tree fails to decode, or if any command header references a
`VisualContextIndex` outside the reconstructed tree, reject the whole display
list update.

`EffectsData::gfx_filter` should use the existing `Gfx::serialize_filter()`
format, with decoded image frames inside filter-image operations encoded as
image frame resource IDs from the same resource transaction. The Compositor
process should deserialize those filters only after the referenced image
resources are present in compositor-side storage.

Matrix, point, rect, corner radii, path, winding-rule, blend-mode, and filter
fields should use explicit IPC coders instead of copying raw object memory.
Large path or filter payloads should be byte-size capped before being placed in
the shared display-list payload buffer.

For video, keep `VideoFrameSource` as a compositor-side resource and update it
with a dedicated IPC message carrying YUV plane data. Display lists continue to
reference the stable `VideoFrameResourceId`; new video frames do not require a
new display list unless geometry or paint state changes.

The first version should send packed Y, U, and V planes through shared memory,
together with the metadata currently needed to reconstruct `Media::VideoFrame`
and `Gfx::YUVData`: frame size, bit depth, subsampling, CICP values, color
space, timestamp, and duration. The Compositor process can then keep using
`DisplayListPlayerSkia::draw_video_frame_source()` and let Skia perform YUV
texture upload/conversion when a GPU backend is available, with the existing
CPU YUV-to-RGB fallback.

The update message should include a monotonically increasing frame sequence ID
per source. The Compositor process should drop stale updates for unknown,
removed, or newer sources.

Every serialized display list should use one shared-memory payload buffer from
the first remote implementation. The payload contains both
`DisplayList::command_bytes()` and the serialized
`AccumulatedVisualContextTree`, addressed by offsets and explicit byte lengths.
This avoids IPC message size limits, removes a size-threshold policy from the
protocol, keeps small and large display lists on the same path, and makes the
command stream and visual context tree one atomic update. Control metadata and
small resource transaction records can still travel in normal IPC messages.

The Compositor process must validate section offsets and declared byte lengths
against the shared payload buffer size before decoding the visual context tree
or reading command headers.

## Threading model inside the Compositor process

The Compositor process does not need to keep the exact "thread" abstraction.
It can run the compositor command queue on the process main event loop.

However, the current `CompositorThread::ThreadData` already has useful
separation between the public API and the compositor loop. The low-risk path is:

1. Extract the reusable compositor engine from `CompositorThread::ThreadData`.
2. Keep the in-process `CompositorThread` wrapper while introducing the remote
   wrapper.
3. Let the Compositor service own the engine directly.

This allows an A/B mode during bring-up:

- in-process compositor thread, current behavior
- remote compositor process, same public `HTML::Navigable` facing API

## Compositor process loop and scheduling

The Compositor process has to accept work from several producers:

- UI input and presentation messages.
- WebContent display list, scroll state, resource, and lifecycle messages.
- Video frame updates.
- Screenshot requests.
- Child compositor surface updates.

The first implementation should not replay display lists inline from IPC
handlers. IPC handlers should validate payloads, update compositor-side state,
mark contexts dirty, and enqueue or coalesce raster work. A scheduler tick then
chooses the next raster job.

Initial scheduling can stay single-threaded and single-raster:

- The IPC event loop owns all compositor context and resource state.
- At most one raster job is active at a time.
- Raster jobs run to completion on the compositor loop during the first
  implementation.
- Parallel raster workers are out of scope until the remote process is working.

This matches the expected first use case. A Browser window normally has one
visible tab presentation. Hidden tabs may keep sending display list and resource
updates, but those updates should be cached and coalesced instead of replayed
into backing stores immediately. The visible top-level presentation, explicit
screenshot requests, and any child surfaces needed by those jobs are the only
things that need rasterization in the normal path.

The scheduler should track at least:

- The active presentation ID for each UI view.
- Whether a top-level context has a pending present request.
- Whether UI has marked the presentation visible.
- Whether UI has selected the presentation as active for a UI view.
- Whether a context has dirty display list, scroll, surface, image, font, or
  video state.
- Whether a backing store is available, or presentation is blocked waiting for
  `ready_to_paint()`.
- Pending screenshot requests.
- Pending frame tokens that WebContent is waiting on.

Suggested priority order:

1. Finish a raster job already in progress.
2. Rasterize an active visible presentation dirtied by UI async scrolling, if a
   backing store is available.
3. Rasterize an explicit `present_frame()` request for the active visible
   presentation.
4. Rasterize screenshot requests.
5. Defer hidden or inactive tab presentation work until the tab becomes visible
   or active.

Display list and resource updates from hidden tabs should still be applied to
the compositor-side cache immediately. The expensive part to avoid is replaying
those display lists into UI backing stores that cannot be shown.

When a hidden tab becomes visible, the UI process sends
`set_presentation_visibility(presentation_id, true)` and, if selected in a UI
view, `set_active_presentation(ui_view_id, presentation_id)`. The scheduler then
treats the latest cached display list and scroll state for that presentation as
dirty and records the first visible frame from the current compositor-side
state.

Child contexts should be treated as dependencies of the top-level raster job.
If a visible parent references a dirty child compositor surface, the scheduler
should update that child surface before replaying the parent display list. The
first implementation can do this serially inside the same raster job.

WebContent should not synchronously wait for child context frame completion
before presenting the parent. A child context can enqueue or mark its compositor
surface update as pending; when the parent raster job runs, the scheduler
updates every dirty child surface needed by the parent display list, then replays
the parent. This keeps parent-child ordering inside one compositor scheduler
instead of splitting it between WebContent and the Compositor process.

`present_frame()` should return or complete using a frame token, not by
forcing immediate display list replay inside the IPC handler. `wait_for_frame()`
then waits for the scheduler to complete the matching raster job. It should not
be used by WebContent to manually order child completion before parent
presentation.

## Async scrolling flow

The target async scroll path is:

1. UI receives a wheel event.
2. UI sends `async_scroll_by(presentation_id, position, delta)` directly to the
   Compositor process.
3. Compositor hit-tests against its cached async scroll tree.
4. If accepted, Compositor mutates compositor-side scroll offsets, presents a
   frame if possible, and sends `schedule_rendering_update(page_id)` to
   WebContent.
5. WebContent runs the normal rendering update and calls
   `take_pending_async_scroll_updates()`.
6. WebContent adopts the pending deltas into DOM scroll state and resolves
   completed async scroll operation promises.
7. WebContent records fresh display list or scroll state and sends it back to
   the Compositor process.

This preserves the current invariant: rendering-update observers see the scroll
positions already presented to the user.

The current `should_defer_*` queries become IPC. If they are too expensive in
practice, replace the repeated synchronous queries with one cached
`CompositorRenderState` response per rendering update.

## Backing stores and presentation

Top-level compositor contexts keep the current double-buffered backing store
model:

- Compositor allocates front and back stores through `BackingStoreManager`.
- Compositor sends both shared images and bitmap IDs to UI.
- Compositor replays display lists into the back store, swaps, and sends
  `did_paint(presentation_id, content_rect, bitmap_id)`.
- UI swaps its local front/back bitmap metadata and invalidates the widget.
- UI sends `ready_to_paint(presentation_id, bitmap_id)` once it no longer needs
  the presented bitmap.

This should remain unchanged from the UI process perspective. The server just
moves from WebContent to the Compositor process.

Child compositor contexts should not allocate UI-visible backing stores. They
render into process-local surfaces and publish snapshots into the parent
context's compositor surface table.

## Screenshots and headless mode

The current screenshot path passes a `Gfx::PaintingSurface` pointer to the
in-process compositor and invokes a callback on completion. That cannot cross a
process boundary.

The remote path should complete through a callback carrying a shareable image:

1. WebContent ensures the display list and scroll state are current.
2. WebContent asks the Compositor process to render a screenshot for a context.
3. Compositor creates a temporary surface, replays the display list, flushes it,
   and sends `did_finish_screenshot()` with a `Gfx::SharedImage` or
   `Gfx::ShareableBitmap`.
4. WebContent converts that callback payload into the existing page screenshot
   response.

Headless mode should initially force CPU painting for determinism, matching the
current test-mode behavior.

## Crash and lifecycle policy

First implementation:

- If a WebContent process exits, the UI process tells the Compositor process to
  drop every context, resource, and presentation mapping owned by that
  WebContent connection.
- If the shared Compositor process exits, the browser instance should not try
  to continue. Treat this as a fatal rendering-process failure and shut down
  the attached UI/browser instance instead of attempting recovery.
- If UI exits, both helper processes exit through the normal process teardown.

Independent Compositor restart is out of scope for the first implementation.
We can revisit it later, but the initial design should keep crash handling
simple and avoid trying to present stale backing stores.

The first implementation should not silently fall back to routing compositor
input through WebContent. That would hide bugs in the new process path and
reintroduce jank in exactly the cases this change is meant to improve.

## IPC validation

The Compositor process consumes serialized data produced by WebContent. Even
with sandboxing out of scope, the IPC boundary must reject malformed data before
replaying display lists or allocating compositor resources.

Required checks:

- Reject unknown display list command types.
- Reject payload sizes that overflow, exceed configured limits, or do not match
  the command type.
- Reject resource IDs referenced by command bytes that are not present after a
  transaction.
- Cap backing store allocation sizes.
- Cap shared image dimensions and reject invalid formats.
- Validate shared font-data buffer sizes before constructing typefaces.
- Validate YUV frame plane sizes against frame size, bit depth, and subsampling
  before constructing `Gfx::YUVData`.
- Validate visual context node count, node ordering, and parent indexes before
  reconstructing `AccumulatedVisualContextTree`.
- Validate every display list command header `VisualContextIndex` against the
  reconstructed visual context tree.
- Validate visual context references in async scrolling metadata against the
  reconstructed visual context tree.
- Validate visual context variant tags, enum values, finite matrix/filter
  numbers, and non-negative rect dimensions.
- Validate visual context path/filter payload sizes and image resource
  references before deserializing `Gfx::Filter`.
- Reject `PresentToUI` context creation or presentation-mode updates unless
  they match a UI-registered `(PresentationId, WebContentConnectionId,
  PresentationCapability)` tuple.
- Reject `register_presentation()` for unknown or closed
  `WebContentConnectionId` values.
- Reject visibility and active-presentation updates that reference an
  unregistered `PresentationId`.
- Account memory and resource usage per WebContent connection, so one buggy
  WebContent process cannot consume the whole shared compositor.
- Validate scroll frame indexes before using them for async hit testing.

## Testing strategy

Acceptance coverage should include focused serialization and scheduler coverage:

- Display list serialization preserves command bytes and async scrolling
  metadata.
- Display list command bytes and accumulated visual context trees are
  transferred through the same shared-memory payload for every display list, not
  only above a size threshold.
- Font bytes are transferred through shared memory and reconstructed under the
  transmitted `FontResourceId`.
- Accumulated visual context trees round-trip while preserving
  `VisualContextIndex` values, node variant data, and derived empty-clip state.
- Accumulated visual context decoding rejects invalid parents, cycles,
  out-of-range command context indexes, invalid async metadata references, and
  filter image references missing from the resource transaction.
- Presentation registration tests reject unregistered presentation IDs,
  mismatched capabilities, wrong WebContent connections, and `page_id`
  collisions across WebContent processes.
- Connection brokering tests verify that UI receives the assigned
  `WebContentConnectionId`, registers the presentation against that ID, and
  forwards the matching capability to WebContent before `PresentToUI`.
- Presentation scheduling tests verify that UI visibility and active-presentation
  updates control which dirty presentations are rasterized.
- YUV video frame updates reconstruct `Media::VideoFrame` under the transmitted
  `VideoFrameResourceId` without requiring display list re-recording.
- Scheduler tests coalesce hidden tab updates and rasterize only the active
  presentation unless a screenshot is requested.
- Scheduler tests update dirty child compositor surfaces before replaying a
  parent display list that references them.
- Resource transactions preserve resource IDs and removal semantics.
- Font resources replay text with the same typeface and glyph IDs.
- Image resources survive shared-image transfer and removal.
- Scroll state snapshots round-trip exactly.

End-to-end coverage should include:

- Basic page load and repaint.
- Tab visibility and active-presentation switching.
- Window resize and backing store reallocation.
- Async wheel scrolling.
- Viewport scrollbar hover and drag.
- Nested iframes in the same WebContent process.
- Multiple WebContent processes connected to one shared Compositor process.
- Canvas compositor surfaces.
- Pages with web fonts.
- Large images and image removal.
- Video playback through YUV frame updates.
- Full-page and DOM-node screenshots.
- WebContent crash and Compositor crash behavior.

For bring-up, add a runtime switch so the same tests can compare in-process and
remote compositor behavior.

## Concrete commit series

The patch stack should keep the existing in-process compositor as the only
runtime path for as long as possible. Preparatory commits should be small,
reviewable, and able to pass the normal test suite independently. The first
commit that actually routes page rendering through a separate Compositor
process should come after the interfaces, scheduler, serialization, validation,
and disabled process plumbing are already landed.

### In-process preparation

1. Move the existing compositor UI IPC endpoint names out of WebContent-specific
   terminology, without changing behavior.
2. Introduce a `PageCompositor` interface with the current `CompositorThread`
   API shape, and keep `CompositorThread` as the only implementation.
3. Change `HTML::Navigable` to own the `PageCompositor` interface instead of a
   concrete `CompositorThread`, without changing call ordering.
4. Introduce explicit `CompositorContextId`, `PresentationId`,
   `PresentationCapability`, and `PresentationMode` types in LibWeb, while still
   resolving them to the current in-process compositor objects.
5. Replace child compositor target pointers in the public compositor API with
   `PresentationMode::PublishToCompositorSurface` data, while keeping the
   in-process implementation underneath.
6. Extract the reusable compositor engine out of
   `CompositorThread::ThreadData`, leaving `CompositorThread` as a wrapper.
7. Add an in-process compositor scheduler class with a single active raster job,
   and make the existing compositor path submit raster work through it.
8. Add active-presentation and visibility tracking to the scheduler, but keep
   current visible tab behavior unchanged.
9. Implement scheduler coalescing for hidden presentation updates, backing store
   blocking on `ready_to_paint()`, and screenshot priority.

### Serialization and validation

10. Add a reusable shared-memory buffer transport type for serialized payloads.
11. Add `AccumulatedVisualContextTree` serialization for scroll, clip,
    transform, perspective, clip-path, effects, and scroll-compensation nodes.
12. Add `ScrollStateSnapshot` and `AsyncScrollOffset` serialization.
13. Add `DisplayList` serialization and deserialization using one shared-memory
    payload for command bytes and visual context tree data, without changing
    production call paths.
14. Add `DisplayListResourceTransaction` serialization for resource add/remove
    operations, preserving transmitted resource IDs.
15. Add font resource serialization using shared font-data buffers, TTC indexes,
    font size metadata, variations, and shape features.
16. Add image frame resource serialization using shared images or shareable
    bitmaps.
17. Add video source registration payloads and YUV video frame update payloads
    with plane-size validation.
18. Add validation for malformed command sizes, missing resource IDs, invalid
    visual context parents, invalid scroll frame indexes, missing filter image
    resources, and backing stores over the size cap.
19. Add an optional in-process serialize/deserialize test mode for display list
    updates, disabled by default, so the remote payload format can be exercised
    before the remote process exists.

### Disabled process plumbing

20. Add the `Services/Compositor` target and `ProcessType::Compositor`, with a
    process that starts, accepts a ping, and exits cleanly.
21. Add `launch_compositor_process()` in LibWebView, but keep it unused by the
    default browser path.
22. Add the UI-to-Compositor IPC shell with presentation-management messages.
23. Add the WebContent-to-Compositor IPC protocol with context lifecycle message
    definitions, but no rendering routed through it.
24. Add UI-side process monitoring for Compositor crash.
25. Add connection brokering so UI can receive each assigned
    `WebContentConnectionId`, register presentation bindings against it, and
    hand each WebContent process its Compositor transport plus presentation
    capability.
26. Add a stub Compositor service connection table keyed by WebContent
    connection and context ID.

### Remote implementation behind a runtime switch

27. Add `RemotePageCompositor` in WebContent with context create/destroy and
    viewport-size messages.
28. Implement remote presentation mode updates for top-level and child contexts,
    including validation against UI-registered presentation mappings.
29. Send serialized display list and resource transactions to the Compositor
    process and store them in the remote context cache.
30. Route `present_frame()` through scheduler frame tokens in the Compositor
    process.
31. Implement CPU rasterization in the Compositor process and send existing
    backing store callbacks to the UI process.
32. Implement child compositor surface updates as dependencies of the parent
    raster job.
33. Implement remote screenshot requests.
34. Implement YUV video frame updates.
35. Route UI async scroll and compositor-scrollbar input directly to the
    Compositor process.
36. Drain pending async scroll updates from WebContent through the remote
    compositor client.
37. Add browser tests covering the end-to-end cases listed above.

### Switch-over and cleanup

38. Enable the remote Compositor process by default for the target desktop
    configuration.
39. Remove the old WebContent-local compositor UI IPC server.
40. Remove the in-process compositor runtime switch after the remote path is
    stable enough that independent fallback no longer adds value.

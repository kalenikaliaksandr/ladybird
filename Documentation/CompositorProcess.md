# Compositor process migration

## Overview

This document describes a behavior-preserving migration from the current
WebContent-owned compositor thread to a separate Compositor process.

Today, each WebContent process owns compositor state through
`Web::Compositor::CompositorThread`. Recent in-process compositor IPC work
already makes the WebContent-to-compositor boundary explicit:
`WebContentCompositorServer.ipc` carries serialized display lists, resource
transactions, scroll snapshots, video frames, compositor surfaces, viewport
updates, frame presentation requests, and screenshot requests. That structure is
the proof point for the process split. The migration should keep that explicit
protocol boundary, move the compositor actor into a service process, and remove
`CompositorThread` once the compositor owns its own event loop.

The target topology has one Compositor process per UI process. The UI process
spawns the Compositor process, keeps a persistent control connection to it, and
passes compositor transport handles to WebContent processes. Each WebContent
process has one connection to the Compositor process. UI allocates compositor
context IDs per navigable, so the Compositor can build a compositor-visible
surface tree keyed by stable UI-owned IDs instead of DOM objects or live
WebContent pointers.

The first process split is intentionally behavior-preserving. It should not add
new security policy, compositor restart, or GPU transport work. The UI process
remains the source of truth for native windows, focus, activation, scale factor,
visibility, occlusion, and platform event delivery. The Compositor process keeps
compositor-visible copies of viewport size, scale factor, visibility, and
occlusion state, and uses existing visibility plumbing such as
`set_system_visibility_state` instead of introducing a parallel visibility API.

The UI process should not compose web content after the switch. Native windows,
menus, chrome, and platform event sources remain in UI; web content composition,
async scrolling, backing-store ownership, and presentation move to Compositor.
`Gfx::ShareableBitmap` remains the initial behavior-preserving frame transport.
GPU/shared-image/IOSurface paths are future optimization work and should not be
required for the isolation milestone.

### Connections

The UI process owns the Compositor process lifetime for this migration. It opens
a persistent UI-to-Compositor control connection, asks the Compositor to create
per-navigable compositor contexts, and passes WebContent-facing compositor
transport handles to each WebContent process. WebContent should not spawn or
discover the Compositor process directly.

Each WebContent process gets one WebContent-to-Compositor connection. That
connection carries all compositor updates for navigables hosted by that
WebContent process. It sends serialized display lists, resource transactions,
scroll snapshots, frame requests, video frame updates, compositor surface
updates, and screenshot requests. Callbacks from Compositor to WebContent keep
the existing shape: request a rendering update, report screenshot completion,
and report compositor loss.

The UI control connection carries native-window state, compositor context
creation and destruction, WebContent transport brokering, mouse and wheel input,
presentation acknowledgements, cursor changes, and compositor-to-UI presentation
notifications.

### Input routing

Mouse and wheel input should flow UI -> Compositor -> WebContent. UI still
decides native focus and activation before sending the input to Compositor.
Compositor uses its async scrolling state and compositor-visible surface tree to
consume scrollable input when possible. Only unconsumed mouse and wheel events
are forwarded to WebContent.

Within this migration, WebContent continues to own pointer capture, mouse
capture, hover semantics, drag state, and DOM cursor computation. Compositor
mediates routing and async scroll consumption, but it does not become the owner
of DOM input semantics. Keyboard input can move through Compositor later, but is
out of scope for the behavior-preserving split.

### Rendering data and resources

WebContent must send serialized display lists to Compositor immediately. The
process switch should not preserve a non-serialized display-list path.
Display-list replay in Compositor must not depend on live WebContent objects.
Display lists, referenced resources, scroll state, compositor surfaces, and
enough visual state to replay a frame belong on the protocol boundary.

All display-list resources referenced across the boundary need transaction
plumbing before the process switch. Fonts, images, video frames, canvas
surfaces, and decoded frame data should be represented by stable resource IDs or
explicit transferable payloads. `DisplayListResourceStorage` becomes
Compositor-owned for replay. WebContent sends transactions that update the
Compositor-owned storage, then the Compositor retains resources according to the
committed frames and presentation acknowledgement lifecycle.

Resource ownership validation between WebContent connections is not part of
this migration. Protocol violations should still fail fast, while expected
teardown and stale callbacks should close or clean up the affected connection or
context without adding recovery policy.

### Frames and scheduling

WebContent still requests frames. Compositor owns compositor-side scheduling,
presentation, async scrolling, and back-pressure. Frame acknowledgements still
flow back to WebContent, but they represent presentation and resource-lifetime
state rather than a nested paint-order primitive.

Back-pressure is represented as one pending committed frame per
navigable/context. If WebContent commits faster than Compositor can present, the
Compositor may replace older unpresented state where safe. Resource lifetime
must remain tied to acknowledged commits so that coalescing display-list updates
does not release still-visible resources too early.

No explicit frame tokens are required for the migration. Current rendering order
can remain driven by `HTML::EventLoop::update_the_rendering()`, which paints
documents in the required order. Nested navigables in the same page share one
compositor context tree. Cross-process iframes contribute independent surfaces
to the same compositor scene, and the shared Compositor process merges surfaces
from multiple WebContent processes into the final web-content composition.

### Failure handling

Restarting the Compositor process is out of scope for the initial isolation
work. If the Compositor process exits, UI and WebContent should treat that as a
fatal compositor loss for this stage. If a WebContent compositor connection dies,
Compositor destroys all contexts owned by that connection. Malformed compositor
IPC should fail fast as a protocol violation; normal teardown, stale frame
acknowledgements, and context destruction should remain non-fatal lifecycle
cases.

### Rollout policy

The new path should initially be hidden behind a feature flag. The feature flag
can become the default only after the full browser path works and the whole
`test-web` suite has zero regressions. Each commit in the migration sequence
should build and pass `test-web` so reviewers can reason about the stack at any
point.

## Commit sequence

### 1. Add compositor process protocol scaffolding

Introduce Compositor service protocol files without changing runtime behavior.
Split the existing in-process protocol concepts into the two real process
relationships:

- UI-to-Compositor control protocol.
- WebContent-to-Compositor rendering protocol.
- Compositor-to-WebContent callback protocol.

This commit should mostly move names and protocol declarations. It should keep
the current WebContent-hosted compositor path active, so the build and
`test-web` continue to exercise the existing behavior.

The protocol should make the ownership model visible: UI creates and destroys
compositor contexts, WebContent publishes rendering state for those contexts,
and Compositor reports presentation and rendering-update callbacks. Context IDs
are UI-allocated and per navigable.

### 2. Replace compositor thread scheduling with an event-loop model

Refactor compositor scheduling so the compositor core is driven by a
`Core::EventLoop` instead of a blocking condition-variable thread loop. This is
still in-process and behavior-preserving, but it removes the largest mismatch
between `CompositorThread` and the future Compositor process.

The resulting compositor core should receive commands on an event loop, process
display-list updates, resource transactions, scroll-state updates, async-scroll
requests, presentation requests, screenshots, and presentation acknowledgements
on that loop, and publish callbacks through explicit clients. After this commit,
`CompositorThread::ThreadData` should no longer be the long-term abstraction;
the state should be process-ready compositor state with event-loop scheduling
around it.

### 3. Move context allocation to UI

Move compositor context allocation out of WebContent and into UI. Each
navigable receives a UI-allocated `CompositorContextId` before WebContent
creates the corresponding compositor context. Page-presenting contexts and
nested navigable contexts should use the same allocation path so the Compositor
can maintain one compositor-visible surface tree.

This commit should preserve the existing rendering path by passing the
UI-allocated IDs into the current in-process compositor host. It should remove
the assumption that `allocate_compositor_context_id()` can be called inside
WebContent for the final architecture.

### 4. Introduce the Compositor process behind a feature flag

Add the Compositor service executable and have UI spawn one Compositor process
when the feature flag is enabled. UI opens its persistent control connection and
keeps the existing in-process compositor path when the flag is disabled.

At this stage the Compositor process may be mostly idle. The point of the
commit is process lifetime, service startup, feature-flag plumbing, and a stable
UI control connection. Compositor restart is deliberately not implemented.

### 5. Broker WebContent compositor transports through UI

Teach UI to create or request a WebContent-facing compositor transport from the
Compositor process and pass that transport handle to each WebContent process.
WebContent should not discover or spawn Compositor directly. Each WebContent
process receives exactly one compositor connection and uses it for all
navigables it hosts.

This commit should keep display-list submission behavior unchanged when the
feature flag is disabled. With the flag enabled, WebContent should establish the
new WebContent-to-Compositor connection successfully, but the runtime switch can
still defer real rendering traffic until the following commits.

### 6. Move display-list and resource submission to Compositor

Route WebContent display-list updates through the WebContent-to-Compositor
connection. The message payloads should remain serialized at the IPC boundary:
display lists, display-list resource transactions, scroll snapshots, video
frames, compositor surfaces, screenshot requests, and related compositor values
must cross as protocol data rather than live WebContent objects.

Move `DisplayListResourceStorage` ownership to Compositor for replay. WebContent
continues to collect referenced resources and send transactions, but Compositor
applies the transactions, retains resources needed by committed frames, and
uses `Gfx::ShareableBitmap` as the initial presentation transport. GPU-backed
surface sharing remains future work.

### 7. Move frame presentation and back-pressure to Compositor

Route `present_frame`, presentation acknowledgement, screenshot completion, and
rendering-update callbacks through the Compositor process. WebContent still
requests frames, while Compositor owns presentation scheduling and
back-pressure.

Represent back-pressure as one pending committed frame per context. When a
newer commit replaces older unpresented state, Compositor must keep resource
lifetimes correct until the visible frame has been acknowledged. Frame
acknowledgements continue to flow back to WebContent, but no new explicit frame
token is introduced for nested paint ordering.

### 8. Build the compositor-visible surface tree

Add the Compositor-owned surface tree keyed by UI-allocated navigable/context
IDs. Same-page nested navigables contribute to one tree. Cross-process iframes
publish independent surfaces that Compositor can merge into the same final
scene. The tree should describe compositor surfaces and ordering, not DOM
objects.

Current document paint ordering remains driven by WebContent rendering order.
The Compositor surface tree records enough state to compose the submitted
surfaces without reaching back into WebContent.

### 9. Move async scrolling to the Compositor process

Move async scrolling state, pending async scroll offsets, wheel listener
generation invalidation, and compositor-side scroll hit testing into the
Compositor process. WebContent continues to publish scroll snapshots and adopt
pending async scroll updates during rendering, but the UI no longer sends wheel
input directly to WebContent first.

This commit should preserve current async-scrolling behavior: Compositor
consumes wheel input when it can scroll a compositor-known target, reports
whether the event was consumed, and leaves main-thread scroll handling to
WebContent for unconsumed cases.

### 10. Route mouse and wheel input through Compositor

Change UI input routing so mouse and wheel events go to Compositor first after
UI accepts native focus and activation. Compositor handles async scroll and
compositor-owned hit testing, then forwards only unconsumed events to
WebContent. Pointer capture, mouse capture, hover semantics, drag state, and DOM
cursor computation remain WebContent-owned in this migration.

Cursor changes are computed by WebContent and delivered back to UI through the
Compositor-mediated path. Keyboard input remains UI-to-WebContent until a later
keyboard-routing project.

### 11. Mirror native window state into Compositor

Mirror viewport size, scale factor, visibility, occlusion, and resize state
from UI into Compositor. UI remains the source of truth for native state, and
Compositor stores the copies needed for composition and input routing. Existing
visibility plumbing such as `set_system_visibility_state` should feed this
state instead of creating a second visibility model.

This commit should make hidden, resized, occluded, and scale-factor-changed
windows behave the same as they did before the process split.

### 12. Switch the feature flag to the Compositor process path

Enable the Compositor process path under the feature flag for normal browser
use. The in-process path still exists as fallback. At this point UI no longer
composes web content when the flag is enabled: WebContent sends rendering state,
Compositor composes and presents, and UI only owns native window integration and
platform event sources.

The flag should not become the default until the full `test-web` suite has zero
regressions and manual browser smoke testing covers painting, resizing, nested
navigables, async scrolling, mouse routing, screenshots, video frames, canvas,
and compositor surface publication.

### 13. Remove the WebContent-owned compositor thread

After the feature-flagged Compositor process path is stable, delete the
WebContent-owned compositor implementation and the `CompositorThread`
abstraction. Remove the in-process compositor actor thread, the direct
WebContent compositor host, and any now-unused IPC compatibility paths that
existed only to host compositor state inside WebContent.

The remaining shape should be direct: UI owns Compositor process lifetime and
context allocation, WebContent owns DOM/layout/paint recording and frame
requests, and Compositor owns compositor state, resource replay storage,
surface-tree composition, async scrolling, and presentation.

### 14. Make the Compositor process path the default

Once the feature-flagged path is stable and the whole `test-web` suite has zero
regressions, make the Compositor process path the default. Keep the flag only if
it is still useful for bisecting or temporary fallback. Otherwise remove the old
runtime switch in a follow-up cleanup.

This is the first commit where the process-isolated compositor becomes the
normal runtime architecture.

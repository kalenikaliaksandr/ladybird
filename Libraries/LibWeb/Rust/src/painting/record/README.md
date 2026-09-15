# Packed paint recording

CSS ordering is defined by `../paint_order_plan.rs`. `program.rs` compiles those
ordering decisions into a flat operation array and a flat table of scope
intervals. A producer is a paint occurrence, not a DOM node: one owner can appear
in several phases, and positioned descendants can appear outside its scope.
Structurally empty helper descents are omitted. Scope boundaries add no rendering
or isolation semantics; transforms, clips and effects belong to the visual-context
tree.

The published cache keeps only the latest frame. Its command bytes and run table
are shared with the C++ display list through immutable `Arc` storage. Hit items
are owned by one frame-wide vector. The frame also retains a complete directory
of output boundaries, availability bits, input snapshots indexed by owner, and
sparse lists of producers with live dependencies or blocking wheel regions.
There are no command or hit vectors, resource maps, or reference-counted payload
objects per producer.

Each producer starts at a closed command-group boundary with an explicit visual
context and empty ambient recording state. It writes directly into the destination
frame. SVG and mask work can remain opaque inside that boundary. Resources are
registered in one recording manifest, then resolved and retained through the
existing frame resource-set publication path.

With unchanged topology, geometry and frame inputs, a bounded owner queue and the
program's packed reverse index identify dirty occurrences. Descendant invalidation
also schedules SVG and scrolling producers that read descendant inputs. Consecutive
clean operations are copied in one run, without validating each clean scope. Unknown
ownership, subtree dependencies and eligibility transitions use the validated walk.
Live producers such as scroll-dependent backgrounds are scheduled independently.

For reuse, an operation or scope must map to a complete interval in the previous
frame. The source directory stays immutable while the destination directory is
written in output order. Copied intervals get new endpoints immediately. Changes
in output size never modify old row addresses or require historical-parent
recovery. A skipped operation has no reusable output, even if an older frame had
painted it. Read-only recordings publish neither the new directory nor a new
cache generation.

Content invalidation uses source-relative row stamps. `topology.rs` accumulates
ordering invalidations against the last published program. Its scope keys describe
actual paint occurrences, including content painted outside its layout parent's
scope. Repeated changes coalesce until a recording publishes its replacement.
Compilation propagates the pending changes through the source program's scope
ancestry, rebuilds affected plans from the final committed state and copies clean
intervals. Subtree changes additionally invalidate nested plans whose participation
can depend on the changed ancestor. The flat arrays and reverse index are rebuilt;
there are no retained command buffers or ordering vectors per scope.

Compilation records source mappings as coalesced intervals, with gaps for new
occurrences. It does not build a source-index table or run-start table per operation.
Copied operations remap their owner through a temporary dense array; the inverse
mapping also carries retained owner inputs into the new frame. These mappings are
discarded after recording and add no per-owner state to the retained program.

Row retirement resolves its old occurrences before clearing paint data, using the
source program's immutable ownership and ancestry. Clearing parents before their
descendants therefore does not require recovering ownership from retired rows.
Attachment also invalidates the destination's existing scopes. Read-only recordings
do not consume the log, and publication preserves changes newer than the recording
snapshot. The log shares the frame's program and owns only pending scope entries;
the old per-row ordering stamps are no longer needed.

Layout commit compares child identities separately from their placement and drawing
data. The order planner consumes `PaintOrderInputs`, and each row keeps an 8-byte
snapshot of those inputs. Commit and visual-context assignment refresh this snapshot, covering
both layout changes and style changes which do not require layout. Geometry and
content changes alone leave the program reusable. Retained compilation consumes
these prepared inputs. Compilation without a source gathers current inputs
independently, so canonical verification can detect a missing notification or
stale prepared snapshot.

Stacking-context entry changes invalidate the context's composition, preserving the
internal plans of unchanged child scopes. Float and inline/replaced counts affect
that composition when their phases appear or disappear. Reattachment conservatively
invalidates the moved subtree's participation as well as its source and destination.

Copied hit items rebind external geometry when the geometry revision changes.
A validated clean scope also proves that geometry owned by its paint subtree
is unchanged: these hit items are copied directly, while references outside
the scope still get repaired. Arbitrary producer spans carry no such proof.
SVG content and scroll metadata additionally depend on
appropriate descendant invalidation; snap areas are not solely inputs of the
scroller's own box.

Viewport scrolling keeps the scrollport size unchanged; the viewport position is
compositor state, not a shared recording input. The root background separately
depends on the union of the viewport and root overflow rectangles. If that painted
area changes, sparse scheduling and scope reuse checks select its background
producer (or opaque SVG unit) while preserving unrelated output.

`visual_context/cache_changes.rs` journals the typed slots invalidated by AVC
retirement, repurposing and recycling. It coalesces changes relative to the last
published recording, preserves changes newer than a recording snapshot and treats
fresh trees or missing history as a full reset. Read-only recordings do not consume
the journal. Payload-only AVC changes preserve identity and remain replay updates.

When structural epochs differ, `avc_reuse.rs` derives invalid references and their
dependent contexts from this journal. A single pass over the source commands and
hit items finds the affected producers through the existing output directory.
Validation includes command headers, embedded scroll and animation references, and
inline mask, group and pattern records. External nested display lists retain their
own visual-context namespace. Only affected source intervals become unavailable;
content, geometry, frame-input and recorded-output checks still apply independently.
Sparse recording schedules invalidated producers even when their layout rows stayed
clean. Missing history and whole-tree resets retain the full-recording fallback.

Metadata layouts have compile-time size checks: operations are 12 bytes, scopes
24 bytes, and output boundaries 8 bytes. Owner inputs are stored once per program
owner. `BenchmarkDisplayListRecording` reports retained capacities, including
invalidation stamps, rather than counting logical payload sizes alone. Those
counters exclude allocator overhead, external resources and paths, scratch
workspace, C++ objects and other in-flight frames; process memory must be measured
separately.

`--verify-paint-cache` compares cached recording with fresh ordering and command
production, including frames retaining only ordering. Directory tests cover growth
and empty/skipped occurrences. Program and topology tests cover copying, remapping,
coalescing, row retirement, reparenting and publication boundaries. Web regressions cover
reparenting, z-order changes, hidden content, scroll metadata and vector resources.
`internals.paintProgramStats()` reports the last recording's rebuilt/copied scope
counts and whole-program sharing. Rebuilt counts include structurally empty helper
plans, which have no retained intervals. Tests can check ordering locality even
when visual-context changes force fresh command production. The shared-context
removal benchmark times mutation/layout and recording separately.

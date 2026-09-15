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

Content invalidation uses the existing source-relative row stamps. Topology has
separate owner and subtree revisions, updated while old and new paint ownership
is known. A topology change recompiles affected ordering scopes; unchanged scope
intervals can be copied into the new program. Unknown ownership forces a fresh
ordering pass. Embedded visual-context indices conservatively require matching
structural epochs. Copied hit items rebind external geometry when the geometry
revision changes. SVG content and scroll metadata additionally depend on
appropriate descendant invalidation; snap areas are not solely inputs of the
scroller's own box.

Metadata layouts have compile-time size checks: operations are 12 bytes, scopes
24 bytes, and output boundaries 8 bytes. Owner inputs are stored once per program
owner. `BenchmarkDisplayListRecording` reports retained capacities, including
invalidation stamps, rather than counting logical payload sizes alone. Those
counters exclude allocator overhead, external resources and paths, scratch
workspace, C++ objects and other in-flight frames; process memory must be measured
separately.

`--verify-paint-cache` compares cached recording with fresh ordering and command
production. Directory tests cover growth and empty/skipped occurrences; program
tests cover copying and remapping scope intervals. Web regressions cover
reparenting, z-order changes, hidden content, scroll metadata and vector resources.

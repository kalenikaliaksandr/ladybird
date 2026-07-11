/*
 * Copyright (c) 2022-2025, Andreas Kling <andreas@ladybird.org>
 * Copyright (c) 2024-2026, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <LibWeb/Forward.h>
#include <LibWeb/Painting/Paintable.h>
#include <LibWeb/Painting/PaintableFragment.h>

namespace Web::Painting {

class HitTestDisplayList;
class InlinePaintable;

// The paintable that owns the results of self-painting inline boxes' foreground: the nearest
// stacking-context-establishing or positioned inline box paints its own fragments (inside its
// group), everything else is painted by the block container.
InlinePaintable const* nearest_self_painting_inline_box(Layout::Node const&);

class PaintableWithLines : public Paintable {
public:
    static NonnullRefPtr<PaintableWithLines> create(Layout::BlockContainer const&);
    virtual ~PaintableWithLines() override;
    virtual StringView class_name() const override { return "PaintableWithLines"sv; }

    virtual void reset_for_relayout() override;

    Vector<PaintableFragment> const& fragments() const { return m_fragments; }
    Vector<PaintableFragment>& fragments() { return m_fragments; }

    Vector<LineRecord> const& lines() const { return m_lines; }
    void set_lines(Vector<LineRecord> lines) { m_lines = move(lines); }

    Vector<InlineBoxPiece> const& inline_box_pieces() const { return m_inline_box_pieces; }
    Vector<InlineBoxPiece>& inline_box_pieces() { return m_inline_box_pieces; }
    void set_inline_box_pieces(Vector<InlineBoxPiece> pieces) { m_inline_box_pieces = move(pieces); }

    void add_fragment(Layout::LineBoxFragment const& fragment, u32 line_index)
    {
        m_fragments.empend(*this, fragment, line_index);
    }
    void reset_fragment_selection_states()
    {
        for (auto& fragment : m_fragments)
            fragment.set_selection_state(SelectionState::None);
    }

    virtual void paint(DisplayListRecordingContext&, PaintPhase) const override;
    virtual void record_hit_test_items(DisplayListRecordingContext&, PaintPhase) const override;
    virtual bool foreground_paints_descendant_content() const override { return true; }
    static void paint_text_fragment_debug_highlight(DisplayListRecordingContext&, PaintableFragment const&);

    // Distributes per-piece geometry to the inline boxes' paintables: their offset and content
    // size become the union of their pieces' content boxes.
    void assign_inline_box_geometry();

    // Decides which paintable paints/hit-tests each fragment's foreground: fragments inside
    // self-painting inline boxes belong to the nearest such box (their glyphs must be recorded
    // inside that box's stacking context), everything else to this block (owner == nullptr).
    struct FragmentRange {
        u32 begin { 0 };
        u32 end { 0 };
    };
    struct FragmentOwnershipFilter {
        bool include_everything { false };
        Vector<FragmentRange, 4> included;
        Vector<FragmentRange, 4> excluded;
        bool is_excluded(size_t index) const;
        bool contains(size_t index) const;
    };
    FragmentOwnershipFilter fragment_ownership_filter(InlinePaintable const* owner) const;

    // Paints selection backgrounds, text shadows and glyph runs for the fragments the filter's
    // owner is responsible for.
    void paint_fragments_foreground(DisplayListRecordingContext&, FragmentOwnershipFilter const&) const;

    // Paints the caret when it sits in a fragment owned by `owner` (see above); the block
    // itself (owner == nullptr) also handles blank lines and empty editable elements.
    void paint_cursor(DisplayListRecordingContext&, InlinePaintable const* owner) const;

protected:
    PaintableWithLines(Layout::BlockContainer const&);

private:
    [[nodiscard]] virtual bool is_paintable_with_lines() const final { return true; }

    Optional<PaintableFragment const&> fragment_at_position(DOM::Position const&) const;
    Optional<CSSPixelRect> empty_line_caret_rect(DOM::Position const&) const;

    // A caret target for a line box with no fragments (e.g. a blank line in a textarea).
    struct EmptyLineCaretTarget {
        size_t offset { 0 };
        size_t line_index { 0 };
        CSSPixelRect rect;
    };
    Vector<EmptyLineCaretTarget> empty_line_caret_targets() const;
    void record_empty_line_caret_items(HitTestDisplayList&, VisualContextIndex) const;

    Vector<PaintableFragment> m_fragments;
    Vector<LineRecord> m_lines;
    Vector<InlineBoxPiece> m_inline_box_pieces;
};

}

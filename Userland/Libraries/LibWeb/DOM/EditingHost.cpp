#include <LibUnicode/Segmenter.h>
#include <LibWeb/DOM/EditingHost.h>
#include <LibWeb/DOM/Text.h>
#include <LibWeb/Selection/Selection.h>

namespace Web::DOM {

JS_DEFINE_ALLOCATOR(EditingHost);

void EditingHost::handle_insert(String const& data)
{
    dbgln(">EditingHost::handle_insert data=({})", data);

    auto selection = m_document->get_selection();
    auto cursor_position = selection->cursor_position();
    if (!cursor_position) {
        dbgln(">EditingHost::handle_insert: no cursor position");
        return;
    }

    auto node = cursor_position->node();
    if (!node) {
        dbgln(">EditingHost::handle_insert: no node at cursor position");
        return;
    }

    if (!is<DOM::Text>(*node)) {
        dbgln(">EditingHost::handle_insert: node at cursor position is not a Text node");
        auto& realm = node->realm();
        auto text = realm.heap().allocate<DOM::Text>(realm, node->document(), data);
        MUST(node->append_child(*text));
        MUST(selection->collapse(*text, 1));
        return;
    }

    auto& text_node = static_cast<DOM::Text&>(*node);

    dbgln(">need to insert text into DOM::Text={}", node->debug_description());

    MUST(text_node.insert_data(cursor_position->offset(), data));

    text_node.invalidate_style(DOM::StyleInvalidationReason::EditingInsertion);
}

void EditingHost::set_cursor_position(JS::NonnullGCPtr<DOM::Position> const& position)
{
    dbgln(">EditingHost::set_cursor_position node={} offset={}", position->node()->debug_description(), position->offset());
    auto selection = m_document->get_selection();
    MUST(selection->collapse(*position->node(), position->offset()));
}

bool EditingHost::increment_cursor_position_offset()
{
    auto selection = m_document->get_selection();
    auto cursor_position = selection->cursor_position();
    if (!cursor_position)
        return false;

    auto node = cursor_position->node();
    if (!node)
        return false;

    dbgln(">EditingHost::increment_cursor_position_offset offset={}", cursor_position->offset());

    if (!is<DOM::Text>(*node))
        return false;

    auto& text_node = static_cast<DOM::Text&>(*node);
    if (auto offset = text_node.grapheme_segmenter().next_boundary(cursor_position->offset()); offset.has_value()) {
        MUST(selection->collapse(*node, *offset));
        return true;
    }

    return false;
}

bool EditingHost::decrement_cursor_position_offset()
{
    auto selection = m_document->get_selection();
    auto cursor_position = selection->cursor_position();
    if (!cursor_position)
        return false;

    dbgln(">EditingHost::decrement_cursor_position_offset offset={}", cursor_position->offset());

    auto node = cursor_position->node();
    if (!node)
        return false;

    if (!is<DOM::Text>(*node))
        return false;

    auto& text_node = static_cast<DOM::Text&>(*node);
    if (auto offset = text_node.grapheme_segmenter().previous_boundary(cursor_position->offset()); offset.has_value()) {
        MUST(selection->collapse(*node, *offset));
        return true;
    }

    return false;
}

void EditingHost::delete_character_before_cursor()
{
}

void EditingHost::delete_character_after_cursor()
{
    dbgln(">EditingHost::delete_character_after_cursor");

    auto selection = m_document->get_selection();
    auto cursor_position = selection->cursor_position();
    if (!cursor_position)
        return;

    auto node = cursor_position->node();
    if (!node)
        return;

    if (!is<DOM::Text>(*node))
        return;

    auto& text_node = static_cast<DOM::Text&>(*node);
    MUST(text_node.delete_data(cursor_position->offset(), 1));
    text_node.invalidate_style(DOM::StyleInvalidationReason::EditingInsertion);
}

void EditingHost::handle_return_key()
{
}

}

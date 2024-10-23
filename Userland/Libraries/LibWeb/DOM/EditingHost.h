#pragma once

#include <LibJS/Heap/Cell.h>
#include <LibJS/Heap/CellAllocator.h>
#include <LibJS/Runtime/Realm.h>
#include <LibWeb/DOM/Document.h>
#include <LibWeb/DOM/InputEventsTarget.h>
#include <LibWeb/Forward.h>

namespace Web::DOM {

class EditingHost : public JS::Cell
    , public InputEventsTarget {
    JS_CELL(EditingHost, JS::Cell);
    JS_DECLARE_ALLOCATOR(EditingHost);

public:
    [[nodiscard]] static JS::NonnullGCPtr<EditingHost> create(JS::Realm& realm, JS::NonnullGCPtr<Document> document)
    {
        return realm.heap().allocate<EditingHost>(realm, document);
    }

    virtual void handle_insert(String const&) override;
    virtual void set_cursor_position(JS::NonnullGCPtr<DOM::Position> const&) override;
    virtual bool increment_cursor_position_offset() override;
    virtual bool decrement_cursor_position_offset() override;
    virtual void delete_character_before_cursor() override;
    virtual void delete_character_after_cursor() override;
    virtual void handle_return_key() override;

    virtual void visit_edges(Cell::Visitor& visitor) override
    {
        Base::visit_edges(visitor);
        visitor.visit(m_document);
    }

    EditingHost(JS::NonnullGCPtr<Document> document)
        : m_document(document)
    {
    }

private:
    JS::NonnullGCPtr<Document> m_document;
};

}

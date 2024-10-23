#pragma once

#include <LibWeb/Forward.h>

namespace Web {

class InputEventsTarget {
public:
    virtual ~InputEventsTarget() = default;

    virtual void handle_insert(String const&) = 0;
    virtual void handle_return_key() = 0;
    virtual void delete_character_before_cursor() = 0;
    virtual void delete_character_after_cursor() = 0;
    virtual void set_cursor_position(JS::NonnullGCPtr<DOM::Position> const&) = 0;
    virtual bool increment_cursor_position_offset() = 0;
    virtual bool decrement_cursor_position_offset() = 0;
};

}

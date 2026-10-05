/*
 * Copyright (c) 2020-2025, Andreas Kling <andreas@ladybird.org>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <AK/Concepts.h>
#include <LibJS/Embedding/Layout.h>
#include <LibJS/Export.h>
#include <LibJS/Forward.h>
#include <LibJS/Heap/EngineCell.h>

namespace JS {

// An object of the Rust runtime. The facade type of a class of objects derives from Object and recognizes the objects
// of its class with `static bool is_engine_class_of(Object const&)`, which is<T>(), as<T>() and as_if<T>() go through:
// an object flag where the runtime keeps one, the class id for a class that no other class extends, and otherwise
// whether the object's class is that class or extends it.
class JS_API Object : public EngineCell {
public:
    template<typename T>
    requires(IsBaseOf<Object, T> && requires(Object const& object) { { T::is_engine_class_of(object) } -> SameAs<bool>; })
    bool fast_is() const
    {
        return T::is_engine_class_of(*this);
    }

    bool has_engine_object_flag(u16 layout_object_flag) const
    {
        static_assert(JS_LAYOUT_OBJECT_FLAGS_SIZE == sizeof(u16));
        u16 flags;
        __builtin_memcpy(&flags, reinterpret_cast<u8 const*>(this) + JS_LAYOUT_OBJECT_FLAGS_OFFSET, sizeof(flags));
        return (flags & layout_object_flag) != 0;
    }

    // A JS_LAYOUT_CLASS_ID_* value. A class that the runtime derives at run time, as it does for each host class, has
    // the id of the class it extends.
    u16 engine_class_id() const;
    bool is_of_engine_class_or_subclass(u16 layout_class_id) const;
};

}

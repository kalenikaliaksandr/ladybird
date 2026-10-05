/*
 * Copyright (c) 2020, Andreas Kling <andreas@ladybird.org>
 * Copyright (c) 2020-2022, Linus Groh <linusg@serenityos.org>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <LibJS/Export.h>
#include <LibJS/Runtime/Object.h>

namespace JS {

// The ordinary global object of a realm whose host does not create one, and the runtime's own global objects, which
// extend it.
class JS_API GlobalObject : public Object {
public:
    static bool is_engine_class_of(Object const& object) { return object.is_of_engine_class_or_subclass(JS_LAYOUT_CLASS_ID_GLOBAL_OBJECT); }
};

}

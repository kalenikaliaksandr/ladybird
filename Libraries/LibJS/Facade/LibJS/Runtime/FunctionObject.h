/*
 * Copyright (c) 2020, Andreas Kling <andreas@ladybird.org>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <LibJS/Runtime/Object.h>

namespace JS {

class JS_API FunctionObject : public Object {
public:
    // The runtime flags every object that has a [[Call]] internal method, including callable proxies.
    static bool is_engine_class_of(Object const& object) { return object.has_engine_object_flag(JS_LAYOUT_OBJECT_FLAG_IS_FUNCTION); }
};

}

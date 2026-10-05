/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <LibJS/EmbeddingABIConversions.h>
#include <LibJS/Runtime/Object.h>

namespace JS {

u16 Object::engine_class_id() const
{
    return js_object_class_id(EmbeddingABI::object_to_abi(*this));
}

bool Object::is_of_engine_class_or_subclass(u16 layout_class_id) const
{
    return js_object_is_subclass_of(EmbeddingABI::object_to_abi(*this), layout_class_id);
}

}

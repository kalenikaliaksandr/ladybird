/*
 * Copyright (c) 2020, Matthew Olsson <mattco@serenityos.org>
 * Copyright (c) 2022-2023, Linus Groh <linusg@serenityos.org>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <AK/Optional.h>
#include <AK/Utf16String.h>
#include <LibGC/CellAllocator.h>
#include <LibGC/Ptr.h>
#include <LibJS/Export.h>
#include <LibJS/Forward.h>
#include <LibJS/Heap/Cell.h>
#include <LibJS/Heap/EngineCell.h>

namespace JS {

class JS_API Symbol final : public EngineCell {
public:
    enum class Kind {
        Unique,
        Global,
        Private,
    };

    // The facade creates unique symbols, with or without a description, and private symbols without one. Global
    // symbols come only from Symbol.for().
    [[nodiscard]] static GC::Ref<Symbol> create(VM&, Optional<Utf16String> description = {}, Kind = Kind::Unique);
    [[nodiscard]] static GC::Ref<Symbol> create_private(VM& vm) { return create(vm, {}, Kind::Private); }

    Utf16String descriptive_string() const;
};

}

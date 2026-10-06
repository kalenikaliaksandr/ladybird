/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <LibJS/EmbeddingABIConversions.h>
#include <LibJS/Runtime/Symbol.h>

namespace JS {

using namespace EmbeddingABI;

GC::Ref<Symbol> Symbol::create(VM& vm, Optional<Utf16String> description, Kind kind)
{
    JSSymbol* symbol = nullptr;
    switch (kind) {
    case Kind::Unique:
        if (description.has_value())
            symbol = js_symbol_create(vm_to_abi(vm), utf16_view_to_abi(*description));
        else
            symbol = js_symbol_create_without_description(vm_to_abi(vm));
        break;
    case Kind::Private:
        VERIFY(!description.has_value());
        symbol = js_symbol_create_private(vm_to_abi(vm));
        break;
    case Kind::Global:
        VERIFY_NOT_REACHED();
    }
    VERIFY(symbol);
    return cell_ref_from_abi<Symbol>(symbol);
}

Utf16String Symbol::descriptive_string() const
{
    return owned_utf16_string_from_abi(js_symbol_descriptive_string(symbol_to_abi(*this)));
}

}

/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <AK/NonnullRefPtr.h>
#include <AK/RefPtr.h>
#include <LibJS/DecodedBytecodeCache.h>
#include <LibJS/EmbeddingABIConversions.h>
#include <LibJS/SourceCode.h>

// Conversions between the facade's source code and bytecode caches and those of the Rust runtime's embedding ABI. Like
// EmbeddingABIConversions.h, only the facade's own .cpp files may include this header.

namespace JS::EmbeddingABI {

inline JSSourceCode const* source_code_to_abi(SourceCode const& source_code)
{
    return reinterpret_cast<JSSourceCode const*>(&source_code);
}

inline RefPtr<SourceCode const> source_code_from_abi(JSSourceCode const* source_code)
{
    return reinterpret_cast<SourceCode const*>(source_code);
}

// Source code whose one reference the runtime created it with, which the caller takes over.
inline NonnullRefPtr<SourceCode const> adopt_source_code_from_abi(JSSourceCode const* source_code)
{
    VERIFY(source_code);
    return NonnullRefPtr<SourceCode const>(NonnullRefPtr<SourceCode const>::Adopt, *reinterpret_cast<SourceCode const*>(source_code));
}

inline JSDecodedBytecodeCache const* decoded_bytecode_cache_to_abi(RustIntegration::DecodedBytecodeCache const& cache)
{
    return reinterpret_cast<JSDecodedBytecodeCache const*>(&cache);
}

// A decoded cache whose one reference the runtime created it with, which the caller takes over.
inline RefPtr<RustIntegration::DecodedBytecodeCache> adopt_decoded_bytecode_cache_from_abi(JSDecodedBytecodeCache const* cache)
{
    if (!cache)
        return {};
    auto& adopted_cache = *reinterpret_cast<RustIntegration::DecodedBytecodeCache*>(const_cast<JSDecodedBytecodeCache*>(cache));
    return NonnullRefPtr<RustIntegration::DecodedBytecodeCache>(NonnullRefPtr<RustIntegration::DecodedBytecodeCache>::Adopt, adopted_cache);
}

inline JSProgramType program_type_to_abi(RustIntegration::ProgramType program_type)
{
    return to_underlying(program_type);
}

}

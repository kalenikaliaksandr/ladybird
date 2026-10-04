/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <LibJS/Embedding/ABI.h>
#include <LibJS/Embedding/Layout.h>

uint16_t embedding_abi_version_seen_from_c(void);

uint16_t embedding_abi_version_seen_from_c(void)
{
    return js_embedding_abi_version();
}

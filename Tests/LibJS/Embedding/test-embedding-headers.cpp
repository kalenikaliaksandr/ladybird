/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <LibJS/Embedding/ABI.h>
#include <LibJS/Embedding/Layout.h>

#include "EmbeddingTest.h"

extern "C" uint16_t embedding_abi_version_seen_from_c(void);

TEST_CASE(abi_version_is_the_one_of_the_host_object_abi_header)
{
    EXPECT_EQ(js_embedding_abi_version(), static_cast<u16>(JS_HOST_ABI_VERSION));
}

TEST_CASE(headers_declare_the_same_abi_to_c)
{
    EXPECT_EQ(embedding_abi_version_seen_from_c(), js_embedding_abi_version());
}

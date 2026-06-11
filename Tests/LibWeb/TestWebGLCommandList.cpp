/*
 * Copyright (c) 2026, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <LibTest/TestCase.h>
#include <LibWeb/WebGL/WebGLCommandList.h>

TEST_CASE(empty_sync_reply_inline_span_is_resolvable)
{
    struct Reply {
        Web::WebGL::WebGLDataSpan data;
    };

    Reply reply {
        .data = { Web::WebGL::WebGLCommandList::first_inline_data_offset(sizeof(Reply)), 0 },
    };

    auto reply_bytes = Web::WebGL::WebGLSyncCall::encode_reply(reply);
    EXPECT_EQ(reply_bytes.size(), Web::WebGL::WebGLCommandList::first_inline_data_offset(sizeof(Reply)));
    Web::WebGL::WebGLCommandList::copy_data_span(reply_bytes, reply.data, {});
}

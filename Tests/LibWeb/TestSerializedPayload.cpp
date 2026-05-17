/*
 * Copyright (c) 2026, Ladybird contributors
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <AK/MemoryStream.h>
#include <AK/Queue.h>
#include <LibIPC/Decoder.h>
#include <LibIPC/Encoder.h>
#include <LibIPC/Message.h>
#include <LibTest/TestCase.h>
#include <LibWeb/Compositor/SerializedPayload.h>

TEST_CASE(decode_large_shared_payload)
{
    constexpr size_t payload_size = 64 * MiB + 1;
    auto payload = TRY_OR_FAIL(Web::Compositor::SerializedPayload::create(payload_size));

    IPC::MessageBuffer buffer;
    IPC::Encoder encoder { buffer };
    TRY_OR_FAIL(encoder.encode(payload));

    auto data = buffer.take_data();
    auto attachment_vector = buffer.take_attachments();
    Queue<IPC::Attachment> attachments;
    for (auto& attachment : attachment_vector)
        attachments.enqueue(move(attachment));

    FixedMemoryStream stream { data.span() };
    IPC::Decoder decoder { stream, attachments };
    auto decoded_payload = TRY_OR_FAIL(decoder.decode<Web::Compositor::SerializedPayload>());

    EXPECT_EQ(decoded_payload.size(), payload_size);
    EXPECT(decoded_payload.buffer().is_valid());
}

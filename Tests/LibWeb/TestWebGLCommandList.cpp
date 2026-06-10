/*
 * Copyright (c) 2026, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <LibTest/TestCase.h>
#include <LibWeb/WebGL/WebGLCommandList.h>

namespace Web::WebGL {

TEST_CASE(empty_list)
{
    WebGLCommandList list;
    EXPECT(list.is_empty());
    size_t count = 0;
    MUST(WebGLCommandList::for_each_command(list.bytes(), [&](auto const&, ReadonlyBytes) -> ErrorOr<void> {
        ++count;
        return {};
    }));
    EXPECT_EQ(count, 0u);
}

TEST_CASE(round_trip)
{
    WebGLCommandList list;

    list.append(Commands::ActiveTexture { .texture = 0x84C0 });

    Array<float, 4> vertex_data { 1.0f, 2.0f, 3.0f, 4.0f };
    auto vertex_bytes = ReadonlyBytes { vertex_data.data(), sizeof(vertex_data) };
    Commands::BufferData buffer_data {
        .target = 0x8892,
        .size = static_cast<GLsizeiptr>(vertex_bytes.size()),
        .has_data = true,
        .data = { WebGLCommandList::first_inline_data_offset(sizeof(Commands::BufferData)), static_cast<u32>(vertex_bytes.size()) },
        .usage = 0x88E4,
    };
    list.append(buffer_data, vertex_bytes);

    Array<WebGLObjectId, 3> ids { 1, 2, 3 };
    auto id_bytes = ReadonlyBytes { ids.data(), sizeof(ids) };
    Commands::GenBuffers gen_buffers {
        .n = 3,
        .buffers = { WebGLCommandList::first_inline_data_offset(sizeof(Commands::GenBuffers)), static_cast<u32>(id_bytes.size()) },
    };
    list.append(gen_buffers, id_bytes);

    Vector<WebGLCommandType> seen;
    MUST(WebGLCommandList::for_each_command(list.bytes(), [&]<typename Command>(Command const& command, ReadonlyBytes payload) -> ErrorOr<void> {
        seen.append(Command::command_type);
        if constexpr (IsSame<Command, Commands::ActiveTexture>) {
            EXPECT_EQ(command.texture, 0x84C0u);
        } else if constexpr (IsSame<Command, Commands::BufferData>) {
            EXPECT_EQ(command.target, 0x8892u);
            EXPECT_EQ(command.size, static_cast<GLsizeiptr>(16));
            EXPECT(command.has_data);
            auto resolved = TRY(WebGLCommandList::resolve_typed_span<float>(payload, command.data));
            EXPECT_EQ(resolved.size(), 4u);
            EXPECT_EQ(resolved[0], 1.0f);
            EXPECT_EQ(resolved[3], 4.0f);
        } else if constexpr (IsSame<Command, Commands::GenBuffers>) {
            EXPECT_EQ(command.n, 3);
            auto resolved = TRY(WebGLCommandList::resolve_typed_span<WebGLObjectId>(payload, command.buffers));
            EXPECT_EQ(resolved.size(), 3u);
            EXPECT_EQ(resolved[2], 3u);
        }
        return {};
    }));

    EXPECT_EQ(seen.size(), 3u);
    EXPECT_EQ(seen[0], WebGLCommandType::ActiveTexture);
    EXPECT_EQ(seen[1], WebGLCommandType::BufferData);
    EXPECT_EQ(seen[2], WebGLCommandType::GenBuffers);
}

static ErrorOr<void> iterate(ReadonlyBytes bytes)
{
    return WebGLCommandList::for_each_command(bytes, [](auto const&, ReadonlyBytes) -> ErrorOr<void> {
        return {};
    });
}

TEST_CASE(truncated_stream_is_an_error)
{
    WebGLCommandList list;
    list.append(Commands::ActiveTexture { .texture = 0x84C0 });
    Array<float, 4> data {};
    Commands::BufferData buffer_data {
        .target = 0x8892,
        .size = sizeof(data),
        .has_data = true,
        .data = { WebGLCommandList::first_inline_data_offset(sizeof(Commands::BufferData)), sizeof(data) },
        .usage = 0x88E4,
    };
    list.append(buffer_data, ReadonlyBytes { data.data(), sizeof(data) });

    auto bytes = list.bytes();
    for (size_t cut = 1; cut <= WebGLCommandList::command_alignment; ++cut)
        EXPECT(iterate(bytes.slice(0, bytes.size() - cut)).is_error());
}

TEST_CASE(invalid_command_type_is_an_error)
{
    WebGLCommandList list;
    list.append(Commands::ActiveTexture { .texture = 0 });
    auto buffer = MUST(ByteBuffer::copy(list.bytes()));
    WebGLCommandHeader header {
        .type = static_cast<WebGLCommandType>(0xFFFF),
        .payload_size = static_cast<u32>(buffer.size() - sizeof(WebGLCommandHeader)),
    };
    __builtin_memcpy(buffer.data(), &header, sizeof(header));
    EXPECT(iterate(buffer).is_error());
}

TEST_CASE(payload_too_small_for_command_is_an_error)
{
    WebGLCommandHeader header {
        .type = WebGLCommandType::ActiveTexture,
        .payload_size = 0,
    };
    auto buffer = MUST(ByteBuffer::create_zeroed(sizeof(header)));
    __builtin_memcpy(buffer.data(), &header, sizeof(header));
    EXPECT(iterate(buffer).is_error());
}

TEST_CASE(out_of_bounds_data_span_is_an_error)
{
    Array<u8, 16> payload {};
    EXPECT(!WebGLCommandList::resolve_data_span(payload, { 0, 16 }).is_error());
    EXPECT(WebGLCommandList::resolve_data_span(payload, { 8, 9 }).is_error());
    EXPECT(WebGLCommandList::resolve_data_span(payload, { 17, 0 }).is_error());
    EXPECT(WebGLCommandList::resolve_data_span(payload, { 0xFFFFFFFF, 0xFFFFFFFF }).is_error());
}

TEST_CASE(misaligned_typed_span_is_an_error)
{
    alignas(16) Array<u8, 32> payload {};
    EXPECT(!WebGLCommandList::resolve_typed_span<float>(payload, { 0, 16 }).is_error());
    EXPECT(WebGLCommandList::resolve_typed_span<float>(payload, { 1, 16 }).is_error());
    EXPECT(WebGLCommandList::resolve_typed_span<float>(payload, { 0, 15 }).is_error());
}

}

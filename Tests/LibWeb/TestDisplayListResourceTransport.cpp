/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <AK/MemoryStream.h>
#include <AK/Queue.h>
#include <AK/Time.h>
#include <LibCore/AnonymousBuffer.h>
#include <LibGfx/ColorSpace.h>
#include <LibGfx/YUVData.h>
#include <LibIPC/Decoder.h>
#include <LibIPC/Encoder.h>
#include <LibIPC/Message.h>
#include <LibMedia/VideoFrame.h>
#include <LibTest/TestCase.h>
#include <LibWeb/Painting/DisplayListResourceTransport.h>
#include <string.h>

static Media::CodingIndependentCodePoints test_cicp()
{
    return {
        Media::ColorPrimaries::BT709,
        Media::TransferCharacteristics::BT709,
        Media::MatrixCoefficients::BT709,
        Media::VideoFullRangeFlag::Full,
    };
}

static void fill_plane(Bytes plane, u8 seed)
{
    for (size_t i = 0; i < plane.size(); ++i)
        plane[i] = static_cast<u8>(seed + i);
}

static ErrorOr<NonnullRefPtr<Media::VideoFrame>> make_video_frame(Gfx::IntSize size, u8 bit_depth, Media::Subsampling subsampling, Media::CodingIndependentCodePoints cicp = test_cicp())
{
    auto yuv_data = TRY(Gfx::YUVData::create(size, bit_depth, subsampling, cicp));
    fill_plane(yuv_data->y_data(), 11);
    fill_plane(yuv_data->u_data(), 37);
    fill_plane(yuv_data->v_data(), 83);

    auto color_space = TRY(Gfx::ColorSpace::from_cicp(cicp));
    return try_make_ref_counted<Media::VideoFrame>(
        AK::Duration::from_milliseconds(123),
        AK::Duration::from_milliseconds(33),
        size.to_type<u32>(),
        bit_depth,
        move(color_space),
        move(yuv_data));
}

static ErrorOr<Web::Painting::VideoFrameTransport> ipc_roundtrip(Web::Painting::VideoFrameTransport const& transport)
{
    IPC::MessageBuffer buffer;
    IPC::Encoder encoder { buffer };
    TRY(encoder.encode(transport));

    Queue<IPC::Attachment> attachments;
    for (auto& attachment : buffer.take_attachments())
        attachments.enqueue(move(attachment));

    FixedMemoryStream stream { buffer.data().span() };
    IPC::Decoder decoder { stream, attachments };
    return decoder.decode<Web::Painting::VideoFrameTransport>();
}

static ErrorOr<NonnullRefPtr<Media::VideoFrame const>> decode_video_frame(IPC::MessageBuffer buffer)
{
    Queue<IPC::Attachment> attachments;
    for (auto& attachment : buffer.take_attachments())
        attachments.enqueue(move(attachment));

    FixedMemoryStream stream { buffer.data().span() };
    IPC::Decoder decoder { stream, attachments };
    return decoder.decode<NonnullRefPtr<Media::VideoFrame const>>();
}

static void expect_bytes_equal(ReadonlyBytes actual, ReadonlyBytes expected)
{
    EXPECT_EQ(actual.size(), expected.size());
    if (actual.size() == expected.size() && !actual.is_empty())
        EXPECT_EQ(memcmp(actual.data(), expected.data(), actual.size()), 0);
}

static void expect_video_frames_equal(Media::VideoFrame const& actual, Media::VideoFrame const& expected)
{
    EXPECT_EQ(actual.timestamp(), expected.timestamp());
    EXPECT_EQ(actual.duration(), expected.duration());
    EXPECT_EQ(actual.size(), expected.size());
    EXPECT_EQ(actual.bit_depth(), expected.bit_depth());
    EXPECT_EQ(actual.yuv_data().size(), expected.yuv_data().size());
    EXPECT_EQ(actual.yuv_data().bit_depth(), expected.yuv_data().bit_depth());
    EXPECT_EQ(actual.yuv_data().subsampling().x(), expected.yuv_data().subsampling().x());
    EXPECT_EQ(actual.yuv_data().subsampling().y(), expected.yuv_data().subsampling().y());
    EXPECT(actual.yuv_data().cicp().color_primaries() == expected.yuv_data().cicp().color_primaries());
    EXPECT(actual.yuv_data().cicp().transfer_characteristics() == expected.yuv_data().cicp().transfer_characteristics());
    EXPECT(actual.yuv_data().cicp().matrix_coefficients() == expected.yuv_data().cicp().matrix_coefficients());
    EXPECT(actual.yuv_data().cicp().video_full_range_flag() == expected.yuv_data().cicp().video_full_range_flag());
    expect_bytes_equal(actual.yuv_data().y_data(), expected.yuv_data().y_data());
    expect_bytes_equal(actual.yuv_data().u_data(), expected.yuv_data().u_data());
    expect_bytes_equal(actual.yuv_data().v_data(), expected.yuv_data().v_data());
}

TEST_CASE(video_frame_transport_round_trips_yuv_420_through_ipc)
{
    auto frame = TRY_OR_FAIL(make_video_frame({ 4, 4 }, 8, Media::Subsampling { true, true }));
    auto transport = Web::Painting::create_video_frame_transport(Web::Painting::VideoFrameResourceId { 1 }, frame);
    EXPECT(transport.frame.has_value());

    auto decoded_transport = TRY_OR_FAIL(ipc_roundtrip(transport));
    EXPECT(decoded_transport.frame.has_value());
    expect_video_frames_equal(*decoded_transport.frame.value(), frame);
}

TEST_CASE(video_frame_transport_round_trips_high_bit_depth_yuv_444_through_ipc)
{
    auto frame = TRY_OR_FAIL(make_video_frame({ 2, 2 }, 10, Media::Subsampling { false, false }));
    auto transport = Web::Painting::create_video_frame_transport(Web::Painting::VideoFrameResourceId { 2 }, frame);
    EXPECT(transport.frame.has_value());

    auto decoded_transport = TRY_OR_FAIL(ipc_roundtrip(transport));
    EXPECT(decoded_transport.frame.has_value());
    expect_video_frames_equal(*decoded_transport.frame.value(), frame);
}

TEST_CASE(video_frame_transport_round_trips_unspecified_cicp_through_ipc)
{
    Media::CodingIndependentCodePoints cicp;
    auto frame = TRY_OR_FAIL(make_video_frame({ 4, 4 }, 8, Media::Subsampling { true, true }, cicp));
    auto transport = Web::Painting::create_video_frame_transport(Web::Painting::VideoFrameResourceId { 3 }, frame);
    EXPECT(transport.frame.has_value());

    auto decoded_transport = TRY_OR_FAIL(ipc_roundtrip(transport));
    EXPECT(decoded_transport.frame.has_value());
    expect_video_frames_equal(*decoded_transport.frame.value(), frame);
}

TEST_CASE(video_frame_transport_round_trips_empty_frame_resource_through_ipc)
{
    Web::Painting::VideoFrameTransport transport {
        .id = Web::Painting::VideoFrameResourceId { 4 },
        .frame = {},
    };

    auto decoded_transport = TRY_OR_FAIL(ipc_roundtrip(transport));
    EXPECT_EQ(decoded_transport.id.value(), transport.id.value());
    EXPECT(!decoded_transport.frame.has_value());
}

TEST_CASE(video_frame_ipc_rejects_corrupt_yuv_buffer_size)
{
    auto cicp = test_cicp();
    auto color_space = TRY_OR_FAIL(Gfx::ColorSpace::from_cicp(cicp));
    auto yuv_data_buffer = TRY_OR_FAIL(Core::AnonymousBuffer::create_with_size(1));
    IPC::MessageBuffer buffer;
    IPC::Encoder encoder { buffer };

    TRY_OR_FAIL(encoder.encode(yuv_data_buffer));
    TRY_OR_FAIL(encoder.encode(color_space));
    TRY_OR_FAIL(encoder.encode(AK::Duration::from_milliseconds(123)));
    TRY_OR_FAIL(encoder.encode(AK::Duration::from_milliseconds(33)));
    TRY_OR_FAIL(encoder.encode(Gfx::IntSize { 4, 4 }));
    TRY_OR_FAIL(encoder.encode(static_cast<u8>(8)));
    TRY_OR_FAIL(encoder.encode(true));
    TRY_OR_FAIL(encoder.encode(true));
    TRY_OR_FAIL(encoder.encode(cicp.color_primaries()));
    TRY_OR_FAIL(encoder.encode(cicp.transfer_characteristics()));
    TRY_OR_FAIL(encoder.encode(cicp.matrix_coefficients()));
    TRY_OR_FAIL(encoder.encode(cicp.video_full_range_flag()));

    auto decoded_frame = decode_video_frame(move(buffer));
    EXPECT(decoded_frame.is_error());
}

#include <LibCore/VulkanSharedMemoryDescriptor.h>
#include <LibIPC/Decoder.h>
#include <LibIPC/Encoder.h>
#include <LibIPC/File.h>

namespace Core {

}

namespace IPC {

template<>
ErrorOr<void> encode(Encoder& encoder, Core::VulkanSharedMemoryDescriptor const& descriptor)
{
    TRY(encoder.encode(TRY(IPC::File::clone_fd(descriptor.fd))));
    return {};
}

template<>
ErrorOr<Core::VulkanSharedMemoryDescriptor> decode(Decoder& decoder)
{
    (void)decoder;
    dbgln(">>>>>>FIXME: VulkanSharedMemoryDescriptor::decode");

    auto file = TRY(decoder.decode<IPC::File>());

    return { Core::VulkanSharedMemoryDescriptor { .fd = file.take_fd() } };

    //     if (auto valid = TRY(decoder.decode<bool>()); !valid)
    //         return Gfx::ShareableBitmap {};
    //
    //     auto anon_file = TRY(decoder.decode<IPC::File>());
    //     auto size = TRY(decoder.decode<Gfx::IntSize>());
    //     auto raw_bitmap_format = TRY(decoder.decode<u32>());
    //     if (!Gfx::is_valid_bitmap_format(raw_bitmap_format))
    //         return Error::from_string_literal("IPC: Invalid Gfx::ShareableBitmap format");
    //
    //     auto bitmap_format = static_cast<Gfx::BitmapFormat>(raw_bitmap_format);
    //
    //     auto buffer = TRY(Core::AnonymousBuffer::create_from_anon_fd(anon_file.take_fd(), Gfx::Bitmap::size_in_bytes(Gfx::Bitmap::minimum_pitch(size.width(), bitmap_format), size.height())));
    //     auto bitmap = TRY(Gfx::Bitmap::create_with_anonymous_buffer(bitmap_format, move(buffer), size));
    //
    //     return Gfx::ShareableBitmap { move(bitmap), Gfx::ShareableBitmap::ConstructWithKnownGoodBitmap };
}

}

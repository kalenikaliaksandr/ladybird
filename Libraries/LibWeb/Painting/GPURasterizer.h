/*
 * Copyright (c) 2026, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <AK/Function.h>
#include <AK/Noncopyable.h>
#include <LibGPUProcessClient/Client.h>
#include <LibWeb/Painting/DisplayList.h>
#include <LibWeb/Painting/GPUResourceRegistry.h>
#include <LibWeb/Painting/ScrollState.h>

namespace Web::Painting {

class GPURasterizer {
    AK_MAKE_NONCOPYABLE(GPURasterizer);
    AK_MAKE_NONMOVABLE(GPURasterizer);

public:
    using PresentationCallback = Function<void(Gfx::IntRect const&, i32)>;

    GPURasterizer(PresentationCallback, NonnullRefPtr<GPUProcessClient::Client>);
    ~GPURasterizer();

    void update_display_list(NonnullRefPtr<DisplayList>, ScrollStateSnapshotByDisplayList&&);
    void update_backing_stores(i32 front_id, i32 back_id);
    void present_frame(Gfx::IntRect);

    void ready_to_paint();

private:
    PresentationCallback m_presentation_callback;
    NonnullRefPtr<GPUProcessClient::Client> m_gpu_client;
    GPUResourceRegistry m_resource_registry;
    u64 m_page_id { 0 };
};

}

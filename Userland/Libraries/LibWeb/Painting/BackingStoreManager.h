/*
 * Copyright (c) 2024, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <LibCore/Forward.h>
#include <LibWeb/Page/Page.h>
#include <LibWeb/Painting/BackingStore.h>
#include <WebContent/Forward.h>

#ifdef USE_VULKAN
#    include <LibCore/VulkanContext.h>
#endif

namespace Web::Painting {

class BackingStoreManager {
public:
#ifdef AK_OS_MACOS
    static void set_browser_mach_port(Core::MachPort&&);
#endif

    enum class WindowResizingInProgress {
        No,
        Yes
    };
    void resize_backing_stores_if_needed(WindowResizingInProgress window_resize_in_progress);
    void reallocate_backing_stores(Gfx::IntSize);
    void restart_resize_timer();

    Web::Painting::BackingStore* back_store() { return m_back_store.ptr(); }
    i32 front_id() const { return m_front_bitmap_id; }

    void swap_back_and_front();

#ifdef USE_VULKAN
    void set_vulkan_context(RefPtr<Core::VulkanContext> vulkan_context);
#endif

    BackingStoreManager(PageClient&);

private:
    PageClient& m_page_client;

    i32 m_front_bitmap_id { -1 };
    i32 m_back_bitmap_id { -1 };
    OwnPtr<BackingStore> m_front_store;
    OwnPtr<BackingStore> m_back_store;
    int m_next_bitmap_id { 0 };

    RefPtr<Core::Timer> m_backing_store_shrink_timer;

#ifdef USE_VULKAN
    RefPtr<Core::VulkanContext> m_vulkan_context;
#endif
};

}

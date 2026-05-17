/*
 * Copyright (c) 2026, Ladybird contributors
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <AK/Error.h>
#include <LibIPC/Forward.h>
#include <LibWeb/Compositor/AsyncScrollingState.h>
#include <LibWeb/Export.h>
#include <LibWeb/Painting/ScrollState.h>

namespace IPC {

template<>
WEB_API ErrorOr<void> encode(Encoder&, Web::Compositor::AsyncScrollNodeStableID const&);

template<>
WEB_API ErrorOr<Web::Compositor::AsyncScrollNodeStableID> decode(Decoder&);

template<>
WEB_API ErrorOr<void> encode(Encoder&, Web::Compositor::AsyncScrollOffset const&);

template<>
WEB_API ErrorOr<Web::Compositor::AsyncScrollOffset> decode(Decoder&);

template<>
WEB_API ErrorOr<void> encode(Encoder&, Web::Painting::ScrollStateSnapshot const&);

template<>
WEB_API ErrorOr<Web::Painting::ScrollStateSnapshot> decode(Decoder&);

}

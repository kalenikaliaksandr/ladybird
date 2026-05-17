/*
 * Copyright (c) 2026, Ladybird contributors
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <AK/Error.h>
#include <AK/NonnullRefPtr.h>
#include <LibIPC/Forward.h>
#include <LibWeb/Export.h>
#include <LibWeb/Painting/DisplayList.h>

namespace IPC {

template<>
WEB_API ErrorOr<void> encode(Encoder&, Web::Painting::DisplayList const&);

template<>
WEB_API ErrorOr<void> encode(Encoder&, NonnullRefPtr<Web::Painting::DisplayList> const&);

template<>
WEB_API ErrorOr<void> encode(Encoder&, NonnullRefPtr<Web::Painting::DisplayList const> const&);

template<>
WEB_API ErrorOr<NonnullRefPtr<Web::Painting::DisplayList>> decode(Decoder&);

template<>
WEB_API ErrorOr<NonnullRefPtr<Web::Painting::DisplayList const>> decode(Decoder&);

}

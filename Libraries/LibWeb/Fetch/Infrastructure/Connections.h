/*
 * Copyright (c) 2026, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <LibURL/Forward.h>

namespace Web::Fetch::Infrastructure {

// https://fetch.spec.whatwg.org/#resolve-an-origin
void resolve_an_origin(URL::Origin const&);

// https://fetch.spec.whatwg.org/#concept-connection-obtain
void obtain_a_connection(URL::Origin const&, bool use_credentials);

}

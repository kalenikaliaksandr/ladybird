/*
 * Copyright (c) 2026, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <AK/Debug.h>
#include <LibRequests/RequestClient.h>
#include <LibURL/Origin.h>
#include <LibURL/URL.h>
#include <LibWeb/Fetch/Infrastructure/Connections.h>
#include <LibWeb/Loader/ContentFilter.h>
#include <LibWeb/Loader/ResourceLoader.h>

namespace Web::Fetch::Infrastructure {

static Optional<URL::URL> url_for_origin(URL::Origin const& origin)
{
    // NOTE: dns-prefetch and preconnect only meaningfully apply to HTTP(S)
    //       origins; opaque and non-HTTP(S) origins have nothing to resolve
    //       or connect to at this layer.
    if (origin.is_opaque())
        return {};
    if (!origin.scheme().has_value() || !origin.scheme()->is_one_of("http"sv, "https"sv))
        return {};

    return URL::create_with_url_or_path(origin.serialize().to_byte_string());
}

// https://fetch.spec.whatwg.org/#resolve-an-origin
void resolve_an_origin(URL::Origin const& origin)
{
    auto url = url_for_origin(origin);
    if (!url.has_value())
        return;

    if (ContentFilter::the().is_filtered(*url)) {
        dbgln("Fetch: Refusing to resolve origin '{}': URL was filtered", *url);
        return;
    }

    // FIXME: Apply the network partition key per the Fetch spec.
    if (auto request_client = ResourceLoader::the().request_client())
        request_client->ensure_connection(*url, RequestServer::CacheLevel::ResolveOnly);
}

// https://fetch.spec.whatwg.org/#concept-connection-obtain
void obtain_a_connection(URL::Origin const& origin, [[maybe_unused]] bool use_credentials)
{
    auto url = url_for_origin(origin);
    if (!url.has_value())
        return;

    if (ContentFilter::the().is_filtered(*url)) {
        dbgln("Fetch: Refusing to preconnect to '{}': URL was filtered", *url);
        return;
    }

    // FIXME: Apply the network partition key and use_credentials per
    //        the Fetch spec.
    if (auto request_client = ResourceLoader::the().request_client())
        request_client->ensure_connection(*url, RequestServer::CacheLevel::CreateConnection);
}

}

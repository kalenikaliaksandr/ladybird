/*
 * Copyright (c) 2025, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <AK/ByteBuffer.h>
#include <AK/ByteString.h>
#include <AK/Optional.h>
#include <AK/String.h>
#include <AK/Variant.h>
#include <AK/Vector.h>
#include <LibIPC/Forward.h>
#include <LibURL/Origin.h>
#include <LibURL/URL.h>
#include <LibWeb/Forward.h>
#include <LibWeb/HTML/SerializedPolicyContainer.h>
#include <LibWeb/HTML/StructuredSerializeTypes.h>
#include <LibWeb/ReferrerPolicy/ReferrerPolicy.h>
#include <LibWebView/Forward.h>

namespace WebView {

// Serialized equivalent of Web::HTML::POSTResource for IPC transport.
// Unlike POSTResource::Directive::type which uses StringView, this uses owning String.
struct UIPostResource {
    Optional<ByteBuffer> request_body;

    enum class RequestContentType : u8 {
        ApplicationXWWWFormUrlencoded,
        MultipartFormData,
        TextPlain,
    };
    RequestContentType request_content_type {};

    struct Directive {
        String type;
        String value;
    };
    Vector<Directive> request_content_type_directives {};
};

// Mirrors Fetch::Infrastructure::Request::Referrer for IPC transport.
enum class UIReferrer : u8 {
    NoReferrer,
    Client,
};

struct UINestedHistory;

// Serializable mirror of Web::HTML::DocumentState.
// Contains the IPC-transferable subset needed for session history management in the UI process.
struct UIDocumentState {
    Optional<Web::UniqueNodeID> document_id;
    Optional<URL::Origin> origin;
    Optional<URL::Origin> initiator_origin;
    Optional<URL::URL> about_base_url;

    // history_policy_container: None = "client" (the spec's default), Some = serialized policy container
    Optional<Web::HTML::SerializedPolicyContainer> history_policy_container;

    Variant<UIReferrer, URL::URL> request_referrer { UIReferrer::Client };
    Web::ReferrerPolicy::ReferrerPolicy request_referrer_policy { Web::ReferrerPolicy::DEFAULT_REFERRER_POLICY };

    Variant<Empty, String, UIPostResource> resource {};

    bool reload_pending { false };
    bool ever_populated { false };

    String navigable_target_name;
    Vector<UINestedHistory> nested_histories;
};

enum class UIScrollRestorationMode : u8 {
    Auto,
    Manual,
};

// Serializable mirror of Web::HTML::SessionHistoryEntry.
// This is the IPC-transferable representation used by the UI process to maintain session history.
struct UISessionHistoryEntry {
    // UI-assigned stable identity for this entry. 0 means "not yet assigned".
    u64 id { 0 };

    // step: nullopt means "pending" (entry not yet assigned a step number)
    Optional<int> step;

    URL::URL url;
    UIDocumentState document_state;

    Web::HTML::SerializationRecord classic_history_api_state;
    Web::HTML::SerializationRecord navigation_api_state;

    String navigation_api_key;
    String navigation_api_id;

    UIScrollRestorationMode scroll_restoration_mode { UIScrollRestorationMode::Auto };

    Optional<Web::HTML::SerializedPolicyContainer> policy_container;
    Optional<ByteString> browsing_context_name;
};

struct UINestedHistory {
    String navigable_id;
    Vector<UISessionHistoryEntry> entries;
};

Vector<int> get_all_used_history_steps(Vector<UISessionHistoryEntry> const&);

}

namespace IPC {

template<>
WEBVIEW_API ErrorOr<void> encode(Encoder&, WebView::UIPostResource::Directive const&);
template<>
WEBVIEW_API ErrorOr<WebView::UIPostResource::Directive> decode(Decoder&);

template<>
WEBVIEW_API ErrorOr<void> encode(Encoder&, WebView::UIPostResource const&);
template<>
WEBVIEW_API ErrorOr<WebView::UIPostResource> decode(Decoder&);

template<>
WEBVIEW_API ErrorOr<void> encode(Encoder&, WebView::UIDocumentState const&);
template<>
WEBVIEW_API ErrorOr<WebView::UIDocumentState> decode(Decoder&);

template<>
WEBVIEW_API ErrorOr<void> encode(Encoder&, WebView::UISessionHistoryEntry const&);
template<>
WEBVIEW_API ErrorOr<WebView::UISessionHistoryEntry> decode(Decoder&);

template<>
WEBVIEW_API ErrorOr<void> encode(Encoder&, WebView::UINestedHistory const&);
template<>
WEBVIEW_API ErrorOr<WebView::UINestedHistory> decode(Decoder&);

}

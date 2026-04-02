/*
 * Copyright (c) 2025, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <AK/QuickSort.h>
#include <LibIPC/Decoder.h>
#include <LibIPC/Encoder.h>
#include <LibWebView/UISessionHistoryEntry.h>

namespace WebView {

// https://html.spec.whatwg.org/multipage/browsing-the-web.html#getting-all-used-history-steps
Vector<int> get_all_used_history_steps(Vector<UISessionHistoryEntry> const& entries)
{
    OrderedHashTable<int> steps;

    Vector<Vector<UISessionHistoryEntry> const*> entry_lists;
    entry_lists.append(&entries);

    while (!entry_lists.is_empty()) {
        auto const* entry_list = entry_lists.take_first();

        for (auto const& entry : *entry_list) {
            if (entry.step.has_value())
                steps.set(entry.step.value());

            for (auto const& nested_history : entry.document_state.nested_histories)
                entry_lists.append(&nested_history.entries);
        }
    }

    auto sorted_steps = steps.values();
    quick_sort(sorted_steps);
    return sorted_steps;
}

}

template<>
ErrorOr<void> IPC::encode(Encoder& encoder, WebView::UIPostResource::Directive const& directive)
{
    TRY(encoder.encode(directive.type));
    TRY(encoder.encode(directive.value));
    return {};
}

template<>
ErrorOr<WebView::UIPostResource::Directive> IPC::decode(Decoder& decoder)
{
    auto type = TRY(decoder.decode<String>());
    auto value = TRY(decoder.decode<String>());
    return WebView::UIPostResource::Directive { move(type), move(value) };
}

template<>
ErrorOr<void> IPC::encode(Encoder& encoder, WebView::UIPostResource const& resource)
{
    TRY(encoder.encode(resource.request_body));
    TRY(encoder.encode(resource.request_content_type));
    TRY(encoder.encode(resource.request_content_type_directives));
    return {};
}

template<>
ErrorOr<WebView::UIPostResource> IPC::decode(Decoder& decoder)
{
    auto request_body = TRY(decoder.decode<Optional<ByteBuffer>>());
    auto request_content_type = TRY(decoder.decode<WebView::UIPostResource::RequestContentType>());
    auto request_content_type_directives = TRY(decoder.decode<Vector<WebView::UIPostResource::Directive>>());
    return WebView::UIPostResource { move(request_body), request_content_type, move(request_content_type_directives) };
}

template<>
ErrorOr<void> IPC::encode(Encoder& encoder, WebView::UIDocumentState const& state)
{
    TRY(encoder.encode(state.document_id));
    TRY(encoder.encode(state.origin));
    TRY(encoder.encode(state.initiator_origin));
    TRY(encoder.encode(state.about_base_url));
    TRY(encoder.encode(state.history_policy_container));
    TRY(encoder.encode(state.request_referrer));
    TRY(encoder.encode(state.request_referrer_policy));
    TRY(encoder.encode(state.resource));
    TRY(encoder.encode(state.reload_pending));
    TRY(encoder.encode(state.ever_populated));
    TRY(encoder.encode(state.navigable_target_name));
    TRY(encoder.encode(state.nested_histories));
    return {};
}

template<>
ErrorOr<WebView::UIDocumentState> IPC::decode(Decoder& decoder)
{
    WebView::UIDocumentState state;
    state.document_id = TRY(decoder.decode<Optional<Web::UniqueNodeID>>());
    state.origin = TRY(decoder.decode<Optional<URL::Origin>>());
    state.initiator_origin = TRY(decoder.decode<Optional<URL::Origin>>());
    state.about_base_url = TRY(decoder.decode<Optional<URL::URL>>());
    state.history_policy_container = TRY(decoder.decode<Optional<Web::HTML::SerializedPolicyContainer>>());
    state.request_referrer = TRY(decoder.decode<Variant<WebView::UIReferrer, URL::URL>>());
    state.request_referrer_policy = TRY(decoder.decode<Web::ReferrerPolicy::ReferrerPolicy>());
    state.resource = TRY(decoder.decode<Variant<Empty, String, WebView::UIPostResource>>());
    state.reload_pending = TRY(decoder.decode<bool>());
    state.ever_populated = TRY(decoder.decode<bool>());
    state.navigable_target_name = TRY(decoder.decode<String>());
    state.nested_histories = TRY(decoder.decode<Vector<WebView::UINestedHistory>>());
    return state;
}

template<>
ErrorOr<void> IPC::encode(Encoder& encoder, WebView::UISessionHistoryEntry const& entry)
{
    TRY(encoder.encode(entry.id));
    TRY(encoder.encode(entry.step));
    TRY(encoder.encode(entry.url));
    TRY(encoder.encode(entry.document_state));
    TRY(encoder.encode(entry.classic_history_api_state));
    TRY(encoder.encode(entry.navigation_api_state));
    TRY(encoder.encode(entry.navigation_api_key));
    TRY(encoder.encode(entry.navigation_api_id));
    TRY(encoder.encode(entry.scroll_restoration_mode));
    TRY(encoder.encode(entry.policy_container));
    TRY(encoder.encode(entry.browsing_context_name));
    return {};
}

template<>
ErrorOr<WebView::UISessionHistoryEntry> IPC::decode(Decoder& decoder)
{
    WebView::UISessionHistoryEntry entry;
    entry.id = TRY(decoder.decode<u64>());
    entry.step = TRY(decoder.decode<Optional<int>>());
    entry.url = TRY(decoder.decode<URL::URL>());
    entry.document_state = TRY(decoder.decode<WebView::UIDocumentState>());
    entry.classic_history_api_state = TRY(decoder.decode<Web::HTML::SerializationRecord>());
    entry.navigation_api_state = TRY(decoder.decode<Web::HTML::SerializationRecord>());
    entry.navigation_api_key = TRY(decoder.decode<String>());
    entry.navigation_api_id = TRY(decoder.decode<String>());
    entry.scroll_restoration_mode = TRY(decoder.decode<WebView::UIScrollRestorationMode>());
    entry.policy_container = TRY(decoder.decode<Optional<Web::HTML::SerializedPolicyContainer>>());
    entry.browsing_context_name = TRY(decoder.decode<Optional<ByteString>>());
    return entry;
}

template<>
ErrorOr<void> IPC::encode(Encoder& encoder, WebView::UINestedHistory const& nested_history)
{
    TRY(encoder.encode(nested_history.navigable_id));
    TRY(encoder.encode(nested_history.entries));
    return {};
}

template<>
ErrorOr<WebView::UINestedHistory> IPC::decode(Decoder& decoder)
{
    auto navigable_id = TRY(decoder.decode<String>());
    auto entries = TRY(decoder.decode<Vector<WebView::UISessionHistoryEntry>>());
    return WebView::UINestedHistory { move(navigable_id), move(entries) };
}

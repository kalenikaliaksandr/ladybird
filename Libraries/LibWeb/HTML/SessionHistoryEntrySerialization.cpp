/*
 * Copyright (c) 2025, Aliaksandr Kalenik <kalenik.aliaksandr@gmail.com>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <LibWeb/HTML/PolicyContainers.h>
#include <LibWeb/HTML/SessionHistoryEntrySerialization.h>

namespace Web::HTML {

static WebView::UIPostResource serialize_post_resource(POSTResource const& resource)
{
    WebView::UIPostResource ui_resource;
    ui_resource.request_body = resource.request_body;
    ui_resource.request_content_type = static_cast<WebView::UIPostResource::RequestContentType>(resource.request_content_type);

    for (auto const& directive : resource.request_content_type_directives) {
        ui_resource.request_content_type_directives.append({
            .type = MUST(String::from_utf8(directive.type)),
            .value = directive.value,
        });
    }

    return ui_resource;
}

WebView::UIDocumentState serialize_document_state(DocumentState const& state)
{
    WebView::UIDocumentState ui_state;

    ui_state.document_id = state.document_id();
    ui_state.origin = state.origin();
    ui_state.initiator_origin = state.initiator_origin();
    ui_state.about_base_url = state.about_base_url();

    // history_policy_container: Variant<GC::Ref<PolicyContainer>, Client> -> Optional<SerializedPolicyContainer>
    state.history_policy_container().visit(
        [&](GC::Ref<PolicyContainer> const& policy_container) {
            ui_state.history_policy_container = policy_container->serialize();
        },
        [&](DocumentState::Client) {
            ui_state.history_policy_container = {};
        });

    // request_referrer: Variant<Referrer, URL::URL> -> Variant<UIReferrer, URL::URL>
    state.request_referrer().visit(
        [&](Fetch::Infrastructure::Request::Referrer referrer) {
            switch (referrer) {
            case Fetch::Infrastructure::Request::Referrer::NoReferrer:
                ui_state.request_referrer = WebView::UIReferrer::NoReferrer;
                break;
            case Fetch::Infrastructure::Request::Referrer::Client:
                ui_state.request_referrer = WebView::UIReferrer::Client;
                break;
            }
        },
        [&](URL::URL const& url) {
            ui_state.request_referrer = url;
        });

    ui_state.request_referrer_policy = state.request_referrer_policy();

    // resource: Variant<Empty, String, POSTResource> -> Variant<Empty, String, UIPostResource>
    state.resource().visit(
        [&](Empty) {
            ui_state.resource = Empty {};
        },
        [&](String const& url_string) {
            ui_state.resource = url_string;
        },
        [&](POSTResource const& post_resource) {
            ui_state.resource = serialize_post_resource(post_resource);
        });

    ui_state.reload_pending = state.reload_pending();
    ui_state.ever_populated = state.ever_populated();
    ui_state.navigable_target_name = state.navigable_target_name();

    for (auto const& nested_history : state.nested_histories()) {
        WebView::UINestedHistory ui_nested;
        ui_nested.navigable_id = nested_history.id;
        for (auto const& entry : nested_history.entries)
            ui_nested.entries.append(serialize_session_history_entry(entry));
        ui_state.nested_histories.append(move(ui_nested));
    }

    return ui_state;
}

WebView::UISessionHistoryEntry serialize_session_history_entry(SessionHistoryEntry const& entry)
{
    WebView::UISessionHistoryEntry ui_entry;

    // step: Variant<int, Pending> -> Optional<int>
    entry.step().visit(
        [&](int step) { ui_entry.step = step; },
        [&](SessionHistoryEntry::Pending) { ui_entry.step = {}; });

    ui_entry.url = entry.url();
    ui_entry.document_state = entry.document_state() ? serialize_document_state(*entry.document_state()) : WebView::UIDocumentState {};

    ui_entry.classic_history_api_state = entry.classic_history_api_state();
    ui_entry.navigation_api_state = entry.navigation_api_state();
    ui_entry.navigation_api_key = entry.navigation_api_key();
    ui_entry.navigation_api_id = entry.navigation_api_id();

    // ScrollRestorationMode -> UIScrollRestorationMode
    ui_entry.scroll_restoration_mode = static_cast<WebView::UIScrollRestorationMode>(entry.scroll_restoration_mode());

    if (entry.policy_container())
        ui_entry.policy_container = entry.policy_container()->serialize();

    ui_entry.browsing_context_name = entry.browsing_context_name();

    return ui_entry;
}

static POSTResource deserialize_post_resource(WebView::UIPostResource const& ui_resource)
{
    POSTResource resource;
    resource.request_body = ui_resource.request_body;
    resource.request_content_type = static_cast<POSTResource::RequestContentType>(ui_resource.request_content_type);

    // FIXME: POSTResource::Directive::type is a StringView (non-owning).
    //        We skip deserializing directives here because the StringView would dangle.
    //        When this deserialization path is actually used (Phase 3+), POSTResource::Directive::type
    //        should be changed to an owning String.
    (void)ui_resource.request_content_type_directives;

    return resource;
}

void apply_ui_document_state(WebView::UIDocumentState const& ui_state, DocumentState& state, GC::Heap& heap)
{
    state.set_document_id(ui_state.document_id);
    state.set_origin(ui_state.origin);
    state.set_initiator_origin(ui_state.initiator_origin);
    state.set_about_base_url(ui_state.about_base_url);

    // history_policy_container: Optional<SerializedPolicyContainer> -> Variant<GC::Ref<PolicyContainer>, Client>
    if (ui_state.history_policy_container.has_value())
        state.set_history_policy_container(create_a_policy_container_from_serialized_policy_container(heap, ui_state.history_policy_container.value()));
    else
        state.set_history_policy_container(DocumentState::Client::Tag);

    // request_referrer: Variant<UIReferrer, URL::URL> -> Variant<Referrer, URL::URL>
    ui_state.request_referrer.visit(
        [&](WebView::UIReferrer referrer) {
            switch (referrer) {
            case WebView::UIReferrer::NoReferrer:
                state.set_request_referrer(Fetch::Infrastructure::Request::Referrer::NoReferrer);
                break;
            case WebView::UIReferrer::Client:
                state.set_request_referrer(Fetch::Infrastructure::Request::Referrer::Client);
                break;
            }
        },
        [&](URL::URL const& url) {
            state.set_request_referrer(url);
        });

    state.set_request_referrer_policy(ui_state.request_referrer_policy);

    // resource: Variant<Empty, String, UIPostResource> -> Variant<Empty, String, POSTResource>
    ui_state.resource.visit(
        [&](Empty) { state.set_resource(Empty {}); },
        [&](String const& url_string) { state.set_resource(url_string); },
        [&](WebView::UIPostResource const& ui_post) { state.set_resource(deserialize_post_resource(ui_post)); });

    state.set_reload_pending(ui_state.reload_pending);
    state.set_ever_populated(ui_state.ever_populated);
    state.set_navigable_target_name(ui_state.navigable_target_name);
}

void apply_ui_session_history_entry(WebView::UISessionHistoryEntry const& ui_entry, SessionHistoryEntry& entry, GC::Heap& heap)
{
    // step: Optional<int> -> Variant<int, Pending>
    if (ui_entry.step.has_value())
        entry.set_step(ui_entry.step.value());
    else
        entry.set_step(SessionHistoryEntry::Pending::Tag);

    entry.set_url(ui_entry.url);

    if (entry.document_state())
        apply_ui_document_state(ui_entry.document_state, *entry.document_state(), heap);

    entry.set_classic_history_api_state(ui_entry.classic_history_api_state);
    entry.set_navigation_api_state(ui_entry.navigation_api_state);
    entry.set_navigation_api_key(ui_entry.navigation_api_key);
    entry.set_navigation_api_id(ui_entry.navigation_api_id);

    // UIScrollRestorationMode -> ScrollRestorationMode
    entry.set_scroll_restoration_mode(static_cast<ScrollRestorationMode>(ui_entry.scroll_restoration_mode));

    if (ui_entry.policy_container.has_value())
        entry.set_policy_container(create_a_policy_container_from_serialized_policy_container(heap, ui_entry.policy_container.value()));
    else
        entry.set_policy_container(nullptr);

    entry.set_browsing_context_name(ui_entry.browsing_context_name);
}

}

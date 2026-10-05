/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#include <AK/Utf16String.h>
#include <LibJS/Runtime/Date.h>
#include <LibJS/Runtime/Realm.h>
#include <LibJS/Runtime/VM.h>
#include <LibJS/Runtime/Value.h>
#include <LibJS/Script.h>
#include <LibTest/TestCase.h>
#include <LibUnicode/TimeZone.h>

// The system time zone that Date caches, which WebContent and the Internals object clear when the host's time zone
// changes. The same expectations hold for the C++ runtime's LibJS and for the facade over the Rust one.

using namespace JS;

static double offset_of_the_epoch_in_minutes(VM& vm, Realm& realm)
{
    auto source_text = "new Date(0).getTimezoneOffset()"_utf16;
    auto script = Script::parse(source_text.utf16_view(), realm);
    VERIFY(!script.is_error());
    return MUST(vm.run(script.value())).as_double();
}

TEST_CASE(clearing_the_cache_picks_up_a_new_time_zone)
{
    auto vm = VM::create();
    auto realm_execution_context = MUST(Realm::initialize_host_defined_realm(*vm, nullptr, nullptr));
    auto& realm = *realm_execution_context->realm;

    MUST(Unicode::set_current_time_zone(u"UTC"sv));
    clear_system_time_zone_cache();
    EXPECT_EQ(offset_of_the_epoch_in_minutes(*vm, realm), 0);

    MUST(Unicode::set_current_time_zone(u"America/New_York"sv));
    EXPECT_EQ(offset_of_the_epoch_in_minutes(*vm, realm), 0);
    clear_system_time_zone_cache();
    EXPECT_EQ(offset_of_the_epoch_in_minutes(*vm, realm), 300);

    MUST(Unicode::set_current_time_zone(u"UTC"sv));
    clear_system_time_zone_cache();
    EXPECT_EQ(offset_of_the_epoch_in_minutes(*vm, realm), 0);

    while (!vm->execution_context_stack().is_empty())
        vm->pop_execution_context();
}

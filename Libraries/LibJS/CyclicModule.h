/*
 * Copyright (c) 2022, David Tuin <davidot@serenityos.org>
 * Copyright (c) 2023, networkException <networkexception@serenityos.org>
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

#pragma once

#include <AK/Vector.h>
#include <LibJS/Embedding/Layout.h>
#include <LibJS/Export.h>
#include <LibJS/Forward.h>
#include <LibJS/Module.h>
#include <LibJS/Runtime/ModuleRequest.h>

namespace JS {

// 16.2.1.5 Cyclic Module Records, https://tc39.es/ecma262/#cyclic-module-record
class JS_API CyclicModule : public Module {
public:
    static bool is_engine_class_of(Module const& module)
    {
        auto class_id = module.engine_class_id();
        return class_id == JS_LAYOUT_CLASS_ID_SOURCE_TEXT_MODULE || class_id == JS_LAYOUT_CLASS_ID_HOST_MODULE;
    }

    Vector<ModuleRequest> const& requested_modules() const;

protected:
    // 16.2.1.7 GetImportedModule ( referrer, request ), https://tc39.es/ecma262/#sec-GetImportedModule
    [[nodiscard]] GC::Ref<Module> get_imported_module(ModuleRequest const& request);
};

}

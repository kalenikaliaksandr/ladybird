/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! The parts of Libraries/LibJS/Runtime/Temporal/PlainMonthDay.cpp the Temporal foundation needs: the
//! Temporal.PlainMonthDay object and CreateTemporalMonthDay. Creating a Temporal.PlainMonthDay needs its constructor,
//! which comes with the Temporal.PlainMonthDay builtins, as do the other operations of PlainMonthDay.cpp.

use ak::Utf16String;
use libjs_runtime_macros::Trace;

use crate::gc::class::{GcCell, define_cell};
use crate::interpreter::vm::Vm;
use crate::layout::cell::Gc;
use crate::layout::function_object::FunctionObject;
use crate::layout::object::Object;
use crate::runtime::abstract_operations::ordinary_create_from_constructor_of;
use crate::runtime::completion::ThrowCompletionOr;
use crate::runtime::error::ErrorKind;
use crate::runtime::error_types::ErrorType;
use crate::runtime::intrinsics::Intrinsics;
use crate::runtime::object::MayInterfereWithIndexedPropertyAccess;
use crate::runtime::temporal::iso_records::ISODate;
use crate::runtime::temporal::plain_date::iso_date_within_limits;

#[repr(C)]
#[derive(Trace)]
pub struct PlainMonthDay {
    base: Object,
    #[gc(untraced)]
    iso_date: ISODate, // [[ISODate]]
    calendar: Utf16String, // [[Calendar]]
}

define_cell!(PlainMonthDay, Object, extends: [Object]);

impl core::ops::Deref for PlainMonthDay {
    type Target = Object;

    fn deref(&self) -> &Object {
        &self.base
    }
}

impl PlainMonthDay {
    fn new(vm: &Vm, iso_date: ISODate, calendar: Utf16String, prototype: Gc<Object>) -> PlainMonthDay {
        PlainMonthDay {
            base: Object::new_with_prototype(vm, Self::CLASS, prototype, MayInterfereWithIndexedPropertyAccess::No),
            iso_date,
            calendar,
        }
    }

    pub fn iso_date(&self) -> ISODate {
        self.iso_date
    }

    pub fn calendar(&self) -> Utf16String {
        self.calendar.clone()
    }
}

// 10.5.2 CreateTemporalMonthDay ( isoDate, calendar [ , newTarget ] ), https://tc39.es/proposal-temporal/#sec-temporal-createtemporalmonthday
pub fn create_temporal_month_day(
    vm: &Vm,
    iso_date: ISODate,
    calendar: Utf16String,
    new_target: Option<Gc<FunctionObject>>,
) -> ThrowCompletionOr<Gc<PlainMonthDay>> {
    let realm = vm.current_realm().expect("CreateTemporalMonthDay runs in a realm");

    // 1. If ISODateWithinLimits(isoDate) is false, throw a RangeError exception.
    if !iso_date_within_limits(iso_date) {
        return vm.throw_completion(ErrorKind::RangeError, ErrorType::TemporalInvalidPlainMonthDay, &[]);
    }

    // 2. If newTarget is not present, set newTarget to %Temporal.PlainMonthDay%.
    let new_target = new_target.unwrap_or_else(|| realm.intrinsics().temporal_plain_month_day_constructor(vm));

    // 3. Let object be ? OrdinaryCreateFromConstructor(newTarget, "%Temporal.PlainMonthDay.prototype%", « [[InitializedTemporalMonthDay]], [[ISODate]], [[Calendar]] »).
    // 4. Set object.[[ISODate]] to isoDate.
    // 5. Set object.[[Calendar]] to calendar.
    let object = ordinary_create_from_constructor_of(
        vm,
        realm,
        new_target,
        Intrinsics::temporal_plain_month_day_prototype,
        |prototype| PlainMonthDay::new(vm, iso_date, calendar, prototype),
    )?;

    // 6. Return object.
    Ok(object)
}

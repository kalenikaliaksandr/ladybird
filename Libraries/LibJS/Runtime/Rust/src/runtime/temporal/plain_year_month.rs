/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! The parts of Libraries/LibJS/Runtime/Temporal/PlainYearMonth.cpp the Temporal foundation needs: the
//! Temporal.PlainYearMonth object, the ISO Year-Month Record operations, and CreateTemporalYearMonth. Creating a
//! Temporal.PlainYearMonth needs its constructor, which comes with the Temporal.PlainYearMonth builtins, as do the
//! other operations of PlainYearMonth.cpp.

use ak::Utf16String;
use libjs_runtime_macros::Trace;

use crate::gc::class::{GcCell, define_cell};
use crate::interpreter::vm::Vm;
use crate::layout::cell::Gc;
use crate::layout::function_object::FunctionObject;
use crate::layout::object::Object;
use crate::runtime::abstract_operations::{modulo, ordinary_create_from_constructor_of};
use crate::runtime::completion::ThrowCompletionOr;
use crate::runtime::error::ErrorKind;
use crate::runtime::error_types::ErrorType;
use crate::runtime::intrinsics::Intrinsics;
use crate::runtime::object::MayInterfereWithIndexedPropertyAccess;
use crate::runtime::temporal::iso_records::{ISODate, ISOYearMonth};

#[repr(C)]
#[derive(Trace)]
pub struct PlainYearMonth {
    base: Object,
    #[gc(untraced)]
    iso_date: ISODate, // [[ISODate]]
    calendar: Utf16String, // [[Calendar]]
}

define_cell!(PlainYearMonth, Object, extends: [Object]);

impl core::ops::Deref for PlainYearMonth {
    type Target = Object;

    fn deref(&self) -> &Object {
        &self.base
    }
}

impl PlainYearMonth {
    fn new(vm: &Vm, iso_date: ISODate, calendar: Utf16String, prototype: Gc<Object>) -> PlainYearMonth {
        PlainYearMonth {
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

// 9.5.3 ISOYearMonthWithinLimits ( isoDate ), https://tc39.es/proposal-temporal/#sec-temporal-isoyearmonthwithinlimits
pub fn iso_year_month_within_limits(iso_date: ISODate) -> bool {
    // 1. If isoDate.[[Year]] < -271821 or isoDate.[[Year]] > 275760, return false.
    if iso_date.year < -271_821 || iso_date.year > 275_760 {
        return false;
    }

    // 2. If isoDate.[[Year]] = -271821 and isoDate.[[Month]] < 4, return false.
    if iso_date.year == -271_821 && iso_date.month < 4 {
        return false;
    }

    // 3. If isoDate.[[Year]] = 275760 and isoDate.[[Month]] > 9, return false.
    if iso_date.year == 275_760 && iso_date.month > 9 {
        return false;
    }

    // 4. Return true.
    true
}

// 9.5.4 BalanceISOYearMonth ( year, month ), https://tc39.es/proposal-temporal/#sec-temporal-balanceisoyearmonth
pub fn balance_iso_year_month(mut year: f64, mut month: f64) -> ISOYearMonth {
    // 1. Set year to year + floor((month - 1) / 12).
    year += ((month - 1.0) / 12.0).floor();

    // 2. Set month to ((month - 1) modulo 12) + 1.
    month = modulo(month - 1.0, 12.0) + 1.0;

    // 3. Return ISO Year-Month Record { [[Year]]: year, [[Month]]: month  }.
    ISOYearMonth {
        year: year as i32,
        month: month as u8,
    }
}

// 9.5.5 CreateTemporalYearMonth ( isoDate, calendar [ , newTarget ] ), https://tc39.es/proposal-temporal/#sec-temporal-createtemporalyearmonth
pub fn create_temporal_year_month(
    vm: &Vm,
    iso_date: ISODate,
    calendar: Utf16String,
    new_target: Option<Gc<FunctionObject>>,
) -> ThrowCompletionOr<Gc<PlainYearMonth>> {
    let realm = vm.current_realm().expect("CreateTemporalYearMonth runs in a realm");

    // 1. If ISOYearMonthWithinLimits(isoDate) is false, throw a RangeError exception.
    if !iso_year_month_within_limits(iso_date) {
        return vm.throw_completion(ErrorKind::RangeError, ErrorType::TemporalInvalidPlainYearMonth, &[]);
    }

    // 2. If newTarget is not present, set newTarget to %Temporal.PlainYearMonth%.
    let new_target = new_target.unwrap_or_else(|| realm.intrinsics().temporal_plain_year_month_constructor(vm));

    // 3. Let object be ? OrdinaryCreateFromConstructor(newTarget, "%Temporal.PlainYearMonth.prototype%", « [[InitializedTemporalYearMonth]], [[ISODate]], [[Calendar]] »).
    // 4. Set object.[[ISODate]] to isoDate.
    // 5. Set object.[[Calendar]] to calendar.
    let object = ordinary_create_from_constructor_of(
        vm,
        realm,
        new_target,
        Intrinsics::temporal_plain_year_month_prototype,
        |prototype| PlainYearMonth::new(vm, iso_date, calendar, prototype),
    )?;

    // 6. Return object.
    Ok(object)
}

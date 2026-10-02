/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! The parts of Libraries/LibJS/Runtime/Temporal/ZonedDateTime.cpp the Temporal foundation needs: the
//! Temporal.ZonedDateTime object, the epoch nanosecond arithmetic in a time zone, and CreateTemporalZonedDateTime.
//! Creating a Temporal.ZonedDateTime needs its constructor, which comes with the Temporal.ZonedDateTime builtins, as do
//! the other operations of ZonedDateTime.cpp.

use ak::Utf16String;
use libjs_runtime_macros::Trace;

use crate::gc::class::{GcCell, define_cell};
use crate::interpreter::vm::Vm;
use crate::layout::cell::Gc;
use crate::layout::function_object::FunctionObject;
use crate::layout::object::Object;
use crate::runtime::abstract_operations::{RoundingMode, ordinary_create_from_constructor_of};
use crate::runtime::big_fraction::BigFraction;
use crate::runtime::big_int::{BigInt, SignedBigInteger};
use crate::runtime::big_int_algorithms::{CompareResult, compare_to_double};
use crate::runtime::completion::{Must, ThrowCompletionOr};
use crate::runtime::date::get_utc_epoch_nanoseconds;
use crate::runtime::error::ErrorKind;
use crate::runtime::error_types::ErrorType;
use crate::runtime::intrinsics::Intrinsics;
use crate::runtime::object::MayInterfereWithIndexedPropertyAccess;
use crate::runtime::temporal::abstract_operations::{
    Disambiguation, OffsetOption, Overflow, Unit, UnitCategory, check_iso_days_range, larger_of_two_temporal_units,
    round_big_number_to_increment, temporal_unit_category,
};
use crate::runtime::temporal::calendar::{calendar_date_add, calendar_date_until};
use crate::runtime::temporal::duration::{
    InternalDuration, combine_date_and_time_duration, date_duration_sign, round_relative_duration,
    time_duration_from_epoch_nanoseconds_difference, time_duration_sign, total_relative_duration, total_time_duration,
    zero_date_duration,
};
use crate::runtime::temporal::instant::{
    NANOSECONDS_PER_MINUTE, add_instant, difference_instant, is_valid_epoch_nanoseconds,
};
use crate::runtime::temporal::iso_records::{ISODate, ISODateTime, TimeDuration, TimeOrStartOfDay};
use crate::runtime::temporal::plain_date::{add_days_to_iso_date, compare_iso_date};
use crate::runtime::temporal::plain_date_time::{
    balance_iso_date_time, combine_iso_date_and_time_record, iso_date_time_within_limits,
};
use crate::runtime::temporal::plain_time::difference_time;
use crate::runtime::temporal::time_zone::{
    disambiguate_possible_epoch_nanoseconds, get_epoch_nanoseconds_for, get_iso_date_time_for,
    get_possible_epoch_nanoseconds, get_start_of_day,
};
use crate::utf16::Utf16View;

// 6 Temporal.ZonedDateTime Objects, https://tc39.es/proposal-temporal/#sec-temporal-zoneddatetime-objects
#[repr(C)]
#[derive(Trace)]
pub struct ZonedDateTime {
    base: Object,
    epoch_nanoseconds: Gc<BigInt>, // [[EpochNanoseconds]]
    time_zone: Utf16String,        // [[TimeZone]]
    calendar: Utf16String,         // [[Calendar]]
}

define_cell!(ZonedDateTime, Object, extends: [Object]);

impl core::ops::Deref for ZonedDateTime {
    type Target = Object;

    fn deref(&self) -> &Object {
        &self.base
    }
}

impl ZonedDateTime {
    fn new(
        vm: &Vm,
        epoch_nanoseconds: Gc<BigInt>,
        time_zone: Utf16String,
        calendar: Utf16String,
        prototype: Gc<Object>,
    ) -> ZonedDateTime {
        ZonedDateTime {
            base: Object::new_with_prototype(vm, Self::CLASS, prototype, MayInterfereWithIndexedPropertyAccess::No),
            epoch_nanoseconds,
            time_zone,
            calendar,
        }
    }

    pub fn epoch_nanoseconds(&self) -> Gc<BigInt> {
        self.epoch_nanoseconds
    }

    pub fn time_zone(&self) -> Utf16String {
        self.time_zone.clone()
    }

    pub fn calendar(&self) -> Utf16String {
        self.calendar.clone()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OffsetBehavior {
    Option,
    Exact,
    Wall,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MatchBehavior {
    MatchExactly,
    MatchMinutes,
}

// 6.5.1 InterpretISODateTimeOffset ( isoDate, time, offsetBehaviour, offsetNanoseconds, timeZone, disambiguation, offsetOption, matchBehaviour ), https://tc39.es/proposal-temporal/#sec-temporal-interpretisodatetimeoffset
#[allow(clippy::too_many_arguments)]
pub fn interpret_iso_date_time_offset(
    vm: &Vm,
    iso_date: ISODate,
    time_or_start_of_day: TimeOrStartOfDay,
    offset_behavior: OffsetBehavior,
    offset_nanoseconds: f64,
    time_zone: Utf16View<'_>,
    disambiguation: Disambiguation,
    offset_option: OffsetOption,
    match_behavior: MatchBehavior,
) -> ThrowCompletionOr<SignedBigInteger> {
    // 1. If time is START-OF-DAY, then
    let TimeOrStartOfDay::Time(time) = time_or_start_of_day else {
        // a. Assert: offsetBehaviour is WALL.
        assert!(offset_behavior == OffsetBehavior::Wall);

        // b. Assert: offsetNanoseconds = 0.
        assert!(offset_nanoseconds == 0.0);

        // c. Return ? GetStartOfDay(timeZone, isoDate).
        return get_start_of_day(vm, time_zone, iso_date);
    };

    // 2. Let isoDateTime be CombineISODateAndTimeRecord(isoDate, time).
    let iso_date_time = combine_iso_date_and_time_record(iso_date, time);

    // 3. If offsetBehaviour is WALL, or offsetBehaviour is OPTION and offsetOption is IGNORE, then
    if offset_behavior == OffsetBehavior::Wall
        || (offset_behavior == OffsetBehavior::Option && offset_option == OffsetOption::Ignore)
    {
        // a. Return ? GetEpochNanosecondsFor(timeZone, isoDateTime, disambiguation).
        return get_epoch_nanoseconds_for(vm, time_zone, &iso_date_time, disambiguation);
    }

    // 4. If offsetBehaviour is EXACT, or offsetBehaviour is OPTION and offsetOption is USE, then
    if offset_behavior == OffsetBehavior::Exact
        || (offset_behavior == OffsetBehavior::Option && offset_option == OffsetOption::Use)
    {
        // a. Let balanced be BalanceISODateTime(isoDate.[[Year]], isoDate.[[Month]], isoDate.[[Day]], time.[[Hour]], time.[[Minute]], time.[[Second]], time.[[Millisecond]], time.[[Microsecond]], time.[[Nanosecond]] - offsetNanoseconds).
        let balanced = balance_iso_date_time(
            f64::from(iso_date.year),
            f64::from(iso_date.month),
            f64::from(iso_date.day),
            f64::from(time.hour),
            f64::from(time.minute),
            f64::from(time.second),
            f64::from(time.millisecond),
            f64::from(time.microsecond),
            f64::from(time.nanosecond) - offset_nanoseconds,
        );

        // b. Perform ? CheckISODaysRange(balanced.[[ISODate]]).
        check_iso_days_range(vm, balanced.iso_date)?;

        // c. Let epochNanoseconds be GetUTCEpochNanoseconds(balanced).
        let epoch_nanoseconds = get_utc_epoch_nanoseconds(&balanced);

        // d. If IsValidEpochNanoseconds(epochNanoseconds) is false, throw a RangeError exception.
        if !is_valid_epoch_nanoseconds(&epoch_nanoseconds) {
            return vm.throw_completion(ErrorKind::RangeError, ErrorType::TemporalInvalidEpochNanoseconds, &[]);
        }

        // e. Return epochNanoseconds.
        return Ok(epoch_nanoseconds);
    }

    // 5. Assert: offsetBehaviour is OPTION.
    assert!(offset_behavior == OffsetBehavior::Option);

    // 6. Assert: offsetOption is PREFER or REJECT.
    assert!(offset_option == OffsetOption::Prefer || offset_option == OffsetOption::Reject);

    // 7. Perform ? CheckISODaysRange(isoDate).
    check_iso_days_range(vm, iso_date)?;

    // 8. Let utcEpochNanoseconds be GetUTCEpochNanoseconds(isoDateTime).
    let utc_epoch_nanoseconds = get_utc_epoch_nanoseconds(&iso_date_time);

    // 9. Let possibleEpochNs be ? GetPossibleEpochNanoseconds(timeZone, isoDateTime).
    let possible_epoch_nanoseconds = get_possible_epoch_nanoseconds(vm, time_zone, &iso_date_time)?;

    // 10. For each element candidate of possibleEpochNs, do
    for candidate in &possible_epoch_nanoseconds {
        // a. Let candidateOffset be utcEpochNanoseconds - candidate.
        let candidate_offset = &utc_epoch_nanoseconds - candidate;

        // b. If candidateOffset = offsetNanoseconds, return candidate.
        if compare_to_double(&candidate_offset, offset_nanoseconds) == CompareResult::DoubleEqualsBigInt {
            return Ok(candidate.clone());
        }

        // c. If matchBehaviour is MATCH-MINUTES, then
        if match_behavior == MatchBehavior::MatchMinutes {
            // i. Let roundedCandidateNanoseconds be RoundNumberToIncrement(candidateOffset, 60 × 10**9, HALF-EXPAND).
            let rounded_candidate_nanoseconds =
                round_big_number_to_increment(&candidate_offset, &NANOSECONDS_PER_MINUTE, RoundingMode::HalfExpand);

            // ii. If roundedCandidateNanoseconds = offsetNanoseconds, return candidate.
            if compare_to_double(&rounded_candidate_nanoseconds, offset_nanoseconds)
                == CompareResult::DoubleEqualsBigInt
            {
                return Ok(candidate.clone());
            }
        }
    }

    // 11. If offsetOption is reject, throw a RangeError exception.
    if offset_option == OffsetOption::Reject {
        return vm.throw_completion(
            ErrorKind::RangeError,
            ErrorType::TemporalInvalidZonedDateTimeOffset,
            &[],
        );
    }

    // 12. Return ? DisambiguatePossibleEpochNanoseconds(possibleEpochNs, timeZone, isoDateTime, disambiguation).
    disambiguate_possible_epoch_nanoseconds(
        vm,
        possible_epoch_nanoseconds,
        time_zone,
        &iso_date_time,
        disambiguation,
    )
}

// 6.5.3 CreateTemporalZonedDateTime ( epochNanoseconds, timeZone, calendar [ , newTarget ] ), https://tc39.es/proposal-temporal/#sec-temporal-createtemporalzoneddatetime
pub fn create_temporal_zoned_date_time(
    vm: &Vm,
    epoch_nanoseconds: Gc<BigInt>,
    time_zone: Utf16String,
    calendar: Utf16String,
    new_target: Option<Gc<FunctionObject>>,
) -> ThrowCompletionOr<Gc<ZonedDateTime>> {
    let realm = vm.current_realm().expect("CreateTemporalZonedDateTime runs in a realm");

    // 1. Assert: IsValidEpochNanoseconds(epochNanoseconds) is true.
    assert!(is_valid_epoch_nanoseconds(epoch_nanoseconds.big_integer()));

    // 2. If newTarget is not present, set newTarget to %Temporal.ZonedDateTime%.
    let new_target = new_target.unwrap_or_else(|| realm.intrinsics().temporal_zoned_date_time_constructor(vm));

    // 3. Let object be ? OrdinaryCreateFromConstructor(newTarget, "%Temporal.ZonedDateTime.prototype%", « [[InitializedTemporalZonedDateTime]], [[EpochNanoseconds]], [[TimeZone]], [[Calendar]] »).
    // 4. Set object.[[EpochNanoseconds]] to epochNanoseconds.
    // 5. Set object.[[TimeZone]] to timeZone.
    // 6. Set object.[[Calendar]] to calendar.
    let object = ordinary_create_from_constructor_of(
        vm,
        realm,
        new_target,
        Intrinsics::temporal_zoned_date_time_prototype,
        |prototype| ZonedDateTime::new(vm, epoch_nanoseconds, time_zone, calendar, prototype),
    )?;

    // 7. Return object.
    Ok(object)
}

// 6.5.5 AddZonedDateTime ( epochNanoseconds, timeZone, calendar, duration, overflow ), https://tc39.es/proposal-temporal/#sec-temporal-addzoneddatetime
pub fn add_zoned_date_time(
    vm: &Vm,
    epoch_nanoseconds: &SignedBigInteger,
    time_zone: Utf16View<'_>,
    calendar: Utf16View<'_>,
    duration: &InternalDuration,
    overflow: Overflow,
) -> ThrowCompletionOr<SignedBigInteger> {
    // 1. If DateDurationSign(duration.[[Date]]) = 0, return ? AddInstant(epochNanoseconds, duration.[[Time]]).
    if date_duration_sign(&duration.date) == 0 {
        return add_instant(vm, epoch_nanoseconds, &duration.time);
    }

    // 2. Let isoDateTime be GetISODateTimeFor(timeZone, epochNanoseconds).
    let iso_date_time = get_iso_date_time_for(time_zone, epoch_nanoseconds);

    // 3. Let addedDate be ? CalendarDateAdd(calendar, isoDateTime.[[ISODate]], duration.[[Date]], overflow).
    let added_date = calendar_date_add(vm, calendar, iso_date_time.iso_date, &duration.date, overflow)?;

    // 4. Let intermediateDateTime be CombineISODateAndTimeRecord(addedDate, isoDateTime.[[Time]]).
    let intermediate_date_time = combine_iso_date_and_time_record(added_date, iso_date_time.time);

    // 5. If ISODateTimeWithinLimits(intermediateDateTime) is false, throw a RangeError exception.
    if !iso_date_time_within_limits(&intermediate_date_time) {
        return vm.throw_completion(ErrorKind::RangeError, ErrorType::TemporalInvalidISODateTime, &[]);
    }

    // 6. Let intermediateNs be ! GetEpochNanosecondsFor(timeZone, intermediateDateTime, COMPATIBLE).
    let intermediate_nanoseconds =
        get_epoch_nanoseconds_for(vm, time_zone, &intermediate_date_time, Disambiguation::Compatible).must();

    // 7. Return ? AddInstant(intermediateNs, duration.[[Time]]).
    add_instant(vm, &intermediate_nanoseconds, &duration.time)
}

// 6.5.6 DifferenceZonedDateTime ( ns1, ns2, timeZone, calendar, largestUnit ), https://tc39.es/proposal-temporal/#sec-temporal-differencezoneddatetime
pub fn difference_zoned_date_time(
    vm: &Vm,
    nanoseconds1: &SignedBigInteger,
    nanoseconds2: &SignedBigInteger,
    time_zone: Utf16View<'_>,
    calendar: Utf16View<'_>,
    largest_unit: Unit,
) -> ThrowCompletionOr<InternalDuration> {
    // 1. If ns1 = ns2, return CombineDateAndTimeDuration(ZeroDateDuration(), 0).
    if nanoseconds1 == nanoseconds2 {
        return Ok(combine_date_and_time_duration(
            zero_date_duration(vm),
            TimeDuration::default(),
        ));
    }

    // 2. Let startDateTime be GetISODateTimeFor(timeZone, ns1).
    let start_date_time = get_iso_date_time_for(time_zone, nanoseconds1);

    // 3. Let endDateTime be GetISODateTimeFor(timeZone, ns2).
    let end_date_time = get_iso_date_time_for(time_zone, nanoseconds2);

    // 4. If CompareISODate(startDateTime.[[ISODate]], endDateTime.[[ISODate]]) = 0, then
    if compare_iso_date(start_date_time.iso_date, end_date_time.iso_date) == 0 {
        // a. Let timeDuration be TimeDurationFromEpochNanosecondsDifference(ns2, ns1).
        let time_duration = time_duration_from_epoch_nanoseconds_difference(nanoseconds2, nanoseconds1);

        // b. Return CombineDateAndTimeDuration(ZeroDateDuration(), timeDuration).
        return Ok(combine_date_and_time_duration(zero_date_duration(vm), time_duration));
    }

    // 5. If ns2 - ns1 < 0, let sign be 1; else let sign be -1.
    let sign: f64 = if nanoseconds2 < nanoseconds1 { 1.0 } else { -1.0 };

    // 6. If sign = -1, let maxDayCorrection be 2; else let maxDayCorrection be 1.
    let max_day_correction = if sign == -1.0 { 2.0 } else { 1.0 };

    // 7. Let dayCorrection be 0.
    let mut day_correction = 0.0;

    // 8. Let timeDuration be DifferenceTime(startDateTime.[[Time]], endDateTime.[[Time]]).
    let mut time_duration = difference_time(&start_date_time.time, &end_date_time.time);

    // 9. If TimeDurationSign(timeDuration) = sign, set dayCorrection to dayCorrection + 1.
    if f64::from(time_duration_sign(&time_duration)) == sign {
        day_correction += 1.0;
    }

    // 10. Let success be false.
    let mut success = false;

    let mut intermediate_date_time = ISODateTime::default();

    // 11. Repeat, while dayCorrection ≤ maxDayCorrection and success is false,
    while day_correction <= max_day_correction && !success {
        // a. Let intermediateDate be AddDaysToISODate(endDateTime.[[ISODate]], dayCorrection × sign).
        let intermediate_date = add_days_to_iso_date(end_date_time.iso_date, day_correction * sign);

        // b. Let intermediateDateTime be CombineISODateAndTimeRecord(intermediateDate, startDateTime.[[Time]]).
        intermediate_date_time = combine_iso_date_and_time_record(intermediate_date, start_date_time.time);

        // c. Let intermediateNs be ? GetEpochNanosecondsFor(timeZone, intermediateDateTime, COMPATIBLE).
        let intermediate_nanoseconds =
            get_epoch_nanoseconds_for(vm, time_zone, &intermediate_date_time, Disambiguation::Compatible)?;

        // d. Set timeDuration to TimeDurationFromEpochNanosecondsDifference(ns2, intermediateNs).
        time_duration = time_duration_from_epoch_nanoseconds_difference(nanoseconds2, &intermediate_nanoseconds);

        // e. Let timeSign be TimeDurationSign(timeDuration).
        let time_sign = f64::from(time_duration_sign(&time_duration));

        // f. If sign ≠ timeSign, then
        if sign != time_sign {
            // i. Set success to true.
            success = true;
        }

        // g. Set dayCorrection to dayCorrection + 1.
        day_correction += 1.0;
    }

    // 12. Assert: success is true.
    assert!(success);

    // 13. Let dateLargestUnit be LargerOfTwoTemporalUnits(largestUnit, DAY).
    let date_largest_unit = larger_of_two_temporal_units(largest_unit, Unit::Day);

    // 14. Let dateDifference be CalendarDateUntil(calendar, startDateTime.[[ISODate]], intermediateDateTime.[[ISODate]], dateLargestUnit).
    let date_difference = calendar_date_until(
        vm,
        calendar,
        start_date_time.iso_date,
        intermediate_date_time.iso_date,
        date_largest_unit,
    );

    // 15. Return CombineDateAndTimeDuration(dateDifference, timeDuration).
    Ok(combine_date_and_time_duration(date_difference, time_duration))
}

// 6.5.7 DifferenceZonedDateTimeWithRounding ( ns1, ns2, timeZone, calendar, largestUnit, roundingIncrement, smallestUnit, roundingMode ), https://tc39.es/proposal-temporal/#sec-temporal-differencezoneddatetimewithrounding
#[allow(clippy::too_many_arguments)]
pub fn difference_zoned_date_time_with_rounding(
    vm: &Vm,
    nanoseconds1: &SignedBigInteger,
    nanoseconds2: &SignedBigInteger,
    time_zone: Utf16View<'_>,
    calendar: Utf16View<'_>,
    largest_unit: Unit,
    rounding_increment: u64,
    smallest_unit: Unit,
    rounding_mode: RoundingMode,
) -> ThrowCompletionOr<InternalDuration> {
    // 1. If TemporalUnitCategory(largestUnit) is TIME, return DifferenceInstant(ns1, ns2, roundingIncrement, smallestUnit, roundingMode).
    if temporal_unit_category(largest_unit) == UnitCategory::Time {
        return Ok(difference_instant(
            vm,
            nanoseconds1,
            nanoseconds2,
            rounding_increment,
            smallest_unit,
            rounding_mode,
        ));
    }

    // 2. Let difference be ? DifferenceZonedDateTime(ns1, ns2, timeZone, calendar, largestUnit).
    let difference = difference_zoned_date_time(vm, nanoseconds1, nanoseconds2, time_zone, calendar, largest_unit)?;

    // 3. If smallestUnit is NANOSECOND and roundingIncrement = 1, return difference.
    if smallest_unit == Unit::Nanosecond && rounding_increment == 1 {
        return Ok(difference);
    }

    // 4. Let dateTime be GetISODateTimeFor(timeZone, ns1).
    let date_time = get_iso_date_time_for(time_zone, nanoseconds1);

    // 5. Return ? RoundRelativeDuration(difference, ns1, ns2, dateTime, timeZone, calendar, largestUnit, roundingIncrement, smallestUnit, roundingMode).
    round_relative_duration(
        vm,
        difference,
        nanoseconds1,
        nanoseconds2,
        &date_time,
        Some(time_zone),
        calendar,
        largest_unit,
        rounding_increment,
        smallest_unit,
        rounding_mode,
    )
}

// 6.5.8 DifferenceZonedDateTimeWithTotal ( ns1, ns2, timeZone, calendar, unit ), https://tc39.es/proposal-temporal/#sec-temporal-differencezoneddatetimewithtotal
pub fn difference_zoned_date_time_with_total(
    vm: &Vm,
    nanoseconds1: &SignedBigInteger,
    nanoseconds2: &SignedBigInteger,
    time_zone: Utf16View<'_>,
    calendar: Utf16View<'_>,
    unit: Unit,
) -> ThrowCompletionOr<BigFraction> {
    // 1. If TemporalUnitCategory(unit) is TIME, then
    if temporal_unit_category(unit) == UnitCategory::Time {
        // a. Let difference be TimeDurationFromEpochNanosecondsDifference(ns2, ns1).
        let difference = time_duration_from_epoch_nanoseconds_difference(nanoseconds2, nanoseconds1);

        // b. Return TotalTimeDuration(difference, unit).
        return Ok(total_time_duration(&difference, unit));
    }

    // 2. Let difference be ? DifferenceZonedDateTime(ns1, ns2, timeZone, calendar, unit).
    let difference = difference_zoned_date_time(vm, nanoseconds1, nanoseconds2, time_zone, calendar, unit)?;

    // 3. Let dateTime be GetISODateTimeFor(timeZone, ns1).
    let date_time = get_iso_date_time_for(time_zone, nanoseconds1);

    // 4. Return ? TotalRelativeDuration(difference, ns1, ns2, dateTime, timeZone, calendar, unit).
    total_relative_duration(
        vm,
        &difference,
        nanoseconds1,
        nanoseconds2,
        &date_time,
        Some(time_zone),
        calendar,
        unit,
    )
}

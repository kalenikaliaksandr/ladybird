/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

use ak::Utf16String;
use libjs_runtime_macros::Trace;
use num_traits::FromPrimitive;

use crate::gc::class::{GcCell, define_cell};
use crate::interpreter::runtime_functions::unimplemented_runtime_function;
use crate::interpreter::vm::Vm;
use crate::layout::cell::{CellHeader, Gc};
use crate::layout::value::Value;
use crate::runtime::completion::{Throw, ThrowCompletionOr};

/// The arbitrary-precision integer a BigInt holds, in place of Crypto::SignedBigInteger.
pub use num_bigint::BigInt as SignedBigInteger;

#[repr(C)]
#[derive(Trace)]
pub struct BigInt {
    header: CellHeader,
    big_integer: SignedBigInteger,
}

define_cell!(BigInt, BigInt);

impl BigInt {
    fn new(big_integer: SignedBigInteger) -> Self {
        Self {
            header: CellHeader::for_class(Self::CLASS),
            big_integer,
        }
    }

    pub fn create(vm: &Vm, big_integer: SignedBigInteger) -> Gc<BigInt> {
        vm.heap().allocate(Self::new(big_integer))
    }

    pub fn big_integer(&self) -> &SignedBigInteger {
        &self.big_integer
    }

    pub fn to_utf16_string(&self) -> Utf16String {
        Utf16String::from_utf8(&format!("{}n", self.big_integer))
    }
}

fn throw_range_error_big_int_from_non_integral() -> Throw {
    unimplemented_runtime_function("number_to_bigint: RangeError (ErrorType::BigIntFromNonIntegral)", 0)
}

// 21.2.1.1.1 NumberToBigInt ( number ), https://tc39.es/ecma262/#sec-numbertobigint
pub fn number_to_bigint(vm: &Vm, number: Value) -> ThrowCompletionOr<Gc<BigInt>> {
    assert!(number.is_number());

    // 1. If IsIntegralNumber(number) is false, throw a RangeError exception.
    if !number.is_integral_number() {
        return Err(throw_range_error_big_int_from_non_integral());
    }

    // 2. Return the BigInt value that represents ℝ(number).
    let big_integer = SignedBigInteger::from_f64(number.as_f64()).expect("an integral number is finite");
    Ok(BigInt::create(vm, big_integer))
}

#[cfg(all(test, libjs_runtime_tests_with_libgc))]
mod tests {
    use super::*;
    use crate::utf16::Utf16View;

    #[test]
    fn integral_numbers_convert_exactly() {
        let vm = Vm::create();
        let cases: [(f64, &str); 6] = [
            (0.0, "0n"),
            (-0.0, "0n"),
            (-123.0, "-123n"),
            (9007199254740993.0, "9007199254740992n"),
            (1e21, "1000000000000000000000n"),
            (-(2f64.powi(80)), "-1208925819614629174706176n"),
        ];
        for (number, expected) in cases {
            let big_int = number_to_bigint(&vm, Value::from_f64(number)).unwrap_or_else(|_| unreachable!());
            vm.heap().collect_garbage();
            assert_eq!(Utf16View::of_string(&big_int.to_utf16_string()), expected);
        }
        let value = Value::from_bigint(BigInt::create(&vm, SignedBigInteger::from(42)));
        assert!(value.is_bigint());
        assert_eq!(*value.as_bigint().big_integer(), SignedBigInteger::from(42));
    }
}

/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! BigInt values. A magnitude crosses as 32-bit words, least significant first, which is the word order of
//! Crypto::UnsignedBigInteger's words() and of its constructor from them. Every function runs on the thread that owns
//! the VM.

use num_bigint::Sign;

use crate::embedding::abi_types::{
    JSBigInt, JSOwnedUtf16String, cell_from_abi, cell_into_abi, owned_utf16_string_into_abi, vm_from_abi,
};
use crate::layout::host_class::JSVM;
use crate::runtime::big_int::{BigInt, SignedBigInteger};
use crate::runtime::big_int_algorithms;

/// A BigInt of `value`. Call on the VM's thread.
///
/// # Safety
///
/// `vm` must be the embedder's VM.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_bigint_create_from_i64(vm: *mut JSVM, value: i64) -> *mut JSBigInt {
    // SAFETY: The caller passes its VM.
    let vm = unsafe { vm_from_abi(vm) };
    cell_into_abi(BigInt::create(vm, SignedBigInteger::from(value)))
}

/// A BigInt of `value`. Call on the VM's thread.
///
/// # Safety
///
/// `vm` must be the embedder's VM.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_bigint_create_from_u64(vm: *mut JSVM, value: u64) -> *mut JSBigInt {
    // SAFETY: The caller passes its VM.
    let vm = unsafe { vm_from_abi(vm) };
    cell_into_abi(BigInt::create(vm, SignedBigInteger::from(value)))
}

/// A BigInt of the magnitude in the `word_count` words at `words`, negated if `is_negative` is set. A zero magnitude
/// makes 0n whatever the sign. Call on the VM's thread.
///
/// # Safety
///
/// `vm` must be the embedder's VM, and `words` must point to `word_count` words unless `word_count` is 0.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_bigint_create_from_magnitude(
    vm: *mut JSVM,
    is_negative: bool,
    words: *const u32,
    word_count: usize,
) -> *mut JSBigInt {
    // SAFETY: The caller passes its VM.
    let vm = unsafe { vm_from_abi(vm) };
    let words: &[u32] = if word_count == 0 {
        &[]
    } else {
        // SAFETY: The caller passes `word_count` words.
        unsafe { core::slice::from_raw_parts(words, word_count) }
    };
    let sign = if is_negative { Sign::Minus } else { Sign::Plus };
    cell_into_abi(BigInt::create(vm, SignedBigInteger::from_slice(sign, words)))
}

/// Whether `bigint` is less than 0n. Call on the VM's thread.
///
/// # Safety
///
/// `bigint` must be a BigInt of the embedder's VM.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_bigint_is_negative(bigint: *mut JSBigInt) -> bool {
    // SAFETY: The caller passes a BigInt of the VM.
    unsafe { cell_from_abi(bigint) }.big_integer().sign() == Sign::Minus
}

/// How many words the magnitude of `bigint` has, without leading zero words, so 0 for 0n. Call on the VM's thread.
///
/// # Safety
///
/// `bigint` must be a BigInt of the embedder's VM.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_bigint_magnitude_word_count(bigint: *mut JSBigInt) -> usize {
    // SAFETY: The caller passes a BigInt of the VM.
    unsafe { cell_from_abi(bigint) }.big_integer().iter_u32_digits().len()
}

/// Writes the magnitude of `bigint` to the `word_count` words at `words`, which must be the count
/// js_bigint_magnitude_word_count returns. Call on the VM's thread.
///
/// # Safety
///
/// `bigint` must be a BigInt of the embedder's VM, and `words` valid for writing `word_count` words unless
/// `word_count` is 0.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_bigint_copy_magnitude_words(bigint: *mut JSBigInt, words: *mut u32, word_count: usize) {
    // SAFETY: The caller passes a BigInt of the VM.
    let bigint = unsafe { cell_from_abi(bigint) };
    let magnitude = bigint.big_integer().iter_u32_digits();
    assert_eq!(
        magnitude.len(),
        word_count,
        "the embedder makes room for the whole magnitude"
    );
    if word_count == 0 {
        return;
    }
    // SAFETY: The caller passes room for `word_count` words.
    let words = unsafe { core::slice::from_raw_parts_mut(words, word_count) };
    for (word, magnitude_word) in words.iter_mut().zip(magnitude) {
        *word = magnitude_word;
    }
}

/// `bigint` modulo 2^64, as a two's complement 64-bit integer, like BigInt.asIntN(64, bigint). Call on the VM's
/// thread.
///
/// # Safety
///
/// `bigint` must be a BigInt of the embedder's VM.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_bigint_to_i64(bigint: *mut JSBigInt) -> i64 {
    // SAFETY: The caller passes a BigInt of the VM.
    big_int_algorithms::to_u64(unsafe { cell_from_abi(bigint) }.big_integer()) as i64
}

/// `bigint` modulo 2^64, like BigInt.asUintN(64, bigint). Call on the VM's thread.
///
/// # Safety
///
/// `bigint` must be a BigInt of the embedder's VM.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_bigint_to_u64(bigint: *mut JSBigInt) -> u64 {
    // SAFETY: The caller passes a BigInt of the VM.
    big_int_algorithms::to_u64(unsafe { cell_from_abi(bigint) }.big_integer())
}

/// The digits of `bigint` in `radix`, which is 2 to 36, with lowercase letters and a leading "-" if it is negative,
/// as BigInt.prototype.toString writes them. The caller owns the string. Call on the VM's thread.
///
/// # Safety
///
/// `bigint` must be a BigInt of the embedder's VM.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn js_bigint_to_string(bigint: *mut JSBigInt, radix: u32) -> JSOwnedUtf16String {
    assert!((2..=36).contains(&radix), "the embedder passes a radix from 2 to 36");
    // SAFETY: The caller passes a BigInt of the VM.
    let digits = big_int_algorithms::to_base(unsafe { cell_from_abi(bigint) }.big_integer(), radix);
    owned_utf16_string_into_abi(ak::Utf16String::from_utf8(&digits))
}

#[cfg(all(test, libjs_runtime_tests_with_libgc))]
mod tests {
    use super::*;
    use crate::embedding::abi_types::{owned_utf16_string_from_abi, vm_into_abi};
    use crate::interpreter::vm::Vm;
    use crate::runtime::realm::test_realm::TestRealm;
    use crate::utf16::Utf16View;

    fn digits(bigint: *mut JSBigInt, radix: u32) -> String {
        // SAFETY: The tests pass live BigInts and take the string's reference.
        Utf16View::of_string(&unsafe { owned_utf16_string_from_abi(js_bigint_to_string(bigint, radix)) }).to_utf8()
    }

    fn magnitude(bigint: *mut JSBigInt) -> Vec<u32> {
        // SAFETY: The tests pass live BigInts, and the vector has room for the whole magnitude.
        unsafe {
            let mut words = vec![0; js_bigint_magnitude_word_count(bigint)];
            js_bigint_copy_magnitude_words(bigint, words.as_mut_ptr(), words.len());
            words
        }
    }

    #[test]
    fn bigints_round_trip_through_64_bit_integers_and_magnitudes() {
        let vm = Vm::create();
        let _test_realm = TestRealm::new(&vm);
        let abi_vm = vm_into_abi(&vm);

        // SAFETY: The VM is live, and so are the BigInts it creates while the test holds them.
        unsafe {
            let minimum = js_bigint_create_from_i64(abi_vm, i64::MIN);
            assert!(js_bigint_is_negative(minimum));
            assert_eq!(js_bigint_to_i64(minimum), i64::MIN);
            assert_eq!(magnitude(minimum), [0, 0x8000_0000]);
            assert_eq!(digits(minimum, 10), "-9223372036854775808");

            let maximum = js_bigint_create_from_u64(abi_vm, u64::MAX);
            assert!(!js_bigint_is_negative(maximum));
            assert_eq!(js_bigint_to_u64(maximum), u64::MAX);
            assert_eq!(js_bigint_to_i64(maximum), -1);
            assert_eq!(digits(maximum, 16), "ffffffffffffffff");

            // 2^64 + 0x1234_5678, negated.
            let words = [0x1234_5678, 0, 1];
            let large = js_bigint_create_from_magnitude(abi_vm, true, words.as_ptr(), words.len());
            assert!(js_bigint_is_negative(large));
            assert_eq!(magnitude(large), words);
            assert_eq!(digits(large, 10), "-18446744074014971512");
            assert_eq!(js_bigint_to_u64(large), 0x1234_5678u64.wrapping_neg());

            // Leading zero words and the sign of zero are dropped.
            let padded = [7, 0, 0];
            let seven = js_bigint_create_from_magnitude(abi_vm, false, padded.as_ptr(), padded.len());
            assert_eq!(magnitude(seven), [7]);
            let zero = js_bigint_create_from_magnitude(abi_vm, true, core::ptr::null(), 0);
            assert!(!js_bigint_is_negative(zero));
            assert!(magnitude(zero).is_empty());
            assert_eq!(digits(zero, 2), "0");
        }
    }
}

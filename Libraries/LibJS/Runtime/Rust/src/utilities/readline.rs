/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! The functions of libedit's <editline/readline.h> that the REPL of js reads its input with.

use core::ffi::{CStr, c_char, c_int};

/// rl_completion_func_t: called with the word before the cursor and its bounds in rl_line_buffer, it returns NULL or
/// a NULL-terminated, malloc()ed array of malloc()ed strings, whose first one replaces the word.
pub type CompletionFunction = unsafe extern "C" fn(text: *const c_char, start: c_int, end: c_int) -> *mut *mut c_char;

mod ffi {
    use core::ffi::{c_char, c_int};

    use super::CompletionFunction;

    unsafe extern "C" {
        pub fn readline(prompt: *const c_char) -> *mut c_char;
        pub fn add_history(line: *const c_char) -> c_int;
        pub fn read_history(filename: *const c_char) -> c_int;
        pub fn write_history(filename: *const c_char) -> c_int;
        pub static mut rl_line_buffer: *mut c_char;
        pub static mut rl_attempted_completion_function: Option<CompletionFunction>;
        pub static mut rl_attempted_completion_over: c_int;
    }
}

/// readline(): shows `prompt` and reads a line, without its newline. Returns None at the end of the input.
pub fn readline(prompt: &CStr) -> Option<Vec<u8>> {
    // SAFETY: The prompt is NUL-terminated, and readline() returns NULL or a NUL-terminated line from malloc(), which
    // is copied before it is freed.
    unsafe {
        let raw_line = ffi::readline(prompt.as_ptr());
        if raw_line.is_null() {
            return None;
        }
        let line = CStr::from_ptr(raw_line).to_bytes().to_vec();
        libc::free(raw_line.cast());
        Some(line)
    }
}

pub fn add_history(line: &CStr) {
    // SAFETY: The line is NUL-terminated, and add_history() copies it.
    unsafe { ffi::add_history(line.as_ptr()) };
}

pub fn read_history(filename: &CStr) {
    // SAFETY: The filename is NUL-terminated.
    unsafe { ffi::read_history(filename.as_ptr()) };
}

pub fn write_history(filename: &CStr) {
    // SAFETY: The filename is NUL-terminated.
    unsafe { ffi::write_history(filename.as_ptr()) };
}

/// rl_line_buffer: the line being edited, for a completion function to complete.
pub fn line_buffer() -> Option<Vec<u8>> {
    // SAFETY: libedit keeps rl_line_buffer NULL or NUL-terminated while it calls a completion function.
    unsafe {
        let line_buffer = ffi::rl_line_buffer;
        if line_buffer.is_null() {
            return None;
        }
        Some(CStr::from_ptr(line_buffer).to_bytes().to_vec())
    }
}

pub fn set_attempted_completion_function(completion_function: CompletionFunction) {
    // SAFETY: js reads its input on one thread, and libedit reads this variable only in readline().
    unsafe { ffi::rl_attempted_completion_function = Some(completion_function) };
}

/// Sets rl_attempted_completion_over, which keeps libedit from completing file names when a completion function
/// finds no matches.
pub fn set_attempted_completion_over() {
    // SAFETY: Completion functions run on the thread that called readline().
    unsafe { ffi::rl_attempted_completion_over = 1 };
}

/// The array that a completion function returns: `common_prefix`, then each of `completions`, as C strings that
/// end at their first NUL like strdup() makes them. Returns NULL if it runs out of memory.
pub fn completion_matches(common_prefix: &[u8], completions: &[Vec<u8>]) -> *mut *mut c_char {
    // SAFETY: The array has room for every string and the terminating NULL, which calloc() zeroes, and strndup()
    // reads at most the length of each string. On failure, everything allocated so far is freed.
    unsafe {
        let matches = libc::calloc(completions.len() + 2, size_of::<*mut c_char>()).cast::<*mut c_char>();
        if matches.is_null() {
            return core::ptr::null_mut();
        }
        let strings = core::iter::once(common_prefix).chain(completions.iter().map(Vec::as_slice));
        for (index, string) in strings.enumerate() {
            let copy = libc::strndup(string.as_ptr().cast(), string.len());
            if copy.is_null() {
                for allocated_index in 0..index {
                    libc::free((*matches.add(allocated_index)).cast());
                }
                libc::free(matches.cast());
                return core::ptr::null_mut();
            }
            *matches.add(index) = copy;
        }
        matches
    }
}

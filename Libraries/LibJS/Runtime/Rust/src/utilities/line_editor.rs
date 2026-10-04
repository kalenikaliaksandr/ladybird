/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! The line editor that the REPL and the debugger prompt of js-rust read their input with. js-rust's C++ main passes
//! in the functions of libedit, so that the runtime, which programs that edit no lines link too, does not depend on it.

use core::cell::Cell;
use core::ffi::{CStr, c_char, c_int};
use std::io::IsTerminal;

/// Completes the line being edited, given as a NUL-terminated string. Returns NULL, or a NULL-terminated, malloc()ed
/// array of malloc()ed strings, the first of which replaces the word before the cursor, as rl_completion_func_t does.
pub type LineCompletionFunction = unsafe extern "C" fn(line: *const c_char) -> *mut *mut c_char;

/// What js-rust's C++ main passes to libjs_runtime_rust_js_main(): functions of libedit's <editline/readline.h>, and
/// one that installs a completion function. Utilities/js-rust.cpp declares it field for field. None of the functions
/// is NULL, and each one is called on the thread that called libjs_runtime_rust_js_main().
#[repr(C)]
#[derive(Clone, Copy)]
pub struct JSLineEditor {
    /// readline(): shows the prompt when the standard input and output are terminals, and reads a line. Returns the
    /// line without its newline, in memory from malloc() that the caller frees, or NULL at the end of the input.
    pub readline: unsafe extern "C" fn(prompt: *const c_char) -> *mut c_char,
    /// add_history(), which copies the line.
    pub add_history: unsafe extern "C" fn(line: *const c_char) -> c_int,
    pub read_history: unsafe extern "C" fn(path: *const c_char) -> c_int,
    pub write_history: unsafe extern "C" fn(path: *const c_char) -> c_int,
    /// Makes readline() complete the line it edits with the function instead of with file names.
    pub set_line_completion_function: unsafe extern "C" fn(complete_line: LineCompletionFunction),
}

/// What makes the completions of a line, the longest common prefix of which replaces the word that is completed.
pub type LineCompleter = fn(line: &[u8]) -> Vec<Vec<u8>>;

thread_local! {
    static LINE_COMPLETER: Cell<Option<LineCompleter>> = const { Cell::new(None) };
}

impl JSLineEditor {
    /// Shows `prompt` and reads a line, without its newline. Returns None at the end of the input.
    pub fn read_line(&self, prompt: &CStr) -> Option<Vec<u8>> {
        // NB: libedit only prompts when the standard input and output are terminals, and writes the prompt to the
        //     stdout of C stdio, which what the runtime buffers for the standard output has to come out before.
        if std::io::stdin().is_terminal() && std::io::stdout().is_terminal() {
            crate::standard_output::flush();
        }

        // SAFETY: The prompt is NUL-terminated, and readline() returns NULL or a NUL-terminated line from malloc(),
        // which is copied before it is freed.
        unsafe {
            let raw_line = (self.readline)(prompt.as_ptr());
            if raw_line.is_null() {
                return None;
            }
            let line = CStr::from_ptr(raw_line).to_bytes().to_vec();
            libc::free(raw_line.cast());
            Some(line)
        }
    }

    pub fn add_line_to_history(&self, line: &CStr) {
        // SAFETY: The line is NUL-terminated, and add_history() copies it.
        unsafe { (self.add_history)(line.as_ptr()) };
    }

    pub fn read_history_file(&self, path: &CStr) {
        // SAFETY: The path is NUL-terminated.
        unsafe { (self.read_history)(path.as_ptr()) };
    }

    pub fn write_history_file(&self, path: &CStr) {
        // SAFETY: The path is NUL-terminated.
        unsafe { (self.write_history)(path.as_ptr()) };
    }

    /// Makes the line editor complete the line it edits with `line_completer`.
    pub fn complete_lines_with(&self, line_completer: LineCompleter) {
        LINE_COMPLETER.set(Some(line_completer));
        // SAFETY: complete_line_with_line_completer() reads only the NUL-terminated line it is given.
        unsafe { (self.set_line_completion_function)(complete_line_with_line_completer) };
    }
}

unsafe extern "C" fn complete_line_with_line_completer(line: *const c_char) -> *mut *mut c_char {
    let Some(line_completer) = LINE_COMPLETER.get() else {
        return core::ptr::null_mut();
    };
    // SAFETY: The line editor passes the NUL-terminated line it edits, which stays alive until this returns.
    let completions = line_completer(unsafe { CStr::from_ptr(line) }.to_bytes());
    if completions.is_empty() {
        return core::ptr::null_mut();
    }
    completion_matches(common_prefix_of(&completions), &completions)
}

/// The longest run of bytes that every completion starts with, which replaces the word that is completed.
fn common_prefix_of(completions: &[Vec<u8>]) -> &[u8] {
    let mut common_prefix = completions[0].as_slice();
    for completion in &completions[1..] {
        let prefix_length = common_prefix
            .iter()
            .zip(completion)
            .take_while(|(prefix_byte, completion_byte)| prefix_byte == completion_byte)
            .count();
        common_prefix = &common_prefix[..prefix_length];
    }
    common_prefix
}

/// The array that a LineCompletionFunction returns: `common_prefix`, then each of `completions`, as C strings that
/// end at their first NUL like strdup() makes them. Returns NULL if it runs out of memory.
fn completion_matches(common_prefix: &[u8], completions: &[Vec<u8>]) -> *mut *mut c_char {
    // SAFETY: The array has room for every string and the terminating NULL, which calloc() zeroes. On failure,
    // everything allocated so far is freed.
    unsafe {
        let matches = libc::calloc(completions.len() + 2, size_of::<*mut c_char>()).cast::<*mut c_char>();
        if matches.is_null() {
            return core::ptr::null_mut();
        }
        let strings = core::iter::once(common_prefix).chain(completions.iter().map(Vec::as_slice));
        for (index, string) in strings.enumerate() {
            let copy = strndup(string);
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

/// strndup(): a copy of `string` in memory from malloc(), which ends at the first NUL of the string.
#[cfg(unix)]
fn strndup(string: &[u8]) -> *mut c_char {
    // SAFETY: strndup() reads at most the length of the string.
    unsafe { libc::strndup(string.as_ptr().cast(), string.len()) }
}

/// strndup(), which the C runtime of Windows lacks.
#[cfg(windows)]
fn strndup(string: &[u8]) -> *mut c_char {
    let length = string.iter().position(|&byte| byte == 0).unwrap_or(string.len());
    // SAFETY: The copy has room for the bytes before the first NUL and the NUL that follows them.
    unsafe {
        let copy = libc::malloc(length + 1).cast::<u8>();
        if !copy.is_null() {
            core::ptr::copy_nonoverlapping(string.as_ptr(), copy, length);
            copy.add(length).write(0);
        }
        copy.cast()
    }
}

/// What the C++ js does without libedit: shows `prompt`, and reads a line as fgets() into a buffer of 4096 bytes does,
/// with its newline. Returns None at the end of the input.
pub fn read_line_without_line_editor(prompt: &CStr) -> Option<Vec<u8>> {
    use std::io::BufRead;

    crate::standard_output::out(prompt.to_bytes());
    crate::standard_output::flush();

    let mut line = Vec::new();
    let mut standard_input = std::io::stdin().lock();
    while line.len() < 4095 {
        let Ok(buffer) = standard_input.fill_buf() else {
            break;
        };
        let Some(&byte) = buffer.first() else {
            break;
        };
        standard_input.consume(1);
        line.push(byte);
        if byte == b'\n' {
            break;
        }
    }
    if line.is_empty() {
        return None;
    }
    Some(line)
}

#[cfg(test)]
mod tests {
    use core::cell::RefCell;
    use std::ffi::CString;

    use super::*;

    thread_local! {
        static SCRIPTED_LINES: RefCell<Vec<&'static CStr>> = const { RefCell::new(Vec::new()) };
        static HISTORY: RefCell<Vec<Vec<u8>>> = const { RefCell::new(Vec::new()) };
        static INSTALLED_LINE_COMPLETION_FUNCTION: Cell<Option<LineCompletionFunction>> = const { Cell::new(None) };
        static COMPLETIONS_OFFERED: RefCell<Vec<Vec<Vec<u8>>>> = const { RefCell::new(Vec::new()) };
    }

    /// Takes a completion array apart, freeing it as libedit does.
    fn take_completion_matches(matches: *mut *mut c_char) -> Vec<Vec<u8>> {
        let mut strings = Vec::new();
        if matches.is_null() {
            return strings;
        }
        // SAFETY: The array is NULL-terminated, and it and its strings come from malloc().
        unsafe {
            let mut index = 0;
            while !(*matches.add(index)).is_null() {
                strings.push(CStr::from_ptr(*matches.add(index)).to_bytes().to_vec());
                libc::free((*matches.add(index)).cast());
                index += 1;
            }
            libc::free(matches.cast());
        }
        strings
    }

    /// readline() of the next scripted line. A tab at the end of the line stands for pressing tab after typing the
    /// rest of it, which calls the completion function.
    unsafe extern "C" fn scripted_readline(_prompt: *const c_char) -> *mut c_char {
        let Some(line) = SCRIPTED_LINES.with_borrow_mut(|lines| (!lines.is_empty()).then(|| lines.remove(0))) else {
            return core::ptr::null_mut();
        };
        let line = line.to_bytes();
        let line = match line.strip_suffix(b"\t") {
            Some(typed_line) => {
                let complete_line = INSTALLED_LINE_COMPLETION_FUNCTION
                    .get()
                    .expect("a completion function is installed");
                let typed_line = CString::new(typed_line).expect("the line has no NULs");
                // SAFETY: The line is NUL-terminated.
                let matches = unsafe { complete_line(typed_line.as_ptr()) };
                COMPLETIONS_OFFERED.with_borrow_mut(|offered| offered.push(take_completion_matches(matches)));
                typed_line
            }
            None => CString::new(line).expect("the line has no NULs"),
        };
        // SAFETY: The line is NUL-terminated.
        unsafe { libc::strdup(line.as_ptr()) }
    }

    unsafe extern "C" fn recording_add_history(line: *const c_char) -> c_int {
        // SAFETY: The caller passes a NUL-terminated line.
        let line = unsafe { CStr::from_ptr(line) }.to_bytes().to_vec();
        HISTORY.with_borrow_mut(|history| history.push(line));
        0
    }

    unsafe extern "C" fn no_history_file(_path: *const c_char) -> c_int {
        0
    }

    unsafe extern "C" fn install_line_completion_function(complete_line: LineCompletionFunction) {
        INSTALLED_LINE_COMPLETION_FUNCTION.set(Some(complete_line));
    }

    const SCRIPTED_LINE_EDITOR: JSLineEditor = JSLineEditor {
        readline: scripted_readline,
        add_history: recording_add_history,
        read_history: no_history_file,
        write_history: no_history_file,
        set_line_completion_function: install_line_completion_function,
    };

    /// Completes a line to the words of the line after it. Like a completer that runs code, it uses the line editor
    /// while the line editor calls it: it adds the line to the history and reads the line after it.
    fn complete_to_the_words_of_the_next_line(line: &[u8]) -> Vec<Vec<u8>> {
        SCRIPTED_LINE_EDITOR.add_line_to_history(&CString::new(line).expect("the line has no NULs"));
        let next_line = SCRIPTED_LINE_EDITOR
            .read_line(c"completions> ")
            .expect("a line follows");
        next_line
            .split(|&byte| byte == b' ')
            .filter(|word| !word.is_empty())
            .map(<[u8]>::to_vec)
            .collect()
    }

    #[test]
    fn lines_come_back_in_memory_of_their_own_and_go_into_the_history() {
        SCRIPTED_LINES.set(vec![c"first", c""]);
        assert_eq!(SCRIPTED_LINE_EDITOR.read_line(c"> ").as_deref(), Some(&b"first"[..]));
        assert_eq!(SCRIPTED_LINE_EDITOR.read_line(c"> ").as_deref(), Some(&b""[..]));
        assert_eq!(SCRIPTED_LINE_EDITOR.read_line(c"> "), None);

        SCRIPTED_LINE_EDITOR.add_line_to_history(c"first");
        assert_eq!(HISTORY.take(), [b"first".to_vec()]);
    }

    #[test]
    fn the_line_completer_can_use_the_line_editor_that_calls_it() {
        SCRIPTED_LINE_EDITOR.complete_lines_with(complete_to_the_words_of_the_next_line);
        SCRIPTED_LINES.set(vec![c"Math.a\t", c"Math.abs Math.acos", c"x\t", c"", c"last"]);

        assert_eq!(SCRIPTED_LINE_EDITOR.read_line(c"> ").as_deref(), Some(&b"Math.a"[..]));
        assert_eq!(SCRIPTED_LINE_EDITOR.read_line(c"> ").as_deref(), Some(&b"x"[..]));
        assert_eq!(SCRIPTED_LINE_EDITOR.read_line(c"> ").as_deref(), Some(&b"last"[..]));
        assert_eq!(SCRIPTED_LINE_EDITOR.read_line(c"> "), None);

        assert_eq!(HISTORY.take(), [b"Math.a".to_vec(), b"x".to_vec()]);
        // The longest common prefix comes first, and a line without completions completes to nothing.
        assert_eq!(
            COMPLETIONS_OFFERED.take(),
            [
                vec![b"Math.a".to_vec(), b"Math.abs".to_vec(), b"Math.acos".to_vec()],
                Vec::new(),
            ]
        );
    }

    #[test]
    fn completions_share_their_longest_common_prefix_of_bytes() {
        let prefix = |completions: &[&[u8]]| {
            let completions: Vec<Vec<u8>> = completions.iter().map(|completion| completion.to_vec()).collect();
            common_prefix_of(&completions).to_vec()
        };
        assert_eq!(prefix(&[b"Math.abs"]), b"Math.abs");
        assert_eq!(prefix(&[b"Math.abs", b"Math.acos", b"Math.acosh"]), b"Math.a");
        assert_eq!(prefix(&[b"xyz", b"x"]), b"x");
        assert_eq!(prefix(&[b"a", b"b"]), b"");
        assert_eq!(prefix(&["\u{E4}".as_bytes(), "\u{E5}".as_bytes()]), b"\xC3");
    }

    #[test]
    fn completion_matches_end_at_the_first_nul_of_each_completion() {
        let matches = completion_matches(b"a", &[b"ab".to_vec(), b"ac\0d".to_vec()]);
        assert_eq!(
            take_completion_matches(matches),
            [b"a".to_vec(), b"ab".to_vec(), b"ac".to_vec()]
        );
    }
}

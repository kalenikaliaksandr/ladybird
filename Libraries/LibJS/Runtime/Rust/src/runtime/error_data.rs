/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

use core::cell::Cell;

use ak::Utf16String;
use libjs_runtime_macros::Trace;

use crate::gc::class::{GcCell, define_cell};
use crate::interpreter::vm::Vm;
use crate::layout::cell::{CellHeader, Gc};
use crate::runtime::primitive_string::PrimitiveString;
use crate::source_range::SourceRange;
use crate::utf16::Utf16View;

pub struct TracebackFrame {
    pub function_name: Utf16String,
    pub cached_source_range: Option<SourceRange>,
}

impl TracebackFrame {
    /// The filename, line and column of the frame's source range, which are empty for a frame without one.
    pub fn source_position(&self) -> (Utf16View<'_>, u32, u32) {
        match &self.cached_source_range {
            Some(source_range) => (
                Utf16View::of_string(source_range.filename()),
                source_range.start.line,
                source_range.start.column,
            ),
            None => (Utf16View::Ascii(b""), 0, 0),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompactTraceback {
    No,
    Yes,
}

/// [[ErrorData]]: the call stack an error was created on.
#[derive(Trace)]
pub struct ErrorData {
    #[gc(untraced)]
    traceback: Vec<TracebackFrame>,
    cached_string: Cell<Option<Gc<PrimitiveString>>>,
}

impl ErrorData {
    pub fn new(vm: &Vm) -> Self {
        Self {
            traceback: Self::populate_stack(vm),
            cached_string: Cell::new(None),
        }
    }

    fn populate_stack(vm: &Vm) -> Vec<TracebackFrame> {
        let stack_trace = vm.stack_trace();
        let mut traceback = Vec::with_capacity(stack_trace.len());
        for element in stack_trace {
            // SAFETY: The elements of a stack trace are the live execution contexts it was taken from.
            let context = unsafe { element.execution_context.as_ref() };
            traceback.push(TracebackFrame {
                function_name: context
                    .function
                    .get()
                    .map(|function| function.name_for_call_stack())
                    .unwrap_or_default(),
                cached_source_range: element.source_range,
            });
        }
        traceback
    }

    pub fn traceback(&self) -> &[TracebackFrame] {
        &self.traceback
    }

    pub fn set_cached_string(&self, string: Gc<PrimitiveString>) {
        self.cached_string.set(Some(string));
    }

    pub fn cached_string(&self) -> Option<Gc<PrimitiveString>> {
        self.cached_string.get()
    }

    pub fn stack_string(&self, compact: CompactTraceback) -> Utf16String {
        if self.traceback.is_empty() {
            return Utf16String::default();
        }

        let mut stack_string_builder: Vec<u16> = Vec::new();

        // Note: We roughly follow V8's formatting
        let append_frame = |builder: &mut Vec<u16>, frame: &TracebackFrame| {
            let function_name = Utf16View::of_string(&frame.function_name);
            let (filename, line, column) = frame.source_position();
            builder.extend("    at ".encode_utf16());
            // Note: Since we don't know whether we have a valid SourceRange here we just check for some default values.
            if !filename.is_empty() || line != 0 || column != 0 {
                if function_name.is_empty() {
                    filename.append_to(builder);
                    builder.extend(format!(":{line}:{column}\n").encode_utf16());
                } else {
                    function_name.append_to(builder);
                    builder.extend(" (".encode_utf16());
                    filename.append_to(builder);
                    builder.extend(format!(":{line}:{column})\n").encode_utf16());
                }
            } else {
                if function_name.is_empty() {
                    builder.extend("<unknown>".encode_utf16());
                } else {
                    function_name.append_to(builder);
                }
                builder.push(u16::from(b'\n'));
            }
        };

        let is_same_frame = |a: &TracebackFrame, b: &TracebackFrame| {
            if a.function_name.is_empty() && b.function_name.is_empty() {
                let (filename_a, line_a, _) = a.source_position();
                let (filename_b, line_b, _) = b.source_position();
                return filename_a == filename_b && line_a == line_b;
            }
            a.function_name == b.function_name
        };

        // Note: We don't want to capture the global execution context, so we omit the last frame
        // Note: The error's name and message get prepended by Error.prototype.stack
        let mut repetitions: usize = 0;
        let used_frames = self.traceback.len() - 1;
        for i in 0..used_frames {
            let frame = &self.traceback[i];
            if compact == CompactTraceback::Yes && i + 1 < used_frames {
                let next_traceback_frame = &self.traceback[i + 1];
                if is_same_frame(frame, next_traceback_frame) {
                    repetitions += 1;
                    continue;
                }
            }
            if repetitions > 4 {
                // If more than 5 (1 + >4) consecutive function calls with the same name, print
                // the name only once and show the number of repetitions instead. This prevents
                // printing ridiculously large call stacks of recursive functions.
                append_frame(&mut stack_string_builder, frame);
                stack_string_builder.extend(format!("    {repetitions} more calls\n").encode_utf16());
            } else {
                for _ in 0..repetitions + 1 {
                    append_frame(&mut stack_string_builder, frame);
                }
            }
            repetitions = 0;
        }
        for _ in 0..repetitions {
            append_frame(&mut stack_string_builder, &self.traceback[used_frames - 1]);
        }

        Utf16String::from_utf16(&stack_string_builder)
    }
}

/// The [[ErrorData]] of a host object that is not an Error, such as a DOMException, in a cell of its own, which the
/// embedder keeps alive with the object.
#[repr(C)]
#[derive(Trace)]
pub struct ErrorDataCell {
    header: CellHeader,
    error_data: ErrorData,
}

define_cell!(ErrorDataCell, Other);

impl ErrorDataCell {
    /// ErrorDataCell::capture(vm): error data with the call stack of the running execution context.
    pub fn capture(vm: &Vm) -> Gc<ErrorDataCell> {
        vm.heap().allocate(Self {
            header: CellHeader::for_class(Self::CLASS),
            error_data: ErrorData::new(vm),
        })
    }

    pub fn error_data(&self) -> &ErrorData {
        &self.error_data
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source_code::SourceCode;
    use crate::source_range::Position;

    fn frame(function_name: &str, source_range: Option<(u32, u32)>) -> TracebackFrame {
        let code = SourceCode::create(Utf16String::from_utf8("eval"), Utf16String::default());
        TracebackFrame {
            function_name: Utf16String::from_utf8(function_name),
            cached_source_range: source_range.map(|(line, column)| SourceRange {
                code: code.clone(),
                start: Position { line, column },
            }),
        }
    }

    fn stack_string(traceback: Vec<TracebackFrame>, compact: CompactTraceback) -> String {
        let error_data = ErrorData {
            traceback,
            cached_string: Cell::new(None),
        };
        Utf16View::of_string(&error_data.stack_string(compact)).to_utf8()
    }

    /// The frames of `function r(n) { if (n == 0) throw new Error("r"); return r(n - 1) } r(count)`, with the bottom
    /// execution context, which the stack string leaves out.
    fn recursion(count: usize) -> Vec<TracebackFrame> {
        let mut traceback = vec![frame("Error", None)];
        traceback.extend((0..=count).map(|_| frame("r", Some((1, 58)))));
        traceback.push(frame("", Some((1, 70))));
        traceback.push(frame("", None));
        traceback
    }

    #[test]
    fn compact_tracebacks_fold_more_than_five_repeated_frames_like_the_cpp_runtime() {
        // What the C++ js binary prints for the uncaught errors of the recursion with 7 and 3 calls.
        assert_eq!(
            stack_string(recursion(7), CompactTraceback::Yes),
            "    at Error\n    at r (eval:1:58)\n    7 more calls\n    at eval:1:70\n"
        );
        assert_eq!(
            stack_string(recursion(3), CompactTraceback::Yes),
            "    at Error\n    at r (eval:1:58)\n    at r (eval:1:58)\n    at r (eval:1:58)\n    at r (eval:1:58)\n    \
             at eval:1:70\n"
        );
        let full = stack_string(recursion(7), CompactTraceback::No);
        assert_eq!(full.matches("    at r (eval:1:58)\n").count(), 8);
        assert!(!full.contains("more calls"));
    }

    #[test]
    fn frames_without_names_or_source_ranges_show_what_they_have() {
        let traceback = vec![
            frame("TypeError", None),
            frame("f", Some((1, 28))),
            frame("", Some((1, 112))),
            frame("", None),
            frame("", Some((1, 97))),
            frame("", None),
        ];
        assert_eq!(
            stack_string(traceback, CompactTraceback::Yes),
            "    at TypeError\n    at f (eval:1:28)\n    at eval:1:112\n    at <unknown>\n    at eval:1:97\n"
        );
        // Frames without names are the same frame if they are on the same line of the same file.
        let traceback = (0..7)
            .map(|column| frame("", Some((2, column))))
            .chain([frame("", None)])
            .collect();
        assert_eq!(
            stack_string(traceback, CompactTraceback::Yes),
            "    at eval:2:6\n    6 more calls\n"
        );
        assert_eq!(stack_string(vec![frame("", None)], CompactTraceback::No), "");
        assert_eq!(stack_string(Vec::new(), CompactTraceback::No), "");
    }
}

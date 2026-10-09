/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! The messages of the errors that the bindings throw, which are those of the JS::ErrorType in
//! Libraries/LibJS/Runtime/ErrorTypes.h that the C++ bindings throw for the same reasons.

/// JS::ErrorType::NotAnObjectOfType.
pub(crate) fn not_an_object_of_type(name: &str) -> String {
    format!("Not an object of type {name}")
}

/// JS::ErrorType::BadArgCountOne.
pub(crate) fn bad_arg_count_one(function: &str) -> String {
    format!("{function}() needs one argument")
}

/// JS::ErrorType::BadArgCountMany.
pub(crate) fn bad_arg_count_many(function: &str, count: usize) -> String {
    format!("{function}() needs {count} arguments")
}

/// JS::ErrorType::InvalidLength.
pub(crate) fn invalid_length(what: &str) -> String {
    format!("Invalid {what} length")
}

/// JS::ErrorType::SharedArrayBuffer.
pub(crate) const SHARED_ARRAY_BUFFER: &str = "The array buffer object cannot be a SharedArrayBuffer";

#[cfg(test)]
mod tests {
    use super::*;

    fn error_type_message(name: &str) -> String {
        let error_types = include_str!("../../../../LibJS/Runtime/ErrorTypes.h");
        let prefix = format!("M({name}, \"");
        let start = error_types.find(&prefix).expect("ErrorTypes.h defines the error type") + prefix.len();
        let length = error_types[start..].find('"').expect("the message ends");
        error_types[start..start + length].to_string()
    }

    #[test]
    fn messages_match_error_types() {
        assert_eq!(
            error_type_message("NotAnObjectOfType").replace("{}", "Thing"),
            not_an_object_of_type("Thing")
        );
        assert_eq!(
            error_type_message("BadArgCountOne").replace("{}", "f"),
            bad_arg_count_one("f")
        );
        assert_eq!(
            error_type_message("BadArgCountMany")
                .replacen("{}", "f", 1)
                .replacen("{}", "2", 1),
            bad_arg_count_many("f", 2)
        );
        assert_eq!(
            error_type_message("InvalidLength").replace("{}", "typed array"),
            invalid_length("typed array")
        );
        assert_eq!(error_type_message("SharedArrayBuffer"), SHARED_ARRAY_BUFFER);
    }
}

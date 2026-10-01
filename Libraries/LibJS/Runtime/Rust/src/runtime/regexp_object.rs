/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

//! The parts of Libraries/LibJS/Runtime/RegExpObject.cpp the runtime implements so far.

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParseRegexPatternError {
    pub error: String,
}

// 22.2.3.4 Static Semantics: ParsePattern ( patternText, u, v ), https://tc39.es/ecma262/#sec-parsepattern
pub fn parse_regex_pattern(
    pattern: &[u16],
    unicode: bool,
    unicode_sets: bool,
) -> Result<Vec<u16>, ParseRegexPatternError> {
    if unicode && unicode_sets {
        return Err(ParseRegexPatternError {
            error: regexp_object_incompatible_flags('u', 'v'),
        });
    }

    let unicode_mode = unicode || unicode_sets;
    validate_named_group_name_surrogates(pattern, unicode_mode)?;

    let mut builder = Vec::with_capacity(pattern.len());
    let mut previous_code_unit_was_backslash = false;
    let mut index = 0;
    while index < pattern.len() {
        let code_unit = pattern[index];
        index += 1;

        if code_unit > 0x7f {
            // Incorrectly escaping this code unit will result in a wildly different regex than intended
            // as we're converting <c> to <\uhhhh>, which would turn into <\\uhhhh> if (incorrectly) escaped again,
            // leading to a matcher for the literal string "\uhhhh" instead of the intended code unit <c>.
            // As such, we're going to remove the (invalid) backslash and pretend it never existed.
            if !previous_code_unit_was_backslash {
                builder.push(u16::from(b'\\'));
            }
            previous_code_unit_was_backslash = false;

            if unicode_mode
                && is_utf16_high_surrogate(code_unit)
                && let Some(&next_code_unit) = pattern.get(index)
                && is_utf16_low_surrogate(next_code_unit)
            {
                let combined = decode_utf16_surrogate_pair(code_unit, next_code_unit);
                builder.extend(format!("u{{{combined:x}}}").encode_utf16());
                index += 1;
                continue;
            }

            if unicode_mode {
                builder.extend(format!("u{{{code_unit:04x}}}").encode_utf16());
            } else {
                builder.extend(format!("u{code_unit:04x}").encode_utf16());
            }
            continue;
        }

        builder.push(code_unit);
        previous_code_unit_was_backslash = code_unit == u16::from(b'\\') && !previous_code_unit_was_backslash;
    }

    Ok(builder)
}

/// ErrorType::RegExpObjectIncompatibleFlags.
fn regexp_object_incompatible_flags(first_flag: char, second_flag: char) -> String {
    format!("RegExp flag '{first_flag}' is incompatible with flag '{second_flag}'")
}

const HIGH_SURROGATE_MIN: u16 = 0xd800;
const HIGH_SURROGATE_MAX: u16 = 0xdbff;
const LOW_SURROGATE_MIN: u16 = 0xdc00;
const LOW_SURROGATE_MAX: u16 = 0xdfff;

fn is_utf16_high_surrogate(code_unit: u16) -> bool {
    (HIGH_SURROGATE_MIN..=HIGH_SURROGATE_MAX).contains(&code_unit)
}

fn is_utf16_low_surrogate(code_unit: u16) -> bool {
    (LOW_SURROGATE_MIN..=LOW_SURROGATE_MAX).contains(&code_unit)
}

fn decode_utf16_surrogate_pair(high_surrogate: u16, low_surrogate: u16) -> u32 {
    ((u32::from(high_surrogate - HIGH_SURROGATE_MIN)) << 10) + u32::from(low_surrogate - LOW_SURROGATE_MIN) + 0x10000
}

fn equals_ascii(code_unit: u16, ascii: u8) -> bool {
    code_unit == u16::from(ascii)
}

fn ascii_hex_digit_value(code_unit: u16) -> Option<u32> {
    char::from_u32(u32::from(code_unit)).and_then(|character| character.to_digit(16))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RegExpNameElementKind {
    CodePoint,
    HighSurrogate,
    LowSurrogate,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RegExpNameElementOrigin {
    Literal,
    FixedEscape,
    BracedEscape,
}

struct RegExpNameElement {
    kind: RegExpNameElementKind,
    origin: RegExpNameElementOrigin,
    next_index: usize,
}

fn invalid_group_name_error() -> ParseRegexPatternError {
    ParseRegexPatternError {
        error: "invalid group name".to_owned(),
    }
}

/// The C++ classifies escaped values through the u16 surrogate predicates, so a braced escape above the BMP is
/// classified by its low 16 bits (\u{1DF00} counts as a low surrogate). That narrowing is kept so that the same group
/// names are rejected.
fn classify_escaped_value(value: u32) -> RegExpNameElementKind {
    let truncated_to_code_unit = value as u16;
    if is_utf16_high_surrogate(truncated_to_code_unit) {
        RegExpNameElementKind::HighSurrogate
    } else if is_utf16_low_surrogate(truncated_to_code_unit) {
        RegExpNameElementKind::LowSurrogate
    } else {
        RegExpNameElementKind::CodePoint
    }
}

fn parse_regexp_name_element(pattern: &[u16], index: usize) -> Result<RegExpNameElement, ParseRegexPatternError> {
    let length = pattern.len();
    let Some(&code_unit) = pattern.get(index) else {
        return Err(invalid_group_name_error());
    };

    if !equals_ascii(code_unit, b'\\') {
        let (kind, next_index) = if is_utf16_high_surrogate(code_unit) {
            match pattern.get(index + 1) {
                Some(&next_code_unit) if is_utf16_low_surrogate(next_code_unit) => {
                    (RegExpNameElementKind::CodePoint, index + 2)
                }
                _ => (RegExpNameElementKind::HighSurrogate, index + 1),
            }
        } else if is_utf16_low_surrogate(code_unit) {
            (RegExpNameElementKind::LowSurrogate, index + 1)
        } else {
            (RegExpNameElementKind::CodePoint, index + 1)
        };
        return Ok(RegExpNameElement {
            kind,
            origin: RegExpNameElementOrigin::Literal,
            next_index,
        });
    }

    if !pattern.get(index + 1).is_some_and(|&next| equals_ascii(next, b'u')) {
        return Err(invalid_group_name_error());
    }

    let mut escape_index = index + 2;
    if pattern.get(escape_index).is_some_and(|&next| equals_ascii(next, b'{')) {
        escape_index += 1;

        let mut value = 0u32;
        let mut digits = 0usize;
        while escape_index < length && !equals_ascii(pattern[escape_index], b'}') {
            let Some(digit) = ascii_hex_digit_value(pattern[escape_index]) else {
                return Err(invalid_group_name_error());
            };
            value = value * 16 + digit;
            if value > 0x10ffff {
                return Err(invalid_group_name_error());
            }
            digits += 1;
            escape_index += 1;
        }

        if digits == 0 || escape_index >= length {
            return Err(invalid_group_name_error());
        }

        return Ok(RegExpNameElement {
            kind: classify_escaped_value(value),
            origin: RegExpNameElementOrigin::BracedEscape,
            next_index: escape_index + 1,
        });
    }

    let Some(hex_digits) = pattern.get(escape_index..escape_index + 4) else {
        return Err(invalid_group_name_error());
    };
    let mut value = 0u32;
    for &digit in hex_digits {
        let Some(digit) = ascii_hex_digit_value(digit) else {
            return Err(invalid_group_name_error());
        };
        value = value * 16 + digit;
    }

    Ok(RegExpNameElement {
        kind: classify_escaped_value(value),
        origin: RegExpNameElementOrigin::FixedEscape,
        next_index: escape_index + 4,
    })
}

/// Checks the group name starting at name_start, and returns the index just past its closing '>'.
fn validate_regexp_name_surrogates(pattern: &[u16], name_start: usize) -> Result<usize, ParseRegexPatternError> {
    let mut index = name_start;

    while index < pattern.len() {
        if equals_ascii(pattern[index], b'>') {
            return Ok(index + 1);
        }

        let element = parse_regexp_name_element(pattern, index)?;
        if element.kind == RegExpNameElementKind::CodePoint {
            index = element.next_index;
            continue;
        }

        if element.kind == RegExpNameElementKind::LowSurrogate {
            return Err(invalid_group_name_error());
        }

        let next_element = parse_regexp_name_element(pattern, element.next_index)?;
        if next_element.kind != RegExpNameElementKind::LowSurrogate
            || element.origin != next_element.origin
            || element.origin == RegExpNameElementOrigin::BracedEscape
        {
            return Err(invalid_group_name_error());
        }

        index = next_element.next_index;
    }

    Err(invalid_group_name_error())
}

fn starts_named_group_or_lookbehind(pattern: &[u16], index: usize) -> bool {
    equals_ascii(pattern[index], b'(')
        && pattern.get(index + 1).is_some_and(|&next| equals_ascii(next, b'?'))
        && pattern.get(index + 2).is_some_and(|&next| equals_ascii(next, b'<'))
}

fn starts_lookbehind(pattern: &[u16], index: usize) -> bool {
    starts_named_group_or_lookbehind(pattern, index)
        && pattern
            .get(index + 3)
            .is_some_and(|&next| equals_ascii(next, b'=') || equals_ascii(next, b'!'))
}

fn pattern_has_named_capture_groups(pattern: &[u16]) -> bool {
    let mut in_character_class = false;
    let mut index = 0;

    while index < pattern.len() {
        let code_unit = pattern[index];

        if equals_ascii(code_unit, b'\\') {
            index += 2;
            continue;
        }

        if equals_ascii(code_unit, b'[') && !in_character_class {
            in_character_class = true;
        } else if equals_ascii(code_unit, b']') && in_character_class {
            in_character_class = false;
        } else if !in_character_class
            && starts_named_group_or_lookbehind(pattern, index)
            && !starts_lookbehind(pattern, index)
        {
            return true;
        }

        index += 1;
    }

    false
}

fn validate_named_group_name_surrogates(pattern: &[u16], unicode_aware: bool) -> Result<(), ParseRegexPatternError> {
    let mut in_character_class = false;
    let has_named_groups_or_unicode = unicode_aware || pattern_has_named_capture_groups(pattern);
    let mut index = 0;

    while index < pattern.len() {
        let code_unit = pattern[index];

        if equals_ascii(code_unit, b'\\') {
            if has_named_groups_or_unicode
                && !in_character_class
                && pattern.get(index + 1).is_some_and(|&next| equals_ascii(next, b'k'))
                && pattern.get(index + 2).is_some_and(|&next| equals_ascii(next, b'<'))
            {
                index = validate_regexp_name_surrogates(pattern, index + 3)?;
                continue;
            }

            index += 2;
            continue;
        }

        if equals_ascii(code_unit, b'[') && !in_character_class {
            in_character_class = true;
        } else if equals_ascii(code_unit, b']') && in_character_class {
            in_character_class = false;
        } else if !in_character_class
            && starts_named_group_or_lookbehind(pattern, index)
            && index + 3 < pattern.len()
            && !starts_lookbehind(pattern, index)
        {
            index = validate_regexp_name_surrogates(pattern, index + 3)?;
            continue;
        }

        index += 1;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utf16(text: &str) -> Vec<u16> {
        text.encode_utf16().collect()
    }

    fn parse(pattern: &[u16], unicode: bool, unicode_sets: bool) -> Result<String, String> {
        parse_regex_pattern(pattern, unicode, unicode_sets)
            .map(|parsed| String::from_utf16(&parsed).expect("a parsed pattern is ASCII"))
            .map_err(|error| error.error)
    }

    fn parse_in_every_mode(pattern: &[u16]) -> [Result<String, String>; 3] {
        [
            parse(pattern, false, false),
            parse(pattern, true, false),
            parse(pattern, false, true),
        ]
    }

    #[test]
    fn ascii_patterns_are_unchanged() {
        for pattern in [
            r"a+(b)\d",
            r"(?<name>x)\k<name>",
            r"[\]\\]",
            r"\\",
            "",
            r"\u{1F600}\ud83d",
        ] {
            for parsed in parse_in_every_mode(&utf16(pattern)) {
                assert_eq!(parsed.as_deref(), Ok(pattern));
            }
        }
    }

    #[test]
    fn non_ascii_code_units_are_escaped() {
        assert_eq!(
            parse_in_every_mode(&utf16("aé€b")),
            [
                Ok("a\\u00e9\\u20acb".to_owned()),
                Ok(r"a\u{00e9}\u{20ac}b".to_owned()),
                Ok(r"a\u{00e9}\u{20ac}b".to_owned()),
            ]
        );
    }

    #[test]
    fn surrogate_pairs_combine_only_in_unicode_modes() {
        assert_eq!(
            parse_in_every_mode(&utf16("😀")),
            [
                Ok("\\ud83d\\ude00".to_owned()),
                Ok(r"\u{1f600}".to_owned()),
                Ok(r"\u{1f600}".to_owned()),
            ]
        );
        assert_eq!(
            parse_in_every_mode(&[0xd83d, u16::from(b'a'), 0xde00]),
            [
                Ok(r"\ud83da\ude00".to_owned()),
                Ok(r"\u{d83d}a\u{de00}".to_owned()),
                Ok(r"\u{d83d}a\u{de00}".to_owned()),
            ]
        );
        assert_eq!(
            parse(&[0xde00, 0xd83d], true, false),
            Ok(r"\u{de00}\u{d83d}".to_owned())
        );
    }

    #[test]
    fn a_backslash_before_a_non_ascii_code_unit_is_dropped() {
        assert_eq!(parse(&utf16(r"\é"), false, false), Ok("\\u00e9".to_owned()));
        assert_eq!(parse(&utf16(r"\é"), true, false), Ok(r"\u{00e9}".to_owned()));
        assert_eq!(parse(&utf16(r"\\é"), false, false), Ok("\\\\\\u00e9".to_owned()));
        assert_eq!(parse(&utf16(r"\\\é"), false, false), Ok("\\\\\\u00e9".to_owned()));
        assert_eq!(parse(&utf16(r"\😀"), true, false), Ok(r"\u{1f600}".to_owned()));
        assert_eq!(parse(&utf16(r"é\é"), false, false), Ok("\\u00e9\\u00e9".to_owned()));
    }

    #[test]
    fn unicode_and_unicode_sets_are_incompatible() {
        assert_eq!(
            parse(&utf16("a"), true, true),
            Err("RegExp flag 'u' is incompatible with flag 'v'".to_owned())
        );
    }

    #[test]
    fn group_names_need_whole_surrogate_pairs_written_the_same_way() {
        let accepted = [
            "(?<\\ud835\\udc9c>a)",
            "(?<𝒜>a)",
            r"(?<\u{1d49c}>a)",
            "(?<a\\u0062>a)\\k<ab>",
            r"(?<=\ud835)",
            r"(?<!\ud835)",
            r"[(?<\ud835>)]",
        ];
        for pattern in accepted {
            for parsed in parse_in_every_mode(&utf16(pattern)) {
                assert!(parsed.is_ok(), "{pattern} should be accepted, got {parsed:?}");
            }
        }

        let rejected = [
            r"(?<\ud835>a)",
            r"(?<\udc9c>a)",
            r"(?<\u{d835}\u{dc9c}>a)",
            r"(?<\ud835\u{dc9c}>a)",
            "(?<\\ud835\\udc9c",
            r"(?<\u{}>a)",
            r"(?<\u{110000}>a)",
            r"(?<\u12>a)",
            r"(?<\x41>a)",
            r"(?<a>.)\k<\udc9c>",
            // The C++ classifies a braced escape by its low 16 bits, so U+1DF00 reads as a low surrogate.
            r"(?<\u{1DF00}>a)",
        ];
        for pattern in rejected {
            for parsed in parse_in_every_mode(&utf16(pattern)) {
                assert_eq!(
                    parsed,
                    Err("invalid group name".to_owned()),
                    "{pattern} should be rejected"
                );
            }
        }

        let mut lone_literal_high_surrogate = utf16("(?<");
        lone_literal_high_surrogate.extend([0xd835, u16::from(b'>'), u16::from(b'a'), u16::from(b')')]);
        assert_eq!(
            parse(&lone_literal_high_surrogate, false, false),
            Err("invalid group name".to_owned())
        );
    }

    #[test]
    fn backreference_names_are_checked_only_with_named_groups_or_unicode() {
        let pattern = utf16(r"\k<\ud835>");
        assert_eq!(parse(&pattern, false, false), Ok(r"\k<\ud835>".to_owned()));
        assert_eq!(parse(&pattern, true, false), Err("invalid group name".to_owned()));
        assert_eq!(parse(&pattern, false, true), Err("invalid group name".to_owned()));
        assert_eq!(
            parse(&utf16(r"(?<a>.)\k<\ud835>"), false, false),
            Err("invalid group name".to_owned())
        );
        assert_eq!(
            parse(&utf16(r"(?<=a)\k<\ud835>"), false, false),
            Ok(r"(?<=a)\k<\ud835>".to_owned())
        );
    }
}

/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

// Prints the expectation tables of the value operator tests, computed by the C++ runtime. Run it through
// regenerate.sh, which splits the output into one file per "//// FILE:" marker.
//
// Values and outcomes are written in one small notation that runtime/value.rs reads back: undefined, null, true,
// false, n:<Number::toString or -0>, s:<string>, b:<decimal BigInt>, sym:<description>, obj, and !<index into
// ERRORS> for a thrown error. Strings escape every code unit outside printable ASCII, and the backslash, the bar that
// separates outcomes and the tilde, as \uXXXX.

"use strict";

function escapeCodeUnits(text) {
    let result = "";
    for (let i = 0; i < text.length; ++i) {
        const codeUnit = text.charCodeAt(i);
        if (codeUnit >= 0x20 && codeUnit < 0x7e && codeUnit !== 0x5c && codeUnit !== 0x7c) result += text[i];
        else result += "\\u" + codeUnit.toString(16).padStart(4, "0");
    }
    return result;
}

function rustString(text) {
    return '"' + text.replaceAll("\\", "\\\\").replaceAll('"', '\\"') + '"';
}

function describe(value) {
    switch (typeof value) {
        case "undefined":
            return "undefined";
        case "boolean":
            return String(value);
        case "number":
            return "n:" + (Object.is(value, -0) ? "-0" : String(value));
        case "string":
            return "s:" + escapeCodeUnits(value);
        case "bigint":
            return "b:" + String(value);
        case "symbol":
            return "sym:" + escapeCodeUnits(value.description);
        default:
            return value === null ? "null" : "obj";
    }
}

// The Rust runtime reports the message of an error it throws in UTF-8, so lone surrogates in it compare as U+FFFD.
const errors = [];
function errorIndex(error) {
    const text = escapeCodeUnits(`${error.name}: ${error.message}`.toWellFormed());
    let index = errors.indexOf(text);
    if (index < 0) {
        index = errors.length;
        errors.push(text);
    }
    return index;
}

function outcome(operation) {
    try {
        return describe(operation());
    } catch (error) {
        return "!" + errorIndex(error);
    }
}

const symbol = Symbol("s");
const objectWithoutMethods = Object.create(null);

const operands = [
    undefined,
    null,
    true,
    false,
    0,
    -0,
    1,
    -1,
    1.5,
    -2.5,
    2147483647,
    -2147483648,
    4294967296,
    2 ** 53,
    NaN,
    Infinity,
    -Infinity,
    5e-324,
    1e21,
    "",
    "0",
    " 12 ",
    "1.5",
    "abc",
    "0x1f",
    "-0",
    "Infinity",
    "9007199254740993",
    "\ud800",
    0n,
    1n,
    -3n,
    2n ** 64n,
    9007199254740993n,
    symbol,
    objectWithoutMethods,
];

// The source text of an operand in a script, or null for the operands that need builtins to create.
function sourceOf(value) {
    switch (typeof value) {
        case "number":
            if (Number.isNaN(value)) return "0 / 0";
            if (value === Infinity) return "1 / 0";
            if (value === -Infinity) return "-1 / 0";
            if (Object.is(value, -0)) return "-0";
            return String(value);
        case "string": {
            let result = '"';
            for (let i = 0; i < value.length; ++i) {
                const codeUnit = value.charCodeAt(i);
                if (codeUnit >= 0x20 && codeUnit < 0x7f && codeUnit !== 0x22 && codeUnit !== 0x5c) result += value[i];
                else result += "\\u" + codeUnit.toString(16).padStart(4, "0");
            }
            return result + '"';
        }
        case "bigint":
            return String(value) + "n";
        case "symbol":
            return null;
        default:
            return value === null || value === undefined || typeof value === "boolean" ? String(value) : null;
    }
}

const binaryOperators = [
    ["+", (a, b) => a + b],
    ["-", (a, b) => a - b],
    ["*", (a, b) => a * b],
    ["/", (a, b) => a / b],
    ["%", (a, b) => a % b],
    ["**", (a, b) => a ** b],
    ["&", (a, b) => a & b],
    ["|", (a, b) => a | b],
    ["^", (a, b) => a ^ b],
    ["<<", (a, b) => a << b],
    [">>", (a, b) => a >> b],
    [">>>", (a, b) => a >>> b],
    ["<", (a, b) => a < b],
    ["<=", (a, b) => a <= b],
    [">", (a, b) => a > b],
    [">=", (a, b) => a >= b],
    ["==", (a, b) => a == b],
    ["!=", (a, b) => a != b],
    ["===", (a, b) => a === b],
    ["!==", (a, b) => a !== b],
    ["in", (a, b) => a in b],
    ["instanceof", (a, b) => a instanceof b],
];

function storeInTypedArray(TypedArray, x) {
    const array = new TypedArray(1);
    array[0] = x;
    return array[0];
}

const unaryOperations = [
    ["-x", x => -x],
    ["+x", x => +x],
    ["~x", x => ~x],
    ["!x", x => !x],
    ["typeof x", x => typeof x],
    [
        "ToNumeric(x), as x++ evaluates to",
        x => {
            let y = x;
            return y++;
        },
    ],
    [
        "++x",
        x => {
            let y = x;
            return ++y;
        },
    ],
    [
        "--x",
        x => {
            let y = x;
            return --y;
        },
    ],
    ["ToString(x)", x => `${x}`],
    ["ToPropertyKey(x)", x => Reflect.ownKeys({ [x]: 0 })[0]],
    ["ToBigInt(x)", x => BigInt.asIntN(4096, x)],
    ["ToBigInt64(x)", x => storeInTypedArray(BigInt64Array, x)],
    ["ToBigUint64(x)", x => storeInTypedArray(BigUint64Array, x)],
    ["ToInt8(x)", x => storeInTypedArray(Int8Array, x)],
    ["ToUint8(x)", x => storeInTypedArray(Uint8Array, x)],
    ["ToUint8Clamp(x)", x => storeInTypedArray(Uint8ClampedArray, x)],
    ["ToInt16(x)", x => storeInTypedArray(Int16Array, x)],
    ["ToUint16(x)", x => storeInTypedArray(Uint16Array, x)],
    ["ToInt32(x)", x => storeInTypedArray(Int32Array, x)],
    ["ToUint32(x)", x => storeInTypedArray(Uint32Array, x)],
];

const canonicalNumericIndexStringKeys = [
    "",
    "0",
    "-0",
    "1",
    "01",
    "00",
    "-00",
    "1.5",
    "-1",
    "-1.5",
    "0.5",
    "-0.5",
    "0.",
    ".5",
    "1.0",
    "-",
    "+1",
    "Infinity",
    "-Infinity",
    "+Infinity",
    "NaN",
    "-NaN",
    "1e21",
    "1e+21",
    "1e-7",
    "0.0000001",
    "123456789012345680000",
    "4294967294",
    "4294967295",
    "4294967296",
    "9007199254740993",
    "abc",
    "0x10",
    " 1",
    "1 ",
    "-0.0",
    "5e-324",
];

// An integer index key is an index; a non-index key is numeric when a typed array ignores setting it.
function canonicalNumericIndexString(key) {
    if (String(Number(key)) === key && Number.isInteger(Number(key)) && Number(key) >= 0 && Number(key) < 4294967295)
        return "index";
    const array = new Int8Array(1);
    array[key] = 1;
    return Object.prototype.hasOwnProperty.call(array, key) ? "undefined" : "numeric";
}

// The interpreter computes some operators on numbers itself, without calling the runtime's operator, and the two do
// not always agree. Operands whose valueOf() returns the numbers reach the operator, so for each pair of numbers an
// outcome is the operator's, followed by ~ and the interpreter's when they differ. The operators listed here treat
// objects differently from the numbers their valueOf() returns.
const operatorsOnObjects = ["==", "!=", "===", "!==", "in", "instanceof"];

const binaryRows = [];
for (let lhsIndex = 0; lhsIndex < operands.length; ++lhsIndex) {
    for (let rhsIndex = 0; rhsIndex < operands.length; ++rhsIndex) {
        const [lhs, rhs] = [operands[lhsIndex], operands[rhsIndex]];
        const outcomes = binaryOperators.map(([name, operation]) => {
            const interpreterOutcome = outcome(() => operation(lhs, rhs));
            if (typeof lhs !== "number" || typeof rhs !== "number" || operatorsOnObjects.includes(name))
                return interpreterOutcome;
            const operatorOutcome = outcome(() => operation({ valueOf: () => lhs }, { valueOf: () => rhs }));
            return operatorOutcome === interpreterOutcome
                ? operatorOutcome
                : `${operatorOutcome}~${interpreterOutcome}`;
        });
        binaryRows.push(`    (${lhsIndex}, ${rhsIndex}, ${rustString(outcomes.join("|"))}),`);
    }
}
const unaryRows = operands.map(
    (operand, index) =>
        `    (${index}, ${rustString(unaryOperations.map(([, operation]) => outcome(() => operation(operand))).join("|"))}),`
);

print("//// FILE: value_operators_table.rs");
print("/*");
print(" * Copyright (c) 2026-present, the Ladybird developers.");
print(" *");
print(" * SPDX-License-Identifier: BSD-2-Clause");
print(" */");
print("");
print("// Generated by value_operators.js, which runs on the C++ js binary. Regenerate with regenerate.sh.");
print("");
print("/// The operands, and their source text in a script where they need no builtins.");
print("const OPERANDS: &[(&str, Option<&str>)] = &[");
for (const operand of operands) {
    const source = sourceOf(operand);
    print(`    (${rustString(describe(operand))}, ${source === null ? "None" : `Some(${rustString(source)})`}),`);
}
print("];");
print("");
print("const ERRORS: &[&str] = &[");
// The errors are only known once every outcome is computed, so they are printed after the rows below are built.
for (const error of errors) print(`    ${rustString(error)},`);
print("];");
print("");
print(`const BINARY_OPERATORS: &[&str] = &[${binaryOperators.map(([name]) => rustString(name)).join(", ")}];`);
print("");
print("/// The outcome of each binary operator, in BINARY_OPERATORS order, for each pair of operands.");
print("const BINARY_OPERATOR_OUTCOMES: &[(usize, usize, &str)] = &[");
for (const row of binaryRows) print(row);
print("];");
print("");
print(`const UNARY_OPERATIONS: &[&str] = &[${unaryOperations.map(([name]) => rustString(name)).join(", ")}];`);
print("");
print("/// The outcome of each unary operation, in UNARY_OPERATIONS order, for each operand.");
print("const UNARY_OPERATION_OUTCOMES: &[(usize, &str)] = &[");
for (const row of unaryRows) print(row);
print("];");
print("");
print("/// CanonicalNumericIndexString(key) in DetectNumericRoundtrip mode, for string keys.");
print("const CANONICAL_NUMERIC_INDEX_STRING: &[(&str, &str)] = &[");
for (const key of canonicalNumericIndexStringKeys)
    print(`    (${rustString(key)}, ${rustString(canonicalNumericIndexString(key))}),`);
print("];");

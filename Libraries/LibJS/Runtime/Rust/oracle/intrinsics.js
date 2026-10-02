/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

// Prints the prototype and the own properties of the intrinsics runtime/intrinsics.rs creates, and of the global
// object, as the C++ runtime has them, for the tests that compare the Rust realm against them. Run it with the C++ js
// binary, through regenerate.sh.

const GeneratorFunction = Object.getPrototypeOf(function* () {}).constructor;
const AsyncGeneratorFunction = Object.getPrototypeOf(async function* () {}).constructor;
const AsyncFunction = Object.getPrototypeOf(async function () {}).constructor;
const ArrayIteratorPrototype = Object.getPrototypeOf([][Symbol.iterator]());
const IteratorPrototype = Object.getPrototypeOf(ArrayIteratorPrototype);
const GeneratorPrototype = GeneratorFunction.prototype.prototype;
const AsyncGeneratorPrototype = AsyncGeneratorFunction.prototype.prototype;
const AsyncIteratorPrototype = Object.getPrototypeOf(AsyncGeneratorPrototype);
const ThrowTypeError = Object.getOwnPropertyDescriptor(Function.prototype, "caller").get;
const MapIteratorPrototype = Object.getPrototypeOf(new Map().entries());
const SetIteratorPrototype = Object.getPrototypeOf(new Set().values());
const StringIteratorPrototype = Object.getPrototypeOf(""[Symbol.iterator]());
const RegExpStringIteratorPrototype = Object.getPrototypeOf(/a/[Symbol.matchAll](""));
const IteratorHelperPrototype = Object.getPrototypeOf([].values().map(x => x));
const WrapForValidIteratorPrototype = Object.getPrototypeOf(Iterator.from({ next() {} }));
const TypedArray = Object.getPrototypeOf(Int8Array);

const objects = [
    ["Object.prototype", Object.prototype],
    ["Function.prototype", Function.prototype],
    ["Array.prototype", Array.prototype],
    ["String.prototype", String.prototype],
    ["Number.prototype", Number.prototype],
    ["Boolean.prototype", Boolean.prototype],
    ["Symbol.prototype", Symbol.prototype],
    ["BigInt.prototype", BigInt.prototype],
    ["Error.prototype", Error.prototype],
    ["EvalError.prototype", EvalError.prototype],
    ["InternalError.prototype", InternalError.prototype],
    ["RangeError.prototype", RangeError.prototype],
    ["ReferenceError.prototype", ReferenceError.prototype],
    ["SyntaxError.prototype", SyntaxError.prototype],
    ["TypeError.prototype", TypeError.prototype],
    ["URIError.prototype", URIError.prototype],
    ["AggregateError.prototype", AggregateError.prototype],
    ["Date.prototype", Date.prototype],
    ["%IteratorPrototype%", IteratorPrototype],
    ["%ArrayIteratorPrototype%", ArrayIteratorPrototype],
    ["%AsyncIteratorPrototype%", AsyncIteratorPrototype],
    ["%IteratorHelperPrototype%", IteratorHelperPrototype],
    ["%MapIteratorPrototype%", MapIteratorPrototype],
    ["%RegExpStringIteratorPrototype%", RegExpStringIteratorPrototype],
    ["%SetIteratorPrototype%", SetIteratorPrototype],
    ["%StringIteratorPrototype%", StringIteratorPrototype],
    ["%WrapForValidIteratorPrototype%", WrapForValidIteratorPrototype],
    ["%GeneratorFunction.prototype%", GeneratorFunction.prototype],
    ["%AsyncGeneratorFunction.prototype%", AsyncGeneratorFunction.prototype],
    ["%AsyncFunction.prototype%", AsyncFunction.prototype],
    ["%GeneratorPrototype%", GeneratorPrototype],
    ["%AsyncGeneratorPrototype%", AsyncGeneratorPrototype],
    ["Object", Object],
    ["Function", Function],
    ["Array", Array],
    ["String", String],
    ["Number", Number],
    ["Boolean", Boolean],
    ["Symbol", Symbol],
    ["BigInt", BigInt],
    ["Error", Error],
    ["EvalError", EvalError],
    ["InternalError", InternalError],
    ["RangeError", RangeError],
    ["ReferenceError", ReferenceError],
    ["SyntaxError", SyntaxError],
    ["TypeError", TypeError],
    ["URIError", URIError],
    ["AggregateError", AggregateError],
    ["Date", Date],
    ["Iterator", Iterator],
    ["%GeneratorFunction%", GeneratorFunction],
    ["%AsyncGeneratorFunction%", AsyncGeneratorFunction],
    ["%AsyncFunction%", AsyncFunction],
    ["Proxy", Proxy],
    ["%ThrowTypeError%", ThrowTypeError],
    ["ArrayBuffer.prototype", ArrayBuffer.prototype],
    ["SharedArrayBuffer.prototype", SharedArrayBuffer.prototype],
    ["DataView.prototype", DataView.prototype],
    ["%TypedArray.prototype%", TypedArray.prototype],
    ["Uint8Array.prototype", Uint8Array.prototype],
    ["Uint8ClampedArray.prototype", Uint8ClampedArray.prototype],
    ["Uint16Array.prototype", Uint16Array.prototype],
    ["Uint32Array.prototype", Uint32Array.prototype],
    ["BigUint64Array.prototype", BigUint64Array.prototype],
    ["Int8Array.prototype", Int8Array.prototype],
    ["Int16Array.prototype", Int16Array.prototype],
    ["Int32Array.prototype", Int32Array.prototype],
    ["BigInt64Array.prototype", BigInt64Array.prototype],
    ["Float16Array.prototype", Float16Array.prototype],
    ["Float32Array.prototype", Float32Array.prototype],
    ["Float64Array.prototype", Float64Array.prototype],
    ["ArrayBuffer", ArrayBuffer],
    ["SharedArrayBuffer", SharedArrayBuffer],
    ["DataView", DataView],
    ["%TypedArray%", TypedArray],
    ["Uint8Array", Uint8Array],
    ["Uint8ClampedArray", Uint8ClampedArray],
    ["Uint16Array", Uint16Array],
    ["Uint32Array", Uint32Array],
    ["BigUint64Array", BigUint64Array],
    ["Int8Array", Int8Array],
    ["Int16Array", Int16Array],
    ["Int32Array", Int32Array],
    ["BigInt64Array", BigInt64Array],
    ["Float16Array", Float16Array],
    ["Float32Array", Float32Array],
    ["Float64Array", Float64Array],
    ["Atomics", Atomics],
    ["globalThis", globalThis],
];

// NB: Everything here is a lexical declaration, so that the script adds no properties to the global object it prints.
const name_of = value => {
    for (const [name, object] of objects) {
        if (object === value) return name;
    }
    return null;
};

const describe = value => {
    if (value === null) return "null";
    if (typeof value === "function" || typeof value === "object") return name_of(value) ?? typeof value;
    if (typeof value === "string") return JSON.stringify(value);
    return String(value);
};

const flags = descriptor => {
    let result = "";
    if ("value" in descriptor) result += descriptor.writable ? "w" : "-";
    result += descriptor.enumerable ? "e" : "-";
    result += descriptor.configurable ? "c" : "-";
    return result;
};

const describe_property = (object, key) => {
    const descriptor = Object.getOwnPropertyDescriptor(object, key);
    const shown_key = typeof key === "symbol" ? `@@${key.description.slice("Symbol.".length)}` : key;
    if ("value" in descriptor) return `${shown_key}=${describe(descriptor.value)}:${flags(descriptor)}`;
    return `${shown_key}=<${descriptor.get ? "get" : ""}${descriptor.set ? "set" : ""}>:${flags(descriptor)}`;
};

const escape = text => JSON.stringify(text);

const lines = [
    "//// FILE: intrinsics_table.rs",
    "/*",
    " * Copyright (c) 2026-present, the Ladybird developers.",
    " *",
    " * SPDX-License-Identifier: BSD-2-Clause",
    " */",
    "",
    "// Generated by intrinsics.js, which runs on the C++ js binary. Regenerate with regenerate.sh.",
    "",
    "/// Each intrinsic, its [[Prototype]], and its own properties in [[OwnPropertyKeys]] order as key=value:flags, where",
    "/// the flags are writable, enumerable and configurable, and an accessor's value is <get>, <set> or <getset>. The",
    "/// global object is the C++ js binary's, which has the properties of the js host as well.",
    "const INTRINSICS: &[(&str, &str, &[&str])] = &[",
];
for (const [name, object] of objects) {
    lines.push(`    (`);
    lines.push(`        ${escape(name)},`);
    lines.push(`        ${escape(describe(Object.getPrototypeOf(object)))},`);
    lines.push(`        &[`);
    for (const key of Reflect.ownKeys(object)) lines.push(`            ${escape(describe_property(object, key))},`);
    lines.push(`        ],`);
    lines.push(`    ),`);
}
lines.push("];");
for (const line of lines) print(line);

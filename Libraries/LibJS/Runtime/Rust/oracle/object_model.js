/*
 * Copyright (c) 2026-present, the Ladybird developers.
 *
 * SPDX-License-Identifier: BSD-2-Clause
 */

// Prints what the C++ runtime observes in the object model scenarios that runtime/object.rs replays through the
// Rust object model, one line per scenario. Run it with the C++ js binary:
//     js --raw-strings --disable-ansi-colors object_model.js

"use strict";

const keys = o => Reflect.ownKeys(o).map(String).join(",");
const out = [];
const s = Symbol("s");

// 1. Integer keys ascending, then strings in insertion order, then symbols; re-adding moves a key last.
{
    const o = {};
    o.b = 1;
    o.a = 2;
    o[2] = 3;
    o[1] = 4;
    o[s] = 5;
    o.c = 6;
    delete o.b;
    o.b = 7;
    o[0] = 8;
    out.push("1 " + keys(o));
}
// 2. Dictionary mode after many properties, deletes and re-adds.
{
    const o = {};
    for (let i = 0; i < 70; ++i) o["p" + i] = i;
    delete o.p5;
    delete o.p66;
    o.p5 = "again";
    out.push(
        "2 " +
            Object.keys(o).length +
            " " +
            Object.keys(o).slice(0, 6).join(",") +
            " ... " +
            Object.keys(o).slice(-4).join(",")
    );
}
// 3. The largest array index is a number; one more is a string key.
{
    const o = {};
    o.x = 1;
    o[4294967295] = 2;
    o[4294967294] = 3;
    o[10] = 4;
    o["01"] = 5;
    out.push("3 " + keys(o));
}
// 4. Arrays grow their length with indices, and holes are not keys.
{
    const a = [];
    a[3] = 1;
    const length_after_index = a.length;
    a.length = 10;
    a[500] = 2;
    out.push("4 " + length_after_index + " " + a.length + " " + keys(a));
}
// 5. A non-configurable element stops a shrinking length.
{
    const a = [0, 1, 2, 3];
    Object.defineProperty(a, 1, { configurable: false });
    const result = Reflect.defineProperty(a, "length", { value: 0 });
    out.push("5 " + result + " " + a.length + " " + keys(a));
}
// 6. A non-writable length stops new indices.
{
    const a = [0, 1];
    Object.defineProperty(a, "length", { writable: false });
    const set_result = Reflect.set(a, 5, 1);
    const define_result = Reflect.defineProperty(a, 1, { value: 9 });
    const grow_result = Reflect.defineProperty(a, "length", { value: 3 });
    const same_result = Reflect.defineProperty(a, "length", { value: 2 });
    out.push(
        "6 " + set_result + " " + define_result + " " + grow_result + " " + same_result + " " + a[1] + " " + keys(a)
    );
}
// 7. Integrity levels.
{
    const o = { a: 1 };
    Object.defineProperty(o, "g", { get: undefined, set: undefined, enumerable: true, configurable: true });
    Object.seal(o);
    const sealed = [
        Object.isSealed(o),
        Object.isFrozen(o),
        Object.getOwnPropertyDescriptor(o, "a").writable,
        Object.getOwnPropertyDescriptor(o, "a").configurable,
    ];
    Object.freeze(o);
    const frozen = [
        Object.isSealed(o),
        Object.isFrozen(o),
        Object.getOwnPropertyDescriptor(o, "a").writable,
        Object.isExtensible(o),
        Reflect.set(o, "a", 2),
        Reflect.deleteProperty(o, "a"),
    ];
    out.push("7 " + sealed.join(",") + " " + frozen.join(",") + " " + keys(o));
}
// 8. Holes and deletes in arrays.
{
    const a = [1, , 3];
    const before = keys(a);
    delete a[0];
    out.push("8 " + before + " " + keys(a) + " " + a.length);
}
// 9. "length" comes after the indices of an array.
{
    const a = [1, 2];
    a.x = 1;
    a[s] = 2;
    out.push("9 " + keys(a));
}
// 10. Changing attributes keeps a property's place.
{
    const o = { a: 1, b: 2 };
    Object.defineProperty(o, "a", { enumerable: false });
    const d = {};
    for (let i = 0; i < 70; ++i) d["q" + i] = i;
    Object.defineProperty(d, "q0", { enumerable: false, value: "x" });
    out.push(
        "10 " +
            Object.keys(o).join(",") +
            " " +
            keys(o) +
            " " +
            Object.keys(d)[0] +
            " " +
            Reflect.ownKeys(d)[0] +
            " " +
            d.q0
    );
}
// 11. Accessors without functions.
{
    const o = {};
    Object.defineProperty(o, "x", { get: undefined, configurable: true });
    const get = o.x;
    const set_result = Reflect.set(o, "x", 1);
    Object.defineProperty(o, "x", { value: 5 });
    const descriptor = Object.getOwnPropertyDescriptor(o, "x");
    out.push(
        "11 " +
            get +
            " " +
            set_result +
            " " +
            descriptor.value +
            " " +
            descriptor.writable +
            " " +
            descriptor.enumerable +
            " " +
            descriptor.configurable
    );
}
// 12. Prototype chain gets and sets, and shadowing.
{
    const p = { inherited: 1 };
    Object.defineProperty(p, "readonly", { value: 2, writable: false });
    const o = Object.create(p);
    const read = o.inherited;
    const set_readonly = Reflect.set(o, "readonly", 3);
    o.inherited = 4;
    out.push(
        "12 " +
            read +
            " " +
            set_readonly +
            " " +
            o.inherited +
            " " +
            p.inherited +
            " " +
            keys(o) +
            " " +
            ("inherited" in o) +
            " " +
            ("missing" in o)
    );
}
// 13. Sealing an array with holes and testing it.
{
    const a = [1, , 3];
    Object.freeze(a);
    out.push(
        "13 " +
            Object.isFrozen(a) +
            " " +
            Reflect.set(a, 0, 9) +
            " " +
            a[0] +
            " " +
            Reflect.defineProperty(a, "length", { value: 0 }) +
            " " +
            a.length +
            " " +
            keys(a)
    );
}
// 14. Sparse arrays in dictionary mode shrink and grow.
{
    const a = [];
    a[1000] = 1;
    a[2] = 2;
    a.length = 500;
    const after_shrink = keys(a);
    a[700] = 3;
    out.push("14 " + after_shrink + " " + a.length + " " + keys(a));
}
// 15. Prevent extensions blocks new properties but not changes.
{
    const o = { a: 1 };
    Object.preventExtensions(o);
    out.push(
        "15 " +
            Reflect.set(o, "b", 1) +
            " " +
            Reflect.set(o, "a", 2) +
            " " +
            o.a +
            " " +
            Reflect.defineProperty(o, 0, { value: 1 }) +
            " " +
            keys(o) +
            " " +
            Object.isSealed(o)
    );
}
// 16. Object.defineProperties applies descriptors in key order.
{
    const o = {};
    Object.defineProperties(o, {
        b: { value: 1, enumerable: true },
        1: { value: 2, enumerable: true },
        a: { value: 3 },
    });
    out.push("16 " + keys(o) + " " + Object.keys(o).join(","));
}
print(out.join("\n"));

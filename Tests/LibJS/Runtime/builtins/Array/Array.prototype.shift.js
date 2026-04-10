test("length is 0", () => {
    expect(Array.prototype.shift).toHaveLength(0);
});

describe("normal behavior", () => {
    test("array with elements", () => {
        var a = [1, 2, 3];
        expect(a.shift()).toBe(1);
        expect(a).toEqual([2, 3]);
    });

    test("empty array", () => {
        var a = [];
        expect(a.shift()).toBeUndefined();
        expect(a).toEqual([]);
    });

    test("array with empty slot", () => {
        var a = [,];
        expect(a.shift()).toBeUndefined();
        expect(a).toEqual([]);
    });
});

test("Issue #5884, GenericIndexedPropertyStorage::take_first() loses elements", () => {
    const a = [];
    for (let i = 0; i < 300; i++) {
        // NOTE: We use defineProperty to prevent the array from using SimpleIndexedPropertyStorage
        Object.defineProperty(a, i, { value: i, configurable: true, writable: true });
    }
    expect(a.length).toBe(300);
    for (let i = 0; i < 300; i++) {
        a.shift();
    }
    expect(a.length).toBe(0);
});

test("throws if the array length is not writable", () => {
    var a = [1, 2];
    Object.defineProperty(a, "length", { writable: false });

    expect(() => {
        a.shift();
    }).toThrow(TypeError);
    expect(a[0]).toBe(2);
    expect(a[1]).toBeUndefined();
    expect(1 in a).toBeFalse();
    expect(a.length).toBe(2);
});

describe("post-shift storage consistency", () => {
    test("shift then push preserves contents", () => {
        const a = [1, 2, 3, 4, 5];
        expect(a.shift()).toBe(1);
        a.push(6);
        expect(a).toEqual([2, 3, 4, 5, 6]);
    });

    test("shift then indexed write", () => {
        const a = [1, 2, 3, 4];
        a.shift();
        a[0] = 99;
        expect(a).toEqual([99, 3, 4]);
    });

    test("shift then indexed read", () => {
        const a = [10, 20, 30];
        a.shift();
        expect(a[0]).toBe(20);
        expect(a[1]).toBe(30);
        expect(a.length).toBe(2);
    });

    test("many shifts then push triggers compaction", () => {
        const a = new Array(1000);
        for (let i = 0; i < 1000; i++) a[i] = i;
        for (let i = 0; i < 500; i++) a.shift();
        expect(a.length).toBe(500);
        expect(a[0]).toBe(500);
        expect(a[499]).toBe(999);
        for (let i = 0; i < 2000; i++) a.push(10000 + i);
        expect(a.length).toBe(2500);
        expect(a[499]).toBe(999);
        expect(a[500]).toBe(10000);
        expect(a[2499]).toBe(11999);
    });

    test("shift then splice", () => {
        const a = [1, 2, 3, 4, 5];
        a.shift();
        a.splice(1, 1);
        expect(a).toEqual([2, 4, 5]);
    });

    test("for-of after shifts", () => {
        const a = [10, 20, 30, 40];
        a.shift();
        a.shift();
        const out = [];
        for (const v of a) out.push(v);
        expect(out).toEqual([30, 40]);
    });

    test("shift on holey array preserves holes", () => {
        const a = [1, 2, , 4];
        expect(a.shift()).toBe(1);
        expect(a.length).toBe(3);
        expect(0 in a).toBeTrue();
        expect(1 in a).toBeFalse();
        expect(2 in a).toBeTrue();
        expect(a[2]).toBe(4);
    });

    test("shift then pop after many shifts", () => {
        const a = [1, 2, 3, 4, 5, 6, 7, 8];
        a.shift();
        a.shift();
        a.shift();
        expect(a.pop()).toBe(8);
        expect(a).toEqual([4, 5, 6, 7]);
    });

    test("shift then unshift", () => {
        const a = [1, 2, 3, 4];
        a.shift();
        a.unshift(99);
        expect(a).toEqual([99, 2, 3, 4]);
    });

    test("drain via shift then refill", () => {
        const a = [1, 2, 3];
        expect(a.shift()).toBe(1);
        expect(a.shift()).toBe(2);
        expect(a.shift()).toBe(3);
        expect(a.length).toBe(0);
        a.push(10);
        a.push(20);
        expect(a).toEqual([10, 20]);
    });

    test("Array.from on shifted array", () => {
        const a = [1, 2, 3, 4];
        a.shift();
        a.shift();
        expect(Array.from(a)).toEqual([3, 4]);
    });
});

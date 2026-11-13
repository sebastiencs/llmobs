import { describe, expect, it } from "vitest";

import {
  attributeValue,
  type Format,
  MAX_DEPTH,
  MAX_PAYLOAD_BYTES,
  serialize,
  toJsonable,
  truncate,
} from "./serialize.js";
import { utf8Encode } from "./web.js";

class TokenError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "TokenError";
  }
}

class Point {
  x: number;
  y: number;
  constructor(x: number, y: number) {
    this.x = x;
    this.y = y;
  }
  norm(): number {
    return Math.hypot(this.x, this.y);
  }
}

class Wrapped {
  value: unknown;
  constructor(value: unknown) {
    this.value = value;
  }
  toJSON(): unknown {
    return { v: this.value };
  }
}

class Boom {
  constructor() {
    // A getter on the class prototype is never read (own enumerable keys only);
    // an own enumerable getter is.
    Object.defineProperty(this, "broken", {
      enumerable: true,
      get(): string {
        throw new Error("nope");
      },
    });
  }
}

const table: ReadonlyArray<[name: string, value: unknown, text: string, format: Format]> = [
  ["a string at the top level stays text", "hello", "hello", "text"],
  ["top-level undefined", undefined, "null", "json"],
  ["null", null, "null", "json"],
  ["true", true, "true", "json"],
  ["an integral number", 42, "42", "json"],
  ["a finite float", 1.5, "1.5", "json"],
  ["NaN becomes a string", Number.NaN, '"NaN"', "json"],
  ["Infinity becomes a string", Number.POSITIVE_INFINITY, '"Infinity"', "json"],
  ["-Infinity becomes a string", Number.NEGATIVE_INFINITY, '"-Infinity"', "json"],
  ["bigint becomes a decimal string", 10n, '"10"', "json"],
  ["a Date becomes its ISO string", new Date(0), '"1970-01-01T00:00:00.000Z"', "json"],
  ["an invalid Date becomes null", new Date("not a date"), "null", "json"],
  ["an Error subclass", new TokenError("boom"), '{"type":"TokenError","message":"boom"}', "json"],
  [
    "a Uint8Array becomes a data URI",
    new Uint8Array([0x4d, 0x61, 0x6e]),
    '"data:application/octet-stream;base64,TWFu"',
    "json",
  ],
  [
    "an ArrayBuffer with one padding byte",
    new Uint8Array([0x4d, 0x61]).buffer,
    '"data:application/octet-stream;base64,TWE="',
    "json",
  ],
  [
    "an ArrayBuffer with two padding bytes",
    new Uint8Array([0x4d]).buffer,
    '"data:application/octet-stream;base64,TQ=="',
    "json",
  ],
  ["an Int16Array becomes its numbers", new Int16Array([1, -2]), "[1,-2]", "json"],
  ["a Float64Array becomes its numbers", new Float64Array([1.5]), "[1.5]", "json"],
  ["a BigInt64Array becomes decimal strings", new BigInt64Array([1n, -2n]), '["1","-2"]', "json"],
  [
    "a Map with non-string keys",
    new Map<unknown, string>([
      [1, "a"],
      [true, "b"],
      ["k", "c"],
    ]),
    '{"1":"a","true":"b","k":"c"}',
    "json",
  ],
  ["a Set keeps iteration order", new Set(["b", "a", "b"]), '["b","a"]', "json"],
  [
    "array items without a JSON form become null",
    [undefined, () => 1, Symbol("s"), 2],
    "[null,null,null,2]",
    "json",
  ],
  [
    "object properties without a JSON form are omitted",
    { a: 1, b: undefined, fn: () => 1, sym: Symbol("s") },
    '{"a":1}',
    "json",
  ],
  ["compact JSON keeps non-ASCII", { a: [1, 2], b: "é" }, '{"a":[1,2],"b":"é"}', "json"],
  ["keys keep insertion order", { b: 1, a: 2 }, '{"b":1,"a":2}', "json"],
  ["a class instance keeps its own fields only", new Point(1, 2), '{"x":1,"y":2}', "json"],
  [
    "a toJSON object is converted through toJSON",
    { toJSON: () => ({ ok: true }) },
    '{"ok":true}',
    "json",
  ],
  ["a toJSON returning a scalar", { toJSON: () => "x" }, '"x"', "json"],
  ["a function at the top level becomes String(value)", () => 42, '"() => 42"', "json"],
  ["a symbol at the top level becomes String(value)", Symbol("tag"), '"Symbol(tag)"', "json"],
  ["undefined inside a nested array", { list: [undefined, 1] }, '{"list":[null,1]}', "json"],
];

describe("serialize", () => {
  for (const [name, value, text, format] of table) {
    it(name, () => {
      expect(serialize(value)).toEqual([text, format]);
    });
  }

  it("binds toJSON to the object", () => {
    expect(serialize(new Wrapped("x"))).toEqual(['{"v":"x"}', "json"]);
  });

  it("detects a cycle through toJSON", () => {
    expect(
      serialize({
        toJSON() {
          return this;
        },
      }),
    ).toEqual(['"[llmobs: cycle]"', "json"]);
  });

  it("replaces a self-referencing object with the cycle marker", () => {
    const value: Record<string, unknown> = {};
    value.self = value;
    expect(serialize(value)).toEqual(['{"self":"[llmobs: cycle]"}', "json"]);
  });

  it("replaces a self-referencing array with the cycle marker", () => {
    const value: unknown[] = [1];
    value.push(value);
    expect(serialize(value)).toEqual(['[1,"[llmobs: cycle]"]', "json"]);
  });

  it("replaces a self-referencing Map with the cycle marker", () => {
    const value = new Map<string, unknown>();
    value.set("m", value);
    expect(serialize(value)).toEqual(['{"m":"[llmobs: cycle]"}', "json"]);
  });

  it("serializes a shared reference each time", () => {
    const shared = { v: 1 };
    expect(serialize({ a: shared, b: shared })).toEqual(['{"a":{"v":1},"b":{"v":1}}', "json"]);
  });

  it("serializes 32 nested containers without a marker", () => {
    const [text, format] = serialize(nested(MAX_DEPTH));
    expect(format).toBe("json");
    expect(text).not.toContain("[llmobs: max depth]");
    expect(dig(JSON.parse(text), MAX_DEPTH)).toBe("leaf");
  });

  it("replaces the 33rd container with the depth marker", () => {
    const [text] = serialize(nested(MAX_DEPTH + 1));
    expect(dig(JSON.parse(text), MAX_DEPTH)).toBe("[llmobs: max depth]");
  });

  it("respects the offset of a byte view", () => {
    const bytes = utf8Encode("Manchester");
    const slice = bytes.subarray(2, 5); // "nch"
    expect(serialize(slice)).toEqual(['"data:application/octet-stream;base64,bmNo"', "json"]);
  });

  it("respects the offset of a DataView", () => {
    const bytes = utf8Encode("Manchester");
    const view = new DataView(bytes.buffer, 2, 3); // "nch"
    expect(serialize(view)).toEqual(['"data:application/octet-stream;base64,bmNo"', "json"]);
  });

  it("stringifies an object key of a Map", () => {
    expect(serialize(new Map([[{}, "c"]]))).toEqual(['{"[object Object]":"c"}', "json"]);
  });

  it("omits Map values without a JSON form", () => {
    const value = new Map<string, unknown>([
      ["gone", undefined],
      ["fn", () => 1],
      ["kept", 1],
    ]);
    expect(serialize(value)).toEqual(['{"kept":1}', "json"]);
  });

  it("never throws on a throwing getter", () => {
    // A top-level value that fails to convert is the rule 7 tuple; the same
    // value nested is the in-place marker, keeping the rest of the payload.
    expect(serialize(new Boom())).toEqual(["<unserializable Boom>", "text"]);
    expect(serialize({ deep: new Boom() })).toEqual(['{"deep":"<unserializable Boom>"}', "json"]);
    expect(serialize([new Boom(), 1])).toEqual(['["<unserializable Boom>",1]', "json"]);
  });

  it("never throws on a throwing toJSON", () => {
    expect(
      serialize({
        toJSON: () => {
          throw new Error("nope");
        },
      }),
    ).toEqual(["<unserializable Object>", "text"]);
    expect(
      serialize({
        nested: {
          toJSON: () => {
            throw new Error("nope");
          },
        },
      }),
    ).toEqual(['{"nested":"<unserializable Object>"}', "json"]);
  });
});

describe("truncate", () => {
  it("keeps text at or under the byte limit", () => {
    expect(truncate("hello", "json")).toEqual(["hello", "json"]);
    expect(truncate("a".repeat(MAX_PAYLOAD_BYTES), "text")).toEqual([
      "a".repeat(MAX_PAYLOAD_BYTES),
      "text",
    ]);
    expect(truncate("é".repeat(MAX_PAYLOAD_BYTES / 2), "json")).toEqual([
      "é".repeat(MAX_PAYLOAD_BYTES / 2),
      "json",
    ]);
  });

  it("cuts an over-limit ASCII string and appends the marker", () => {
    const value = "a".repeat(MAX_PAYLOAD_BYTES + 1);
    const original = value.length;
    expect(truncate(value, "json")).toEqual([
      `${"a".repeat(MAX_PAYLOAD_BYTES)}…[llmobs: truncated, original ${original} bytes]`,
      "text",
    ]);
  });

  it("steps back over a 2-byte character at the cut", () => {
    const value = `x${"é".repeat(600_000)}`; // 1 + 1_200_000 bytes
    const [text, format] = truncate(value, "json");
    expect(format).toBe("text");
    expect(text).toBe(`x${"é".repeat(524_287)}…[llmobs: truncated, original 1200001 bytes]`);
  });

  it("steps back over a 4-byte character at the cut", () => {
    const value = `x${"🎉".repeat(300_000)}`; // 1 + 1_200_000 bytes
    const [text, format] = truncate(value, "text");
    expect(format).toBe("text");
    expect(text).toBe(`x${"🎉".repeat(262_143)}…[llmobs: truncated, original 1200001 bytes]`);
  });

  it("turns truncated JSON into text", () => {
    const padding = 1000;
    const value = { v: "a".repeat(MAX_PAYLOAD_BYTES + padding) };
    // {"v":"…"} is 8 bytes of JSON around the string.
    const original = 8 + MAX_PAYLOAD_BYTES + padding;
    const [text, format] = serialize(value);
    expect(format).toBe("text");
    expect(text.startsWith('{"v":"aaaa')).toBe(true);
    expect(text.endsWith(`…[llmobs: truncated, original ${original} bytes]`)).toBe(true);
  });
});

describe("toJsonable", () => {
  it("converts containers to records and arrays", () => {
    expect(toJsonable(new Map([["k", 1n]]))).toEqual({ k: "1" });
    expect(toJsonable(new Set([1, 2]))).toEqual([1, 2]);
    expect(toJsonable(1)).toBe(1);
  });
});

describe("attributeValue", () => {
  it("passes scalars through", () => {
    expect(attributeValue("hello")).toBe("hello");
    expect(attributeValue(true)).toBe(true);
    expect(attributeValue(42)).toBe(42);
    expect(attributeValue(-1.5)).toBe(-1.5);
  });

  it("serializes values an attribute cannot carry", () => {
    expect(attributeValue(2 ** 53)).toBe("9007199254740992");
    expect(attributeValue(Number.NaN)).toBe('"NaN"');
    expect(attributeValue(10n)).toBe('"10"');
    expect(attributeValue(null)).toBe("null");
    expect(attributeValue({ a: 1 })).toBe('{"a":1}');
  });

  it("truncates long strings", () => {
    const value = "a".repeat(MAX_PAYLOAD_BYTES + 1);
    expect(attributeValue(value)).toBe(
      `${"a".repeat(MAX_PAYLOAD_BYTES)}…[llmobs: truncated, original ${value.length} bytes]`,
    );
  });
});

/** `containers` nested objects around the string "leaf". */
function nested(containers: number): unknown {
  let value: unknown = "leaf";
  for (let i = 0; i < containers; i++) {
    value = { a: value };
  }
  return value;
}

/** Follow `a` `hops` times from `node`. */
function dig(node: unknown, hops: number): unknown {
  let current = node;
  for (let i = 0; i < hops; i++) {
    current = (current as { a: unknown }).a;
  }
  return current;
}

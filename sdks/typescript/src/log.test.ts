import { beforeEach, describe, expect, it, vi } from "vitest";

import { debug, resetWarningsForTests, warnOnce } from "./log.js";

describe("warnOnce", () => {
  beforeEach(() => {
    resetWarningsForTests();
  });

  it("warns once per key", () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    try {
      warnOnce("missing-api-key", "first message");
      warnOnce("missing-api-key", "second message");
      expect(warn).toHaveBeenCalledTimes(1);
      expect(warn).toHaveBeenCalledWith("first message");
    } finally {
      warn.mockRestore();
    }
  });

  it("warns separately per key", () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    try {
      warnOnce("a", "one");
      warnOnce("b", "two");
      expect(warn).toHaveBeenCalledTimes(2);
      expect(warn).toHaveBeenNthCalledWith(1, "one");
      expect(warn).toHaveBeenNthCalledWith(2, "two");
    } finally {
      warn.mockRestore();
    }
  });

  it("warns again after resetWarningsForTests", () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    try {
      warnOnce("key", "message");
      resetWarningsForTests();
      warnOnce("key", "message");
      expect(warn).toHaveBeenCalledTimes(2);
    } finally {
      warn.mockRestore();
    }
  });

  it("debug does not throw", () => {
    expect(() => debug("something happened at debug level")).not.toThrow();
  });
});

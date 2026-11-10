import { inspect } from "node:util";
import { describe, expect, it } from "vitest";

import { type Config, ConfigError, type ConfigOptions, resolveConfig } from "./config.js";

const KEY = "sk-llmobs-test-key";
const STORED_KEY = "sk-llmobs-super-secret-key-42";

function resolve(options: Record<string, unknown> = {}): Config {
  return resolveConfig(options as ConfigOptions);
}

function message(options: Record<string, unknown>): string {
  try {
    resolve(options);
  } catch (error) {
    expect(error).toBeInstanceOf(ConfigError);
    return error instanceof Error ? error.message : String(error);
  }
  throw new Error("expected a ConfigError");
}

describe("defaults", () => {
  it("fills the spec defaults when only the key is set", () => {
    const config = resolve({ apiKey: KEY, env: {} });
    expect(config).toMatchObject({
      enabled: true,
      baseUrl: "http://localhost:8080",
      environment: undefined,
      release: undefined,
      captureContent: true,
      sampleRate: 1,
      instrument: [],
      mask: undefined,
    });
    expect(config.apiKey).toBe(KEY);
  });

  it("disables without reading the other options", () => {
    const config = resolve({
      enabled: false,
      apiKey: 1,
      sampleRate: 4,
      env: { LLMOBS_SAMPLE_RATE: "nope", GIT_COMMIT: "abc123", SOURCE_VERSION: "src" },
    });
    expect(config.enabled).toBe(false);
    expect(config.apiKey).toBeUndefined();
    expect(config.baseUrl).toBe("http://localhost:8080");
    expect(config.release).toBeUndefined();
    expect(config.sampleRate).toBe(1);
  });
});

describe("precedence", () => {
  it.each([
    ["apiKey", "LLMOBS_API_KEY", KEY, "sk-llmobs-from-env"],
    ["baseUrl", "LLMOBS_BASE_URL", "https://arg.example", "https://env.example"],
    ["environment", "LLMOBS_ENVIRONMENT", "prod", "staging"],
    ["release", "LLMOBS_RELEASE", "1.2.3", "9.9.9"],
    ["captureContent", "LLMOBS_CAPTURE_CONTENT", false, "true"],
    ["sampleRate", "LLMOBS_SAMPLE_RATE", 0.25, "0.5"],
    ["enabled", "LLMOBS_ENABLED", false, "true"],
  ])("%s prefers the argument over the env var", (option, envName, argument, envValue) => {
    const env: Record<string, string> = { [envName]: String(envValue) };
    if (option !== "apiKey") {
      env.LLMOBS_API_KEY = KEY;
    }
    const config = resolve({ apiKey: KEY, [option]: argument, env });
    const value = option === "apiKey" ? config.apiKey : config[option as keyof Config];
    expect(value).toBe(argument);
  });

  it("treats a blank env value as unset", () => {
    for (const blank of ["", "   ", "\n\t"]) {
      const config = resolve({
        apiKey: KEY,
        env: {
          LLMOBS_BASE_URL: blank,
          LLMOBS_ENVIRONMENT: blank,
          LLMOBS_RELEASE: blank,
          LLMOBS_SAMPLE_RATE: blank,
        },
      });
      expect(config.baseUrl).toBe("http://localhost:8080");
      expect(config.environment).toBeUndefined();
      expect(config.release).toBeUndefined();
      expect(config.sampleRate).toBe(1);
    }
  });

  it("does not trim an explicit argument", () => {
    expect(message({ apiKey: `  ${KEY}`, env: {} })).toBe("apiKey: must start with sk-llmobs-");
  });
});

describe("enabled", () => {
  it("lets an explicit false win over LLMOBS_ENABLED=true", () => {
    const config = resolve({ enabled: false, env: { LLMOBS_ENABLED: "true" } });
    expect(config.enabled).toBe(false);
  });

  it("stops at an invalid enabled before reading the key", () => {
    expect(message({ enabled: "maybe", apiKey: 1, env: {} })).toBe("enabled: must be a boolean");
  });

  it.each(["true", "false", "1", "0", "yes", "no", "on", "off", "TRUE", "Off"])(
    "accepts the env spelling %s",
    (spelling) => {
      const config = resolve({ apiKey: KEY, env: { LLMOBS_CAPTURE_CONTENT: spelling } });
      expect(config.captureContent).toBe(
        ["true", "1", "yes", "on"].includes(spelling.toLowerCase()),
      );
    },
  );

  it("names the env var when the spelling is unknown", () => {
    expect(message({ apiKey: KEY, env: { LLMOBS_CAPTURE_CONTENT: "maybe" } })).toBe(
      "LLMOBS_CAPTURE_CONTENT: must be a boolean",
    );
  });
});

describe("secrecy", () => {
  it("keeps the key off enumerable surfaces", () => {
    const config = resolve({ apiKey: STORED_KEY, env: {} });
    expect(JSON.stringify(config)).not.toContain(STORED_KEY);
    expect(inspect(config)).not.toContain(STORED_KEY);
    expect(JSON.stringify({ ...config })).not.toContain(STORED_KEY);
    expect(Object.keys(config)).not.toContain("apiKey");
    expect(config.apiKey).toBe(STORED_KEY);
  });

  it("never echoes a bad key or URL", () => {
    const badKey = "sk-other-super-secret";
    const badUrl = "ftp://user:secret-token@evil.example/path?q=1#frag";
    expect(message({ apiKey: badKey, env: {} })).toBe("apiKey: must start with sk-llmobs-");
    expect(message({ apiKey: badKey, env: {} })).not.toContain(badKey);
    expect(message({ env: { LLMOBS_API_KEY: badKey } })).toBe(
      "LLMOBS_API_KEY: must start with sk-llmobs-",
    );
    expect(message({ env: { LLMOBS_API_KEY: badKey } })).not.toContain(badKey);
    expect(message({ apiKey: KEY, baseUrl: badUrl, env: {} })).not.toContain("evil.example");
    expect(message({ apiKey: KEY, baseUrl: badUrl, env: {} })).not.toContain("secret-token");
  });
});

describe("baseUrl", () => {
  it("strips trailing slashes", () => {
    const config = resolve({ apiKey: KEY, baseUrl: "https://host/prefix///", env: {} });
    expect(config.baseUrl).toBe("https://host/prefix");
  });

  it.each(["ftp://example.com", "localhost:8080", "http://host/?q=1", "http://host/#f"])(
    "rejects %s",
    (baseUrl) => {
      const text = message({ apiKey: KEY, baseUrl, env: {} });
      expect(text.startsWith("baseUrl:")).toBe(true);
      expect(text).not.toContain(baseUrl);
    },
  );
});

describe("sampleRate", () => {
  it.each([0, 1, 0.25])("accepts %s", (sampleRate) => {
    expect(resolve({ apiKey: KEY, sampleRate, env: {} }).sampleRate).toBe(sampleRate);
  });

  it.each([-0.1, 1.5, Number.NaN, Number.POSITIVE_INFINITY, true, "0.5"])(
    "rejects %s",
    (sampleRate) => {
      expect(message({ apiKey: KEY, sampleRate, env: {} })).toBe(
        "sampleRate: must be a number between 0 and 1",
      );
    },
  );

  it("rejects a non-numeric env value", () => {
    expect(message({ apiKey: KEY, env: { LLMOBS_SAMPLE_RATE: "abc" } })).toBe(
      "LLMOBS_SAMPLE_RATE: must be a number between 0 and 1",
    );
  });
});

describe("environment and release", () => {
  it("rejects the reserved environment without echoing it", () => {
    const text = message({ apiKey: KEY, environment: "llmobs-eval", env: {} });
    expect(text).toBe("environment: is reserved");
    expect(message({ apiKey: KEY, env: { LLMOBS_ENVIRONMENT: "llmobs-eval" } })).toBe(
      "LLMOBS_ENVIRONMENT: is reserved",
    );
  });

  it("uses the release argument, then LLMOBS_RELEASE, and ignores git env vars", () => {
    expect(
      resolve({
        apiKey: KEY,
        release: "argument",
        env: { LLMOBS_RELEASE: "named", GIT_COMMIT: "abc", SOURCE_VERSION: "source" },
      }).release,
    ).toBe("argument");
    expect(
      resolve({ apiKey: KEY, env: { LLMOBS_RELEASE: "named", GIT_COMMIT: "abc" } }).release,
    ).toBe("named");
    expect(
      resolve({ apiKey: KEY, env: { GIT_COMMIT: "abc", SOURCE_VERSION: "source" } }).release,
    ).toBe(undefined);
  });
});

describe("instrument and mask", () => {
  it("copies instrument into a frozen list", () => {
    const names = ["openai"];
    const config = resolve({ apiKey: KEY, instrument: names, env: {} });
    names.push("extra");
    expect(config.instrument).toEqual(["openai"]);
    expect(Object.isFrozen(config.instrument)).toBe(true);
  });

  it.each(["openai", ["", "x"], [1]])("rejects %j", (instrument) => {
    expect(message({ apiKey: KEY, instrument, env: {} })).toBe(
      "instrument: must be a list of non-empty strings",
    );
  });

  it("accepts a function mask and rejects anything else", () => {
    const mask = (value: unknown) => value;
    expect(resolve({ apiKey: KEY, mask, env: {} }).mask).toBe(mask);
    expect(message({ apiKey: KEY, mask: 1, env: {} })).toBe("mask: must be callable");
  });
});

describe("process.env", () => {
  it("reads the default env from globalThis.process", () => {
    const previous = process.env.LLMOBS_BASE_URL;
    process.env.LLMOBS_API_KEY = KEY;
    process.env.LLMOBS_BASE_URL = "https://from-process.example";
    try {
      expect(resolveConfig().baseUrl).toBe("https://from-process.example");
    } finally {
      delete process.env.LLMOBS_API_KEY;
      if (previous === undefined) {
        delete process.env.LLMOBS_BASE_URL;
      } else {
        process.env.LLMOBS_BASE_URL = previous;
      }
    }
  });

  it("does not throw when process itself is missing", () => {
    const previous = globalThis.process;
    Object.defineProperty(globalThis, "process", { value: undefined, configurable: true });
    try {
      expect(resolve({ apiKey: KEY }).baseUrl).toBe("http://localhost:8080");
      expect(message({})).toBe("apiKey: required");
    } finally {
      Object.defineProperty(globalThis, "process", { value: previous, configurable: true });
    }
  });
});

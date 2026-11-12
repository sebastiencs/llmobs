import { inspect } from "node:util";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { type Config, ConfigError, type LlmobsOptions, resolveConfig } from "./config.js";
import { resetWarningsForTests } from "./log.js";

const KEY = "sk-llmobs-test-key";
const STORED_KEY = "sk-llmobs-super-secret-key-42";

type Env = Record<string, string | undefined>;

function resolve(options: Record<string, unknown> = {}, env: Env = {}): Config {
  return resolveConfig(options as LlmobsOptions, env);
}

function message(options: Record<string, unknown> = {}, env: Env = {}): string {
  try {
    resolve(options, env);
  } catch (error) {
    expect(error).toBeInstanceOf(ConfigError);
    return error instanceof Error ? error.message : String(error);
  }
  throw new Error("expected a ConfigError");
}

function suppressWarn() {
  return vi.spyOn(console, "warn").mockImplementation(() => {});
}

beforeEach(() => {
  resetWarningsForTests();
});

describe("defaults", () => {
  it("fills the spec defaults when only the key is set", () => {
    const config = resolve({ apiKey: KEY });
    expect(config).toMatchObject({
      enabled: true,
      baseUrl: "http://localhost:8080",
      serviceName: undefined,
      environment: undefined,
      release: undefined,
      captureContent: true,
      sampleRate: 1,
      mask: undefined,
    });
    expect(config.apiKey).toBe(KEY);
  });

  it("disables without reading the other options", () => {
    const config = resolve(
      { enabled: false, apiKey: 1, sampleRate: 4 },
      { LLMOBS_SAMPLE_RATE: "nope", GIT_COMMIT: "abc123", SOURCE_VERSION: "src" },
    );
    expect(config.enabled).toBe(false);
    expect(config.apiKey).toBeUndefined();
    expect(config.baseUrl).toBe("http://localhost:8080");
    expect(config.release).toBeUndefined();
    expect(config.sampleRate).toBe(1);
  });

  it("warns once and runs disabled when the key is missing (D-F-3)", () => {
    const warn = suppressWarn();
    try {
      const config = resolve();
      expect(config.enabled).toBe(false);
      expect(config.apiKey).toBeUndefined();
      expect(config.baseUrl).toBe("http://localhost:8080");
      expect(config.serviceName).toBeUndefined();
      expect(warn).toHaveBeenCalledTimes(1);
      expect(warn).toHaveBeenCalledWith(
        "llmobs: no API key; set LLMOBS_API_KEY, or LLMOBS_ENABLED=false to silence this; tracing is disabled",
      );
      resolve();
      expect(warn).toHaveBeenCalledTimes(1);
    } finally {
      warn.mockRestore();
    }
  });

  it("validates nothing else when the key is missing", () => {
    const warn = suppressWarn();
    try {
      expect(resolve({ sampleRate: 4 }).enabled).toBe(false);
      expect(resolve({}, { LLMOBS_BASE_URL: "ftp://bad" }).enabled).toBe(false);
      // Two resolutions, one warning: warnOnce deduplicates by key.
      expect(warn).toHaveBeenCalledTimes(1);
    } finally {
      warn.mockRestore();
    }
  });

  it("still raises on a key with the wrong prefix", () => {
    expect(message({ apiKey: "pk-llmobs-nope" })).toBe("apiKey: must start with sk-llmobs-");
    expect(message({}, { LLMOBS_API_KEY: "pk-llmobs-nope" })).toBe(
      "LLMOBS_API_KEY: must start with sk-llmobs-",
    );
  });

  it("disables from the env when no key is set", () => {
    const config = resolve({}, { LLMOBS_ENABLED: "false" });
    expect(config.enabled).toBe(false);
    expect(config.apiKey).toBeUndefined();
    expect(config.baseUrl).toBe("http://localhost:8080");
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
    const config = resolve({ apiKey: KEY, [option]: argument }, env);
    const value = option === "apiKey" ? config.apiKey : config[option as keyof Config];
    expect(value).toBe(argument);
  });

  it.each([
    ["baseUrl", "LLMOBS_BASE_URL", "http://env.example", "http://localhost:8080"],
    ["environment", "LLMOBS_ENVIRONMENT", "staging", undefined],
    ["release", "LLMOBS_RELEASE", "env", undefined],
    ["captureContent", "LLMOBS_CAPTURE_CONTENT", false, true],
    ["sampleRate", "LLMOBS_SAMPLE_RATE", 0.5, 1],
    ["enabled", "LLMOBS_ENABLED", false, true],
  ])("%s prefers the env var over the default", (option, envName, expected, fallback) => {
    const config = resolve({ apiKey: KEY }, { [envName]: String(expected) });
    expect(config[option as keyof Config]).toBe(expected);
    expect(config[option as keyof Config]).not.toBe(fallback);
  });

  it("treats a blank env value as unset", () => {
    for (const blank of ["", "   ", "\n\t"]) {
      const config = resolve(
        { apiKey: KEY },
        {
          LLMOBS_BASE_URL: blank,
          LLMOBS_ENVIRONMENT: blank,
          LLMOBS_RELEASE: blank,
          LLMOBS_SAMPLE_RATE: blank,
        },
      );
      expect(config.baseUrl).toBe("http://localhost:8080");
      expect(config.environment).toBeUndefined();
      expect(config.release).toBeUndefined();
      expect(config.sampleRate).toBe(1);
    }
  });

  it("does not trim an explicit argument", () => {
    expect(message({ apiKey: `  ${KEY}` })).toBe("apiKey: must start with sk-llmobs-");
  });
});

describe("enabled", () => {
  it("lets an explicit false win over LLMOBS_ENABLED=true", () => {
    const config = resolve({ enabled: false }, { LLMOBS_ENABLED: "true" });
    expect(config.enabled).toBe(false);
  });

  it("stops at an invalid enabled before reading the key", () => {
    expect(message({ enabled: "maybe", apiKey: 1 })).toBe("enabled: must be a boolean");
  });

  it.each(["true", "false", "1", "0", "yes", "no", "on", "off", "TRUE", "Off"])(
    "accepts the env spelling %s",
    (spelling) => {
      const config = resolve({ apiKey: KEY }, { LLMOBS_CAPTURE_CONTENT: spelling });
      expect(config.captureContent).toBe(
        ["true", "1", "yes", "on"].includes(spelling.toLowerCase()),
      );
    },
  );

  it("names the env var when the spelling is unknown", () => {
    expect(message({ apiKey: KEY }, { LLMOBS_CAPTURE_CONTENT: "maybe" })).toBe(
      "LLMOBS_CAPTURE_CONTENT: must be a boolean",
    );
  });
});

describe("secrecy", () => {
  it("keeps the key off enumerable surfaces", () => {
    const config = resolve({ apiKey: STORED_KEY });
    expect(JSON.stringify(config)).not.toContain(STORED_KEY);
    expect(inspect(config)).not.toContain(STORED_KEY);
    expect(JSON.stringify({ ...config })).not.toContain(STORED_KEY);
    expect(Object.keys(config)).not.toContain("apiKey");
    expect(config.apiKey).toBe(STORED_KEY);
  });

  it("never echoes a bad key or URL", () => {
    const badKey = "sk-other-super-secret";
    const badUrl = "ftp://user:secret-token@evil.example/path?q=1#frag";
    expect(message({ apiKey: badKey })).toBe("apiKey: must start with sk-llmobs-");
    expect(message({ apiKey: badKey })).not.toContain(badKey);
    expect(message({}, { LLMOBS_API_KEY: badKey })).toBe(
      "LLMOBS_API_KEY: must start with sk-llmobs-",
    );
    expect(message({}, { LLMOBS_API_KEY: badKey })).not.toContain(badKey);
    expect(message({ apiKey: KEY, baseUrl: badUrl })).not.toContain("evil.example");
    expect(message({ apiKey: KEY, baseUrl: badUrl })).not.toContain("secret-token");
  });
});

describe("baseUrl", () => {
  it("strips trailing slashes", () => {
    const config = resolve({ apiKey: KEY, baseUrl: "https://host/prefix///" });
    expect(config.baseUrl).toBe("https://host/prefix");
  });

  it.each([
    ["http://[::1]:8080", "http://[::1]:8080"],
    ["HTTPS://X.example/", "HTTPS://X.example"],
  ])("accepts %s", (baseUrl, expected) => {
    expect(resolve({ apiKey: KEY, baseUrl }).baseUrl).toBe(expected);
  });

  it.each(["http://[::1", "http://::1]", "http://[zz]", "http://a]b", "http://", "http://:8080"])(
    "rejects the host %s",
    (baseUrl) => {
      expect(message({ apiKey: KEY, baseUrl })).toBe(
        "baseUrl: must be an absolute http or https URL",
      );
    },
  );

  it("allows an empty query or fragment", () => {
    expect(resolve({ apiKey: KEY, baseUrl: "http://host/?" }).baseUrl).toBe("http://host/?");
    expect(resolve({ apiKey: KEY, baseUrl: "http://host/#" }).baseUrl).toBe("http://host/#");
  });

  it.each(["ftp://example.com", "localhost:8080", "http://host/?q=1", "http://host/#f"])(
    "rejects %s",
    (baseUrl) => {
      const text = message({ apiKey: KEY, baseUrl });
      expect(text.startsWith("baseUrl:")).toBe(true);
      expect(text).not.toContain(baseUrl);
    },
  );
});

describe("sampleRate from the env", () => {
  it.each(["1", "0", "0.25", ".5", "1.", "0.0", " 0.5 "])("accepts %j", (raw) => {
    expect(resolve({ apiKey: KEY }, { LLMOBS_SAMPLE_RATE: raw }).sampleRate).toBe(Number(raw));
  });

  it.each(["+0.5", "-0", "5e-1", "0x1", "0b1", "0o1", "0.2_5", "١", "Infinity", "NaN", "1.5"])(
    "rejects %j",
    (raw) => {
      expect(message({ apiKey: KEY }, { LLMOBS_SAMPLE_RATE: raw })).toBe(
        "LLMOBS_SAMPLE_RATE: must be a number between 0 and 1",
      );
    },
  );
});

describe("sampleRate", () => {
  it.each([0, 1, 0.25])("accepts %s", (sampleRate) => {
    expect(resolve({ apiKey: KEY, sampleRate }).sampleRate).toBe(sampleRate);
  });

  it.each([-0.1, 1.5, Number.NaN, Number.POSITIVE_INFINITY, true, "0.5"])(
    "rejects %s",
    (sampleRate) => {
      expect(message({ apiKey: KEY, sampleRate })).toBe(
        "sampleRate: must be a number between 0 and 1",
      );
    },
  );

  it("rejects a non-numeric env value", () => {
    expect(message({ apiKey: KEY }, { LLMOBS_SAMPLE_RATE: "abc" })).toBe(
      "LLMOBS_SAMPLE_RATE: must be a number between 0 and 1",
    );
  });
});

describe("environment and release", () => {
  it("rejects the reserved environment without echoing it", () => {
    const text = message({ apiKey: KEY, environment: "llmobs-eval" });
    expect(text).toBe("environment: is reserved");
    expect(message({ apiKey: KEY }, { LLMOBS_ENVIRONMENT: "llmobs-eval" })).toBe(
      "LLMOBS_ENVIRONMENT: is reserved",
    );
  });

  it("uses the release argument, then LLMOBS_RELEASE, and ignores git env vars", () => {
    expect(
      resolve(
        { apiKey: KEY, release: "argument" },
        { LLMOBS_RELEASE: "named", GIT_COMMIT: "abc", SOURCE_VERSION: "source" },
      ).release,
    ).toBe("argument");
    expect(resolve({ apiKey: KEY }, { LLMOBS_RELEASE: "named", GIT_COMMIT: "abc" }).release).toBe(
      "named",
    );
    expect(resolve({ apiKey: KEY }, { GIT_COMMIT: "abc", SOURCE_VERSION: "source" }).release).toBe(
      undefined,
    );
  });
});

describe("serviceName", () => {
  it("reads OTEL_SERVICE_NAME from the env source", () => {
    expect(resolve({ apiKey: KEY }, { OTEL_SERVICE_NAME: "api" }).serviceName).toBe("api");
  });

  it("trims the value and treats an empty one as unset", () => {
    expect(resolve({ apiKey: KEY }, { OTEL_SERVICE_NAME: "  api\t" }).serviceName).toBe("api");
    for (const blank of ["", "   "]) {
      expect(resolve({ apiKey: KEY }, { OTEL_SERVICE_NAME: blank }).serviceName).toBeUndefined();
    }
  });

  it("is undefined when disabled, even with the env var set", () => {
    expect(resolve({ enabled: false }, { OTEL_SERVICE_NAME: "api" }).serviceName).toBeUndefined();
  });
});

describe("mask", () => {
  it("accepts a function mask and rejects anything else", () => {
    const mask = (value: unknown) => value;
    expect(resolve({ apiKey: KEY, mask }).mask).toBe(mask);
    expect(message({ apiKey: KEY, mask: 1 })).toBe("mask: must be callable");
  });
});

describe("process.env", () => {
  it("reads the default env from globalThis.process", () => {
    const previousKey = process.env.LLMOBS_API_KEY;
    const previousUrl = process.env.LLMOBS_BASE_URL;
    process.env.LLMOBS_API_KEY = KEY;
    process.env.LLMOBS_BASE_URL = "https://from-process.example";
    try {
      expect(resolveConfig().baseUrl).toBe("https://from-process.example");
    } finally {
      if (previousKey === undefined) {
        delete process.env.LLMOBS_API_KEY;
      } else {
        process.env.LLMOBS_API_KEY = previousKey;
      }
      if (previousUrl === undefined) {
        delete process.env.LLMOBS_BASE_URL;
      } else {
        process.env.LLMOBS_BASE_URL = previousUrl;
      }
    }
  });

  it("does not throw when process itself is missing", () => {
    const warn = suppressWarn();
    const previous = globalThis.process;
    Object.defineProperty(globalThis, "process", { value: undefined, configurable: true });
    try {
      expect(resolveConfig({ apiKey: KEY }).baseUrl).toBe("http://localhost:8080");
      // D-F-3: no env to read, so the key is missing; warn, do not raise.
      expect(resolveConfig().enabled).toBe(false);
      expect(warn).toHaveBeenCalledTimes(1);
    } finally {
      Object.defineProperty(globalThis, "process", { value: previous, configurable: true });
      warn.mockRestore();
    }
  });
});

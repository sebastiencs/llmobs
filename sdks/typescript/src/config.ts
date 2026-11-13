/**
 * SDK configuration resolution.
 *
 * The rules are spec 10 § Configuration, items 1-11. `init()` will be the caller.
 * Never log a resolved config; `apiKey` is on the object.
 */

import type { ReadableSpan } from "@opentelemetry/sdk-trace";

import type { Instrumentation } from "./instrument.js";
import { warnOnce } from "./log.js";
import { hasHttpAuthority, runtimeGlobals } from "./web.js";

const ENV_PREFIX = "LLMOBS_";
const ENV_API_KEY = `${ENV_PREFIX}API_KEY`;
const ENV_BASE_URL = `${ENV_PREFIX}BASE_URL`;
const ENV_ENVIRONMENT = `${ENV_PREFIX}ENVIRONMENT`;
const ENV_RELEASE = `${ENV_PREFIX}RELEASE`;
const ENV_CAPTURE_CONTENT = `${ENV_PREFIX}CAPTURE_CONTENT`;
const ENV_SAMPLE_RATE = `${ENV_PREFIX}SAMPLE_RATE`;
const ENV_ENABLED = `${ENV_PREFIX}ENABLED`;
// OTel's own name, read from the same env source (README § Names).
const ENV_SERVICE_NAME = "OTEL_SERVICE_NAME";

const KEY_PREFIX = "sk-llmobs-";
// Reserved evaluation environment. llmobs_core::names::environment::EVAL
const RESERVED_ENVIRONMENT = "llmobs-eval";
const DEFAULT_BASE_URL = "http://localhost:8080";
const TRUE_WORDS = new Set(["true", "1", "yes", "on"]);
const FALSE_WORDS = new Set(["false", "0", "no", "off"]);

type Env = Readonly<Record<string, string | undefined>>;

export class ConfigError extends Error {
  readonly option: string;

  constructor(option: string, problem: string) {
    super(`${option}: ${problem}`);
    this.name = "ConfigError";
    this.option = option;
  }
}

export interface Config {
  readonly enabled: boolean;
  readonly apiKey: string | undefined;
  readonly baseUrl: string;
  readonly serviceName: string | undefined;
  readonly environment: string | undefined;
  readonly release: string | undefined;
  readonly captureContent: boolean;
  readonly sampleRate: number;
  readonly mask: ((value: unknown) => unknown) | undefined;
  // D-F-4: register our provider as the global one (opt-in).
  readonly useGlobalProvider: boolean;
  // D-F-5: replaces the export rule in both provider modes.
  readonly exportFilter: ((span: ReadableSpan) => boolean) | undefined;
  // D-F-7: third-party instrumentations to bind to our provider.
  readonly instrument: readonly Instrumentation[];
}

export interface LlmobsOptions {
  apiKey?: string;
  baseUrl?: string;
  environment?: string;
  release?: string;
  captureContent?: boolean;
  sampleRate?: number;
  mask?: (value: unknown) => unknown;
  useGlobalProvider?: boolean;
  exportFilter?: (span: ReadableSpan) => boolean;
  instrument?: readonly Instrumentation[];
  enabled?: boolean;
}

interface ResolvedFields {
  enabled: boolean;
  apiKey: string | undefined;
  baseUrl: string;
  serviceName: string | undefined;
  environment: string | undefined;
  release: string | undefined;
  captureContent: boolean;
  sampleRate: number;
  mask: ((value: unknown) => unknown) | undefined;
  useGlobalProvider: boolean;
  exportFilter: ((span: ReadableSpan) => boolean) | undefined;
  instrument: readonly Instrumentation[];
}

/** Resolve SDK options. An explicit argument wins, then the env var, then the default. */
export function resolveConfig(options: LlmobsOptions = {}, env?: Env): Config {
  const raw = options as Record<string, unknown>;
  const source = env ?? defaultEnv();
  const enabled = resolveBool("enabled", ENV_ENABLED, raw.enabled, source, true);
  if (!enabled) {
    return finish(disabled());
  }
  const apiKey = resolveApiKey(raw.apiKey, source);
  if (apiKey === undefined) {
    // D-F-3: a missing key warns once and runs disabled; nothing else is validated.
    warnOnce(
      "missing-api-key",
      "llmobs: no API key; set LLMOBS_API_KEY, or LLMOBS_ENABLED=false to silence this; tracing is disabled",
    );
    return finish(disabled());
  }
  return finish({
    enabled: true,
    apiKey,
    baseUrl: resolveBaseUrl(raw.baseUrl, source),
    serviceName: readEnv(source, ENV_SERVICE_NAME),
    environment: resolveEnvironment(raw.environment, source),
    release: resolveRelease(raw.release, source),
    captureContent: resolveBool(
      "captureContent",
      ENV_CAPTURE_CONTENT,
      raw.captureContent,
      source,
      true,
    ),
    sampleRate: resolveSampleRate(raw.sampleRate, source),
    mask: resolveMask(raw.mask),
    useGlobalProvider: resolveUseGlobalProvider(raw.useGlobalProvider),
    exportFilter: resolveExportFilter(raw.exportFilter),
    instrument: resolveInstrument(raw.instrument),
  });
}

function defaultEnv(): Env {
  return runtimeGlobals().process?.env ?? {};
}

function disabled(): ResolvedFields {
  return {
    enabled: false,
    apiKey: undefined,
    baseUrl: DEFAULT_BASE_URL,
    serviceName: undefined,
    environment: undefined,
    release: undefined,
    captureContent: true,
    sampleRate: 1,
    mask: undefined,
    useGlobalProvider: false,
    exportFilter: undefined,
    instrument: [],
  };
}

function finish(fields: ResolvedFields): Config {
  const config = {
    enabled: fields.enabled,
    baseUrl: fields.baseUrl,
    serviceName: fields.serviceName,
    environment: fields.environment,
    release: fields.release,
    captureContent: fields.captureContent,
    sampleRate: fields.sampleRate,
    mask: fields.mask,
    useGlobalProvider: fields.useGlobalProvider,
    exportFilter: fields.exportFilter,
    instrument: fields.instrument,
  };
  Object.defineProperty(config, "apiKey", {
    value: fields.apiKey,
    enumerable: false,
    writable: false,
  });
  return Object.freeze(config) as Config;
}

function readEnv(source: Env, name: string): string | undefined {
  const raw = source[name];
  if (typeof raw !== "string") {
    return undefined;
  }
  const trimmed = raw.trim();
  return trimmed === "" ? undefined : trimmed;
}

function resolveBool(
  argument: string,
  envName: string,
  value: unknown,
  source: Env,
  fallback: boolean,
): boolean {
  if (value !== undefined) {
    if (typeof value !== "boolean") {
      throw new ConfigError(argument, "must be a boolean");
    }
    return value;
  }
  const raw = readEnv(source, envName);
  if (raw === undefined) {
    return fallback;
  }
  const parsed = raw.toLowerCase();
  if (TRUE_WORDS.has(parsed)) {
    return true;
  }
  if (FALSE_WORDS.has(parsed)) {
    return false;
  }
  throw new ConfigError(envName, "must be a boolean");
}

function resolveApiKey(value: unknown, source: Env): string | undefined {
  if (value !== undefined) {
    if (typeof value !== "string" || !value.startsWith(KEY_PREFIX)) {
      throw new ConfigError("apiKey", `must start with ${KEY_PREFIX}`);
    }
    return value;
  }
  const raw = readEnv(source, ENV_API_KEY);
  if (raw === undefined) {
    return undefined;
  }
  if (!raw.startsWith(KEY_PREFIX)) {
    throw new ConfigError(ENV_API_KEY, `must start with ${KEY_PREFIX}`);
  }
  return raw;
}

function resolveBaseUrl(value: unknown, source: Env): string {
  if (value !== undefined) {
    return checkBaseUrl(value, "baseUrl");
  }
  const raw = readEnv(source, ENV_BASE_URL);
  if (raw === undefined) {
    return DEFAULT_BASE_URL;
  }
  return checkBaseUrl(raw, ENV_BASE_URL);
}

const SAMPLE_RATE_ENV = /^(?:[0-9]+(?:\.[0-9]*)?|\.[0-9]+)$/;

function checkBaseUrl(value: unknown, name: string): string {
  if (typeof value !== "string" || !hasHttpAuthority(value)) {
    throw new ConfigError(name, "must be an absolute http or https URL");
  }
  const hash = value.indexOf("#");
  const beforeHash = hash === -1 ? value : value.slice(0, hash);
  const query = beforeHash.indexOf("?");
  const queryValue = query === -1 ? "" : beforeHash.slice(query + 1);
  const fragment = hash === -1 ? "" : value.slice(hash + 1);
  if (queryValue !== "" || fragment !== "") {
    throw new ConfigError(name, "must not include a query or fragment");
  }
  return value.replace(/\/+$/, "");
}

function resolveEnvironment(value: unknown, source: Env): string | undefined {
  if (value !== undefined) {
    return checkEnvironment(value, "environment");
  }
  const raw = readEnv(source, ENV_ENVIRONMENT);
  if (raw === undefined) {
    return undefined;
  }
  return checkEnvironment(raw, ENV_ENVIRONMENT);
}

function checkEnvironment(value: unknown, name: string): string {
  if (typeof value !== "string") {
    throw new ConfigError(name, "must be a string");
  }
  if (value === RESERVED_ENVIRONMENT) {
    throw new ConfigError(name, "is reserved");
  }
  return value;
}

function resolveRelease(value: unknown, source: Env): string | undefined {
  if (value !== undefined) {
    if (typeof value !== "string") {
      throw new ConfigError("release", "must be a string");
    }
    return value;
  }
  return readEnv(source, ENV_RELEASE);
}

function resolveSampleRate(value: unknown, source: Env): number {
  if (value !== undefined) {
    return checkSampleRate(value, "sampleRate");
  }
  const raw = readEnv(source, ENV_SAMPLE_RATE);
  if (raw === undefined) {
    return 1;
  }
  if (!SAMPLE_RATE_ENV.test(raw)) {
    throw new ConfigError(ENV_SAMPLE_RATE, "must be a number between 0 and 1");
  }
  return checkSampleRate(Number(raw), ENV_SAMPLE_RATE);
}

function checkSampleRate(value: unknown, name: string): number {
  if (typeof value !== "number" || !Number.isFinite(value) || value < 0 || value > 1) {
    throw new ConfigError(name, "must be a number between 0 and 1");
  }
  return value;
}

function resolveMask(value: unknown): ((value: unknown) => unknown) | undefined {
  if (value === undefined) {
    return undefined;
  }
  if (typeof value !== "function") {
    throw new ConfigError("mask", "must be callable");
  }
  return value as (value: unknown) => unknown;
}

/** No env var: a provider registration is a code decision, not a setting. */
function resolveUseGlobalProvider(value: unknown): boolean {
  if (value === undefined) {
    return false;
  }
  if (typeof value !== "boolean") {
    throw new ConfigError("useGlobalProvider", "must be a boolean");
  }
  return value;
}

function resolveExportFilter(value: unknown): ((span: ReadableSpan) => boolean) | undefined {
  if (value === undefined) {
    return undefined;
  }
  if (typeof value !== "function") {
    throw new ConfigError("exportFilter", "must be callable");
  }
  return value as (span: ReadableSpan) => boolean;
}

/** No env var: instrumentations are wired in code (D-F-7). */
function resolveInstrument(value: unknown): readonly Instrumentation[] {
  if (value === undefined) {
    return [];
  }
  const valid =
    Array.isArray(value) &&
    value.every(
      (item) =>
        typeof item === "object" &&
        item !== null &&
        isInstrumentation(item as Record<string, unknown>),
    );
  if (!valid) {
    throw new ConfigError("instrument", "must be a list of instrumentation objects");
  }
  return value as readonly Instrumentation[];
}

function isInstrumentation(item: Record<string, unknown>): boolean {
  return (
    typeof item.setTracerProvider === "function" &&
    typeof item.enable === "function" &&
    typeof item.disable === "function"
  );
}

/**
 * SDK configuration resolution.
 *
 * The rules are spec 10 § Configuration, items 1-11. `init()` will be the caller.
 * Never log a resolved config; `apiKey` is on the object.
 */

const ENV_PREFIX = "LLMOBS_";
const ENV_API_KEY = `${ENV_PREFIX}API_KEY`;
const ENV_BASE_URL = `${ENV_PREFIX}BASE_URL`;
const ENV_ENVIRONMENT = `${ENV_PREFIX}ENVIRONMENT`;
const ENV_RELEASE = `${ENV_PREFIX}RELEASE`;
const ENV_CAPTURE_CONTENT = `${ENV_PREFIX}CAPTURE_CONTENT`;
const ENV_SAMPLE_RATE = `${ENV_PREFIX}SAMPLE_RATE`;
const ENV_ENABLED = `${ENV_PREFIX}ENABLED`;

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
  readonly environment: string | undefined;
  readonly release: string | undefined;
  readonly captureContent: boolean;
  readonly sampleRate: number;
  readonly instrument: readonly string[];
  readonly mask: ((value: unknown) => unknown) | undefined;
}

export interface ConfigOptions {
  apiKey?: string;
  baseUrl?: string;
  environment?: string;
  release?: string;
  captureContent?: boolean;
  sampleRate?: number;
  instrument?: readonly string[];
  mask?: (value: unknown) => unknown;
  enabled?: boolean;
  env?: Env;
}

interface ResolvedFields {
  enabled: boolean;
  apiKey: string | undefined;
  baseUrl: string;
  environment: string | undefined;
  release: string | undefined;
  captureContent: boolean;
  sampleRate: number;
  instrument: readonly string[];
  mask: ((value: unknown) => unknown) | undefined;
}

/** Resolve SDK options. An explicit argument wins, then the env var, then the default. */
export function resolveConfig(options: ConfigOptions = {}): Config {
  const raw = options as Record<string, unknown>;
  const source = raw.env === undefined ? defaultEnv() : (raw.env as Env);
  const enabled = resolveBool("enabled", ENV_ENABLED, raw.enabled, source, true);
  if (!enabled) {
    return finish(disabled());
  }
  return finish({
    enabled: true,
    apiKey: resolveApiKey(raw.apiKey, source),
    baseUrl: resolveBaseUrl(raw.baseUrl, source),
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
    instrument: resolveInstrument(raw.instrument),
    mask: resolveMask(raw.mask),
  });
}

function defaultEnv(): Env {
  const proc = (globalThis as { process?: { env?: Env } }).process;
  return proc?.env ?? {};
}

function disabled(): ResolvedFields {
  return {
    enabled: false,
    apiKey: undefined,
    baseUrl: DEFAULT_BASE_URL,
    environment: undefined,
    release: undefined,
    captureContent: true,
    sampleRate: 1,
    instrument: [],
    mask: undefined,
  };
}

function finish(fields: ResolvedFields): Config {
  const config = {
    enabled: fields.enabled,
    baseUrl: fields.baseUrl,
    environment: fields.environment,
    release: fields.release,
    captureContent: fields.captureContent,
    sampleRate: fields.sampleRate,
    instrument: Object.freeze([...fields.instrument]),
    mask: fields.mask,
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

function resolveApiKey(value: unknown, source: Env): string {
  if (value !== undefined) {
    if (typeof value !== "string" || !value.startsWith(KEY_PREFIX)) {
      throw new ConfigError("apiKey", `must start with ${KEY_PREFIX}`);
    }
    return value;
  }
  const raw = readEnv(source, ENV_API_KEY);
  if (raw === undefined) {
    throw new ConfigError("apiKey", "required");
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

function checkBaseUrl(value: unknown, name: string): string {
  if (typeof value !== "string" || !isAbsoluteHttpUrl(value)) {
    throw new ConfigError(name, "must be an absolute http or https URL");
  }
  if (value.includes("?") || value.includes("#")) {
    throw new ConfigError(name, "must not include a query or fragment");
  }
  return value.replace(/\/+$/, "");
}

/** True for `http(s)://host...` with a non-empty host. No DOM `URL` in this build. */
function isAbsoluteHttpUrl(value: string): boolean {
  const match = /^(https?):\/\/([^/?#]+)/i.exec(value);
  return match !== null && match[2] !== undefined && match[2] !== "";
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
  return checkSampleRate(Number(raw), ENV_SAMPLE_RATE);
}

function checkSampleRate(value: unknown, name: string): number {
  if (typeof value !== "number" || !Number.isFinite(value) || value < 0 || value > 1) {
    throw new ConfigError(name, "must be a number between 0 and 1");
  }
  return value;
}

function resolveInstrument(value: unknown): readonly string[] {
  if (value === undefined) {
    return [];
  }
  if (!Array.isArray(value)) {
    throw new ConfigError("instrument", "must be a list of non-empty strings");
  }
  const items: string[] = [];
  for (const item of value) {
    if (typeof item !== "string" || item === "") {
      throw new ConfigError("instrument", "must be a list of non-empty strings");
    }
    items.push(item);
  }
  return items;
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

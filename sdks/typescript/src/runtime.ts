/**
 * The runtime: our own TracerProvider, its processor and the SDK lifecycle.
 *
 * `init()` resolves the config and installs one runtime per process (D-F-15);
 * by default the provider stays private. `useGlobalProvider` (D-F-4) registers
 * it as the global one when no app provider is set; OTel JS providers cannot
 * take processors after construction, so with an app provider we warn once and
 * stay private. The OTel *context* is always the global one, so instrumentor
 * spans nest under ours: every `Runtime.create` tries to register an
 * `AsyncLocalStorageContextManager` and keeps whatever manager was already
 * registered (D-F-4).
 */

import {
  type TracerProvider as ApiTracerProvider,
  context,
  ProxyTracerProvider,
  propagation,
  type Tracer,
  trace,
} from "@opentelemetry/api";
import { AsyncLocalStorageContextManager } from "@opentelemetry/context-async-hooks";
import { defaultResource, resourceFromAttributes } from "@opentelemetry/resources";
import {
  type IdGenerator,
  ParentBasedSampler,
  type SpanExporter,
  type SpanProcessor,
  TraceIdRatioBasedSampler,
  TracerProvider,
} from "@opentelemetry/sdk-trace";

import { type Config, type LlmobsOptions, resolveConfig } from "./config.js";
import { FetchTraceExporter } from "./exporter.js";
import { VERSION } from "./index.js";
import { debug, resetWarningsForTests, warnOnce } from "./log.js";
import { MaskingExporter } from "./mask.js";
import { LlmobsSpanProcessor } from "./processor.js";
import { ScoreQueue } from "./scores.js";
import { type FetchLike, runtimeGlobals } from "./web.js";

export class Runtime {
  readonly config: Config;
  readonly provider: TracerProvider;
  readonly ownsProvider: boolean;
  readonly processor: LlmobsSpanProcessor;
  /** The background score queue (§ Score request). */
  readonly scores: ScoreQueue;
  readonly tracer: Tracer;
  private shutDown = false;

  private constructor(
    config: Config,
    provider: TracerProvider,
    processor: LlmobsSpanProcessor,
    scores: ScoreQueue,
  ) {
    this.config = config;
    this.provider = provider;
    // Always true: OTel JS providers cannot take processors after
    // construction, so even global mode builds a provider of our own (D-F-4).
    this.ownsProvider = true;
    this.processor = processor;
    this.scores = scores;
    this.tracer = provider.getTracer("llmobs", VERSION);
  }

  static create(
    config: Config,
    seams?: {
      exporter?: SpanExporter;
      idGenerator?: IdGenerator;
      fetch?: FetchLike;
      /** Passed to the score queue together with `fetch`. */
      scoreSleep?: (ms: number) => Promise<void>;
      /** Tests inject a clock for the queue's once-a-minute drop warning. */
      scoreNow?: () => number;
    },
  ): Runtime {
    // D-F-4: in global mode we register our provider as the global one when no
    // app provider is set; otherwise one warning and we stay private.
    const registerGlobal =
      config.useGlobalProvider && !appProviderRegistered(trace.getTracerProvider());
    if (config.useGlobalProvider && !registerGlobal) {
      warnOnce(
        "global-provider-exists",
        "llmobs: a global tracer provider is already registered; llmobs keeps its own provider. Add spanProcessor() to your provider to send its spans to llmobs",
      );
    }
    const innerExporter =
      seams?.exporter ??
      new FetchTraceExporter({
        url: `${config.baseUrl}/v1/traces`,
        apiKey: requireApiKey(config),
        fetch: seams?.fetch,
      });
    const scores = new ScoreQueue({
      url: `${config.baseUrl}/api/v1/scores`,
      apiKey: requireApiKey(config),
      fetch: seams?.fetch,
      sleep: seams?.scoreSleep,
      now: seams?.scoreNow,
    });
    // The export-time masking pass exists only with a mask or capture off
    // (D-F-6); otherwise spans reach the exporter untouched.
    const exporter =
      config.mask === undefined && config.captureContent
        ? innerExporter
        : new MaskingExporter(innerExporter, {
            mask: config.mask,
            captureContent: config.captureContent,
          });
    const processor = new LlmobsSpanProcessor(exporter, {
      environment: config.environment,
      release: config.release,
      exportFilter: config.exportFilter,
      // The default rule applies only to a provider we registered globally.
      defaultFilter: registerGlobal,
    });
    const provider = new TracerProvider({
      resource: defaultResource().merge(
        resourceFromAttributes({
          "telemetry.distro.name": "llmobs",
          "telemetry.distro.version": VERSION,
          ...(config.serviceName ? { "service.name": config.serviceName } : {}),
        }),
      ),
      sampler: new ParentBasedSampler({ root: new TraceIdRatioBasedSampler(config.sampleRate) }),
      // Flattened instrumentor messages exceed OTel's default 128 (D-F-10).
      spanLimits: { attributeCountLimit: 10_000 },
      idGenerator: seams?.idGenerator,
      spanProcessors: [processor],
    });
    if (registerGlobal) {
      trace.setGlobalTracerProvider(provider);
    }
    registerContextManager();
    return new Runtime(config, provider, processor, scores);
  }

  /** Resolves when pending spans and scores are exported. */
  async flush(): Promise<void> {
    await Promise.all([this.processor.forceFlush(), this.scores.flush()]);
  }

  /** Idempotent; spans and scores after it are dropped. */
  async shutdown(): Promise<void> {
    if (this.shutDown) {
      return;
    }
    this.shutDown = true;
    try {
      await this.flush();
      await this.processor.shutdown();
      await this.scores.shutdown();
      if (this.ownsProvider) {
        await this.provider.shutdown();
      }
    } catch (error) {
      debug(`llmobs: shutdown failed: ${errorName(error)}`);
    }
  }
}

let activeRuntime: Runtime | undefined;
let initCalled = false;
// resetForTests never clears this: the listener stays for the process lifetime.
let beforeExitRegistered = false;

export function init(options?: LlmobsOptions): void {
  if (initCalled) {
    warnOnce("init-twice", "llmobs: init() was already called; this call is ignored");
    return;
  }
  // Set before anything else, so whatever the first init() did (installed a
  // runtime, ran disabled, missed its key, threw ConfigError), a second call
  // only warns (D-F-15).
  initCalled = true;
  const config = resolveConfig(options);
  if (!config.enabled) {
    return;
  }
  try {
    installRuntime(Runtime.create(config));
  } catch (error) {
    debug(`llmobs: runtime startup failed: ${errorName(error)}`);
    return;
  }
  const process = runtimeGlobals().process;
  if (!beforeExitRegistered && process?.once !== undefined) {
    beforeExitRegistered = true;
    process.once("beforeExit", () => void shutdown());
  }
}

export function flush(): Promise<void> {
  const runtime = activeRuntime;
  if (runtime === undefined) {
    return Promise.resolve();
  }
  return runtime.flush();
}

export function shutdown(): Promise<void> {
  const current = activeRuntime;
  if (current === undefined) {
    return Promise.resolve();
  }
  return current.shutdown().then(() => {
    if (activeRuntime === current) {
      activeRuntime = undefined;
    }
  });
}

/** The provider in use; a no-op proxy when not running (spec 10 rule 1). */
export function tracerProvider(): ApiTracerProvider {
  const runtime = activeRuntime;
  if (runtime === undefined) {
    // A proxy without a delegate hands out no-op tracers.
    return new ProxyTracerProvider();
  }
  return runtime.provider;
}

/** Our span processor, for apps that build their own OTel pipeline. */
export function spanProcessor(): SpanProcessor | undefined {
  return activeRuntime?.processor;
}

/** The running runtime, when one is installed. Internal. */
export function currentRuntime(): Runtime | undefined {
  return activeRuntime;
}

/** Install a runtime. Internal, and the test seam. */
export function installRuntime(runtime: Runtime): void {
  activeRuntime = runtime;
}

/** Shut down and forget everything but the beforeExit listener; tests only. */
export async function resetForTests(): Promise<void> {
  await shutdown();
  context.disable();
  trace.disable();
  propagation.disable();
  resetWarningsForTests();
  initCalled = false;
  // No instrument list to clear yet (T11 adds it).
}

/**
 * D-F-4: does the global provider hold an app provider? The API returns its
 * singleton `ProxyTracerProvider` both before and after a registration, so the
 * test is by identity on the delegate — the shared no-op provider means
 * nothing is registered. Never compare constructor names: they break under
 * minification.
 */
function appProviderRegistered(provider: ApiTracerProvider): boolean {
  if (!(provider instanceof ProxyTracerProvider)) {
    return true;
  }
  return provider.getDelegate() !== new ProxyTracerProvider().getDelegate();
}

function registerContextManager(): void {
  const manager = new AsyncLocalStorageContextManager();
  if (context.setGlobalContextManager(manager.enable())) {
    return;
  }
  // Another manager is already registered (the app's, or ours from an earlier
  // runtime). Keep it and drop ours; never disable the registered one. The
  // API also logs a diag error about the duplicate attempt; that is expected.
  manager.disable();
}

function requireApiKey(config: Config): string {
  // Unreachable through init(): resolveConfig disables the config instead of
  // returning an enabled one without a key.
  if (config.apiKey === undefined) {
    throw new Error("Runtime.create requires a config with an API key");
  }
  return config.apiKey;
}

function errorName(error: unknown): string {
  return error instanceof Error ? error.name : typeof error;
}

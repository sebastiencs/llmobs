export { ConfigError, type LlmobsOptions } from "./config.js";
export { flush, init, shutdown, spanProcessor, tracerProvider } from "./runtime.js";

/** Package version. Kept equal to package.json. */
export const VERSION = "0.0.0";

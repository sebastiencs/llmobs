# TypeScript SDK

The `llmobs` package (MIT, ESM only) is the TypeScript SDK. Spec: `docs/spec/10-sdk.md`.

What exists now is configuration resolution in `src/config.ts`. `init()` and tracing are not implemented yet. The package entry exports `VERSION` only.

Node ≥ 22, plus Bun, Deno, and edge runtimes (D77).

Checks: `just check-ts` (Biome, tsc, vitest, tsdown, publint, attw).

# Web UI

Vite + React + TypeScript. Spec: `docs/spec/11-ui.md`.

## Run

- `just web-dev` — dev server. The API is mocked from `fixtures/api`.
- `just web-dev-server` — same UI. `/api` and `/v1` are proxied to `localhost:8080`.

## Mocks

MSW reads `fixtures/api/manifest.json`. An entry's path params must match. A missing `project`, and `org` when the entry omits it, matches any value. Query matching picks the entry with the largest matching subset.

## API types

`just openapi` regenerates `crates/server/openapi.json` and `web/src/api/schema.gen.ts`. `pnpm --dir web run check:api` fails when the generated file is stale.

## Add a page

1. Add a route file under `web/src/routes/`. `pnpm --dir web run gen:routes` writes `routeTree.gen.ts`. Typecheck and test run it first.
2. Prefetch in the loader with `ensureQueryData` or `ensureInfiniteQueryData`.
3. Test with `renderApp` from `web/src/test/render.tsx`. Override one endpoint with `server.use`.

## Production build

`pnpm --dir web run check` lints, typechecks, tests, builds, and checks `dist/` for mock code, fixture paths, and the example email.

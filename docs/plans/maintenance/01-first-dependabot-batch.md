# Maintenance · The first Dependabot batch

Plan for the first five Dependabot PRs (opened 2026-11-10). Part of the work is for the user on GitHub. The rest is one sub-phase (M1), done by one agent and ending with `just check` green and one commit.

**Read first:** `AGENTS.md` § Dependencies and § CI, `docs/mvp-decisions.md` D81, `.github/dependabot.yml`, `deny.toml`.

## Review of the PRs

The review read each branch's diff, ran the affected `just` recipes locally, and looked up the PRs' check results on GitHub.

| PR | Change | CI | Verdict |
|---|---|---|---|
| [sebastiencs/llmobs#1](https://github.com/sebastiencs/llmobs/pull/1) | `taiki-e/install-action` 2.89.1 → 2.90.0 | 10/10 | **Correct but incomplete.** Only the two pins in `ci.yml` moved. The same action is pinned in `.github/actions/rust/action.yml` and `.github/actions/js/action.yml`, which Dependabot does not scan: the config only lists directory `/`, and Dependabot reads composite actions only from directories it is told about. Merging would leave two versions of the action in use. |
| [sebastiencs/llmobs#2](https://github.com/sebastiencs/llmobs/pull/2) | `arrow` and `parquet` 59.3.2 → 60.1.0 (major) | 8/10, `rust-test` fails | **Reject.** It breaks the pinned family: DataFusion 55.2.0 is the latest release and requires `arrow ^59.2`, so DataFusion and arrow would end up on different majors. It also breaks core: arrow 60's `MapBuilder` names the map entry fields `key`/`value` instead of `keys`/`values`, so 8 tests in `crates/core/src/batch.rs` fail with "column types must match schema types" (reproduced locally). |
| [sebastiencs/llmobs#3](https://github.com/sebastiencs/llmobs/pull/3) | `react`, `react-dom` 19.1.10 → 19.3.1 | 10/10 | **Merge.** `just check-web` passes locally: 29/29 tests and a production build. `@types/react` 19.3.0 already matches. |
| [sebastiencs/llmobs#4](https://github.com/sebastiencs/llmobs/pull/4) | `lucide-react` 1.66.0 → 1.67.0 | 10/10 | **Merge.** `just check-web` passes locally. |
| [sebastiencs/llmobs#5](https://github.com/sebastiencs/llmobs/pull/5) | `typescript` 5.9.3 → 7.0.2 (major) | 8/10, `web` fails | **Reject for now.** `openapi-typescript` 7.13.0 (the latest) declares `typescript: ^5.x` and calls the TypeScript JS compiler API, which TypeScript 7 (the native compiler) does not provide. `check:api` crashes with `Cannot read properties of undefined (reading 'createKeywordTypeNode')`. The TS SDK's own checks pass on 7.0.2, but both packages stay on one TypeScript version. |

`ci-ok` is red on both rejected PRs, so neither can be merged by accident once `ci-ok` is a required status.

## Why CI could not have caught the DataFusion mismatch

No crate depends on `datafusion` yet. Cargo resolves only what a crate uses, so `datafusion` is not in `Cargo.lock`, and the pin in `[workspace.dependencies]` is checked by nothing. #2 fails today only because of the map field names. A later arrow major without such a change would pass CI while splitting the family. M1 adds a `cargo-deny` rule that fails as soon as two versions of a family crate are in the graph. It takes effect when the storage track adds `datafusion`.

## Steps for the user on GitHub

The agent cannot do these: there is no `gh` in the sandbox, and merging is the user's call.

1. **Merge #3 and #4** (React, lucide) now. They are independent of M1.
2. **Close #2 and #5** with a short comment, for example "Blocked: DataFusion 55.2 requires arrow 59" and "Blocked: openapi-typescript requires TypeScript 5". The `ignore` rules M1 adds keep them from coming back. Do not use the `@dependabot ignore` comment commands: the config file stays the only place where ignores live.
3. **Leave #1 open** until M1 is merged. Then comment `@dependabot recreate` on #1, so that it updates the two composite actions too. Merge it once CI is green.

## M1 — Dependabot and cargo-deny follow the pins

**Edit only:** `.github/dependabot.yml`, `deny.toml`, `AGENTS.md` § Dependencies.
**Commit:** `ci: scan composite actions and hold blocked majors`

### 1. Scan the composite actions

In the `github-actions` entry of `.github/dependabot.yml`, replace `directory: /` with:

```yaml
    directories:
      - /
      - /.github/actions/*
```

Check the current Dependabot options reference: `directories` with a glob must be supported for `github-actions`. If the glob is not, list `/.github/actions/rust` and `/.github/actions/js` explicitly. The `actions` group stays as it is, so one PR covers the workflows and both composite actions.

### 2. Hold the arrow-side majors of the DataFusion family

In the `cargo` entry, add:

```yaml
    ignore:
      # DataFusion picks the arrow, parquet and object_store majors. A major of these
      # arrives only together with a DataFusion major, and that PR is completed by hand
      # (AGENTS.md § Dependencies).
      - dependency-name: "arrow"
        update-types: ["version-update:semver-major"]
      - dependency-name: "arrow-*"
        update-types: ["version-update:semver-major"]
      - dependency-name: "parquet"
        update-types: ["version-update:semver-major"]
      - dependency-name: "object_store"
        update-types: ["version-update:semver-major"]
```

`datafusion` majors are not ignored. When DataFusion 56 (or later) comes out, Dependabot opens a PR for it alone. Whoever takes that PR bumps `arrow`, `parquet` and `object_store` to the versions that DataFusion release requires (`cargo info datafusion@<version>`), in the same PR, and fixes what breaks. Leave a short note in the YAML comment pointing at the arrow 60 map field change (next section), since the first such upgrade will hit it.

Minor and patch updates of the family still go through the `datafusion-family` group.

### 3. Hold the TypeScript major

In the `npm` entry, add:

```yaml
    ignore:
      # openapi-typescript 7.13.0 declares typescript ^5.x and uses the JS compiler API,
      # which TypeScript 7 does not ship. Remove when openapi-typescript supports it.
      - dependency-name: "typescript"
        update-types: ["version-update:semver-major"]
```

TypeScript 6.0.3 also exists. It still has the JS API, but `openapi-typescript`'s `^5.x` peer range excludes it, so it is out of scope here. Do not upgrade to it.

### 4. Fail on two versions of a family crate

In `deny.toml`, `[bans]`, make cargo-deny fail when the graph holds more than one version of `datafusion`, `arrow`, `parquet` or `object_store`. Check how cargo-deny 0.20.2 (the pinned version, see `.github/actions/rust/action.yml`) expresses a per-crate multiple-versions error. It is a `deny` entry with a multiple-versions flag; take the exact field name from the cargo-deny docs for that version. Cover the arrow sub-crates (`arrow-array`, `arrow-schema`, `arrow-buffer`, `arrow-data`, …) too, either one entry per crate or with a wildcard if the version supports one. Add a comment pointing at AGENTS.md § Dependencies.

The global `multiple-versions = "warn"` stays.

Test it in a scratch worktree, not committed:
1. Add a dev-dependency `arrow-schema = "60"` to `crates/core`.
2. Run `just deny` and check that it now fails, naming `arrow-schema`.
3. Revert.

Say in the report that you did this.

### 5. AGENTS.md § Dependencies

Replace the family bullet with:

> - `datafusion`, `arrow`, `parquet` and `object_store` are a pinned family: never bump one without the others. DataFusion picks the majors. Dependabot ignores arrow-side majors; a DataFusion major PR is completed by hand with the arrow, parquet and object_store versions that release requires. `cargo deny` fails on two versions of a family crate.

And after the JS bullet:

> - `typescript` stays on 5.x while `openapi-typescript` requires it (`.github/dependabot.yml` ignores TypeScript majors).

### Checks

- `just check` and `just check-workflows` are green.
- Validate `.github/dependabot.yml` against SchemaStore's `dependabot-2.0` schema (as plan 06 S4 did).

### Report

- The exact `deny.toml` syntax used, and the output of the failing `just deny` in the scratch test.
- Whether the `directories` glob was accepted, or which directories were listed.

## Note for the first DataFusion upgrade

Arrow 60 changed the default field names `MapBuilder` gives map entries, from `keys`/`values` to `key`/`value`. `crates/core/src/schema.rs` declares `keys`/`values` (spec 01 and the `insta` snapshots pin them). When the family moves to arrow ≥ 60:
- `batch.rs` must build its maps with explicit field names that match the schema (`MapBuilder::new` with `MapFieldNames`);
- or, if we decide to follow arrow's new default, the schema, spec 01 and the snapshots change in the same commit.

Stored Parquet files are not a concern yet: nothing has been written.

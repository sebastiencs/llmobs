# Phase 0 · Steps 6 and 7 — CI and Phase 0 close-out

Implementation plan, split into small sub-phases. Each sub-phase is done by one agent in one go and ends with `just check` green and one commit.

**Prerequisites:** steps 1–5 merged, including step 5 S8 (Node ≥ 22.12, D80).

**Read first:**
- `AGENTS.md`, especially § Commands, § Dependencies and § Parallel work.
- `docs/mvp-decisions.md`: D53, D55, D57, D74, D77, D79 and **D81** (written by S5 of this plan; its text is below).
- `docs/spec/12-operations.md` § Packaging and § Security checklist.
- `justfile`: CI runs the same recipes developers run.
- The docs of every GitHub Action and tool for the exact version you pin. Where this plan names an input or flag the pinned version renamed, use the new name and say so in your report.

## Why these steps exist

Step 6 is GitHub Actions CI (D55). Wave 1 runs three parallel sessions on separate branches (D53). Until now, nothing outside a developer's machine checks that a branch passes `just check`. CI makes that check automatic on every pull request and push to `main`, and gives a single required status, `ci-ok`.

Step 7 of the original Phase 0 list was "record the new decisions (D52 onward), then commit". Decisions were recorded step by step (D52–D80), so step 7 no longer needs a plan of its own. It becomes the last sub-phase here (S6): an audit that the decision log, specs and plans agree before Wave 1 branches off.

## Scope

**In:**
- `.github/workflows/ci.yml`:
  - Rust jobs: format + clippy, tests, cargo-deny.
  - Web job.
  - TypeScript SDK on the repo's Node version and a packed-package smoke test on Node 22.12.
  - Python SDK on 3.11 and 3.15.
  - A clean-tree check in every job, and the `ci-ok` aggregate.
  - Workflow linting.
- `.github/workflows/advisories.yml`: weekly RustSec advisory check.
- `.github/dependabot.yml`: weekly updates for Actions, cargo, npm (pnpm) and uv, with a 7-day cooldown and grouped PRs.
- Two composite actions for the shared setup steps.
- `just` recipes: `assert-clean`, `ts-smoke`, `check-workflows`, `deny-advisories`.
- Decision D81, spec 12 edits, `AGENTS.md` § CI.
- Phase 0 close-out: decision-log audit, stale crate layout in the spec README, the Open list.

**Out:**
- Release builds, cross-compilation, Docker images, publishing to npm/PyPI, and macOS runners. These come with the server track, once the binary serves the embedded UI and `/healthz` (user decision; D81).
- SDK integration tests against a running `llmobs serve` (spec 10 § Testing). There is no server yet.
- Coverage reports, benchmarks in CI, and caching across forks.
- Repository settings: branch protection, Dependabot alerts, Actions permissions. These are the user's to change. The report lists what to set (S5).

AGENTS.md applies to workflow code too:
- no placeholder jobs or recipes for future work;
- no abstraction without at least two users (a composite action needs two or more jobs using it);
- every `run:` step calls a `just` recipe or is a one-line setup command.

## Decisions this plan relies on

| # | Summary |
|---|---|
| D55 | GitHub Actions CI; `just` as task runner. |
| D57 | cargo-deny runs advisories, bans and sources; licenses deferred. |
| D74 | Exact JS versions; `minimumReleaseAge` 7 days; uv for Python. |
| D77 | Node ≥ 22.12, ESM only; Python ≥ 3.11. |
| D79 | `just check` = Rust + web + Python + TS. |
| D81 (new, S5) | Linux-only CI calling `just` recipes; actions pinned by SHA; `ci-ok` aggregate; SDKs on min + latest runtimes; weekly advisories; Dependabot with a 7-day cooldown and the DataFusion family grouped; no release pipeline yet; cargo-deny advisories replace `cargo audit`. |

User answers behind D81: Linux runners only, no release artifacts yet, Dependabot (not Renovate), and SDKs tested on the minimum and latest runtimes.

## Versions and pinning

- **Actions are pinned by full commit SHA**, with the tag as a trailing comment: `uses: actions/checkout@<40-hex-sha> # v5.0.0`.
  - Pick the latest release published **at least 7 days ago**, the same rule as `minimumReleaseAge`.
  - Resolve each SHA with `git ls-remote https://github.com/<owner>/<repo> 'refs/tags/<tag>*'`. For an annotated tag, use the `^{}` (dereferenced) line, which is the commit.
  - Never pin a branch or a moving tag such as `v4`.
- **Actions used:** `actions/checkout`, `actions/setup-node`, `pnpm/action-setup`, `astral-sh/setup-uv`, `Swatinem/rust-cache`, `taiki-e/install-action`. Add no others unless a step cannot be written without one, and justify any addition in your report.
- **Tool versions are exact:** `just@X.Y.Z`, `cargo-deny@X.Y.Z`, `actionlint@X.Y.Z`, `zizmor@X.Y.Z`, and the uv version in `setup-uv`.
  - Use the latest release that is at least 7 days old.
  - The cargo-deny version must accept the existing `deny.toml` (`version = 2` sections).
  - If you have the tools installed locally, the local `just` and `cargo-deny` versions are the ones to match, unless they are older than the latest by a major version.
- **Runtimes come from the repo,** never duplicated in workflow files:
  - Rust from `rust-toolchain.toml`;
  - Node from `.node-version` (`node-version-file`);
  - pnpm from the root `package.json` `packageManager` (`pnpm/action-setup` with no `version` input);
  - Python 3.11 from `sdks/python/.python-version`.
  - The only literal runtime versions in workflows are the matrix extras `3.15` and `22.12.0`.

List every SHA, tag and tool version you chose in your report.

## Final layout

```
.github/
  actions/
    rust/action.yml        # toolchain from rust-toolchain.toml + rust-cache + just
    js/action.yml          # pnpm + Node (.node-version, pnpm cache) + just
  workflows/
    ci.yml                 # every check; required status: ci-ok
    advisories.yml         # weekly cargo-deny advisories
  dependabot.yml
  zizmor.yml               # only if a finding needs a justified ignore
justfile                   # + assert-clean, ts-smoke, check-workflows, deny-advisories
```

Job ids are stable, because branch protection and humans refer to them:

| Job id | Runs |
|---|---|
| `rust-lint` | `just fmt-check`, `just lint` |
| `rust-test` | `just test` |
| `rust-deny` | `just deny` |
| `web` | `just check-web` |
| `ts-sdk` | `just check-ts` (Node from `.node-version`) |
| `ts-sdk-min-node` | pack with the repo Node, then `just ts-smoke` on Node 22.12.0 |
| `python-sdk` (matrix `3.11`, `3.15`) | `just check-py` |
| `workflows` | `just check-workflows` |
| `ci-ok` | aggregate; the only required check |

---

## S1 — CI workflow with the Rust jobs

### `justfile`

Add, next to the other check recipes:

```
# Fail if the working tree changed (generated files, snapshot or fixture writes)
assert-clean:
    git status --porcelain --untracked-files=all
    git diff --exit-code
    test -z "$(git status --porcelain --untracked-files=all)"
```

The first line prints what changed, so the CI log shows it. Don't add `assert-clean` to `check`. Locally the tree is usually dirty while you work.

### `.github/actions/rust/action.yml`

A composite action (`runs.using: composite`) with one input, `cache` (default `"true"`). Steps:

1. `rustup toolchain install`: with no arguments, it installs the toolchain and components named in `rust-toolchain.toml`. Verify this works with the rustup on `ubuntu-latest`. If that rustup does not support it, use `rustup show` and say so in your report. Use `shell: bash`.
2. `Swatinem/rust-cache`, only when `inputs.cache == 'true'`, with:
   - `key: ${{ github.job }}`: clippy and test builds produce different artifacts, so each job gets its own cache;
   - `save-if: ${{ github.ref == 'refs/heads/main' }}`: PRs restore but never save, which keeps cache-poisoning risk and cache churn low.
3. `taiki-e/install-action` with `tool: just@<version>,cargo-deny@<version>`. `rust-deny` and the weekly advisories job need cargo-deny; installing one small prebuilt binary in the other jobs is cheaper than a second input.

### `.github/workflows/ci.yml` (S1 state)

```yaml
name: CI

on:
  pull_request:
  push:
    branches: [main]
  workflow_dispatch:

permissions:
  contents: read

concurrency:
  group: ci-${{ github.event.pull_request.number || github.ref }}
  cancel-in-progress: ${{ github.event_name == 'pull_request' }}

env:
  CARGO_TERM_COLOR: always
  CARGO_INCREMENTAL: "0"
  RUST_BACKTRACE: short
```

Jobs: `rust-lint`, `rust-test` and `rust-deny`, all on `runs-on: ubuntu-latest`. Each job:
1. `actions/checkout` with `persist-credentials: false`.
2. `uses: ./.github/actions/rust` (`rust-deny` passes `with: { cache: "false" }`).
3. Run the job's recipes (job table above).
4. `just assert-clean`.

Timeouts: `rust-lint` 30 min, `rust-test` 45 min, `rust-deny` 10 min.

Notes:
- `rust-deny` builds nothing. It needs the toolchain (cargo-deny calls `cargo metadata`), but no cache.
- GitHub sets `CI=true`. `insta` then refuses to write snapshots, and tests fail instead. `assert-clean` also catches any test that writes into the repo. Don't set the `LLMOBS_*_WRITE` variables.
- Disk: if `rust-test` runs out of space on `ubuntu-latest`, report it with the `df -h` output. Don't add disk-cleanup steps preemptively.

### `ci-ok` (added now, extended in S2 and S3)

```yaml
  ci-ok:
    if: always()
    needs: [rust-lint, rust-test, rust-deny]
    runs-on: ubuntu-latest
    timeout-minutes: 5
    steps:
      - name: All required jobs succeeded
        run: test "${{ !contains(needs.*.result, 'failure') && !contains(needs.*.result, 'cancelled') && !contains(needs.*.result, 'skipped') }}" = true
```

`if: always()` makes `ci-ok` report failure when a dependency fails, instead of being skipped. GitHub treats a skipped check as passing.

### Verify

- `just assert-clean` passes on a clean tree and fails after `touch x`. Remove `x` afterwards.
- Run `just fmt-check lint test deny` locally.
- Validate the YAML. S3 adds actionlint and zizmor; if they are installed already, run them now.

**Commit:** `ci: add GitHub Actions workflow with Rust jobs`

---

## S2 — Web, TypeScript SDK and Python SDK jobs

### `.github/actions/js/action.yml`

A composite action with no inputs:
1. `pnpm/action-setup`, with no `version` input; it reads `packageManager`.
2. `actions/setup-node` with `node-version-file: .node-version` and `cache: pnpm`.
3. `taiki-e/install-action` with `tool: just@<same version as the rust action>`.

The `just` recipes run `pnpm install --frozen-lockfile` themselves (`js-install`). Don't install in the composite.

### `ts-smoke` recipe and the minimum-Node job

`justfile`:

```
# Install a packed TypeScript SDK into a scratch project and load it from ESM and CommonJS
ts-smoke tarball:
    #!/usr/bin/env bash
    set -euo pipefail
    tarball="$(realpath '{{tarball}}')"
    expected="$(tar -xOzf "$tarball" package/package.json | node -p 'JSON.parse(require("fs").readFileSync(0, "utf8")).version')"
    dir="$(mktemp -d)"
    trap 'rm -rf "$dir"' EXIT
    cd "$dir"
    npm init -y >/dev/null
    npm install --no-audit --no-fund --ignore-scripts "$tarball" >/dev/null
    EXPECTED="$expected" node --input-type=module -e 'import { VERSION } from "llmobs"; if (VERSION !== process.env.EXPECTED) { console.error(`ESM: VERSION ${VERSION}`); process.exit(1); }'
    EXPECTED="$expected" node -e 'const { VERSION } = require("llmobs"); if (VERSION !== process.env.EXPECTED) { console.error(`CJS: VERSION ${VERSION}`); process.exit(1); }'
    echo "llmobs ${expected} loads from ESM and CommonJS on Node $(node --version)"
```

The CommonJS line is the point of this recipe. It proves the D77 claim that an ESM-only package works for `require()` callers on Node 22.12. The package has no dependencies, so `npm install` fetches nothing from the registry. `--ignore-scripts` keeps it that way.

Job `ts-sdk-min-node` steps:
1. Checkout (`persist-credentials: false`) and `./.github/actions/js`.
2. `just js-install`, then `pnpm --dir sdks/typescript run build`, then `pnpm --dir sdks/typescript pack --pack-destination "$RUNNER_TEMP"`. The build and pack run on the repo Node, because pnpm and tsdown may not run on 22.12.
3. `actions/setup-node` with `node-version: 22.12.0`, and no cache.
4. `just ts-smoke "$RUNNER_TEMP"/llmobs-*.tgz`. Resolve the glob in the step with `shell: bash`. Fail if it matches anything other than exactly one file.
5. Unit tests on 22.12. Check the `engines` field of the pinned `vitest` (`npm view vitest@<version> engines`).
   - If it allows 22.12, run `pnpm --dir sdks/typescript exec vitest run` right after step 4, still on Node 22.12. pnpm itself must run under the active Node; if pnpm 11 refuses Node 22.12, call the binary directly with `node sdks/typescript/node_modules/vitest/vitest.mjs run` (check the path in the installed package).
   - If vitest does not allow 22.12, skip the unit tests here and say so in your report. The packed-package smoke test is the required part.
6. `just assert-clean`. The tarball is in `$RUNNER_TEMP`, outside the repo.

Timeout: 15 min.

### Other jobs

| Job | Steps after checkout | Timeout |
|---|---|---|
| `web` | `./.github/actions/js`, `just check-web`, `just assert-clean` | 20 min |
| `ts-sdk` | `./.github/actions/js`, `just check-ts`, `just assert-clean` | 15 min |
| `python-sdk` | `astral-sh/setup-uv` (pinned `version`, `enable-cache: true`, `cache-dependency-glob: sdks/python/uv.lock`), `taiki-e/install-action` (`just`), `just check-py`, `just assert-clean` | 15 min |

`python-sdk` details:
- `strategy.matrix.python: ["3.11", "3.15"]`, `fail-fast: false`, job `name: python-sdk (${{ matrix.python }})`.
- Job-level `env: UV_PYTHON: ${{ matrix.python }}`. `UV_PYTHON` overrides `.python-version`, and uv downloads a managed interpreter when the runner has none. Check that `uv sync --locked` succeeds for 3.15 with the committed `uv.lock`, whose `requires-python` is `>=3.11`, so one lock covers both versions.
- Check that 3.15.0 final is released and at least 7 days old. If it isn't, use the newest stable minor version, and report it.
- `filterwarnings = ["error"]` turns a new-version `DeprecationWarning` into a failure. That's intended. Fix the code, don't silence the warning.

Add every new job to `ci-ok.needs`.

### Verify locally

- `just check-web check-ts check-py`.
- `pnpm --dir sdks/typescript run build && pnpm --dir sdks/typescript pack --pack-destination /tmp/llmobs-pack && just ts-smoke /tmp/llmobs-pack/llmobs-*.tgz`. Run this with your current Node, and with Node 22.12 if you can install it (`fnm`, `nvm`, or `npx node@22.12.0`).
- `UV_PYTHON=3.15 just check-py` if uv can fetch 3.15.

**Commit:** `ci: add web, TypeScript SDK and Python SDK jobs`

---

## S3 — Workflow linting and weekly advisories

### `check-workflows`

`justfile`:

```
# Lint GitHub Actions workflows (actionlint) and audit them for security issues (zizmor)
check-workflows:
    actionlint
    zizmor --offline .github
```

- Don't add it to `just check`. D79 defines `check` as the four toolchains, and these two tools are optional locally.
- In `AGENTS.md` § Required tools (S5), list them as needed only when editing `.github/`.
- `zizmor --offline` covers the audits that matter here: unpinned `uses`, `persist-credentials`, template injection, excessive permissions and cache poisoning. Online audits need a token. Leave them out.

Job `workflows`:
- checkout with `persist-credentials: false`;
- `taiki-e/install-action` with `tool: just@…,actionlint@…,zizmor@…`;
- `just check-workflows`.

Timeout: 5 min. Add it to `ci-ok.needs`.
- If `taiki-e/install-action` does not support zizmor at your pinned version, install it with `astral-sh/setup-uv` and run `uvx zizmor@<version> --offline .github` via a recipe variable. Say which way you used.

Fix every finding. If a finding is a deliberate choice, ignore it with an inline `# zizmor: ignore[<rule>]` comment that explains why.
- **Example:** the `cache-poisoning` audit on `Swatinem/rust-cache` is mitigated by `save-if` on `main` only. If zizmor still flags it, ignore it with that reason.
- Create `.github/zizmor.yml` only if an inline ignore is not possible.

### `.github/workflows/advisories.yml`

RustSec advisories change while the code doesn't, so `main` must be checked on a schedule:

```yaml
name: Advisories

on:
  schedule:
    - cron: "17 6 * * 1"   # Mondays 06:17 UTC; off the hour to avoid the scheduler rush
  workflow_dispatch:

permissions:
  contents: read
```

One job, `rust-advisories`:
- checkout with `persist-credentials: false`;
- `./.github/actions/rust` with `cache: "false"`;
- `just deny-advisories`, a new recipe: `cargo deny check advisories` (comment: "RustSec advisories only; run weekly by CI").

Timeout: 10 min.
- A failed scheduled run emails the repo owner by default; nothing else is needed.
- JavaScript and Python advisories are covered by Dependabot alerts, which the user turns on in the repo settings (S5 report).

**Commit:** `ci: lint workflows and check advisories weekly`

---

## S4 — Dependabot

`.github/dependabot.yml`, `version: 2`. Every entry has:
- `schedule`: `interval: weekly`, `day: monday`, `time: "05:00"`, `timezone: Etc/UTC`;
- `cooldown: { default-days: 7 }`, the same rule as `minimumReleaseAge`;
- `open-pull-requests-limit: 5`;
- `labels: [dependencies]`;
- `commit-message: { prefix: deps }`.

| Ecosystem | `directory` | Groups (first match wins, so order matters) |
|---|---|---|
| `github-actions` | `/` (it also scans `.github/actions/*`) | `actions`: `patterns: ["*"]` |
| `cargo` | `/` | 1. `datafusion-family`: `patterns: ["datafusion", "datafusion-*", "arrow", "arrow-*", "parquet", "object_store"]`, with **no** `update-types`, so majors move together too (AGENTS.md: the family is pinned together). 2. `cargo-minor-patch`: `patterns: ["*"]`, `update-types: [minor, patch]`. |
| `npm` | `/` (the pnpm workspace root, where `pnpm-lock.yaml` is) | 1. `react`: `react`, `react-dom`, `@types/react`, `@types/react-dom`. 2. `tanstack-router`: `@tanstack/react-router`, `@tanstack/router-*`. 3. `vitest`: `vitest`, `@vitest/*`. 4. `openapi`: `openapi-fetch`, `openapi-react-query`, `openapi-typescript`. 5. `npm-minor-patch`: `*`, `[minor, patch]`. Groups 1–4 have no `update-types`. They are the "same major" sets from step 5 § Versions. |
| `uv` | `/sdks/python` | `uv-dev`: `patterns: ["*"]` |

Notes:
- **pnpm 11 support.** Check Dependabot's docs for the pnpm version it supports for `npm`.
  - If pnpm 11 lockfiles are not supported, or Dependabot would rewrite the lockfile with an older pnpm, remove the `npm` entry and add an `## Open` item in `docs/mvp-decisions.md`: "JS dependency updates are manual until Dependabot supports pnpm 11."
  - Say which case applies.
- **What Dependabot doesn't update:** `rust-toolchain.toml`, `.node-version` and `packageManager` (the pnpm version). S5 documents that these are updated manually.
- **Major updates outside the four npm groups and the DataFusion family** arrive as one PR per package.
- **Validate the file:** `actionlint` doesn't check it. Use the JSON schema from SchemaStore (`dependabot-2.0.json`) with any validator you can run offline (e.g. `uvx check-jsonschema --schemafile <downloaded schema> .github/dependabot.yml`), and report the result. No new recipe is needed for a one-off validation.

**Commit:** `ci: add Dependabot configuration`

---

## S5 — Decision D81, spec 12 and `AGENTS.md`

### `docs/mvp-decisions.md`

Add after D80:

| D81 | CI | GitHub Actions on Linux runners only. Jobs call `just` recipes: Rust fmt + clippy, tests and cargo-deny as separate jobs, web, TypeScript SDK, Python SDK on 3.11 and 3.15, and a packed TypeScript SDK loaded from ESM and CommonJS on Node 22.12. Every job ends with a clean-tree check, and `ci-ok` is the single required status. Actions are pinned by commit SHA; workflows are linted with actionlint and zizmor. RustSec advisories are also checked weekly; `cargo deny check advisories` replaces `cargo audit` (same database). Dependabot opens weekly grouped PRs with a 7-day cooldown, and the DataFusion family moves in one PR. No release pipeline until the server serves the embedded UI and `/healthz`. |

### `docs/spec/12-operations.md`

- § Packaging, Binary row: after "Built in CI with `cargo build --release` …", add "(release pipeline added with the server track, D81)".
- § Security checklist: replace "`cargo deny` + `cargo audit` in CI; dependency licenses compatible with any license we later pick." with "`cargo deny` (advisories, bans, sources) on every CI run and `cargo deny check advisories` weekly (RustSec, the database `cargo audit` uses); Dependabot alerts for JS and Python; dependency licenses compatible with any license we later pick (license check enabled once D9 is decided, D57)."

### `AGENTS.md`

- § Commands table: add `just check-workflows` ("lint and audit GitHub Actions workflows (actionlint, zizmor)"), `just assert-clean` ("fail if the working tree changed; CI runs it after each job"), `just ts-smoke <tarball>` ("load a packed TS SDK from ESM and CommonJS") and `just deny-advisories` ("RustSec advisories only").
- Required tools: add "`actionlint` and `zizmor` (only when editing `.github/`)".
- New section `## CI` after § Commands, at most 12 lines:
  - `.github/workflows/ci.yml` runs on every PR and every push to `main`; `ci-ok` is the required status.
  - CI runs the `just` recipes from the table. A branch that passes `just check` locally and leaves a clean tree passes CI, except for the runtime matrix (Python 3.15, Node 22.12).
  - Actions are pinned by commit SHA with the tag in a comment; Dependabot updates them. Never use a tag or branch reference.
  - Workflow `run:` steps call `just` recipes. New checks go into the `justfile` first.
  - Updated by hand: `rust-toolchain.toml`, `.node-version`, `packageManager`.
- § Dependencies: add "Dependabot PRs follow the same rules; the DataFusion family arrives as one PR."

**Commit:** `docs: record the CI decision`

**Report back (S5):** the repository settings the user should change, as a checklist:
- branch protection on `main`: require PRs, require the `ci-ok` status, require branches to be up to date;
- Dependabot alerts and security updates on;
- Actions → General: "Read repository contents" as the default workflow token permission, and "Allow GitHub Actions to create and approve pull requests" off.

Don't change these settings yourself.

---

## S6 — Phase 0 close-out (old step 7)

Phase 0's last step was to record the decisions and commit. That happened step by step. This sub-phase checks that the result is consistent, so Wave 1 sessions read one truth. Only edit `docs/` and `AGENTS.md`; any code change you find necessary goes in the report instead.

### 1. Decision log

- `docs/mvp-decisions.md` numbers D1 through D81 exactly once each, in order, with no gaps. Report any gap or duplicate; don't renumber, because plans and specs cite the numbers.
- Every `D<n>` cited in `docs/spec/`, `docs/plans/`, `AGENTS.md`, `crates/`, `web/`, `sdks/` and `fixtures/` exists in the log. For example: `rg -o 'D[0-9]{1,3}\b' -g '!target' -g '!node_modules' | sort -u`, then compare. Ignore false positives such as hex strings or CSS. Report every citation to a missing decision.
- Every decision that names a spec section (for example "spec 10 § Configuration") points at a section that exists.

### 2. Stale crate layout in `docs/spec/README.md`

§ Crate layout still shows the pre-D52 layout with 11 crates (`llmobs-query`, `llmobs-meta`, `llmobs-auth`, `llmobs-eval`, `llmobs-alerts`, `llmobs-mail`, `llmobs-api`, and a `crates/llmobs/` binary) and the matching dependency direction.
- Replace the block and the "Dependency direction" line with the D52 layout, using the wording of `AGENTS.md` § Layout and its enforced dependency graph.
- Map the old crates to their new homes:
  - query → `llmobs-storage`;
  - meta, auth, eval, alerts, mail, api → modules of `llmobs-server`;
  - the binary → `llmobs-cli`.
- `web/` is embedded into the binary by `llmobs-server` (`memory-serve`), not by `crates/llmobs-api`.

Then search every spec for the old crate names, `rg -n 'llmobs-(query|meta|auth|eval|alerts|mail|api)\b|crates/llmobs/' docs`:
- Replace each with the new crate or module, e.g. "`llmobs-server` (`auth` module)".
- Don't invent module names that don't exist yet; "`llmobs-server`" alone is fine.
- Leave `docs/research/` unchanged; it is a historical record.

### 3. The Open list

`docs/mvp-decisions.md` § Open currently lists the license (D9) and the real name (D46). Add each of these items, unless a spec or decision already settles it. Check first, and say which ones you dropped and why.

- **`.cursor/` is untracked.** D58 says `.cursor/rules` is kept, and the plans tell agents to read it, but `git status` shows `.cursor/` as untracked. The user decides whether to commit it or drop D58's mention.
- **Cost when a model has a price entry but some token prices are null** while those tokens are present (spec 02 § Cost): null cost or zero for that part?
- **Embedding spans:** whether `cost_output` is always null or 0.
- **`ai.settings.*` (Vercel AI SDK):** whether the prefix is stripped when mapping to request parameters.
- **Vercel `ai.response.text`:** whether it maps to the output message when `ai.response.messages` is absent.
- **Nullable optional query parameters** in the generated TypeScript (`web/src/api/schema.gen.ts`): optional query params come out as `T | null`. Decide whether to keep that or change the utoipa annotations.

Each item is one line, stated as a question with the place it affects.

### 4. Final review

1. Check out a fresh copy of the branch (`git clone` the local repo into a temp dir), then run `just check` and `just check-workflows` there. Both must pass, and `just assert-clean` must pass afterwards.
2. `just openapi`, `just api-examples` and `just fixtures-fmt` leave the tree unchanged.
3. Ask the user to push the branch and open a PR. If you can push yourself, push to a non-`main` branch only. Every CI job must pass on the PR. Report job durations; the `rust-test` duration with a cold and a warm cache is the number Wave 1 will feel.
4. Re-read `AGENTS.md` top to bottom: every command, path and rule matches the repo.

**Report back:**
- every action SHA/tag and tool version (S1–S3);
- the Dependabot pnpm outcome (S4);
- the repository-settings checklist (S5);
- decision-log findings, stale references replaced, and Open items added or dropped (S6);
- CI durations per job;
- every place you had to interpret this plan or a spec.

**Commit:** `docs: close Phase 0`

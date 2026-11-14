# List recipes
default:
    @just --list

# Format Rust, the web and TypeScript SDK (Biome), and the Python SDK (ruff)
fmt:
    cargo fmt --all
    pnpm exec biome format --write .
    cd sdks/python && uv run ruff format

# Check formatting
fmt-check:
    cargo fmt --all -- --check

# Lint (warnings are errors)
lint:
    cargo clippy --workspace --all-targets --all-features -- -D warnings

# Run all Rust tests
test:
    cargo test --workspace --all-features
    # The crash tests must reuse the workspace feature set: `-p llmobs-storage`
    # resolves different features and rebuilds every dependency from scratch.
    cargo test --workspace --all-features -- --ignored --exact crash_tests::every_flush_hook_aborts_safely
    cargo test --workspace --all-features -- --ignored --exact crash_tests::every_compaction_hook_aborts_safely
    cargo test --workspace --all-features -- --ignored --exact crash_tests::every_deletion_hook_aborts_safely
    cargo test --workspace --all-features -- --ignored --exact crash_tests::every_media_hook_aborts_safely
    cargo test --workspace --all-features -- --ignored --exact crash_tests::every_drop_hook_aborts_safely

# Opt-in live provider tests. Ignored by `just check` (D167).
eval-live:
    cargo test -p llmobs-eval --test live -- --ignored --test-threads 1

# Supply-chain checks: advisories, bans, licences, sources (D-P-3)
deny:
    cargo deny check advisories bans licenses sources

# Debug build
build:
    cargo build --workspace

# Build the web UI, then the release binary with the UI embedded (D-G-18)
build-release: js-install
    pnpm --dir web run build
    cargo build --release -p llmobs-cli

# Build the release binary and check that the UI is embedded (D-G-37)
check-release: build-release
    ./target/release/llmobs version | grep -qx 'ui: embedded'

# Run the binary with arguments, e.g. `just run --version`
run *args:
    cargo run -p llmobs-cli -- {{args}}

# Rust checks: format, lint, tests, supply chain
check-rust: fmt-check lint test deny check-ee

# Install JS dependencies from the lockfile
js-install:
    pnpm install --frozen-lockfile

# Web UI checks: lint, types, tests, production build
check-web: js-install
    pnpm --dir web run check

# Python SDK checks: format, lint, types, tests
check-py:
    cd sdks/python && uv sync --locked && uv run ruff format --check && uv run ruff check && uv run mypy && uv run pytest

# TypeScript SDK checks: lint, types, tests, package
check-ts: js-install
    pnpm --dir sdks/typescript run check

# Every check (D79); must pass before every commit
check: check-rust check-web check-py check-ts

# Build the web, run the Rust e2e tests with the UI embedded, then the SDK suites against a real server (D-G-20, D-G-18)
e2e: js-install
    pnpm --dir web run build
    E2E_REQUIRE_UI=1 cargo test --workspace --all-features --test e2e
    bash tools/e2e/run.sh
    ./target/debug/llmobs version | grep -qx 'ui: embedded'

# Fail if the working tree changed (generated files, snapshot or fixture writes)
assert-clean:
    git status --porcelain --untracked-files=all
    git diff --exit-code
    test -z "$(git status --porcelain --untracked-files=all)"

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

# Lint GitHub Actions workflows (actionlint) and audit them for security issues (zizmor)
check-workflows:
    actionlint
    zizmor --offline .github

# RustSec advisories only; run weekly by CI
deny-advisories:
    cargo deny check advisories

# Check the ee licence files and SPDX headers (D-P-2)
check-ee:
    #!/usr/bin/env bash
    set -euo pipefail
    expected=6448026f2e06ab3da9ed8ddda686694bba37179e88db92dea6d4aab516c957ea
    header='// SPDX-License-Identifier: Elastic-2.0'
    status=0
    for dir in crates/server/src/ee web/src/ee tools/license; do
        if [ ! -f "$dir/LICENSE" ]; then
            echo "$dir/LICENSE is missing"
            status=1
        elif [ "$(sha256sum "$dir/LICENSE" | cut -d' ' -f1)" != "$expected" ]; then
            echo "$dir/LICENSE is not the Elastic License 2.0 text"
            status=1
        fi
        while IFS= read -r -d '' file; do
            if [ "$(head -n 1 "$file")" != "$header" ]; then
                echo "$file: the first line must be '$header'"
                status=1
            fi
        done < <(find "$dir" -type f \( -name '*.rs' -o -name '*.ts' -o -name '*.tsx' \) -print0)
    done
    while IFS= read -r file; do
        case "$file" in
            crates/server/src/ee/*|web/src/ee/*|tools/license/*) ;;
            *) echo "$file: Elastic-2.0 header outside an ee directory"; status=1 ;;
        esac
    done < <(git grep -l -F 'SPDX-License-Identifier: Elastic-2.0' -- '*.rs' '*.ts' '*.tsx' || true)
    exit "$status"

# Re-sign fixtures/license/payloads with the dev key (D-P-6)
license-fixtures:
    cargo run -q -p llmobs-license-tool -- fixtures

# Web dev server with mocked API (fixtures/api)
web-dev: js-install
    pnpm --dir web run dev

# Web dev server proxied at the real API (no mocks)
web-dev-server: js-install
    pnpm --dir web run dev:server

# Regenerate crates/server/openapi.json and web/src/api/schema.gen.ts
openapi: js-install
    LLMOBS_OPENAPI_WRITE=1 cargo test -p llmobs-server --test openapi -- contract_file_is_up_to_date
    pnpm --dir web run gen:api

# Regenerate fixtures/api responses and manifest
api-examples:
    LLMOBS_API_EXAMPLES_WRITE=1 cargo test -p llmobs-server --lib api_examples::examples_are_up_to_date

# Validate OTLP fixtures
fixtures-check:
    cargo test -p llmobs-otel --test fixtures

# Rewrite OTLP fixtures in canonical JSON formatting
fixtures-fmt:
    LLMOBS_FIXTURES_FMT=1 cargo test -p llmobs-otel --test fixtures -- canonical_formatting

# Rewrite expected.json from the normalizer. Review the diff before committing.
fixtures-bless:
    LLMOBS_BLESS=1 cargo test -p llmobs-otel --test golden

# Regenerate the built-in price snapshot from a downloaded LiteLLM JSON (D104).
# Download model_prices_and_context_window.json from
# https://github.com/BerriAI/litellm/blob/main/model_prices_and_context_window.json
# by hand. This recipe does not fetch it.
prices-update litellm_json:
    LLMOBS_LITELLM_JSON={{litellm_json}} cargo test -p llmobs-otel --test prices -- refresh

# Fixture capture

# Capture OTLP fixtures from real instrumentations (tools/capture), e.g. `just capture openinference/openai-chat-tool`
capture +cases: capture-py-sync capture-ts-sync
    cargo run -q -p llmobs-capture -- run {{cases}}

# Recapture every scenario
capture-all: capture-py-sync capture-ts-sync
    cargo run -q -p llmobs-capture -- run --all

# Install the Python capture project (tools/capture/py)
capture-py-sync:
    cd tools/capture/py && uv sync --locked

# Install the TypeScript capture project (tools/capture/ts)
capture-ts-sync:
    pnpm --dir tools/capture/ts install --frozen-lockfile

# Lint and type-check the capture scenarios, then recapture every scenario and compare (timing excluded)
check-capture: capture-py-sync capture-ts-sync
    cd tools/capture/py && uv run ruff format --check && uv run ruff check && uv run mypy
    pnpm --dir tools/capture/ts run typecheck
    pnpm exec biome check tools/capture/ts
    cargo run -q -p llmobs-capture -- run --all --check

# List recipes
default:
    @just --list

# Format all Rust code
fmt:
    cargo fmt --all

# Check formatting
fmt-check:
    cargo fmt --all -- --check

# Lint (warnings are errors)
lint:
    cargo clippy --workspace --all-targets --all-features -- -D warnings

# Run all Rust tests
test:
    cargo test --workspace --all-features

# Supply-chain checks (licenses deferred, D57)
deny:
    cargo deny check advisories bans sources

# Debug build
build:
    cargo build --workspace

# Release build of the binary
build-release:
    cargo build --release -p llmobs-cli

# Run the binary with arguments, e.g. `just run --version`
run *args:
    cargo run -p llmobs-cli -- {{args}}

# Everything CI runs for Rust
check: fmt-check lint test deny

# Validate OTLP fixtures
fixtures-check:
    cargo test -p llmobs-otel --test fixtures

# Rewrite OTLP fixtures in canonical JSON formatting
fixtures-fmt:
    LLMOBS_FIXTURES_FMT=1 cargo test -p llmobs-otel --test fixtures -- canonical_formatting

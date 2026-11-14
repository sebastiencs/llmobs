#!/usr/bin/env bash
# Starts a real `llmobs serve` and runs the Python and TypeScript SDK suites against it (D-G-20).
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$root"

cargo build -p llmobs-cli

# Three ports, held open together so no two are the same. They are released before the
# server binds them: another process may take one, and the readiness wait then fails.
read -r http_port otlp_port grpc_port < <(python3 -c '
import socket

socks = [socket.socket() for _ in range(3)]
for sock in socks:
    sock.bind(("127.0.0.1", 0))
print(*(sock.getsockname()[1] for sock in socks))
for sock in socks:
    sock.close()
')

dir="$(mktemp -d)"
server_pid=""
cleanup() {
  if [[ -n "${server_pid}" ]]; then
    kill "${server_pid}" 2>/dev/null || true
    wait "${server_pid}" 2>/dev/null || true
  fi
  rm -rf "$dir"
}
trap cleanup EXIT

key="sk-llmobs-0123456789ABCDEFGHIJKLMNOPQRSTUV"
base="http://127.0.0.1:${http_port}"

LLMOBS_DATA_DIR="$dir/data" \
LLMOBS_HTTP_ADDR="127.0.0.1:${http_port}" \
LLMOBS_OTLP_HTTP_ADDR="127.0.0.1:${otlp_port}" \
LLMOBS_GRPC_ADDR="127.0.0.1:${grpc_port}" \
LLMOBS_INIT_ADMIN_EMAIL="ada@example.com" \
LLMOBS_INIT_ADMIN_PASSWORD="llmobs-e2e-1" \
LLMOBS_INIT_ORG="Acme" \
LLMOBS_INIT_PROJECT="E2E" \
LLMOBS_INIT_SECRET_KEY="$key" \
  "$root/target/debug/llmobs" serve >"$dir/serve.log" 2>&1 &
server_pid=$!

ready=0
for _ in $(seq 1 120); do
  if curl -sf "$base/readyz" >/dev/null; then
    ready=1
    break
  fi
  if ! kill -0 "$server_pid" 2>/dev/null; then
    break
  fi
  sleep 0.5
done
if [[ "$ready" -ne 1 ]]; then
  cat "$dir/serve.log"
  echo "llmobs serve did not become ready at $base" >&2
  exit 1
fi

export LLMOBS_E2E_BASE_URL="$base"
export LLMOBS_E2E_API_KEY="$key"
uv run --locked --directory sdks/python pytest -m e2e tests/test_e2e.py
pnpm --dir sdks/typescript exec vitest run src/e2e.test.ts

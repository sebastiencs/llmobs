# 12 — Operations: configuration, packaging, limits, metering, backups, upgrades

## Configuration

Sources, later overriding earlier: built-in defaults → `llmobs.toml` (`--config` or `LLMOBS_CONFIG`) → env vars (`LLMOBS_` + upper-snake path, e.g. `LLMOBS_STORAGE__OBJECT_STORE` for `storage.object_store`; common ones also have short aliases listed below). Parsed with `figment`; unknown keys are an error. `llmobs admin check` validates config, data dir, encryption key, SMTP and object store connectivity.

| Key | Env alias | Default |
|---|---|---|
| `data_dir` | `LLMOBS_DATA_DIR` | `./llmobs-data` |
| `public_url` | `LLMOBS_PUBLIC_URL` | `http://localhost:8080` |
| `http_addr` | `LLMOBS_HTTP_ADDR` | `0.0.0.0:8080` |
| `grpc_addr` | `LLMOBS_GRPC_ADDR` | `0.0.0.0:4317` |
| `otlp_http_addr` | `LLMOBS_OTLP_HTTP_ADDR` | `0.0.0.0:4318` (empty = disabled) |
| `storage.object_store` | `LLMOBS_OBJECT_STORE` | `file://<data_dir>/objects` · `s3://bucket/prefix` |
| `storage.s3.*` | standard `AWS_*` env | region, endpoint (MinIO/R2/…), path-style, credentials chain |
| `encryption_key` | `LLMOBS_ENCRYPTION_KEY` | generated into `secret.key` |
| `smtp_url`, `email_from` | `LLMOBS_SMTP_URL`, `LLMOBS_EMAIL_FROM` | unset (email off) |
| `auth.*` | see 08 | |
| `ingest.max_request_bytes` | | 10 MiB |
| `ingest.max_attribute_bytes` | | 8 MiB |
| `media.min_bytes` / `media.max_bytes` | | 4 KiB / 32 MiB |
| `storage.wal.sync` | | `"always"` (or a duration such as `"5ms"`: fsync at most that often) |
| `storage.flush.max_bytes` / `storage.flush.max_age` | | `"256MiB"` / `"120s"` |
| `storage.memtable.hard_limit` | | `"1GiB"` |
| `storage.compaction.target_file_size` / `storage.compaction.threads` | | `"512MiB"` / cores/4 |
| `storage.query.max_memory` | | `"2GiB"` |
| `storage.query.metadata_cache_bytes` | | `"512MiB"` |
| `storage.deletion.max_delay` | | `"24h"` |

The storage engine's settings form the `[storage]` section, with one sub-table per area (`wal`, `flush`, `memtable`, `compaction`, `query`, `gc`, `deletion`, `anchor`); spec 03 names them without the `storage.` prefix. Sizes are strings with a binary unit (`"256MiB"`, `"1GiB"`) and durations are strings with a unit (`"2ms"`, `"120s"`, `"24h"`) (D90). The full list, with defaults, is the `StorageConfig` doc in `llmobs-storage`.
| `eval.workers` | | 16 |
| `log.format` / `log.level` | `LLMOBS_LOG` | `pretty` on TTY else `json` / `info` |
| `telemetry.enabled` | `LLMOBS_TELEMETRY` | decided with the license (anonymous usage ping, opt-out) |

TLS: terminated by a reverse proxy in the reference setups; native TLS optional via `tls.cert` / `tls.key` (rustls) for simple installs.

## Packaging

| Artifact | Details |
|---|---|
| Binary | `llmobs` for linux-x86_64, linux-aarch64 (musl, static), macOS arm64/x86_64. Web UI embedded. Built in CI with `cargo build --release` (LTO thin, `codegen-units=1`, `panic=abort` off — we want unwinding for task isolation) (release pipeline added with the server track, D81). |
| Docker image | `ghcr.io/<org>/llmobs:<version>`, distroless/static base, non-root uid 10001, `VOLUME /data`, `EXPOSE 8080 4317 4318`, healthcheck `/healthz`. Multi-arch. |
| docker-compose | `llmobs` only (local disk) and a variant with MinIO for S3 mode. |
| Helm chart | One `StatefulSet` (replicas: 1, enforced), `PersistentVolumeClaim` for `/data`, `Service` (8080, 4317, 4318), optional `Ingress`, `Secret` for encryption key / SMTP / OIDC, values for S3. `podManagementPolicy: OrderedReady`, `terminationGracePeriodSeconds: 60`, `strategy: Recreate` semantics (never two pods on one volume). |

A file lock (`flock` on `<data_dir>/LOCK`) prevents two processes from opening the same data dir.

## Process lifecycle

- **Startup:** lock data dir → load config → open `meta.db` + migrations → verify encryption key → open storage (`catalog.db` + migrations, load catalog, replay WAL; ingestion cannot start before this ends) → start background tasks → bind ports → `/readyz` = 200.
- **Shutdown (SIGTERM/SIGINT):** stop accepting connections, drain in-flight requests (≤ 20 s), flush memtables (best effort ≤ 30 s), stop workers (leases expire and are re-queued), close SQLite.
- `/healthz`: process alive. `/readyz`: WAL writable, SQLite reachable, object store reachable (cached 10 s), free disk above threshold.

## Limits & metering (cloud-ready from day one)

Per-org `limits` JSON (04 `orgs.limits`), all optional; instance-wide defaults in config:

```json
{"ingest_spans_per_sec": 20000, "ingest_burst": 100000,
 "spans_per_month": null, "max_projects": null, "max_members": null,
 "max_retention_days": null, "judge_usd_per_month": null}
```

- Rate limits: token bucket per org in memory (GCRA); exceeding → 429 with `Retry-After`.
- Monthly quotas: checked against `usage_daily` aggregates cached in memory; exceeding `spans_per_month` → 429 with a clear message (hard limit) — soft-limit warnings logged/notified at 80 % and 100 %.
- Metering: `usage_daily` (spans, bytes, scores, judge tokens/cost) per org/project/day — the future billing source. Exposed in org settings → usage and via API.

## Self-monitoring

Deferred to just before release (D38). Placeholder decisions: Prometheus `/metrics` on a separate admin port, OTel self-traces opt-in.

## Backups (documented procedure, D37)

What to back up: `meta.db` and `catalog.db` (taken together, in the same pause), the object store (`objects/` or the bucket), `secret.key` (or the env key). The WAL does not need backing up if the snapshot is taken after a flush.

**Procedure (local disk mode):**

```bash
llmobs admin backup --to /backups/llmobs-$(date +%F)   # online, consistent
```

1. Forces a memtable flush (so WAL content is in Parquet).
2. Pauses compaction / deletion rewrites / retention (they create and obsolete files).
3. `VACUUM INTO` consistent copies of `meta.db` and `catalog.db`.
4. Copies (or hard-links, when on the same filesystem) all active data files listed in the copied `catalog.db` + media objects.
5. Resumes background tasks. Writes a manifest with checksums.

**S3 mode:** run `llmobs admin backup --meta-only --to …` (steps 1–3, 5) and rely on bucket versioning / replication for objects; deleted-file cleanup by GC respects a configurable `gc.object_retention` so a metadata backup stays restorable for that period.

**Restore:** stop the server, replace `meta.db`, `catalog.db` and objects, start. Objects the restored catalog doesn't list stay in the store; `llmobs admin gc-orphans` lists the object store and deletes them (the regular GC never lists the bucket, 03 §5).

**GDPR and backups:** deleted data persists in backups until they expire; the docs recommend a backup retention ≤ 30 days and, on restore, re-applying the `deletions` recorded after the backup date (`llmobs admin reapply-deletions --since <date>` replays them from the current `deletions` audit table).

## Upgrades

- Semantic versioning. Minor/patch upgrades: replace the binary/image, restart; SQLite migrations run automatically (forward-only); pre-migration copies `meta.db.pre-<version>` and `catalog.db.pre-<version>` are kept for the last upgrade.
- Parquet schema changes are additive only (03 §3); normalization changes bump `schema_version` without rewriting data.
- WAL format carries a version byte in each segment header; a new binary reads the previous WAL version (replays it before writing the new format).
- Downgrades are not supported (documented: restore the pre-upgrade backup).

## Security checklist (MVP)

- All secrets encrypted at rest (04 §Encryption); keys/tokens hashed.
- Outbound requests (webhooks, LLM connections, OIDC, Slack) go through an HTTP client with an SSRF guard: deny private/link-local/loopback ranges unless `security.allow_private_network_egress = true` (needed for local Ollama/vLLM — the setting is shown in the LLM connection form).
- Request size limits everywhere; JSON depth limit 128.
- Content-Security-Policy on the UI (`default-src 'self'`; media from `self` only); payloads rendered as text (no HTML injection from trace content).
- `cargo deny` (advisories, bans, sources) on every CI run and `cargo deny check advisories` weekly (RustSec, the database `cargo audit` uses); Dependabot alerts for JS and Python; dependency licenses compatible with any license we later pick (license check enabled once D9 is decided, D57).

## Documentation deliverables (part of the MVP)

Quickstart (Docker, binary), OTel recipe per language, SDK guides (Python/TS), Vercel AI SDK guide, attribute reference, filter DSL + SQL schema reference, evaluators & online rules, alerts, auth/SSO setup (GitHub, Google, Okta, Entra ID, Keycloak), SMTP setup, S3 setup, backup/restore, GDPR guide, Helm values reference, API reference (rendered OpenAPI).

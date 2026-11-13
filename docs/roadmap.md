# Roadmap

How the MVP (`mvp-decisions.md` § MVP scope) gets built: phases, the tracks inside each wave, where each track stands, and how tracks name their decisions.

**Keeping it current.** A PR that changes a track's status (plan merged, milestone merged, track done) updates that track's row in the same PR. A new track gets its row when its plan is first written.

## How the work is organised

- **Phase 0** (serial) set the contracts: workspace, `llmobs-core`, OTLP fixtures, the API contract, web and SDK scaffolds, CI (D53).
- **Waves** run tracks in parallel, each in its own worktree and branch, at most 6 sessions at a time (D53). A track owns the directories its plan names (`AGENTS.md` § Parallel work and ownership).
- **Each track gets a plan before any code**: one plan of small sub-phases, or a design doc plus one plan per milestone when the track has real unknowns (A1). Another model implements the plan one sub-phase per commit.
- **Wave 1** builds each area on its own, against fixtures and mocks.
- **Wave 2** connects the areas into a running product and adds the features that need several of them.

## Decision IDs

Decisions D1–D168 keep their numbers. From now on:

- A track's decisions are `D-<track>-<n>`, numbered from 1 within the track: `D-A1-1`, `D-F-1`, `D-G-1`. They go in the track's own section of `mvp-decisions.md` (`## Track F — SDK tracing`), created with its first decision.
- A decision that belongs to no track (repository rules, CI, cross-cutting product choices) takes the next plain number, from D169, and only the coordinating session writes one.
- Specs, plans and code comments cite the full ID.

Two tracks can never pick the same ID, so parallel plans need no reservation and no renumbering at merge time.

## Phase 0 — contracts (done)

Plans: [`plans/phase-0/`](plans/phase-0/). Decisions D52–D82; D1–D51 are the product decisions made before it. A first maintenance batch is in [`plans/maintenance/`](plans/maintenance/).

## Wave 1 — areas in isolation

| Track | Area | Plan | Status | PRs | Decisions |
|---|---|---|---|---|---|
| **A1** Storage engine | `crates/storage`: WAL, memtable, Parquet, catalog, traces, compaction, deletion (spec 03) | [`plans/a1-storage/`](plans/a1-storage/): design doc, then one plan per milestone | **In progress.** M1–M5 merged. M6 (resources and hardening) is planned (`11-m6a-resources-hardening.md`, `12-m6b-read-cache-benchmarks.md`) and being implemented; open: #40. | #8, #11, #18, #20, #22, #24, #27, #28, #30, #34, #37 | D83–D90, D119–D120, D152–D154, D168; M4 onwards `D-A1-n` |
| **A2** Query layer | `crates/storage` `filter/`, `sql/`, views: filter DSL, SQL sandbox, reads, metrics (spec 05) | [`plans/a2-query/`](plans/a2-query/) (Q1–Q20) | **Implemented.** Reviewed against the plan (`reviews/a2-query.md`: 2 blockers, both SQL-console resource limits, and 4 majors); fixes in progress. | #9, #13 | D91–D101 |
| **B1** Normalization | `crates/otel`: decoding, conventions, payloads, media, truncation, cost (spec 02) | [`plans/b1-normalization/`](plans/b1-normalization/) (N1–N23) | **Implemented.** The 51 captured fixture cases are blessed and reviewed; the review's fixes follow D-B1-1–D-B1-9. | #10, #12, #14, #16, #35 | D102–D113, D-B1-1–D-B1-9 |
| **B2** Fixture capture | `tools/capture`: mock provider, OTLP receiver, scenarios | [`plans/b2-capture/`](plans/b2-capture/) (K1–K20) | **Implemented.** | #15 | D114–D118, D121–D122 |
| **C** Web UI on mocks | `web/`: pages with an API contract, on MSW mocks (spec 11) | [`plans/c-web-ui/`](plans/c-web-ui/) (U1–U29) | **Implemented** on mocks. Reviewed (`docs/reviews/c-web-ui.md`); the review's major findings are fixed by [`plans/c-fixes/`](plans/c-fixes/) (CF1–CF11). Wiring to the real API is track G. | #17, #19 | D123–D134, D-C-1–D-C-5 |
| **D** Metadata DB and auth | `crates/server`: config, axum app, `meta.db`, identity, sessions, keys, orgs, members, invites, email, OAuth/OIDC, admin (specs 04, 08, 09) | [`plans/d-meta-auth/`](plans/d-meta-auth/) (P1–P26) | **Implemented.** | #21, #25 | D135–D151 |
| **E** Eval library | `crates/eval`: checks, mapping, templating, judge adapters, webhook, judge traces, runner (spec 06) | [`plans/e-eval/`](plans/e-eval/) (E1–E20) | **Implemented.** The database side of evaluation is track I (D156). | #23, #26 | D155–D167 |
| **F** SDK tracing | `sdks/python`, `sdks/typescript`: init, exporter, `observe`, context, manual spans, `score` (spec 10, tracing half) | [`plans/f-sdk/`](plans/f-sdk/): a shared contract, then a Python and a TypeScript track (28 sub-phases) | **Implemented.** Both SDKs pass the shared conformance fixtures. | #31, #36 | `D-F-n` |

## Wave 2 — integration (proposed)

Only G is planned so far. Each other track still gets the usual choice of one plan or a design doc first. The split puts one serial track first, G, which connects ingest, storage, the read API and the UI, and settles the wiring every other track extends: the server's app state and route table, `openapi.json`, the `fixtures/api` manifest and the web router. The feature tracks then add routes and pages in parallel.

| Track | Scope | Specs | Depends on | Plan | Status | Decisions |
|---|---|---|---|---|---|---|
| **G** Spine | OTLP HTTP and gRPC ingest (`sk`/`pk` keys, ingest-only scope, 413 limits, normalizer → storage append, media blobs, usage counters); read handlers for traces, spans, sessions, users, scores, metrics, schema and media; score writes and the public feedback endpoint; SSE; `llmobs serve` serving the embedded UI; the existing pages moved from mocks to the API; one end-to-end test from SDK to UI API | 02, 03, 05, 09, 11, 12 | A1 M1–M3, A2, B1, C, D | [`plans/g-spine/`](plans/g-spine/): design doc, then one plan per milestone (M1 written) | **In progress.** M1 implemented. | `D-G-n` |
| **H** Console and agents | SQL console endpoint, saved SQL queries, saved views on the server, MCP server, CLI query commands | 05, 09 | G | — | Not planned | — |
| **I** Online evaluation | LLM connections, secret encryption, evaluators, online rules, the `eval_jobs` queue in `meta.db` with sampling and cost caps, judge traces through the internal ingest path, "run evaluator on this trace", the annotation form, and their UI pages | 04, 06, 11 | G, E | — | Not planned | — |
| **J** Datasets and experiments | Versioned datasets, CSV/JSONL import, add-from-trace, the experiments API, SDK `evaluate()` (spec 10, second half), the comparison UI | 06, 09, 10, 11 | G, F | — | Not planned | — |
| **K** Alerts | Rule evaluation over A2's metrics; Slack, webhook and email delivery (D's mailer, E's egress client); the alerts UI | 07, 11 | G | — | Not planned | — |
| **L** Admin UI | Login (restyling G's minimal page, D-G-19), signup and setup; orgs, projects, members, API keys, personal access tokens, invites and instance-admin pages over D's API | 08, 09, 11 | D (can start before G) | — | Not planned | — |
| **M** Deletion and retention | Delete by user, by trace or filter, by project and by org, cascading to `meta.db`; per-project retention | 03, 04, 09, 12 | A1 M5, G | — | Not planned | — |
| **N** Packaging and ops | Docker image, docker-compose, Helm chart, backup docs, per-org limits, quotas and the usage API over G's `usage_daily` counters (D10, D-G-11), release CI | 12 | G | — | Not planned | — |

**Order.** G and L start first; L needs only D's API, which is merged. A1 M4–M6 and F continue alongside. After G: H, I, J and K, three at a time. M starts when A1 M5 lands. N comes last, though a Docker image can come earlier.

## After the MVP

Out of the MVP by decision, to revisit once Wave 2 is done:

- annotation queues, custom dashboards, playground, issue detection, prompt registry, gateway, SAML, 2FA/passkeys, message dedup (D19, D20);
- sandboxed code evaluators (D11);
- proxy/gateway ingestion (D4).

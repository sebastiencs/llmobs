# CI compiler cache

The Rust lint, test, and e2e jobs use kache 2.0.0 with the Garage bucket `kache`
served at `https://s3.chapu.is`. The S3 endpoint and bucket are configured in
`actions/rust/action.yml`; the region is `us-east-1`.

## Credentials

On the Garage host, create an access key scoped to the `kache` bucket with read
and write permission:

```sh
garage key new --name llmobs-ci
garage bucket allow --read --write kache --key llmobs-ci
```

Store the key's **Access Key ID** and **Secret Access Key** in the GitHub
environment `s3-cache` using these secret names:

- `S3_CACHE_ACCESS_KEY_ID`
- `S3_CACHE_SECRET_ACCESS_KEY`

Add them under the GitHub repository's **Settings → Environments → s3-cache →
Environment secrets**. The Rust compilation jobs select this environment to
receive its secrets. Alternatively, from an authenticated GitHub CLI, run these
commands and enter each value at its hidden prompt:

```sh
gh secret set S3_CACHE_ACCESS_KEY_ID --repo sebastiencs/llmobs --env s3-cache
gh secret set S3_CACHE_SECRET_ACCESS_KEY --repo sebastiencs/llmobs --env s3-cache
```

## Trusted runs

| Run | Cache access |
| --- | --- |
| Push to protected `main` | Read and save under `kache-v2/` |
| PR authored by `sebastiencs`, from this repository, triggered and re-run by `sebastiencs` | Read main/PR caches and save under `kache-pr-v2/` |
| Other PRs, forks, Dependabot, or manual runs | None |

`CI_CACHE_ALLOWED` checks GitHub's event metadata before the Rust jobs receive
the S3 credentials. The composite action skips kache when caching is disabled
or credentials are missing. Whenever caching is enabled, kache saves its
results. Jobs with `cache: "false"` (rust-deny) do not install kache or access
S3.

Kache 2.0 enforces a separate CI write policy: only protected-branch pushes may
publish to the main prefix. `save-cache: true` alone does not enable PR uploads.
`KACHE_PULL_REQUEST_PREFIX=kache-pr-v2` enables its supported PR write path:
owner PRs read `kache-v2/` first, then `kache-pr-v2/`, and write only to
`kache-pr-v2/`. Protected `main` pushes ignore the PR prefix. Manual runs are
read-only in kache and therefore do not enable caching here.

Only the writer credential pair is used. This policy assumes that repository
writers and the code executed in trusted runs are trusted; repository writers
can modify workflows to access repository secrets. Keep the `pull_request`
trigger, which withholds secrets from fork PRs; do not switch to
`pull_request_target` to expose the cache.

The bucket stays private; neither public access nor CORS configuration is
needed. GitHub's workflow token keeps `contents: read`, and kache writes its
report to the job summary without posting PR comments.

## First runs

After the secrets are configured, a qualifying owner PR or a run on `main` can
populate the cache. The first run against the empty bucket compiles everything;
run it again and compare the complete `rust-lint`, `rust-test`, and `e2e` job
durations, including kache setup and its post step.

The kache setup step should report the configured S3 remote. Its job summary
shows cache hits and misses. Each caching job has its own prefetch manifest
(`manifest-key: github.job`) within the appropriate main or PR prefix. Selective
warming is enabled; the entire remote is not downloaded before each build.

The remote bucket is not bounded by kache's local cache limit. Garage has no
built-in object lifecycle rules; if the bucket grows too large, delete old
objects under `kache-v2/` and `kache-pr-v2/` with a script and a temporary
broad key. Deleted entries are rebuilt on demand.

`s3.chapu.is` is served through Cloudflare's proxy, which caps the size of a
single request body (100 MB unless the zone is on Enterprise). Kache uploads
one object per compilation; if its job summary ever shows upload failures for
large cached executables, grey-cloud the `s3` DNS record or set
`cache-executables: false` in `actions/rust/action.yml`.

References:
[kache action](https://github.com/kunobi-ninja/kache-action/tree/ad7317540dcdd71c904f7a60a87b620a0c0e67ed),
[kache CI write policy](https://ninja.kunobi.com/docs/kache/remote-cache/ci#who-may-write).

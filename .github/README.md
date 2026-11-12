# CI compiler cache

The Rust lint and test jobs use kache 2.0.0 with the private Cloudflare R2
bucket `llmobs-cache`. The S3 endpoint and bucket are configured in
`actions/rust/action.yml`; the region is `auto`.

## Credentials

In Cloudflare, open **R2 Object Storage → Manage R2 API Tokens** and create
one token with **Object Read & Write** permission, scoped only to `llmobs-cache`.
Store its credentials in these GitHub repository secrets:

- `R2_ACCESS_KEY_ID`
- `R2_SECRET_ACCESS_KEY`

Save the generated **Access Key ID** and **Secret Access Key**, not the
Cloudflare API token value. Add them under the GitHub repository's
**Settings → Secrets and variables → Actions → Repository secrets**.
Alternatively, from an authenticated GitHub CLI, run these commands and enter
each value at its hidden prompt:

```sh
gh secret set R2_ACCESS_KEY_ID --repo sebastiencs/llmobs
gh secret set R2_SECRET_ACCESS_KEY --repo sebastiencs/llmobs
```

## Trusted runs

| Run | Cache access |
| --- | --- |
| Push or manual run on `main` | Read and save |
| PR authored by `sebastiencs`, from this repository, triggered and re-run by `sebastiencs` | Read and save |
| Other PRs, forks, Dependabot, or manual runs on other branches | None |

`CI_CACHE_ALLOWED` checks GitHub's event metadata before the Rust jobs receive
R2 credentials. The composite action skips kache when caching is disabled or
credentials are missing. Whenever caching is enabled, kache saves its results.
Jobs with `cache: "false"` do not install kache or access R2.

Only the writer credential pair is used. A read-only R2 token is no longer
needed. This policy assumes that repository writers and the code executed in
trusted runs are trusted; repository writers can modify workflows to access
repository secrets. Keep the `pull_request` trigger, which withholds secrets
from fork PRs; do not switch to `pull_request_target` to expose the cache.

The bucket stays private; neither public access nor CORS configuration is
needed. GitHub's workflow token keeps `contents: read`, and kache writes its
report to the job summary without posting PR comments.

## First runs

After the secrets are configured, a qualifying owner PR or a run on `main`
can populate the cache. Run it again and compare the complete
`rust-lint` and `rust-test` job durations, including kache setup and its post
step.

The kache setup step should report the configured S3 remote. Its job summary
shows cache hits and misses. Lint and test jobs have separate prefetch manifests
but share compiled artifacts under `kache-v2/`. Selective warming is enabled;
the entire remote is not downloaded before each build.

The remote bucket is not bounded by kache's local cache limit. If needed, add
an R2 lifecycle rule to expire objects under `kache-v2/` after 30 days. Expired
entries are rebuilt on demand.

References: [R2 credentials](https://developers.cloudflare.com/r2/api/tokens/),
[kache action](https://github.com/kunobi-ninja/kache-action/tree/ad7317540dcdd71c904f7a60a87b620a0c0e67ed).

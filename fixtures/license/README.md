# Licence fixtures

`dev-1.key` is the **development** ed25519 private key (32-byte seed, standard base64). It is
public on purpose: only debug builds trust its public half (`DEV_KEYS` in
`crates/server/src/ee/token.rs`, D-P-6). Release builds trust only `prod-1`, whose private
key is never in this repository.

`payloads/*.json` are token payloads; `tokens/*.token` are those payloads signed with
`dev-1` by `just license-fixtures` (P3). Do not edit `tokens/` by hand.

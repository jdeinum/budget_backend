[![Check](https://github.com/jdeinum/budget_backend/actions/workflows/check.yaml/badge.svg)](https://github.com/jdeinum/budget_backend/actions/workflows/check.yaml)
[![Test](https://github.com/jdeinum/budget_backend/actions/workflows/test.yaml/badge.svg)](https://github.com/jdeinum/budget_backend/actions/workflows/test.yaml)
[![Audit](https://github.com/jdeinum/budget_backend/actions/workflows/audit.yaml/badge.svg)](https://github.com/jdeinum/budget_backend/actions/workflows/audit.yaml)

# Budget Backend

A personal finance tracker's backend — accounts, transactions, and tags,
sourced from a live Plaid sync or hand-imported bank/card statements (Neo,
Amex). Built with Axum, sqlx, and Tokio, backed by embedded SQLite.

## Getting Started

### Prerequisites

- [Rust](https://rustup.rs/) (version pinned in `rust-toolchain.toml`)
- A Plaid developer account (sandbox is enough) — only needed for the
  `/plaid/*` routes; everything else (accounts, manual transactions,
  statement imports, tags, rules) works without it
- [Doppler](https://doppler.com) (optional — `justfile`'s `dev`/`prod`
  recipes read secrets from it; copy `.env.example` to `.env` instead if you
  don't use it)

### Setup

```sh
cp .env.example .env   # fill in BUDGET__PLAID__CLIENT_ID / BUDGET__PLAID__SECRET
cargo run               # or: just dev   (via Doppler)
```

Migrations run automatically on startup against `data/budget.db` (created if
missing). The server listens on `config/default.toml`'s `server.host`/
`server.port` (`0.0.0.0:3000` by default) — override with `BUDGET__SERVER__*`
env vars, same double-underscore convention as the Plaid credentials.

## Testing

```sh
cargo nextest run
```

Runs the unit tests plus `tests/api`'s integration suite, which spins up the
real app (`budget::build`, same code path `main.rs` uses) against a
throwaway SQLite file and a mocked Plaid API, and drives it over real HTTP.

Two kinds of tests are opted out of that default run, since each trades
speed for something the fast suite doesn't need on every `cargo test`:

- **Property tests** (`quickcheck`) — check pure-function invariants (cursor
  encode/decode round-trips, tag-layer merge ordering, description
  normalization) against ~100 generated inputs each, instead of a handful of
  hand-picked examples. Skipped by default; opt in with:
  ```sh
  PROP_TEST=1 cargo nextest run
  ```
- **Snapshot tests** (`insta`) — the Amex/Neo statement parsers' "parses the
  real export" tests assert against a committed `.snap` file rather than
  hand-picked fields, so a parser change that touches _any_ field shows up as
  a diff. If a change is intentional, review and accept it:
  ```sh
  cargo install cargo-insta
  cargo insta review
  INSTA_UPDATE=always cargo test
  ```

## Roadmap

- ⬜ Cloudflare Workers port - swap the embedded SQLite file (`sqlx` +
  `sqlite://data/budget.db`) for [Cloudflare D1](https://developers.cloudflare.com/d1/) and run the API as a worker.
- ⬜ OAuth - no authentication exists yet; the API isn't publicly reachable
  today, only called by the frontend over a private Docker network
- ⬜ WASM plugin model - client driven code that only accesses the data you want
  it to, and sandboxed to prevent accessing your system in unexpected ways

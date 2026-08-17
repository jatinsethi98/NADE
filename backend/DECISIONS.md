# Backend decisions

Judgement calls made while building, and why. Anything here that contradicts a
brief is called out as such. Newest at the bottom.

---

## D1 — No compile-time sqlx query macros. Permanent.

`sqlx::query!`, `query_as!`, `query_scalar!` and `query_file*!` are banned in
this crate, forever. Every statement uses the runtime API (`sqlx::query`,
`sqlx::query_as` with `#[derive(FromRow)]`), so `cargo build` never needs a live
database, a `DATABASE_URL`, or a checked-in `.sqlx` cache.

The `macros` feature *is* enabled, because `sqlx::migrate!` lives behind it.
`migrate!` reads `.sql` files off disk at compile time and talks to no database,
so it does not weaken the rule — but a feature flag is a weak fence, so the rule
is enforced by a test instead: `db::tests::no_compile_time_query_macros` greps
`src/` and fails on any banned macro.

Cost: no compile-time column/type checking. Mitigation: every query is covered
by a test that runs it against a real, freshly migrated PostgreSQL.

## D2 — Two `sqlx` versions in the tree, on purpose.

`postgresql_embedded 0.21` depends on `sqlx 0.9`; the brief pins us to `sqlx
0.8`. Both compile. Nothing crosses the boundary — `postgresql_embedded` only
uses its copy internally to run `create database` — so the only cost is build
time. Kept at 0.8 as briefed. Revisit when `postgresql_embedded` and our pin
agree.

## D3 — Dead-letter marker is `jobs.dead_at`.

PLAN.md's `jobs` table has no terminal marker and the brief left the choice
open. A job that has failed `NADE_JOB_MAX_ATTEMPTS` (5) times gets
`dead_at = now()`, and both the claim predicate and the partial index
`jobs_ready_idx` exclude `dead_at is not null`, so it is claimable-never.
`done_at` stays null, because the job never succeeded and a `done_at` would lie
to every later "how many jobs completed" query. The same transaction writes an
`audit_log` row (`actor='system'`, `action='job_dead_letter'`).

## D4 — The live pairing code is mirrored to a 0600 file.

The brief says "keep it in memory; regenerate on restart" *and* "a `just pair`
path that reprints/regenerates the code". Those cannot both hold literally:
`just pair` is a **different process**, and it cannot read the server's heap.

Resolution: the code lives in the server's memory *and* in
`backend/secrets/pair-code.json` (mode 0600, in an already-gitignored
directory). Every pairing attempt re-reads the file, so a code minted by
`just pair` is honoured by the running server immediately, and restarting the
server still mints a fresh one.

Why this is not a weakening:

* the code is already printed to the console in plaintext — the file is not a
  new disclosure, it is the same disclosure with `0600` on it;
* it is a 6-digit secret with a 10-minute TTL and a 10-per-minute rate limit;
* it buys a real safety property: single-use becomes **atomic across
  processes**, because exactly one `unlink()` of a path can succeed. Two racing
  pair requests can never both mint a device.

`SIGHUP` also regenerates the code in the running server (unix only), so
`kill -HUP` is the no-file path if you want one.

## D5 — A fifth error code: `rate_limited` (HTTP 429).

The brief lists the four codes P1 needs (`unauthorized`, `not_found`,
`bad_request`, `internal`) and separately requires a pairing rate limit. Folding
"too many attempts" into `unauthorized` would make the limit invisible to the
client and untestable. `docs/contract/` already carries codes outside the four
(`token_consumed`, `approval_expired`), so the envelope is open by design. The
shape is unchanged: `{"error":{"code":"rate_limited","message":"…"}}`.

## D6 — Objection to nothing in `docs/contract/`, but two observations.

No fixture was changed. Two things worth recording:

1. **`pair.json`'s token is 20 bytes, the brief says 32.**
   `"nade_3f9c1a7e5b2d4c8f9a1e6b3d7c5f2a8e4d6b9c1f"` is `nade_` + 40 hex
   characters = 20 random bytes; the brief says "32 random bytes, rendered as
   the `nade_` + hex form the fixture shows". We ship the fixture's **form**
   (`nade_` + lowercase hex) with the brief's **entropy** (32 bytes → 64 hex
   characters), because a token's value can never be byte-matched by a test
   anyway — it is a per-device secret — while its entropy is a real security
   property. `api::auth::tests::pair_returns_a_contract_shaped_token` asserts
   the key set against the fixture and the form of the value.

2. **`fts` uses the `simple` configuration, which does not segment CJK.**
   PLAN.md specifies `to_tsvector('simple', …)` and `websearch_to_tsquery
   ('simple', …)`, and `simple` tokenises on whitespace with no CJK word
   segmenter. A Japanese sentence is therefore one token: searching `荷物`
   finds nothing, searching `配送のお知らせ` finds it. Correct per the plan, and
   a real limitation P2's search inherits. Fixing it later means a different
   configuration or `pg_bigm`/`pgroonga`, both schema changes.

## D7 — `/v1/healthz` reports `status: "degraded"` when the database is down.

The brief pins the healthy body exactly (`{"status":"ok","db":"ok","version":…}`)
and says the unhealthy one is a 503 with `db:"down"`, without pinning `status`.
`"degraded"` says what is true: the process is up and serving, its dependency is
not. There is no `healthz` fixture, and nothing on the iOS side decodes it.

## D8 — Schema additions beyond PLAN.md §Postgres schema.

Small, and each earns its place:

| Addition | Why |
|---|---|
| `devices.created_at`, `devices.revoked_at` | PLAN.md §Auth bootstrap requires tokens to be "revocable"; `revoked_at` revokes without destroying the audit trail, and `error_unauthorized.json` already says "unknown, or revoked". |
| `jobs.created_at` | Queue age is the first thing you want when a queue misbehaves. |
| `jobs.dead_at` | D3. |
| Nullable `devices.account_id`, `audit_log.account_id` | A device pairs at P1, before any Gmail account exists (P2); a dead-lettered job belongs to no mailbox. |
| `not null` + defaults on columns PLAN.md writes bare | The plan's shorthand implies them (`default now()`, `default '{}'`); making them explicit is expansion, not invention. |
| FK `on delete cascade` / `set null` | The plan writes `references accounts` without an action; cascade from the account root, `set null` for the optional `run_id` back-references so deleting a run does not delete the note it wrote. |

## D9 — The embedded PostgreSQL is shared, pinned, and deliberately outlives us.

One cluster per machine: `backend/.pgdata`, binaries in `backend/.pgcache`,
fixed port `54329`, fixed password `nade-dev`, `fsync=off`,
`max_connections=200`. Both already gitignored.

* **Shared with the dev server.** `just run` and `cargo test` use the same
  postmaster; tests each create their own `nade_test_<uuid>` database, and the
  next test binary sweeps the leftovers. That is what makes a test run cost
  ~30 ms of database setup instead of a ~20 s boot.
* **Left running on exit.** `postgresql_embedded`'s `Drop` stops the server, so
  we park the handle in a `static` that is never dropped. `just db-stop` stops
  it. The alternative — a fresh random-port cluster per run — orphans a
  postmaster on every crash; this way there is at most one, always the same one.
* **`fsync=off`** because this cluster holds nothing but dev scratch and
  per-test databases.

## D10 — `NADE_ENV` defaults to production, and only the exact string `dev` is dev.

Unset, blank, `development`, `Dev ` with a space — all production. The dev
shortcuts (`NADE_TOKEN`, the embedded database) must be something you opt into,
never something you fall into. `config::Config::usable_dev_token()` is the only
place `NADE_ENV` is consulted for auth, and
`api::auth::tests::dev_token_is_impossible_outside_dev` asserts the negative.

## D11 — `tower` is a dev-dependency only.

The brief suggested it; nothing in the server needs it directly (the middleware
we use comes from `axum` and `tower-http`). It stays in `[dev-dependencies]`
for `ServiceExt::oneshot` in tests rather than sitting unused in the binary's
dependency list.

## D12 — Handler panics are contained by `tokio::spawn`, not `catch_unwind`.

Each job handler runs in its own task, so a panic surfaces as
`JoinError::is_panic()` and the worker loop never unwinds. `AssertUnwindSafe` +
`catch_unwind` would have worked too, but it lies about unwind safety;
spawn-and-join gives real isolation and, as a bonus, an `AbortHandle` the
heartbeat task uses to stop a handler the instant its lease is stolen.

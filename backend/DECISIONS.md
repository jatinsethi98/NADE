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

---

# P2 — "Mail lands"

## D13 — Thread rollups are two tables, not an aggregate query.

PLAN.md §Postgres schema does not list `threads` or `thread_labels`; the P2
brief does say "maintain the per-thread rollups the list endpoint needs", and
this is what that means.

`GET /mailboxes/{id}/threads` is *filtered by label* and *ordered by time*, with
a keyset cursor. Computed on the fly from `messages`, that is a `distinct on
(thread_id) … order by internal_ts desc` over the whole mailbox followed by a
sort — never an index scan, and never a total keyset order. So the rollup is
maintained at ingest:

| Table | Holds |
|---|---|
| `threads` | one row per thread: `last_ts`, newest message's subject/snippet/sender, `unread` (true if **any** message is), `msg_count`. |
| `thread_labels` | the union of the thread's messages' labels, with `last_ts`/`unread` denormalised. |

`thread_labels_keyset_idx (account_id, label_id, last_ts desc, thread_id desc)`
is then exactly the walk the endpoint performs, which
`api::mail::tests::thread_list_query_uses_an_index` proves with `EXPLAIN` over
20 000 threads — index scan, no `Seq Scan`, and no `Sort` node.

Cost: two more tables and a recompute per touched thread per sync.
`sync::store::refresh_thread` is the single place "newest message wins" and "any
unread message makes the thread unread" are decided.

## D14 — `attachments.att_id` is ours, not Gmail's.

The brief calls the column "Gmail's opaque id". It cannot be, and the reason is
structural: **`messages.get?format=raw` does not carry `attachmentId` anywhere.**
`format=raw` is one base64 blob of the whole RFC-822 message, and PLAN.md fixes
`format=raw` as what the sync fetches, because it is the only format our MIME
parser can see.

So the parser assigns `att_id = base64url(sha256("<gmail_id>\0<index>\0<name>")[..16])`
— 22 characters, opaque, URL-safe, and a pure function of bytes that never
change, so a re-sync upserts the same row. `API.md`'s stated properties
(opaque, long, may contain `-` and `_`) all hold.

The proxy resolves it on demand: one `messages.get?format=full`, match the part
by `(filename, body.size)` or by `Content-ID`, then either serve `body.data`
directly (small parts) or call `attachments.get` (large ones). Two round trips
on a download, none on a sync — and downloads are rare while syncs are not.

Rejected alternative: a second `format=full` fetch per message-with-attachments
*during* the sync, to capture Gmail's real ids. It costs 5 quota units per such
message on every sync forever, and it would still need correlation logic,
because the `full` part tree and our parsed part list are produced by different
parsers.

## D15 — The `/auth/gmail/*` routes carry no bearer, but are not open.

`API.md` §0 exempts only `/healthz`, `/auth/pair` and `/webhooks/gmail` from the
bearer guard. The two Gmail OAuth routes have to join that list, because neither
can carry a bearer token: `start` is a URL that opens in a browser, and
`callback` is where Google's redirect lands.

The first cut of this stopped there, and that was a hole. PKCE and `state` prove
a callback belongs to a flow *someone* started; they do not prove that someone
was allowed to configure this server. On a reachable, not-yet-connected server,
any caller could complete consent with their own Google account, become the
singleton mailbox, and leave the real owner meeting the 409.

So authorisation to **initiate** moved to a route that can be authenticated, and
the browser itself is bound:

* `POST /v1/auth/gmail/link` sits **behind** the bearer guard and mints a
  single-use, ten-minute capability, returning the full `start` URL carrying it.
  Pairing is the trust root — the pairing code is printed on the server's own
  console — so a device with a bearer token has already proved console access,
  and there is no bootstrapping problem.
* `GET /auth/gmail/start` verifies and consumes that capability before anything
  else, re-checks that the minting device is still paired, and only then
  redirects. Without one it renders a refusal page: `403`, never a redirect,
  never a `Set-Cookie`, and byte-identical whether or not a mailbox is already
  connected — the refusal must not double as a "is this server up for grabs?"
  probe. Because the capability is checked first, an unauthorised caller also
  cannot fingerprint whether `secrets/web_client.json` exists, and cannot put a
  single entry into the pending-consent map.
* `start` sets `nade_gmail_link` — 32 random bytes, `HttpOnly`, `SameSite=Lax`,
  `Path` scoped to the OAuth routes, `Max-Age=600`, and `Secure` when the
  registered redirect is https — and stores its value beside the PKCE verifier.
  `callback` requires it to match, in constant time, **before** the token
  exchange. Consent must finish in the browser that started it, so a `state`
  lifted out of a redirect is worth nothing on its own. `SameSite=Lax` is a
  decision, not a default: Google's redirect back is a cross-site top-level GET,
  which `Lax` permits and `Strict` would drop.
* Revoking a device is checked at **both** ends — `start` and `callback` — so
  revoking a stolen token also closes a consent that is already half way through
  Google, rather than only refusing new ones.
* Everything the earlier note claimed still holds: single-use `state` with a
  10-minute TTL and its PKCE verifier, a pending map capped at 64, and a
  completed consent for a **different** mailbox refused with a 409 page.

Neither browser-facing URL needs a bearer header. The `API.md` §0 exception
table was reported to the contract lane rather than patched here.


## D16 — Gmail tokens are AES-256-GCM at rest.

`0001_init.sql` already said `access_token` holds "AES-GCM ciphertext … (P2)".
This is that, plus the same treatment for `refresh_token`, which is the one that
matters — it mints access tokens until the user revokes it.

The key is `NADE_TOKEN_KEY` (64 hex characters) or, unset, a 0600 file at
`backend/secrets/token-key` minted on first use — the same pattern as the
pairing-code mirror (D4), in the same already-gitignored directory. Losing the
key means the account re-consents; it does not mean data loss.

## D17 — `GET /me` answers before an account exists.

`API.md` §1 does not say what `/me` returns before the first Gmail connection.
The options were `404`, a `409 needs_reauth`, or an answer. We answer:
`{"email": "", "status": "needs_reauth"}`.

`needs_reauth` already means "Gmail sync is paused and the client shows the
re-sign-in row in Settings", which is exactly the right screen for "not
connected yet". Keeping `/me` total also means the iOS lane never special-cases
a missing body. Reported as an `API.md` gap.

## D18 — `thread.mailbox_name` prefers a user label.

`API.md` §2 says it is "the label the thread is filed under, for the footer" and
leaves the choice open when a thread carries several — which is always: every
inbox thread is in `INBOX` **and** a `CATEGORY_*`. Showing those would make the
footer read "Inbox" for every thread in the app.

So: the first **user** label in mailbox order wins; failing that, the first
whitelisted system label; failing that, `""`. `docs/contract/thread.json` says
`"To Reply"` for a thread that is also in `INBOX` and `CATEGORY_PERSONAL`, which
is the same rule.

## D19 — `reqwest` takes `rustls-no-provider`, and we install `ring`.

`reqwest 0.13`'s default rustls build is `aws-lc-rs`, which needs `cmake` — not
present on this machine — and `sqlx` already links `ring`. Two crypto providers
in one process is a trap: whichever calls `CryptoProvider::get_default()` first
wins, and the failure is a runtime panic rather than a build error.

So the crate takes `rustls-no-provider` and `gmail::http_client()` installs
`ring` exactly once, behind a `Once`. Building a client any other way panics at
construction, which is why `gmail::tests::no_bare_reqwest_clients` greps for
`reqwest::Client::new()` and fails the build — the same enforcement style as D1.

## D20 — The dev caps are `NADE_`-prefixed.

PLAN.md writes the cap as `MAX_SYNC_MESSAGES`. Every other variable this crate
reads is `NADE_`-prefixed, and `config::tests::env_example_documents_every_var`
keeps the list and `backend/.env.example` in lockstep either way. The variable
is `NADE_MAX_SYNC_MESSAGES`; the cap's *value* is PLAN.md's 2 000, clamped in
`Config::from_env` rather than trusted, and asserted by
`sync::tests::the_dev_caps_are_applied` alongside `newer_than:30d`,
`MAX_BATCH` per batch and one batch per second.

Amended at P3: this said "45 per batch". `MAX_BATCH` is 10, and
`Config::from_env` refuses to start above it (the batch width is a concurrency
figure, not a quota one). `backend/.env.example` documented 45 as well, so
copying the documented file made the server refuse to boot; that is fixed and
`config::tests::every_documented_value_is_one_the_server_would_boot_with` now
parses the example's values rather than only its names.

## D21 — A placeholder `nade-gmail-sim/src/lib.rs`.

Another lane added `crates/nade-gmail-sim` to `backend/Cargo.toml`'s `members`
before writing its sources. Cargo refuses to load a workspace whose member has
no targets, so **every** command in the workspace failed, including
`cargo test -p nade-server`. A two-line placeholder `src/lib.rs` restores the
workspace for both lanes and is expected to be overwritten by the real crate.
Removing the member instead would have broken theirs.

## D22 — Two error codes added to `error.rs` beyond P1's five.

`upstream_unavailable` (502) and `payload_too_large` (413), plus `needs_reauth`
(409), all of which `API.md` §0's table already defines and
`docs/contract/error_*.json` already ships. A Gmail failure after retries is a
**502**, never a 500: the fault is upstream and the client's correct response
(retry later) is different. `error::tests::every_served_code_matches_its_contract_fixture`
byte-checks all eight codes we serve against their fixtures, message included —
the message is the user's only explanation, so it is contract, not detail.

## D23 — Amending `0001_init.sql` invalidates an existing dev database.

The P2 brief says to amend the migration in place, because nothing has shipped.
`sqlx`'s migrator records a checksum per version and refuses a modified one, so
an already-migrated **dev** database fails to boot with
`migration 1 was previously applied but has been modified`.

Test databases are created fresh per test and never see this. The dev one is
fixed by `just db-reset`, added for the purpose. Recorded here because the
symptom is a startup failure with no obvious cause, and it will recur every time
P3 touches the migration before the first real deployment.

## D24 — A search cursor carries a fingerprint of its query.

`API.md` §0 already said it: "the query is fingerprinted into the cursor and a
mismatched pair is `400 bad_request`". The code did not do it. `TokenPayload`
held Gmail's `pageToken` alone, so `?q=<anything>&cursor=<token minted for
something else>` was forwarded to Gmail unchecked.

Gmail does not reliably reject that pair. It can answer with a page of the
*other* search — the user scrolls one query and is served another, with a 200.

The fingerprint is a SHA-256 of the **canonical** query (`NormalisedQuery::
as_str`), truncated to 12 bytes, not the raw `q`: `from:A  OR  from:B` and
`from:a or from:b` are one search, and a cursor has to survive the user
retyping it. Hashed rather than embedded because a cursor travels in URLs and
logs, and the query is the user's mail.

Validation therefore has to happen *before* the cursor is decoded, which
reorders `search()`. A pre-fingerprint cursor fails to deserialise and is
refused — correct, since nothing in it says which query it belonged to.

## D25 — `attachments.ordinal`, because a name and a size do not identify a part.

The sync fetches `format=raw`, which carries no `attachmentId`, so a stored
attachment is matched back to a live Gmail part at download time (D14). That
match was `filename == name && size == size`, first hit wins.

A message with two files of the same name and the same length — the ordinary
`invoice.pdf` twice — served the **first** one's bytes for both. Deterministic,
silent, 200.

`ordinal` is the position among *our* attachments in part-tree order. The
resolver now prefers a `Content-ID` (unique within a message, RFC 2392), and
otherwise collects every part matching name and size and picks by rank among
the stored look-alikes. Rank, not absolute tree position: the two sides are
built by different parsers and their trees do not align part-for-part, but they
agree on order, which is all a rank needs.

Fewer candidates than the rank asks for is a **404**. Falling back to the first
would be the original bug wearing a different name.

## D26 — `threads.complete`, and a thread completes itself when opened.

`docs/SEARCH.md` traded a local index for Gmail's, which makes the cache
deliberately incomplete. Search hydrates the messages that *matched* a query —
for an old conversation, one message out of ten — and `GET /threads/{id}` then
read local rows only. Nine absences, no marker, `msg_count` wrong: a thread
that looked whole and was not. An agent under a token budget cannot tell.

So the thread endpoint asks `users.threads.get?format=minimal` (10 quota units)
for the true message list, fetches what is missing, verifies by **count** that
the rows landed, and only then records `complete = true` and sets `msg_count`
to Gmail's own. Verified rather than attempted, because a sub-request can fail
silently inside a `200` batch.

Cost is one call per thread, once, ever; after that it is a local read. This is
what Superhuman and Spark do.

When completion fails — throttled, offline, dead credential — the cached rows
are still served, with `partial: true`, and `complete` stays false so the next
open tries again. The rows are worth showing; presenting them as the whole
conversation is not.

EDGE (staleness): a completed thread that receives a new message is still
complete once the message is ingested. Until P3's webhook lands, that ingest
waits for the next sync — the same window the mail list already has.

## D27 — `cargo test` at the workspace root failed while `cargo test -p …` passed.

Five `nade-gmail-sim` HTTP tests panicked with "No rustls crypto provider is
configured" — but only in a whole-workspace run.

Cargo unifies features per dependency across the workspace. `nade-server` builds
`reqwest` with `rustls-no-provider` and installs `ring` itself at startup
(`gmail/mod.rs`). The simulator asks for `reqwest` with no TLS feature at all —
it speaks plain HTTP to a localhost socket — but the unified build gives it
`rustls-no-provider` anyway, and `reqwest::Client::new()` then panics because
nothing in *that* process installed a provider.

`cargo test -p nade-gmail-sim` does not unify, so it passed. `CLAUDE.md`
documents the per-crate command, which is exactly why this hid: the failing
command was the one nobody ran.

Fixed by installing the `ring` provider once in the simulator's own tests — the
library and the `determinism` integration binary separately, since an
integration test is its own process. `ring` rather than aws-lc-rs, to match the
server: two providers in one process is a trap.

The general lesson: a per-crate test command can pass on a feature set the real
build never uses. The workspace command is the one that tells the truth.

## D28 — The singleton account is an advisory lock, not a unique index.

`existing_account_email` and `save_consent` used to run in **separate**
transactions. Two callbacks for two different addresses could both observe "no
account" and both insert; `accounts.email` being unique does not help, because
the whole premise of the race is that the addresses differ. `state.account()`
then picked an arbitrary earliest row.

`save_consent` now takes `pg_advisory_xact_lock(ACCOUNT_SINGLETON_LOCK)` as the
**first** statement of its existing transaction and re-checks inside it,
returning `TokenError::AlreadyBound { existing }` rather than inserting. The
handler maps that to the same 409 page as the cheap pre-check, which stays only
to buy the friendly path. The key is `i64::from_be_bytes(*b"nadeacct")`, so it is
recognisable in `pg_locks`; what matters is only that every writer of `accounts`
takes the same one. `pg_advisory_xact_lock` is released by COMMIT, ROLLBACK, and
the backend dying, so a consent that fails half way cannot wedge every later
sign-in.

**A `unique index on accounts ((true))` was considered and rejected.** Six tests
in `db.rs` insert several accounts on purpose, to prove that every query is
scoped per account. A hard singleton index would forbid exactly the tests that
prove isolation, and the lock closes the race without that cost.

Two smaller things fell out of the same transaction:

* `accounts.email` is unique but PostgreSQL text is case-sensitive, so the old
  `on conflict (email) do update` would have produced a **second** row for one
  mailbox spelled two ways — both spellings pass the `eq_ignore_ascii_case`
  guard. When a row exists it is now updated by id; the insert arm only runs
  under the lock, where there provably is none.
* `order by created_at` is not a total order. Every "the account" query —
  `AppState::account`, `oauth::existing_account_email`,
  `api::auth::singleton_account` — now orders by `created_at, id`, so two calls
  inside one request cannot disagree.

## D29 — The control-character scrub moved to the writer, after a third instance.

`storable()` in `sync/store.rs` now sits on every text column `upsert_message`
binds. The three instances, in order:

1. `body_text` — a `NUL` in a body killed the first live sync. Fixed beside the
   extractor, which normalises text.
2. `body_html` — a *sibling* derived from the same source that never passes
   through the extractor. Killed the sync again, four attempts each.
3. `snippet` — found by review, not by an outage. `IngestRow::metadata_only`
   runs Gmail's own snippet through `html::decode_entities`, and `&#0;` decodes
   to a literal `NUL` via `char::from_u32`. `parse::snippet` only collapses
   whitespace, and `NUL` is not `White_Space`. So a message whose MIME failed to
   parse — the path that exists so a parse failure is survivable — could take
   the whole insert down.

Three fixes at the value, three escapes. The rule belongs at the **writer**: one
function writes a `messages` row, so putting it there makes the guarantee true
for every column, including the ones P4 adds, without anyone remembering.

The general shape, stated once: **a guarantee attached to a value protects that
value. Attach it to the chokepoint the values pass through.**

## D30 — Search hydration reports what it could not fetch.

`cache::fetch_missing` matched on `BatchOutcome::Failed { .. }`, logged, and
returned `Ok(())`. The client has already decided which failures are worth
retrying (`gmail::types::is_transient`) — the sync consumes that field and fails
the job rather than advancing its cursor, which is what stopped the first live
sync losing 77 messages to a transient 429. The search path threw the same
answer away.

The visible symptom: a throttle mid-search drops the message,
`rows_in_gmail_order` filters the thread out, and the user gets a short page
indistinguishable from "there were fewer matches".

`fetch_missing` now returns the ids it could not resolve, and search records an
audit row. **This is not the whole fix.** `ThreadsResponse` has no `partial`
field, so the response still cannot say so — `ThreadDetail` gained one (D26) and
the threads list should too. Recorded rather than half-done quietly.

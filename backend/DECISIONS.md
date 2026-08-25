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

# P3 — "Mail stays current"

## D31 — The history cursor is `max(record.id)`, never `historyId`.

`HistoryList.historyId` is the mailbox's **current** id, and Gmail repeats it on
every page of a walk. Store it after page one and the next walk asks
`history.list` from a point past everything it had not read; the log filter is
`record.id > start`, so the answer is a `200` **with no `history` key** — byte
for byte what a quiet mailbox returns.

Nothing errors. Nothing retries. The messages are simply never ingested, and the
only thing that can notice is a full re-sync.

`crates/nade-gmail-sim/tests/sync_story.rs::advancing_to_the_top_level_history_id_after_page_one_loses_records`
was written before the consumer existed and is the reproduction.
`HistoryList::max_record_id` is the only way to a cursor, and the warning lives
in its doc comment — next to the trap rather than in a design document.

## D32 — The push body's `historyId` is never persisted.

The webhook logs and audits it and writes it nowhere. The walk is driven
**only** by our own stored cursor.

That one rule is what makes Pub/Sub's at-least-once, out-of-order delivery
irrelevant: a notification for 100 arriving before one for 99 triggers exactly
the same walk, and a redelivery produces an empty page. Writing it would be a
second cursor with different semantics, updated by an untrusted body.

Rejected alternative: storing Pub/Sub `messageId`s to deduplicate deliveries. It
buys nothing — the work is cursor-driven and idempotent at every layer — and it
would be a second table to garbage-collect and a second thing to get wrong.

## D33 — `jobs.dedupe_key`'s index must not mention `locked_by`.

The obvious predicate is `... and locked_by is null`, so a notification arriving
mid-walk can still enqueue. It is unsafe: `Queue::fail`, `Queue::release` and
`Queue::reap_expired_leases` **all** set `locked_by = null`, so a replacement row
inserted during the run collides the instant the original fails, is released at
shutdown, or has its lease reaped. The queue would be unable to record its own
failures during exactly the outage that caused them.

So the index is over pending rows regardless of lock, and the race it would have
solved is closed by `sync_state.rerun_requested` instead (D34).
`db::tests::a_running_job_can_still_fail_release_and_be_reaped_with_a_pending_twin`
fails against the unsafe predicate.

## D34 — The lost wakeup, and why re-reading the cursor cannot fix it.

A notification that lands after the walk's last (empty) page and before the job
completes is suppressed at enqueue by D33's index. The walk cannot see it by
re-reading `last_history_id`, because D32 means the webhook never wrote it.

"Did our own cursor move?" is not a signal either: it moved because *we* moved
it, so it is true after every productive walk and would spend an extra
`history.list` confirming silence for ever.

So the suppressed enqueue records itself on `sync_state.rerun_requested`, and
the walk takes **and clears** the flag in one statement before deciding whether
to finish. Read-then-clear as two statements would drop a notification landing
between them, which is the bug the mechanism exists to fix. Bounded at
`MAX_DRAIN_PASSES`, after which the flag is put back and the next job takes it.

## D35 — The reconciliation debt is durable, not inferred.

The 404 recovery re-syncs the window and then sweeps. The obvious structure —
"both in one job, so a crash retries both" — does not work: `sync_window`
durably commits a **fresh** cursor before returning, so the retry's
`history.list` succeeds, never produces the `404` that enters recovery, and the
sweep is skipped **permanently**. Deleted and moved mail stays live with nothing
able to notice.

`sync_state.reconcile_after` is written **when the `404` is seen** — before the
recovery re-sync begins — and cleared only by a completed sweep, so the
reconciliation survives independently of whether the 404 ever recurs. The
guarantee is the ordering, not a shared transaction: persisting first means
every crash point between the 404 and the sweep still owes the work.

## D36 — The sweep's floor, and the two ways it would delete live mail.

"Everything in the window the fresh listing did not mention" is wrong twice over
without guards:

1. `messages` legitimately holds mail from **outside** the window. Every search
   hit is cached (`mail::cache::fetch_missing`) and opening an old thread pulls
   in the whole conversation; a `newer_than:30d` listing can never name any of
   it. Without `internal_ts >= floor` the sweep deletes everything the user has
   ever searched for.
2. `list_message_ids` truncates at `MAX_SYNC_MESSAGES`, so the window may not
   have been enumerated at all. `messages.list` is newest-first, so a truncated
   listing enumerates exactly the window's *suffix* — and the floor becomes the
   oldest message actually seen, not the window's edge. Not the later of the
   two: untruncated, we want `now - window`, because the band between it and the
   oldest listed message is precisely where deleted mail lives.

And the sweep does not run at all unless the re-sync accounted for every
message, because a listing that gave up half way through looks exactly like
"these messages were deleted".

## D37 — `jsonwebtoken`'s `rust_crypto` backend, and why not `aws_lc_rs`.

`jsonwebtoken` 11 offers `rust_crypto` and `aws_lc_rs` and **no `ring` option at
all**, so CLAUDE.md's "APNs via reqwest + jsonwebtoken" did not settle the
question. `aws_lc_rs` needs cmake, which is not on this machine, and D19 already
forbids a second crypto provider in one process.

`rust_crypto` it is. Its `rsa` and `sha2` are already in `Cargo.lock` (via
`sqlx-mysql` and our own hashing), it installs no rustls provider, and
`grep -c aws-lc Cargo.lock` is still `0` after the add — which is the check to
re-run if this dependency is ever bumped.

## D38 — `AccountLock` has a `Drop`, because `release` is an `async fn`.

The guard released the lock only on the path that awaited `release`. The job
worker deliberately does not guarantee that path: it aborts a handler whose
lease it has lost, and again when the shutdown grace expires.

Dropping a `PoolConnection` **returns it to the pool**, where it is recycled
rather than closed — and a session-scoped advisory lock belongs to the session.
The lock would survive on a connection now serving unrelated requests, and every
later sync of that account would stand down as `skipped_concurrent` for ever,
with no lock anyone could find.

`Drop` now detaches the connection so PostgreSQL frees its session locks. Losing
one pooled connection on an aborted sync is a cost worth paying; a permanently
wedged account is not. The bug predates P3 and applied to `run_sync` too.

## D39 — A malformed history record is audited, not blocking.

A history entry with no message id is counted and audited rather than skipped in
silence — but it deliberately does **not** hold the cursor.

The id is the only handle on the message, so refusing to advance would re-read
the same malformed record for ever: a wedged account that has *also* lost the
mail, which is strictly worse than an audited skip. The 30-day sync and the
reconciliation sweep are the backstops that can still find it.

Contrast a **transient** fetch failure, which does hold the cursor (asking again
is likely to work), and a **permanent** one, which does not (asking again
produces the same 400 for ever, so one dead message would otherwise pin
incremental sync for the life of the account).

## D40 — Truncation is the listing's answer, not a count comparison.

The reconciliation sweep narrows its floor when the 30-day listing was cut
short. That was inferred from `listed == MAX_SYNC_MESSAGES`, which is wrong in
both directions: a mailbox holding exactly the cap with no next page is
*complete*, and treating it as truncated leaves genuinely deleted rows behind
while clearing the debt anyway.

`list_message_ids` now returns `(ids, truncated)`, with `truncated` derived from
a `nextPageToken` surviving the loop.

## D41 — The sweep's boundary is `>` when truncated, `>=` otherwise.

Gmail pages by `internalDate`, so a capped listing can stop **inside** a group
of messages sharing one timestamp: it returns some of them and stops. With
`internal_ts >= floor`, the ones it did not reach match the predicate, are
absent from the listed ids, and are soft-deleted while still live in Gmail.

`>` keeps the whole boundary cohort. The cost is that a message genuinely
deleted at exactly that timestamp waits for the next reconciliation; the
alternative cost is deleting live mail, silently.

## D42 — A busy account lock is not a completed notification.

`run_incremental` returns `Ok` with `skipped_concurrent` when another worker
holds the lock, and `jobs.rs` marks any `Ok` handler done — so a push racing a
full sync would be acknowledged and its work discarded, with the change waiting
on the 30-minute poll.

The handler re-enqueues itself once, ten seconds out, with a `requeued` flag so
a second contention stops rather than looping.

## D43 — The JWKS cache separates *lookup* from *freshness*.

Two bugs came from conflating them, and both were outages:

- an unknown `kid` refetched only when the set was **stale**, so Google rotating
  a key inside our TTL rejected every push until the TTL expired — up to the
  24-hour clamp;
- a failed refresh logged "serving a stale JWKS" and then rejected the token
  anyway, because the next lookup insisted on freshness. The documented
  stale-key fallback was unreachable.

Freshness now decides only *when to refetch*. A cached key verifies whether or
not the set is stale, and an unknown `kid` may provoke a refetch at any time,
gated solely by the rate limit that stops forged `kid`s becoming an amplifier
pointed at Google. The limit is a field rather than a constant so a test can
prove both halves without sleeping through a minute.

## D44 — The outbox writes the intention before it moves the card.

The queue write was `try?` and the card was resolved regardless. A disk or
migration failure therefore produced the worst possible outcome: the UI said the
approval had happened, nothing was ever sent, and the token — the capability
needed to retry — was cleared from the only copy the client had.

Durability first, optimism second. A failed enqueue leaves the card actionable
and surfaces the error.

# Scale-out review — many accounts, one server

## D45 — Account resolution is the device's binding, with a sole-account fallback.

The schema was fully account-scoped from 0001; the *resolution layer* was not.
Every handler asked `state.account()` — "the earliest `accounts` row" — so the
server could hold any number of mailboxes and would still show everyone the
first one. `devices.account_id` existed from P1 and was never written.

Resolution is now: **the authenticated device's bound account; else the sole
account iff exactly one exists; else nothing.** Consent writes the binding —
`save_consent` binds the initiating `device_id` in the same transaction that
creates the account — and `AlreadyBound` narrowed from "this *server* already
has a mailbox" to "this *device* is already bound to a different mailbox". A
second user with their own unbound device and their own Gmail now just
succeeds. The dev token is a synthetic unbound device and resolves the same
way.

The full table, with what each row preserves:

| Device | Accounts | Resolves to | Why |
|---|---|---|---|
| bound to X | any | X | the binding is the answer; other accounts are other people's |
| unbound | 0 | none | byte-identical to the old no-account path (D17's `/me` answer included) |
| unbound | 1 | the one | byte-identical to the old single-user behaviour |
| unbound | ≥2 | none | there is no honest guess; the device must link its own Gmail |
| bound, account deleted mid-request | any | none | falling back would silently adopt somebody else's mailbox |

No new error codes: every "no account" row above lands on a path the contract
already defines (`/me` answers `needs_reauth`, lists are empty, thread/
attachment are `not_found`), and the 409 consent page is the same page with the
guard narrowed. A new code would have forced an iOS change for states the
client already renders correctly.

`AppState::account()` is renamed `sole_account()` and *means* it — `Some` iff
exactly one row — because the old name was exactly how "the current user" and
"the only user" stayed conflated for three phases. Its remaining callers are
the resolver's fallback and the job handlers' payload fallback (a job row with
no `account_id`).

The `nadeacct` advisory lock (D28) survives, still global, still necessary:
it closes the same-email insert race (`accounts.email` is unique but
case-sensitive, and the compare is not) and now also the same-device bind race
— two callbacks racing one device both see "unbound" outside it.

## D46 — `settings` re-keyed per account, row born with the account.

The singleton `settings` table was "one server, one mailbox" written in DDL:
with D45 it had no owner, and two users would have shared one
`approval_required_default`. Migration 0003 re-keys it
(`account_id uuid primary key references accounts on delete cascade`),
backfills existing accounts with the singleton's value — a migration must not
flip a mailbox back to requiring approvals it had turned off — and
`save_consent` inserts the row (`on conflict do nothing`) in the transaction
that creates the account, so P4's `GET /settings` never invents a default at
read time.

## D47 — Quota buckets and refresh gates are per account.

Gmail's 250 units/second and its concurrency count are **per user**; ours were
per process. One shared bucket made every account queue behind every other's
sync — enforcing a limit Gmail never imposed, and turning "add a user" into
"halve everyone's sync rate". Same shape for the refresh gate: one global
mutex serialised account B's refresh behind account A's slow token endpoint,
protecting nothing, because each account rotates its own refresh token.

Both are now `Mutex<HashMap<Uuid, Arc<…>>>`, get-or-create, never evicted (the
maps are bounded by the number of accounts). Bucket internals are unchanged —
`MAX_CONCURRENT_REQUESTS = 1` was live-tested *as a per-user figure*, and per
account is the scope that makes that measurement true. The pre-account probe
(`probe_client`, the callback's "who consented?" call) shares one bucket keyed
by the nil uuid, which `gen_random_uuid()` can never mint.

## D48 — The `messages.label_ids` GIN is dropped.

`messages_label_ids_idx` was insurance for a query that never arrived: no
statement in the crate uses a GIN-servable operator on the column — label
reads go through `thread_labels`, which has its own keyset index (D13). The
GIN was not free where it did sit: every `upsert_message` paid its maintenance
on the sync's hottest write path. Dropped in migration 0003;
`db::tests::required_indexes_exist` now asserts the absence, so it cannot
quietly return.

## D49 — Consent bumps `gmail_tokens.generation`; a refresh spends only what it read.

Found by the cross-model review of D45's diff. `save_consent` serialises
against other *consents* (the `nadeacct` lock) and `refresh` against other
*refreshes* (the per-account gate) — nothing serialised the two against each
other, and both end in unconditional writes. A refresh reads the stored
refresh token, waits on Google, and a consent commits fresh credentials
meanwhile; the stale refresh then either overwrites them with its old-lineage
result, or — having met `invalid_grant` on the token it spent — flips the
account back to `needs_reauth` seconds after the user fixed it. Not
theoretical: Testing-mode consents expire every 7 days, so re-consent racing a
background refresh is the *weekly* recovery path.

Migration 0004 adds `generation bigint` (an explicit integer, not a token
fingerprint — a fingerprint equals itself again after re-consent with an
unrotated token, and "same bytes" is not "same lineage"). Consent bumps it in
the statement that writes the tokens; the refresh's write-back carries
`and generation = $read` with `rows_affected` checked, and its `needs_reauth`
marking re-checks the generation under the `nadeacct` lock — the lock consent
holds for its whole transaction, so "still current" cannot turn false between
check and commit. A stale refresh discards its result, serves the consent's
cached token, or re-reads and retries once; a second staleness inside one
refresh returns an error rather than looping. Both interleavings are pinned by
barrier-controlled tests (a gated wiremock token endpoint parks Google's
answer until the consent has committed).

Two smaller findings landed in the same pass:

* **Revocation is authoritative at the consent commit.** The two route checks
  (`start`, pre-exchange in `callback`) close most of the window; the bind
  inside `save_consent` now requires `revoked_at is null` with exactly one
  affected row, and zero rows aborts the transaction whole — account, settings,
  tokens, audit and first-sync job. Same refusal page as the pre-exchange
  check, so the revocation's timing is not an oracle. `TokenError::
  DeviceRevoked` is a browser-page class, not a new wire code.
* **A deviceless consent keeps the old server-wide guard.** The `NADE_TOKEN`
  principal resolves through the sole-account fallback, so letting it mint a
  second mailbox would strand it: `sole_account` rightly refuses two rows, and
  the new account would be reachable by nobody. Deviceless consent may
  re-consent an existing mailbox or create the first — never a second.

## D50 — Money is an integer, in nano-USD, end to end.

The spend ceiling's whole job is to be right at its boundary: a ledger standing
at exactly $1.00 blocks, one at $0.999999999 does not. Dollars as `f64` cannot
promise that — it is the same arithmetic as `0.1 + 0.2 != 0.3` — and the failure
is silent, intermittent, and shows up only as "the ceiling let one more run
through sometimes", which is indistinguishable from a race.

So `llm::cost` does everything in `i64` **nano-USD**, `llm_calls.cost_usd` is
`numeric(14,9)` (never `double precision`), and the value crosses the wire as
**text** cast by the statement, because `sqlx` is built here without a decimal
type and binding an `f64` would undo the whole thing in one line.

It is exact for a second reason worth knowing: every published Anthropic price
is a whole number of dollars per million tokens, so a per-token rate is a whole
number of nano-USD. $1.00/MTok is exactly 1 000 nano per token, and the cache
multipliers (1.25x, 0.1x) stay integral too.

`NADE_LLM_DAILY_USD` is parsed by `config::nano_usd`, which reads the digits as
integers and never constructs a float at all — `1.0` cannot become
999 999 999.99998 on its way to becoming a ceiling.

## D51 — The column is `trailing_clause`, because `TRAILING` is reserved.

`API.md` §5 calls the field `trailing`. PostgreSQL calls it a **reserved word**
(it is part of `trim(trailing ...)`), and `add column trailing text` is a syntax
error — which is how it was found, when migration 0005 failed to apply.

Quoting it everywhere would work, and would be forgotten exactly once, in a
query no test happened to cover. The column is `trailing_clause` and the
serialiser renames it back for the wire, where the contract's name is the one
that matters.

## D52 — The keyset cursor was lossy, and dropped rows.

`cursor::encode` used `SecondsFormat::Secs`. That was harmless for the P2/P3
endpoints, whose ordering keys come from Gmail at second precision, and wrong
the moment P4 paginated rows stamped by `now()`: a cursor is one half of a
keyset comparison — `(ts, id) < (cursor.ts, cursor.id)` — so truncating it
**skips every row inside the cursor's own second**. `GET /runs` served an empty
page two with 51 rows in the table.

Now `SecondsFormat::AutoSi`, which emits no fractional part when there is none,
so a whole-second timestamp still encodes byte-identically and the contract
fixtures are unaffected.

The test that asserted the truncation was itself wrong, and its reasoning is
worth recording because it was plausible: *"the iOS side decodes with a fixed
formatter, so a fractional second would fail to parse there."* A cursor is
opaque (`API.md` §0, "clients must not parse it"); the app stores it as a
string and never base64-decodes one; the `ts` inside it is not a wire timestamp.
The rule the test should have encoded is **a cursor must round-trip its
ordering key exactly**.

## D53 — The spend ceiling cannot be reported as an `Err` from `Llm::chat`.

`Llm::chat`'s contract says an `Err` means *"the host should retry this job
later"*. A breach reported that way is retried by the queue — five times, with
backoff — and every retry that reaches a model call spends again. PLAN.md names
that path and forbids it.

So the breach travels out of band: `ledger::SpendGuard` holds an `AtomicBool`
the adapter sets and the **job handler** reads, and the handler ends the run
with `Engine::cancel` instead of returning `Err`. The load-bearing detail is
that `Engine::new` takes its `Llm` **by value**, so the handler must clone the
guard *before* the adapter is moved in; there is no way to reach back for it
afterwards.

There are two checks, not one. `POST /agents/{id}/run` pre-flights at the HTTP
layer and answers `429`, so no run row is created — doing it in the job instead
would strand a `queued` run with an empty journal, which D57 explains cannot be
ended by the engine at all. The adapter then re-checks **before** each call;
after would be too late to prevent the spend it exists to prevent.

**The ceiling is exact at its boundary and loose under concurrency, on purpose.**
A call is priced only after the provider answers, so `NADE_WORKERS` runs can
each pass the check before any of them records. The bound is
`ceiling + NADE_WORKERS x (one model turn)` — about $0.25 per extra worker at
Haiku prices against a $1.00 ceiling, with the default of two workers.
Reserving the maximum cost before the call and reconciling after would bind it
exactly, and was rejected: a crash between the two halves leaves a permanently
over-charged row, and this table is also the honest record of what the user
spent. A ledger that lies is worse than a ceiling that overshoots by cents.

## D54 — An unpriced model is charged the most expensive rate, never zero.

`cost::rates_for` falls back to the top of the table for a model it does not
recognise. The two alternatives are both worse. Priced at zero, the ceiling
stops binding and the account can spend without limit — the failure mode is
unbounded and invisible. Treated as an error, a model rename by the provider
takes the whole agent runtime down. Over-charging stops runs early, which is
recoverable, visible, and cheap.

## D55 — A tool returns a projection, never a wire type.

`search::PAGE_SIZE` is 50 and `EngineConfig::max_tool_result_bytes` is 16 KiB.
Fifty `ThreadSummary`s, or one `ThreadDetail` carrying real message bodies,
exceed that on **ordinary** mail — so handing back the HTTP shape would deliver
the model a truncation envelope instead of results, every time, and the
"oversized result" path would be the normal path rather than an edge case.

`search_mail` returns ten hits of `{thread_id, subject, from, date, snippet}`;
`read_thread` returns at most eight messages with bodies trimmed to 1 500 bytes
and fenced. Both state `truncated` explicitly — PLAN.md's "no silent caps"
applies to what a model is shown as much as to what a human is.

They still call `search::search` and `api::mail::thread_detail`, so the
validation, hydration, ordering and cache-completion cannot diverge between the
screen and the agent. `read_thread` needed `thread_detail` extracted out of the
handler first; that is one implementation with two callers, not a copy.

## D56 — `ilike` needs its wildcards escaped, and concatenation does not do it.

`GET /notes?q=` is a deliberate substring scan (`API.md` §3). Building the
pattern in SQL — `title ilike '%' || $2 || '%'` — does **not** neutralise `%`
and `_` inside `$2`: a search for `%` became `ilike '%%%'` and matched every
note in the mailbox. `notes::escape_like` escapes `\`, `%` and `_`, in that
order, with an explicit `escape '\'` clause.

## D57 — A run with no journal is settled by a row update, never by the engine.

`Engine::cancel` refuses an empty journal outright — *"cannot cancel a run that
was never started"* — and `agent_runs.status` defaults to `queued`, so a run has
no journal for the whole window between being created and being claimed. That
is the ordinary state, and therefore the state `DELETE /agents/{id}` most often
meets.

Writing the row straight to `failed` with `journal: []` is not an escape either:
it breaks `API.md` §6.1 ("the wire status follows from the last entry") and
`validate.py`'s "a run always has at least a `run_started` entry". So
`cancel_runs_of` asks the journal first and picks its path: a *started* run is
ended through the engine, so its log records its own ending rather than being
yanked away by the FK cascade; a queued one is settled by an update.

## D58 — `CorruptJournal` is not cancellable, so it is not routed to cancel.

The first draft of the run handler's error table was wrong on three of its four
rows, and the SDK says so directly in `Engine::cancel`'s own doc comment: *"A
run refused with `Error::CorruptJournal` or `Error::UnsupportedJournalFormat`
cannot be cancelled either, because the engine cannot know which sequence to
append at — that is a storage problem, and the host owns it."* `cancel` re-runs
the very replay that raised them.

The routing now reads:

| error | handler |
|---|---|
| `Llm`, `Journal`, `SeqConflict` | return `Err` — transport, so job backoff and eventually a dead letter |
| `ToolChanged` | `Engine::cancel` — raised at *dispatch*, after replay succeeded, so cancel works |
| `CorruptJournal`, `UnsupportedJournalFormat` | try cancel (one `CorruptJournal` **is** raised at dispatch and is cancellable), and on the same failure settle the row and audit it |
| `AmbiguousEffect` | **no row.** It is never an `Err`: `grep` finds zero construction sites, and the engine returns it as `Ok(RunOutcome::Failed)`. |

## D59 — The model client refuses to aim at the real API from a test.

`backend/justfile` sets `dotenv-load := true` and `backend/.env` holds a live
`ANTHROPIC_API_KEY`. From the moment `Config` read it, any test that built an
adapter without overriding `NADE_LLM_API_BASE` would send real requests and bill
real money — on whichever machine happens to have a key, and nowhere else, which
is the worst possible way to find out.

`anthropic::guard_against_live_calls_in_tests` panics under `cfg(test)` when the
base URL is the real one, with `NADE_LIVE=1` as the deliberate escape for a live
smoke. Structural, like `gmail::tests::no_bare_reqwest_clients`, rather than a
convention someone has to remember.

## D60 — The compile path is the one that spends without a ceiling. It has one now.

Three routes reach a model: `POST /agents`, `PATCH /agents/{id}` and the
`run_agent` job. Only the job was covered. `SpendGuard` lives inside
`Adapter::chat`, and `compile` deliberately goes around the adapter — it needs a
forced `tool_choice`, which the SDK's provider-neutral `ChatRequest` cannot
express — so it inherited nothing, while still *writing* `llm_calls` rows that
counted against the ceiling for everybody else.

An authenticated device could loop `POST /agents` at ~$0.012 a call, forever,
answered `200` every time (a compile failure is not an HTTP failure, so there
was never an error to back off from). Hundreds of dollars an hour against a
$1/day cap.

The check is now inside `compile::compile`, above `client.send`, because that is
the one place both handlers pass through and a third caller would otherwise
inherit nothing again. It returns `CompileError::CeilingReached`, which the
handlers map to `429` rather than storing: a ceiling breach is a fact about
today, not about the sentence, and recording it as a `compile_error` would
permanently mark a sentence that will compile perfectly well tomorrow.

## D61 — A billed call that we failed to parse is still a billed call.

`compile` recorded its ledger row *after* `extract_call`, so every early return
between the provider's `200` and a decoded answer spent money the ledger never
saw. A body that will not decode, or a model that answers without calling the
forced tool, both take that path — and the second is reachable by a crafted
`nl_definition`.

Both are now recorded before parsing, from `WireResponse::usage()`, which exists
for exactly this: price the response, then decode it. `Adapter` already closed
the same hole on the run path; this brings the compiler alongside it. The two
`let _ = ledger::record(...)` calls became a helper that logs at `error!`, on the
same reasoning — a silent gap in the ledger is how a ceiling stops binding.

## D62 — The fence carries a per-run nonce, and determinism was never the obstacle.

The first version used fixed delimiters and argued a nonce would break the SDK's
idempotency contract. That was wrong twice.

Wrong about the threat: `backend/testdata/injection/README.md` rates a fixed
label **High** — "a fence whose delimiter is a constant is guessable the moment
this repo is readable. The fence needs a per-run nonce." And a constant
delimiter cannot be neutralised by string replacement: the old code rewrote an
attacker's closer to `UNTRUSTED-EMAIL-CONTENT >>>`, one space from the real one
and the same token sequence to a model, while the test counted exact matches and
stayed green. Lower case passed through untouched.

Wrong about determinism: the SDK requires a tool's output to be identical across
**attempts at one step**, and `run_id` is fixed for the life of a run —
`CallContext` documents `replay` as "the only field that varies between
attempts". A nonce derived from `run_id` is perfectly deterministic.

`fence::nonce_for` takes 16 hex from the run id; `neutralise` strips the nonce
outright and de-fangs marker-shaped text case-insensitively. The tests now
assert the property rather than a count: **nothing the content contributed may
read as a boundary**, checked over all 85 corpus cases.

## D63 — A tool result is budgeted by measurement, not by estimate.

`read_thread` capped each body at 1 500 bytes and showed eight of them, which
fits 16 KiB — for a body with nothing JSON-escapable in it. Every `\n` and `"`
costs two bytes once serialised and `strip_control_characters` deliberately
keeps newlines, so ordinary hard-wrapped mail with quoted replies measured
18 304 bytes. Over the cap the SDK replaces the **whole** result with a
truncation envelope, so the model would have received a fragment of JSON instead
of the thread — on normal mail, which is exactly the failure D55 says the
projection exists to prevent.

The projection now drops messages from the oldest end until
`serde_json::to_vec(...)` fits. `subject`, `from`, `from_name` and `to` are
capped too: `mail::parse::MAX_SUBJECT` allows 4 000 **characters**, ~16 KB in
astral codepoints, enough to blow the cap single-handed and destroy every other
field beside it. The guarding test now uses quoted, hard-wrapped bodies, five
recipients and a 4 000-codepoint subject — and fails by 2 KB without the fix.

## D64 — Never mix a byte index from one string with a length from another.

`compile::verbatim_span` did `nl_definition.to_lowercase().find(span)` and then
sliced `nl_definition` with the result plus `span.len()`. `str::to_lowercase` is
full Unicode case mapping and is **not** byte-length preserving: `İ` (2 bytes)
lowercases to 3, `ẞ` (3) to 2. Measured against the shipped code:

* `İx` + span `X` → byte index 4 into a 3-byte string: **panic**
* `İÉ save this` + span `é save` → start 3 is not a char boundary: **panic**
* `ẞIG news…` + span `ßig news` → silently returns `ẞIG new`, one character
  short, which the builder then underlines wrongly with no error anywhere

The panic reaches `POST /agents` from a 4 000-character user string, becomes a
500 through `CatchPanicLayer`, and **loses the sentence** — the one outcome
`API.md` §5 says must never happen. `find_case_insensitive` now walks characters
and returns offsets into the haystack. The same class of bug is why
`fence::replace_ignoring_case` is hand-written rather than `to_lowercase` plus
`str::replace`.

## D65 — `Retry-After` is part of what `rate_limited` means.

`API.md` §0 defines the code as "Too many attempts; `Retry-After` header set".
`ApiError` had no way to carry one, so the spend-ceiling 429 told the app to
wait with no idea that it meant a day — while `APIClient.swift` already exposes
`APIFailure.retryAfter` and had nothing to read. `ApiError` gained
`retry_after_secs`, `IntoResponse` emits the header, and the ceiling refusal
carries the seconds to the next UTC midnight, which is when the ledger's day
actually rolls over.

## D66 — A schedule is validated against §5.2, not written through as `jsonb`.

`PATCH /agents/{id}` bound `Option<Value>` straight into the column. Three
defects in one:

* the app breaks — `WireSchedule` declares seven non-optional fields, and a
  decode failure on one agent fails the **whole** `GET /agents` body, so every
  agent disappears rather than the broken one;
* `runs_done` became client-writable, though this struct's own doc comment said
  it was "ignored if sent". §5.2 calls it server-maintained and read-only, and
  `ends.after` is counted against it;
* the trigger/schedule invariant broke — `validate.py` and the contract tests
  both enforce that a schedule trigger and a schedule imply each other, and
  `compile.rs` refuses to compile a schedule trigger for exactly that reason,
  which `PATCH` then walked around.

`validate_schedule` implements §5.2 in full (freq, interval, byweekday only for
week, bymonthday 1..28 or -1 only for month, `HH:MM`, an IANA tz `chrono-tz` can
parse, the three `ends` shapes) and carries `runs_done` over from the stored row.
P4's compiler never emits a schedule trigger, so in practice every schedule is
refused today — through the rule that will still be right when P7 derives
`next_run_at` from one.

## D67 — `trigger_summary` is a string, so it needed a test that compares strings.

The server rendered `"Not set up"` and `"Every week at 08:00"`; the fixtures —
which are what the iOS lane renders and what the signed-off screenshots show —
say `"Not set"` and `"Every weekday at 08:00"`. Both lanes were green the whole
time, because every agent assertion in `contract_tests.rs` used `shape_of`, which
compares key sets and JSON types and never looks at a value.

Fixed, and pinned: `trigger_summary_matches_every_agent_fixture` drives the real
renderer with each full fixture's own `spec` and `schedule` and compares the
result to that fixture's `trigger_summary`, then checks the list rows agree with
the full objects. `shape_of` is the right tool for a shape; a rendered string
needs `assert_eq`.

## D68 — Two more findings whose fixes are worth knowing, and one that was not a bug.

**A permanent provider error is not a transport error.** `Llm::chat` can only
answer `Err`, and the host reads every `Err` as "retry later" — so a bad API key
or a retired model id burned five job attempts and an hour of backoff to reach
the same answer, leaving the run non-terminal throughout. The adapter now raises
an out-of-band flag for `400/401/403/404/413`, the same shape as the spend
breach, and the handler cancels instead of retrying.

**A ceiling breach could strand a run.** The branch read
`self.cancel(...).or_else(|_| Ok(()))` under a comment that said "settle the row
instead" — and settled nothing. The job was marked done, `agent_runs.status`
stayed `running` forever, and the user had already been told the run stopped. It
now falls through to `settle_row_as_failed`.

**The UTC day boundary was reported as a live bug and is not.**
`date_trunc('day', now() at time zone 'utc')` really is a naked `timestamp`
whose comparison against a `timestamptz` happens in the session timezone, and
this cluster's `postgresql.conf` really does say `America/New_York` — so the
reading was sound. But `sqlx` negotiates `TimeZone=UTC` on every connection, so
the two spellings coincide and no spend was ever miscounted. The explicit second
`at time zone 'utc'` went in anyway — correctness that does not depend on a
driver default — and the test now asserts the thing that actually protects us:
that the pool's timezone is UTC. A test written for the reported bug would have
passed against the unfixed code, which is how the difference was found.

## D69 — A cleanup pass over P4, and the two defects it turned up.

Four independent reviews of the P4 diff — reuse, simplification, efficiency,
depth — with no brief to look for bugs. Most of what came back was what was
asked for. Two things were not.

**A display name is as attacker-controlled as a body, and it was not being
de-fanged.** The scrub-then-cap chain was typed out at nine call sites in two
tools, and three of them — `from`, `from_name` and every entry in `to` — had
`fence::strip_control_characters` and `cap` but no `fence::neutralise`. Those
fields land in the model's JSON *outside* any fence. A per-run nonce makes the
delimiter unforgeable, so this was defence in depth rather than an open door,
but the corpus test only ever looked at the body and could not have found it.
The chain is now one function, `fence::field`, and
`every_sender_controlled_field_is_defanged_and_not_only_the_body` asserts the
property over every scalar `read_thread` emits — so a field added later that
forgets the call fails rather than passes quietly. Reverting one field proves
the test: it fails with a forged closing delimiter, carrying the run's real
nonce, sitting in a display name.

**An unparseable response was priced at zero.** `Adapter::chat` recorded a
`from_wire` failure as `Tokens::default()`, because `from_wire` consumes the
response and the counters went with it. A body that will not decode was still
billed, so the ceiling under-counted by exactly the calls that went wrong — the
failure mode `WireResponse::usage` was written to prevent, and which the spec
compiler was already using it for. `chat` now reads the counters before parsing.

Everything else was ordinary cleanup, of which four are worth naming:

* **The retry schedule was reused, not rewritten.** `gmail::quota::backoff`
  already does exponential-with-jitter, already prefers a longer server
  `Retry-After`, and already caps both. The adapter's own version derived its
  "jitter" from `attempt * 37 % 250` — a pure function of the attempt number, so
  two workers that hit the same 429 slept for *exactly* the same time, which is
  the one thing jitter exists to prevent — and applied a 30 s cap *after* the
  server's figure, turning "wait two minutes" into thirty seconds and hammering.
* **`Engine::cancel` no longer needs a model provider.** It replays the journal
  and appends `run_ended`, dispatching nothing and calling nothing, but it was
  being handed the run path's engine — which fails outright without an API key.
  A `DELETE` on a keyless server therefore fell through to writing the row
  straight to `failed`, the exact shape D57 says breaks `API.md` §6.1. It now
  builds from `llm::Unreachable` and an empty tool set, which the SDK documents
  as legal, and a test takes the key away mid-life to prove it.
* **The `nl_definition` bounds moved into `compile::compile`**, beside the spend
  ceiling, for the reason the ceiling is there: it is the one place every
  compile passes through. They had been two copies of an `if` in two handlers —
  and, it turned out, two copies no test exercised. There are now three,
  including the unicode one that catches `len()` being used for a cap the
  contract states in characters.
* **`Retry-After` is supplied by `IntoResponse` for every `rate_limited`.**
  `API.md` §0 defines the code as "Too many attempts; `Retry-After` header set",
  and the pairing brute-force guard (D5) had been emitting it bare while the
  agent budget's carried a figure. An invariant that is part of a code's
  *definition* belongs with the code, not at each call site.

`just ci`'s `red-team` recipe also became `cargo test`. It was `cargo check`,
which proved the detached harness still compiled while all 85 injection cases
sat unexecuted — acceptable when the fence was hypothetical, not now that
`agents::fence` is what stands between hostile mail and the model.

**One efficiency finding was deliberately not taken.** `search_mail` asks Gmail
for 50 message ids and shows the model 10, so a cold page costs five paced
batches, ~4 s of sleep and 250 quota units for threads the model never sees.
The fix is not a cleanup: 50 *messages* collapse into at most 50 *threads*, so
asking for 10 would show the model an unpredictable number of hits and make
`truncated` meaningless. Re-tuning what an agent perceives is a product decision
with a live bench behind it, not something to slip into a refactor.

## D70 — The approval capability belongs to the card, not to the run.

`0001_init.sql` put `approval_token` and `approval_expires_at` on `agent_runs`,
where nothing ever read or wrote them. Using them at P5 would have been wrong
three times.

A run can pause **more than once**: `max_steps` is 12, and an agent holding
both `write_note` and `draft_reply` can be gated, approved, resumed and gated
again — so the second pause overwrites the first card's token and deadline.
`API.md` §7 keeps `approval_expires_at` non-null on a resolved or expired card,
"what lets an expired card say *when* it expired", and read through the run that
value changes under a finished card. And the SDK says so outright, in
`Resolution`'s own documentation: "a host that issues its own single-use token
per approval (NADE's `approval_token`, **one per feed item**) should store
`step_seq` beside it and pass it back here."

Migration 0006 moves all three onto `feed_items` and **drops** the two dead
columns. Dropping rather than leaving them is the point: a second place to look
for the same fact is worse than no place, and this one would have been the
wrong place.

The consequence is finding 11's: `feed_items.run_id` is `on delete set null`, so
deleting an agent used to leave a card holding a **live token** with no run to
approve — `new_count` stuck above zero for ever. `DELETE /agents/{id}` now
settles the agent's live cards *before* it cancels the runs, which also fixes
the lock order: every path takes `feed_items` then `agent_runs`.

## D71 — `Engine::run` on a parked run appends nothing, so the journal's primary key is not the backstop.

The first cut of P5 reasoned that two callers driving one run would collide on
`run_journal`'s `(run_id, seq)` and one would lose harmlessly. That is true of
two callers that *dispatch*. It is not true here: `Engine::run` replays, sees a
pending approval, and returns `PendingApproval` from `parked_outcome` **before**
`drive` — no append, no collision, microseconds. Nothing protected
`agent_runs`:

1. a stale `run_agent` job replays a parked run and gets its outcome back;
2. the user approves — the run moves to `queued`, the card resolves;
3. the resume job carries the run to `done` and writes the note;
4. the stale job settles, putting the run back to `pending_approval` with the
   answered request in `pending_action`. For ever, with the effect written and
   the card resolved.

Two fixes, and both are needed. `run_agent` returns early on `pending_approval`
and `waiting` — a parked run belongs to `resume_run` and to nothing else — so
the stale outcome is not generated. And every `settle` write carries a status
predicate, so one that is generated anyway cannot land. The predicate is
`status = 'running'`, not `status in ('queued','running')`: approve leaves the
run `queued`, so the looser one still lets step 4 through.

## D72 — Two contract violations had already shipped, invisible because `/feed` was unmounted.

`agents/run.rs`'s spend-ceiling card and `gmail/oauth.rs`'s needs-reauth card
both wrote `data.reason`. `data` is served **verbatim** by `GET /feed`, and
`FEED_DATA`'s `none` shape in `docs/contract/validate.py` is an exact key set —
"a missing key and an extra key are both violations". The reconnect path also
set `resolved_note` on a `kind: "info"` card, which `API.md` §7 forbids
outright. Neither could fail anything: no test served those rows, and
`validate.py` only ever sees the generated fixtures.

The reason moved to a column (0006), both writers moved with it, and the
reconnect stopped writing a note — the card resolving *is* the message, for
someone who has just come back from Google's consent screen.

What stops the next one is not the fix. `agents::feed::info_data` is now the
single author of every `action: "none"` payload, and
`every_card_the_system_raises_for_itself_is_contract_shaped` drives the **real
writers** and checks what `GET /feed` returns — the assertion neither card had.

## D73 — The daily cap counted the wrong thing, and the cheapest agent walked through the hole.

`PLAN.md` §Dev caps says "≤20 triaged messages per agent per day". The only
counter that existed, `ledger::triage_calls_today`, counts rows with
`purpose = 'triage'` — **model calls**. And `compile.rs` tells the model to
leave `semantic` null "if the filters suffice", so the *default* compiled agent
(`label_ids: ["INBOX"]`, `semantic: null`) makes no model call at all. It was
capped by nothing, while starting a full run per inbox message: `max_steps` 12,
50 000 tokens each. The $1/day ceiling was the only backstop, and it is
account-wide — one agent burning it silences every other.

The cap is now counted against **runs** as well, before the run row is written,
because a run is where the money is. Both breaches are a no-op plus an audit
row and never a job error: retrying into the same wall is how a queue
dead-letters itself.

The run's `dedupe_key` is also checked *before* the semantic call rather than
after. A replayed webhook re-enqueues `triage_message` — the jobs dedupe index
is partial on *pending* rows, so a completed job no longer suppresses one — and
`agent_runs.dedupe_key` would have refused the second run only after the model
had already been paid for.

## D74 — Three timestamps are tied together, and only the journal entry satisfies them.

`docs/contract/validate.py` asserts that a card's `created_at` **equals** the
`approval_requested` journal entry's, and that `approval_expires_at` equals
that plus seven days. `feed_items.created_at` defaults to `now()`, which is
settle-time and strictly later; `ApprovalRequest::requested_at` is the step's
`opened_at`, and `Entry::new` reads the clock a *second* time when it builds the
entry, so the two can differ by a second. Neither is the value the contract
names.

The producer reads the gate entry back out of `run_journal` — one
`distinct on (kind)` that also fetches the model's last words for the card's
body — and binds its `created_at` explicitly. Same trap `runtime/journal.rs`
documents for the journal itself, one table over.

## D75 — The card body is model prose, and it is the largest string on the home screen.

`RunOutcome::PendingApproval` carries no model text, so the body is the newest
`model_response`'s `text` — absent in the ordinary case, because a turn whose
whole content is a tool call has no prose (`API.md` §6.1). The fallback is
therefore the common path, and it is pinned by `assert_eq!` rather than left to
drift (D67).

More importantly, that prose came out of a model that had just read somebody
else's email. It cannot make NADE send anything — the tools do not exist — but
it can put "Sent your reply to ops@parcel-status-updates.com" on the home
screen at 14 pt, which PLAN C1/C2 calls a bug whoever caused it. The body is
screened for a **narrow** set of verbs — send, forward, archive, delete,
unsubscribe — and a hit falls back to the rendered sentence. Narrower than
`validate.py`'s `OUTBOUND_VERBS` on purpose: "schedule", "book" and "pay"
describe what is *in* the message rather than something NADE did, and screening
those would push most honest cards to the fallback. `validate.py` now sweeps
`body` too, with the same `reply` exemption `resolved_note` already had.

The other half of the same finding is the corpus's own number 10: a draft card
is contained **only** if it shows the recipient list and the `never_messaged`
flag, because "an approval card that renders only the body launders a
redirected draft". The contract has carried both fields since P1 and nothing
rendered them. Deviation 62.

## D76 — `fence::stored`, not `fence::field`, for a string a person will read.

`field` strips control characters, **neutralises** marker-shaped text, and caps.
`stored` does the first and the third. The card's subject and recipients take
`stored`: they are rendered to a human, and mangling `<<<NADE-UNTRUSTED-DATA`
inside a subject line would corrupt what the user reads for no gain — there is
no prompt on that path. What they share is the part that is not optional, since
PostgreSQL rejects a NUL inside a `jsonb` string and a live sync already died of
one (D29).

The triage prompt takes `field`, and it is the first NADE code to build a prompt
out of a **subject** — which is the corpus's open finding 6: "`body_text` never
contains it, so a prompt builder that fences only the body leaves an
attacker-controlled string outside the fence." `fence::nonce_from` gives triage
a nonce before any run exists, hashed from the message id rather than truncated
from it.

## D77 — Two invariants the fixtures state that runtime cannot keep, and one the contract had backwards.

`validate.py` tied an item's status to its run's exactly, in both directions.
Three of the four hold; `resolved` does not, and cannot: a run that pauses a
**second** time is `pending_approval` while the first card is legitimately
`resolved`, and one that fails on a later step is `failed`. The rule is relaxed
fixture-first to what is actually invariant — a `resolved` card's run may be
anywhere except `skipped` or `expired`, which would be two answers to one
question.

Two shapes were widened for the same underlying reason, that a card is raised
from `approval_requested` **before** the tool runs and can only publish what the
model's call already carried. `draft_reply`'s `data.thread_id` becomes
`string|null` — the tool's own `thread_id` is optional and §3 already typed a
draft row's the same way, so §7.1 was the outlier, and a model that omitted it
parked a run the server could not card at all. And the `none` shape gains
`draft_id`, because an agent with `approval_required = false` and `draft_reply`
in its tools writes a draft with no card to approve, and the `info` card that
followed had no field to name what it had made.

`agent_note` moved the other way: the server renders it from the pending action,
and `threads.json`'s "two next steps to approve" described the note's *contents*
— a string no server can reproduce from a tool call. The fixture moved to what
is derivable, which is what the Reply Drafter row had always been.

## D78 — The post-implementation review, and the four blockers it found in work that looked done.

An unbiased subagent reviewed the finished P5 diff with no brief but "find what
is wrong". Eighteen findings above MINOR, four of them blockers, and every one
of the four was in code that had already passed `just ci` twice.

**A shipped sentence promised an outbound action, and four guards were pointed
at it.** `Outbox.expiredNote` read *"This expired before it could be sent."* —
under a card whose own button says "Save draft". `OutboxDurabilityTests`,
`MailUITests` (twice) and `AccessibilityUITests` all matched
`\bsend(s|ing)?\b`, which does not match **`sent`**. A past tense is the most
natural way to claim an outbound action and it was the one form nothing looked
for. Fixed in the copy, in all four patterns, and in
`AccessibilityUITests`'s own offender table, which had never listed it.

Widening the pattern immediately found two *false* positives, and both are the
line worth writing down: `Delete` is a real v1 control (`DELETE /agents/{id}`),
and `Sent` is a Gmail mailbox. DESIGN §4 forbids "sending, archiving, Gmail
mutation" — an agent is none of those and a mailbox is a place. So the **UI**
sweep screens send/forward/archive and the **server's** card-body screen is
broader, because there the string is a claim about what happened to somebody's
mail rather than the name of a control.

**Every approved run raised a second card.** `settle` called `raise_run_info`
on any `Done`, and the headline flow ends in `Done`: card → approve → the resume
job writes the note → a *second* card saying "The agent saved a note." beside
the first saying "Saved to Notes.", with `new_count` climbing back the moment
the user cleared it. `feed.json` has one card per run. The test that should have
caught it asserted `new_count == 0` **before** the resume job ran — the one
moment the bug is invisible.

**The 2 KB triage cap was dead code.** It was applied *after* fencing, against a
ceiling of `2 KB + MAX_BLOCK_BYTES` — and `fence::fence` already truncates at
`MAX_BLOCK_BYTES` plus ~318 bytes of delimiters, so the predicate was
`10 558 > 12 288` and never true. Every semantic call paid five times the
documented input, on the one path that runs per inbound message, while three
comments and `.env.example` all said 2 KB. Capped before fencing, and asserted
on the real prompt rather than on the constant.

**D71's fix was TOCTOU.** `status = 'running'` cannot tell which job owns a run:
a stale `run_agent` past the early return holds a replayed `PendingApproval`
while the resume job claims the run and sets it `running`, and the stale settle
then matches the *resume's* write. The guard now also names the `attempt` the
claim stamped, because the statement that claims a run is the statement that
bumps it. Worth stating plainly: **no test in the phase could reach either
`settle` guard** — deleting them left everything green — and both are now
mutation-proven, along with the `on conflict` clause, the allowlist
intersection, the D61 ledger write, and the fenced subject.

## D79 — The half of a corpus test that measures nothing.

Two of the red-team assertions were unfalsifiable, and both for the same
reason: the fake model only ever emitted one tool.

`assert_eq!(notes, 0)` could not fail, because nothing in the run ever called
`write_note`. And the corpus's *central* claim — "the allowlist and the approval
gate are host-side" — was not measured at all: with `tools::build`'s
intersection deleted every case would be granted every tool, every run would
park at the gate, zero rows would still be written, and every assertion would
still have passed.

What distinguishes the two worlds is **which** runs park. An owner who asked for
a summary granted no mutating tool, so the model's call is refused at dispatch
and that run can only end `failed`. `parked_without_a_grant == 0` is the
assertion, and deleting the intersection turns it red.

The same shape appeared in the fence test: it rebuilt the prompt by hand from
`fence::field` instead of calling `triage::prompt`, so it tested the fence —
which has its own tests — and interpolating the subject raw left it green. It
calls the shipped builder now. And because the corpus contains no **subject**
carrying a forged delimiter, the property sweep alone still could not see the
regression; `a_forged_delimiter_in_the_subject_cannot_close_the_fence` supplies
the missing case, with the attacker guessing the nonce correctly.

## D80 — A queue cannot clean up after a handler, so the handler gets last rites.

The approve transaction commits the user's answer — token spent, card
`resolved`, run `queued` — and then depends on a job. Five failures later the
queue gave up, and the run sat `queued` for ever: `run_agent` has a different
dedupe key, nothing re-enqueues it, and there is no stuck-run reaper anywhere in
the tree. The card still read "Saved to Notes." about a note that was never
written, with one `audit_log` row as the only trace. That is the failure P5
exists to prevent, arriving through the back door.

`Handler::on_dead_letter` is the fix, and it belongs on the handler rather than
in `Queue::fail` because the queue does not know what a handler owns. Both run
handlers end the run through the engine — `run_ended` belongs in the journal
(`API.md` §6.1) — and correct the card's sentence. A handler that implements
nothing is saying its work is safe to lose, which for `gmail_incremental` is
true: the next webhook re-walks the same history.

## D81 — Two more places a status meant two different things.

**`409 conflict` is not `409 token_consumed`.** `error.rs` says so in its own
words — "one means 'reload', the other means 'you already won'" — and the iOS
outbox mapped both to `.alreadyRecorded`, telling the user their tap had been
recorded when it had not. They are now distinct outcomes; the handling is the
same (drop the row, re-read the card) and the *name* is now true, which is what
a later reader will act on.

**A deleted agent's card answered `token_consumed` too.** `settle_cards_of`
writes those cards to `skipped`, and `take` checked the status before the run —
so the ordinary path got the "you already won" answer and the documented
`410 gone` was reachable only in the narrow race the delete exists to prevent.
The run check moved ahead of the status check, which is also what gives `gone` a
real emitter rather than a fixture with no code behind it.

## D82 — Four smaller things the same review found, each worth a line.

**The card's recipient list was uncapped.** `MAX_RECIPIENTS` is enforced by
`draft_reply` at *execution*, which is after the human decides, so a coerced
model could put two hundred addresses on a card that renders two truncated
lines — defeating the control `backend/testdata/injection`'s finding 10 depends
on. Capped where the card is built.

**`POST /feed/seen` dismissed locally what the server refuses to dismiss.** The
server gates on `dismissible` so the needs-reauth card survives a scroll; the
client marked every id it sent `resolved` anyway, and `seenSent` then blocked
ever asking again. It takes the badge from the response and leaves the statuses
to the next `GET /feed` — and an id whose send failed leaves `seenSent`, so the
next scroll retries.

**The feed never armed its own generation guard.** `saveFeed` has taken an
`expectedGeneration` since P3 and the only caller passed `nil`; the thread path
does it correctly, which is what made this look finished. An in-flight page two
landing after a pull-to-refresh wrote `reached_end = true` and made pages three
onward unreachable.

**Two counters were written and never read.** `lastEnqueueError` — so a failed
durable write left the button in place with nothing said, and every further tap
did the same nothing. And `pending_action.attempts`, so one card whose 500
mapped to `.retry` pinned the whole queue for ever, on every foreground, with no
dead letter. The outbox now gives up at five, matching the server queue's own
`max_attempts`.

## D83 — The P5 cleanup pass, and the one defect it found.

Four independent reviews of the finished diff — reuse, simplification,
efficiency, altitude — none of them briefed to look for correctness bugs, all of
them read-only, each required to name the existing helper, the simpler form, the
cheaper alternative or the deeper home before an item counted as a finding.
Forty-three findings, twenty-seven applied, three skipped. Four pairs converged
independently on the same mechanism, which is what makes those four worth
believing.

**One of them was not cleanup.** `ThreadView`'s approval card drew the kicker,
the summary and the buttons, and nothing else. `FeedRow` drew a recipient row
above the same buttons, with a comment citing
`backend/testdata/injection/README.md` finding 10 verbatim: "`identity-02` and
`tool-01`/`tool-06` are contained **only** if the approval card shows the actual
recipient list and flags `never_messaged`. An approval card that renders only
the body launders a redirected draft." Approving a `draft_reply` from inside a
thread showed neither the address nor **Never messaged** — the control existed
on one of the two surfaces that offer the button. It is now `ApprovalControls`,
one view, drawing the recipient row and the buttons for both, and P6's push
detail and P7's draft sheet inherit it instead of copying whichever they find.

That is the same shape as D69: a defect hiding inside duplication, invisible to
a reviewer reading either copy on its own, obvious the moment they are lined up.

**`OutboxDriver.onProblem` was declared, invoked twice, and never assigned.**
D82 recorded that `lastEnqueueError` "had no reader, so a failed durable write
left the button in place with nothing said". The fix added a closure, two call
sites and a `StateCopy.approvalNotQueued` string — and nothing ever set the
closure, so the banner never appeared and the localized string had one
unreachable caller. `MailSync` wires it beside `onNeedsReauth` now. A bug fix
that ships inert is worse than the bug, because the record says it is closed.

**Five `match tool` sites became one table.** `data.action`, `action_label`,
the mail row's `agent_note`, the card's fallback sentence, the settled card's
italic line and whether `actions` carries `edit` were each their own match with
its own `_` arm, in two modules — so a tool nobody had taught them about did not
fail, it rendered as a note: "Saved to Notes." under a card that saved something
else. `feed::GatePresentation` is keyed by tool name and
`every_gated_tool_has_a_presentation` walks `V1_TOOLS`, so a sixth tool with a
gate and no copy is a red test rather than wrong copy. It also screens the
table's own strings through `promises_an_outbound_action`, which no `_` arm
could.

**Three copies of the priced forced-tool-call chain became one.** `compile` and
`triage::judge` both want an object rather than prose, so neither can go through
`Adapter` — and both hand-rolled the same five steps. **Both ledger defects this
project has ever had were a missing step of that chain**: D60 (the ceiling
belongs where the path spends) and D61 (price from `usage()` *before* parsing,
because a 200 was billed whether or not the body decoded). P5 wrote the third
copy. `anthropic::forced_tool_call` is now the one, next to `Adapter::chat`,
which does the same five steps for the other shape; callers keep only what
differs — the request, the tool name, and how they map `ForcedCall`.

**`feed.rs` claimed to be the single author of `feed_items` and was not.**
`run::raise_spend_ceiling_notice` and `gmail::oauth` each inlined their own
`insert … where not exists (…)`, borrowing only `info_data`. That half-measure
is exactly how D72's contract breach came to live in one writer and not the
other. `feed::raise_notice`/`resolve_notice` own the statement now, with
`OncePer::{Ever, UtcDay}` covering the two guards, and the module doc is true.

**Six copies of the outbound-verb regex became two.** D78's violation passed all
of them because all of them matched `send(s|ing)?` and none matched `sent`. P5
fixed the four *strings* and left the four *copies*, plus two weaker variants
(`contains("send")`, a six-word substring array) that could not have caught it
either. `OutboundCopy` — one per test target, because a UI test target cannot
see the unit target's sources, and the count is stated in both files.

**The rest, briefly.** `Spec::parse` was introduced as the typed reader of
`agents.spec` and left all four raw probes in place, including the one that
builds every run's system prompt — where `Spec::instruction` defaults to `""`
and the raw probe fell back to `nl_definition`, so a naive substitution would
have handed a model an empty task. `Spec::instruction_or` states that fallback
once. Four `audit_log` wrappers became `agents::audit` plus the caller's own
`subject`. `approve` and `skip` were the same forty-five lines twice, including
the commit-then-refuse dance two named tests exist to protect; they are one
`decide(…, Decision)`. `agent_name` re-queried a column `load`'s join already
had, inside the open settle transaction. `claim_expired` selected thirteen
columns, took row locks it released one statement later, and kept one field.
The mail trigger enqueued one job per ingested message inside the page
transaction — a hundred extra statements per Gmail history page, now one
`unnest`. `feed_items_run_step_idx` is partial on `step_seq is not null`, so the
thread screen's "newest card of this run" could not use it and scanned; three
indexes were added and `feed_items_token_idx`'s comment corrected to say it has
no reader. `Filters::matches` lowercased the whole message body per agent per
message to answer a filter the compiler never emits.

**Skipped, and why.** A `ticker::DueProbe` trait for `sync::schedule`'s two
due-sources: the layering complaint is fair — a Gmail sync module should not
import `agents::expire` — but a new module, a trait and a registry for two
entries is machinery ahead of its second real caller. Restructuring triage's
advisory pre-checks: it would move when a `triage_capped` audit row is written,
which is behaviour, not cleanup. Splitting `FixtureSeed`'s reference
`MailSource` out of `Debug/`: cosmetic, and the file is 540 lines under one
clear doc comment.

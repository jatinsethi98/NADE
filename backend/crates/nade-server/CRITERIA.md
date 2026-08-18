# nade-server — P1 acceptance criteria & edge-case checklist

Written **before** the code, per PLAN.md §Execution doctrine. Every line below is
either a test (named) or a `// EDGE:` comment sitting next to the code that
handles it. Most are both.

Marks are filled in after the adversarial self-review pass:
`[x]` = verified by a passing test, `[~]` = verified by inspection + an `// EDGE:`
comment (no automated test possible at P1), `[ ]` = not done.

---

## A. Crate skeleton

- [x] A1 `cargo build -p nade-server` succeeds **with no database reachable** —
  no `query!`/`query_as!` anywhere, `sqlx`'s `macros` feature is off in
  `Cargo.toml` so it cannot regress by accident.
- [x] A2 `cargo clippy -p nade-server --all-targets -- -D warnings` is clean.
- [x] A3 `cargo fmt --check` is clean.
- [x] A4 `#![forbid(unsafe_code)]` at the crate root.
- [x] A5 Binary + library split, so unit tests live in the module they test and
  `cargo test -p nade-server jobs::` / `auth::` select them by path.

## B. Migrations

- [x] B1 Every table in PLAN.md §Postgres schema exists after migration:
  accounts, gmail_tokens, messages, labels, sync_state, agents, agent_runs,
  run_journal, notes, drafts, feed_items, audit_log, devices, jobs (14).
  `db::tests::migration_creates_every_planned_table`
- [x] B2 **Superseded by `docs/SEARCH.md`.** `messages.fts` was a stored
  generated tsvector over the 30-day sync window — 0.78% of the mailbox — so a
  search for anything older returned an empty result indistinguishable from "no
  such mail exists". The column, its GIN index and the 100,000-character
  truncation are gone, and the criterion is now its inverse: **nothing
  maintains a second index**, checked against both the live schema and the
  source. `db::tests::nothing_maintains_a_second_index`
- [x] B3 Required indexes exist by name and kind (btree/gin, partial where
  specified). `db::tests::required_indexes_exist`
- [x] B4 Every `check` constraint from the plan rejects an out-of-domain value.
  `db::tests::status_check_constraints_reject_bad_values`
- [x] B5 Re-running the migrator on an already-migrated database is a no-op
  (idempotent). `db::tests::migrations_are_idempotent`
- [x] B6 Migrations apply cleanly on two independently created fresh databases.
  `db::tests::migrations_apply_to_two_fresh_databases`
- [x] B7 `agent_runs.dedupe_key` is unique; two rows with the same key conflict,
  and multiple `null`s are allowed.
  `db::tests::agent_runs_dedupe_key_is_unique_and_nullable`
- [x] B8 The `jobs` claim predicate uses an index, not a seq scan (EXPLAIN over
  20 000 rows). `jobs::tests::claim_query_uses_an_index_not_a_seq_scan`

## C. Dev database harness

- [x] C1 `DATABASE_URL` set → that URL is used verbatim, embedded Postgres is
  never started. `db::tests::explicit_database_url_is_used_verbatim`
- [~] C2 `DATABASE_URL` unset + `NADE_ENV=dev` → embedded Postgres boots, the
  `nade` database is created and migrated, the URL is logged. (Exercised by the
  live smoke in the final report; the whole test suite runs on the same
  embedded harness, so the boot path is covered on every `cargo test`.)
- [x] C3 `DATABASE_URL` unset + `NADE_ENV != dev` → hard, actionable startup
  error, never a silent embedded boot. `db::tests::prod_without_database_url_fails`
- [x] C4 Each test gets its **own** freshly created database; two tests cannot
  see each other's rows. `db::tests::each_test_gets_an_isolated_database`
- [x] C5 The harness works under default `cargo test` parallelism (server
  `max_connections=200`, per-test pools capped at 5).
- [~] C6 First boot downloads the Postgres binaries into `backend/.pgcache`; a
  failure prints what failed, where it was going, and what to do about it
  (`embedded.rs`, `download_hint()`).
- [~] C7 Data dir is `backend/.pgdata`, cache `backend/.pgcache` — both already
  in the repo `.gitignore`.

## D. `/v1/healthz`

- [x] D1 Unauthenticated: no `Authorization` header → 200, not 401.
  `api::health::tests::healthz_is_unauthenticated_and_reports_ok`
- [x] D2 Body is exactly `{"status":"ok","db":"ok","version":"<crate version>"}`.
  Same test.
- [x] D3 Pool cannot round-trip `select 1` → 503 with `"db":"down"`.
  `api::health::tests::healthz_reports_db_down_when_the_pool_is_unreachable`

## E. Jobs queue

- [x] E1 `enqueue` stores kind/payload/run_after and returns the id.
  `jobs::tests::enqueue_then_claim_round_trips_the_payload`
- [x] E2 Claim is exclusive under concurrency: 64 jobs, 8 racing tasks, every job
  claimed exactly once. `jobs::tests::claim_is_exclusive_under_concurrency`
- [x] E3 Claim honours `run_after` (future jobs are invisible).
  `jobs::tests::claim_skips_jobs_whose_run_after_is_in_the_future`
- [x] E4 Claim honours the handler registry: unknown kinds are never claimed.
  `jobs::tests::claim_only_takes_registered_kinds`
- [x] E5 Expired lease is re-claimable. `jobs::tests::expired_lease_is_reclaimable`
- [x] E6 Live lease survives the reaper: a heartbeated job is not reclaimed.
  `jobs::tests::reaper_never_touches_a_live_lease`
- [x] E7 Backoff is exactly `2^attempts` minutes, measured in-database so no
  wall-clock flake. `jobs::tests::backoff_is_exactly_two_to_the_attempts_minutes`
- [x] E8 Dead-letter on the 5th attempt: `dead_at` set, never claimable again,
  and an `audit_log` row exists.
  `jobs::tests::fifth_failure_dead_letters_and_writes_an_audit_row`
- [x] E9 A panicking handler does not poison the worker loop; the job is failed
  and the next job still runs.
  `jobs::tests::panicking_handler_does_not_poison_the_worker`
- [x] E10 Graceful shutdown: in-flight job finishes, the loop exits, nothing is
  left leased. `jobs::tests::graceful_shutdown_lets_an_inflight_job_finish`
- [x] E11 A worker that loses its lease (stolen) cannot complete or fail the job.
  `jobs::tests::a_worker_that_lost_its_lease_cannot_complete_the_job`
- [x] E12 Heartbeat extends the lease while a handler runs.
  `jobs::tests::heartbeat_extends_the_lease`

## F. Pairing auth

- [x] F1 The code is 6 digits, uniformly drawn from `0..1_000_000` (no modulo
  bias) by a CSPRNG. `api::auth::tests::pairing_codes_are_six_digits_and_unbiased`
- [x] F2 `POST /v1/auth/pair` with the live code returns
  `{"token":"nade_<hex>"}` matching `docs/contract/pair.json`'s key set + form.
  `api::auth::tests::pair_returns_a_contract_shaped_token`
- [x] F3 Only `sha256(token)` is stored; the plaintext token appears nowhere in
  `devices`. `api::auth::tests::only_the_token_hash_is_persisted`
- [x] F4 Second use of the same code → 401 with the
  `docs/contract/error_unauthorized.json` envelope.
  `api::auth::tests::a_consumed_code_is_rejected`
- [x] F5 Wrong code → 401, same envelope. `api::auth::tests::a_wrong_code_is_rejected`
- [x] F6 Expired code (TTL 10 min) → 401. `api::auth::tests::an_expired_code_is_rejected`
- [x] F7 Pairing attempts are rate limited (10/min/process); the 11th is 429.
  `api::auth::tests::pairing_attempts_are_rate_limited`
- [x] F8 Bearer middleware guards every `/v1/*` route except `healthz` and
  `auth/pair` — including paths that do not exist yet.
  `api::tests::unknown_v1_routes_are_auth_guarded`
- [x] F9 A valid token on an unknown `/v1` route → 404, not 401 (proves the
  middleware passes through). `api::tests::a_valid_token_reaches_the_router`
- [x] F10 `NADE_TOKEN` is accepted when `NADE_ENV=dev`.
  `api::auth::tests::dev_token_is_accepted_in_dev`
- [x] F11 `NADE_TOKEN` is **rejected** when `NADE_ENV != dev`, even when the env
  var is set. `api::auth::tests::dev_token_is_impossible_outside_dev`
- [x] F12 A revoked device's token is rejected.
  `api::auth::tests::a_revoked_device_is_rejected`
- [x] F13 Successful pairing writes an `audit_log` row.
  `api::auth::tests::pairing_writes_an_audit_row`

## G. Error envelope

- [x] G1 `unauthorized` serialises byte-identically (as parsed JSON) to
  `docs/contract/error_unauthorized.json`, status 401.
  `error::tests::unauthorized_matches_the_contract_fixture`
- [x] G2 Status codes: unauthorized 401, not_found 404, bad_request 400,
  internal 500, rate_limited 429. `error::tests::status_codes_match_the_codes`
- [x] G3 The envelope has exactly two keys under `error` and nothing else at the
  top level. `error::tests::envelope_shape_is_exactly_code_and_message`
- [x] G4 A panicking handler becomes a 500 `internal` envelope, not a dropped
  connection. `api::tests::a_panicking_handler_returns_the_internal_envelope`
- [x] G5 Malformed JSON body → 400 `bad_request` envelope, not axum's plain-text
  rejection. `api::auth::tests::malformed_json_is_a_bad_request_envelope`

## H. Ergonomics

- [~] H1 `backend/justfile` has `run test migrate pair fmt lint` (+ `build`,
  `db-stop`, `ci`).
- [~] H2 `backend/.env.example` documents every env var the crate reads; a test
  asserts the two lists agree. `config::tests::env_example_documents_every_var`
  → promoted to `[x]`.
- [x] H2 (as above, test-enforced).
- [~] H3 No secret ever reaches git: pair-code state lives in `backend/secrets/`,
  which is already gitignored.

---

## Mandated edge cases

| # | Edge case | Where it is handled | Verified by |
|---|---|---|---|
| 1 | **Empty input** — empty JSON body, empty `code`, empty `device_name`, empty queue, empty `Authorization` header | `api::auth::pair` validation; `Queue::claim` returns `None`; `require_bearer` | `malformed_json_is_a_bad_request_envelope`, `empty_or_oversized_device_name_is_rejected`, `claim_on_an_empty_queue_returns_none`, `missing_or_empty_authorization_is_unauthorized` |
| 2 | **Unicode** — emoji/CJK device names, unicode subjects and bodies stored and searched intact, non-ASCII `Authorization` header | `pair` (char-count limit, NUL rejection); ingest + the `q` sent to Gmail; `HeaderValue::to_str` | `unicode_device_names_round_trip`, `sync::tests::unicode_survives_ingest`, `search::tests::a_search_finds_a_message_outside_the_sync_window`, `non_ascii_authorization_header_is_unauthorized` |
| 3 | **Crash mid-step** — worker dies holding a lease | lease expiry makes the row claimable again; `Queue::release` on shutdown grace | `expired_lease_is_reclaimable`, `graceful_shutdown_lets_an_inflight_job_finish` |
| 4 | **Duplicate delivery / replay** — same pairing code twice, same job claimed twice, replayed dedupe key | code consumed by an atomic `unlink`; `for update skip locked` + lease stamp; `agent_runs.dedupe_key` unique | `a_consumed_code_is_rejected`, `claim_is_exclusive_under_concurrency`, `agent_runs_dedupe_key_is_unique_and_nullable` |
| 5 | **Expiry** — pairing code TTL, job lease TTL | `PairingStore::verify` TTL check; claim predicate `lease_expires_at < now()` | `an_expired_code_is_rejected`, `expired_lease_is_reclaimable` |
| 6 | **Pagination boundary** — P1 exposes no paginated endpoint; the analogue is the queue's `limit 1` claim at the boundaries | `Queue::claim` | `claim_on_an_empty_queue_returns_none`, `claim_is_exclusive_under_concurrency` (8 workers > jobs at the tail), `claim_skips_jobs_whose_run_after_is_in_the_future` |
| 7 | **429 / timeout** — P1 makes no outbound HTTP; the analogue is the pairing rate limit and the healthz DB timeout | `RateLimiter`; `healthz` 2 s timeout on `select 1` | `pairing_attempts_are_rate_limited`, `healthz_reports_db_down_when_the_pool_is_unreachable` |
| 8 | **Clock skew** — wall clock jumping | rate limiter uses monotonic `Instant`; all queue timing is computed **inside Postgres** (`now()`), never on the client; pairing TTL is wall clock but also rejects codes minted in the future | `RateLimiter` unit test; `backoff_is_exactly_two_to_the_attempts_minutes` (measured in-DB); `a_code_minted_in_the_future_is_rejected` |
| 9 | **Concurrent workers racing for the same row** | `for update skip locked limit 1` + lease stamp in one statement; `complete`/`fail`/`heartbeat` all re-assert `locked_by = $me` | `claim_is_exclusive_under_concurrency`, `a_worker_that_lost_its_lease_cannot_complete_the_job`, `reaper_never_touches_a_live_lease` |

---

# P2 — "Mail lands"

Written **before** the P2 code, same rule. Marks filled in after the adversarial
self-review pass: `[x]` = verified by the named test, `[~]` = verified by
inspection plus an `// EDGE:` comment (no automated test is possible), `[ ]` =
not done. **Nothing is `[ ]`.**

## I. Schema additions (amended into `0001_init.sql`)

- [x] I1 `attachments` exists with `message_id` (FK -> `messages(id)`, cascade),
  `att_id`, `name`, `mime`, `size_bytes`, `content_id` (nullable), `inline`, and
  is unique on `(message_id, att_id)`. Bytes are never a column - the test
  asserts the *absence* of `bytes`/`data`/`content`/`body`/`blob`/`payload`.
  `db::tests::attachments_table_matches_the_brief`
- [x] I2 Deleting a message cascades its attachments away.
  `db::tests::attachments_cascade_from_messages`
- [x] I3 `settings` can hold **one** row and no more: the same key is a unique
  violation, a different key is a check violation, so there is no way in.
  `db::tests::settings_is_a_singleton`
- [x] I4 `settings.approval_required_default` is `not null default true`. Same test.
- [x] I5 `agents.status` is `not null default 'draft'` - a bare insert yields a
  draft, and the column default is asserted too.
  `db::tests::a_created_agent_is_a_draft`
- [x] I6 `threads` + `thread_labels` rollups exist with the keyset index the list
  endpoint walks (backend/DECISIONS.md D13). `db::tests::required_indexes_exist`
- [x] I7 The thread-list query uses `thread_labels_keyset_idx` over 20 000
  threads: index scan, no `Seq Scan`, **and no `Sort` node**.
  `api::mail::tests::thread_list_query_uses_an_index`
- [x] I8 Every P1 migration test still passes against the amended file; the table
  count assertion moved 14 -> 18 rather than being deleted.
  `db::tests::migration_creates_every_planned_table`,
  `db::tests::migrations_apply_to_two_fresh_databases`

## J. `mail/parse.rs`

- [x] J1 **26/26** conformance cases pass, every field asserted, each case's
  `note` in the failure message. A panic is a failure.
  `mail::parse::tests::conformance_corpus`
- [x] J2 `body_html` is `None` when there is no genuine `text/html` part
  (PARSER.md trap 1). `mail::parse::tests::body_html_is_null_without_a_real_html_part`
  plus `body_html_present` on all 26 cases.
- [x] J3 The header block is transcoded from windows-1252 **only** when it is not
  valid UTF-8, and the body is never touched (trap 2).
  `mail::parse::tests::header_sanitisation_is_utf8_first_and_header_only`
- [x] J4 `find_header_end` accepts `\n\n` as well as `\r\n\r\n`, and takes
  whichever comes first. `mail::parse::tests::header_end_accepts_lf_only`
- [x] J5 The **first** `Subject` wins (trap 3).
  `mail::parse::tests::the_first_subject_header_wins` + corpus case 24.
- [x] J6 HTML -> text is our two-pass `lol_html` extractor: `<style>`/`<script>`
  content never leaks, entities are decoded, block boundaries become newlines,
  `alt` text is kept, link targets are dropped.
  `mail::html::tests::script_and_style_content_never_leaks`,
  `..::block_boundaries_do_not_fuse_words`,
  `..::entities_are_decoded_and_alt_text_is_kept`,
  `..::link_targets_are_dropped_and_link_text_is_kept`
- [x] J7 Zero-width and non-breaking padding becomes spaces and collapses.
  `mail::html::tests::marketing_preview_padding_is_stripped`
- [x] J8 `cid:` references are rewritten to
  `/v1/messages/{gmail_id}/attachments/{att_id}` **at parse time**.
  `mail::html::tests::cid_urls_are_rewritten`, corpus case 10, and
  `api::contract_tests::rewritten_inline_images_point_at_a_declared_attachment`
- [x] J9 `body_text` is never null; empty is legal (case 19).
  `mail::parse::tests::body_text_is_never_null_and_empty_is_legal`
- [x] J10 A missing or unparseable `Date` yields `None` and the caller falls back
  to Gmail's `internalDate` (cases 13, 14).
  `sync::tests::internal_date_wins_over_the_date_header`
- [x] J11 Live smoke over `testdata/live/raw/`: **60/60** parsed, nothing panics,
  `body_text` never empty, every message yields a sender and a timestamp; skips
  cleanly when the directory is absent. `mail::parse::tests::live_mail_smoke`
- [x] J12 Parsing is total on truncated, empty, and random-byte input.
  `mail::parse::tests::garbage_input_never_panics`,
  `mail::html::tests::malformed_markup_never_panics`

## K. Gmail OAuth (`gmail/oauth.rs`)

- [x] K1 `GET /v1/auth/gmail/start` -> 302 with `code_challenge`,
  `code_challenge_method=S256`, `state`, `access_type=offline`, `prompt=consent`,
  and no client secret. `api::gmail_auth::tests::start_redirects_with_pkce_and_state`
- [x] K2 The callback verifies **both** `state` and the PKCE verifier; an unknown
  or replayed `state` is refused and creates no second account.
  `api::gmail_auth::tests::callback_rejects_an_unknown_or_replayed_state`,
  `gmail::oauth::tests::a_state_is_single_use_and_expires`
- [~] K2a The 10-minute `state` TTL itself is a constant plus a predicate
  (`PendingAuths::take`); waiting it out in a test would cost ten minutes per run.
- [x] K3 A successful callback binds the account, stores the tokens, queues the
  first sync, writes an audit row, and renders a plain "you can close this tab"
  page. `api::gmail_auth::tests::callback_binds_the_account_and_renders_a_close_page`
- [x] K4 **Every** refresh persists the rotated refresh token.
  `gmail::oauth::tests::a_rotated_refresh_token_is_persisted`
- [x] K5 A refresh that omits `refresh_token` keeps the old one rather than
  nulling it. `gmail::oauth::tests::a_refresh_without_rotation_keeps_the_token`
- [x] K6 `invalid_grant` -> account `needs_reauth`, an `info` feed row, an audit
  row, and sync paused.
  `gmail::oauth::tests::invalid_grant_marks_needs_reauth_exactly_once`,
  `sync::tests::sync_is_paused_when_the_account_needs_reauth`
- [x] K7 A second `invalid_grant` does not write a second feed row, and
  re-consent clears the card. Same test.
- [x] K8 A live access token is reused with **no** HTTP call; an expired one is
  refreshed once.
  `gmail::oauth::tests::a_live_token_is_not_refreshed_but_a_nearly_dead_one_is`
- [x] K9 Tokens are AES-256-GCM at rest; the plaintext never appears in the
  column. `gmail::crypto::tests::*`, `gmail::oauth::tests::tokens_are_ciphertext_at_rest`
- [x] K10 Clock skew: a token expiring within 60 s is treated as expired, so it
  is refreshed rather than 401ing mid-batch. Same test as K8.
- [x] K11 Eight concurrent callers produce **one** refresh - two would burn the
  rotating token. `gmail::oauth::tests::concurrent_refreshes_do_not_race`
- [~] K12 The live consent click. Human-required and explicitly out of scope for
  this lane; every leg either side of it is covered above.

## L. Quota + backoff (`gmail/quota.rs`)

- [x] L1 250 units/user/second, debiting the **true** cost per call, so the
  ceiling is 50 `messages.get`/s. Asserted against an injected clock, never by
  timing. `gmail::quota::tests::the_bucket_is_250_units_per_second`,
  `..::each_method_debits_its_own_cost`
- [x] L2 Refill is proportional and clamped to one second's capacity.
  `gmail::quota::tests::refill_is_proportional_and_clamped`
- [x] L3 A cost larger than the capacity cannot deadlock the bucket.
  `gmail::quota::tests::an_oversized_cost_never_deadlocks`
- [x] L4 Backoff is exponential 1 s -> 60 s with jitter and never exceeds the cap.
  `gmail::quota::tests::backoff_is_exponential_and_capped`
- [x] L5 429 and 403 `rateLimitExceeded` retry; a permission 403 does not, and a
  404 is never retried. `gmail::client::tests::retries_429_then_succeeds`,
  `..::a_throttling_403_retries_but_a_permission_403_does_not`,
  `..::a_404_is_not_found_and_is_not_retried`
- [x] L6 `Retry-After` wins when longer, is ignored when shorter, and is capped.
  `gmail::quota::tests::retry_after_wins_when_longer`
- [x] L7 Clock skew: the bucket runs on monotonic `Instant`, and concurrent
  callers share one budget.
  `gmail::quota::tests::concurrent_callers_share_the_budget`

## M. Gmail client (`gmail/client.rs`, `gmail/batch.rs`)

- [x] M1 `getProfile`, `labels.list`, `messages.list`, `messages.get`
  (`raw` + `metadata` + `full`), `attachments.get`, `history.list` and the batch
  endpoint all round-trip against `wiremock`. `gmail::client::tests::*`
- [x] M2 `messages.list` follows `nextPageToken`, stops at the cap, and cannot
  spin on a server that always returns a token.
  `gmail::client::tests::list_paginates_and_stops_at_the_cap`,
  `..::an_empty_page_with_a_token_terminates`
- [x] M3 The batch body is real `multipart/mixed` posted to
  `/batch/gmail/v1`, one `Content-ID` per sub-request.
  `gmail::batch::tests::request_body_is_multipart_mixed`
- [x] M4 Responses are correlated by **`Content-ID`**, not by order: a reversed
  response still maps correctly.
  `gmail::batch::tests::out_of_order_responses_are_correlated_by_content_id`
- [x] M5 One sub-request 404ing returns the other 44 results, over real HTTP,
  with each message's body checked against *its own* id.
  `gmail::batch::tests::a_single_404_does_not_fail_the_batch`,
  `gmail::client::tests::a_real_batch_returns_the_other_44_when_one_is_gone`
- [x] M6 A malformed multipart response is an error, not a panic; a part with no
  status line is dropped and the rest of the batch survives.
  `gmail::batch::tests::a_malformed_response_is_an_error_not_a_panic`
- [x] M7 Empty input: a batch of zero requests makes **no** HTTP call.
  `gmail::client::tests::an_empty_batch_makes_no_request`,
  `gmail::batch::tests::an_empty_batch_is_just_the_terminator`
- [x] M8 Unicode survives the multipart round trip.
  `gmail::batch::tests::unicode_survives_the_round_trip`
- [x] M9 A stale `history.list` cursor is a `404` the caller can tell apart from
  an outage, which is what triggers a full re-sync at P3.
  `gmail::client::tests::history_pages_and_reports_a_stale_cursor`

## N. Sync (`sync/`)

- [x] N1 `getProfile`'s `historyId` is read **before** the list - asserted on the
  order of the received requests - and stored.
  `sync::tests::history_id_is_read_before_listing`
- [x] N2 The list query is exactly `newer_than:30d`, the cap is
  `NADE_MAX_SYNC_MESSAGES`, and both are checked on the wire.
  `sync::tests::the_dev_caps_are_applied`, `config::tests::the_dev_caps_have_the_values_plan_md_fixes`
- [x] N3 Batches are 45 messages, `format=raw`, at most one per second.
  `sync::tests::batches_are_45_and_paced`
- [x] N4 **Partial failure mid-sync**: an unparseable message gets a
  metadata-only row plus an `audit_log` entry, and the other four are fully
  parsed. `sync::tests::a_parse_failure_is_recorded_and_the_sync_continues`
- [x] N4a A single message Gmail could not serve is counted, audited, and left
  for the next sync. `sync::tests::one_broken_message_does_not_stop_the_batch`
- [x] N5 **A message that vanishes between list and get** is skipped, not
  retried, and is not audited as a failure.
  `sync::tests::a_message_that_vanished_is_skipped`
- [x] N6 Crash mid-sync: a re-run does not duplicate messages, threads, labels or
  attachments. `sync::tests::a_replayed_sync_is_idempotent`
- [x] N7 Duplicate delivery: the same test - a replayed sync is a no-op re-ingest.
- [x] N8 Labels are stored whole, including the `[Gmail]`-prefixed ones the API
  hides. `sync::tests::labels_are_stored_including_the_ones_the_api_hides`
- [x] N9 Per-thread rollups: newest message wins for ts/from/subject/snippet,
  `unread` is true if **any** message is, labels are the union.
  `sync::tests::thread_rollups_follow_the_newest_message`
- [x] N10 **Ingest never calls an LLM**, enforced by a source grep over
  `src/sync/` and `src/mail/`. `sync::tests::ingest_never_calls_an_llm`
- [x] N11 `needs_reauth` pauses sync: the handler succeeds, writes nothing, and
  makes **no** Gmail call at all.
  `sync::tests::sync_is_paused_when_the_account_needs_reauth`
- [x] N12 `gmail_sync` is a registered job kind, claimable, and a no-op when no
  account is connected. `sync::tests::gmail_sync_is_a_registered_job_kind`
- [x] N13 Empty input: a window with no messages issues no batch request and
  still records the cursor. `sync::tests::an_empty_window_is_not_an_error`
- [x] N14 Unicode: an RFC 2047 subject with an astral codepoint and a CJK body
  survive ingest byte for byte. `sync::tests::unicode_survives_ingest` (finding
  them again is Gmail's job now —
  `search::tests::a_search_finds_a_message_outside_the_sync_window`)
- [x] N15 Clock skew: `internal_ts` is Gmail's `internalDate`, with the sender's
  `Date` header only as a fallback.
  `sync::tests::internal_date_wins_over_the_date_header`

## O. Read endpoints (`api/`)

- [x] O1 `GET /me` returns `{email, status}` and answers before any account
  exists (backend/DECISIONS.md D17).
  `api::mail::tests::me_reports_the_account_and_its_status`
- [x] O2 `GET /mailboxes` is the eight-label whitelist in `API.md` §2's order and
  display names, then user labels by **raw byte value** of the name, with
  `[Gmail]`-prefixed ones hidden and `UNREAD`/`IMPORTANT`/`SPAM`/`TRASH` never
  exposed. `api::mail::tests::mailboxes_are_the_whitelist_then_user_labels`
- [x] O3 `unread`/`total` count **threads** inside the synced window, not
  messages. `api::mail::tests::mailbox_counts_are_threads_not_messages`
- [x] O4 `GET /mailboxes/{id}/threads` is keyset-paginated, `limit` default 50 and
  clamped at 100, sorted `ts desc, id desc`.
  `api::mail::tests::thread_list_paginates_by_keyset`,
  `api::cursor::tests::limits_are_clamped_to_the_contract`
- [x] O5 **Pagination boundary**: the last page is `next_cursor: null`; an empty
  or unknown mailbox is `[]` + `null`, never 404; a row inserted mid-scroll
  neither duplicates nor skips.
  `api::mail::tests::an_empty_mailbox_is_an_empty_array_not_a_404`,
  `api::mail::tests::a_row_inserted_mid_scroll_neither_duplicates_nor_skips`
- [x] O6 An unknown, corrupt, empty or foreign cursor is `400 bad_request` —
  including a keyset cursor handed to `/search`, or a search cursor handed to
  the thread list.
  `api::cursor::tests::an_unknown_or_corrupt_cursor_is_a_bad_request`,
  `api::cursor::tests::a_corrupt_or_mistyped_page_token_is_a_bad_request`,
  `api::mail::tests::an_unknown_cursor_is_a_bad_request`,
  `search::tests::a_corrupt_search_cursor_is_a_bad_request`
- [x] O7 `GET /threads/{id}` matches `API.md` §2: **no `id` on a message**,
  `body_text` non-null, `body_html` null without an HTML part, messages oldest
  first, `mailbox_name` + `account_email` present.
  `api::mail::tests::thread_detail_matches_the_contract`,
  `api::mail::tests::an_empty_body_is_an_empty_string`
- [x] O8 `GET /search` **delegates to Gmail** (`docs/SEARCH.md`): it covers the
  whole mailbox rather than the 30-day window, validates the query before
  sending it because Gmail answers a bad `q` with an empty `200`, hydrates cache
  misses, keeps Gmail's relevance order, and pages through Gmail's `pageToken`
  inside an opaque cursor. It still rejects empty/whitespace `q` with 400, caps
  `q` at 512 characters, returns `[]` + `null` for no hits, and shares the
  thread-row shape.
  `search::tests::a_search_finds_a_message_outside_the_sync_window`,
  `..::a_search_hydrates_a_cache_miss_and_leaves_it_cached`,
  `..::a_refused_query_is_a_400_that_says_why_and_gmail_would_have_said_nothing`,
  `..::results_keep_gmails_order_and_are_not_re_sorted_by_date`,
  `..::pages_walk_gmails_page_token_through_an_opaque_cursor`,
  `..::a_search_row_matches_the_thread_list_contract`,
  `api::mail::tests::search_refuses_an_empty_or_oversized_query`
- [x] O9 The attachment proxy streams from Gmail on demand, caps at 25 MB
  (refusing **before** spending quota), sets `Content-Disposition` with an
  RFC 5987 filename, and sets `Cache-Control: no-store, private`.
  `api::mail::tests::attachment_proxy_streams_from_gmail_with_an_rfc5987_filename`,
  `..::attachment_proxy_refuses_over_25mb`,
  `..::a_small_attachment_is_served_from_the_inline_body`
- [x] O10 A message Gmail no longer has, or an attachment we never stored, is a
  `404`. `api::mail::tests::attachment_proxy_404s_when_gmail_forgot_the_message`
- [x] O11 Unicode: emoji and CJK filenames are percent-encoded into an ASCII
  header, and a filename cannot inject one.
  `api::mail::tests::content_disposition_cannot_inject_a_header`
- [x] O12 Every new route is behind the bearer guard except the two
  browser-facing OAuth ones (backend/DECISIONS.md D15).
  `api::gmail_auth::tests::the_oauth_routes_are_public_and_the_rest_are_not`,
  `api::tests::unknown_v1_routes_are_auth_guarded`
- [x] O13 An upstream Gmail failure is `502 upstream_unavailable`, never a 500.
  `api::mail::tests::an_upstream_failure_is_a_502_not_a_500`
- [x] O14 `agent_note` is passed through verbatim from whichever run stored it -
  the seam P5 fills. `api::mail::tests::an_agent_note_is_passed_through_verbatim`

## P. Fixture conformance

- [x] P1 The real response types serialise to `docs/contract/`, compared as
  parsed `serde_json::Value` so key order cannot matter: `me`, `mailboxes`, all
  three thread pages, both search pages, both thread details, both health bodies,
  and all eight error codes we serve - message included.
  `api::contract_tests::*`, `error::tests::every_served_code_matches_its_contract_fixture`
- [x] P1a The fixture's `next_cursor` decodes with **our** decoder and
  re-encodes byte-identically, so it is a real example rather than a plausible
  string. `api::contract_tests::the_fixture_cursor_is_a_real_keyset_cursor`
- [x] P2 No fixture contradicted `API.md`. Two `API.md` *gaps* were found and
  reported rather than patched into `docs/`: the §0 auth-exception list omits the
  two browser-facing OAuth routes (D15), and §1 does not say what `/me` returns
  before an account exists (D17). Three error messages were adopted **from** the
  fixtures into `error.rs` so the two agree.

## Mandated edge cases - P2

| # | Edge case | Where | Verified by |
|---|---|---|---|
| 1 | **Empty input** | empty batch, empty window, empty `q`, empty mailbox, empty body, empty HTML, empty cursor | `an_empty_batch_makes_no_request`, `an_empty_window_is_not_an_error`, `search_refuses_an_empty_or_oversized_query`, `an_empty_mailbox_is_an_empty_array_not_a_404`, `body_text_is_never_null_and_empty_is_legal`, `empty_and_whitespace_html_is_empty_text`, `an_unknown_or_corrupt_cursor_is_a_bad_request` |
| 2 | **Unicode** | RFC 2047 in every form, cp1252 headers, astral subjects, CJK search, emoji filenames | `conformance_corpus`, `unicode_survives_ingest`, `unicode_survives_the_round_trip`, `search_matches_the_index_and_shares_the_thread_shape`, `content_disposition_cannot_inject_a_header`, `snippets_cap_characters_not_bytes` |
| 3 | **Crash mid-step** | sync resumes off the P1 job queue; every write is an upsert | `a_replayed_sync_is_idempotent`, `gmail_sync_is_a_registered_job_kind` |
| 4 | **Duplicate delivery / replay** | `(account_id, gmail_id)` unique + upsert; replayed OAuth `state`; repeated `invalid_grant` | `a_replayed_sync_is_idempotent`, `callback_rejects_an_unknown_or_replayed_state`, `invalid_grant_marks_needs_reauth_exactly_once` |
| 5 | **Expiry** | access-token expiry with a 60 s skew margin; single-use `state` | `a_live_token_is_not_refreshed_but_a_nearly_dead_one_is`, `a_state_is_single_use_and_expires` |
| 6 | **Pagination boundary** | keyset cursor, last page, mid-scroll insert, `limit` clamp, list cap | `thread_list_paginates_by_keyset`, `a_row_inserted_mid_scroll_neither_duplicates_nor_skips`, `limits_are_clamped_to_the_contract`, `list_paginates_and_stops_at_the_cap` |
| 7 | **429 / timeout** | quota bucket, exponential backoff, `Retry-After`, transport failure, 502 envelope | `the_bucket_is_250_units_per_second`, `backoff_is_exponential_and_capped`, `retry_after_wins_when_longer`, `retries_429_then_succeeds`, `an_upstream_failure_is_a_502_not_a_500` |
| 8 | **Clock skew** | monotonic bucket and pacer; `internalDate` over the `Date` header; 60 s expiry margin | `concurrent_callers_share_the_budget`, `internal_date_wins_over_the_date_header`, `a_live_token_is_not_refreshed_but_a_nearly_dead_one_is` |
| 9 | **Partial failure mid-sync** (new) | parse failure -> metadata row + audit, sync continues; one 404 inside a batch does not kill the batch; one whole batch failing does not kill the sync | `a_parse_failure_is_recorded_and_the_sync_continues`, `one_broken_message_does_not_stop_the_batch`, `a_real_batch_returns_the_other_44_when_one_is_gone` |
| 10 | **A message that vanishes between list and get** (new) | `404` on a batched `messages.get` is skipped, never retried, never audited as a failure | `a_message_that_vanished_is_skipped`, `a_single_404_does_not_fail_the_batch` |

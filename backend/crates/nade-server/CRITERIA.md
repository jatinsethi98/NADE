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
- [x] B2 `messages.fts` is a stored generated tsvector and is searchable with
  `websearch_to_tsquery('simple', …)`.
  `db::tests::fts_column_is_generated_and_searchable`
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
| 2 | **Unicode** — emoji/CJK device names, unicode subjects and bodies through the generated `fts` column, non-ASCII `Authorization` header | `pair` (char-count limit, NUL rejection); `messages.fts`; `HeaderValue::to_str` | `unicode_device_names_round_trip`, `fts_column_is_generated_and_searchable`, `non_ascii_authorization_header_is_unauthorized` |
| 3 | **Crash mid-step** — worker dies holding a lease | lease expiry makes the row claimable again; `Queue::release` on shutdown grace | `expired_lease_is_reclaimable`, `graceful_shutdown_lets_an_inflight_job_finish` |
| 4 | **Duplicate delivery / replay** — same pairing code twice, same job claimed twice, replayed dedupe key | code consumed by an atomic `unlink`; `for update skip locked` + lease stamp; `agent_runs.dedupe_key` unique | `a_consumed_code_is_rejected`, `claim_is_exclusive_under_concurrency`, `agent_runs_dedupe_key_is_unique_and_nullable` |
| 5 | **Expiry** — pairing code TTL, job lease TTL | `PairingStore::verify` TTL check; claim predicate `lease_expires_at < now()` | `an_expired_code_is_rejected`, `expired_lease_is_reclaimable` |
| 6 | **Pagination boundary** — P1 exposes no paginated endpoint; the analogue is the queue's `limit 1` claim at the boundaries | `Queue::claim` | `claim_on_an_empty_queue_returns_none`, `claim_is_exclusive_under_concurrency` (8 workers > jobs at the tail), `claim_skips_jobs_whose_run_after_is_in_the_future` |
| 7 | **429 / timeout** — P1 makes no outbound HTTP; the analogue is the pairing rate limit and the healthz DB timeout | `RateLimiter`; `healthz` 2 s timeout on `select 1` | `pairing_attempts_are_rate_limited`, `healthz_reports_db_down_when_the_pool_is_unreachable` |
| 8 | **Clock skew** — wall clock jumping | rate limiter uses monotonic `Instant`; all queue timing is computed **inside Postgres** (`now()`), never on the client; pairing TTL is wall clock but also rejects codes minted in the future | `RateLimiter` unit test; `backoff_is_exactly_two_to_the_attempts_minutes` (measured in-DB); `a_code_minted_in_the_future_is_rejected` |
| 9 | **Concurrent workers racing for the same row** | `for update skip locked limit 1` + lease stamp in one statement; `complete`/`fail`/`heartbeat` all re-assert `locked_by = $me` | `claim_is_exclusive_under_concurrency`, `a_worker_that_lost_its_lease_cannot_complete_the_job`, `reaper_never_touches_a_live_lease` |

---

# P2 — "Mail lands"

Written **before** the P2 code, same rule. `[x]` = verified by a named test,
`[~]` = verified by inspection plus an `// EDGE:` comment (no automated test
possible without a live Google), `[ ]` = not done.

## I. Schema additions (amended into `0001_init.sql`)

- [ ] I1 `attachments` exists with `message_id` (FK → `messages(id)`, cascade),
  `att_id`, `name`, `mime`, `size_bytes`, `content_id` (nullable), `inline`, and
  is unique on `(message_id, att_id)`. Bytes are never a column.
  `db::tests::attachments_table_matches_the_brief`
- [ ] I2 Deleting a message cascades its attachments away.
  `db::tests::attachments_cascade_from_messages`
- [ ] I3 `settings` can hold **one** row and no more; the second insert fails.
  `db::tests::settings_is_a_singleton`
- [ ] I4 `settings.approval_required_default` is `not null default true`.
  Same test.
- [ ] I5 `agents.status` is `not null default 'draft'` — a bare insert yields a
  draft. `db::tests::a_created_agent_is_a_draft`
- [ ] I6 `threads` + `thread_labels` rollups exist with the keyset index the
  list endpoint walks. `db::tests::required_indexes_exist`
- [ ] I7 The thread-list query uses `thread_labels_keyset_idx`, not a seq scan,
  over 20 000 threads. `api::mail::tests::thread_list_query_uses_an_index`
- [ ] I8 Every P1 migration test still passes with the amended file, and the
  table count assertion is updated rather than removed.

## J. `mail/parse.rs`

- [ ] J1 **26/26** conformance cases pass, every field asserted, the case's
  `note` in the failure message. A panic is a failure.
  `mail::parse::tests::conformance_corpus`
- [ ] J2 `body_html` is `None` when there is no genuine `text/html` part
  (PARSER.md trap 1) — asserted per case by `body_html_present`.
- [ ] J3 The header block is transcoded from windows-1252 **only** when it is
  not valid UTF-8, and the body is never touched (trap 2).
  `mail::parse::tests::header_sanitisation_is_utf8_first_and_header_only`
- [ ] J4 `find_header_end` accepts `\n\n` as well as `\r\n\r\n`.
  `mail::parse::tests::header_end_accepts_lf_only`
- [ ] J5 The **first** `Subject` wins (trap 3). Case 24 + a direct unit test.
- [ ] J6 HTML → text is our two-pass `lol_html` extractor: `<style>`/`<script>`
  content never leaks, entities are decoded, block boundaries become newlines,
  `alt` text is kept, link targets are dropped.
  `mail::html::tests::*`
- [ ] J7 Zero-width and non-breaking padding (`\u{00A0} \u{200B} \u{200C}
  \u{200D} \u{FEFF} \u{034F}`) becomes spaces and collapses.
  `mail::html::tests::marketing_preview_padding_is_stripped`
- [ ] J8 `cid:` references in `body_html` are rewritten to
  `/v1/messages/{gmail_id}/attachments/{att_id}` at parse time.
  `mail::parse::tests::conformance_corpus` (case 10) +
  `mail::html::tests::cid_urls_are_rewritten`
- [ ] J9 `body_text` is never null; empty is legal (case 19).
- [ ] J10 A missing or unparseable `Date` yields `None` and the caller falls
  back to Gmail's `internalDate` (cases 13, 14).
- [ ] J11 Live smoke over `testdata/live/raw/`: nothing panics, `body_text` is
  never empty, every message yields a sender and a timestamp; **skips cleanly**
  when the directory is absent. `mail::parse::tests::live_mail_smoke`
- [ ] J12 Parsing is total: `parse()` returns `Result`, never panics, on
  truncated, empty, and random-byte input.
  `mail::parse::tests::garbage_input_never_panics`

## K. Gmail OAuth (`gmail/oauth.rs`)

- [ ] K1 `GET /v1/auth/gmail/start` → 302 to Google with `code_challenge`,
  `code_challenge_method=S256`, `state`, `access_type=offline`,
  `prompt=consent`. `api::gmail_auth::tests::start_redirects_with_pkce_and_state`
- [ ] K2 The callback verifies **both** `state` and the PKCE verifier; an
  unknown/replayed `state` is refused.
  `api::gmail_auth::tests::callback_rejects_an_unknown_state`,
  `..._rejects_a_replayed_state`
- [ ] K3 A successful callback binds the account, stores the tokens, and renders
  a plain "you can close this tab" page.
  `api::gmail_auth::tests::callback_binds_the_account_and_renders_a_close_page`
- [ ] K4 **Every** refresh persists the (possibly rotated) refresh token.
  `gmail::oauth::tests::a_rotated_refresh_token_is_persisted`
- [ ] K5 A refresh that omits `refresh_token` keeps the old one rather than
  nulling it. `gmail::oauth::tests::a_refresh_without_rotation_keeps_the_token`
- [ ] K6 `invalid_grant` → account `needs_reauth`, an `info` feed row, an audit
  row, and sync paused. `gmail::oauth::tests::invalid_grant_marks_needs_reauth`
- [ ] K7 A second `invalid_grant` does not write a second feed row (idempotent).
  Same test.
- [ ] K8 A valid, unexpired access token is not refreshed; an expired one is,
  once. `gmail::oauth::tests::a_live_token_is_not_refreshed`
- [ ] K9 Tokens are encrypted at rest (AES-256-GCM); the plaintext never appears
  in the column. `gmail::crypto::tests::*`, `gmail::oauth::tests::tokens_are_ciphertext_at_rest`
- [ ] K10 Clock skew: the access token is treated as expired 60 s early, so a
  token that dies in flight is refreshed rather than 401ing.
  `gmail::oauth::tests::expiry_has_a_skew_margin`

## L. Quota + backoff (`gmail/quota.rs`)

- [ ] L1 The bucket is 250 units/user/second and debits the **true** cost per
  call (`messages.get` = 5), so the ceiling is 50 gets/s. Tested directly
  against an injected clock, never by timing.
  `gmail::quota::tests::the_bucket_is_250_units_per_second`
- [ ] L2 Refill is proportional and clamped to the capacity.
  `gmail::quota::tests::refill_is_proportional_and_clamped`
- [ ] L3 A cost larger than the capacity cannot deadlock the bucket.
  `gmail::quota::tests::an_oversized_cost_never_deadlocks`
- [ ] L4 Backoff is exponential 1 s → 60 s with jitter, and never exceeds 60 s.
  `gmail::quota::tests::backoff_is_exponential_and_capped`
- [ ] L5 429 and 403 `rateLimitExceeded` both retry; other 403s do not.
  `gmail::client::tests::retries_429_then_succeeds`,
  `..::a_plain_403_is_not_retried`
- [ ] L6 `Retry-After` on a 429 is honoured when it is longer than our backoff.
  `gmail::quota::tests::retry_after_wins_when_longer`
- [ ] L7 Clock skew: the bucket is driven by `Instant` (monotonic), never the
  wall clock. `// EDGE:` + L1's injected clock.

## M. Gmail client (`gmail/client.rs`, `gmail/batch.rs`)

- [ ] M1 `getProfile`, `labels.list`, `messages.list`, `messages.get`
  (`raw` + `metadata`), `attachments.get` and the batch endpoint all round-trip
  against `wiremock`. `gmail::client::tests::*`
- [ ] M2 `messages.list` follows `nextPageToken` and stops at the cap.
  `gmail::client::tests::list_paginates_and_stops_at_the_cap`
- [ ] M3 The batch body is real `multipart/mixed` posted to
  `https://gmail.googleapis.com/batch/gmail/v1`, one `Content-ID` per
  sub-request. `gmail::batch::tests::request_body_is_multipart_mixed`
- [ ] M4 Responses are correlated by **`Content-ID`**, not by order — a
  deliberately shuffled response still maps correctly.
  `gmail::batch::tests::out_of_order_responses_are_correlated_by_content_id`
- [ ] M5 One sub-request 404ing returns the other 44 results and does not fail
  the batch. `gmail::batch::tests::a_single_404_does_not_fail_the_batch`
- [ ] M6 A malformed multipart response is an error, not a panic.
  `gmail::batch::tests::a_malformed_response_is_an_error_not_a_panic`
- [ ] M7 Empty input: a batch of zero requests makes no HTTP call.
  `gmail::batch::tests::an_empty_batch_makes_no_request`
- [ ] M8 Unicode: a subject with astral codepoints survives the multipart
  round-trip. `gmail::batch::tests::unicode_survives_the_round_trip`

## N. Sync (`sync/`)

- [ ] N1 `getProfile`'s `historyId` is read **before** the list, and stored, so
  a message arriving mid-sync overlaps instead of vanishing.
  `sync::tests::history_id_is_read_before_listing`
- [ ] N2 The list query is exactly `newer_than:30d` and the cap is
  `NADE_MAX_SYNC_MESSAGES`. `sync::tests::the_dev_caps_are_applied`
- [ ] N3 Batches are 45 messages, `format=raw`, at most one per second.
  `sync::tests::batches_are_45_and_rate_limited`
- [ ] N4 **Partial failure mid-sync**: a message whose parse fails gets a
  metadata-only row plus an `audit_log` entry and the sync carries on.
  `sync::tests::a_parse_failure_is_recorded_and_the_sync_continues`
- [ ] N5 **A message that vanishes between list and get** (404) is skipped, not
  retried, and does not fail the sync.
  `sync::tests::a_message_that_vanished_is_skipped`
- [ ] N6 Crash mid-sync: the job resumes and already-ingested messages are not
  duplicated (upsert on `(account_id, gmail_id)`).
  `sync::tests::a_resumed_sync_does_not_duplicate`
- [ ] N7 Duplicate delivery: enqueuing two syncs for one account leaves one
  logical sync — the second is a no-op re-ingest.
  `sync::tests::a_replayed_sync_is_idempotent`
- [ ] N8 Labels are stored, and `[Gmail]`-prefixed ones are stored but hidden by
  the API. `sync::tests::labels_are_stored`
- [ ] N9 Per-thread rollups are maintained: newest message wins for
  `ts`/`from`/`subject`/`snippet`, `unread` is true if **any** message is,
  `msg_count` counts messages in the window.
  `sync::tests::thread_rollups_follow_the_newest_message`
- [ ] N10 **Ingest never calls an LLM.** Enforced by a source-grep test, the
  same way D1 is. `sync::tests::ingest_never_calls_an_llm`
- [ ] N11 `needs_reauth` pauses sync: the handler returns cleanly and writes
  nothing. `sync::tests::sync_is_paused_when_the_account_needs_reauth`
- [ ] N12 The whole thing is a registered job `kind` (`gmail_sync`) and
  heartbeats its lease. `sync::tests::gmail_sync_is_a_registered_job_kind`
- [ ] N13 Empty input: an account with zero messages in the window syncs to a
  clean, empty state. `sync::tests::an_empty_window_is_not_an_error`
- [ ] N14 Unicode: an astral-plane subject and a CJK body survive ingest.
  `sync::tests::unicode_survives_ingest`
- [ ] N15 Clock skew: `internal_ts` comes from Gmail's `internalDate`, never
  from our clock; `Date`-header time is only a fallback when it is present.
  `// EDGE:` + `sync::tests::internal_date_wins_over_the_date_header`

## O. Read endpoints (`api/`)

- [ ] O1 `GET /me` returns `{email, status}`; `status` follows
  `accounts.status`. `api::me::tests::*`
- [ ] O2 `GET /mailboxes` returns the eight-label whitelist with `API.md` §2's
  display names and order, then user labels by name, `[Gmail]`-prefixed hidden.
  `api::mail::tests::mailboxes_are_the_whitelist_then_user_labels`
- [ ] O3 `unread`/`total` count **threads** inside the synced window.
  `api::mail::tests::mailbox_counts_are_threads_not_messages`
- [ ] O4 `GET /mailboxes/{id}/threads` is keyset-paginated, `limit` default 50,
  max 100, sorted `ts desc, id desc`.
  `api::mail::tests::thread_list_paginates_by_keyset`
- [ ] O5 **Pagination boundary**: the last page has `next_cursor: null`; an
  empty mailbox is `[]` + `null`, never 404; a row inserted mid-scroll cannot
  duplicate or skip. `api::mail::tests::pagination_boundaries`,
  `..::a_row_inserted_mid_scroll_neither_duplicates_nor_skips`
- [ ] O6 An unknown or corrupt cursor is `400 bad_request`, never a silent reset.
  `api::cursor::tests::*`, `api::mail::tests::an_unknown_cursor_is_a_bad_request`
- [ ] O7 `GET /threads/{id}` matches `API.md` §2: no `id` on a message,
  `body_text` non-null, `body_html` null when there is no HTML part, messages
  oldest first, `mailbox_name` + `account_email` present.
  `api::mail::tests::thread_detail_matches_the_contract`
- [ ] O8 `GET /search` rejects empty/whitespace `q` with `400`, returns `[]` +
  `null` for no hits, and caps `q` at 512 chars.
  `api::mail::tests::search_*`
- [ ] O9 The attachment proxy streams from Gmail on demand, caps at 25 MB
  (`413`), sets `Content-Disposition` with an RFC 5987 filename, and never
  caches. `api::mail::tests::attachment_proxy_*`
- [ ] O10 A message Gmail no longer has → `404 not_found`.
  `api::mail::tests::attachment_proxy_404s_when_gmail_forgot_the_message`
- [ ] O11 Unicode: a filename with emoji and CJK survives `filename*=UTF-8''…`.
  `api::mail::tests::attachment_filenames_are_rfc5987_encoded`
- [ ] O12 Every new route is behind the bearer guard except the two
  browser-facing OAuth ones. `api::tests::unknown_v1_routes_are_auth_guarded`,
  `api::gmail_auth::tests::the_oauth_routes_are_public`
- [ ] O13 429/timeout: an upstream Gmail failure surfaces as
  `502 upstream_unavailable`, never a 500 or a hang.
  `api::mail::tests::an_upstream_failure_is_a_502`

## P. Fixture conformance

- [ ] P1 The real response types serialise to the `docs/contract/` fixtures,
  compared as parsed `serde_json::Value` so key order does not matter.
  `api::contract_tests::*`
- [ ] P2 Where a fixture and `API.md` disagree, `API.md` wins and the
  discrepancy is reported, not patched into `docs/`.

## Mandated edge cases — P2

| # | Edge case | Where | Verified by |
|---|---|---|---|
| 1 | Empty input | empty batch, empty window, empty `q`, empty mailbox, empty body (case 19) | M7, N13, O8, O5, J9 |
| 2 | Unicode | RFC 2047 in every form, astral subjects, cp1252 headers, emoji filenames | J1, M8, N14, O11 |
| 3 | Crash mid-step | sync resumes from the job queue; upserts make re-ingest harmless | N6 |
| 4 | Duplicate delivery / replay | `(account_id, gmail_id)` unique + upsert; replayed OAuth `state` refused | N7, K2 |
| 5 | Expiry | access-token expiry with a 60 s skew margin; OAuth `state` TTL | K8, K10, K2 |
| 6 | Pagination boundary | keyset cursor, last page, mid-scroll insert, `limit` clamp | O4, O5 |
| 7 | 429 / timeout | quota bucket, exponential backoff, `Retry-After`, 502 envelope | L1, L4, L5, L6, O13 |
| 8 | Clock skew | monotonic bucket; `internalDate` not our clock; skewed expiry margin | L7, N15, K10 |
| 9 | **Partial failure mid-sync** | parse failure → metadata row + audit, sync continues; one 404 in a batch does not kill the batch | N4, M5 |
| 10 | **A message that vanishes between list and get** | 404 on `messages.get` is skipped, never retried | N5 |

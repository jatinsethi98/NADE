# `nade-gmail-sim` — acceptance criteria and edge-case checklist

Written **before** any code, per the execution doctrine in `docs/PLAN.md`.
Every edge case below is either a named test or an `// EDGE:` comment beside the
code that handles it. Tests are preferred; the table records which.

---

## 1. Scope

A **stateful, in-process simulator of the Gmail REST API v1**. It holds a
mailbox — messages, labels, threads, a history log, a monotonic `historyId` —
and serves every endpoint *consistently from that state*, the way Gmail does.

It exists because the failures that matter in a mail sync are stateful, and a
static stub has no state. A `wiremock` stub cannot express "the second
application of this history page must change nothing", because it has no idea
it is the second.

**In scope**

* the mailbox model and its mutation API (every mutation appends history);
* the endpoints listed in §3;
* the `multipart/mixed` batch endpoint, parsed and generated for real;
* deterministic fault injection, including a quota model;
* two transports (in-process trait, and HTTP on `127.0.0.1:0`) over **one**
  implementation;
* seeding from the live `.eml` sample, from the MIME conformance corpus, and
  from a compact literal builder;
* a `Scenario` type that scripts mutations against a controlled clock.

**Out of scope** — deliberately, and each is safe because NADE never calls it:

* sending mail (`messages.send`, `drafts.*`) — v1 takes no outbound actions
  (PLAN.md C1/C2), so there is nothing to test against;
* `settings.*`, `messages.import`, `messages.insert`, filters, forwarding,
  delegation, S/MIME;
* real Gmail search relevance, spam classification, and Gmail's own
  categorisation into `CATEGORY_*` (the sim lets a test set those labels
  explicitly instead of guessing them);
* multiple users — the simulator is one mailbox, addressed as `me` or as its own
  email address.

**Hard rules**

* **Zero NADE types.** It models *Gmail*. No dependency on `nade-server` or
  `nade-agent-sdk`, ever.
* **No wall clock.** Enforced at compile time: `chrono` is taken with
  `default-features = false`, which removes `Utc::now` from the crate entirely,
  and a source-scanning test (`no_wall_clock_and_no_rng`) rejects
  `SystemTime::now`, `Instant::now` and any RNG.
* **No RNG.** No `rand`, no `uuid::new_v4`. Ids, page tokens, MIME boundaries
  and batch boundaries are all derived from deterministic counters.

---

## 2. Acceptance criteria

| # | Criterion | How it is checked |
|---|---|---|
| A1 | Crate and all targets build clean | `cargo build -p nade-gmail-sim --all-targets` |
| A2 | No clippy warnings anywhere, including tests | `cargo clippy -p nade-gmail-sim --all-targets -- -D warnings` |
| A3 | Formatted | `cargo fmt --check` |
| A4 | All unit, integration and doc tests green | `cargo test -p nade-gmail-sim` |
| A5 | Rustdoc builds with no warnings | `cargo doc -p nade-gmail-sim --no-deps` |
| A6 | The above pass **twice consecutively** | run the block twice |
| A7 | One implementation, two transports | `http_and_in_process_agree_byte_for_byte` compares every response body from the HTTP server against the in-process one for the same script |
| A8 | Byte-identical replay | `the_same_script_twice_is_byte_identical` runs a 40-call script against two fresh simulators and asserts every response body and status is equal |
| A9 | The sync story the crate exists for | `initial_sync_then_incremental_then_replay_changes_nothing` — initial sync → three mutations → incremental history → **replay the same page** → second application is a no-op |
| A10 | No dependency on NADE crates | manifest review + `no_nade_dependencies` test reading `Cargo.toml` |
| A11 | Every mutation appends exactly one history record and bumps `historyId` by a strictly increasing amount | `every_mutation_appends_exactly_one_history_record` |
| A12 | Wire shapes match Gmail | golden JSON assertions per endpoint, plus the string/number fidelity tests in E30–E33 |

---

## 3. Endpoint checklist

| Endpoint | Must do |
|---|---|
| `users.getProfile` | `emailAddress`, `messagesTotal`, `threadsTotal`, `historyId` (**string**) |
| `users.labels.list` | system labels with `name == id` and `type: "system"`; user labels with `type: "user"`; counts |
| `users.labels.get` | same resource, plus the counters Gmail only returns here |
| `users.messages.list` | `q` (§4), `maxResults`, `includeSpamTrash`, `labelIds`, keyset `pageToken`, `resultSizeEstimate` |
| `users.messages.get` | `format` = `minimal` \| `metadata` \| `full` \| `raw`, `metadataHeaders` |
| `users.messages.attachments.get` | `attachmentId`, `size`, `data` (base64url) |
| `users.history.list` | all four `historyTypes`, `labelId`, paging, `404` outside the retention window |
| `users.watch` / `users.stop` | `historyId` + `expiration`; 7-day default expiry driven by the `Clock` |
| `users.threads.list` / `users.threads.get` | not required by the brief; included because they are pure reuse and make "the thread grew under a cursor" testable |
| batch | `multipart/mixed` in and out, correlated by `Content-ID`, per-sub-request quota, one `404` does not fail the batch |
| OAuth token endpoint | `refresh_token` grant, rotation, and `invalid_grant` |

---

## 4. Query (`q`) support

Implicit AND between terms; `OR` (uppercase) and `{a b}`; `-` negation;
parentheses; quoted phrases.

Operators: `newer_than:Nd|Nm|Ny`, `older_than:`, `after:`/`before:`
(epoch-seconds or `YYYY/MM/DD`), `label:`, `-label:`, `in:`, `from:`, `to:`,
`cc:`, `bcc:`, `subject:`, `is:unread|read|starred|important`,
`has:attachment`, `filename:`, `larger:`/`smaller:`, `rfc822msgid:`, and bare
words matched against subject + decoded body + sender.

Anything the parser does not recognise is treated as a free-text term, which is
what Gmail does — it never rejects a query.

---

## 5. Edge-case checklist

The brief's minimum list is E1–E13; the rest came out of the design and the
adversarial self-review.

| # | Edge case | Expected behaviour | Covered by |
|---|---|---|---|
| E1 | Empty mailbox | `list` → `{"resultSizeEstimate":0}` with **no** `messages` key; `getProfile` totals 0; `history.list` from the seed id → no `history` key | `empty_mailbox_*` |
| E2 | A single message | initial sync yields exactly one row; every format renders it | `single_message_*` |
| E3 | Pagination exactly on a boundary | `maxResults == len` → **no** `nextPageToken`; `maxResults == len-1` → a token whose page 2 holds exactly the last item | `pagination_on_the_exact_boundary` |
| E4 | A message deleted mid-pagination | keyset cursor: page 2 loses the deleted row but skips nothing and duplicates nothing | `message_deleted_between_pages` |
| E5 | Replayed history page | applying the same page twice changes nothing; the sim's own state is untouched by reads | `initial_sync_then_incremental_then_replay_changes_nothing` |
| E6 | `historyId` far in the future | Gmail's behaviour, not an invention — see §7.3 | `history_id_from_the_future` |
| E7 | `historyId` far in the past | `404 notFound`, with Gmail's exact error body | `history_id_older_than_retention_is_404` |
| E8 | A label removed twice | second removal is a **no-op**: no history record, `historyId` unchanged | `removing_a_label_twice_appends_one_record` |
| E9 | Batch of 1 | still `multipart/mixed`, still correlated | `batch_of_one` |
| E10 | Batch of 100 | all 100 correlate; quota debited 100× | `batch_of_one_hundred` |
| E11 | Batch where every sub-request fails | outer status is **200**, every part carries its own error | `batch_where_every_subrequest_fails` |
| E12 | Unicode and RFC-2047 headers | `format=raw` bytes are byte-identical to what was inserted; `format=full` headers stay **encoded**; snippet is HTML-escaped | `unicode_and_rfc2047_round_trip` |
| E13 | An 8 MB message | rendered in every format without truncation; `sizeEstimate` reflects it; base64url length is exact | `eight_megabyte_message` |
| E14 | Trashed message | **not** a 404 — `messages.get` still returns it, with `TRASH` in `labelIds`; it vanishes from `list` unless `includeSpamTrash=true`. See §7.1 | `trashed_message_is_gettable_but_unlisted` |
| E15 | Permanently deleted message | `404`; `attachments.get` on it also `404` | `deleted_message_is_404` |
| E16 | `pageToken` from a different query | `400 invalidArgument` — a token is bound to its query | `page_token_from_another_query_is_rejected` |
| E17 | Garbage `pageToken` | `400`, never a panic and never page 1 | `garbage_page_token_is_400` |
| E18 | `maxResults=0` / above the cap | 0 → Gmail's default (100); above 500 → clamped to 500 | `max_results_clamping` |
| E19 | History page boundary | `maxResults` on `history.list` splits records without dropping or repeating one; the last page has no `nextPageToken` | `history_paging_never_drops_a_record` |
| E20 | `historyTypes` filter | a record with no matching change is omitted **entirely**, not returned empty | `history_types_filter_omits_empty_records` |
| E21 | Two mutations to one message | two records, two ids, both visible in one page | `two_mutations_on_one_message` |
| E22 | Insert into an existing thread | `threadId` reused; thread message count changes under an open list cursor | `thread_grows_under_a_cursor` |
| E23 | Quota exhaustion | 50 `messages.get` in one simulated second pass; the 51st is over quota | `quota_ceiling_is_fifty_gets_per_second` |
| E24 | Quota refills with the clock | advancing the clock refills, deterministically | `quota_refills_when_the_clock_advances` |
| E25 | 429 with `Retry-After` | header present and exact; body is Gmail's `rateLimitExceeded` | `injected_429_carries_retry_after` |
| E26 | 403 `rateLimitExceeded` vs `userRateLimitExceeded` | distinct bodies, both 403, both distinguishable from 429 | `the_two_403_rate_limits_differ` |
| E27 | 500 / 503 | exact bodies; 503 may carry `Retry-After` | `five_hundred_and_five_oh_three` |
| E28 | Expired access token | `401` with `reason: "authError"`; after a refresh the same call succeeds | `expired_token_then_refresh_succeeds` |
| E29 | `invalid_grant` on refresh | `400 {"error":"invalid_grant"}`, and the old access token stays dead | `invalid_grant_on_refresh` |
| E30 | `historyId` is a **string** in message/profile/history JSON | asserted as `Value::String`, never a number | `history_id_is_a_string_on_the_wire` |
| E31 | `internalDate` is a **string** of epoch millis | asserted as `Value::String` | `internal_date_is_a_string_of_millis` |
| E32 | `sizeEstimate`, `resultSizeEstimate`, `body.size` are **numbers** | asserted as `Value::Number` | `sizes_are_numbers_on_the_wire` |
| E33 | `resultSizeEstimate` is an estimate, not a count | matches the live sample: 500 rows returned, `nextPageToken` present, estimate `501` | `result_size_estimate_is_an_estimate` |
| E34 | Batch response order | **reversed** by default, because Google does not guarantee order; a client that zips must break here | `batch_responses_come_back_out_of_order` |
| E35 | Batch sub-request with a bad path | that part is `404`, the rest succeed | `batch_with_an_unroutable_subrequest` |
| E36 | Batch with a duplicate `Content-ID` | both parts returned; correlation is the client's problem, and the sim does not hide it | `batch_with_duplicate_content_ids` |
| E37 | Empty batch body | `400` — Gmail rejects a batch with no parts | `empty_batch_is_400` |
| E38 | Batch over the size cap | `413`-shaped `400` per Google's documented 100-call limit | `batch_over_one_hundred_is_rejected` |
| E39 | `format=metadata` with `metadataHeaders` | only the named headers come back, matched case-insensitively | `metadata_headers_filter` |
| E40 | `format=full` body data | base64url **unpadded**, transfer-encoding already decoded, attachment parts carry `attachmentId` and no `data` | `full_format_part_tree` |
| E41 | `format=raw` | base64url **unpadded** of the exact stored bytes; `payload` absent | `raw_format_is_exact_bytes` |
| E42 | Message with no `Date` header | `internalDate` still present (the sim assigns it); corpus case 13 | `corpus_message_without_a_date_header` |
| E43 | Message with CRLF vs LF line endings | stored verbatim; `format=raw` does not normalise; corpus case 25 | `mixed_line_endings_survive_raw` |
| E44 | A label deleted while messages carry it | membership drops from every message, each as its own `labelRemoved` record | `deleting_a_label_removes_it_everywhere` |
| E45 | Deleting a system label | `400` — Gmail refuses | `system_labels_cannot_be_deleted` |
| E46 | `mark_read` on an already-read message | no-op, no history record | `marking_a_read_message_read_is_a_noop` |
| E47 | Clock never moves on its own | 1000 calls with no `advance` → `historyId` unchanged, all `expiration`s equal | `the_clock_only_moves_when_told` |
| E48 | `newer_than:30d` boundary | a message exactly `30d` old is **excluded**, `30d - 1ms` included | `newer_than_boundary_is_exclusive` |
| E49 | `label:` matching | case-insensitive, matches system labels by id and user labels by name, spaces as `-` | `label_query_matching` |
| E50 | Unknown `q` operator | treated as free text, never an error | `unknown_query_operator_is_free_text` |
| E51 | Live corpus absent | seeding returns `Ok(0)` and the tests that need it skip loudly, never fail | `live_corpus_degrades_gracefully` |
| E52 | Whole MIME corpus seeds | all 26 cases load, list, and render in all four formats without panicking | `mime_corpus_seeds_and_renders` |
| E53 | `watch` expiry | exactly 7 days from the sim clock, as a **string** of epoch millis | `watch_returns_a_seven_day_expiry` |
| E54 | `stop` on a mailbox that is not watched | `204`, not an error | `stop_without_watch_is_fine` |
| E55 | Concurrent readers | `Simulator` is `Send + Sync`; interleaved reads never observe a half-applied mutation | `simulator_is_send_sync` + `mutation_is_atomic_under_readers` |
| E56 | HTTP transport port | always `127.0.0.1:0`, never a fixed port; two servers coexist | `two_servers_coexist_on_ephemeral_ports` |
| E57 | Slow fault | in-process reports `latency` and does not sleep; over HTTP it really sleeps, so a `reqwest` timeout can fire | `slow_fault_reports_latency` |
| E58 | Nth-call fault | fires on exactly the Nth matching call, then stops | `nth_call_fault_fires_once` |
| E59 | History for a message inserted then deleted | both records present in the same page, in id order | `insert_then_delete_shows_both_records` |
| E60 | `startHistoryId` equal to the current `historyId` | empty result plus the current `historyId`, not a 404 | `start_at_current_history_id_is_empty` |

---

## 6. Determinism protocol

1. Every id, token, boundary and `Message-ID` is a pure function of a counter
   and the mailbox contents.
2. `Clock` is a trait; `TestClock` is the only implementation the crate ships,
   and it moves only when a test calls `advance` or `set`.
3. Faults fire on counted, matched calls — never on a probability.
4. JSON is emitted through `serde_json` with `BTreeMap`-backed objects
   (`preserve_order` is **not** enabled), so key order is stable.
5. A8 pins all of the above: the same script against two fresh simulators
   produces byte-identical bodies.

---

## 7. Where the brief and Gmail disagree, and what won

### 7.1 Trashed messages are not 404

The brief says "a trashed or deleted message is `404`". Real Gmail returns a
trashed message from `users.messages.get` with `TRASH` in `labelIds`; only a
**permanently deleted** message is `404`. Doctrine 5 says fidelity beats
convenience, and the brief itself names Gmail's documented behaviour as the
authority, so the simulator models reality:

* `trash_message` → `get` still 200, `TRASH` added, `INBOX` removed;
* `delete_message` → `get` is 404;
* `list` hides `TRASH`/`SPAM` unless `includeSpamTrash=true`.

This matters: a client that treats "gone from `list`" as "deleted" is correct
for trash, and a simulator that 404'd on trash would hide the far more common
real-world shape.

### 7.2 `historyId` is a string

Gmail returns `historyId` as a JSON **string** everywhere it appears in a
resource. The simulator does the same, including inside history records, so a
client that types it as `u64` fails here rather than in production.

### 7.3 A `startHistoryId` from the future

Gmail does not document this. Observed behaviour is that it does **not** 404 —
it returns an empty result with the mailbox's current `historyId`. The
simulator matches that, and the test carries a comment saying it is
observed-not-documented so a future reader knows the confidence level.

---

## 8. Judgement calls to record in the report

1. The seam is the **HTTP request**, not a typed method: `Simulator::handle`
   takes a `SimRequest` and returns a `SimResponse`. Both transports call it,
   so they cannot diverge. The typed façade builds a `SimRequest` too.
2. Batch responses default to **reversed** order. Deterministic, and it breaks a
   zipping client on the first test rather than the first production incident.
3. `snippet` is approximate. Gmail's snippet algorithm is undocumented; the sim
   produces a stable, HTML-escaped, 200-character approximation and the docs say
   loudly that no client may assert on its exact text.
4. `resultSizeEstimate` follows the live observation (returned + 1 when another
   page exists) rather than a true count, because a true count would train the
   client to trust it.
5. MIME structure for `format=full` is walked by this crate's **own** splitter,
   not by `mail-parser`. If the simulator and the code under test shared a MIME
   parser, a bug in that parser would be invisible to both.

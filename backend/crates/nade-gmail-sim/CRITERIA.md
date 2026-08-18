# `nade-gmail-sim` — acceptance criteria and edge-case checklist

Written **before** any code, per the execution doctrine in `docs/PLAN.md`, then
corrected in place by the adversarial self-review pass. Rows the review changed
are marked ✎ and explained in §7.

Every edge case below is either a named test or an `// EDGE:` comment beside the
code. Tests are preferred; the table records which.

**Status: 272 tests, all passing.** 180 unit, 67 `tests/edges.rs`, 7
`tests/determinism.rs`, 7 `tests/hygiene.rs`, 6 `tests/sync_story.rs`, 5 doc.

---

## 1. Scope

A **stateful, in-process simulator of the Gmail REST API v1**. It holds a
mailbox — messages, labels, threads, a history log, a monotonic `historyId` —
and serves every endpoint *consistently from that state*, the way Gmail does.

It exists because the failures that matter in a mail sync are stateful, and a
static stub has no state. A `wiremock` stub cannot express "the second
application of this history page must change nothing", because it has no idea it
is the second.

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
* the **mutating** HTTP endpoints (`messages.modify`, `trash`, `untrash`,
  `delete`). A test drives the world through `Mailbox`/`Simulator` methods, which
  is where the mutation API belongs; a client that never mutates Gmail cannot be
  tested by exposing them, and they would only add untested surface. Same
  reasoning, one level up, as the previous bullet;
* `settings.*`, `messages.import`, `messages.insert`, filters, forwarding,
  delegation, S/MIME;
* real Gmail relevance ranking, stemming, spam classification, and Gmail's own
  categorisation into `CATEGORY_*` (the sim lets a test set those labels
  explicitly instead of guessing them);
* multiple users — one mailbox, addressed as `me` or as its own email address.

**Hard rules**

* **Zero NADE types.** It models *Gmail*. No dependency on `nade-server` or
  `nade-agent-sdk`, ever. — `hygiene::no_nade_dependencies`
* **No shared MIME parser.** If the simulator and the code under test both used
  `mail-parser`, a bug in it would be invisible to both. —
  `hygiene::no_mail_parser_dependency`
* **No wall clock.** Enforced at compile time: `chrono` is taken with
  `default-features = false`, which removes `Utc::now` from the crate's
  dependency surface entirely, plus a source scan. —
  `hygiene::chrono_is_taken_without_its_clock_feature`,
  `hygiene::no_wall_clock_and_no_rng_in_the_library`
* **No RNG.** No `rand`, no `uuid::new_v4`, no `DefaultHasher` (whose output is
  not stable across compiler releases). Ids, page tokens, MIME boundaries and
  batch boundaries all come from counters.

---

## 2. Acceptance criteria

| # | Criterion | How it is checked | Status |
|---|---|---|---|
| A1 | Crate and all targets build clean | `cargo build -p nade-gmail-sim --all-targets` | ✅ |
| A2 | No clippy warnings anywhere, including tests | `cargo clippy -p nade-gmail-sim --all-targets -- -D warnings` | ✅ |
| A3 | Formatted | `cargo fmt --check` | ✅ |
| A4 | All unit, integration and doc tests green | `cargo test -p nade-gmail-sim` | ✅ 272 |
| A5 | Rustdoc builds with no warnings | `cargo doc -p nade-gmail-sim --no-deps` | ✅ |
| A6 | The above pass **twice consecutively** | run the block twice | ✅ |
| A7 | One implementation, two transports | `determinism::http_and_in_process_agree_byte_for_byte` | ✅ |
| A8 | Byte-identical replay | `determinism::the_same_script_twice_is_byte_identical` | ✅ |
| A9 | The sync story the crate exists for | `sync_story::initial_sync_then_incremental_then_replay_changes_nothing` | ✅ |
| A10 | No dependency on NADE crates | `hygiene::no_nade_dependencies` | ✅ |
| A11 | Every mutation appends exactly one history record and moves `historyId` | `mailbox::tests::every_mutation_appends_exactly_one_record_and_moves_history` | ✅ |
| A12 | Wire shapes match Gmail | golden assertions per endpoint + E30–E33 | ✅ |

---

## 3. Endpoint checklist

| Endpoint | Faithful | Simplified, and why it is safe |
|---|---|---|
| `users.getProfile` | `emailAddress`, `messagesTotal`, `threadsTotal`, `historyId` as a **string** | `messagesTotal` counts Trash and Spam too; Gmail's exact inclusion rule is undocumented and no client can depend on it |
| `users.labels.list` | system labels `name == id`, `type: "system"`, **no** counters, **no** visibility fields; user labels carry both visibility fields | label colours are not modelled — NADE does not read them |
| `users.labels.get` | adds `messagesTotal`/`messagesUnread`/`threadsTotal`/`threadsUnread` | — |
| `users.messages.list` | `q` (§4), `maxResults` (default 100, ceiling 500), `includeSpamTrash`, repeated `labelIds` **ANDed**, keyset `pageToken`, `resultSizeEstimate` | ordering is always newest-first; Gmail may relevance-order a text query, which a sync that pages `newer_than:30d` cannot observe |
| `users.messages.get` | `format` = `minimal`/`metadata`/`full`/`raw`, `metadataHeaders`, base64url **padded**, transfer-decoded `body.data`, `attachmentId` on `full` **only** | `snippet` is an approximation — Gmail's algorithm is undocumented, so the *shape* (≈200 chars, collapsed, HTML-escaped) is faithful and the text is not |
| `users.messages.attachments.get` | `size` + `data`, scoped to the owning message | `attachmentId` is reversible rather than an opaque server lookup; not observable on the wire |
| `users.history.list` | all four `historyTypes`, `labelId`, paging, `404` outside the window, exclusive `startHistoryId`, top-level `historyId` = mailbox current on **every** page | records for a type not asked for are stripped from a record that survives on another type |
| `users.watch` / `users.stop` | `historyId` + `expiration`, both strings, 7-day TTL from the sim clock; `stop` is `204` | no Pub/Sub message is actually published — the test drives the webhook itself |
| `users.threads.list` / `.get` | beyond the brief's list; pure reuse, and makes "the thread grew under a cursor" testable | — |
| batch `POST /batch/gmail/v1` | real `multipart/mixed` in and out, `Content-ID` correlation with a `response-` prefix, per-sub-request quota and faults, 100-call cap, one `404` does not fail the batch, **reversed** order by default | — |
| OAuth `POST /token` | `refresh_token` grant, rotation, `invalid_grant` | only the refresh grant; the authorization-code exchange is a human step (PHASE0 H3) |

---

## 4. Query (`q`) support

Implicit AND between terms; `OR`, `|` and `{a b}`; `-` negation; parentheses;
quoted phrases.

Operators: `newer_than:`/`older_than:`, `after:`/`before:`/`newer:`/`older:`,
`label:`, `in:`, `category:`, `from:`, `to:`, `cc:`, `bcc:`, `subject:`,
`is:unread|read|starred|important`, `has:attachment|userlabels|nouserlabels`,
`filename:`, `larger:`/`smaller:`, `list:`, `rfc822msgid:`, and bare words
matched against subject + sender + recipients + decoded body.

### What is measured, and what is only documented

A live read-only probe against the real account, scoped to `newer_than:3d`
(baseline **85** ids) so counts discriminate, settled most of this. **Where the
probe and the documentation disagree, the probe wins.** Four items below were
corrected *away* from a conservative reading that had made the simulator
stricter than Gmail — and a simulator that rejects what Gmail accepts teaches
the client to avoid perfectly good queries, which is its own kind of wrong.

| Behaviour | Status |
|---|---|
| `newer_than:` accepts `h` — `24h` → 35, `1d` → 35 | ✎ **measured**; undocumented, and it works |
| `newer_than:` accepts `d`, `m`, `y` | documented **and** measured |
| `newer_than:1w` → 0, `newer_than:30` (no unit) → 0 | **measured**: unsupported. Refused here |
| `\|` is an OR alias — `(a \| b)` → 6, `(a OR b)` → 6 | ✎ **measured**; documented nowhere |
| Operator **names** fold case — `IS:UNREAD` → 59, `is:unread` → 59 | ✎ **measured** |
| The boolean keyword `or` is case-**sensitive** — `(a or b)` → 0 | **measured**: matched as the literal word |
| `from: x` still filters — `from:chase.com` → 1, `from: chase.com` → 1 | ✎ **measured**: whitespace is skipped |
| `from:` with a genuinely empty argument is a no-op — `newer_than:3d from:` → 85 | **measured**: the full baseline |
| An unknown operator matches nothing, never a `400` — `zzz:qqq` → 0 | **measured** |
| `q=label:` is case-insensitive | **measured** |
| `newer_than:Nd` is a **rolling instant** — `1d` → 35 ≡ `after:<now − 24h>` → 35, exact; again at 30 days (86 ≡ 86) | ✎ **measured** |
| `newer_than:Nm` is a **calendar month floored to midnight UTC** — `1m` → 88 ≡ `after:<2026-07-17 00:00 UTC>` → 88, while `after:<now − 31d>` → 86 | ✎ **measured** |
| A bare date is **midnight UTC** — `after:2026/08/10` → 260 ≡ `<Aug 10 00:00 UTC>` → 260; Pacific gives 257 | ✎ **measured**; the docs say Pacific and are wrong |
| `in:anywhere` overrides `includeSpamTrash=false` — 84 → 93 with the parameter untouched | ✎ **measured** |
| `in:trash` / `in:spam` widen scope the same way — 1 and 122, parameter untouched | ✎ **measured** |
| `labelIds` reaches Spam and Trash too — `labelIds=TRASH` → 1, `labelIds=SPAM` → 122, parameter unset | ✎ **measured** |
| `q=label:` takes the **name**, never the id — `label:Subscriptions` → 500 capped, `label:Label_8725…` → **0** | ✎ **measured** |
| A bare space inside a label name is tolerated — quoted / hyphenated / lowercased / bare-space all → 18 | ✎ **measured** |
| Ranges are half-open `[after, before)` | documented only, via Google's worked example |
| `newer_than:1y` | **unmeasured** — every scope stayed above the 500-id page cap. Modelled as 12 calendar months by analogy with `m` |

**Nothing in the query layer is inferred any more.** Every behaviour is measured
or explicitly marked otherwise.

`includeSpamTrash=false` is **not a guarantee about anything**: it gates neither
`q` nor `labelIds`. It is a floor, never a ceiling.

Two pairs coexist and are easy to conflate.

* Operator **names** are case-insensitive; the boolean **keyword** `or` is
  case-sensitive.
* `newer_than:Nd` and `newer_than:Nm` differ **in kind**, not just magnitude:
  the day form is a rolling instant to the millisecond, the month form is a
  calendar step floored to midnight UTC. Nothing in the documentation hints at
  this, and no amount of reading would have found it — `1m` is not "30d with a
  nicer name", and on some days it reaches further back than `31d` does.

Both pairs are measured, and the simulator implements all four behaviours.

Only `MM/DD/YYYY` is refused despite being documented: `03/04/2004` is genuinely
ambiguous against `YYYY/MM/DD` and Google publishes no disambiguation rule, so
zero results is safer than three confidently wrong months. Note that with dates
now measured as UTC there is no Pacific offset anywhere in this crate, and so no
DST ambiguity window either.

**A malformed query is indistinguishable from an empty inbox.** `messages.list`
answers `200` with no `messages` key; there is no `400` path for `q` at all. A
client must validate `q` itself. `labelIds`, by contrast, *is* validated: it is
case-sensitive and an unknown id is `400 Invalid label: …`.

### `resultSizeEstimate` is unusable

Two careful measurements of the real API disagree, so **no formula reproduces
it**:

* `backend/testdata/live/list.json` — `maxResults=500`, 500 rows, a
  `nextPageToken`, estimate **501**;
* the live probe — 85 rows, no next page, estimate **201**, and *pegged at 201
  for every query tried*, including the empty one and including queries whose
  real counts were 1, 6, 35 and 59.

The simulator therefore defaults to `ResultSizeEstimate::Saturating`
(`2 × maxResults + 1` when anything matched, `0` when nothing did), which can
**never** equal the number of rows returned. A client that reads the field as a
count, sizes a progress bar from it, or compares it between two queries fails on
its first call instead of on its first large mailbox. `PageBased` reproduces the
`list.json` shape for a test that wants to pin that fixture.

Related, and worth knowing when designing any count comparison: **a single page
caps at 500 ids**, so two mailboxes of different sizes both hit the ceiling and
look identical.

Not modelled, and safe: stemming, synonyms, `AROUND`, wildcards, Gmail's own
categorisation. Bare-word matching is whole-token with no stemming — stricter
than Gmail, so a client cannot pass by accident on a partial word.

## 5. Edge-case checklist

The brief's minimum list is E1–E13; E14–E60 came out of the design and E61–E65
out of the adversarial self-review.

| # | Edge case | Expected behaviour | Covered by | ✓ |
|---|---|---|---|---|
| E1 | Empty mailbox | `list` → `resultSizeEstimate: 0` with **no** `messages` key; totals 0; history empty | `empty_mailbox_lists_nothing_and_says_so_the_way_gmail_does` | ✅ |
| E2 | A single message | one row; every format renders it | `single_message_renders_in_every_format` | ✅ |
| E3 | Pagination exactly on a boundary | `maxResults == len` → **no** token; `len-1` → page 2 holds exactly the last row, and the two pages equal the single page | `pagination_on_the_exact_boundary` | ✅ |
| E4 | A message deleted mid-pagination | the row the **cursor names** is deleted; page 2 skips nothing and repeats nothing | `message_deleted_between_pages_skips_nothing_and_repeats_nothing` | ✅ |
| E5 | Replayed history page | the same bytes come back, and applying them twice changes nothing | `initial_sync_then_incremental_then_replay_changes_nothing` | ✅ |
| E6 | `historyId` far in the future | empty result + the mailbox's real `historyId`, **not** a 404 (observed, §7.3) | `history_id_from_the_future_is_empty_and_not_an_error` | ✅ |
| E7 | `historyId` far in the past | `404` with Gmail's exact body | `history_id_older_than_the_window_is_a_404_with_gmails_body`, `an_expired_cursor_forces_a_full_resync_and_the_sweep_finds_the_drift` | ✅ |
| E8 | A label removed twice | second removal is a no-op: no record, `historyId` unchanged | `a_label_removed_twice_writes_history_once` | ✅ |
| E9 | Batch of 1 | still `multipart/mixed`, still correlated | `batch_of_one` | ✅ |
| E10 | Batch of 100 | all 100 correlate; 100 sub-requests logged and charged | `batch_of_one_hundred` | ✅ |
| E11 | Batch where every sub-request fails | outer status **200**, five `404` parts | `batch_where_every_subrequest_fails_is_still_a_200` | ✅ |
| E12 | Unicode and RFC-2047 headers | `raw` byte-identical; `full` headers stay **encoded**; snippet HTML-escaped; the index still finds the decoded word | `unicode_and_rfc2047_survive_intact` | ✅ |
| E13 | An 8 MB message | every format, nothing truncated, `attachments.get` returns the exact bytes | `an_eight_megabyte_message_renders_in_every_format` | ✅ |
| E14 | Trashed message | **not** a 404 — `get` returns it with `TRASH`; gone from `list` unless `includeSpamTrash` (§7.1) | `a_trashed_message_leaves_the_listing_and_stays_gettable` | ✅ |
| E15 | Permanently deleted message | `404` | same test, second half | ✅ |
| E16 | `pageToken` from another query | `400 invalidArgument` | `a_page_token_is_bound_to_its_query` | ✅ |
| E17 | Garbage `pageToken` | `400`, never a panic and never page 1 | `a_garbage_page_token_is_a_400_and_never_page_one` | ✅ |
| E18 | `maxResults` 0 / absent / junk / huge | default 100, ceiling 500 | `max_results_clamps_the_way_gmail_does` | ✅ |
| E19 | History page boundary | no record dropped, none repeated, id order across pages | `history_paging_never_drops_or_repeats_a_record` | ✅ |
| E20 | `historyTypes` filter | a record with no matching change is omitted entirely | `history_types_filter_omits_records_with_nothing_to_say` | ✅ |
| E21 | Two mutations on one message | two records; each stub carries the labels **after** its change | `two_mutations_on_one_message_are_two_records` | ✅ |
| E22 | Thread grows under a cursor | thread count and `historyId` move; page 2 still does not repeat page 1 | `a_thread_grows_under_an_open_cursor` | ✅ |
| E23 | Quota exhaustion | 50 gets pass, the 51st is `429` with `Retry-After` | `quota_ceiling_is_fifty_gets_per_simulated_second` | ✅ |
| E24 | Quota refills with the clock | 20 ms buys exactly one more get | same test | ✅ |
| E25 | 429 with `Retry-After` | header exact; fires on exactly the Nth matching call | `injected_429_carries_retry_after_and_fires_on_exactly_the_nth_call` | ✅ |
| E26 | 403 `rateLimitExceeded` vs `userRateLimitExceeded` vs 429 | three distinct bodies | `the_two_403_rate_limits_and_the_429_are_all_distinguishable` | ✅ |
| E27 | 500 / 503 | exact bodies; 503 may carry `Retry-After` | `five_hundred_and_five_oh_three_look_like_gmails` | ✅ |
| E28 | Expired access token → refresh → success | `401 authError`, then a rotated token pair, then 200; the old refresh token dies | `expired_token_then_refresh_then_success` | ✅ |
| E29 | `invalid_grant` on refresh | `400 {"error":"invalid_grant"}` — the `needs_reauth` trigger | `invalid_grant_on_refresh_is_the_needs_reauth_path` | ✅ |
| E30 | `historyId` is a **string** everywhere | asserted on profile, message, history record, watch | `the_string_and_number_fields_are_the_way_gmail_types_them` | ✅ |
| E31 | `internalDate` is a **string** of epoch millis | same test | ✅ |
| E32 | `sizeEstimate`, `resultSizeEstimate`, `body.size` are **numbers** | same test | ✅ |
| E33 | `resultSizeEstimate` is an estimate | 500 rows + token → 501, matching `testdata/live/list.json` | `render::tests::result_size_estimate_matches_the_live_sample` | ✅ |
| E34 | Batch response order | **reversed** by default; every id still lands on its own body | `batch_responses_come_back_out_of_order_by_default` | ✅ |
| E35 | Batch sub-request with a bad path | that part is `404`, the rest succeed | `an_unroutable_batch_subrequest_is_a_404_part_beside_the_successes` | ✅ |
| E36 | Batch with a duplicate `Content-ID` | both parts returned, both labelled the same; the sim does **not** hide it | `duplicate_content_ids_in_a_batch_are_passed_through_not_hidden` | ✅ |
| E37 | Empty / non-multipart batch body | `400` | `an_empty_or_malformed_batch_is_a_400` | ✅ |
| E38 | Batch over 100 calls | `400 invalidArgument`, whole batch refused | `batch_over_one_hundred_is_rejected_whole` | ✅ |
| E39 | `format=metadata` + `metadataHeaders` | only the named headers, case-insensitive; no `parts` | `metadata_headers_filters_and_metadata_carries_no_parts` | ✅ |
| E40 ✎ | `format=full` body data | base64url **padded**, transfer-encoding already decoded, attachment parts carry `attachmentId` and no `data` | `render::tests::full_carries_the_tree_with_decoded_data`, `…an_attachment_part_has_an_id_and_no_data` | ✅ |
| E41 ✎ | `format=raw` | base64url **padded** of the exact stored bytes; `payload` absent | `render::tests::raw_is_the_exact_bytes_and_omits_payload` | ✅ |
| E42 | Message with no `Date` header | `internalDate` still assigned, from the fallback ladder (corpus case 13/14) | `seed::tests::a_message_with_no_date_header_lands_on_the_fallback_ladder` | ✅ |
| E43 | Mixed CRLF/LF line endings | stored verbatim; `raw` does not normalise (corpus case 25) | `a_message_with_mixed_line_endings_keeps_them` | ✅ |
| E44 ✎ | A label deleted while messages carry it | membership drops everywhere, as **one** record with an entry per message (§7.4) | `deleting_a_label_removes_it_everywhere_as_one_record` | ✅ |
| E45 | Deleting a system label | error, and the label survives | `system_labels_cannot_be_deleted` | ✅ |
| E46 | `mark_read` on a read message | no-op, no record | `mailbox::tests::a_no_op_mutation_writes_no_history` | ✅ |
| E47 | Clock never moves on its own | 400 reads → time, `historyId` and `expiration` all unchanged | `the_clock_only_moves_when_told` | ✅ |
| E48 | `newer_than:30d` boundary | exactly 30 d old is **excluded**, 1 ms inside is included | `the_thirty_day_window_excludes_a_message_exactly_thirty_days_old`, `query::tests::newer_than_excludes_the_exact_boundary` | ✅ |
| E49 | `label:` matching | case-insensitive; system by id, user by display name; `-` for a space | `query::tests::label_matches_system_ids_and_user_names` | ✅ |
| E50 | Unknown `q` operator | free text, never a `400` | `query::tests::an_unknown_operator_is_free_text_not_an_error`, `the_query_operators_the_brief_requires_all_work_over_the_wire` | ✅ |
| E51 | Live corpus absent | seeding returns empty; the test skips loudly | `seed::tests::the_live_corpus_seeds_when_it_is_present` | ✅ |
| E52 | Whole MIME corpus | all 26 load, list, and render in all four formats; `raw` round-trips | `the_whole_mime_corpus_serves_over_the_api_without_losing_a_byte` | ✅ |
| E53 | `watch` expiry | exactly 7 days from the sim clock, as a **string** of epoch millis | `watch_returns_a_seven_day_expiry_and_stop_is_a_204` | ✅ |
| E54 | `stop` without a watch | `204`, twice over | same test | ✅ |
| E55 | Concurrent readers | `Simulator: Send + Sync`; no response is internally inconsistent | `the_simulator_is_send_and_sync_and_reads_never_see_half_a_mutation` | ✅ |
| E56 | HTTP transport port | always `127.0.0.1:0`; two servers coexist | `http::tests::two_servers_get_different_ephemeral_ports` | ✅ |
| E57 | Slow fault | in-process reports `latency` without sleeping; over HTTP it really sleeps so a `reqwest` timeout fires | `a_slow_fault_reports_latency_without_the_in_process_path_sleeping`, `determinism::a_slow_fault_really_waits_over_http_so_a_client_timeout_can_fire` | ✅ |
| E58 | Nth-call fault | fires on exactly the Nth **matching** call, then stops | `fault::tests::nth_call_fires_once_at_exactly_the_right_place`, `…path_conditions_count_only_matching_calls` | ✅ |
| E59 | Insert then delete | both records, in id order | `insert_then_delete_shows_both_records_in_id_order` | ✅ |
| E60 | `startHistoryId` == current | empty, not a 404 | `start_at_the_current_history_id_is_empty_not_a_404` | ✅ |
| E61 ✎ | Malformed base64 in a large body | linear, not quadratic — the naive shrink-and-retry decoder took minutes on 2 MB | `mime::tests::malformed_base64_at_scale_is_linear_not_quadratic` | ✅ |
| E62 ✎ | Top-level `historyId` on a paged history response | it is the mailbox's **current** id on every page; storing it after page 1 loses page 2 | `advancing_to_the_top_level_history_id_after_page_one_loses_records` | ✅ |
| E63 ✎ | `attachmentId` outside `format=full` | absent from `raw`, `minimal` and `metadata` | `no_format_but_full_carries_an_attachment_id` | ✅ |
| E64 ✎ | `historyId` contiguity | jumps by 1/2/3 deterministically, so `+1` cannot look correct | `history::tests::history_ids_are_strictly_increasing_and_not_contiguous` | ✅ |
| E65 ✎ | An attachment id from another message | `404` — attachments are scoped to their message | `an_attachment_id_from_another_message_does_not_resolve` | ✅ |
| E66 | Malformed queries that used to loop forever | 18 pathological inputs all terminate | `query::tests::malformed_queries_terminate_instead_of_looping` | ✅ |
| E67 | Another user's mailbox | `404`; `me` and the authenticated address both route | `another_users_mailbox_is_a_404_and_the_address_form_works` | ✅ |
| E68 | A batch inside a batch | that part is `400`, the envelope is still `200` | `a_batch_inside_a_batch_is_refused` | ✅ |
| E69 | Unbounded MIME nesting | depth-capped; the walk terminates | `mime::tests::unbounded_nesting_stops_instead_of_blowing_the_stack` | ✅ |
| E70 | Absurd clock advances | saturating, never wrapping into the past | `clock::tests::advancing_saturates_instead_of_wrapping` | ✅ |
| E71 | Clock moved backwards | allowed (it models skew); the quota bucket does not go negative | `fault::tests::a_backwards_clock_does_not_poison_the_bucket` | ✅ |
| E72 | 8-bit bytes in a header | become `U+FFFD` in `payload.headers` — the simulator must **not** repair what the client has to (PARSER.md trap 2) | `mime::tests::eight_bit_header_bytes_become_replacement_chars_not_a_panic` | ✅ |
| E73 | Duplicate `Subject` headers | both returned, in order; `header()` takes the first (PARSER.md trap 3) | `mime::tests::duplicate_headers_are_all_kept_in_order` | ✅ |
| E74 ✎ | Thread listing preview | `threads.list` previews the **newest** message; `threads.get` reads oldest-first | `a_thread_listing_previews_its_newest_message` | ✅ |
| E75 ✎ | Unsupported `newer_than:` unit | `1w` and a bare number degrade to free text and match nothing; `h` **is** supported (`24h` == `1d`) | `query::tests::an_unsupported_span_unit_degrades_to_free_text_and_matches_nothing` | ✅ |
| E76 ✎ | A bare date's timezone | midnight **Pacific**, not UTC; epoch seconds exact; `MM/DD/YYYY` refused | `query::tests::a_bare_date_means_midnight_pacific_not_midnight_utc` | ✅ |
| E77 ✎ | Date range inclusivity | half-open `[after, before)` | `query::tests::a_date_range_is_half_open` | ✅ |
| E78 ✎ | `or` vs `OR` vs `IS:` | the boolean `or` is case-sensitive; operator **names** are not; `\|` **is** an OR alias | `query::tests::the_boolean_or_is_case_sensitive_but_operator_names_are_not` | ✅ |
| E79 ✎ | A space after the colon | `from: x` still filters, identically to `from:x` | `query::tests::a_space_after_the_colon_still_applies_the_operator` | ✅ |
| E79b ✎ | A genuinely empty operator argument | `from:` is a no-op returning the unfiltered baseline, and does not swallow a following operator | `query::tests::an_operator_with_a_genuinely_empty_argument_is_a_no_op` | ✅ |
| E80 ✎ | Unknown `labelIds` value | `400 Invalid label: inbox` — case-sensitive, unlike `q=label:` | `an_unknown_label_id_is_a_400_not_an_empty_result` | ✅ |
| E81 ✎ | `category:` mapping | `category:primary` → `CATEGORY_PERSONAL` (there is no `CATEGORY_PRIMARY`); `reservations`/`purchases` have no label id | `query::tests::category_maps_onto_the_label_gmail_actually_uses` | ✅ |
| E82 ✎ | Malformed `q` | always `200` with no `messages` key — indistinguishable from an empty inbox | `a_malformed_query_is_indistinguishable_from_an_empty_inbox` | ✅ |
| E83 ✎ | `resultSizeEstimate` saturates | three queries with counts 40/3/37 all report the same estimate; only a zero-result query reports 0 | `result_size_estimate_saturates_and_cannot_compare_two_queries` | ✅ |
| E84 ✎ | A page caps at 500 ids | `maxResults=1000` yields 500 + a token; two different mailbox sizes both hit the ceiling | `a_single_page_caps_at_five_hundred_ids` | ✅ |
| E85 ✎ | `Nd` vs `Nm` differ in kind | the day form rolls to the millisecond; the month form lands on midnight UTC of the calendar date, and catches mail a 30-day window misses | `day_spans_roll_and_month_spans_land_on_a_calendar_date`, `query::tests::a_day_span_is_a_rolling_instant_and_a_month_span_is_a_calendar_date` | ✅ |
| E86 ✎ | A bare date's timezone | midnight **UTC**, not Pacific — the documented behaviour is simply wrong | `query::tests::a_bare_date_means_midnight_utc_whatever_the_docs_say` | ✅ |
| E87 ✎ | `in:anywhere` vs `includeSpamTrash` | `q` **and** `labelIds` both override the parameter — it gates neither | `in_anywhere_reaches_spam_and_trash_without_the_parameter` | ✅ |
| E88 ✎ | A label id passed to `q=label:` | matches nothing, `200`, no error — and the id is exactly what a program has to hand | `a_label_id_passed_to_q_matches_nothing_and_says_nothing` | ✅ |
| E89 ✎ | Label names with spaces | quoted, hyphenated, lowercased and bare-space spellings all agree; a leftover word stays a search term and a following operator is not swallowed | `every_spelling_of_a_spaced_label_name_agrees` | ✅ |
| E90 ✎ | A zero is never evidence | five different mistakes produce the identical empty `200` | `four_different_mistakes_all_produce_the_same_empty_result` | ✅ |

---

## 6. Determinism protocol

1. Every id, token, boundary and `Message-ID` is a pure function of a counter
   and the mailbox contents.
2. `Clock` is a trait; `TestClock` is the only implementation, and it moves only
   when a test calls `advance` or `set_ms`.
3. Faults fire on counted, matched calls — never on a probability.
4. Page-token fingerprints use a hand-rolled FNV-1a, not `DefaultHasher`, whose
   output is explicitly not stable across compiler releases.
5. `read_dir` output is **sorted** before seeding, because filesystem order
   differs between machines.
6. A8 pins the lot: the same 35-call script against two fresh simulators
   produces byte-identical statuses, headers and bodies, and a third run still
   matches the first.

---

## 7. Where the brief and Gmail disagree, and what won

### 7.1 Trashed messages are not 404

The brief says "a trashed or deleted message is `404`". Real Gmail returns a
trashed message from `users.messages.get` with `TRASH` in `labelIds`; only a
**permanently deleted** message is `404`. Doctrine 5 says fidelity beats
convenience and the brief itself names Gmail's documented behaviour as the
authority, so the simulator models reality:

* `trash_message` → `get` still 200, `TRASH` added, `INBOX` and `UNREAD` removed;
* `delete_message` → `get` is 404;
* `list` hides `TRASH`/`SPAM` unless `includeSpamTrash=true`.

This matters. A client that treats "gone from `list`" as "deleted" is *correct*
for trash, and a simulator that 404'd on trash would hide the far more common
real shape and teach the client to hard-delete rows a user can still restore.

### 7.2 `historyId` is a string

Gmail returns `historyId` as a JSON **string** everywhere it appears. So does
the simulator, including inside history records, so a client that types it as
`u64` fails here rather than in production.

### 7.3 A `startHistoryId` from the future

Undocumented. Observed behaviour is an empty result with the mailbox's current
`historyId`, not a `404`. The simulator matches that and the test says in its
comment that it is observed-not-documented, so a future reader knows the
confidence level.

### 7.4 ✎ Deleting a label is one record, not N

The first draft of this document said "each as its own `labelRemoved` record".
The review changed it: Gmail emits records that touch many messages at once
("mark all as read" is one record with dozens of `labelsRemoved` entries), and a
client that reads `history[0].messages[0]` and stops is wrong in a way only a
multi-message record can expose. `Mailbox::bulk_modify` is the primitive; label
deletion uses it.

### 7.5 ✎ base64url is padded, not unpadded

The first draft said unpadded. Google's own client samples call
`base64.urlsafe_b64decode` directly on `raw`, `body.data` and `attachments.get`'s
`data`, which raises on missing padding — so Gmail pads. The simulator pads, and
its decoder accepts both.

### 7.6 ✎ Documented is not the same as true

Three rounds, and the through-line is the finding.

**Round one, from the published docs.** Six places where the first draft was
*more permissive* than Gmail. The worst was `from: x` with a space: an operator
with an empty argument `contains("")`, so a stray space became a silently
unfiltered sync.

**Round two, a live read-only probe**, which overturned four of those
corrections for over-shooting into being *stricter* than Gmail:

| Corrected to | Measured |
|---|---|
| `h` refused as undocumented | `24h` → 35 = `1d` → 35. It works |
| `\|` refused as undocumented | `(a \| b)` → 6 = `(a OR b)` → 6. It is an OR |
| `from: x` made free text | `from: chase.com` → 1 = `from:chase.com` → 1. Still filters |
| all keywords case-sensitive | `IS:UNREAD` → 59 = `is:unread` → 59. Only the boolean `or` is case-sensitive |

**Round three, four more probes**, which settled the date semantics and
overturned one more documented behaviour:

| Was | Measured |
|---|---|
| `newer_than:Nd` boundary unknown | a **rolling instant**: `1d` → 35 ≡ `after:<now − 24h>` → 35, exact |
| `m` = 30 days | a **calendar month floored to midnight UTC**: `1m` → 88 ≡ `after:<2026-07-17 00:00 UTC>` → 88, while `after:<now − 31d>` → 86 |
| bare dates = midnight Pacific (documented) | **midnight UTC**: `after:2026/08/10` → 260 ≡ `<Aug 10 00:00 UTC>`; Pacific gives 257 |
| `in:anywhere` unmodelled | it **overrides** `includeSpamTrash=false`: 85 → 94 |

So: **five corrections were made from the documentation, and measurement
reversed every one of them.** The space-after-colon, the pipe, the `h` unit, the
case folding, and the timezone. Five for five.

Two rules follow, and both are written into the module docs:

1. *Undocumented is not the same as absent.* `h` and `|` work and appear
   nowhere in Google's tables.
2. *Documented is not the same as true.* The filtering guide states plainly that
   dates are interpreted as midnight Pacific. They are not.

**Round four** closed the remaining items. `in:trash`/`in:spam` were confirmed
to widen scope exactly as inferred, `labelIds` was found to reach Spam and Trash
as well, and `q=label:` was found to take the name and never the id — silently,
which earned it a client-bug case of its own.

For anything on the sync path the API is the authority and the documentation is
a hypothesis. Every §4 row now carries its status — **measured**, documented
only, or unmeasured — so a later reader can see exactly how much weight each
behaviour bears. **No inferences remain in the query layer.**

### 7.6.1 A zero is never evidence

A third rule, learned from the probe rather than from the API. On this API an
unknown operator, a real label with no mail, a label addressed by id instead of
name, an unsupported unit and a malformed query all return the **identical**
empty result, and no `400` is ever raised for any of them.

The probe was caught by exactly this: its first pass at the label-spelling
measurement returned 0 for every spelling and looked like all four failing. The
label simply had no mail. It was only caught by checking the label had messages
before drawing a conclusion.

So any simulator behaviour justified by "the live API returned nothing" needs a
**positive control** before it can be believed — a query known to match, run
against the same data, in the same session.
`tests/edges.rs::four_different_mistakes_all_produce_the_same_empty_result` pins
all five paths side by side with a working control, so the ambiguity is
documented as a property of the API rather than rediscovered by the next person.

### 7.7 ✎ `format=raw` carries no `attachmentId`

Confirmed against the P2 backend, which mints its own part-derived attachment
ids at parse time and resolves them with a separate `format=full` fetch, exactly
because a raw response has no `payload` and therefore no Gmail ids at all. The
simulator emits `attachmentId` on `format=full` only.

---

## 8. Judgement calls to record in the report

1. **The seam is the HTTP request, not a typed method.** `Simulator::handle`
   takes a `SimRequest` and returns a `SimResponse`; both transports call it, so
   they cannot diverge, and the in-process path still exercises URL building and
   query encoding.
2. **Batch responses default to reversed order.** Deterministic, and it breaks a
   zipping client on its first test rather than in its first incident.
3. **`snippet` is approximate.** Gmail's algorithm is undocumented; the shape is
   faithful and the text is not, and the rustdoc says no client may assert on it.
4. **`resultSizeEstimate` follows the live observation** (returned + 1 when
   another page exists) rather than a true count, because a true count would
   train the client to trust it.
5. **MIME structure is walked by this crate's own splitter.** A shared parser
   would make its bugs invisible to both sides.
6. **Quota is off by default.** Most tests are not about quota, and a bucket
   that silently throttled them would make every failure ambiguous.
7. **Auth is enforced by default.** It is what Gmail does, and the refresh path
   is only reachable from there. `AuthMode::Off` exists for tests about
   something else.
8. **The mutating HTTP endpoints are absent**, and the mutation API lives on
   `Mailbox`/`Simulator` instead — see §1.

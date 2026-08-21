# Contract fixtures

The canonical wire bytes for every `/v1` endpoint in [`../API.md`](../API.md).
Backend tests serialise their real response types and compare against these
files; iOS tests decode the same files into the app's `Codable` types. Drift is
a failing test on both sides rather than a runtime surprise.

**These files are generated. Never hand-edit them.**

```sh
python3 docs/contract/generate.py && python3 docs/contract/validate.py
```

`generate.py` builds **one** Python object graph — one account, one label list,
seven threads, four agents, seven runs, six feed items, two notes, one draft —
and serialises every fixture from it. A cross-reference cannot disagree with
itself because it is the same string. `validate.py` then re-checks the output
against `API.md` and exits non-zero on any violation.

Hand-authored fixtures drift: the previous set had a run that was
simultaneously `pending_approval` and `done`, a draft attributed to an agent
without `draft_reply`, a feed item citing a note no file defined, and
"deterministic" effect ids that were actually v4. Every one of those is now a
validator failure.

A shape change therefore goes: **`API.md` → `validate.py` → `generate.py`**.
Editing the JSON fails `validate.py`'s canonical-formatting check on the next
run.

## Conventions (`API.md` §0)

- **Time** ISO-8601 UTC, second precision, always `Z`: `2026-08-16T09:12:04Z`.
  Never an offset, never fractional seconds.
- **Nullability is explicit.** A field marked `|null` is always present and may
  be null. Nothing is ever omitted, so both sides can use non-optional decoding
  wherever the contract says non-null. **One licensed exception:** journal
  payloads are the SDK engine's own serialization, byte-faithful, and the
  fields the SDK omits when absent (serde `skip_serializing_if`) are absent,
  not null — API.md §6.1 marks them "may be absent".
- **Pagination is per-endpoint.** `next_cursor: string|null` appears **if and
  only if** the endpoint is paginated. Bounded collections — mailboxes, agents,
  settings — carry no cursor field at all. Cursors are opaque base64url keyset
  tokens; clients must not parse them. Empty collections return an empty array
  **and** `next_cursor: null`, never `404`.
- **Ids** UUIDs are lowercase and hyphenated. Gmail message and thread ids are
  16 lowercase hex; label ids are `INBOX`-style or `Label_<n>` (both the short
  `Label_12` and long `Label_1901000275082506782` forms appear on purpose).
  Bearer tokens are `nade_` + 64 hex, 69 characters total.
- **The journal is the SDK engine's, verbatim.** `run_journal` has one author
  — the engine, via the host's `Journal` driver — so the run fixtures carry
  the SDK vocabulary (`run_started`, `model_response`, `step_started`,
  `step_done`, `approval_requested`, `approval_resolved`, `run_waiting`,
  `run_woken`, `cap_breached`, `run_ended`) with payloads exactly as
  `nade-agent-sdk/src/journal.rs` serialises them. Host facts — the feed item,
  its token, its deadline — never appear in a journal; an approval reaches it
  only as an engine-written `approval_resolved`. A failed tool call is a
  `step_done` with `is_error: true`; there is no separate failure kind.
- **Journal steps are identified by `step_seq`.** Every entry has its own
  `seq`, +1, no gaps — `(run_id, seq)` is a primary key, so sharing one was
  never possible. A *step* is identified by the seq of the entry that
  **opened** it: the `step_started` itself for an ungated call, the
  `approval_requested` for a gated one (each carries its own seq as
  `step_seq`). `step_done` and the `step_started` that executes an approved
  gate carry that opening seq as `step_seq`, so steps correlate by pointer —
  guessing at "the nearest preceding open with the same tool" breaks the
  moment an agent calls a tool twice, which is ordinary.
- **Effect ids are `uuid5`.** `uuid5(EFFECT_NAMESPACE, "<run-id>:<seq>")` under
  the frozen namespace `6e616465-5f65-6666-6563-745f6e737631` — the ASCII bytes
  of `nade_effect_nsv1`, defined in
  `backend/crates/nade-agent-sdk/src/ids.rs`. The seq is the **opening** seq
  and nothing else, so a gated step keeps the id minted at
  `approval_requested` through approval and execution — which is what makes
  re-execution after a crash upsert rather than duplicate. **Anything an agent
  wrote has a v5 id; anything else has v4.** `validate.py` recomputes every one
  of them and cross-checks the namespace against the Rust golden vector.
- **The world is frozen.** No clocks, no randomness: every timestamp is a
  constant derived from one base day (2026-08-16/17) and every id is a literal
  or a `uuid5` derivation. `validate.py` parses `generate.py` and fails it for
  importing anything that could read a clock or an RNG.

## What the world contains

Ordering satisfies causality throughout: a run starts after the mail that
triggered it, its journal entries follow, its feed item follows those, and its
resolution follows that.

| Agent | Status | Trigger | Tools | Runs |
|---|---|---|---|---|
| Job Search Tracker | published | mail | `search_mail` `read_thread` `write_note` | pending (note), skipped, failed, expired |
| Morning To-Do | published | schedule | `search_mail` `read_thread` `write_note` | done → note |
| Reply Drafter | draft | mail (run by hand) | `read_thread` `draft_reply` | done → draft, pending (draft) |
| Flight Watcher | draft | — | none | none (`spec: null`, `compile_error` set) |

A run that has not reached `done` has **not** written its effect, so its note or
draft appears in no list — but its id is still exact, and is what the feed item
publishes so the client can deep-link straight after approving. Every run whose
feed item publishes such an id has a journal fixture, so `validate.py`
recomputes the id rather than trusting it.

## Files

| Fixture | Endpoint / case |
|---|---|
| `pair.json` | `POST /auth/pair` |
| `me.json`, `me_needs_reauth.json` | `GET /me`, both statuses |
| `mailboxes.json` | `GET /mailboxes` — 8 system labels mapped and ordered, `[Gmail]` and flag labels filtered out |
| `threads.json` | `GET /mailboxes/{id}/threads`, first page (`next_cursor` set) |
| `threads_last_page.json` | same, terminal page (`next_cursor: null`) |
| `threads_empty.json` | same, empty |
| `thread.json` | `GET /threads/{id}` — 3 messages, one with no `text/html` part, inline + regular attachments, 3 agent cards incl. one with `feed_item_id: null` |
| `thread_html_only.json` | same, message with no `text/plain` part; `to: []`, `agent_cards: []` |
| `thread_partial.json` | `partial: true` — the detail could not be completed, so it carries **one** message where the list row counts two, and an `expired` agent card (the only fixture that renders that status). Added at P2: API.md has always said clients must surface this state and nothing had ever serialised it |
| `search.json`, `search_empty.json` | `GET /search` |
| `notes.json` | `GET /notes` — one agent note (v5) and one written outside a run (v4, `agent_name: null`) |
| `notes_empty.json` | same, empty |
| `note.json` | `GET /notes/{id}` — always `unread: false`, the state *after* the read |
| `drafts.json`, `drafts_empty.json` | `GET /drafts` |
| `draft.json` | `PATCH /drafts/{id}` — the same draft after a body-only patch |
| `ask_request.json` | `POST /ask` body, `route_hint: null` (the normal case) |
| `ask_request_route_hint.json` | `POST /ask` body with the route forced |
| `agents.json`, `agents_empty.json` | `GET /agents` (list rows; no `allowed_tools`) |
| `agent.json` | `GET /agents/{id}` — published, mail-triggered |
| `agent_scheduled.json` | same, published + `schedule` |
| `agent_draft.json` | same, `status: "draft"`, output kind `draft` |
| `agent_compile_failed.json` | same, `spec: null` + `compile_error`, spans null |
| `runs.json`, `runs_empty.json` | `GET /runs` — all six runs, newest first |
| `run.json` | `GET /runs/{id}` — `pending_approval`, gated on `write_note` |
| `run_pending_draft.json` | `pending_approval`, gated on `draft_reply` — the Edit card's run |
| `run_done.json` | `done` — gated step, `approval_resolved {decision: approve}`, effect written under the approval's id |
| `run_failed.json` | `failed` — `step_done {is_error: true}` then `run_ended {reason.cap: cancelled}` (the host's loud-fail), `error` set |
| `run_skipped.json`, `run_expired.json` | the two runs whose approved effect will never be written |
| `feed.json` | `GET /feed` — two live approvals, unseen info, resolved, skipped, expired; `new_count` counted |
| `feed_empty.json` | same, empty |
| `feed_item.json` | `GET /feed/{id}` — the live approval, with its token |
| `feed_item_info.json` | same, an `info` item |
| `feed_item_editable.json` | same, the live `draft_reply` approval — the only card that renders `["approve", "edit", "skip"]` |
| `approve.json`, `skip.json`, `seen.json` | feed actions |
| `settings.json` | `GET /settings` |
| `ask_answer.sse` | `POST /ask`, `route.kind = answer` |
| `ask_results.sse` | `results` — tokens before the hits |
| `ask_agent_draft.sse` | `agent_draft` — separate `when_span` / `do_span` |
| `ask_error.sse` | terminal `error` after partial tokens; no `done` follows |
| `error_*.json` | one per code in `API.md` §0 (13 files) |
| `healthz.json`, `healthz_db_down.json` | `GET /healthz` |

SSE files are exact wire bytes: `event: <name>\ndata: <json>\n`, a blank line
between events, `route` first, exactly one terminal `done` **or** `error`, and a
trailing blank line.

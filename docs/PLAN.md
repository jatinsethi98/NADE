# NADE — Combined Build Plan v3

One plan, one executor: a single orchestrating AI agent on this Mac, fanning out subagent teams per phase. Lanes marked [backend] [ios] [sdk] may run as parallel subagents only where marked. Every acceptance criterion is a command, never a look.

Draft 3 folds in a third adversarial review (findings C1–C10, appendix) and two directives: **test-speed scoping** (sync 30 days, dev caps everywhere, no bloat) and the **execution doctrine** below.

## Where we are — 2026-08-21

| Phase | State |
|---|---|
| P1 Foundations | ✅ 2026-08-17 |
| P2 Mail lands | ✅ 2026-08-19 |
| P3 Mail stays current | ✅ 2026-08-20 — backend live; the iOS screens run on fixtures until P4/P5 serve them |
| P4 First runs | ✅ 2026-08-21 — live against real mail and a real model |
| P5 The loop closes | next |
| P6–P8 | not started |

Green at `8b05de1`: 785 Rust tests, 258 iOS unit, 46 iOS UI, each twice
consecutively. 38 screenshots in `docs/screens/`, all distinct by hash.

**2026-08-21 architecture checkpoint.** Three corrections landed before P4
opened. The multi-account **resolution layer** is in: accounts are
device-bound, and quota buckets, the triage scheduler, webhook routing and
settings all resolve per account — v1 still ships **one mailbox per device**;
what changed is that nothing assumes a singleton anymore. The **journal
contract was rewritten to the SDK's own vocabulary** (`API.md` §6.1: the ten
`journal.rs` kinds, payloads verbatim, engine as the journal's only author —
the previous §6.1 described entries no engine would ever write and the P5
approve tx would have appended a kind SDK replay refuses as corrupt). And the
**P4–P8 phase lists below were completed** with the work other documents
already assigned them — the Anthropic adapter, the `llm_calls` ledger,
`/settings` and the P7 screens, the P6 `done.sources.thread_id` fix — so no
lane discovers its scope mid-phase.

A Codex cross-model review of the finished diff returned nine findings; seven
were folded — a persisted `gmail_tokens.generation` so a stale refresh cannot
undo a fresh re-consent (migration 0004, D49), revocation checked inside the
consent tx, the deviceless dev-consent guard, whole-second engine stamps
(`Entry::new`), `validate.py` forbidding `approval_requested.expires_at`, the
iOS stale-detail clear in `observe(id:)`, and the retired singleton language in
these docs — and two rejected: byte-identical 409 page copy (the new sentence
states the new rule) and pre-computing real tool fingerprints (P4 owns that,
named below). Checkpoint gates: **800** Rust tests and **334 + 46** iOS tests,
each green twice consecutively; `validate.py` green twice. The iOS perf pass
of the same checkpoint is D98–D105.

## Caveats — read before you test

- **The Ask tab shows fixtures, not your mail.** 2a, 1a, 1b, 1c and 1d are
  built and correct, but `/agents` lands at P4 and `/feed` at P5. Against the
  live server they say "This server doesn't offer that yet." — loudly and on
  purpose, rather than showing an empty list.
- **Push stops whenever the tunnel restarts.** A quick tunnel's hostname is
  ephemeral and `NADE_PUSH_AUDIENCE` is read once at boot, so after any restart
  run `just tunnel` and restart the server. Until you do, mail still arrives —
  just up to 30 minutes late, on the poll fallback.
- **Gmail consent dies every 7 days.** The OAuth app is in Testing mode.
  `scripts/bench.sh` prints the re-consent command as soon as `/v1/me` stops
  being ok, so you find out at the start of a session rather than mid-test.
- **A new tunnel hostname needs one manual paste** into the OAuth client's
  redirect URIs — `gcloud` cannot set them. `just tunnel` prints the line. Only
  the Gmail link flow cares; ordinary sync does not.
- **Every LLM call is Haiku**, via `NADE_LLM_MODEL` / `NADE_LLM_COMPILE_MODEL`.
  That puts the spec compiler on the cheap tier on purpose. If compiled specs
  come out weak on ambiguous sentences, this is the first knob to turn at P4.
- **Secrets live only in `backend/.env`** (gitignored, mode 0600). A new var
  added to `.env.example` must also go into `config::ENV_VARS`, or the config
  test fails in both directions.
- **`scripts/bench.sh` is the bench**, not `-NADESeed`: one command starts the
  backend, builds, installs, pairs and deep-links against the *live* server.
  `docs/BENCH.md`. `cargo` is not on the default PATH.
- **Simulator only so far.** Nothing has run on a physical phone; H9 (the $99
  program and an APNs key) is still undecided and only device push needs it.

## Execution doctrine (applies to every task)

1. Before writing code, the assigned agent writes the task's acceptance criteria AND an edge-case checklist (minimum: empty input, unicode, crash mid-step, duplicate delivery/replay, expiry, pagination boundary, 429/timeout, clock skew where relevant).
2. Implement. Then adversarially self-review against the checklist. Loop until every criterion and edge case passes — the goal is completely right in one shot, not iterated in review.
3. A task is done when its acceptance command passes twice consecutively (flake check) and the checklist is committed next to the code (`// EDGE:` comments or a test per case).
4. Scope discipline: build to the testable level, no further. Dev caps below are law.

**Dev caps (config, enforced in code):** sync window `newer_than:30d`, `MAX_SYNC_MESSAGES=2000`, triage ≤20 msgs/agent/day, LLM spend ceiling $1/day default, run `max_steps=12`, per-run token budget 50k. The spend ceiling and the triage cap are **per account** — they read the per-account `llm_calls` ledger (P4), not a process-wide counter.

---

## Product contract (fixes scope parity, auth, compliance, approval + schedule semantics — settle before code)

### v1 definition

v1 takes **no outbound actions**. Agents observe, search, take notes, and prepare drafts; drafts live in NADE, never in Gmail. Mail IS stored on your own server and excerpts ARE sent to the LLM APIs you configure — the app discloses this in Settings ("Your mail syncs to your own server and is processed by the AI models you connect"). Personal use under Google's Testing mode (≤100 users) is compliant. ANY public distribution — even read-only — triggers restricted-scope verification + CASA Tier 2; send scopes add more. Post-v1, priced then.

### Design parity map (design = target state; v1 ships the subset below; UI copy must match what ships)

| Design shows | v1 ships | Path back |
|---|---|---|
| 2 accounts, account rows (1g) | Single account; accounts section shows one row; "Add mailbox" hidden | Post-v1 multi-account |
| Smart mailboxes + "＋ New" (1g, 1e chips) | Gmail system categories + user labels as chips; no rule builder | Post-v1 smart mailboxes |
| OTP chip with Copy (2a) | Generic `info` feed item; no OTP extraction | Post-v1 feed kind `otp` |
| Notes tab, agent-written notes (1h/1i) | **Live read-only**: list + markdown detail from `GET /notes` (agents write via `write_note`); no editing | Post-v1 editor |
| Calendar tab (1j) | Stub. **No endpoint exists**; the tab renders `NADE/Fixtures/calendar.json`, bundled in the app. "Add event" hidden | Post-v1 |
| Schedule sheet (1d) has no time or timezone control | v1 **adds an "At" time row** — the model carries `at` and the parent sentence renders "at 8:00", so the mockup simply has a gap. Timezone is captured from the device; still no control | Post-v1 tz picker |
| Notes rows show four metadata shapes and a gold rule that means "selected" in the live mockup | One shape, and the gold rule means **an agent edit you have not opened** — which is what the mockup's own caption says. `GET /notes` gained `agent_name` and `unread` to make it renderable | — |
| Settings shows a global approval default and a Run log with no API behind them | `GET/PATCH /settings` and `GET /runs` added | — |
| Thread footer reads "Filed in {mailbox} · {account}" with no source | `GET /threads/{id}` gained `mailbox_name` and `account_email` | — |
| Settings footer: "mail stays on device between runs" | **False for v1.** Replaced by the server-supplied `disclosure` string, so the honest sentence cannot drift from what the server does | Permanent |
| Tools: Notion, Slack, Web access, Calendar (1c) | v1 tools are mail-internal: `search_mail`, `read_thread`, `write_note`, `draft_reply`. Builder shows only real tools | Post-v1 integrations |
| "Approve" forwards/sends (2a, 1f) | Approve confirms **local** effects (save draft / write note). Card copy says what happens: "Save draft", never "Send" | Post-v1 send upgrade |
| Lucide icons (DS) | SF Symbols, nearest glyph per icon | Recorded deviation; swap pass post-v1 |
| Cormorant Garamond + Lora via Google CSS | Same faces, OFL TTFs downloaded into the repo at P1 and bundled | Permanent |

### Auth bootstrap (was unauthenticated)

- Server startup prints a one-time 6-digit pairing code (also `just pair` reprints). `POST /v1/auth/pair {code, device_name}` → `{token}`. Code is single-use, 10-min TTL. Tokens are opaque 32-byte, hashed at rest, revocable (`devices` row).
- Dev shortcut: `NADE_TOKEN` env accepted when `NADE_ENV=dev`.
- Gmail OAuth (web client) uses **PKCE + state** via the `oauth2` crate; the callback upserts the Gmail account's `accounts` row and **binds the initiating device to it** (`devices.account_id`). One mailbox per device; a bound device is refused a different mailbox.

### Approval semantics (atomic, fetch-through-session)

- APNs payload carries **only** `feed_item_id` + alert text. No capability tokens in push.
- Deep link: `GET /v1/feed/{id}` (authenticated) returns the item incl. `approval_token` while status=`new`.
- `POST /feed/{id}/approve {approval_token}` runs ONE transaction, five writes: validate+consume token → run `pending_approval→queued` with approved `pending_action` → feed item `resolved` (+`resolved_note`) → audit row → job enqueued. **The tx never touches `run_journal`** — the journal's only author is the engine (API.md §6.1), and `approval_resolved` lands when the resume job runs `Engine::resume`. 409 `token_consumed` on replay (client treats as success), 410 when expired.
- Edit path (design's Edit button): approve creates/updates the draft; `PATCH /v1/drafts/{id}` edits it after; thread card links to the draft. No separate pre-approval edit flow in v1.

### Schedule model (was next_run_at only)

`agents.schedule` jsonb: `{"freq":"day"|"week"|"month","interval":1,"byweekday":["mon",…],"bymonthday":1,"at":"08:00","tz":"America/Phoenix","ends":{"kind":"never"|"on"|"after","date"?,"count"?},"runs_done":0}`. `bymonthday` is freq=month only, capped at 28 so a monthly schedule never silently skips February, `-1` = the actual last day; semantics in API.md §5.2. `next_run_at` is derived: computed with `chrono-tz` in the schedule's tz, wall-clock semantics (DST: keep local time; skipped/ambiguous instants roll to next valid). Recomputed after each run; `ends` enforced (`after` counts `runs_done`, then agent→`paused`). API exposes the jsonb verbatim; the 1d sheet maps 1:1.

### Ask routing (one field, three intents)

Everything from the unified field goes to `POST /v1/ask`. Server classifies (heuristics first: quoted strings / `from:` → search; imperative "when…"/"every…" → agent; else cheap model) and streams a typed SSE session: first event `route {"kind":"answer"|"results"|"agent_draft"}`, then `token`/`done` for answers, `results {threads:[…]}` + `done` for searches, or `draft {name, nl_definition, when_span, do_span, trailing, tool, approval_required, status}` + `done` for agent drafts (API.md §4 — the two spans are separate fields because the design underlines exactly them) — client renders the 1a states accordingly; saving the draft POSTs `/v1/agents`. `GET /v1/search` remains as the whole-mailbox search endpoint. **It has no
screen in v1**: `DESIGN.md` §1e draws no search field on the mail list, and the
mockup's only search pill is 1h Notes'. The sentence that used to promise "the
Mail tab's plain search box" described a screen that does not exist in the
design; the endpoint is reached through Ask and by the agents' `search_mail`.

### Recorded deviations

(1) No Redis — Postgres only. (2) No `genai` — hand-rolled `Llm` adapters (OpenAI-compat + Anthropic). (3) No `a2` — APNs via reqwest + jsonwebtoken (ES256). (4) crates.io publish post-v1. (5) Dev database via `postgresql_embedded` (portable binaries, no Docker until P8); fallback Docker if it misbehaves. (6) SF Symbols for Lucide (parity map).

---

## Phase 0 — HUMAN REQUIRED

**Live status lives in `docs/PHASE0.md`.** As of 2026-08-21: H1, H2, H3, H6
and H7 are **done** — H6's subscription is now created or updated by
`just tunnel` on every run, and the Anthropic key is in `backend/.env`. H4 was
**dropped** at P3: a quick tunnel needs no login, at the cost of a hostname
that changes each session. H8 is done once (the 4-second push proof) and comes
back at P5. Left: H5 (the weekly consent click), H9 (optional, device push
only) and H10 (P8 deploy).

The table below is the original definition; `PHASE0.md` supersedes it.



Self-installed by the orchestrator, no human: rustup/cargo (running now), `cloudflared` (static binary download), Postgres (embedded binaries), font TTFs, Swift packages. Verified present: Xcode 26.6 + 5 iPhone simulators.

| # | Action (exact) | Unblocks |
|---|---|---|
| H2 | GCP Console (project `deliveriesapp-293223`): enable **Gmail API** + **Cloud Pub/Sub API**. Create topic `gmail-events`. On the topic: add `gmail-api-push@system.gserviceaccount.com` → **Pub/Sub Publisher**. | P3 push |
| H3 | GCP Console: create OAuth client, type **Web application**, redirects `http://localhost:8080/v1/auth/gmail/callback` + later `https://<domain>/…`. JSON → `backend/secrets/web_client.json`. | P2 |
| H4 | `cloudflared tunnel login` browser click when asked. | P3 |
| H5 | OAuth consent click when the orchestrator opens it. Recurs ~weekly (Testing mode). | P2; ongoing |
| H6 | Push subscription on `gmail-events` → tunnel URL, **Enable authentication** with a service account you own; put its email in `.env` as `PUSH_SA_EMAIL`. **Also grant** the Pub/Sub service agent (`service-<project#>@gcp-sa-pubsub.iam.gserviceaccount.com`) **roles/iam.serviceAccountTokenCreator** on that service account — without it authenticated push cannot mint OIDC tokens. | P3 |
| H7 | LLM keys in `backend/.env`: `ANTHROPIC_API_KEY` (+ optional `OPENAI_COMPAT_BASE_URL`/`KEY`). | P4 |
| H8 | ~3×: send a test email to jatinsethi98@gmail.com when asked. | P3, P5 |
| H9 | *Optional, device push only:* Apple Developer $99, App ID `com.jatinsethi.nade` + Push, APNs .p8 → `backend/secrets/apns.p8`. Simulator needs none of this. | P6 device leg |
| H10 | *P8 only:* VPS + DNS + SSH; Docker Desktop install (first GUI launch needs you). | P8 |

---

## Canonical API Contract

**Moved.** The complete, unambiguous contract now lives in **`docs/API.md`** —
every endpoint, every field, every nullability, the error-code table, the
pagination rule per endpoint, the typed journal payloads, the typed feed `data`
shapes, and the SSE grammar. A duplicated summary here would only drift, and an
adversarial review found it already had.

`docs/contract/` holds the fixtures, generated from one coherent world state by
`docs/contract/generate.py` and checked by `docs/contract/validate.py`. The rule
stands: **a shape change touches the fixture first.** Backend tests serialise
their real types against those files; iOS tests decode them.

Things `API.md` settles that this document previously left to the implementer:
the initial status of a created agent (always `draft`), whether `skip` carries a
token (it does), how an `info` feed item ever stops being new (`POST /feed/seen`),
which buttons a feed item offers (an explicit `actions` array), how a message is
identified on the wire (`gmail_id`; the bigint primary key never leaves the
database), what `runs_done` counts, what happens in a DST fold, and which Gmail
system labels become mailboxes and under what display names.


## Architecture

### Repo layout

```
NADE/
├── NADE.xcodeproj, NADE/          # iOS app
├── backend/                       # Cargo workspace
│   ├── crates/nade-agent-sdk/     # generic: llm.rs, tool.rs, journal.rs, engine.rs,
│   │                              #   ids.rs — no HTTP, no DB, no runtime (its own manifesto)
│   └── crates/nade-server/        # axum: api/, gmail/, mail/parse.rs, sync/,
│                                  #   agents/{compile,triggers,tools/}, runtime/ (journal.rs =
│                                  #   the Journal impl over run_journal), llm/ (anthropic.rs),
│                                  #   jobs.rs, push.rs, migrations/
├── docs/PLAN.md, docs/contract/   # this plan + shared fixtures
├── docs/MockUps/                  # design source (Email App.dc.html + Classical DS)
└── docker-compose.yml             # P8 only; dev uses postgresql_embedded
```

SDK rule: compiles with zero NADE types, and equally with zero infrastructure — no HTTP client, no database, no runtime. The Postgres `Journal` driver and the LLM adapters are therefore **host code in `nade-server`** (`runtime/journal.rs`, `llm/anthropic.rs`), not SDK features: a `--features postgres` flag on the SDK was the exact per-crate feature trap D27 documents. Engine caps per run: `max_steps`, token budget; breach → `failed` + feed item.

### Postgres schema

**The migration is the schema.** `backend/crates/nade-server/migrations/0001_init.sql`
is real, executable DDL with foreign keys, `not null` constraints, check
constraints and indexes; the sketch that used to sit here was pseudocode
(`uuid pk`, `bigint identity pk`) that no implementer could execute and that
silently permitted a much weaker schema. It is deleted rather than kept in sync.

The tables: `accounts`, `gmail_tokens`, `messages`, `attachments`, `labels`,
`sync_state`, `agents`, `agent_runs`,
`run_journal`, `notes`, `drafts`, `feed_items`, `audit_log`, `devices`,
`settings`, `jobs`. Ownership columns are `not null` and cascade from
`accounts`; every enum column carries a check constraint; `agents.status`
defaults to `draft`.

Two rules the DDL cannot express, so they live here:

- **Agent-written `notes.id` and `drafts.id` are `uuid5(EFFECT_NAMESPACE,
  "<run-id>:<seq>")`**, with the namespace frozen in
  `crates/nade-agent-sdk/src/ids.rs`. They are written with an upsert, never a
  plain insert, which is what makes re-execution after a crash harmless. Rows
  created any other way are v4.
- **`jobs` claim semantics**: `for update skip locked`, `lease_expires_at`
  heartbeated every 60 s, a reaper that reclaims only expired leases, backoff of
  2^attempts minutes, dead-letter plus an audit row at the fifth attempt.


### Exactly-once side effects (★ new)

Journal-before-effect protocol: (1) append `step_started {step_seq, tool, args, args_hash, effect_id, …}` — committed; (2) execute the tool; effect rows use deterministic ids `uuid5(run_id‖step_seq)` so re-execution upserts; (3) append `step_done {step_seq, result, is_error, …}`. Resume: skip steps with `step_done`; re-execute `step_started`-only steps (safe — upsert). Long steps heartbeat the job lease; the reaper reclaims only expired leases, never live runs. Every tool result is size-capped before journaling.

### Gmail sync (quota-correct, test-scoped)

Parsing is specified separately and was validated before P2 began — see
**`docs/PARSER.md`** for the stack (`mail-parser` plus our own `lol_html`
HTML→text extractor; `html2text` measured and rejected), the two-pass recipe,
and the three traps a probe already found. 47% of real mail in this account has
no `text/plain` part at all, so HTML→text quality is a primary path, not a
fallback.

Quota: 250 units/user/s; `messages.get`=5 units → ceiling 50 gets/s. Token bucket debits true cost; 429/403 → exponential backoff 1→60 s.

1. **OAuth** `oauth2` crate, PKCE + state; persist every refresh (rotation). `invalid_grant` → `needs_reauth`, pause jobs, feed + push alert; recover via `/auth/gmail/start`.
2. **Initial sync** `getProfile` historyId first (overlap, not gap). `messages.list q="newer_than:30d"`, cap `MAX_SYNC_MESSAGES=2000`; batch `format=raw` 10/batch ≤1/s (the 45/batch figure predated the live-tested 10); `mail-parser` (RFC 2047, QP, charsets). Parse failure → metadata row + audit; never abort. **~3.5 min for a 30-day window** (2000 msgs ÷ 10/batch × ~1 s/batch pacer).
3. **Incremental** webhook → `history.list` all 4 historyTypes; per-page transaction; advance per page; enqueue `triage_message` per add. Ingest never calls an LLM.
4. **404 recovery** re-run 30-day sync + reconciliation sweep (ids diff → deletes; per-label membership rebuild).
5. **Watch** daily renewal job; polling fallback if no webhook 30 min (covers dev without tunnel).
6. **Webhook auth** full OIDC: JWKS signature, iss, aud, email==`PUSH_SA_EMAIL`, email_verified. Audience alone is forgeable.
7. **Attachments** proxy endpoint streams `attachments.get` (25 MB cap); `cid:` rewritten; `body_text` guaranteed by our own `lol_html` extractor. (`html2text` was measured and **rejected** — `docs/PARSER.md` §20: "not a dependency. Do not add it.")
8. **Full backfill** of the 63k box: post-v1 flag (`nade backfill --all`, ~35–45 min quota-paced). Not in the test path.

### Agent runtime

```rust
trait Llm  { async fn chat(&self, req: ChatRequest) -> Result<ChatResponse>; }
trait Tool { fn name(&self)->&str; fn schema(&self)->Value;
             fn requires_approval(&self, call:&ToolCall)->bool;
             async fn execute(&self, call:&ToolCall)->Result<Value>; }
trait Journal { async fn append(&self, run:RunId, e:Entry)->Result<Seq>;
                async fn load(&self, run:RunId)->Result<Vec<Entry>>; }
// Engine::run / Engine::resume — resume replays journal per §Exactly-once.

queued → running → done | failed
running → pending_approval —approve(atomic tx)→ queued → running
                          —skip→ skipped        —expiry(7d cron)→ expired
running → waiting(wake_at) —cron→ running
```

Triggers: strong model compiles `nl_definition`→`spec` at save. Deterministic SQL filters first; only `spec.semantic` hits the cheap model (dev cap ≤20 msgs/agent/day, 2 KB body). v1 tools: `search_mail`, `read_thread`, `write_note`, `draft_reply`. `http_fetch` cut (post-v1: forced approval + allowlist + IP guard).

Injection defenses, all on: fenced untrusted-data blocks (10 KB) · mutating tools approval-gated · host-enforced `allowed_tools` · never-messaged recipients flagged red · size-capped tool results · secrets never in prompts.

Ask retrieval: **delegated to Gmail** — see `docs/SEARCH.md`. The validated
query goes to `users.messages.list` (thread-scoped when a `thread_id` is
given), hits are hydrated from the cache and misses batch-fetched, then
recency re-ranked (30-day half-life) and packed newest-first into 8k tokens
for the strong model, citing `gmail_id`s → SSE per contract, `route` first.
There is no local index: the tsvector indexed 0.78% of the mailbox and
answered everything older with a silent empty result.

### iOS app

MVVM + `@Observable`, iOS 18 target, GRDB 7 pinned, MainActor-default isolation with `Sendable` records. No Xcode GUI steps: pbxproj/Info.plist/entitlements/xcscheme edited as files, verified by `xcodebuild`. `NADE_BASE_URL`/`NADE_TOKEN` via launch env; Settings overrides into Keychain (pairing-code entry screen ships in Settings).

GRDB mirrors the wire: `thread` per contract; `thread_mailbox` join; cursors in `mailbox_sync`; `feed_item` enums verbatim; `pending_action` outbox (409=success). `note`/`draft` tables mirror their endpoints. No local FTS.

HTML mail: `body_text` native; "View original" = locked WKWebView (JS off, remote blocked, fixed height). SSE client parses event/data pairs incl. `route`; 300 s timeout; cancellable. Offline acceptance: relaunch with unreachable base URL + XCUITest. Fonts: OFL TTFs (Lora, Cormorant Garamond) downloaded into the repo, registered via UIAppFonts, asserted by a font-load test. Screenshot QA every phase, not last: each screen lands with a `simctl` screenshot beside its design render in `docs/screens/`.

---

## Phases

✓ = machine-checkable acceptance. Gates hard. Screenshot pass and dev caps apply from the phase each first becomes possible.

**P1 — Foundations. ✅ DONE 2026-08-17.**
- [backend] Workspace; `postgresql_embedded` harness (Docker-free dev DB); migrations; `/healthz`; jobs queue with lease heartbeat/reaper/backoff/dead-letter; pairing auth. **66 tests green**, clippy and fmt clean. Live smoke passed: healthz ok, pairing code → token, replay → 401, brute force → 429 after 8 attempts, clean shutdown.
- [sdk] Traits, memory journal, engine with scripted Llm; pause/resume, crash-replay, caps, step_started-replay tests. **52 tests + 4 doctests green.**
- [ios] Scripted project setup, GRDB 7.11.1, four OFL font faces registered, `Theme.swift`, 11 components, `RootTabView`, component gallery. **33 unit + 4 UI tests green**, 14 screenshots in `docs/screens/`.
- [contract] `docs/API.md` written as the canonical contract; `docs/contract/` regenerated from `generate.py` and checked by `validate.py`.
- **Gate out passed**, plus one extra: an adversarial Codex review of the spec seams found 25 findings, all resolved before P2 opened. Two lanes hit a stall watchdog mid-hardening; their work was already green and the remaining hardening was folded into the review pass.

**P2 — Mail lands. ✅ DONE 2026-08-19. [backend] ∥ [ios]**
- [backend] OAuth PKCE flow, token rotation persistence, `needs_reauth` path, quota-bucket client with real multipart batch, **30-day sync (≤2000 msgs)**, mailboxes/threads/thread/search/me. ✓ wiremock suite green (list/get/batch/history/429/rotation); live: search returns a DB-derived subject's id; fixtures byte-match.
- [ios] Mailboxes (1g, the tab root), mail list + chips (1e), thread + agent
  card (1f); GRDB store + ValueObservation; **live HTTP client, pairing, and
  1k's CONNECTION + ACCOUNT sections** — see the scope note below.
  **205 unit + 37 UI tests green**, twice consecutively on iPhone 17 Pro and on
  iPhone SE (3rd gen), no warnings; 22 screenshots in `docs/screens/p2-*`, all
  distinct by hash; the Release build carries no fixture mail. An adversarial
  Codex review of the finished lane returned 19 findings — a dead server-URL
  field, an empty `mailboxes: []` destroying the cache, `needs_reauth` handled
  on only one of three request paths, and sixteen more — all resolved.

**Scope moved into P2 (decided 2026-08-19).** The lane was specified "on
fixtures", with networking at the P5 gate and Settings at P3. It shipped live
instead: a `URLSession` client over the six read endpoints, `POST /auth/pair`
with an origin-bound Keychain credential, and the two sections of 1k whose
backing already existed. What moved, and what deliberately did **not**:

| Moved into P2 | Stayed where it was |
|---|---|
| HTTP client, error envelope, `Retry-After`, `409 needs_reauth` | Feed, approve/skip round-trip, outbox — *built* at P3 against fixtures, *live* at **P5** |
| Pairing + Keychain + server URL | Home feed, Ask states, agents, schedule sheet (**P3**) |
| 1k **CONNECTION** and **ACCOUNT** only | 1k **AGENTS**, **READING**, `disclosure` footer (**P7**; `/runs` lands at P4, `/settings` at P7) |
| The Gmail link flow (`POST /auth/gmail/link`) | Attachments proxy, "View original" WKWebView (**P3**) |
| Offline degradation + the unreachable-host XCUITest | Push, SSE, `/ask` — including 1f's ask bar (**P6**) |

The fixture world did not go away: it is DEBUG-only, launch-argument selected,
and backed by its own database file, because the live P2 backend serves
`agent_cards: []` and `agent_note: null` until P4/P5, so the agent card can only
be *seen* against fixtures.

**Work that P2 pulled out of other phases, so nobody looks for it twice:**

| Now done | Was going to be |
|---|---|
| `Lora-Italic.ttf`, `Theme.Font.bodyItalic`, `UIAppFonts` 4 → 5 | P1's design system. `Font.italic()` on a family with no italic member renders the roman silently, and 1e's caption, 1f's footer and every state caption are italic |
| `docs/contract/thread_partial.json`, wired into `validate.py` **and** `api/contract_tests.rs` | never existed. `API.md` §2 has always said clients must surface `partial`; nothing on either lane had ever serialised it. It also corrected `validate.py`, which required `msg_count == len(messages)` — right for a complete thread, wrong for a partial one |
| `TARGETED_DEVICE_FAMILY = 1`, portrait only | never decided. The target advertised iPad and landscape, which `DESIGN.md`'s single 402 × 874 frame has no layout for and no criterion covered |
| `scripts/screenshots.sh`, checked in | P1 kept its shot script in a scratchpad (D25). This one pins a clock, a time zone and a status bar, and none of that survives being retyped |
| `scripts/assert-release-has-no-fixtures.sh` | new. A Release build must not ship the DEBUG fixture mail, and no test can check a configuration tests never run in |

**Things a later phase should expect to find already true:**

- 1g is the Mail tab's **root**; 1e and 1f are pushed from it, and 1k is pushed
  from 1g. `AppNavigation` (hoisted above `RootTabView`) owns the selection, the
  mail path and the selected mailbox — that is where P6's push deep link lands.
- The tab bar is conditional. Visibility is a property of the **active tab's top
  route**, not of stack depth: 1e, 1g and 1k all keep it and only 1f hides it.
- `import GRDB` lives only in `NADE/Store/`, `URLSession` only in `NADE/API/`,
  and `ModuleBoundaryTests` enforces both.
- Time crosses the wire, the database and the screen through **one** formatter.
  `-NADENow` pins it for a screenshot or a test.
- There is **no local read-marker**, and no column for one. `API.md` §2 forbids
  it and the schema makes it unwritable.

**Still not done in P2, and deliberately:** mail search has no screen —
`DESIGN.md` §1e draws no search field and the mockup has none — the agent card
has no buttons (they need `GET /feed/{id}`'s `action_label`, P5), attachments
are not tappable (proxy at P3), and 1f's ask bar is **chrome**: the mockup's own
field and circle are `<span>`s, so it renders without being a control until P6
gives it one.

**P3 — Mail stays current. ✅ DONE 2026-08-20. [backend] ∥ [ios]**
- [backend] cloudflared quick tunnel (`just tunnel`), webhook + full OIDC,
  incremental history (all four types), 404 sweep, daily watch renewal, 30-min
  poll fallback. ~~attachments proxy + cid rewrite~~ — **both shipped in P2**
  (`api/mail.rs`, `mail/html.rs`, criteria O9–O11); P3 owed nothing there.
  ✓ proven live: real mail arrived at 01:22:21 and the authenticated webhook
  landed at 01:22:25 — **4 s** against a ≤60 s criterion, no restart. Forged
  JWTs 401 in every forgery class. The gate is `cargo test` at the workspace
  root (D27); `cargo test sync::` alone misses the OIDC suite, which lives
  under `api::webhooks::`.
- [ios] 2a (feed ⇄ focus), 1a's three route states, 1b, 1c and 1d, plus
  `NAskField` and the `FlowLayout` the wrapping tag row needed. Attachments
  tappable, "View original" in a locked `WKWebView`, outbox 409 semantics.
  Built against the DEBUG fixture world because the endpoints behind these
  screens land at P4–P6; they go live with no rewrite.

  Three review passes ran on the finished lane — adversarial Codex, an unbiased
  subagent, then `/simplify`. The defects worth remembering:

  - link previews bypassed `decidePolicyFor` entirely, fetching a remote URL on
    long press — the exact tracking callback the CSP exists to stop;
  - the outbox was never drained on launch, foreground or pairing, so a kill
    between the durable write and the request stranded an approval;
  - `ends.date` moved a day in either direction, because a calendar-only date
    round-tripped through an absolute `Date`;
  - two in-flight agent `PATCH`es both derived from the same pre-edit object,
    so the second undid the first;
  - a mid-stream SSE `error` erased the partial answer already on screen;
  - the fixture world returned canned successes for approve/skip, so the
    outbox's refetch restored the card it had just resolved and a consumed
    token could replay forever;
  - the models rebuilt empty on every tab tap, and 1b's "New" popped the screen
    instead of opening the builder;
  - `.submitLabel(.send)` printed "Send" on the keyboard — a C1/C2 violation no
    gallery guard could see.

  Recorded as deviations 50–57 in `DESIGN.md`. Two matter at P6: 1a's citation
  rows are **not tappable** (the contract returns a message id where the thread
  route needs a thread id) and 1c's Invocation radios are read-only (`PATCH`
  accepts no trigger kind).

  **1k's remaining sections are P7, not P3.** AGENTS, READING and the served
  `disclosure` footer need `GET/PATCH /settings` and `GET /runs` — `/runs`
  lands at P4, `/settings` at P7, and the screens that consume both are P7's.
  The footer especially: its point is that the sentence is server-supplied so
  it cannot drift, which a bundled copy would defeat.

**P4 — First runs. ✅ DONE 2026-08-21. [backend]**

Shipped: `runtime/journal.rs` (the Postgres `Journal`, `synchronous_commit` set
for its own transaction, the engine's stamp bound explicitly against the
column's `default now()`), `llm/{anthropic,cost,ledger}.rs` (the Messages
adapter, integer nano-USD pricing, the per-account ledger), `agents/`
(`fence.rs` with a per-run nonce, the four tools, the spec compiler, the
`run_agent` job), the ten routes on `/agents`, `/runs`, `/notes`, `/drafts`, and
migration `0005`. Real tool fingerprints replaced the generator's stand-ins.

✓ **Proven live, not only in tests**: a real sentence compiled (7 s steady
state; the first call is ~50 s of cold TLS), and a run against the live mailbox
journalled `search_mail` → five **parallel** `read_thread` calls in one turn →
`write_note`, `steps=7`, every body fenced, the note landing with a v5 uuid.
`draft_reply` produced a real draft and `PATCH /drafts/{id}` edited it. Total
spend $0.041, and the ledger matches to the digit.

Three review passes ran on the finished lane — correctness, wire contract, and
security/money — and found **four blockers and eleven majors**, all folded in.
The ones worth remembering are `DECISIONS.md` D60–D68: the compile path spent
with no ceiling at all; a billed call whose body would not parse wrote no ledger
row; `verbatim_span` **panicked** on unicode case mapping and lost the user's
sentence through a 500; the fence's fixed delimiter was rated High by the
project's own injection corpus and its guard test counted matches instead of
asserting the property; `read_thread` blew the 16 KiB cap on ordinary
hard-wrapped mail; `PATCH` wrote an unvalidated `schedule` straight into
`jsonb`, which would have failed the app's decode of the *whole* agent list; and
`trigger_summary` disagreed with the fixtures in two places while both lanes
stayed green, because every agent assertion compared shapes and never strings.

One finding was reported as live and was not: the UTC day boundary. The SQL
reasoning was right, but `sqlx` pins `TimeZone=UTC`, so nothing was ever
miscounted — recorded in D68 because a test written for the reported bug passes
against the unfixed code.

**Still owed by P4's neighbours**, discovered during the review and owned by no
phase: the iOS lane still answers `APIFailure.notServedYet` for all six
`/agents` routes (`MailSource.swift`), and no phase's `[ios]` list says "Agents
tab live". P5's gate or P7's list needs it.

The original P4 definition follows.

**P4 — First runs (as specified). [backend] (gate: P1 [sdk] green)**

The SDK stays as it is — no HTTP, no DB, no runtime, per its own manifesto.
Everything below is host code in `nade-server`; every line item is named here
so none is discovered mid-lane.

- [backend] `runtime/journal.rs`: the `Journal` impl over `run_journal` —
  append-only, durable before `Ok`, `created_at` stored byte-faithfully, never
  the database's clock — tested with the existing embedded-PG harness.
  Kill-mid-run resume test; deterministic-id upsert test (kill between execute
  and step_done → no duplicate note).
- [backend] `llm/anthropic.rs`: the Anthropic Messages `Llm` adapter, plus the
  env plumbing — `ANTHROPIC_API_KEY`, `NADE_LLM_MODEL`,
  `NADE_LLM_COMPILE_MODEL` into `config::ENV_VARS` **and** `.env.example`
  (the config test fails in both directions otherwise).
- [backend] `llm_calls` spend ledger, keyed **per account**: ts, purpose
  `run|compile|triage|ask`, `agent_id` (nullable), model, tokens in/out,
  cost_usd. The $1/day ceiling and the ≤20/agent/day triage cap read this
  table. A mid-run breach → `Engine::cancel` loud-fail
  (`run_ended {reason.cap: "cancelled"}`) + a feed `info` item — never the job
  retry path, which would re-spend.
- [backend] Engine host config: **`approval_ttl = None`** — the SDK defaults
  to 7 days and must be overridden. The host's hourly sweep and the approve
  tx's 410 own expiry (API.md §7); double enforcement would expire a timely
  approval whose resume job lagged.
- [backend] Pause plumbing: persist the full `ApprovalRequest` into
  `agent_runs.pending_action`; when P5 raises the feed item, `step_seq` + the
  effect id go into `feed_items.data` — the approve path needs `step_seq` to
  build `Resolution::Approve`.
- [backend] Tools ×4, compile-at-save, manual trigger, runs API (`GET /runs` +
  `GET /runs/{id}` serving the journal verbatim, `agent_runs.summary` served —
  the column lands in migration 0003), notes/drafts endpoints + thread join
  (`notes.unread` served, also 0003; flips false on `GET /notes/{id}`),
  **run-fixture fingerprints made real** (`docs/contract/run*.json` carry
  stand-in `tool_fingerprint`s the generator admits no build reproduces — once
  the four tools exist, compute the real hashes via the SDK's
  `tool_fingerprint` and regenerate, fixture-first),
  **spend ceiling + per-agent caps enforced from this phase**.
- [backend] `DELETE /agents/{id}` cancels non-terminal runs
  (`Engine::cancel`) before the delete: the FK cascade removes `run_journal`
  with the agent, and a live run must not have its log yanked from under it
  (API.md §5 — cancel, don't orphan).
- [backend] Contract completeness: a test asserts every fixture in
  `docs/contract/` is referenced by some contract test, with an allowlist for
  request-body and SSE fixtures.
- ✓ POST /agents compiles a real sentence; a run journal shows ≥1 tool call in
  the SDK vocabulary (API.md §6.1); draft in GET /drafts; PATCH edits it;
  ceiling test trips; kill-mid-run resumes. The gate is `cargo test` at the
  **workspace root**, green twice (D27 — a per-crate feature gate is the exact
  trap it documents).

**P5 — The loop closes. [backend] then GATE then [ios] live.**
- [backend] Mail triggers + capped triage, approval pause (server token, 7-day expiry cron), feed producer, **atomic approve tx**, GET /feed/{id}, **POST /feed/seen** (the built app already calls it), audit, 10 injection red-team fixtures in CI. ✓ H8 email → feed item with token; approve → queued in ONE tx and the resume job carries it to done (test asserts all **five** writes land or none, **and that the tx leaves `run_journal` untouched** — `approval_resolved` arrives only via `Engine::resume`); replayed webhook → no second run; replay approve → 409; expiry flips; red-team fixtures all end pending_approval or no-op.
- **GATE** then [ios] live against local backend: **feed** live, approve/skip
  round-trip, outbox replay. ✓ XCUITest e2e green. (Mailboxes and threads went
  live at P2; this gate is now about the approval loop, not first contact.)

**P6 — Ask + push. [backend] ∥ [ios], join.**
- [backend] /ask with route classification + retrieval spec. Classifier = heuristics + `route_hint`; **ambiguous → `answer`** (a cheap-model fallback is optional and may be cut). **`done.sources` gains `thread_id`** — fixture-first at P6 — so 1a's citation rows become tappable; closes DESIGN deviation 51. **POST /devices** + the APNs payload constant; push on new approval items, with the **ES256 sender deferred behind the existing feature flag until H9 is decided** — simulator acceptance is `simctl push` and needs no key. ✓ `curl -N /ask` streams route→…→done for all three intents (fixture queries); `cargo test ask::` green.
- [ios] SSE client incl. route event; Ask live (answer/results/draft states); **thread-scoped ask from 1f renders the 1a states in a sheet over 1f** — the response surface DESIGN never specified (deviation 61); APPROVAL category fetches via GET /feed/{id} then approves; device registration (POST /devices). ✓ SSE tests green; `simctl push` banner → APPROVE → run status flips.

**P7 — Schedules + Notes + polish. [backend] ∥ [ios]**
- [backend] Schedule jsonb → next_run_at (chrono-tz, DST rules), ends enforcement, waiting/wake_at timers; **GET/PATCH /settings, per account** — the singleton `settings` table is gone in migration 0003. ✓ +2-min agent runs exactly once (test clock); DST boundary test (skipped hour) passes; `after 2` pauses after 2 runs.
- [ios] Notes tab live read-only (list + markdown detail); schedule sheet wired to schedule jsonb; **1k's AGENTS + READING sections and the served `disclosure` footer**; **the Run log screen** over `GET /runs` (the route lands at P4; the screen is here); **the Calendar tab stub** rendering `NADE/Fixtures/calendar.json` per the parity map — promised there and, until now, scheduled nowhere; **a minimal draft sheet** — editable `body_text` over `PATCH /drafts/{id}`, presented from the feed card's Edit and from the thread agent card (v1's only edit path otherwise dead-ends with no surface; DESIGN deviation 60); focus pull-down (2b) time-allowing; cut-list copy audit (no UI promises v1 can't keep). ✓ build test green; screenshot set complete vs design renders.

**P8 — Deploy. [backend], needs H10.**
- Docker image + compose, VPS, Caddy TLS, prod push subscription, phone re-consent, nightly pg_dump + restore test; optional `nade backfill --all`. ✓ https healthz; H8 → feed ≤60 s on prod; restore rebuilds scratch DB.
- **Prod starts empty.** Re-pair, re-consent, resync — mail is re-derivable from Gmail, so nothing migrates. `pg_dump` agents/notes/drafts across from the dev box if wanted; that is the whole data story.
- The **sqlx 0.8→0.9 collapse** belongs here: it removes the duplicate dependency tree, which starts to matter exactly when cold builds do.

**Post-v1:** send scopes + CASA · multi-account · smart mailboxes · OTP feed kind · Notes editing · Calendar · external tools (Notion/Slack) · `http_fetch` guarded · Lucide swap · crates.io publish · full 63k backfill by default · inline cid images.

---

## Test strategy

Doctrine checklist per task (§Execution doctrine).

**Parser.** The plan used to call for 237 real message bodies as golden files.
Those turned out to be the *output* of a parser we already know is buggy, so
they would have encoded its bugs as the specification — and they are personal
mail that would have had to live in git. Replaced by
`backend/testdata/mime/`: 26 hand-built conformance cases whose ground truth is
defined first and encoded by Python's stdlib, so it never passes through a
parser of ours. Real mail is still tested, as a *smoke* against the live
account — nothing panics, `body_text` is never empty — with the messages
gitignored. `docs/PARSER.md` records the validated recipe and the three traps a
probe already found (synthesised `body_html`, destroyed 8-bit headers,
last-wins duplicate `Subject`).

**`nade-gmail-sim` is frozen**: no new sim endpoints, no new sim features. A
new fake uses wiremock unless statefulness is the very thing under test; the
sim's forward value is the P4 `search_mail` harness and the P6 ask harness.

Engine: scripted Llm — pause/resume/kill/replay/caps/deterministic-id upserts.
Sync: wiremock incl. 429, rotation, history 404. Contract: generated fixtures
plus `validate.py`, which enforces referential integrity, causal consistency,
the journal protocol, uuid5-ness of agent-written effects, and that no
`action_label` ever says an outbound verb. iOS: decode, outbox, SSE(route),
offline XCUITest, gallery + per-screen snapshots. Security: forged-OIDC 401,
pairing replay 401, pairing brute-force 429, 10 injection fixtures. Live smoke
behind `NADE_LIVE=1`. **Every phase gate additionally gets an adversarial Codex
review before it counts as done.**

## Top risks

1. 7-day token death (Testing mode) — `needs_reauth` lifecycle + weekly H5 cadence.
2. Prompt injection — defenses + red-team CI; `http_fetch` cut.
3. Duplicate side effects — journal-before-effect + deterministic ids + lease heartbeats (tested by kill tests).
4. Seam drift — canonical contract + shared fixtures + decode tests.
5. Scope creep vs the mockups — parity map is law; UI copy audited in P7.

---

## Appendix — review dispositions

Rounds: B (backend review, 15), I (iOS review, 15) — all fixed in v2, dispositions retained in git history of this file. C (third review, 10) — dispositions below. Zero rebuttals across all 40; C1 partially rebutted on direction, accepted on substance.

| # | Sev | Disposition |
|---|---|---|
| C1 | BLOCKER | Scope parity: kept the lean v1 (speed directive) but added the §Design parity map — every mockup feature mapped to ship/simplify/stub with UI-copy rule; Notes upgraded to live read-only. Mockups remain the target state. |
| C2 | BLOCKER | Compliance wording fixed: v1 = "no outbound actions", server storage + LLM processing disclosed in-app; CASA applies to readonly at ANY public distribution, stated in §Product contract. |
| C3 | BLOCKER | Auth bootstrap designed: one-time pairing code → bearer (hashed, revocable), dev env shortcut, PKCE + state on Gmail OAuth. |
| C4 | BLOCKER | Exactly-once: journal-before-effect, deterministic uuid5(run_id‖seq) effect ids (upsert on replay), lease heartbeats, reaper reclaims only expired leases; kill-between-execute-and-journal test required (P4). |
| C5 | MAJOR | Approval completed: GET /feed/{id} deep link, atomic approve tx (tested; write list finalised 2026-08-21 as five writes with `run_journal` excluded — §Approval semantics), PATCH /drafts for Edit, APNs payload slimmed to feed_item_id only — no capabilities in push. |
| C6 | MAJOR | Schedule model: persisted jsonb recurrence (freq/interval/byweekday/at/tz/ends), chrono-tz wall-clock DST rules, ends enforcement, API exposure; DST boundary test in P7. |
| C7 | MAJOR | Ask routing contract: route SSE event with three kinds, heuristics-then-cheap-model classification, client state mapping; /search stays for Mail tab. |
| C8 | MAJOR | Design adherence made executable: mockups live at docs/MockUps (verified), ios-plan folded into this file, OFL TTFs downloaded into repo at P1 with font-load test; SF-Symbols-for-Lucide recorded as deviation in the parity map. |
| C9 | MAJOR | Cost controls + screenshot QA moved to first-possible phases: spend ceiling + caps enforced at P4 (first live LLM), per-screen screenshots from P2. |
| C10 | MAJOR | H6 corrected: Pub/Sub service agent needs roles/iam.serviceAccountTokenCreator on the push SA; exact principal format included. |

# NADE

**Not A Dumb Email** — an iOS Gmail client whose defining feature is email
agents you write in a sentence.

You type *"When a recruiter emails, note the next steps"*. A model compiles that
sentence into a spec; the runtime watches your mail for matches, runs a
tool-calling loop against the ones that hit, and drops the result in a feed you
approve or skip. A Rust backend holds the mail store and the agent runtime. The
SwiftUI app is the client.

This is a personal build, in progress. It is not packaged for anyone else to
run — the Gmail OAuth client is in Google's Testing mode, which caps it at 100
users and expires consent every seven days.

## v1 takes no outbound actions

The hard product line, and the one every UI string is audited against: **agents
observe, search, take notes and prepare drafts. Nothing leaves the machine.**
Drafts live in NADE's own database and are never written back to Gmail, so an
approval button says "Save draft" or "Save note" and never "Send". A prompt
injected into a message body can, at worst, cause a note you did not ask for.

What that costs is honesty about the rest: your mail *is* stored on your own
server, and excerpts *are* sent to the model you configure. Settings says so in
a string the server supplies, so the sentence cannot drift from what the code
does.

## Where it is

| Phase | State |
|---|---|
| P1 Foundations | ✅ 2026-08-17 |
| P2 Mail lands | ✅ 2026-08-19 |
| P3 Mail stays current | ✅ 2026-08-20 |
| P4 First runs | ✅ 2026-08-21 — live against real mail and a real model |
| P5 The loop closes | ✅ 2026-08-25 — feed, approval transaction, mail trigger |
| P6 Ask + push · P7 Schedules + Notes · P8 Deploy | not started |

Green at P5: 1200 workspace tests plus a detached red-team harness's 45, 345
iOS unit and 53 iOS UI tests, each run twice consecutively; `just ci` green
twice; 38 screenshots, all distinct by hash. Everything so far has run on the
simulator only.

## Layout

```
backend/                       Cargo workspace (edition 2021, rustc ≥ 1.90)
  crates/nade-agent-sdk/       generic agent engine — zero NADE types, zero infrastructure
  crates/nade-gmail-sim/       stateful in-process simulator of the Gmail REST API
  crates/nade-server/          axum API, Gmail sync, agent runtime, jobs queue
NADE/, NADE.xcodeproj/         the iOS app (SwiftUI, iOS 18, GRDB 7, MVVM + @Observable)
docs/                          PLAN, DESIGN, API, PARSER, SEARCH, contract/, MockUps/
```

### The agent engine

`nade-agent-sdk` is the part that would be worth extracting: a tool-calling
loop that survives being killed. Three traits describe the world — `Llm` talks
to a model, `Tool` does something, `Journal` remembers — and `Engine` drives
them. It compiles with no HTTP client, no database and no async runtime, which
is why the Postgres journal driver and the Anthropic adapter are host code in
`nade-server` rather than crate features.

It guarantees **at-least-once execution with stable idempotency keys**, and it
is careful about the difference between that and exactly-once. The journal is
written *before* the effect, not after:

1. append `step_started { step_seq, tool, args, args_hash, effect_id }`, commit;
2. execute the tool;
3. append `step_done { step_seq, result }`.

On restart, a step with both entries is skipped, a step with only the first is
**run again** — because at the instant a process dies, whether the effect
landed is a fact about the outside world the engine has no record of. Effect
rows carry deterministic `uuid5(run_id‖step_seq)` ids and are written as
upserts, so the retry collapses into the original. Exactly-once *effects* are
therefore available and are the host's to build.
`backend/crates/nade-agent-sdk/README.md` is the long version.

## Running it

`cargo` is not on the default non-interactive PATH.

```sh
export PATH="$HOME/.cargo/bin:$PATH"

cd backend && cargo test              # the whole workspace
cd backend && just ci                 # build, clippy -D warnings, fmt-check, tests, red team

xcodebuild -project NADE.xcodeproj -scheme NADE \
  -destination 'platform=iOS Simulator,name=iPhone 17 Pro' build test

scripts/bench.sh --screen 1e          # backend + build + install + pair + deep link, live
```

`scripts/bench.sh` is the bench, not the `-NADESeed` fixture launch: it runs
the app against the real server on real mail. `docs/BENCH.md` covers it.

The dev database is `postgresql_embedded` — portable binaries, no Docker until
P8. Secrets live only in `backend/.env` and `backend/secrets/`, both gitignored;
`docs/PHASE0.md` lists the Google Cloud and Apple steps that a human has to
click through, and which of them are already done.

Dev caps are enforced in code and are deliberately small: a 30-day sync window,
`MAX_SYNC_MESSAGES=2000`, ≤20 triaged messages per agent per day, a $1/day LLM
ceiling, `max_steps=12`, and 50k tokens per run.

## Decisions worth knowing before reading the code

- **Postgres for everything.** No Redis; the jobs queue is `for update skip
  locked` with leases, a reaper and dead-lettering.
- **No sqlx compile-time macros** — runtime `query`/`query_as` only, so a build
  never needs a live database.
- **Hand-rolled LLM adapters**, no `genai`. APNs is reqwest + jsonwebtoken, no
  `a2`. Every call is Haiku.
- **The migration is the schema.** `0001_init.sql` is executable DDL with the
  constraints; there is no prose copy of it to drift.
- **Search is delegated to Gmail.** A local tsvector index reached 0.78% of the
  mailbox and answered everything older with a silent empty result
  (`docs/SEARCH.md`).
- **HTML→text is a primary path**, not a fallback: 47% of real mail in this
  account has no `text/plain` part at all (`docs/PARSER.md`).

## Documents

`docs/PLAN.md` governs the build — phases, semantics, caps. `docs/DESIGN.md`
governs every pixel, against the mockups in `docs/MockUps/`. `docs/API.md` and
`docs/contract/` are the wire: a shape change touches the fixture first, and
`validate.py` fails the build if it does not. `docs/PHASE0.md` is what only a
human can do. `backend/DECISIONS.md` is the running log of why.

## License

MIT — see [LICENSE](LICENSE).

The bundled Cormorant Garamond and Lora faces are not covered by it: they ship
under the SIL Open Font License 1.1, whose text sits beside them in
`NADE/Resources/Fonts/`.

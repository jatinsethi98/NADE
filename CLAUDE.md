# NADE — working notes for agents

NADE ("Not A Dumb Email") is an iOS Gmail client whose defining feature is
user-defined LLM email agents. A Rust backend holds the mail store and the
agent runtime; the SwiftUI app is the client.

## The four documents that outrank your judgement

| File | What it governs |
|---|---|
| `docs/PLAN.md` | The build. Phases, schema, semantics, dev caps. Source of truth. |
| `docs/DESIGN.md` | Every pixel. Tokens, per-screen layout, the v1 parity cuts. |
| `docs/contract/` | Every wire shape. A shape change touches the fixture **first**. |
| `docs/PHASE0.md` | What only a human can do, and what is already done. |

The mockups themselves live in `docs/MockUps/` (`Email App.dc.html`, screen
anchors `1a`–`1k`, `2a`, `2b`, plus the Classical design system CSS). When
`DESIGN.md` is ambiguous, the mockup's inline styles are the answer.

## Execution doctrine (PLAN.md, applies to every task)

1. Write the acceptance criteria **and** an edge-case checklist before the code.
   Minimum: empty input, unicode, crash mid-step, duplicate delivery/replay,
   expiry, pagination boundary, 429/timeout, clock skew.
2. Implement, then adversarially self-review against that checklist. Loop until
   everything passes. Right in one shot, not fixed in review later.
3. Each edge case is a test or an `// EDGE:` comment beside the code.
4. Done means the acceptance command passed **twice consecutively**.
5. Build to the testable level, no further. The dev caps are law: 30-day sync
   window, `MAX_SYNC_MESSAGES=2000`, ≤20 triaged messages per agent per day,
   $1/day LLM ceiling, `max_steps=12`, 50k tokens per run.

Checkpoints get an **adversarial review from Codex** before they count as done.

## Layout

```
backend/                       Cargo workspace
  crates/nade-agent-sdk/       generic, publishable agent engine — zero NADE types
  crates/nade-server/          axum API, Gmail sync, agent runtime, jobs queue
  secrets/                     gitignored: web_client.json, apns.p8
NADE/, NADE.xcodeproj/         the iOS app
NADETests/, NADEUITests/       test targets
docs/                          PLAN, DESIGN, PHASE0, contract/, MockUps/, screens/
```

## Commands

```sh
export PATH="$HOME/.cargo/bin:$PATH"          # cargo is NOT on the default PATH
cd backend && cargo test                       # the whole workspace; see DECISIONS.md D27
cd backend && cargo test -p nade-server        # faster, but a per-crate feature set
xcodebuild -project NADE.xcodeproj -scheme NADE \
  -destination 'platform=iOS Simulator,name=iPhone 17 Pro' build test
```

Tool locations that are not on the default non-interactive PATH:
`~/.cargo/bin` (cargo 1.97.1), `~/google-cloud-sdk/bin` (gcloud, project
`deliveriesapp-293223`), `/opt/homebrew/bin` (brew, `codex` 0.147,
python3.14), `~/.local/bin` (cloudflared).

## Conventions that are already decided

- **No sqlx compile-time macros.** Runtime `query`/`query_as` only, so builds
  never need a live database.
- **No Docker in dev.** Postgres comes from `postgresql_embedded`; Docker
  appears at P8 only.
- **No Redis, no `genai` crate, no `a2`.** Postgres for everything; hand-rolled
  LLM adapters; APNs via reqwest + jsonwebtoken.
- **Fonts are bundled, not fetched.** Four OFL static faces in
  `NADE/Resources/Fonts/`, PostScript names `Lora-Regular`, `Lora-SemiBold`,
  `CormorantGaramond-Regular`, `CormorantGaramond-SemiBold`.
- **Light appearance only.** The design ships one visual world; dark mode is
  post-v1.
- **SF Symbols stand in for Lucide.** Recorded deviation, swap pass post-v1.
- The Xcode project uses `PBXFileSystemSynchronizedRootGroup`, so a new file
  dropped into `NADE/` joins the target with no pbxproj edit.

## What v1 does not do

v1 takes **no outbound actions**. Agents observe, search, take notes and
prepare drafts; drafts live in NADE and never in Gmail. Approval buttons
confirm local effects — the copy says "Save draft" or "Save note", never
"Send". Any UI string that promises otherwise is a bug (PLAN.md C1/C2).

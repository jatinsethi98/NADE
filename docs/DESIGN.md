# NADE Design Spec — extracted from `docs/MockUps/Email App.dc.html`

Source of truth for every pixel. The Classical design system tokens live in
`docs/MockUps/_ds/classical-*/styles.css`; this file distils them plus the
per-screen layout into something SwiftUI can be built against. **Numbers here
are the design's numbers.** Where the design uses fractional points (10.5, 14.5,
12.5) keep them — SwiftUI takes CGFloat.

Design frame: iPhone at **402 × 874 pt** (iPhone 17 Pro / 16 Pro logical size),
from `ios-frame.jsx`'s `IOSDevice`.

**Markers.** Every difference from the mockup is marked inline and indexed in
§6. An implementer must never have to guess whether a difference is deliberate.

- **`v1 →`** the mockup shows X, v1 ships Y, for the stated reason.
- **`+ Added`** the mockup does not contain this at all; v1 needs it.
- Data bindings quote `docs/API.md`. If this file and `API.md` disagree,
  `API.md` wins on field names and `Email App.dc.html` wins on pixels.

---

## 1. Tokens

### Color (light)

| Token | Hex | Use |
|---|---|---|
| `bg` | `#f3f2f2` | every screen ground |
| `surface` | `#eae9e9` | sheets, dialogs |
| `ink` | `#201f1d` | primary text |
| `accent` | `#b68235` | the single accent: active tab, links, rules, focus |
| `accent2` | `#ac803e` | secondary accent (unused by any screen) |
| `divider` | `ink @ 16%` | every hairline (1 pt) |

Neutral ramp: `100 #f8f4f4`, `200 #eae7e7`, `300 #d7d3d3`, `400 #bab6b6`,
`500 #9b9797`, `600 #7d7979`, `700 #605d5d`, `800 #444141`, `900 #2d2b2b`.

Accent ramp: `100 #fff3e4`, `200 #ffe3bf`, `300 #facb8d`, `400 #e1ad66`,
`500 #c28d41`, `600 #a06f24`, `700 #7d5411`, `800 #5a3b0a`, `900 #3a270d`.

Accent-2 ramp: `100 #fff3e4`, `200 #ffe3be`, `300 #f5cd96`, `400 #dbaf70`,
`500 #bc8f4e`, `600 #9b7232`, `700 #79561f`, `800 #573d14`, `900 #382810`.

**Text opacity ladder** (the design uses `color-mix(ink N%, transparent)`):
`ink50`, `ink55`, `ink60`, `ink62`, `ink68`, `ink70` — implement as
`Color.ink.opacity(0.55)` etc. These are *the* muted greys; never invent one.
The screens use only 55, 60, 62 and 68, and the difference between them is
load-bearing: read the per-screen number, do not round.

Shadows: `sm 0 1 2 / #2d2b2b@14%`, `md 0 3 10 / @16%`, `lg 0 12 32 / @22%`.

Focus ring (DS `:focus-visible`): 2 pt accent outline, offset 2 — SwiftUI
`.focused` + accent overlay. Inside a segmented control the DS uses offset −2.

**Dark mode: out of scope for v1.** The design ships one visual world. Force
light appearance app-wide (`.preferredColorScheme(.light)`) so the palette
always resolves as designed. Recorded deviation, revisit post-v1.

### Type

Two families, both bundled as OFL TTFs (SIL Open Font License, redistributable):

- **Cormorant Garamond** — headings, card titles, buttons, numerals in the
  calendar. Weight 600 (semibold) for interface headings; **400 (regular) for
  display text** ("Good morning" 26–27 pt, OTP code 25 pt, calendar day 22 pt,
  the "{n} agents" count). The DS retired bold: 600 is the ceiling.
- **Lora** — all body text, 15 pt base, line-height 1.55.

Scale actually used on screens (pt): `27` greeting (1a) · `26` greeting
(2a/2b) · `25` OTP code, md `#` · `24` thread subject · `23` screen title ·
`22` builder agent name / calendar day · `20` sheet title, "＋" on 1h ·
`19` draft agent name, md `##` · `17` list item title (1b/1h), feed date header
(2a) · `16` builder sentence (1c), agenda title (1j), agents count (2a focus),
md `###` · `15` list primary / body base · `14.5` mail body, settings row,
focus prompt · `14` secondary body, buttons · `13.5` search placeholder, reply
pill, agenda row · `13` meta, nav actions · `12.5` feed peek, mail chip, note
source · `12` captions · `11.5` prompt numeral, calendar item name ·
`11` timestamps · `10.5` tab labels, feed source · `10` section eyebrows ·
`9.5` calendar item time and weekday.

The 1a "TRY" eyebrow is **11 pt**; the 2a/2b one is **10.5 pt**. They are not
the same size — do not unify them.

Letter-spacing: uppercase eyebrows `0.09–0.10em`, uppercase meta `0.06–0.08em`
(feed source `0.07em`), calendar item time `0.03em`, OTP code `0.12em`,
tag `0.02em`, headings `-0.015em`.

`font-variant-numeric: tabular-nums` on every timestamp, count, and numeral —
in SwiftUI: `.monospacedDigit()`.

**Time strings.** The mockup's fixtures use four different shapes; v1 uses two
formatters and nothing else.

| Formatter | Rule | Used by |
|---|---|---|
| `listTime` | today → `H:mm` · yesterday → "Yesterday" · < 7 days → weekday abbr ("Fri") · this year → "D MMM" · else "D MMM YYYY" | mail row, feed row, thread meta |
| `relTime` | < 60 s → "just now" · < 60 min → "N min ago" · < 24 h → "Nh ago" · yesterday → "yesterday" · < 7 days → "N days ago" · this year → "D MMM" · else "D MMM YYYY" | notes meta, agents last-run |

### Space & shape

`space-1 4.6` · `2 9.2` · `3 13.8` · `4 18.4` · `6 27.6` · `8 36.8`
Radius: `sm 2` · `md 4` · `lg 7` · pill `999`.
Hairline = 1 pt `divider`, full-bleed unless stated.

**Horizontal padding — the real rule.** 22 pt is the screen default. The
exceptions are exact, not approximate:

| Where | Horizontal pad |
|---|---|
| every root screen body (1a, 1b, 1e, 1g, 1h, 1j, 1k), 1c and 1d in full | **22** |
| thread (1f) and note (1i): **nav bar and bottom bar** | **20** |
| thread (1f) and note (1i): **scrolling body** | **22** |
| 1a's docked ask field | **18** |
| 2a and 2b, every band including the feed rows | **20** |
| 1e mail rows | 22 right, **14 left** (the unread dot hangs in the inset) |
| tab bar | 10 |

### Components (from the DS)

- **Button** — Cormorant 600, 14 pt, radius 4, padding `9.2 × 16.6`,
  line-height 1.2, icon gap 6.
  `primary` = accent text + 1 pt accent border, transparent fill;
  `secondary` = ink text + 1 pt divider border;
  `ghost` = accent text, no border, horizontal padding 4.6.
  **There are no filled buttons anywhere.** Color is a stroke.
- **Input** — 1 pt divider border, radius 4 (pill 999 for the ask field),
  min-height 36 in the DS, 14 pt, caret accent, focus border → accent,
  hover border `ink45`.
- **Card** — 1 pt divider border, radius 4, padding 13.8, transparent fill.
  Kicker: 10 pt uppercase accent `0.1em`.
- **Tag** — 11 pt, `0.02em`, padding `3 × 10`, radius 3. `neutral` =
  neutral-100 bg / neutral-800 text; `outline` = 1 pt accent border + accent
  text; `accent` = accent-100 bg / accent-800 text.
- **Segmented** — inline row, 1 pt divider border, radius 4, 1 pt divider
  between options; the selected option gets an *inset* 1 pt accent ring +
  accent text, the rest inherit ink. DS option padding `7 / 12` at 13 pt;
  **1c overrides to `6 / 14` at 13 pt**, 1i to `5 / 12` at 12 pt.
- **Radio** — 16 pt circle, 1.5 pt ring; selected = accent ring + accent fill
  with a 4 pt inset ring in `bg` (donut); unselected = divider ring,
  transparent fill.
- **Toggle** — *not a DS component; built inline on 1c and 1k.* 46 × 26 pill,
  1 pt border, 20 pt knob at top 2, x = 2 (off) / 22 (on), `left` 0.18 s ease.
  On = accent-100 track, **accent** knob, accent border. Off = transparent
  track, **neutral-400** knob, divider border.

### Safe area

The frame's status bar is **62 pt tall** (`21 + 22 + 19`) and is
`position: absolute; top: 0` — it **overlays** the screen, it does not push it
down. Its glyph row occupies y 21–43. Every screen's top padding is therefore
measured **from the top edge of the display**, and the 4 pt spread between 58,
60 and 62 is real: it is extra air under the glyphs, not a status-bar offset.

The home-indicator region is 34 pt tall with the 139 × 5 bar sitting 8–13 pt
above the bottom edge. The tab bar's `26` bottom padding leaves the labels
13 pt clear of the bar.

| Design measurement | From | Value | SwiftUI |
|---|---|---|---|
| screen header top | display top edge | 58 / 60 / 62 | pad from the display edge, **not** below the safe-area inset — the design device's top inset is also 62, so subtracting it collapses all three screens to 0 and loses the design's air |
| tab labels' bottom | display bottom edge | 26 | let the bar extend under the home indicator and pad 26 from the display edge; deferring to the 34 pt inset would put the labels at 34–42 |

Tab bar padding is `9 / 10 / 26` and that 26 is a display-edge number.

---

## 2. Chrome

### Tab bar (every root screen)

Four tabs, equal width, 1 pt top divider, icon 18 pt stroke 1.8 above a 10.5 pt
uppercase label with `0.09em` tracking, icon-to-label gap 5. Active = accent;
inactive = `ink62`.

| Tab | Lucide (design) | SF Symbol (v1) |
|---|---|---|
| Ask | `sparkles` | `sparkles` |
| Mail | `mail` | `envelope` |
| Notes | `file-text` | `doc.text` |
| Calendar | `calendar` | `calendar` |

Recorded deviation: SF Symbols stand in for Lucide (parity map, PLAN.md).
Rendered at `.font(.system(size: 18, weight: .light))` to match stroke 1.8 —
**and framed at 18 × 18**. The font size sets the stroke; it does not set the
box. An SF Symbol reports its *font* line box instead (~24 pt at size 18, and a
different number per symbol), where every glyph in the mockup is an
`<svg width="18" height="18">`. Taken as a box, the bar measured 78.67 pt
against the mockup's 75.28 and the four labels landed on baselines 2.33 pt
apart. `NIcon` therefore carries the square frame, and the label's own box is
the inherited `line-height: 1.55` — `10.5 × 1.55 = 16.275`, not Lora's ~13.4.
Bar height is exactly `1 + 9 + 18 + 5 + 16.275 + 26 = 75.275`.

Which screens carry the bar, and which tab is lit — this is the design's own
navigation map, so follow it:

| Screen | Tab bar | Lit |
|---|---|---|
| 2a / 2b, 1a | yes | Ask |
| 1b Agents | yes | **Ask** (Agents is pushed from the Ask tab) |
| 1e Mail list, 1g Mailboxes | yes | Mail |
| 1h Notes list | yes | Notes |
| 1j Calendar | yes | Calendar |
| 1c builder, 1d sheet | **no** — modal | — |
| 1f thread, 1i note | **no** — pushed detail | — |
| 1k Settings | yes, with **no tab lit** | — |

`v1 →` 1k's unlit bar means the mockup never says where Settings lives. v1
pushes Settings from the **Mailboxes screen (1g)**, whose nav bar's right-hand
"Edit" becomes the word **"Settings"** at the same metrics (accent, 13 pt) —
1g is the Mail tab's only settings-shaped surface, and "Edit" has nothing to
edit under a single account. Settings therefore keeps the Mail tab lit.

### The ask field

Pill input + circular 1 pt accent button, gap 10, both `align-items: center`.

| Screen / state | Field | Border | Circle | Glyph | Placeholder |
|---|---|---|---|---|---|
| 2a feed (pinned top) | min-height **38**, pad `8 / 15`, 13.5 pt | divider | 38 | `sparkles` 16 | "Ask, search, or describe an agent" |
| 2a focus / 2b (centred) | min-height **44**, pad `10 / 16`, 14 pt | **accent** | 44 | `arrow.up` 18 | same |
| 1a (docked bottom) | min-height **40**, pad `9 / 15`, 14 pt | divider | 40 | `arrow.up` 17 | same |
| 1f thread (bottom bar) | pad `10 / 15`, 13.5 pt, no min-height | divider | 38 | `sparkles` 16 | `v1 →` "Ask for a draft or an answer…" |

`v1 →` 1f's placeholder replaces the mockup's "Reply, or ask for a draft…":
"Reply" promises the one thing §4's own rule forbids — v1 sends nothing — so
the field must not open with it (deviation 58).

2b's static render pads `11 / 16` with no min-height; 2a's live input is the one
v1 builds. Submitting from any of them posts `POST /ask` (PLAN §Ask routing); on
1f the `thread_id` goes with it, and the response renders as the 1a states in a
sheet over 1f (P6; deviation 61).

---

## 3. Screens

Screen ids match the mockup anchors.

**Tab root.** The mockup's own turn title says turn 2 *replaces* 1a, so the Ask
tab's root is **2a** (feed ⇄ focus). `v1 →` **1a's idle state is not built** —
2a's focus state is its replacement. 1a survives as the two *query* states
(`results`, `agent_draft`, plus `answer`, below), pushed over the root when the
ask field is submitted, in 1a's own layout.

### 2a — Home, feed (default) · `HomeFeedView`

Two stacked states inside one screen, absolutely positioned over each other and
toggled by `display` — **feed** (default) and **focus**.

Feed state:
1. Header, padding `58 / 20 / 11`, bottom divider.
   - Row (`align-items: baseline`, gap 8, margin-bottom 11): date
     "Sunday 16 August" Cormorant 600 17 pt · right `{new_count} NEW` 11 pt
     uppercase accent `0.06em` tabular.
   - Row: the ask field (38, see §2).
2. Grabber strip, **inside the scroll view** so it scrolls away with the feed:
   centred 34 × 3 pill `neutral-400`, gap 5, under it "PULL DOWN TO HIDE" 10 pt
   uppercase `ink55` `0.08em`; padding `9 / 0 / 8`; bottom divider.
   Tapping it → focus state.
3. Feed rows, each padding `12 / 20 / 13`, bottom divider, **left border 2 pt**
   — accent when `kind == "approval" && status == "new"`, `clear` otherwise.
   - Row 1: `title` (the agent's name) 10.5 pt uppercase accent `0.07em` ·
     right `listTime(created_at)` 11 pt tabular `ink60`.
   - `body` 14 pt, line-height 1.6, top 5.
   - Buttons, top 10, gap 8 — **rendered from `actions`, in that order**:
     `approve` = primary, `edit` = secondary, `skip` = ghost. `actions` is `[]`
     for anything resolved, skipped, expired or `kind: "info"`, so the row has
     no buttons then.
     `v1 →` the primary button's label is **`data.action_label`** ("Save note",
     "Save draft"), not the mockup's literal "Approve": v1 takes no outbound
     action and the button must name the local effect (PLAN C2, API.md §7.1).
   - Resolved: `resolved_note` rendered verbatim, italic 12.5 pt `accent-700`,
     top 8.
   - `v1 →` **no OTP chip** (parity map — kind `otp` is post-v1). The mockup's
     chip is: top 9, 1 pt divider border, radius 4, padding `8 / 13`, gap 12,
     code Cormorant **400** 25 pt `0.12em` tabular, right "Copy" 12 pt accent
     with a 1 pt `accent-300` underline.
4. Footer: "Older activity lives in the run log." italic 12 pt `ink55`,
   padding `16 / 20 / 24`.

Focus state:
1. "Good morning" Cormorant **400** 26 pt · right "SUN 16 AUG" 11 pt uppercase
   `ink60` `0.06em` tabular. Padding `58 / 20 / 0`.
2. Vertically centred block, padding `0 / 20`: eyebrow "TRY" 10.5 pt uppercase
   `ink60` `0.09em`, margin-bottom 8; three prompt rows, each `11 / 0` with a
   **top** divider, gap 12, accent tabular numeral `01–03` at 11.5 pt
   (padding-top 3) + prompt text 14.5 pt line-height 1.45.
3. `+ Added:` the agents row, directly under the prompts — top divider,
   margin-top 26, padding-top 14, gap 8, baseline-aligned: "{n} agents"
   Cormorant **400** 16 pt in ink · "· {p} published, {d} drafts" 13 pt
   `ink60` · right "All →" accent 13 pt, pushing 1b. This is lifted verbatim
   from 1a's idle state, which v1 does not build; without it the Agents list is
   unreachable. 1a idle's italic last-run line is **not** carried over — the
   feed already shows every run.
4. Ask field 44 with **accent** border, margin-top 22 (see §2).
5. `v1 →` **no tips caption.** 2b renders "Tips fade out after the first week —
   the feed becomes the default view." italic 12 pt `ink55` centred, top 16;
   2a's live focus state goes straight from the ask field to the bottom peek.
   v1 follows 2a, for two reasons: 2a is the interactive component and 2b is a
   static render of the same state, and v1 does not implement a first-week
   fade-out, so the sentence would be a promise the app cannot keep (§4).
6. Bottom peek: top divider, padding `9 / 20 / 12`. Grabber block (gap 5,
   margin-bottom 9): 34 × 3 pill + "{new_count} NEW · SWIPE UP" 10 pt uppercase
   accent `0.08em` tabular. Under it, at **opacity 0.45**, gap 7, two one-line
   feed previews: source 10.5 uppercase accent `0.07em` + snippet 12.5
   truncated; the first has a 2 pt accent left border with padding-left 9, the
   second padding-left 11 and no border.

Transition: pull down / tap grabber ⇄ swipe up. Implement as a drag gesture on
the grabber + tap, with the two states cross-fading (no partial drag in v1).
PLAN P3 builds both states; P7 lists the pull-down gesture as time-allowing.

Binds: `GET /feed` — `new_count`, `items[].{title, body, kind, status,
created_at, actions, resolved_note, data.action_label}`. `POST /feed/{id}/approve`
and `/skip` carry `approval_token`; 409 is treated as success.

### 1a — Ask query states · `AskView`

Header "Good morning" Cormorant 400 27 pt + right "SUN 16 AUG" 11 pt uppercase
`ink62` `0.08em` tabular; padding `62 / 22 / 8`. Scroll area padding
`14 / 22 / 8`. Docked ask field at the bottom (40) above the tab bar, top
divider, padding `10 / 18 / 12`.

The state is the SSE `route` event (API.md §4).

- **`answer`** — `+ Added`: the mockup has no screen for this route, but the
  contract has three. Render the query bubble, then the streamed prose at 14 pt
  line-height 1.65, then `done.sources` as rows `11 / 0` with a top divider —
  **subject only**, 13 pt `ink62` (a source carries `gmail_id` and `subject`
  and nothing else), tapping opens the thread. Empty `sources` renders nothing.
  Button: "Clear" (ghost).
- **`results`** — the asked query as a right-aligned bubble (max-width 78%,
  1 pt divider border, radius 7, padding `9 / 13`, 14 pt, `neutral-100` fill,
  margin-bottom 16); the streamed prose lead-in 14 pt line-height 1.65,
  margin-bottom 14; then the hit rows (`11 / 0`, top divider): `from_name`
  14 pt + right `listTime(ts)` 11 pt tabular `ink62`, `subject` 13 pt `ink62`
  top 2. Buttons top 16, gap 8.
  - `v1 →` **the progress card is not built.** The mockup's card (1 pt divider
    border, radius 4, padding `12 / 13`, kicker 11 pt uppercase accent
    `0.08em`, 3 pt `neutral-300` track with an accent fill, 12 pt tabular
    `ink60` footings) has no field behind it in any endpoint.
  - **"Make this an agent" ships**, beside "Clear" (ghost). It re-posts the
    same `query` to `POST /ask` with `route_hint: "agent_draft"`, which forces
    the route instead of letting the server classify, and the screen moves to
    the `agent_draft` state. (`route_hint` was added to `API.md` §4 for exactly
    this button — the spec briefly cut it as unimplementable, which it was
    until the contract gained the field.)
- **`agent_draft`** — query bubble (max-width 80%), lead line "Here's what I'd
  build. Tap anything underlined to change it." 14 pt line-height 1.65,
  margin-bottom 16, then the **draft card**: 1 pt **accent** border, radius 4,
  padding `15 / 15 / 13`; kicker "DRAFT AGENT" 10 pt uppercase accent `0.1em`,
  margin-bottom 10; `name` Cormorant 600 19 pt, margin-bottom 12; the sentence
  `When {when_span}, {do_span}.` at 15 pt line-height 1.85 with both spans
  underlined 1 pt accent (padding-bottom 1), then `trailing` in `ink60` when
  non-null; tag row top 14, gap 6, wrapping: `tool` as `tag-outline`,
  "Approval required" `tag-neutral` when `approval_required`, "Draft"
  `tag-neutral` from `status`. On save: italic 13 pt `accent-700`, top 14,
  "Saved to Agents as a draft. It won't run until you publish it."
  Buttons: "Save as draft" (primary) → `POST /agents {nl_definition}`,
  "Start over" (secondary).

### 1b — Agents · `AgentsListView`

Header `62 / 22 / 12` + bottom divider, gap 10: "Agents" Cormorant 600 23 pt,
right "New" 13 pt accent. Filter row `11 / 22`, bottom divider, gap 16, 12 pt:
"All {n}" (accent + 1 pt accent underline, padding-bottom 3), "Published {p}",
"Drafts {d}" (`ink60`). Rows padding `14 / 22`, bottom divider, name row gap 9:
name Cormorant 600 17 pt + right `status` 10 pt uppercase `0.06em` tabular in
the status color; `nl_definition` 13 pt `ink68` line-height 1.55 top 4; meta row
top 8, gap 7, 11 pt `ink62` tabular: `trigger_summary` **in accent** · "·" ·
`ran {relTime(last_run_at)}` or "never run" · optional "· needs approval" when
`approval_required`.

| `status` | Color |
|---|---|
| `published` | accent |
| `draft` | **`ink62`** |
| `paused` | `neutral-500` — `+ Added`: no agent in the mockup is paused, but `ends.after` moves agents there (API.md §5.2), so the state needs a color |

Binds: `GET /agents`.

### 1c — Agent builder · `AgentBuilderView`

Modal, no tab bar. Nav bar `60 / 22 / 11` + divider, baseline-aligned:
"Cancel" accent 13 · centred "Edit agent" Cormorant 600 15 · "Save" accent 13.
Everything below the nav bar is one scroll view — **including the footer
buttons**, which are not pinned.

1. Sentence block `20 / 22 / 18`, bottom divider: `name` Cormorant 600 22 pt,
   margin-bottom 14; the sentence 16 pt line-height **2**; hint italic 12 pt
   `ink60` top 14: "Tap any underline to edit, or open a section below."

   The sentence is **composed, not parsed**, and the composition branches on
   `spec.trigger.kind`:

   - **schedule** — `{When_span}, {do_span}.` with **no "When" prefix**: the
     span itself opens the sentence, its first letter capitalised. The
     authority is the mockup's own 1d parent sentence ("Every weekday at 8:00,
     build today's to-do list…" — see §1d), and "When every weekday at 08:00,
     …" is not English. This rule resolves what used to be a
     self-contradiction between this section and §1d (deviation 59).
   - **everything else** (`mail`, `manual`) — `When {when_span}, {do_span}.`

   Either way the sentence is followed by `{trailing}` in `ink60` when
   non-null. Both spans are underlined 1 pt accent with padding-bottom 2, and
   each is independently tappable. All three fields come from
   `GET /agents/{id}` (`API.md` §5) and are the same ones the `/ask` draft
   event carries, so a sentence looks and behaves identically whether you
   arrived from a fresh draft or a saved agent.

   Tapping a span opens a single-line editor over that span alone; editing
   either one sends the recomposed sentence as
   `PATCH /agents/{id} {nl_definition}`, which recompiles `spec` and returns
   fresh spans. **Fallback:** when `spec` is null — a compile failure — all
   three fields are null, so render `nl_definition` as plain unstyled text with
   the compile error beneath it, and let a tap edit the whole sentence. That is
   the only state in which the underlines are absent.
2. Four disclosure sections, each padding `15 / 22 / 14` + bottom divider, the
   whole header row tappable. Header: eyebrow 10 pt uppercase accent `0.1em` +
   right summary 12 pt `ink55`. Expanded-content top margin is **per section**:
   Invocation 14, Inputs 13, Tools 6, Settings 13.
   - **Invocation** — summary `Automatic | Manual | Scheduled` from
     `spec.trigger.kind` (`mail | manual | schedule`). Radio list, **in the
     mockup's order**, each row `11 / 0` with a top divider, gap 11, radio 16 pt
     with margin-top 3, label 15 pt above a hint 12 pt `ink55`:

     | Label | Hint | `trigger.kind` |
     |---|---|---|
     | When mail arrives | Runs on its own, on matching mail | `mail` |
     | Only when I ask | You tap Run on the agent | `manual` |
     | On a schedule | Daily, weekly, or a custom repeat | `schedule` |

     Then a bordered filter table (top 12, radius 4, padding `12 / 13`), rows
     `6 / 0` at 13 pt with top dividers between them, label column 66 pt wide
     `ink60`. **Read-only** — `spec` is compiled from the sentence, so the
     table displays and never edits.
     `v1 →` **two rows, not three.** From ← `filters.from_domains` +
     `from_contains`, About ← `subject_contains` + `body_contains` +
     `trigger.semantic`; an empty value renders "Anyone" / "Anything" in
     `ink62`. The mockup's **To** row ("Any inbox") is dropped: v1 has one
     account and `filters` has no recipient field.
     `v1 →` **"＋ Add a schedule as well" is hidden** — an agent has exactly one
     `trigger.kind`, so a mail trigger plus a schedule is not representable.
   - **Inputs** — summary "Prompt" (`v1 →` the mockup's static
     "Prompt · 1 attachment"; there are no attachments). Bordered prompt box
     (radius 4, padding `11 / 12`, 14 pt, line-height 1.6) bound to
     `spec.instruction`.
     `v1 →` **the attachment tag and "＋ Attach" are hidden** — attachments are
     not supported.
   - **Tools** — summary = the selected names joined by ", ", or "None".
     Checkbox rows `11 / 0` with a top divider, gap 10, 14 pt: tick glyph in a
     14 pt column (always accent; the glyph is "✓" or empty) + name + right
     note 11 pt `ink60`. Binds `allowed_tools`.
     `v1 →` the mockup's Notion / Slack / Web access / Calendar are replaced by
     v1's real tool set (parity map):

     | Row | `allowed_tools` id | Note |
     |---|---|---|
     | Search mail | `search_mail` | your synced mail |
     | Read thread | `read_thread` | one thread in full |
     | Write note | `write_note` | into Notes |
     | Draft reply | `draft_reply` | saved in NADE |
   - **Settings** — summary `{Status} · asks first | · runs alone`. "Status"
     14 pt + a right-aligned segmented Draft/Published (`status`); then, top 16
     / padding-top 14 / top divider, "Ask before it acts" 14 pt with the hint
     "You approve each draft or change" 12 pt `ink55` and a right toggle
     (`approval_required`); then, same 16/14/divider rhythm, an italic 12 pt
     `ink55` line-height 1.6 note.
     `v1 →` the note's two strings become "Nothing is saved without your tap."
     (on) and "This agent saves notes and drafts on its own." (off). The
     mockup's "Nothing leaves your account without your tap." and "This agent
     will send and file on its own." both describe outbound actions v1 does not
     take (§4).
3. Footer `18 / 22 / 30`, gap 10: "Run once now" (primary, flex 1) →
   `POST /agents/{id}/run`, + "Delete" (secondary) → `DELETE /agents/{id}`.

### 1d — Schedule sheet · `ScheduleSheet`

Bottom sheet over the builder, the whole parent dimmed with
`neutral-900 @45%` — including its nav bar, which the mockup redraws with
Cancel/Save in `ink55` rather than accent, and its body at opacity 0.35.
Sheet: `surface` fill, 1 pt top divider, radius 7 on the **top corners only**,
padding `16 / 22 / 34`, shadow lg, pinned to the bottom edge. Grabber 38 × 4
`neutral-400` centred, margin-bottom 16. Title "How often" Cormorant 600 20 pt,
margin-bottom 18.

1. **Repeat every** — row `align-items: center`, gap 12, padding-bottom 16,
   bottom divider. Label 14 pt · `−` and `＋` boxes 30 × 30 (1 pt divider
   border, radius 4, accent glyph) around a count (min-width 22, centred,
   tabular, 15 pt) · right-aligned unit chip (1 pt divider border, radius 4,
   padding `6 / 12`, 14 pt, inner gap 8) with a 10 pt accent ▼.
   The count clamps to **1…30**. The chip **cycles day → week → month** on tap;
   it is not a menu.
2. **Repeat on** — padding `16 / 0`, bottom divider. Label 12 pt `ink55`,
   margin-bottom 11. Seven circles in a flex row, gap 8, each `flex: 1`,
   `aspect-ratio: 1`, radius 50%, 12 pt, labelled `S M T W T F S` **starting
   Sunday**.
   - selected: 1 pt accent ring, **`accent-100` fill, `accent-800` text**
   - unselected: 1 pt divider ring, transparent fill, **`ink62`** text
   **Always visible, at every unit** — the mockup renders this block
   unconditionally and applies no disabled styling. Only the persisted value
   and the summary are unit-sensitive: `byweekday` is written **only when the
   unit is week** (API.md §5.2) and is sent as `[]` for day and month.
3. **At** — `+ Added`: the mockup has no time control at all, yet the schedule
   model carries `at` and the builder's sentence says "Every weekday at 8:00",
   so without this row the time is unsettable. Padding `16 / 0`, bottom
   divider, styled as the rows above it: label "At" 12 pt `ink55` with
   margin-bottom 11, value right-aligned, tabular, 15 pt in ink, rendering
   `schedule.at` as 24-hour `HH:MM`. The whole row is the tap target; it
   expands **inline below the label**, inside the sheet, into a
   `DatePicker(.hourAndMinute)` in `.wheel` style, and the sheet grows.
   `tz` has **no control in v1**: it is captured from the device's
   `TimeZone.current.identifier` when the agent is created and never shown
   (API.md §5.2). `bymonthday` likewise has no control and is always sent as
   `null`.
4. **Ends** — padding `16 / 0 / 18`, **no** bottom divider. Label 12 pt `ink55`,
   margin-bottom 8. Three radio rows `9 / 0`, gap 11, `align-items: center`:
   radio 16 pt / 1.5 pt, label 14 pt, right value 13 pt tabular. The labels are
   **"Never" / "On" / "After"** — the value column carries the rest ("",
   "6 Jan 2027", "13 runs"). Value color is **ink when that option is
   selected and `ink62` when it is not** — it is the muted state that is
   coloured, never the selected one.
5. Footer: top divider, padding-top 16, gap 10: "Cancel" (secondary, **flex 1**)
   + primary (**flex 1**) whose label **is the recurrence summary**.

The summary string, exactly as the mockup computes it:

```
"Every " + (interval > 1 ? "{interval} {unit}s" : "{unit}")
        + (unit == week && any day selected
             ? " · " + (exactly Mon–Fri ? "weekdays" : "Mon Tue Wed …")
             : "")
```

"Every week · weekdays" (the default), "Every day", "Every 3 months",
"Every week · Mon Wed". **It never contains a time.** Do not write
"Every weekday at 8:00" on this button — that sentence belongs to the builder
behind the sheet, which renders `nl_definition`.

Maps onto `agents.schedule` jsonb (PLAN §Schedule model, API.md §5.2):

| Control | Field |
|---|---|
| unit chip | `freq` — `day` \| `week` \| `month` |
| count | `interval`, 1–30 |
| day circles | `byweekday`, written only when `freq == "week"`, else `[]` |
| At row | `at`, `"HH:MM"` |
| — none — | `tz`, from the device at creation |
| — none — | `bymonthday`, always `null` in v1 |
| Ends radios | `ends.kind` + `ends.date` / `ends.count` |
| — none — | `runs_done`, server-maintained, read-only |

### 1e — Mail list · `MailListView`

Header `60 / 22 / 10`, baseline-aligned, gap 10, **no bottom divider** (the chip
row carries it): the selected chip's name Cormorant 600 23 pt + right a 7 pt
accent dot + "2 accounts" 11 pt uppercase `ink60` `0.06em`, gap 7.
`v1 →` **single account — the dot + count are replaced by the account's
address** at the same 11 pt uppercase `ink60` (parity map).
`+ Added` **a leading `‹` in accent, at the title's own 23 pt**, as the first
item of the same baseline row. The mockup draws 1e as an artboard with no way
in or out; v1 makes 1g the tab's root and pushes 1e from it, so the screen needs
a way back. Putting the chevron inside the existing row costs no height and
keeps the header's "no bottom divider" rule intact. Swiping from the edge pops
too — UIKit disables that gesture whenever the navigation bar is hidden, and
every screen here hides it, so it is restored explicitly.

Chip row: horizontal scroll, gap 8, padding `6 / 22 / 12`, bottom divider;
chip = `flex: none`, padding `5 / 12`, radius 999, 12.5 pt.

| Chip | Ring | Text | Fill |
|---|---|---|---|
| selected | accent | **`accent-800`** | `accent-100` |
| else | divider | **`ink62`** | transparent |

Optional smart-rule caption row (`10 / 22`, italic 12 pt `ink55`, bottom
divider). `v1 →` it renders the selected mailbox's `name` only when
`kind == "user"`; there are no rule sentences to show (parity map).

Rows: padding `13 / 22 / 13 / 14`, bottom divider, gap 9:
- 6 pt unread dot (accent when `unread`, `clear` when read), margin-top 8.
- Column (`flex: 1`, `min-width: 0`): `from_name` 15 pt (weight **600** when
  unread, 400 when read) + right `listTime(ts)` 11 pt tabular `ink62`;
  `subject` 14 pt top 2; `snippet` 13 pt line-height 1.5 `ink55` single-line
  truncated, top 2; `agent_note` when non-null, 11 pt accent with a 1 pt
  `accent-300` underline (padding-bottom 1), top 7.
- `v1 →` **omits** the vertical account label on the right edge (10 pt
  uppercase `ink55` `0.05em`, `writing-mode: vertical-rl`) — one account.

Binds: `GET /mailboxes` for the chips, `GET /mailboxes/{id}/threads` for rows.

### 1f — Thread · `ThreadView`

Nav `60 / 20 / 10` + divider, `align-items: center`, gap 12, 13 pt:
"‹ {mailbox_name}" accent.
`v1 →` **the right-hand "Archive" and "⋯" are both dropped.** v1 performs no
Gmail mutation, so Archive has nothing to call and the "⋯" menu would be empty
(§4). The nav bar is the back affordance alone.

Body scroll padding `20 / 22 / 8`:
`subject` Cormorant 600 24 pt line-height 1.2; meta row top 10, gap 9, 13 pt:
`from_name` in ink + "· to me" `ink60` + right `listTime(ts)` tabular; `hr`
(1 pt divider) with margin `14 / 0`; message body from `body_text`, 14.5 pt
line-height 1.75, **12 pt between paragraphs**; attachment row top 14, gap 9:
`tag-neutral` with `name` + 11 pt `ink62` tabular size.

**Corrected at P2.** This used to read "12 pt gaps between messages". It is not:
in the mockup that `margin-top: 12` sits between two `<div>`s of *one* message —
both halves of Priya's first mail — so it is a paragraph gap. The mockup draws
one message and has no separator between two.

`+ Added` **messages 2…n repeat the whole `meta → hr → body` unit**, at a 20 pt
top gap (the design's own "new block" gap, from the agent card's `margin-top`).
No new visual vocabulary; the design's, applied again.

The meta row binds to **`messages.first`**, not the newest. The mockup's `9:12`
is `thread.json`'s `messages[0].ts` (`09:12:04Z`) and its body is message one's
text, so the row is the thread's opening line — which is what reading from the
top means. "· to me" renders only when `to` contains the account's address;
`thread_html_only.json` has `to: []` and inventing a recipient there would be
the server's job, which it declined.

`v1 →` the mockup justifies the body with hyphenation
(`text-align: justify; hyphens: auto`). SwiftUI has no justified text without a
custom `TextRenderer` — use leading alignment. Recorded deviation.

**Agent card** — one per entry in `agent_cards`, top 20: 1 pt accent border,
radius 4, padding `14 / 15`; kicker row margin-bottom 9, gap 8, baseline:
`agent_name` 10 pt uppercase accent `0.1em` + right status 11 pt `ink62`;
`summary` 14 pt line-height 1.7; buttons top 12, gap 8.

Status strings (the mockup shows only "waiting on you"; the API returns the run
enum, so the rest are `+ Added` for completeness):
`queued` → "queued" · `running` → "running" · `pending_approval` →
**"waiting on you"** · `waiting` → "scheduled" · `done` → "done" · `failed` →
"failed" · `expired` → "expired" · `skipped` → "skipped".

Buttons render **only** when `status == "pending_approval"` and
`feed_item_id != null`; the client fetches `GET /feed/{id}` for the token,
`actions` and `action_label`, then renders exactly as the feed row does — so
the primary button reads "Save note" / "Save draft", never "Approve" or "Send".

Then an `hr` with margin `20 / 0 / 8` and the footer: italic 12 pt `ink62`,
padding-bottom 10, **"Filed in {mailbox_name} · {account_email}"** — both from
`GET /threads/{id}` (API.md §2).

Bottom bar: top divider, padding `11 / 20 / 30`, gap 10: reply pill + 38 pt
accent circle (see §2).

### 1g — Mailboxes · `MailboxesView`

Header `62 / 22 / 12` + divider: "Mailboxes" Cormorant 600 23 pt + right
`v1 →` **"Settings"** accent 13 pt pushing 1k, in place of the mockup's "Edit"
accent 13 pt (nothing is editable under a single account, and Settings
otherwise has no entry point — see §2).

- Section eyebrow "ACCOUNTS" `16 / 22 / 6`, 10 pt uppercase `ink62` `0.1em`.
  Every later eyebrow on this screen is `20 / 22 / 6`.
- Selected account row: padding `12 / 22`, top **and** bottom divider,
  **accent-100 fill**, gap 10, baseline: 7 pt accent dot, `email` 15 pt above a
  sub-label 12 pt `ink55`, right `unread` 13 pt tabular `accent-800`.
  Unselected rows: no top divider, hollow dot (1 pt `neutral-500` ring), count
  `ink60`.
  `v1 →` **one row, the connected Gmail account; "＋ Add mailbox" hidden**
  (parity map). The sub-label is the account's `status` — "Connected" /
  "Needs sign-in".
- "SMART MAILBOXES" eyebrow + right "＋ New" accent 12 pt.
  `v1 →` **the section is titled "LABELS"**, lists Gmail system categories then
  user labels in `GET /mailboxes` order, and has **no "＋ New"** (parity map).
  Row: padding `13 / 22`, top divider, `name` 15 pt + right count 13 pt tabular
  (accent when `unread > 0`, else `ink60`), and a 12 pt `ink62` line top 3.
  `v1 →` that second line is the mockup's rule sentence; v1 has no rules, so it
  renders `"{total} threads"`.
- "STANDARD" eyebrow + a 2-column grid of Drafts / Sent / Archive / Junk, each
  50% width, padding `12 / 22`, 14 pt, 1 pt divider under every cell and to the
  left of the right-hand column; the whole grid has a top divider. Trailing
  24 pt spacer before the tab bar.
  `v1 →` **only "Sent" ships.** `GET /mailboxes` exposes `SENT` and none of the
  other three (API.md §2: `DRAFT`, `TRASH` and `SPAM` are never exposed, and
  Archive is `[Gmail]All Mail`, which is hidden), so a four-cell grid would be
  three dead cells. Render the one cell full-width at the same metrics.

### 1h — Notes list · `NotesListView`

Header `62 / 22 / 12`, baseline, **no bottom divider**: "Notes" Cormorant 600
23 pt + right "＋" accent 20 pt.
`v1 →` **read-only — the ＋ is hidden** (parity map; agents write notes, people
do not).
Search pill: padding `0 / 22 / 12`; the pill itself is 1 pt divider border,
radius 999, padding `8 / 14`, 13.5 pt, placeholder "Search notes" in `ink60`
→ `GET /notes?q`.
Rows: padding `14 / 22`, **top** divider, **left border 2 pt**; title Cormorant
600 17 pt; meta 12 pt `ink60` tabular top 4.

**The gold rule.** 2 pt left border, **accent when `unread == true`** — an
agent wrote or updated the note and you have not opened it — and **`divider`
otherwise** (the mockup's unselected value; it reads as a faint ledger rail, not
as nothing).

`v1 →` the mockup's live code binds this rule to the **selected** row
(`ring: i === noteId ? ACC : DIV`), which contradicts the screen's own footer
caption, "Their edits show a gold rule until you open them." We follow the
caption: the rule is a state marker, not a selection marker. Notes is a
push-navigation list, so there is no persistent selection to mark, and
`GET /notes` ships `unread` precisely to drive this (API.md §3). `unread` flips
to false on `GET /notes/{id}`.

**Row meta is one shape**, not the mockup's four ("Edited 2h ago · 214 words",
"Edited yesterday", "Clipped by Recipe Clipper · Fri", "Synced with Notion ·
9:12" — word counts, clip verbs and Notion are all things v1 has no field for):

> `{agent_name} · {relTime(updated_at)}` — e.g. "Job Search Tracker · 2h ago".
> When `agent_name` is `null` (a note with no run behind it), and only then:
> `Edited {relTime(updated_at)}`.

Footer caption: top divider, padding `16 / 22`, italic 12 pt `ink60` —
`v1 →` **"Agents write into Notes. Their edits show a gold rule until you open
them."** The second sentence is the mockup's verbatim; the first drops the
mockup's named agents (Recipe Clipper, Job Search Tracker), which are fixture
data, not a v1 guarantee.

### 1i — Note detail · `NoteView`

Nav `60 / 20 / 10` + divider, `align-items: center`, gap 10, 13 pt:
"‹ Notes" accent.
`v1 →` **the Read/Write segmented control is omitted** (not disabled). v1 has
no editor, and a disabled control is an affordance that does nothing (§4). The
mockup's control: right-aligned, options `5 / 12` at 12 pt, selected accent /
unselected `ink62`.

Body padding `16 / 22 / 20`, rendered from `body_md` with
`AttributedString(markdown:)`. The mockup's renderer sets the house style, so
match it: `#` Cormorant 600 25 pt, `##` 19 pt, `###` 16 pt — all line-height
1.15, margin `16 / 0 / 8`; body paragraphs 14.5 pt line-height 1.7,
margin-bottom 11 (justified with hyphenation in the mockup — leading in v1, same
reason as 1f); `---` → a 1 pt divider with margin `16 / 0`; `>` → 2 pt accent left border,
padding-left 12, italic, opacity 0.82, margin `12 / 0`; `- [ ]` / `- [x]` →
a 14 pt square, radius 2, 1 pt border (accent when ticked, divider when not)
with an accent ✓, gap 10, margin `7 / 0`, and struck-through text at opacity
0.45 when ticked; bullets in a list with margin `8 / 0` and padding-left 18,
items `4 / 0`; inline `` ` `` → `neutral-200` fill, padding `1 / 5`, radius 2,
12.5 pt monospace; links `accent-700` with a 3 pt underline offset.

`v1 →` **the bottom formatting bar is omitted** (`#`, **B**, *I*, ☐, `` ` ``,
❝ + right "Saved" — top divider, padding `10 / 20 / 30`, gap 14, 12 pt `ink62`).
It is a write-mode affordance and v1 has no write mode.

### 1j — Calendar · `CalendarView`

`v1 →` **fixture stub. There is no calendar endpoint in v1** (API.md §8): the
tab decodes **`NADE/Fixtures/calendar.json`**, bundled inside the app. Nothing
network-backed sits behind it — no refresh, no sync, no write path — and the
tab must not imply otherwise. The fixture holds exactly the six days the design
shows: header from `month` + `range`, grid from `days[].{weekday, day, items}`,
initial selection from `selected_date`.

Header `62 / 22 / 12`, baseline, gap 10, **no bottom divider** (the grid has a
top one): "August" Cormorant 600 23 pt + range "17 — 22" 13 pt `ink62` tabular.
`v1 →` **the mockup's right-hand ‹ › (accent, 14 pt, gap 14) are omitted** —
the fixture is one week and there is nothing to page to (§4).

Grid: `flex: 1`, 3 columns × 2 rows, top divider; each cell has a right and a
bottom divider, padding `11 / 11 / 10`, and an **inset 1 pt ring** — accent when
selected, `divider` when not — over a fill that is `accent-100` when selected
and transparent when not.
Day header: gap 5, margin-bottom 9, baseline — numeral Cormorant **400** 22 pt
tabular, **accent when the day is selected and ink otherwise**, + weekday
9.5 pt uppercase `ink62` `0.1em`.
Timeline: `flex: 1`, 1 pt divider left border, padding-left 9, column gap 8.
Each item: a 5 × 1 pt accent tick absolutely placed at left −12, top 5; time
9.5 pt `accent-700` tabular `0.03em`; name 11.5 pt line-height 1.25, top 1,
single-line truncated.

Agenda below: top divider, padding `13 / 22 / 8`. Title row gap 8, margin-bottom
8: "{weekday} {day}" Cormorant 600 16 pt + right "Add event" 11 pt accent —
`v1 →` **hidden**, there is nothing to write to. Rows `7 / 0` with a top
divider, gap 12, 13.5 pt: time column 44 pt wide `accent-700` tabular + name.

### 1k — Settings · `SettingsView`

Header `62 / 22 / 12` + divider: "Settings" Cormorant 600 23 pt. Grouped rows,
each `13 / 22` with a top divider (and a bottom divider on the last row of each
group), baseline-aligned: label 14.5 pt + right value 12 pt `ink62` ending in
"›" (part of the string, not a separate chevron). Section eyebrows 10 pt
uppercase `ink62` `0.1em` at `20 / 22 / 6` — except the **first**, which is
`18 / 22 / 6`.

v1 sections:
- **ACCOUNT** — one row: the connected Gmail `email`, value "Connected ›" or
  "Needs sign-in ›" when `status == "needs_reauth"`; a second row "Sign in
  again" (accent, 14.5 pt) appears only in that state, starting the Gmail link
  flow. That is **two** calls, not one: `POST /auth/gmail/link` returns a
  single-use `url`, and the app opens it (`API.md` §1, backend/DECISIONS.md
  D15). Open it with `ASWebAuthenticationSession` or Safari — **never** a
  throwaway `WKWebView`. `start` sets an `HttpOnly` cookie that `callback`
  requires, so a session the app discards loses the cookie and the user is told
  the link expired.
  `v1 →` the mockup's two account rows and its accent "＋ Add mailbox" row are
  gone (parity map).
- **AGENTS** — "Approve before it acts" with the hint "Applies to every new
  agent" (12 pt `ink62`) + a right toggle bound to
  `settings.approval_required_default`, `PATCH /settings` on change; then
  "Run log" / "Last 30 days ›" pushing a list of `GET /runs`
  (`agent_name`, `trigger_kind`, `status`, `summary`, `created_at`).
  `v1 →` the mockup's label is "Approve before **sending**"; v1 never sends, so
  the row is renamed to match 1c's "Ask before it acts" (§4). Changing it does
  not touch existing agents (API.md §8).
  `v1 →` "Connected tools · Notion, Slack ›" is **hidden** (parity map).
- **CONNECTION** — `+ Added`, not in the mockup: server URL + "Pair this
  device ›", which is where the pairing-code entry screen lives (PLAN P3).
  Without it a fresh install cannot reach the backend at all.
- **READING** — "Text size ›" (Dynamic Type passthrough).
  `v1 →` "Swipe actions · Archive, Snooze ›" is **hidden** — v1 has no
  mutations, so neither action exists.
- Footer, padding `18 / 22 / 26`, italic 12 pt `ink62`:
  **`Version {app_version} — {disclosure}`**, with `disclosure` from
  `GET /settings` rendered **verbatim**, giving "Version 1.0 — Your mail syncs
  to your own server and is processed by the AI models you connect."
  `v1 →` the mockup's "Version 1.0 — mail stays on device between runs." is
  false for v1 (PLAN §v1 definition), and the honest sentence is served rather
  than hardcoded so it cannot drift from what the server does (API.md §8).

---

## 4. Copy rules

Every string the UI shows must be true of what v1 does (PLAN C1/C2). v1 takes
**no outbound actions**: no sending, no archiving, no Gmail mutation at all.

- Approval confirms **local** effects only. The primary button renders
  `data.action_label` — "Save note" or "Save draft" — and **never "Send"**,
  "Forward", "Reply-all" or "Archive".
- No affordance that does nothing. This is why 1f's Archive and "⋯", 1i's
  segmented control and formatting bar, 1j's ‹ › and "Add event", 1c's
  "＋ Add a schedule as well" and "＋ Attach", and the mockup's "＋ New" /
  "＋ Add mailbox" / "Swipe actions" rows are all cut rather than disabled.
  The rule cuts an affordance whose *backing* is missing — it is not a licence
  to cut one that is merely inconvenient. "Make this an agent" was cut under
  this rule and has since been restored, because the contract gained the field
  it needed rather than the button being wished away.
- No sentence describing behaviour v1 does not implement — 2b's "Tips fade out
  after the first week", 1c's "Nothing leaves your account without your tap",
  1k's "mail stays on device between runs".
- The mockup's prompt suggestions are all things v1 cannot do (forwarding mail,
  unsubscribing over the web, tracking flight status). `v1 →` replace all three:
  1. "What did Priya say about the design review?"
  2. "Find every receipt from last month"
  3. "When a recruiter emails, note the next steps"
  One per route the classifier produces — `answer`, `results`, `agent_draft` —
  so tapping each demonstrates a different state.

---

## 5. Screenshot QA

Every screen lands with a `simctl` screenshot in `docs/screens/<id>.png`
beside the design render, from the phase it first exists (PLAN C9).

---

## 6. Deviation register

Everything v1 renders differently from `Email App.dc.html`, in one place. `+` is
an addition the mockup does not contain; the rest are substitutions.

| # | Screen | Deviation | Why |
|---|---|---|---|
| 1 | all | SF Symbols for Lucide | parity map |
| 2 | all | light appearance forced | one visual world in v1 |
| 3 | 2a | OTP chip cut | parity map: kind `otp` is post-v1 |
| 4 | 2a/1f | primary button reads `action_label`, not "Approve" | v1 takes no outbound action |
| 5 | 2a | focus state has **no** tips caption (2b has one) | 2a is the live component; v1 has no first-week fade |
| 6 | 2a | **+** agents row lifted from 1a idle | 1a idle is not built and it was the only route to 1b |
| 7 | 1a | idle state not built | turn 2 replaces 1a |
| 8 | 1a | **+** `answer` route rendering | the contract has three routes, the mockup drew two |
| 9 | 1a | progress card cut | no field behind it |
| ~~10~~ | 1a | ~~"Make this an agent" hidden~~ — **ships**, via `route_hint` | resolved: `API.md` §4 gained the field |
| 11 | 1b | **+** `paused` status colour `neutral-500` | `ends.after` produces the state |
| ~~12~~ | 1c | ~~sentence not underlined~~ — **underlined**, via `when_span`/`do_span` | resolved: `API.md` §5 gained the fields |
| 13 | 1c | filter table loses the **To** row | single account; no recipient filter in `spec` |
| 14 | 1c | "＋ Add a schedule as well" hidden | one `trigger.kind` per agent |
| 15 | 1c | attachments hidden | not supported |
| 16 | 1c | v1's four tools replace Notion/Slack/Web/Calendar | parity map |
| 17 | 1c | approval note rewritten | mockup's strings describe sending |
| 18 | 1d | day picker always visible | matches the mockup; the **old spec** was wrong |
| 19 | 1d | **+** "At" row | `schedule.at` was unsettable |
| 20 | 1d | `tz`, `bymonthday` have no control | captured / always null |
| 21 | 1e | single-account header; no vertical account label | parity map |
| 22 | 1e | rule caption shows the label name only | no rule builder in v1 |
| 23 | 1f | Archive and "⋯" cut | no mutations |
| 24 | 1f | leading text, not justified | SwiftUI cannot justify without a custom renderer |
| 25 | 1f | **+** run-status strings beyond "waiting on you" | the API returns eight statuses |
| 26 | 1g | "Edit" → "Settings", pushing 1k | Settings had no entry point in the mockup |
| 27 | 1g | one account row; "＋ Add mailbox" cut | parity map |
| 28 | 1g | "SMART MAILBOXES" → "LABELS", no "＋ New", description line = thread count | parity map |
| 29 | 1g | STANDARD grid reduced to "Sent" | the other three labels are not synced |
| 30 | 1h | gold rule = unread, not selection | the screen's own caption says so |
| 31 | 1h | one meta shape | the four in the mockup need fields v1 lacks |
| 32 | 1h | footer caption generalised; "＋" hidden | fixture agent names; read-only |
| 33 | 1i | segmented control and formatting bar cut | no write mode |
| 34 | 1j | fixture-only; ‹ ›, "Add event" cut | no calendar endpoint |
| 35 | 1k | Settings reached from 1g | the mockup lights no tab |
| 36 | 1k | "Approve before sending" → "Approve before it acts" | v1 never sends |
| 37 | 1k | Connected tools, Swipe actions cut | parity map / no mutations |
| 38 | 1k | **+** CONNECTION section | pairing has to live somewhere |
| 39 | 1k | footer = served `disclosure` | the mockup's sentence is false for v1 |
| 40 | 1a/2a | all three prompt suggestions replaced | the mockup's three are outbound actions |
| 41 | 1e | **+** leading `‹` in the header, and edge-swipe to pop | 1g is the tab root; the mockup gives 1e no way back |
| 42 | 1f | **+** per-message `meta → hr → body` unit at a 20 pt gap | the mockup draws one message; a thread has many |
| 43 | 1f | **+** `partial` caption under the subject, italic 12 pt `ink62` | `API.md` §2: clients must surface a thread with gaps |
| 44 | 1e/1g | **+** empty, unpaired, syncing and offline captions, italic 12 pt | the design draws none, and `mailboxes: []` is a two-minute state on first run |
| 45 | 1e/1g | **+** no spinner in any state | the design has none; the store's first value is synchronous |
| 46 | 1f | ask bar renders as **chrome**, not a control | the mockup's field and circle are `<span>`s; `POST /ask` is P6 |
| 47 | 1f | inline (`cid:`) parts are listed as attachments | P2 does not render `body_html`, so hiding them would lose them entirely. **Revisited at P3 and kept**: "View original" now renders them in place (deviation 57), but its CSP allows `nade-inline:` and nothing else, and a reader who wants the file itself still needs a way to reach it |
| 48 | 1k | **+** `PairingView`, assembled from DS parts | §1k gives the row and no pixels beyond it |
| 49 | all | iPhone portrait only (`TARGETED_DEVICE_FAMILY = 1`) | the design defines one 402 × 874 frame; iPad and landscape had no render and no criterion |
| 50 | 1c | the Invocation radios are **read-only** | `PATCH /agents/{id}` accepts no trigger kind (`API.md` §5); it is compiled from the sentence, like the filter table beside it, so a writable radio would be a control with nothing behind it |
| 51 | 1a | `answer` citation rows are **not tappable** | §1a says tapping opens the thread, but `API.md` §4 gives a source `gmail_id` and `subject` "and nothing else" — a *message* id, which `GET /threads/{id}` rejects. **Closure scheduled**: PLAN P6 adds `thread_id` to `done.sources` (fixture-first at P6); the rows become tappable then |
| 52 | 1a | the draft card's spans edit **locally**, before saving | the card is pre-save, so there is no agent id to `PATCH`; the recomposed sentence is what `POST /agents` sends, which is what makes the lead line's "Tap anything underlined" true |
| 53 | 2a | `{new_count} NEW` renders at zero too | §2a's header is a fixed row; hiding it moved the date's baseline the moment the last approval was answered |
| 54 | 2a | the `edit` button takes the **approve** action | PLAN §Approval semantics: "approve creates/updates the draft; `PATCH /drafts/{id}` edits it after". There is no pre-approval edit flow in v1 and no drafts surface until P7, so Edit saves and the editing lands later |
| 55 | 2a | pull-down ⇄ swipe-up is a **tap** on the grabber | §2a lists the drag as P7 "time-allowing"; both states and the transition ship now, the gesture does not |
| 56 | 1f | **+** "View original" toggle, and attachments are tappable | PLAN P3's iOS line. The WKWebView is locked: no JS, CSP `default-src 'none'` with `img-src nade-inline:` and no `http(s):` source of any kind, link previews off, non-persistent store, and every navigation but the first `about:` load cancelled |
| 57 | 1f | inline images load over a private `nade-inline:` scheme | this is what actually closes 47. The server rewrites `cid:` to a relative `/v1/messages/…/attachments/…` before the client sees it, so an `img-src cid:` allowance matched **nothing** and the relative URL resolved against `about:blank` — every inline image was a broken box. `WKWebView` cannot attach a bearer to a subresource, so a `WKURLSchemeHandler` fetches the bytes through the authenticated client and sniffs the type from the bytes rather than trusting the message |
| 58 | 1f | ask-bar placeholder → "Ask for a draft or an answer…" | the mockup's "Reply, or ask for a draft…" promises replying, which §4's own rule forbids for v1 (no outbound actions) |
| 59 | 1c/1d | schedule-triggered agents compose `{When_span}, {do_span}.` with **no "When" prefix**, span capitalised | the mockup's 1d parent sentence ("Every weekday at 8:00, build…") is the authority; §1c's unconditional "When {when_span}, …" contradicted §1d, and "When every weekday at 08:00" is not English. Mail/manual agents keep `When {when_span}, {do_span}.` |
| 60 | 2a/1f | **+** minimal draft sheet (P7): editable `body_text` over `PATCH /drafts/{id}`, from the feed card's Edit and the thread agent card | the mockup draws no draft editor, yet Edit = "approve, then edit the draft" is v1's only edit path — without a surface it dead-ends |
| 61 | 1f | **+** thread-scoped ask renders the 1a states in a sheet over 1f (P6) | the mockup gives 1f an ask bar and no response surface; the 1a states already exist, so they are reused rather than a new vocabulary invented |

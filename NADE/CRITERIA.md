# P1 [ios] — acceptance criteria + edge-case checklist

Written **before** any code, per PLAN.md §Execution doctrine step 1. Every edge
case below is either a test (named here) or an `// EDGE:` comment beside the
code that handles it.

Revised 2026-08-17 after an adversarial review. Several claims in the first
version were not true — a lint that "nothing routes around", font tests that
proved rendering, geometry tests that proved geometry, a shell later phases
could build on. Where a claim has been narrowed, the narrowing is stated rather
than quietly dropped, and §D records what is still **not** claimed.

Revised again 2026-08-18 after a second review. This time every claim was
checked by **planting the regression it says it catches and watching the test go
red** — not by reading the test. Twelve geometry values turned out to be
asserted only against themselves, the SSE validator accepted three framings
`docs/contract/validate.py` rejects, the deployment-target sweep undercounted
its own configurations, and one screenshot was a byte-identical copy of another
under a different name. All fixed; the red output for each is in the lane
report, and IOS_DECISIONS.md D39–D45 records the reasoning.

## A. Acceptance criteria

| # | Criterion | How it is checked |
|---|---|---|
| A1 | `xcodebuild -list -project NADE.xcodeproj` shows targets `NADE`, `NADETests`, `NADEUITests` and a **shared** scheme `NADE`. | acceptance cmd 1 |
| A2 | `xcodebuild -resolvePackageDependencies` resolves GRDB 7 and writes `Package.resolved`. | acceptance cmd 2 + file exists |
| A3 | `xcodebuild … clean build test` on iPhone 17 Pro ends `** TEST SUCCEEDED **`, twice consecutively, with no warning this lane introduced. | acceptance cmd 3 |
| A4 | `IPHONEOS_DEPLOYMENT_TARGET = 18.0` in **every** build configuration — eight of them: the project and all three targets, Debug and Release each. | `InfoPlistTests.testEveryBuildConfigurationTargets18` parses `project.pbxproj` and requires `settings == XCConfigurationLists × 2`, so a count is **derived**, not guessed (the previous floor of "at least 6" let two of the eight be deleted silently — verified). `testNoConfigurationDefersToAnXcconfig` requires no configuration to have a `baseConfigurationReference`, which is what makes those literals the effective values. `testTheBuiltConfigurationTargets18` covers the built bundle, and only ever sees Debug. |
| A5 | `NADE/Info.plist` exists, is the target's `INFOPLIST_FILE`, `GENERATE_INFOPLIST_FILE` stays `YES`, and the built `Info.plist` contains `UIAppFonts` (4 files) and `NSAppTransportSecurity.NSAllowsLocalNetworking = true`. | `InfoPlistTests` reads `Bundle.main` |
| A6 | All four bundled faces resolve by PostScript name, are **not** the system face, and are what `Theme.Font.heading` / `.body` actually render in. | `FontLoadTests` (registration, `UIAppFonts`, the TTFs' own PostScript names, and the `tnum` **monospaced-numbers selector** by both id **and name** — selector 4 "Default" is present in all four faces, so "a selector exists" is not the check) + `RenderedFaceTests` (renders `Text` through the Theme API and compares the resolved size with Core Text's in the expected face, and separately with the system face's). Verified: with `Theme.Font.heading` swapped for `Font.system`, all 8 registration tests pass and all 4 rendering tests fail. |
| A7 | Theme tokens resolve to the exact RGBA from DESIGN.md; space/radius scales are exact. | `ThemeTests` — **every** step of the neutral, accent and accent-2 ramps, not only the endpoints. Verified against a one-unit (1/255) drift on six steps, including the case where both ramps' 200 step is wrong *and* identical, which the old "two components differ" check could not see. |
| A8 | No raw **colour** and no raw **font** anywhere in `NADE/**` except `NADE/Theme.swift`. | `NoRawValuesTests`. Scope is deliberate and narrower than the original claim — see §D1. Non-vacuous: `testTheLintCatchesEveryEvadedForm` plants 17 violating snippets, and seven of them (`Font.custom`, a split `.font(.custom(`, `Font.system`, `Color(red:green:blue:)` inline and split, `Color.red`, `.foregroundStyle(.secondary)`) were also planted in a **real** component file and caught. The exemption is by path; see §D7 for why a second `Theme.swift` cannot exist here anyway. |
| A9 | App launches to a 4-tab bar; tapping each tab changes the selection; the bar reads as a tab bar. | `NADEUITests/TabBarUITests` — existence, launch selection, selection changes, per-tab screens, the container role and label, "Tab N of 4", and each column's 44 pt frame |
| A10 | `docs/screens/p1-*.png` exist, cover **every** gallery section including AX5 and Reduce Motion, and match the Classical DS. | 22 files, all 13 gallery sections plus both shells, on iPhone 17 Pro and iPhone SE (3rd gen); `simctl io screenshot` + reading every image back. **Checked for duplicates by hash — 22 files, 22 distinct.** That check is not ceremony: `-13-motion.png` was byte-identical to `-12-ax5b.png`, because `motion` is the last section and `scrollTo(anchor: .top)` clamped at the end of the content (IOS_DECISIONS D45). Reading the images also caught a system notification banner over the SE shell. |
| A11 | Fixtures in `docs/contract/` are readable from `NADETests`, and the SSE streams are well-formed. | `ContractFixturesTests` — the set is **enumerated from the directory**, cross-checked both ways against the bundle, and each stream is parsed against `docs/contract/README.md`'s rules **in blocks split on the blank line**, mirroring `docs/contract/validate.py`'s `parse_sse`. A line-by-line reader cannot see framing at all and accepted three streams `validate.py` rejects. Payload *shapes* are not checked — see §D7. |
| A12 | App is locked to light appearance. | `.preferredColorScheme(.light)` at the root of `NADEApp`. Not directly assertable from a test (the modifier is not introspectable), but every token is an absolute sRGB value rather than a system semantic colour, so nothing can flip even without it — `ThemeTests` pins the exact RGBA of each token, and the screenshots confirm the rendered result. |
| A13 | Switching tabs does not rebuild a screen. | `NADEUITests/ShellStateUITests` — a counter on each screen survives a full rotation through all four tabs |
| A14 | Every control's drawn box is the design's size **and** its hit region is at least 44 × 44. | `ComponentGeometryTests` measures the drawn box — by layout where `sizeThatFits` can see it, and **by pixels where it cannot** (the toggle knob inside its track, the stepper's count under its `min-width`, the disabled opacity, pill vs `radius-md` corners, the AX5 glyph inside its circle). `HitTargetUITests` reads the hit frame in the running app and taps outside the drawn edge. Both halves are needed: either one alone passes for the wrong reason. Verified: thirteen constants were left in place while the views rendered something else, and every one now fails. |

## B. Edge-case checklist (doctrine minimum + this lane's own)

| # | Edge case | Handling | Where |
|---|---|---|---|
| E1 | **Dynamic Type at the largest accessibility size** (`AX5`) | Every text style is `Font.custom(_:size:relativeTo:)`, so it scales; `Font.custom(_:fixedSize:)` is banned and linted. Chrome that cannot grow without breaking (tab bar, toggle, stepper, segmented, tag, chip, **and an icon button's glyph — its box is fixed**) is clamped with `.dynamicTypeSize(…DynamicTypeSize.accessibility1)`; the tab-bar label additionally gets `minimumScaleFactor(0.7)` + `lineLimit(1)`. Content (cards, body copy, radio rows, text buttons) is never clamped. `NRadioDot` scales via `@ScaledMetric` with its ring widths proportional to the dot. | `Theme.swift` `Metrics.chromeTypeCeiling`, `NButton.clampsGlyph(for:)`, `NTabBar`, `NToggle`, `NStepper`, `NSegmented`, `NTag`, `NChip`, `NRadioDot`; `RenderedFaceTests.testThemeFontsGrowWithDynamicType`; `ComponentGeometryTests.testIconButtonBoxDoesNotGrowAtAX5` **and `testTheIconButtonsGlyphStaysInsideItsCircleAtAX5`** — the second is the one that fails when the ceiling is removed (the first measures the box, which a `.frame` pins regardless, and a hand-clamped `NIcon`, which is not the button); the gallery's own **`ax5` / `ax5b` sections**, both screenshotted |
| E2 | **Smallest supported width** (iPhone SE-class, 320–375 pt) | No fixed widths on containers; the tab bar is four `maxWidth: .infinity` columns with a shrinking label; the chip row scrolls horizontally; every text component wraps or truncates rather than forcing a width; `NSegmented` compresses instead of overflowing. | all components; `ComponentGeometryTests.testSegmentedCompressesRatherThanOverflowingASmallScreen`, `testTabBarFitsTheNarrowestSupportedScreen`; verified by running the gallery and the shell on `iPhone SE (3rd generation)` |
| E3 | **Very long unbroken string** (a 200-char token, a long URL) | Every label that can receive user data gets `.lineLimit` + `.truncationMode(.tail)`, or `.fixedSize(horizontal: false, vertical: true)` where wrapping is wanted. No component sizes itself from its text width in a way that can push the row off-screen. | `NCard`, `NTag`, `NChip`, `NButton`, `NRadioRow`, `NSegmented`; gallery has an explicit "pathological strings" block |
| E4 | **Empty string** | Components must not collapse to zero height or draw a 0-width pill. Minimum heights are on the container, not the text. | `NTag`, `NChip`, `NButton`, `NTextField` placeholder; `ComponentGeometryTests.testEmptyButtonKeepsItsBox`; gallery renders each with `""` |
| E5 | **RTL layout** | No `.left`/`.right` anywhere; no hard-coded `x` offsets that assume LTR — `NToggle`'s knob is a `ZStack(alignment: .leading/.trailing)` + `.padding(3)`, not a fixed `offset(x:)`, so it mirrors for free. Every stack uses leading/trailing alignment. | `NToggle`, `NStepper`, `NSegmented`, `NTabBar`; gallery has an `.environment(\.layoutDirection, .rightToLeft)` block, screenshotted |
| E6 | **VoiceOver labels on every interactive element** | Every control declares `.accessibilityLabel`, the right `.accessibilityAddTraits`, and `.accessibilityValue` where it has state. **No control can end up with an empty label**: `NButton`/`NChip` fall back to the trait's noun. **No visible value is silent**: `NRadioRow`'s trailing value is the element's `accessibilityValue`. Decorative strokes, dots and glyphs are `.accessibilityHidden(true)`. The tab bar is a **container** with the `isTabBar` trait, a label, and per-tab "Tab N of 4". | every component; `ThemeTests` pins the composition, `NADEUITests/AccessibilityUITests` and `TabBarUITests` read it back out of the running app |
| E7 | **Reduced motion** | The only animation in P1 is `NToggle`'s 0.18 s knob ease. It is wrapped in `Theme.Motion.toggle(reduceMotion:)` which returns `nil` when `\.accessibilityReduceMotion` is on. That value is read-only, so it cannot be forced in the gallery — both branches are unit-tested instead, and the gallery's `motion` section documents it. | `NToggle`, `Theme.Motion`; `ThemeTests.testToggleAnimationRespectsReduceMotion` |
| E8 | **Missing / mis-registered font file** — must fail **loudly** | `FontLoadTests` asserts (a) `UIFont(name:size:)` is non-nil for all four PostScript names, (b) the resolved `familyName` is the expected face, (c) it is **not** the system family, (d) `UIFont.familyNames` contains both families, (e) the four filenames are in `Bundle.main` **and** in the built `UIAppFonts`, (f) each TTF declares the PostScript name we ask for. Silent system fallback cannot pass. | `NADETests/FontLoadTests.swift` |
| E9 | **Hairlines are 1 pt** | The mockup frame is 402 CSS px and the device is 402 pt, so `1px` is 1 pt; DESIGN.md says the same. `Hairline`/`VHairline` and `Theme.Stroke.border` are all `1`. **This reverses P1's original ⅓-pt decision** — see IOS_DECISIONS.md D9 for why that reasoning was wrong. | `Theme.swift`; `ComponentGeometryTests.testHairlineIsOnePoint` measures a hosted `Hairline` |
| E10 | **Tabular numerals** | `.monospacedDigit()` helper (`.tabularNumerals()`) applied at every count/time site. Both families are asserted to expose the **monospaced-numbers selector** by id *and* name, and — the part that actually matters — the substitution is asserted **on the render**: ten `1`s and ten `0`s must come out the same width with it on, and *different* widths with it off (they are 43 pt apart in Lora at 17 pt, 25 pt in Cormorant), so the test cannot pass because the faces were already monospaced. | `Theme.swift`, `FontLoadTests.testBothFamiliesOfferTheMonospacedNumbersSelector`, `RenderedFaceTests.testTabularNumeralsActuallySubstitutesInBothFamilies`, gallery |
| E11 | **Disabled + pressed button states** | `NButton` is a `ButtonStyle`, so pressed state comes from `configuration.isPressed`; disabled comes from `\.isEnabled` (opacity 0.45, DS `.btn:disabled`). Both are rendered in the gallery. | `NButtonStyle` |
| E12 | **Dark mode** | Out of scope for v1 (DESIGN.md §Color). Root forces `.preferredColorScheme(.light)`; every colour is an absolute sRGB value, never a system semantic colour, so nothing can flip. | `NADEApp`, `Theme.Color` |
| E13 | **Font file present but not listed in `UIAppFonts`** | Covered by E8(e): the test reads the *built* `UIAppFonts` array and cross-checks it against the bundled files. | `FontLoadTests.testUIAppFontsMatchesBundledFiles` |
| E14 | **Gallery leaking into Release** | The gallery route is `#if DEBUG` only; a release build has no path to it. That is not a licence for the gallery to say untrue things — it is still the Phase 1 artefact (E18). | `NADEApp` |
| E15 | **`docs/contract` fixture missing, renamed, stale or malformed** | The fixture set is **enumerated from `docs/contract/`**, not listed by name, and cross-checked both ways against the test bundle. SSE streams are parsed **in blank-line-delimited blocks**, as `validate.py` does, and checked against README.md's stated wire format: each event exactly one `event:` line and one `data:` line, `route` first and once, every payload valid JSON, exactly one terminal `done`/`error` and it is last, trailing blank line. | `ContractFixturesTests`; `testTheStreamValidatorRejectsEveryMalformedShape` feeds it **14** malformed streams — 11 that passed the original substring check, plus 3 framing errors that passed the line-by-line parser that replaced it |
| E16 | **Source-tree lint runs where there is no source tree** (CI artefact-only run) | `NoRawValuesTests` and `ContractFixturesTests` derive the repo root from `#filePath`; if the tree is absent they `XCTSkip` with a clear message rather than silently passing. | `NoRawValuesTests`, `ContractFixturesTests` |
| E17 | **Touch targets below 44 pt** | DESIGN.md draws controls smaller than the HIG minimum and the pixels must not change, so `.nadeHitTarget()` grows the *tappable* region in an overflowing background that contributes nothing to layout; the tab bar instead moves its own padding inside each column. Not applied to `NRadioRow`, whose rows stack edge to edge — see IOS_DECISIONS.md D28. | `Theme.swift` `nadeHitTarget`, `NButton`, `NToggle`, `NStepper`, `NChip`, `NSegmented`, `NTabBar`; `HitTargetUITests` |
| E18 | **The artefact must not claim something v1 does not do** | v1 takes no outbound action, so nothing says "Send" (DESIGN.md §4). The ask-field button is "Ask". | `GalleryView`; `AccessibilityUITests.testNothingInTheGallerySaysSend` |
| E19 | **Switching tabs must not reset a screen** | All four screens stay in the view tree; the inactive three are transparent, untappable and hidden from VoiceOver. | `RootTabView`; `ShellStateUITests` |

## C. Out of scope for P1 (explicitly not built)

Mail, feed, agents, notes, calendar and settings screens (P2/P3/P7); networking;
GRDB schema/records (P2); SSE; the ask field's behaviour. `RootTabView` hosts
four **placeholder** screens whose only job is to be replaced without the shell
changing.

## D. What is *not* claimed

Stated plainly, because the first version of this file over-claimed and the
review was right to say so.

1. **The lint does not police geometry.** `NoRawValuesTests` enforces no raw
   colour and no raw font outside `Theme.swift`. Sizes, paddings and gaps live
   beside the component they belong to as named `static let`s — they are
   per-component design facts, not shared tokens. `ComponentGeometryTests` is
   what keeps them honest, by measuring them.
2. **`.isTabBar` does not make XCUITest report a `tabBar` element.** The trait is
   applied; XCUITest still reports the container as `Other`, so the UI test
   queries it by identifier. What is asserted is that the container exists, is
   named, groups exactly the four tabs, and that each announces its position.
3. **SwiftUI's box model is not CSS's.** `strokeBorder` draws inside the frame
   where CSS `border` adds to an auto height, and SwiftUI's `TextField` reserves
   a slightly taller line box than CSS. The two mostly cancel; the residual is
   ≤ ~1.1 pt on any control, and the geometry tests state their tolerance
   (±1.0–1.5) rather than hiding it. IOS_DECISIONS.md D32.
4. **There is no snapshot-diffing harness.** Screenshots are compared with the
   mockup by eye. The gallery's section anchors would make a deterministic
   snapshot suite cheap to add if drift becomes a problem. What *is* automated
   is narrower: individual measurements (layout and pixel), not whole frames.
5. **`.preferredColorScheme(.light)` is not directly asserted** (A12).
6. **Justified body text** (1f) has no SwiftUI equivalent; recorded as a
   deviation, resolved when P2 builds `ThreadView`.
7. **The contract fixtures' payload *shapes* are not checked.**
   `docs/contract/validate.py` validates every field against `docs/API.md`. P1
   has no models, so `ContractFixturesTests` checks reachability, valid JSON and
   stream **framing** only. P2 decodes them, and that is when the shapes get
   asserted on this side.
8. **The deployment-target sweep cannot see a command-line `-xcconfig`.** It
   reads the literal values in `project.pbxproj` and requires that no
   configuration attaches an `.xcconfig` of its own. A test runs inside the
   simulator; nothing there can reach `xcodebuild -showBuildSettings`.
9. **The lint's path exemption guards a case this target cannot reach.** A
   second `NADE/**/Theme.swift` fails the *build* ("Multiple commands produce
   Theme.stringsdata") because `NADE/` is a synchronized file group. The
   exemption is still by path — it costs nothing — but the protection is the
   build system's, not the lint's.
10. **Not every component is pinned to its face.** `NButton`, `NTag` and `NChip`
    are (their widths are measured against a label rendered in the expected
    face). `NCardTitle`, `NCardBody` and the tab-bar label are pinned by size
    and geometry but not by face: swapping Cormorant for Lora inside one of them
    would change how it looks without failing a test. `RenderedFaceTests` covers
    the `Theme.Font` API itself, which is where such a swap would normally
    happen.
11. **Some tokens are pinned as constants and never rendered.**
    `Theme.Shadow.*`, `Theme.Stroke.rule` and `Theme.Radius.sm`/`.lg` are used
    only by the gallery's own token swatches — no shipping component draws them
    yet, so there is nothing to measure. Two that *are* drawn and still are not
    measured: `Theme.Radius.tag` (3 pt, `NTag`) — the corner test covers
    `NTextField`'s pill-vs-`radius-md` only — and
    `Theme.Metrics.TabBar.labelTracking`, because tracking changes a width and a
    tab column's label is `accessibilityHidden`, so neither a layout measurement
    nor XCUITest can reach it. (`NTag.tracking` *is* covered, as a side effect of
    measuring the tag's width against its rendered label.)
12. **Opacity is measured at one pixel, not over the render.** Text is smoothed
    with a contrast-dependent gamma, so an ink total over a whole control is not
    linear in its opacity. `NButton.disabledOpacity` is solved from a fully
    covered border pixel; other opacities in the design are token values pinned
    by `ThemeTests` and are not separately measured on a render.

---

# P2 [ios] — acceptance criteria + edge-case checklist

Written **before** the code, per PLAN.md §Execution doctrine step 1. Same rules
as P1 above: every criterion names the test that proves it, every edge case is
a test or an `// EDGE:` comment beside the code, and §D says plainly what is
*not* claimed.

**Scope note.** PLAN.md specified this lane "on fixtures", with networking at
the P5 gate and Settings at P3. It ships **live** instead — a URLSession client,
pairing with an origin-bound Keychain credential, and 1k's CONNECTION + ACCOUNT
sections. `docs/PLAN.md` §P2 carries the full moved/not-moved table; this file
carries the criteria that expansion owes.

## A. Acceptance criteria

| # | Criterion | How it is checked |
|---|---|---|
| A1 | `clean build test` on iPhone 17 Pro ends `** TEST SUCCEEDED **`, **twice consecutively**, with no warning this lane introduced. | acceptance cmd 1 |
| A2 | The same on `iPhone SE (3rd generation)`. | acceptance cmd 2 |
| A3 | Every mail and auth fixture in `docs/contract/` decodes into the wire models with **non-optional** properties wherever `API.md` says non-null. A missing key throws; a `null` in a non-null field throws; an unknown *extra* key does not. | `WireDecodeTests` — values asserted, not "no throw"; the two planted-payload tests are what make "non-optional" mean something |
| A4 | The wire time formatter accepts `2026-08-16T09:12:04Z` and **rejects** `…04.123Z`, `…04+00:00` and `…04`. | `WireTimeTests` |
| A5 | The fixtures the app bundles are byte-identical to `docs/contract/`, and the set is complete. | `FixtureParityTests` — asserts the *manifest*, then compares repo→repo and repo→`Bundle.main` bytes. A per-file loop alone is vacuous over an empty directory. (Ten, not nine: `search.json` joined because it is the only page in the contract carrying the partial thread's row, and a detail cannot be stored without one) |
| A6 | A **Release** `.app` contains no mail fixture. | `scripts/assert-release-has-no-fixtures.sh` — resolves `BUILT_PRODUCTS_DIR` from `xcodebuild -showBuildSettings` and **exits non-zero**. Not a test: tests only ever run Debug (§D2 of P1 restated) |
| A7 | The schema enforces `NOT NULL` on every column the wire marks non-null, plus its foreign keys and cascades; a throw mid-write leaves the store unchanged. | `MailStoreTests` — behaviour, not `sqlite_master` introspection |
| A8 | Re-applying a page is a no-op; the last page sets `reached_end`; **a list refresh preserves detail columns and a detail write preserves list columns**. | `MailStoreTests` — both directions. One direction alone is how an opened thread silently becomes "never loaded" again |
| A9 | `ValueObservation` delivers on the main actor, and a write **inside the tracked region that leaves the fetched value equal** does not re-emit. | `MailObservationTests`. Stated narrowly on purpose: GRDB's region tracking already suppresses unrelated-table writes, so "an unrelated write does not emit" would stay green with `.removeDuplicates()` deleted |
| A10 | 1e/1f/1g/1k geometry is the design's, measured on a **render** against design constants, in **points** throughout. | `MailGeometryTests` (D33/D40/D49) |
| A11 | Pushing a thread hides the tab bar; pushing 1e or 1k does **not**; a launch-argument-restored thread survives a tab rotation. | `ThreadNavigationUITests` |
| A12 | No `import GRDB` outside `NADE/Store/`; no `URLSession` outside `NADE/API/`. | `ModuleBoundaryTests` |
| A13 | Five OFL faces register; `Theme.Font.bodyItalic` renders in `Lora-Italic`, is not the system face, and is not the roman. | `FontLoadTests` (the driving list gained a fifth row) + `RenderedFaceTests.testItalicIsTheItalicFaceAndNotTheRoman` |
| A14 | `docs/screens/p2-*.png` — 11 per device × {iPhone 17 Pro, iPhone SE (3rd gen)} = 22: 1g, 1g needs-Gmail, 1e inbox, 1e user label, 1e empty, 1e offline, 1f, 1f partial, 1k, and AX5 for 1e and 1f. All distinct by hash, and every state in the set is also asserted by a UI test. | `scripts/screenshots.sh` (which pins the clock, the time zone and the status bar) + its hash check + `MailUITests`/`OfflineUITests`/`ThreadNavigationUITests`. Distinct hashes alone prove only that images differ — a broken seed showing eleven different error screens passes that check |
| A15 | Opening a thread performs **no** write to `thread.unread`. | `MailStoreTests.testDetailWriteNeverTouchesUnread` |
| A16 | Relaunched against an **unreachable base URL**, the app shows its cached rows and says the connection failed. It never presents an empty list as "no mail" and never clears the store. | `OfflineUITests` — an XCUITest, as PLAN.md §iOS app requires. An `APIClient` test can only prove the network failed, never what the model did about it |
| A17 | The first-run state machine reaches "mail ready" **without a relaunch**. | `AppStateTests` — `mailboxes: []` then a populated response drives `syncPending → mailReady` |
| A18 | Changing the server URL **clears the stored token**. A bearer minted by server A is never sent to server B. | `CredentialTests` |
| A19 | `TARGETED_DEVICE_FAMILY = 1` and portrait-only, in **every** configuration. | `InfoPlistTests.testEveryBuildConfigurationTargetsIPhoneOnly` + `testNoConfigurationAdvertisesLandscape`, derived from the configuration-list count the same way A4 of P1 is |

## B. Edge-case checklist (doctrine minimum + this lane's own)

| # | Edge case | Handling | Where |
|---|---|---|---|
| P1 | **Empty input** — `mailboxes: []`, `threads: []`, `to: []`, `attachments: []`, `agent_cards: []`, `subject: ""`, `from_name: ""` | Rows keep their box and still show a time. **`from_name == ""` falls back to `from_email`** — `API.md` §2 leaves that call to the UI, so the UI makes it here rather than shipping a blank sender | `threads_last_page.json`; `MailRow`; `WireDecodeTests`, `MailGeometryTests` |
| P2 | **Unicode** — `Föhn … 📦`, combining marks, RTL, a 200-char unbroken token, an embedded **NUL** | Round-tripped byte-exact through SQLite. The backend lane lost time to a NUL twice; this is the same hazard one layer up | `MailStoreTests.testHostileTextRoundTripsByteExact` |
| P3 | **Crash mid-step** | One transaction per page and per detail; a throw leaves zero rows rather than half a page | `MailStore`; `MailStoreTests` |
| P4 | **Duplicate delivery / replay** | Upsert on the wire's own primary key, and **column-scoped** so a list write cannot erase detail | `MailStore`; `MailStoreTests` (A8) |
| P5 | **Pagination boundary** | `reached_end` is its own column — `next_cursor IS NULL` conflates "never fetched" with "last page". A **generation counter** discards a page that lands after the mailbox changed. One page request in flight at a time | `mailbox_sync`; `MailStoreTests`, `MailListModel` |
| P6 | **Clock skew** — `ts` after `now` | DESIGN.md's `listTime` table has no future branch, so one is defined: clamp to today's `H:mm`. Never "in 3 hours", never a crash | `ListTime`; `ListTimeTests` |
| P7 | **Expiry** | The pairing code is single-use with a 10-minute TTL; wrong, spent and expired are all `401` and the UI says so **without** distinguishing them, because the server deliberately does not | `PairingView`; `APIClientTests` |
| P8 | **429 / timeout / 502 / offline** | `Retry-After` honoured, 30 s timeout, failures rendered **over cached rows** | `APIClient`; `APIClientTests`, `OfflineUITests` (A16) |
| P9 | **`409 needs_reauth`** | Writes `account.status`, which is what makes 1g's sub-label and 1k's "Sign in again" row appear. Surfacing it only as an API error would leave the recovery action invisible | `MailSync`; `AppStateTests` |
| P10 | **Store unopenable or corrupt** | Delete `.sqlite`/`-wal`/`-shm`, retry once, then an error caption. It is a cache; erasing it is always safe | `StoreLocation`; `MailStoreTests` |
| P11 | **Stale local DB across launches** | Any `-NADESeed` launch resets its store first, so UI tests and screenshots are hermetic | `FixtureSeed` |
| P12 | **`partial: true`** | Its own caption under the subject, in its own slot | `ThreadView`; `thread_partial.json`, `WireDecodeTests` |
| P13 | **Coexisting truths** | `partial` and a network failure can both be true — `API.md` says `partial` is *produced by* an upstream failure. Two independent slots, never one overwritten string | `ThreadModel` |
| P14 | **AX5 Dynamic Type** | Rows and bodies are content and scale to AX5; the chip row is chrome and clamps at `Theme.Metrics.chromeTypeCeiling` (already inside `NChip`) | screenshots + `MailGeometryTests` |
| P15 | **375 pt width** | The chip row scrolls; the mail row's 14/22 asymmetric inset holds; nothing clips | acceptance cmd 2 + the SE screenshot set |
| P16 | **Unread stays unread after opening** | `API.md` bans a local read-marker: it would disagree with Gmail within minutes and give the user two contradictory inboxes. There is **no column** to write | A15 |
| P17 | **Keychain write fails after the code is consumed** | The code is spent and the token exists exactly once. Surfaced as token loss requiring a fresh code — never swallowed | `CredentialTests` |
| P18 | **Unknown enum value from a future server** | The wire model falls back to `.unknown(String)` **and the column stores the raw string**. A `CHECK` pinned to today's values would turn a forward-compatible decode into a failed transaction — the exact screen-blanking the fallback exists to prevent | `WireMail`, `Schema`; `MailStoreTests` |
| P19 | **`msg_count` ≠ `messages.count`** | Both stored verbatim, neither derived from the other. `thread_partial.json` is the fixture that makes the difference real | `MailStore`; `WireDecodeTests` |

## C. Out of scope for P2 (explicitly not built)

- **1f's bottom ask bar is chrome, not a control.** `POST /ask` lands at P6.
  The mockup's own field is a `<span>` and its circle is a `<span>`, so P2
  renders a non-focusable, non-editable, `accessibilityHidden` band — a picture
  of a bar, which is what the mockup is. The agent card's three buttons *are*
  real `<button>`s in the mockup, which is why those are cut instead.
- **The agent card has no buttons.** They need `GET /feed/{id}` for `actions`
  and `action_label` (P5). Rendering a literal "Approve" would break DESIGN §4
  and PLAN C1/C2.
- **"View original"** (`body_html != null` → locked WKWebView) — P3, with the
  attachments proxy whose `cid:` URLs give it meaning. `body_html` is decoded
  and stored now so P3 is a UI-only change.
- **Attachments are not tappable.** No proxy until P3, and the mockup draws no
  `onClick`. **Inline (`cid:`) parts are still listed**, which a mail client
  would normally suppress: P2 does not render `body_html`, so hiding them would
  make them invisible everywhere rather than merely redundant. Revisit with P3's
  "View original", not before.
- **1k's AGENTS, READING and `disclosure` footer** — no `GET/PATCH /settings`
  and no `GET /runs` route exists until P7.
- **Mail search.** `GET /search` is modelled and decoded; DESIGN.md draws no
  search field on 1e and the mockup has none.
- **Push, SSE, feed, notes, calendar, agents** — P3/P5/P6/P7.

## D. What is *not* claimed

1. **The Release fixture exclusion is a build script, not a test.** Tests run
   only in Debug, so nothing inside the simulator can see a Release bundle.
2. **The interactive pop gesture under a hidden navigation bar does not survive
   on its own.** Measured, not assumed: it is put back by
   `nadeInteractivePopGesture()`, and `testTheSwipeBackGestureStillPops` fails
   without it. What is *not* claimed is that a future iOS will keep needing
   that; the test is what would say so either way.
3. **`EXCLUDED_SOURCE_FILE_NAMES` against a `PBXFileSystemSynchronizedRootGroup`**
   was unverified when this lane was planned. It works: a Release build carries
   `calendar.json` and nothing else. A6 is what keeps that true, and it
   cross-checks that Debug carries ten — otherwise "excluded from Release" and
   "never built at all" look identical.
4. **1f's per-message meta row is a design addition.** The mockup draws one
   message. The repeat reuses the mockup's own components at its own numbers,
   but no render exists to check it against.
5. **`PairingView` has no mockup at all.** DESIGN.md §1k gives the row that
   pushes it and nothing beyond. It is assembled from DS parts and is the one
   screen in this lane a reviewer cannot read off a render.
6. **The agent card cannot be seen against the live backend.** P2 serves
   `agent_cards: []`; its screenshot comes from the fixture world. The same is
   true of `agent_note` on the mail row.
7. **No snapshot-diffing harness.** P1's §D4 stands: screenshots are read by
   eye against the mockup, and only individual measurements are automated.
8. **`msg_count` is the server's number, not a count of what we hold.** The
   client never reconciles them; `partial` is how the difference is explained.
9. **A visible timestamp does not re-render when the day turns.** `clock.now()`
   is captured when SwiftUI builds a screen, so a row left on screen across
   midnight keeps saying "23:59" until something else invalidates it. A timer
   that fires at the day boundary is the fix and is not built here — P3 brings
   push, which invalidates these screens far more often than a clock would.
10. **`Retry-After` is honoured by refusing to ask again before it elapses**,
    not by scheduling a retry for that moment. Nothing re-runs on its own when
    the window passes; the next foreground or screen appearance does.
11. **Two servers' mail cannot mix, but only because changing the origin wipes
    the store.** The database is one file per mode, not one per origin. If a
    later phase adds a way to change the server that does not go through
    `MailSync.pair(origin:…)`, that guarantee is gone.

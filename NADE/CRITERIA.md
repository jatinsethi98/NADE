# P1 [ios] — acceptance criteria + edge-case checklist

Written **before** any code, per PLAN.md §Execution doctrine step 1. Every edge
case below is either a test (named here) or an `// EDGE:` comment beside the
code that handles it.

Revised 2026-08-17 after an adversarial review. Several claims in the first
version were not true — a lint that "nothing routes around", font tests that
proved rendering, geometry tests that proved geometry, a shell later phases
could build on. Where a claim has been narrowed, the narrowing is stated rather
than quietly dropped, and §D records what is still **not** claimed.

## A. Acceptance criteria

| # | Criterion | How it is checked |
|---|---|---|
| A1 | `xcodebuild -list -project NADE.xcodeproj` shows targets `NADE`, `NADETests`, `NADEUITests` and a **shared** scheme `NADE`. | acceptance cmd 1 |
| A2 | `xcodebuild -resolvePackageDependencies` resolves GRDB 7 and writes `Package.resolved`. | acceptance cmd 2 + file exists |
| A3 | `xcodebuild … clean build test` on iPhone 17 Pro ends `** TEST SUCCEEDED **`, twice consecutively, with no warning this lane introduced. | acceptance cmd 3 |
| A4 | `IPHONEOS_DEPLOYMENT_TARGET = 18.0` in **every** build configuration (project + all three targets). | `InfoPlistTests.testEveryBuildConfigurationTargets18` parses `project.pbxproj`; `testTheBuiltConfigurationTargets18` covers the built bundle. The bundle check alone only ever sees Debug. |
| A5 | `NADE/Info.plist` exists, is the target's `INFOPLIST_FILE`, `GENERATE_INFOPLIST_FILE` stays `YES`, and the built `Info.plist` contains `UIAppFonts` (4 files) and `NSAppTransportSecurity.NSAllowsLocalNetworking = true`. | `InfoPlistTests` reads `Bundle.main` |
| A6 | All four bundled faces resolve by PostScript name, are **not** the system face, and are what `Theme.Font.heading` / `.body` actually render in. | `FontLoadTests` (registration, `UIAppFonts`, the TTFs' own PostScript names, the `tnum` **monospaced-numbers selector**) + `RenderedFaceTests` (renders `Text` through the Theme API and compares the resolved size with Core Text's in the expected face, and separately with the system face's). Registration alone is not enough: with `Theme.Font.heading` swapped for `Font.system`, all 8 registration tests still pass and all 4 rendering tests fail. |
| A7 | Theme tokens resolve to the exact RGBA from DESIGN.md; space/radius scales are exact. | `ThemeTests` — **every** step of the neutral, accent and accent-2 ramps, not only the endpoints |
| A8 | No raw **colour** and no raw **font** anywhere in `NADE/**` except `NADE/Theme.swift`. | `NoRawValuesTests`. Scope is deliberate and narrower than the original claim — see §D1. Non-vacuous: `testTheLintCatchesEveryEvadedForm` plants 17 violating snippets, including the five forms that evaded the first version. |
| A9 | App launches to a 4-tab bar; tapping each tab changes the selection; the bar reads as a tab bar. | `NADEUITests/TabBarUITests` — existence, launch selection, selection changes, per-tab screens, the container role and label, "Tab N of 4", and each column's 44 pt frame |
| A10 | `docs/screens/p1-*.png` exist, cover **every** gallery section including AX5 and Reduce Motion, and match the Classical DS. | `simctl io screenshot` + my own visual review. The first delivery's AX5 screenshot was cut off mid-card and its "1a draft card" was not 1a's card; both are re-shot. |
| A11 | Fixtures in `docs/contract/` are readable from `NADETests`, and the SSE streams are well-formed. | `ContractFixturesTests` — the set is **enumerated from the directory**, cross-checked both ways against the bundle, and each stream is parsed against `docs/contract/README.md`'s rules |
| A12 | App is locked to light appearance. | `.preferredColorScheme(.light)` at the root of `NADEApp`. Not directly assertable from a test (the modifier is not introspectable), but every token is an absolute sRGB value rather than a system semantic colour, so nothing can flip even without it — `ThemeTests` pins the exact RGBA of each token, and the screenshots confirm the rendered result. |
| A13 | Switching tabs does not rebuild a screen. | `NADEUITests/ShellStateUITests` — a counter on each screen survives a full rotation through all four tabs |
| A14 | Every control's drawn box is the design's size **and** its hit region is at least 44 × 44. | `ComponentGeometryTests` measures the drawn box; `HitTargetUITests` reads the hit frame in the running app and taps outside the drawn edge. Both halves are needed: either one alone passes for the wrong reason. |

## B. Edge-case checklist (doctrine minimum + this lane's own)

| # | Edge case | Handling | Where |
|---|---|---|---|
| E1 | **Dynamic Type at the largest accessibility size** (`AX5`) | Every text style is `Font.custom(_:size:relativeTo:)`, so it scales; `Font.custom(_:fixedSize:)` is banned and linted. Chrome that cannot grow without breaking (tab bar, toggle, stepper, segmented, tag, chip, **and an icon button's glyph — its box is fixed**) is clamped with `.dynamicTypeSize(…DynamicTypeSize.accessibility1)`; the tab-bar label additionally gets `minimumScaleFactor(0.7)` + `lineLimit(1)`. Content (cards, body copy, radio rows, text buttons) is never clamped. `NRadioDot` scales via `@ScaledMetric` with its ring widths proportional to the dot. | `Theme.swift` `Metrics.chromeTypeCeiling`, `NButton.clampsGlyph(for:)`, `NTabBar`, `NToggle`, `NStepper`, `NSegmented`, `NTag`, `NChip`, `NRadioDot`; `RenderedFaceTests.testThemeFontsGrowWithDynamicType`; `ComponentGeometryTests.testIconButtonBoxDoesNotGrowAtAX5`; the gallery's own **`ax5` section**, screenshotted |
| E2 | **Smallest supported width** (iPhone SE-class, 320–375 pt) | No fixed widths on containers; the tab bar is four `maxWidth: .infinity` columns with a shrinking label; the chip row scrolls horizontally; every text component wraps or truncates rather than forcing a width; `NSegmented` compresses instead of overflowing. | all components; `ComponentGeometryTests.testSegmentedCompressesRatherThanOverflowingASmallScreen`, `testTabBarFitsTheNarrowestSupportedScreen`; verified by running the gallery and the shell on `iPhone SE (3rd generation)` |
| E3 | **Very long unbroken string** (a 200-char token, a long URL) | Every label that can receive user data gets `.lineLimit` + `.truncationMode(.tail)`, or `.fixedSize(horizontal: false, vertical: true)` where wrapping is wanted. No component sizes itself from its text width in a way that can push the row off-screen. | `NCard`, `NTag`, `NChip`, `NButton`, `NRadioRow`, `NSegmented`; gallery has an explicit "pathological strings" block |
| E4 | **Empty string** | Components must not collapse to zero height or draw a 0-width pill. Minimum heights are on the container, not the text. | `NTag`, `NChip`, `NButton`, `NTextField` placeholder; `ComponentGeometryTests.testEmptyButtonKeepsItsBox`; gallery renders each with `""` |
| E5 | **RTL layout** | No `.left`/`.right` anywhere; no hard-coded `x` offsets that assume LTR — `NToggle`'s knob is a `ZStack(alignment: .leading/.trailing)` + `.padding(3)`, not a fixed `offset(x:)`, so it mirrors for free. Every stack uses leading/trailing alignment. | `NToggle`, `NStepper`, `NSegmented`, `NTabBar`; gallery has an `.environment(\.layoutDirection, .rightToLeft)` block, screenshotted |
| E6 | **VoiceOver labels on every interactive element** | Every control declares `.accessibilityLabel`, the right `.accessibilityAddTraits`, and `.accessibilityValue` where it has state. **No control can end up with an empty label**: `NButton`/`NChip` fall back to the trait's noun. **No visible value is silent**: `NRadioRow`'s trailing value is the element's `accessibilityValue`. Decorative strokes, dots and glyphs are `.accessibilityHidden(true)`. The tab bar is a **container** with the `isTabBar` trait, a label, and per-tab "Tab N of 4". | every component; `ThemeTests` pins the composition, `NADEUITests/AccessibilityUITests` and `TabBarUITests` read it back out of the running app |
| E7 | **Reduced motion** | The only animation in P1 is `NToggle`'s 0.18 s knob ease. It is wrapped in `Theme.Motion.toggle(reduceMotion:)` which returns `nil` when `\.accessibilityReduceMotion` is on. That value is read-only, so it cannot be forced in the gallery — both branches are unit-tested instead, and the gallery's `motion` section documents it. | `NToggle`, `Theme.Motion`; `ThemeTests.testToggleAnimationRespectsReduceMotion` |
| E8 | **Missing / mis-registered font file** — must fail **loudly** | `FontLoadTests` asserts (a) `UIFont(name:size:)` is non-nil for all four PostScript names, (b) the resolved `familyName` is the expected face, (c) it is **not** the system family, (d) `UIFont.familyNames` contains both families, (e) the four filenames are in `Bundle.main` **and** in the built `UIAppFonts`, (f) each TTF declares the PostScript name we ask for. Silent system fallback cannot pass. | `NADETests/FontLoadTests.swift` |
| E9 | **Hairlines are 1 pt** | The mockup frame is 402 CSS px and the device is 402 pt, so `1px` is 1 pt; DESIGN.md says the same. `Hairline`/`VHairline` and `Theme.Stroke.border` are all `1`. **This reverses P1's original ⅓-pt decision** — see IOS_DECISIONS.md D9 for why that reasoning was wrong. | `Theme.swift`; `ComponentGeometryTests.testHairlineIsOnePoint` measures a hosted `Hairline` |
| E10 | **Tabular numerals** | `.monospacedDigit()` helper (`.tabularNumerals()`) applied at every count/time site. Both families are asserted to expose the **monospaced-numbers selector**, not merely a number-spacing feature type — a face offering only the proportional selector would satisfy the weaker check while `.tabularNumerals()` did nothing. | `Theme.swift`, `FontLoadTests.testBothFamiliesOfferTheMonospacedNumbersSelector`, gallery |
| E11 | **Disabled + pressed button states** | `NButton` is a `ButtonStyle`, so pressed state comes from `configuration.isPressed`; disabled comes from `\.isEnabled` (opacity 0.45, DS `.btn:disabled`). Both are rendered in the gallery. | `NButtonStyle` |
| E12 | **Dark mode** | Out of scope for v1 (DESIGN.md §Color). Root forces `.preferredColorScheme(.light)`; every colour is an absolute sRGB value, never a system semantic colour, so nothing can flip. | `NADEApp`, `Theme.Color` |
| E13 | **Font file present but not listed in `UIAppFonts`** | Covered by E8(e): the test reads the *built* `UIAppFonts` array and cross-checks it against the bundled files. | `FontLoadTests.testUIAppFontsMatchesBundledFiles` |
| E14 | **Gallery leaking into Release** | The gallery route is `#if DEBUG` only; a release build has no path to it. That is not a licence for the gallery to say untrue things — it is still the Phase 1 artefact (E18). | `NADEApp` |
| E15 | **`docs/contract` fixture missing, renamed, stale or malformed** | The fixture set is **enumerated from `docs/contract/`**, not listed by name, and cross-checked both ways against the test bundle. SSE streams are parsed and checked against README.md's stated wire format: `route` first and once, every payload valid JSON, exactly one terminal `done`/`error` and it is last, trailing blank line. | `ContractFixturesTests`; `testTheStreamValidatorRejectsEveryMalformedShape` feeds it 11 malformed streams that all passed the previous substring check |
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
   snapshot suite cheap to add if drift becomes a problem.
5. **`.preferredColorScheme(.light)` is not directly asserted** (A12).
6. **Justified body text** (1f) has no SwiftUI equivalent; recorded as a
   deviation, resolved when P2 builds `ThreadView`.

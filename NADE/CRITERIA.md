# P1 [ios] — acceptance criteria + edge-case checklist

Written **before** any code, per PLAN.md §Execution doctrine step 1. Every edge
case below is either a test (named here) or an `// EDGE:` comment beside the
code that handles it.

## A. Acceptance criteria

| # | Criterion | How it is checked |
|---|---|---|
| A1 | `xcodebuild -list -project NADE.xcodeproj` shows targets `NADE`, `NADETests`, `NADEUITests` and a **shared** scheme `NADE`. | acceptance cmd 1 |
| A2 | `xcodebuild -resolvePackageDependencies` resolves GRDB 7 and writes `Package.resolved`. | acceptance cmd 2 + file exists |
| A3 | `xcodebuild … clean build test` on iPhone 17 Pro ends `** TEST SUCCEEDED **`, twice consecutively, with no warning this lane introduced. | acceptance cmd 3 |
| A4 | `IPHONEOS_DEPLOYMENT_TARGET = 18.0` in **every** build configuration (project + all three targets). | `xcodebuild -showBuildSettings` grep |
| A5 | `NADE/Info.plist` exists, is the target's `INFOPLIST_FILE`, `GENERATE_INFOPLIST_FILE` stays `YES`, and the built `Info.plist` contains `UIAppFonts` (4 files) and `NSAppTransportSecurity.NSAllowsLocalNetworking = true`. | `InfoPlistTests` reads `Bundle.main` |
| A6 | All four bundled faces resolve by PostScript name and are **not** the system face. | `FontLoadTests` |
| A7 | Theme tokens resolve to the exact RGBA from DESIGN.md; space/radius scales are exact. | `ThemeTests` |
| A8 | No `Color(hex:`, no bare `.font(.system(`, no `#rrggbb` literal, no `Color(red:`/`UIColor(red:` anywhere in `NADE/**` except `Theme.swift`. | `NoRawValuesTests` |
| A9 | App launches to a 4-tab bar; tapping each tab changes the selection. | `NADEUITests/TabBarUITests`: `testTabBarHasAllFourTabs`, `testAskIsSelectedOnLaunch`, `testTappingEachTabChangesTheSelection`, `testEachTabShowsItsOwnScreen` |
| A10 | `docs/screens/p1-gallery*.png` exist and match the Classical DS. | `simctl io screenshot` + my own visual review |
| A11 | Fixtures in `docs/contract/` are readable from `NADETests`. | `ContractFixturesTests` |
| A12 | App is locked to light appearance. | `.preferredColorScheme(.light)` at the root of `NADEApp`. Not directly assertable from a test (the modifier is not introspectable), but every token is an absolute sRGB value rather than a system semantic colour, so nothing can flip even without it — `ThemeTests` pins the exact RGBA of each token, and the screenshots confirm the rendered result. |

## B. Edge-case checklist (doctrine minimum + this lane's own)

| # | Edge case | Handling | Where |
|---|---|---|---|
| E1 | **Dynamic Type at the largest accessibility size** (`AX5`) | Every text style is `Font.custom(_:size:relativeTo:)`, so it scales; `Font.custom(_:fixedSize:)` is banned and linted. Chrome that cannot grow without breaking (tab bar, toggle, stepper, segmented, tag, chip) is clamped with `.dynamicTypeSize(…DynamicTypeSize.accessibility1)`; the tab-bar label additionally gets `minimumScaleFactor(0.7)` + `lineLimit(1)`. Content (cards, body copy, radio rows, buttons) is never clamped. `NRadioDot` scales via `@ScaledMetric` with its ring widths proportional to the dot. | `Theme.swift` `Metrics.chromeTypeCeiling`, `NTabBar`, `NToggle`, `NStepper`, `NSegmented`, `NTag`, `NChip`, `NRadioDot`; `NoRawValuesTests.testEveryCustomFontIsDynamic`; `ThemeTests.testTextStyleMappingCoversTheDesignScaleMonotonically`; gallery renders an AX5 block |
| E2 | **Smallest supported width** (iPhone SE-class, 320–375 pt) | No fixed widths on containers; the tab bar is four `maxWidth: .infinity` columns with a shrinking label; the chip row scrolls horizontally; every text component wraps or truncates rather than forcing a width. | all components; verified by running the gallery and the shell on `iPhone SE (3rd generation)` |
| E3 | **Very long unbroken string** (a 200-char token, a long URL) | Every label that can receive user data gets `.lineLimit` + `.truncationMode(.tail)`, or `.fixedSize(horizontal: false, vertical: true)` where wrapping is wanted. No component sizes itself from its text width in a way that can push the row off-screen. | `NCard`, `NTag`, `NChip`, `NButton`, `NRadioRow`; gallery has an explicit "pathological strings" block |
| E4 | **Empty string** | Components must not collapse to zero height or draw a 0-width pill. Minimum heights are on the container, not the text. | `NTag`, `NChip`, `NButton`, `NTextField` placeholder; gallery renders each with `""` |
| E5 | **RTL layout** | No `.left`/`.right` anywhere; no hard-coded `x` offsets that assume LTR — `NToggle`'s knob is a `ZStack(alignment: .leading/.trailing)` + `.padding(3)`, not a fixed `offset(x:)`, so it mirrors for free. Every stack uses leading/trailing alignment. | `NToggle`, `NStepper`, `NSegmented`, `NTabBar`; gallery has an `.environment(\.layoutDirection, .rightToLeft)` block, verified in `docs/screens/p1-gallery-10-edges.png` |
| E6 | **VoiceOver labels on every interactive element** | Every control declares `.accessibilityLabel`, the right `.accessibilityAddTraits`, and `.accessibilityValue` where it has state. Decorative strokes/dots are `.accessibilityHidden(true)`. Composite rows use `.accessibilityElement(children: .combine)`. | every component; `NADEUITests` queries tabs **by accessibility identifier**, which only exists because the traits are set |
| E7 | **Reduced motion** | The only animation in P1 is `NToggle`'s 0.18 s knob ease. It is wrapped in `Theme.Motion.toggle(reduceMotion:)` which returns `nil` when `\.accessibilityReduceMotion` is on. That value is read-only, so it cannot be forced in the gallery — both branches are unit-tested instead. | `NToggle`, `Theme.Motion`; `ThemeTests.testToggleAnimationRespectsReduceMotion` |
| E8 | **Missing / mis-registered font file** — must fail **loudly** | `FontLoadTests` asserts (a) `UIFont(name:size:)` is non-nil for all four PostScript names, (b) the resolved `familyName` is the expected face, (c) it is **not** the system family, (d) `UIFont.familyNames` contains both families (proves `UIAppFonts` registration), (e) the four filenames are in `Bundle.main` **and** in the built `UIAppFonts` array. Silent system fallback therefore cannot pass. | `NADETests/FontLoadTests.swift` |
| E9 | **1 px hairline, not 1 pt** | `Hairline`/`VHairline` divide by `\.displayScale` from the environment (not the deprecated `UIScreen.main.scale`), so they are one device pixel at any scale. Component *outlines* stay at 1 pt (`Theme.Stroke.border`) because in this design colour is a stroke. Reasoning in `IOS_DECISIONS.md` D9. | `Theme.swift` `Hairline`, `VHairline`, `Theme.Stroke` |
| E10 | **Tabular numerals** | `.monospacedDigit()` helper (`.tabularNumerals()`) applied at every count/time site; the gallery shows a proportional/tabular comparison so a regression is visible. | `Theme.swift`, gallery |
| E11 | **Disabled + pressed button states** | `NButton` is a `ButtonStyle`, so pressed state comes from `configuration.isPressed`; disabled comes from `\.isEnabled` (opacity 0.45, DS `.btn:disabled`). Both are rendered in the gallery. | `NButtonStyle` |
| E12 | **Dark mode** | Out of scope for v1 (DESIGN.md §Color). Root forces `.preferredColorScheme(.light)`; every colour is an absolute sRGB value, never a system semantic colour, so nothing can flip. | `NADEApp`, `Theme.Color` |
| E13 | **Font file present but not listed in `UIAppFonts`** | Covered by E8(e): the test reads the *built* `UIAppFonts` array and cross-checks it against the bundled files. | `FontLoadTests.testUIAppFontsMatchesBundledFiles` |
| E14 | **Gallery leaking into Release** | The gallery route is `#if DEBUG` only; a release build has no path to it. | `NADEApp` |
| E15 | **`docs/contract` fixture missing or renamed** | `ContractFixturesTests` fails loudly if a named fixture is absent or is not valid JSON. | `ContractFixturesTests` |
| E16 | **Source-tree lint runs where there is no source tree** (CI artefact-only run) | `NoRawValuesTests` derives the repo root from `#filePath`; if the tree is absent it `XCTSkip`s with a clear message rather than silently passing. | `NoRawValuesTests` |

## C. Out of scope for P1 (explicitly not built)

Mail, feed, agents, notes, calendar and settings screens (P2/P3/P7); networking;
GRDB schema/records (P2); SSE; the ask field's behaviour. `RootTabView` hosts
four **placeholder** screens whose only job is to be replaced without the shell
changing.

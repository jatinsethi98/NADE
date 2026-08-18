# iOS decisions and recorded deviations

Every place the SwiftUI build departs from `docs/DESIGN.md` / the Classical DS,
and why. Phase 1 ([ios] foundations). Additions go at the bottom of each
section, newest last.

---

## P1 — project configuration

### D1. Deployment target 26.5 → 18.0
The template shipped `IPHONEOS_DEPLOYMENT_TARGET = 26.5`, which would refuse to
install on any phone not on the newest iOS. Set to **18.0** in every build
configuration (project Debug/Release + three targets — eight settings in all).
18.0 still gives `@Observable`, `safeAreaInset`,
`AccessibilityTraits.isToggle`, `ScrollViewReader` and everything else this app
uses.

Asserted twice, because once was not enough:
`InfoPlistTests.testTheBuiltConfigurationTargets18` reads `MinimumOSVersion`
from the built bundle — which only ever sees Debug — and
`testEveryBuildConfigurationTargets18` parses `project.pbxproj` and requires all
eight settings to be 18.0. See D36; the earlier claim that the bundle check
alone caught a drifting configuration was false.

### D2. Info.plist is hand-written *and* generated
`INFOPLIST_FILE = NADE/Info.plist` with `GENERATE_INFOPLIST_FILE = YES`. Xcode
merges its generated keys (`CFBundle*`, scene manifest, launch screen,
orientations) into the hand-written file, so the file only holds what Xcode
cannot generate: `UIAppFonts` and `NSAppTransportSecurity`.
`InfoPlistTests.testGeneratedKeysAreStillMergedIn` proves the merge happened.

Because `NADE/` is a `PBXFileSystemSynchronizedRootGroup`, `Info.plist` was
being copied into the bundle *as a resource* as well as processed as the
Info.plist ("Multiple commands produce …/NADE.app/Info.plist"). Fixed with a
`PBXFileSystemSynchronizedBuildFileExceptionSet` that excludes `Info.plist` and
`CRITERIA.md` from the target. The font OFL licences **stay** in the bundle —
the SIL OFL requires the licence to travel with the fonts.

### D3. ATS: `NSAllowsLocalNetworking`, not `NSAllowsArbitraryLoads`
The dev backend is plain HTTP on the LAN. `NSAllowsLocalNetworking` permits
local/link-local hosts only; it does not weaken ATS for the public internet.
`InfoPlistTests` asserts `NSAllowsArbitraryLoads` is *absent*.

### D4. Contract fixtures reach `NADETests` via a synchronized group
`docs/contract` is a second `PBXFileSystemSynchronizedRootGroup` whose only
target membership is `NADETests`. Every fixture — including the `.sse` streams,
which no copy-files phase would classify correctly — lands flat in
`NADETests.xctest` and stays current as the backend lane adds files. The
alternative (an explicit copy-files phase) would need a `PBXFileReference` per
fixture and would rot the first time the contract grows. `NADE` (the app) does
**not** get the fixtures — they are test data, not shipping data.

### D5. GRDB 7 is linked but unused in P1
`XCRemoteSwiftPackageReference` on `https://github.com/groue/GRDB.swift`,
`upToNextMajorVersion` from `7.0.0`; resolved to **7.11.1**. Wired into
`packageReferences`, the target's `packageProductDependencies` and the
Frameworks phase. P2 adds the schema and records. Pinning now means P2 never
discovers a resolution problem mid-feature.

### D6. Shared scheme is hand-written
`NADE.xcodeproj/xcshareddata/xcschemes/NADE.xcscheme` builds the app and runs
both test targets, so `xcodebuild test -scheme NADE` works from a clean
checkout with no Xcode GUI step (PLAN.md §iOS app: "No Xcode GUI steps").

### D7. Light appearance is forced app-wide
`.preferredColorScheme(.light)` at the root (`NADEApp`). DESIGN.md §Color:
the design ships one visual world and dark mode is out of scope for v1. Every
token is an absolute sRGB value rather than a system semantic colour, so
nothing can flip even if the modifier were removed. Revisit post-v1.

### D8. `AccentColor` asset set to `#b68235`
`ASSETCATALOG_COMPILER_GLOBAL_ACCENT_COLOR_NAME = AccentColor` feeds the system
tint (selection handles, the text caret in system controls). Left at the Xcode
default it would have been blue. Set to the design accent so nothing the app
does not draw itself can arrive in the wrong colour.

---

## P1 — design system

### D9. `Hairline` is one **point** — the earlier ⅓-pt decision was wrong
**Corrected 2026-08-17.** P1 shipped `Hairline` as `1 / displayScale` (⅓ pt at
@3x) and recorded it here as a deviation. Recording a deviation does not make it
faithful, and this one had no basis:

- The mockup frame is 402 CSS px and the device is 402 pt. The mapping is
  **1 : 1**, so every `border: 1px` in the mockup — dividers included — is
  **1 pt**. There is no conversion step in which a rule could become thinner.
- `docs/DESIGN.md` §Space & shape says the same thing in words: "Hairline =
  1 pt `divider`, full-bleed unless stated." §1 Color: "`divider` … every
  hairline (1 pt)."
- The original argument was that 1 pt "reads as a heavy line beside the design's
  fine rules". That is an argument about a *different* design. The design's
  rules are 1 CSS px on a 402 px frame; at 1 : 1 they are 1 pt. Preferring the
  iOS separator idiom over the design's own measurement is a taste override, not
  a port, and it made every divider in the app a third of its designed weight.

`Hairline`, `VHairline` and `Theme.Stroke.border` are therefore all 1 pt.
Both types still exist because they mean different things — a full-bleed rule
versus a component outline — and a later phase may want to move one without the
other. `\.displayScale` is no longer read at all.

Caught by `ComponentGeometryTests.testHairlineIsOnePoint`, which measures a
hosted `Hairline` rather than reading the constant back.

### D10. Dynamic Type: everything scales, chrome has a ceiling
Every text style goes through `Font.custom(_:size:relativeTo:)` with a text
style chosen by `Theme.Font.textStyle(for:)` (asserted monotonic in
`ThemeTests`). `Font.custom(_:fixedSize:)` is banned and the ban is linted.

Chrome that cannot grow without destroying the layout is clamped at
`DynamicTypeSize.accessibility1` (`Theme.Metrics.chromeTypeCeiling`): the tab
bar, `NToggle`, `NStepper`, `NSegmented`, `NTag`, `NChip`. Content — cards,
body copy, radio rows, buttons — is never clamped and runs to AX5. The tab bar
additionally shrinks its label (`minimumScaleFactor(0.7)`, one line) rather
than wrapping, because "CALENDAR" with 0.09 em tracking is already tight on a
320 pt device.

### D11. SF Symbols scale through `NIcon`
`Font.system(size:)` is a frozen size. `NIcon` wraps it in `@ScaledMetric` so
glyphs grow with Dynamic Type. `Theme.Font.icon` is the single legitimate
`.system(` call in the app; every other file is linted against it.

### D12. SF Symbols stand in for Lucide
Already recorded in PLAN.md §Design parity map. `sparkles` · `mail` →
`envelope` · `file-text` → `doc.text` · `calendar`, at
`.system(size: 18, weight: .light)` to approximate the design's 1.8 stroke.
`ThemeTests.testTabsAreTheFourInTheDesign` asserts each symbol actually exists
on the running OS, so a rename in a future SF Symbols release fails a test
rather than drawing a blank.

### D13. Shadow blur conversion
CSS blur radius is roughly twice a SwiftUI shadow radius. `0 1px 2px` → `y 1,
radius 1`; `0 3px 10px` → `y 3, radius 5`; `0 12px 32px` → `y 12, radius 16`.
Offsets are 1:1. Asserted in `ThemeTests.testShadowsMatchTheCSS`.

### D14. Corner style is `.circular`, not `.continuous`
CSS `border-radius` is a circular arc. Every rounded rect in the components
passes `style: .circular` rather than letting SwiftUI default to the iOS
squircle, which at radius 4 would visibly soften the design's crisp corners.

### D15. Uppercase is a rendering transform, not a string transform
`SectionEyebrow`, `NTabBar` and the eyebrows use `.textCase(.uppercase)` rather
than `String.uppercased()`, so VoiceOver still reads "Calendar" and "Accounts"
instead of spelling them out. Costs nothing, and the UI test asserts the tab
labels are the title-case forms.

### D16. Selected chip text is `accent-800`, not `accent`
DESIGN.md §1e prose says the selected mail chip is "accent ring + accent text +
accent-100 fill". The mockup's inline style is
`color: var(--color-accent-800)`, which is what actually renders and is far
more legible on the accent-100 fill (it also matches `.tag-accent`, the DS's
own filled-tag pairing). Followed the markup. Same pairing used for `NTag`'s
accent style, which the DS states explicitly.

### D17. `NButton` gained `corner`, `box`, `glyph` and `width` knobs
DESIGN.md §1 lists four button variants. Four extra parameters, every one of
them driven by a number the design states:

- `corner: .rounded | .pill` — the DS `.btn` is radius 4, but the ask field's
  button is a circle.
- `box:` on the icon initialiser — DS `.btn-icon` is 36 × 36, but the ask
  field's circle is 38 (2a/1f), 40 (1a) or 44 (2a focus / 2b).
- `glyph:` — **added 2026-08-17.** The glyph was hardcoded at 17. DESIGN.md §2's
  table gives four different sizes: `sparkles` **16** on 2a's feed bar and on
  1f, `arrow.up` **17** on 1a, `arrow.up` **18** on 2a's focus state and 2b.
  Measured through the `.ghost` variant in
  `ComponentGeometryTests.testTheIconGlyphSizeReachesTheGlyph`, because the
  `.icon` variant's fixed box hides the difference.
- `width: .intrinsic | .flexible` — **added 2026-08-17.** CSS `flex: 1`, which
  1c's "Run once now" and both of 1d's footer buttons carry. This has to live
  *inside* `NButtonStyle`: `.frame(maxWidth: .infinity)` on the `Button` only
  stretches the wrapper, because the style has already sized its border and its
  press wash to the label. The shipped SE screenshot showed the result — a
  centred, label-width "Run once now" where the design has a full-width one.

A glyph beside a label renders at the label's 14 pt (DS `.btn { gap: 6 }`); a
glyph on its own renders at its screen's size.

### D18. `NToggle`'s knob is laid out, not offset
The mockup's `left: 2px / 22px` are measured inside the 1 pt border, so the
knob sits 3 pt from the outer edge at both ends. Implemented as a
`ZStack(alignment: isOn ? .trailing : .leading)` with `.padding(3)` rather than
a hard `offset(x:)` — exact, and it mirrors correctly in RTL for free.
`ThemeTests.testToggleGeometryMatchesTheDesign` pins the arithmetic.

### D19. Reduce Motion
`\.accessibilityReduceMotion` is a **read-only** environment value, so the
gallery cannot force it for a side-by-side demo. `NToggle` reads it and asks
`Theme.Motion.toggle(reduceMotion:)`, which returns `nil` (no animation);
`ThemeTests.testToggleAnimationRespectsReduceMotion` covers both branches.

### D20. `NStepper` is one adjustable element to VoiceOver
The −/+ boxes are `.accessibilityHidden(true)` and the pair exposes
`.accessibilityAdjustableAction`, so VoiceOver users swipe up/down on one
element instead of hunting two unlabelled glyph buttons.

### D21. `NTextField` draws its own placeholder, and carries six presets
`prompt:` does not reliably honour a custom colour and face. The placeholder is
an overlay `Text` in `ink62` at the variant's own size, with
`.allowsHitTesting(false)` and `.accessibilityHidden(true)`, and the field
carries an explicit `.accessibilityLabel` so nothing is lost to VoiceOver.
`showsFocusRing` exists for the gallery/previews only — it draws the accent
focus border without stealing first responder.

**Amended 2026-08-17.** The component fixed 14 pt and `9 × 15` padding while
claiming to cover four ask-field variants; DESIGN.md §2 gives each screen its
own row and no two agree. `NTextField.Metrics` now carries all six, and a screen
picks one instead of forking the component:

| Preset | Screen | Font | Pad | Min-height | Shape |
|---|---|---|---|---|---|
| `.input` | DS `.input` | 14 | 6 / 10 | 36 | md |
| `.askPinned` | 2a feed | 13.5 | 8 / 15 | 38 | pill |
| `.askDocked` | 1a | 14 | 9 / 15 | 40 | pill |
| `.askCentred` | 2a focus / 2b | 14 | 10 / 16 | 44 | pill |
| `.askThread` | 1f | 13.5 | 10 / 15 | 38\* | pill |
| `.searchPill` | 1h | 13.5 | 8 / 14 | 38\* | pill |

\* 1f and 1h declare no `min-height` in the mockup; their CSS box is ~43 and ~39,
driven entirely by padding and the inherited `line-height: 1.55`. 38 is a floor
that never binds — it is there so an empty field cannot collapse (E4) and so
1f's field can never draw shorter than the 38 pt circle beside it.

### D22. `TabView` is not used
SwiftUI's `TabView` cannot be given the design's 1 pt top hairline, its 18 pt
light-stroke glyphs, its 10.5 pt uppercase 0.09 em labels or its accent /
`ink62` pairing. `NTabBar` is a plain `HStack` of buttons over an `NTab` enum,
hosted by `RootTabView` in a plain `VStack` (see D26 — an earlier version of
this entry said `.safeAreaInset(edge: .bottom)`, which is no longer how the
shell is built).

Not using `TabView` means the shell has to supply what `TabView` would have
given for free. Two things, both of which P1 originally missed and D29 / D30
now cover: keeping every tab's screen alive across a selection change, and
telling the accessibility system that four buttons in a row are a tab bar.

---

## P1 — testing

### D23. The no-raw-values lint: what it enforces, exactly
`NoRawValuesTests` walks `<repo>/NADE/**/*.swift`.

**It claims: no raw *colour* and no raw *font* outside `NADE/Theme.swift`.**
That is a hex literal, `Color(red:…)` / `UIColor(red:…)`, `Color(.sRGB…)`,
`.system(size:)`, `.custom(…)`, `Font.custom(_:fixedSize:)`, or a system palette
colour reached through any of `foregroundStyle` / `foregroundColor` / `tint` /
`accentColor` / `fill` / `stroke` / `strokeBorder` / `background` / `shadow`.

**It does not claim to police geometry.** `NChip.paddingH = 12` and
`NStepper.gap = 12` are raw numbers living beside the component they describe,
deliberately: they are per-component design facts, not shared tokens, and
hoisting them into `Theme` would turn `Theme` into a dictionary of one-use
constants. `ComponentGeometryTests` is what holds geometry honest, by measuring
it. The previous version of this entry claimed "the design system is only a
system if nothing routes around it" while `NCard` carried a raw `.opacity(0.8)`
and every component carried its own sizes — the claim is now the narrower true
one, and `.opacity(0.8)` became `Theme.Color.ink80`.

**Rewritten 2026-08-17 because the old matcher was evadable five ways.** Each of
these was checked against the rules exactly as they shipped, and every one of
them slipped through:

| Evasion | Old lint | New lint |
|---|---|---|
| `.font(.custom("Lora-Regular", …))` (leading dot) | missed — it matched only `Font.custom(` | caught |
| `Font.system(size:)` | missed — it matched only `.font(.system(` | caught |
| `Color(hex:` split across lines | missed — matching was per line | caught |
| `Color(red:` split across lines | missed — per line | caught |
| `Circle().fill(.blue)` | missed — only `foregroundStyle` was covered | caught |
| `UIColor.systemBackground` | missed | caught |
| a second file named `Theme.swift` | missed — exempt by `lastPathComponent` | caught (exempt by **path**) |

Three supporting details:

- **Matching is whole-file with newlines folded to spaces** (same length, so
  offsets still map to real line numbers). That is what makes a call split
  across lines catchable.
- **Comments are stripped before matching.** Theme.swift's own documentation
  names the APIs the rules ban, and a design note is allowed to quote a hex
  value. Line numbers survive both transforms.
- **It skips, loudly, when the tree is absent** (an artefact-only CI run)
  rather than passing vacuously. A companion test asserts `Theme.swift` still
  *contains* the raw values, so the lint cannot become true by the tokens moving
  somewhere the scan cannot see.

Proven, not asserted. `testTheLintCatchesEveryEvadedForm` plants seventeen
violating snippets — including every row above — and fails if the matcher misses
one; `testTheLintDoesNotFireOnCommentsOrGeometry` plants prose and geometry and
fails if it fires. On top of that, a file containing five of the evasions was
planted under `NADE/Gallery/` and the lint was watched failing on all five with
correct line numbers. The remaining two evasions the *compiler* blocks:
`Color(hex:)` is `fileprivate` to Theme.swift, and a second `Theme.swift` in the
same target fails the build outright.

### D24. Gallery screenshots are driven by launch arguments, not gestures
`-NADEGallery 1 -NADEGallerySection <id>` scrolls the gallery straight to a
section on appear, so each `simctl io … screenshot` is deterministic instead of
depending on a swipe landing in the same place twice. The whole gallery is
`#if DEBUG`; a release build has no path to it.

`-NADEGallery` is read through `UserDefaults` rather than
`ProcessInfo.arguments.contains`, so `-NADEGallery 0` correctly means *off* —
which the UI test relies on to launch the shipping shell.

---

### D25. Screenshot set (`docs/screens/`)

| File | What it shows |
|---|---|
| `p1-shell.png` | `RootTabView` on iPhone 17 Pro — the shipping shell |
| `p1-gallery-01-color.png` … `-13-motion.png` | one per gallery section (13 of them), iPhone 17 Pro |
| `p1-se-shell.png`, `p1-gallery-se-*.png` | the same on **iPhone SE (3rd gen), 375 × 667** — narrowest and shortest device the 18.0 target supports (E2) |

On SE the four tab labels still fit without truncation, no row overflows, and
nothing clips. The acceptance command names the device literally
(`-destination 'platform=iOS Simulator,name=iPhone SE (3rd generation)'`), so a
simulator with exactly that name has to exist:

```
xcrun simctl create "iPhone SE (3rd generation)" \
  com.apple.CoreSimulator.SimDeviceType.iPhone-SE-3rd-generation \
  com.apple.CoreSimulator.SimRuntime.iOS-26-5
```

The script that produced the screenshots lives in the session scratchpad,
not the repo (it is three `simctl` calls in a loop and would rot as a
checked-in artefact — regenerate with `simctl launch …  -NADEGallery 1
-NADEGallerySection <id>`).

Reading the screenshots back against the DS CSS and the mockup renders turned
up two real mismatches, both fixed before this was reported: the leading glyph
on a text button was rendering at the icon-only 17 pt instead of the label's
14 pt, and the ask field's send button was locked to 36 pt instead of the
screen's 38/40/44.

**Amended 2026-08-17.** The AX5 block used to be the tail of the Edge-cases
section, and `p1-gallery-10-edges.png` ended mid-card — the AX5 buttons, icon
buttons, tab bar and the Reduce Motion block were never actually in frame, so
nothing about them had been seen. AX5 (`ax5`) and Reduce Motion (`motion`) are
now their own gallery sections with their own anchors and their own
screenshots, and the AX5 block gained the icon buttons, the ask field, the
stepper, the segmented control and a radio row — the controls where a fixed box
and a scaling glyph actually collide.

---

## Known gaps left for later phases

- **1a/1c's editable spans render with an ink underline, not an accent rule.**
  The mockup's spans carry `border-bottom: 1px solid accent` — a rule under the
  text, not a typographic underline — and SwiftUI's `Text` will not give an
  underline an independent colour: `Text.underline(_:color:)`, the SwiftUI
  attribute scope's `underlineStyle` and the UIKit scope's `underlineColor` all
  lose to the run's foreground. Tried all three; the gallery keeps the underline
  (it is what marks the spans as editable) in ink. The faithful accent rule
  needs per-span layout, which P3 has to build regardless because `when_span`
  and `do_span` are separately tappable.

- Justified body text with hyphenation on the thread screen (1f) — SwiftUI has
  no justified alignment; DESIGN.md already records this as a deviation.
  Leading alignment when P2 builds `ThreadView`.
- The 2a ⇄ 2b pull-down transition (P3).
- `NSegmented` renders its options at their intrinsic widths (`fixedSize`),
  matching the DS's `display: inline-flex`. A full-width variant, if a screen
  ever needs one, is a one-line addition.
- No snapshot-diffing harness. P1 compares gallery screenshots to the mockup by
  eye; if drift becomes a problem, the gallery's section anchors already make a
  deterministic snapshot suite cheap to add.

---

## D26 — the tab bar owns its 26 pt, and the shell is a VStack

**2026-08-17, after the DESIGN.md correction pass.**

P1 read DESIGN.md's `26` as "the home indicator, which the safe area now owns"
and shipped `paddingBottom: 8` inside a `.safeAreaInset(edge: .bottom)`. The
corrected spec measures that 26 **from the bottom edge of the display**, and
the design frame's indicator region is 34 pt — so the two stacked and the
labels sat 42 pt up, visibly higher than the design and out of step with every
other bottom band in the app.

Fixed two ways, both now matching the mockup:

- `Theme.Metrics.TabBar.paddingBottom` is **26**, the mockup's own number.
- `RootTabView` is a plain `VStack` with `.ignoresSafeArea(.container,
  edges: .bottom)` instead of `.safeAreaInset`. Beyond the arithmetic, this is
  structurally right: in the mockup the tab bar is a **sibling** of the
  scrolling band, so content ends at the hairline and never scrolls underneath.
  `safeAreaInset` propagates an inset that invites exactly the scroll-under
  behaviour P2's lists would otherwise inherit for free.

`ThemeTests` was updated to assert 26; verified by screenshot at
`docs/screens/p1-shell.png`, which is the shipped shell. (A separate
`p1-shell-safearea-fixed.png` recorded the same frame under a name that only
made sense during the fix; it was byte-for-byte the shell shot and has been
removed rather than kept as a second copy — see D41.) All 37 tests still green
at the time.

---

## P1 — the adversarial-review pass (2026-08-17)

### D27. Line height is a baseline-relative box, not `lineSpacing`

SwiftUI has no line-height. The first implementation mapped CSS `line-height`
onto `.lineSpacing`, which is wrong twice over: `lineSpacing` is only the *gap
between* lines, so it does nothing at all to a single line — and a single line
is exactly what sets the height of an input, a tag, a chip and a segmented
option in CSS — and its `max(0, …)` clamp made a line box *tighter* than the
face's own impossible, which `.card-title`'s 1.2 on Cormorant (own box 1.211 em)
needs. On top of that, almost nothing used it: the DS sets `body 1.55`,
`.btn 1.2` and `.card-title 1.2`, and `NButton`, `NTabBar`, `NTag`, `NChip`,
`NSegmented`, `NRadioRow` and `NTextField` set no equivalent, so every one of
them was a few points short of its designed height.

`nadeLineHeight` now takes the face's real line box at the current Dynamic Type
size, works out the delta to the CSS target, puts the whole delta between the
lines and **half of it above and below the block**:

```
n·natural + (n−1)·delta + 2·(delta/2) = n·(natural + delta) = n·size·multiple
```

An `n`-line paragraph therefore measures exactly `n × size × multiple`, and a
one-line control measures exactly `size × multiple`. `delta` may be negative —
that is how a tighter line box is expressed, and negative vertical padding is
what makes it work.

Applied at: `.btn` (1.2, Cormorant), `.card-title` (1.2), `.card-body` (1.55),
`NTag`, `NChip`, `NSegmented`, `NRadioRow`'s label and hint. **Not** applied to
`NTextField`: SwiftUI's `TextField` already reserves a line box a shade taller
than the CSS one (≈22 pt at both 13.5 and 14), so padding + `min-height`
reproduces the CSS box on its own and the modifier on top double-counts,
leaving every field 2–3 pt too tall. Measured, not assumed —
`ComponentGeometryTests.testAskFieldHeightsMatchEachScreen`.

Measured face metrics, for the record: Cormorant Garamond 1.211 em, Lora
1.280 em, SF 1.193 em.

### D28. The 44 pt hit target is invisible

DESIGN.md never mentions a minimum touch target — it draws a 30 × 30 stepper
box, a 46 × 26 switch, a 16 pt radio dot — so 44 is the HIG's number, not the
design's, and it is added **without moving a pixel**. P1 had no control with a
44 pt region: icon buttons were 36, the toggle 26 tall, stepper boxes 30 × 30,
chips ~29, segmented options 32–34, and the tab bar's `9 / 10 / 26` padding sat
*outside* each button so a tab's own region was the ~43 pt glyph-and-label stack
with 35 pt of dead space beneath it. `contentShape` was already there; it
changes the *shape* of a hit region, never its size.

Two mechanisms, chosen per control:

- **`.nadeHitTarget()`** — an overflowing `Color.clear` background with
  `.frame(minWidth: 44, minHeight: 44)` and its own `contentShape`. A background
  is laid out inside the parent's slot, may overflow it, and contributes nothing
  to the parent's measured size, so the drawn box and the row height are
  untouched. Used on the icon button, the toggle, the stepper boxes, the chip
  and the segmented option — every control whose row is sized by something else.
- **Moving the padding inside** — the tab bar's `9 / … / 26` now lives in each
  column rather than around the row. Identical pixels, and each tab's frame is
  the full height of the bar.

Deliberately **not** applied to `NRadioRow`. Its rows stack edge to edge, so a
target taller than the row would overlap its neighbours and the last one drawn
would quietly take the other's top edge. 1c's row measures ~66 and 1d's ~40,
both full-width; a 40 × 358 row is not the failure mode this is about.

Proving it needs both halves, and they live in different suites on purpose:
`ComponentGeometryTests` measures the hosted view and requires the *drawn* box
to still be 26 / 30 / 38 / ~29; `HitTargetUITests` reads the element's frame in
the running app and requires the *hit* region to be ≥ 44, and taps 3–5 pt
outside the drawn edge and requires the control to have acted. Remove
`.nadeHitTarget()` and the chip measures 30.33 in the UI test — which is exactly
what it should still measure in the unit test.

### D29. The shell keeps all four screens alive

`RootTabView` used to `switch` on the selection, so only the active screen
existed and switching tabs destroyed the outgoing one — with it every `@State`
it owned, its scroll offset, its `NavigationStack` path and any `.task` it had
running. With placeholder screens that is invisible. The moment P2 puts a mail
list, a note draft or an SSE stream behind a tab it is a bug report, and the
claim that later phases "only replace `screen(for:)`" was unsafe.

All four screens are now always constructed, in a `ZStack`; the inactive three
are `.opacity(0)`, `.allowsHitTesting(false)` and `.accessibilityHidden(true)`.
This is what `TabView` would have done for free (D22).

`ShellStateUITests` taps a counter on each screen a different number of times,
rotates through all four tabs and requires every count to still be there.

### D30. The tab bar has tab-bar semantics, not four loose buttons

Also what `TabView` would have given for free. `NTabBar` now marks the row of
buttons `.accessibilityElement(children: .contain)` with
`.accessibilityAddTraits(.isTabBar)` and the container label "Tabs", and each
tab carries `.accessibilityValue("Tab N of 4")` — the position VoiceOver's own
tab bars announce and SwiftUI does not derive.

Three implementation notes worth keeping:

- `children: .contain` is **not** optional. Without it the container's label,
  identifier and traits propagate *down*, and every tab is renamed "Tabs" with
  the identifier "tabbar". Verified by dumping the hierarchy both ways.
- The container is applied to the button row, not to the whole bar, so the
  decorative top hairline stays outside it.
- Each tab's glyph and text are `.accessibilityHidden(true)` and the label
  collapses with `children: .ignore`, so a tab is one stop rather than three —
  an SF Symbol brings its own name ("Sparkle", "Get Mail", "Plain Text
  Document") and that must never be what a tab announces.

Honest limitation: SwiftUI's `.isTabBar` trait does not change the element
*type* XCUITest reports — the container comes back as `Other`, not `tabBar` —
so `TabBarUITests` queries it by identifier. What is assertable, and asserted,
is that the container exists, is named "Tabs", and is the parent of exactly the
four tabs, each announcing its position.

### D31. One component per screen variant, never an average of two

Three components claimed to cover several screens while implementing an average
of them. Each now carries the design's own numbers as named presets:

- **`NRadioRow.Metrics`** — 1c Invocation is label 15, gap 11, padding 11, dot
  aligned to the first baseline; 1d Ends is label 14, gap 11, padding 9, centred,
  with a trailing value. The code used label 15, gap 9.2, padding 11 for both.
- **`NSegmentedMetrics`** — DS `7 / 12` @ 13, **1c `6 / 14` @ 13**, 1i
  `5 / 12` @ 12. The gallery rendered 1c's Status control at the DS metrics.
- **`NTextField.Metrics`** — see D21.

Two further mismatches found in the same pass and fixed:

- **`NStepper.gap` was 8; 1d's row is `gap: 12`** end to end, and the unit
  chip's internal label→chevron gap is 8 in the mockup and was 6 in the gallery.
- **`NRadioValue`'s colours were inverted.** The code drew the selected row's
  value in **accent** and the rest in `ink55`. The mockup's own
  `valColor: s.ends === e.key ? 'var(--color-text)' : DIM` (and DESIGN.md §3 1d)
  make the *selected* value plain ink and the unselected ones `ink62` — it is
  the muted state that is coloured, never the selected one.

### D32. Where SwiftUI's box model differs from CSS's, and by how much

CSS `border` adds to an auto height; SwiftUI's `strokeBorder` draws *inside* the
frame. On an outlined control that is a 2 pt difference, and this design has
nothing but outlined controls. Drawing the border outside the frame instead
would misalign it with the fill and the press wash, so the SwiftUI idiom wins
and the difference is recorded rather than fudged.

The residual, measured, is at most ~1.1 pt on any control, because the other
direction cancels most of it: SwiftUI's `TextField` reserves a line box about a
point taller than CSS's. `ComponentGeometryTests` asserts against the CSS
number with a stated ±1.0–1.5 tolerance and never wider; the ask-field variants
are 2–6 pt apart, so the tolerance cannot swallow a wrong preset.

### D33. Geometry is measured, not recited

The previous suite asserted `NToggle.trackWidth == 46` and called that "the
design's geometry". It is not. It stays green if the view stops using the
constant, if a `.padding` is added around it, or if a line-height change moves
every control in the app — and it omitted every value this review found wrong.

`NADETests/RenderMeasure.swift` hosts a real view in a `UIHostingController`
(with `safeAreaRegions = []`, or every measurement comes back 54 pt tall) and
reads the size UIKit resolves. `ComponentGeometryTests` uses it for line boxes,
button widths, `flex: 1`, ask-field heights, tag/chip/segmented boxes, the
toggle, the stepper, both radio rows, card padding and the tab bar. `ThemeTests`
still pins the tokens — that is a different job, and both are needed.

The same technique closes the font suite. `UIFont(name:)` being non-nil proves
the face is *installed*, not that `Theme.Font` reaches it: with
`Theme.Font.heading` swapped for `Font.system`, all eight registration tests
still pass — including `testResolvedFacesAreNotTheSystemFallback`, the one that
claimed to catch exactly this. `RenderedFaceTests` renders `Text` through the
Theme API and compares the resolved size with what Core Text lays out in the
expected face, and separately with what the system face would produce; under the
same swap, all four of its tests fail.

### D34. v1 says "Ask", never "Send"

DESIGN.md §4 is explicit that the primary button never reads "Send", because v1
takes no outbound action. The gallery exposed "Send" to VoiceOver twice and
printed "send button" as visible copy. DEBUG-only is not an exemption — it is
still the Phase 1 artefact, and it is the thing a reviewer reads the design off.
Every ask-field button is now labelled "Ask" (1f's is "Ask for a draft"), and
`AccessibilityUITests.testNothingInTheGallerySaysSend` sweeps the buttons and
the visible copy of two sections for the word.

The gallery also mislabelled 2b: it called the button a "sparkles send" and
rendered `sparkles`. 2b's glyph is an **upward arrow** at 18 pt; `sparkles`
belongs to 2a's feed bar and 1f, at 16.

### D35. Contract fixtures are enumerated, and streams are parsed

`ContractFixturesTests` listed 21 JSON names and 3 SSE names by hand. The
directory now holds 57 and 4; a hardcoded list means the synchronized group can
copy a brand-new malformed fixture into the bundle with nothing checking it, and
silently stops covering whatever the backend adds. The set is now enumerated
from `docs/contract/` and cross-checked both ways against the bundle, so a
fixture that never arrives and a stale one that lingers both fail.

The SSE assertion was `contains("event: route")` — satisfied by any malformed
stream holding the three substrings in any order. Streams are now parsed and
checked against what `docs/contract/README.md` specifies: `event:`/`data:`
pairs, `route` first and once, every payload valid JSON, exactly one terminal
`done` **or** `error` and it is the last event, and a trailing blank line.
`testTheStreamValidatorRejectsEveryMalformedShape` feeds it eleven malformed
streams — every one of which passed the old check — and fails if any is
accepted.

### D36. The deployment target is asserted across every configuration

`InfoPlistTests` read `MinimumOSVersion` out of `Bundle.main`, which is whatever
configuration the tests happened to be built in — always Debug. Release could
drift back to 26.5 (D1's exact bug) and stay green forever, because tests never
run against Release. The suite now also parses `NADE.xcodeproj/project.pbxproj`
and requires **every** `IPHONEOS_DEPLOYMENT_TARGET` in it to be 18.0 — eight of
them: the project and three targets, Debug and Release each.

### D37. Empty labels, and trailing values, reach VoiceOver

Two E6 holes:

- `NButton("")` and `NChip("")` copied the empty string straight into
  `.accessibilityLabel`, so the gallery's own empty-string edge case produced
  interactive elements VoiceOver could not name. An empty title is a *layout*
  edge case (E4), never a licence to ship an unnamed element: the label now
  falls back to the trait's noun ("Button", "Filter"), and both initialisers
  take an explicit `accessibilityLabel:` for callers that have a better one.
- `NRadioRow`'s explicit label carried only the label and the hint, so 1d's
  "16 Sep 2026" and "12 runs" were never spoken and the three Ends options were
  indistinguishable. The trailing value is now a first-class `value:` parameter
  that renders *and* becomes the element's `accessibilityValue`. The generic
  trailing-view initialiser is gone — it was the thing that made it possible to
  render something no one could hear.

### D38. `NSegmented` no longer defeats its own overflow defences

`fixedSize(horizontal: true)` asked for the full intrinsic width
unconditionally, so the options were never handed a constraint and their
`lineLimit(1)` and `minimumScaleFactor` could not engage — long labels ran
straight off a 320 pt screen. Removed. The control still takes its intrinsic
width when there is room (the DS's `display: inline-flex`) and compresses when
there is not.

`fixedSize(horizontal: false, vertical: **true**)` was added in its place, for a
different reason: `VHairline` is a `Rectangle`, which has no ideal height and
will fill whatever it is offered, so a container proposing a tall box stretched
the whole control.

---

## P1 — the second review pass (2026-08-18)

Ten findings, nine of them about **whether the tests prove anything**. Every one
was checked the same way: plant the regression the test claims to catch, watch
what happens, then fix and watch it fail. Three of the ten turned out to be
already closed by the first pass and are recorded as verified rather than
re-fixed; three were closed but *not by the assertion that claimed to close
them*; the rest were real.

### D39. A test nobody has seen fail is not evidence

Every claim below has a recorded red. Three findings survived a first
mutation and died to a second, which is the reason the discipline exists:

- **F14.** `testBothFamiliesOfferTheMonospacedNumbersSelector` looked live. It
  is — but the first probe (expect selector **4**) passed, because both
  families genuinely list selector 4 ("Default") under number-spacing type 6.
  Cormorant offers `[0 Monospaced, 4 Default]`; Lora offers
  `[0 Monospaced, 1 Proportional, 4 Default]`. A probe has to name a selector
  the faces do *not* have (7) before the assertion goes red. The test now also
  asserts the selector's **name** is "Monospaced Numbers", so an index alone
  cannot satisfy it.
- **F23.** With `NButton.clampsGlyph` forced to `false`, only the pure-function
  assertion failed. Every *rendered* assertion in
  `testIconButtonBoxDoesNotGrowAtAX5` still passed, because the box's own
  `.frame` pins its size no matter what the glyph does, and the test measured a
  bare `NIcon` with the ceiling applied by hand rather than the button's. See
  D42.
- **F16.** The stream validator rejected all eleven planted shapes — and
  accepted three more that `docs/contract/validate.py` rejects. See D43.

### D40. Ten geometry values were decoupled from their constants, and 78 tests stayed green

The first pass built `ComponentGeometryTests` and said geometry was now
measured. It is, for the values it measures. To find the ones it did not, ten
components were edited to keep their `static let` and render something else:

| Constant | Rendered instead | Caught before |
|---|---|---|
| `TabBar.paddingTop` 9 | 4 | no |
| `TabBar.paddingBottom` 26 | 40 | no |
| `TabBar.iconLabelGap` 5 | 15 | no |
| `TabBar.iconSize` 18 | 28 | no |
| `TabBar.paddingHorizontal` 10 | 34 | no |
| `NTag.paddingH` 10 | 30 | no |
| `NChip.paddingH` 12 | 30 | no |
| `NButton.labelGap` 6 | 26 | no |
| `NToggle.knobDiameter` 20 | 8 | no |
| `NToggle.knobInset` 3 | 9 | no |
| `NStepper.countSize` 15 | 9 | no |
| `NButton.disabledOpacity` 0.45 | 0.6 | no |
| `NTextField.Metrics.shape` | `.pill` ⇄ `.rounded` | no |

All thirteen now fail. The tab bar's four vertical numbers come out of one
equation — `1 + 9 + glyph + 5 + label + 26` — with the two variable terms
rendered rather than assumed. `paddingHorizontal` changes nothing a unit test
can see (the four columns are `maxWidth: .infinity` and absorb it), so it is
measured in `TabBarUITests` as the gap between the screen edge and the first
column. Tag and chip widths are measured against their **rendered** label, so
the same assertion also catches a component that swapped Lora for Cormorant.

### D41. Some geometry is only visible in pixels

Three of those twelve are invisible to `sizeThatFits` in principle, not by
oversight:

- the toggle's knob lives *inside* a 46 × 26 track, so the component measures
  the same whatever it draws;
- the stepper's count has `min-width: 22`, which swallows any digit narrower
  than the box;
- an opacity is not a size at all;
- and a *shape* is not a size either — `.pill` and `.rounded` measure
  identically. They are told apart on the outermost row of the render, where a
  corner of radius `r` insets the flat edge by `r`: 4 pt for `radius-md`, 19–22
  for a pill. The allowance is `sqrt(2r / scale)` — how far the arc stays within
  one device pixel of the tangent — derived from the geometry rather than chosen
  to fit.

`RenderMeasure.bitmap(of:)` renders through `ImageRenderer` at a **fixed** 3 px
per point — fixed, so a bitmap expectation means the same thing on an @2x SE as
on an @3x 17 Pro — and hands back the pixels. Three readers sit on top:

- `bounds(matching:)` — the bounding box of a colour.
- `longestRun(matching:onRowAt:)` — needed because an "on" toggle's knob and its
  track border are the *same* accent. A rectangular inset cannot separate them:
  a capsule's ring curves inward at the top and bottom, so any inset deep enough
  to miss the ring also clips the knob. The knob is instead the longest run
  across the track's middle — 20 pt against the ring's 1.
- `alpha(atX:y:foreground:over:)` — solves `out = bg + α·(fg − bg)` from **one
  fully covered pixel** of the accent border. Deliberately not an average over
  the render: text is smoothed with a contrast-dependent gamma, so a whole-button
  ink total is *not* linear in the opacity. Measured, a sum over the button reads
  0.486 in sRGB and 0.588 in linear light for a true 0.45; the single-pixel solve
  reads 0.45 in sRGB, which also settles that SwiftUI composites `.opacity` on
  the encoded values rather than in linear light.

Tolerances are `2 / bitmapScale` — two rendered device pixels, which is what
antialiasing on a curved edge costs — and are stated as that rather than as a
number chosen to make the test pass.

### D42. The icon button's ceiling is checked by where the glyph lands

`testIconButtonBoxDoesNotGrowAtAX5` measures the box (which cannot grow, `.frame`
pins it) and a hand-clamped `NIcon` (which is not the button). Neither is F23's
claim. What F23 claims is that the glyph does not overrun its circle, and a
`.frame` does not clip, so the button is now rendered inside a canvas three times
its size: an overflowing glyph paints *outside* the circle and the accent ink's
bounding box says so. With the ceiling removed, an 18 pt glyph at AX5 paints
47.7 pt tall inside a 44 pt circle, and all four design pairs (16/38, 17/40,
18/44, 17/36) fail.

### D43. The SSE validator parses blocks, because a line reader cannot see framing

`docs/contract/README.md` says the wire bytes are `event: <name>\ndata: <json>\n`
with **a blank line between events**. A line-by-line reader pairs each `event:`
with the next `data:` and skips blank lines, so it cannot see the framing at all.
Three streams that `docs/contract/validate.py` rejects passed here:

- two events with no blank line between them,
- a blank line *inside* an event,
- two blank lines between events.

`problems(inStream:)` now splits on the blank line and requires each block to be
exactly two lines, which is `validate.py`'s `parse_sse` move for the same reason.
Fourteen malformed shapes are planted, and the validator has to reject all of
them.

**Not claimed:** the payloads. `validate.py` checks every frame against API.md's
shapes; P1 has no models, so this checks only that each payload is valid JSON.
P2 decodes them.

### D44. The deployment-target count is derived, not guessed

D36 says eight settings and is right. The test said `>= 6`, and two of the eight
could be deleted with it still green — verified. Xcode gives every
`XCConfigurationList` one `XCBuildConfiguration` per configuration, so the test
now requires `settings == lists × 2`: a new target cannot arrive without its own
pin, and an existing one cannot drop it.

A second hole: the sweep reads the *literal* values in `project.pbxproj`, and an
`.xcconfig` attached to any configuration would override them invisibly. A test
runs inside the simulator and cannot shell out to `xcodebuild
-showBuildSettings`, so the honest close is to require that no configuration has
one — `testNoConfigurationDefersToAnXcconfig`. Still not claimed:
`xcodebuild -xcconfig …` passed on the command line. Nothing readable from
inside the simulator can see that.

### D45. Every gallery anchor can reach the top of the screen

`-NADEGallerySection motion` produced a screenshot **byte-identical** to
`-NADEGallerySection ax5b`: `motion` is the last section, so
`scrollTo(_:anchor: .top)` clamped at the end of the content and both landed on
the same frame. `docs/screens/` therefore claimed a Reduce Motion shot that was a
second copy of AX5 under a different name — the same defect class F25 was about.

The gallery now ends with one screen of `Color.clear`
(`.containerRelativeFrame(.vertical)`), so the last section can actually be
scrolled to the top. The whole set was re-shot and checked for duplicates by
hash; 22 files, 22 distinct.

Reading the images back also caught a system notification banner
("Ready for Apple Intelligence") sitting over `p1-se-shell.png`, because it was
the first shot taken after that simulator booted. Re-shot.

### F11's second `Theme.swift`: not reachable in this target

The lint's exemption is by path, not by `lastPathComponent`, and
`testTheExemptionIsAPathNotAFilename` fails if that is reversed — verified. The
scenario it guards against cannot actually occur here, though: adding
`NADE/Gallery/Theme.swift` fails the *build* with "Multiple commands produce
Theme.stringsdata", because `NADE/` is a synchronized file group and two files
of that name cannot coexist in one target. The path exemption is still the right
call — it costs nothing and does not depend on that staying true — but the
finding's premise is weaker than it reads.

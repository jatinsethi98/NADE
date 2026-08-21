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
`InfoPlistTests.testTheBuiltConfigurationTargets26` reads `MinimumOSVersion`
from the built bundle — which only ever sees Debug — and
`testEveryBuildConfigurationTargets26` parses `project.pbxproj` and requires all
eight settings to agree. See D36; the earlier claim that the bundle check alone
caught a drifting configuration was false.

**Superseded by D98**, which raises the target to 26.0 for the Liquid Glass
pass. Both assertions moved with it, keeping their derived count — the part
that has caught real drift — and both were renamed rather than left saying 18.

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

---

## P1 — the design-parity audit (2026-08-18)

A pass comparing the shipped component layer against `Email App.dc.html`
directly, by measuring rendered pixels rather than re-reading the source. Four
findings, all closed here. 108 tests green.

### D46. The glyph's box is the design's, not the font's

`NIcon` sized itself by `Font.system(size:)`, so its layout box was the SF
Symbol's **font line box** — ~24 pt at size 18, and a different number for every
symbol. Every glyph in the mockup is an `<svg width="18" height="18">`.

Measured off `docs/screens/p1-shell.png`: the tab bar stood at **78.67 pt**
against the mockup's 75.28, and because each column is its own `VStack`, the
four glyph boxes gave the four labels four baselines — ink tops at
39.00 / 39.33 / 41.00 / 41.33 pt below the rule, a **2.33 pt spread**, with
glyph ink running 15.0 / 16.0 / 18.67 / 20.0.

`NIcon` now carries `.frame(width: size, height: size)`. It does not clip; the
ink still renders at the font's own size and is centred, which is what a
`viewBox` lets its paths do. Deviation #1 covered the Lucide→SF swap as a change
of *shape*; it never covered the change of *box*, and DESIGN.md §2 now says so.

Re-measured after the fix: bar **75.33 pt**, label spread **0.00 pt**.

### D47. The tab label's box is set, not derived

The label was the term that decided the rest of the bar's height and it had no
line-height at all, so its box was Lora's own ~13.4 rather than the inherited
`line-height: 1.55` (16.275).

`nadeLineHeight` is the usual answer and it is wrong here: it derives its delta
from `UIFont.lineHeight`, which holds at the 13–17 pt the modifier was measured
against but not at 10.5, where Lora reports **13.44** and SwiftUI lays out
**14.33**. The modifier therefore overshoots by **1.06 pt** at this size. A tab
label is a fixed one-line box, so `NTabBar` sets it with a `@ScaledMetric`
frame, which is exact and still scales.

**This is a live crack in `nadeLineHeight`, not a closed one.** The same
overshoot applies wherever it is used below ~13 pt — `NTag` at 11 and `NChip`
at 12.5 — and their geometry assertions run at the `cssBox` tolerance (~1.9 pt),
which is wide enough to hide it. Nothing here fixes that; it is written down so
the next pass does not rediscover it as a surprise.

### D48. `.askCentred` is gold before anyone taps it

The mockup's centred ask field (2a focus / 2b, line 57) carries
`border-color: var(--color-accent)` **on the element**, and DESIGN.md §2's table
and §3-2a·4 both repeat it. `NTextField.Metrics` had no field for a resting
border, so the preset drew a hairline until focused — and the field is not
focused when that screen opens, so 2b would have shipped grey.

`Metrics` gained `border: Border`, defaulting to `.divider`, `.accent` on
`.askCentred` alone.

The interesting part is why nothing caught it. The gallery passed
`showsFocusRing: true` on that one row — a flag whose own doc comment says
"gallery / preview only" — which made the screenshot look right. And
`testPillAndRoundedControlsActuallyDrawDifferentCorners` rendered `.askCentred`
at rest and searched the top row for the **hairline** colour, so the test
asserted the bug. Both now read the colour from `Metrics.restingBorderColor`,
and `testEachPresetDrawsItsOwnRestingBorder` reads the outline off the render
for all six presets.

### D49. A sum of its own parts is not a measurement

`testTabBarHeightIsBuiltFromItsOwnMetrics` measured `icon.height` and
`label.height` from the very views the bar is built from, then asserted the bar
equalled their sum plus the padding. That is an identity. It proved the padding
was applied and nothing about the two terms that decide the height — it was
green throughout the 78.67 pt bar and the 2.33 pt baseline spread.

This is D33's failure mode recurring one level up: the previous round replaced
constants-compared-with-themselves, and the replacement compared a *render* with
itself. The fix is that the expectation contains no term read back off the view:
`1 + 9 + 18 + 5 + 10.5 × 1.55 + 26`, every term a `Theme` constant, cross-checked
against the literal 75.275. `testEveryTabGlyphOccupiesTheSameDesignBox` pins the
box that D46 was about.

---

## P2 — mail, live

### D50. The Mail tab's root is 1g, and 1e is pushed from it

The mockup draws 1e and 1g as independent artboards and never says how you reach
one from the other. 1e's header is `{{ filter }}` plus a non-interactive account
chip; its rows have no `onClick` either. Both edges were inferred.

v1 makes **1g the root**. Its ACCOUNTS row is the mockup's own "All inboxes"
entry, so tapping it opens the inbox; every LABELS row opens its own mailbox;
the STANDARD "Sent" cell opens `SENT`. 1e then has somewhere to go back to, and
1g — which is the only entry point to Settings — is somewhere you land rather
than somewhere you have to find.

1e gains a leading `‹` at the title's own 23 pt, inside the existing baseline
row. Registered as deviation 41.

### D51. Tab-bar visibility is a property of the top route, not of stack depth

`docs/DESIGN.md` §2's navigation map keeps the bar on 1e, 1g **and** 1k, all of
which are pushes, and removes it only on 1f. So "deeper than the root hides it"
is wrong on the very first push, and three assertions in
`ThreadNavigationUITests` fail under any depth-based rule.

`AppNavigation` is hoisted above `RootTabView` and owns the selection, the mail
path and the selected mailbox. `showsTabBar` switches on the **active tab**
first, so a thread pushed on Mail cannot hide the bar while Notes is on screen —
which matters because D29 keeps all four stacks alive underneath.

A `PreferenceKey` was considered and rejected twice over: every inactive screen
would emit one, and preferences propagate as a layout side effect, which
flickers for a frame.

**The obvious restoration test is impossible.** "Push a thread, tap Notes, come
back" cannot be written: while 1f is on screen the tab bar is gone, so
`tab.notes` does not exist to tap. The thread is restored by launch argument
instead, popped, rotated through the tabs, and re-pushed.

### D52. The edge-swipe had to be put back by hand

Every screen draws its own nav bar, so the system one is hidden throughout.
UIKit disables `interactivePopGestureRecognizer` whenever the bar is hidden, on
the reasonable assumption that a screen with no visible back button has no back.
Here that assumption is wrong.

`nadeInteractivePopGesture()` restores it with **our** delegate rather than
`nil`: clearing the delegate re-enables the gesture at the root too, where there
is nothing to pop, and UIKit has historically deadlocked the stack when that
completes. `viewControllers.count > 1` is the whole guard.

Measured, not assumed — `testTheSwipeBackGestureStillPops` failed before this
existed and fails again if it is removed. (It also needed a real
`UIScreenEdgePanGestureRecognizer` gesture: `app.swipeRight()` starts in the
centre of the screen and the edge recogniser ignores it by design, so that form
of the test would have failed whether or not the app worked.)

### D53. Two database files, not one data source

P2 ships a DEBUG fixture world alongside the live client, and "swap the
`MailSource`" is **not** provenance. Both sources would write the same rows; a
live launch deliberately does not reset, because it has to keep cached mail for
offline; so fixture rows that no live id happened to overwrite would sit beside
real mail indefinitely. `mail.sqlite` and `mail-fixtures.sqlite` make the
separation structural.

### D54. Every write is column-scoped

`thread` carries the list fields and the detail fields in one row, so that
opening a thread cannot contradict the row behind it. That only works if the
writes are scoped: a whole-record upsert built from a `WireThreadRow` writes nil
into `detail_*`, and a routine list refresh after opening a thread would make it
"never loaded" again — footer, `partial` caption and every message gone until
the detail was fetched a second time.

`MailStoreTests` asserts **both** directions. One alone does not see it.

### D55. `msg_count` is the server's number and `partial` explains the gap

`docs/contract/` had no `partial: true` fixture, so neither lane had ever
serialised or decoded the state `API.md` §2 says clients must surface.
`thread_partial.json` was added at P2, through `generate.py` and `validate.py`
and into `api/contract_tests.rs` — a fixture only one side can produce is not a
contract.

It also corrected the validator: `msg_count == len(messages)` is the right
assertion for a complete thread and the wrong one for a partial. Both relations
still bind in the direction that can catch a defect (`len(messages) <=
msg_count`, newest present message no later than the row's `ts`).

### D56. A `CHECK` constraint would have undone the enum fallback

`kind` and `status` decode to `.unknown(raw)` rather than throwing, so one new
server value cannot blank a screen — a decode failure inside a
`ValueObservation` ends the observation, it does not skip a row.

`mailbox.kind` and `account.status` therefore carry **no** `CHECK`. Pinning them
to today's values would move the same failure from the decoder to the write, and
a forward-compatible decode that then fails the transaction is not forward
compatible.

### D57. A NUL is stripped, because SQLite truncates at one

The wire cannot carry a `NUL` — PostgreSQL rejects `0x00` in a `text` column, so
the server cannot store one, and the backend lane wrote three separate fixes to
keep it that way. But SQLite silently **truncates** at a NUL, so a client that
passed one through would turn `before␀after` into `before` and read as a parser
bug for a week. The store drops the character and keeps everything after it.
`testANulIsStrippedRatherThanTruncatingTheRestOfTheString` is what says so; the
first version of that test asserted byte-exactness and failed, which is how the
truncation was found.

### D58. Cancellation is not a failure, and a race is not a network error

Two defects the first live run surfaced, both of which had put
"The server sent something unexpected." on screen:

- A screen going away mid-request throws `CancellationError` from `await`, and a
  generic `catch` turned that into a user-facing server error.
- The mail list's `.task` and the shell's can start in either order, so a page
  could be filed against a mailbox row that had not arrived. The store now
  throws `MailStoreError.unknownMailbox` — naming the precondition instead of
  surfacing SQLite's "FOREIGN KEY constraint failed" — and `MailSync` fetches
  the mailboxes and retries once. The same shape applies to a thread detail,
  which is why `loadThread` takes the mailbox it was opened from.

### D59. An unreachable server must not make a paired device look unpaired

Found by `OfflineUITests`, not by reading. A transport failure left `state` at
its initial `.unpaired`, so 1g rendered "Not paired yet." over a full mailbox
list — telling the user the setup they had completed had not happened, because
the Wi-Fi was down. A transport failure with mailboxes in the store now means
`.ready`: the rows are the truth and the banner carries the rest.

The banner is a **second slot**, never a replacement for the state. `API.md` §2
says a thread's `partial` flag is *produced by* an upstream failure, so a thread
with gaps and a connection that is currently down are routinely true at once,
and one string would have to overwrite one of them.

### D60. The first run needs a state machine, because the server answers empty

`docs/PLAN.md` puts the initial Gmail sync at ~1–2 minutes and `api/mail.rs`
deliberately returns `mailboxes: []` until it finishes. "Fetch once at launch"
therefore lands in an empty app, and with no poll, no refresh control and no
completion event the finished sync never reaches the screen until the next
launch.

`AppState` is an explicit enum — `unpaired`, `needsGmail`, `syncPending`,
`ready` — with one designed rendering each, and `syncPending` polls
5 s → 30 s for five minutes and stops the moment a non-empty list lands. P3
replaces it with the webhook.

`409 needs_reauth` writes `account.status`, because that is what makes 1g's
sub-label and 1k's "Sign in again" row appear. Surfacing it only as an error
would tell the user something is wrong and give them no way out.

### D61. The token is bound to the server that minted it

One Keychain item holding `{baseURL, token}`, not two settings. Independently
stored, they can be recombined: set only `NADE_BASE_URL`, or edit the URL in
Settings, and the next request sends a bearer minted by server A to server B.
Asking for a credential names an origin, and a mismatch clears the item rather
than leaving it for a later edit to point back at.

`NADE_BASE_URL` is read from the process environment and is deliberately **not**
in the shared scheme: that file is committed, the scheme's launch settings are
inherited by the test action, and a token there would point every unit test at a
live server.

The pairing code is single-use and the token exists exactly once, so a Keychain
write that fails *after* the server has spent the code is reported as its own
failure — the user needs a fresh code, not another attempt with the same one.

### D62. The Release fixture exclusion works, and is proven by a script

`EXCLUDED_SOURCE_FILE_NAMES` on the app target's Release configuration **does**
filter a `PBXFileSystemSynchronizedRootGroup`; this was unverified when the lane
was planned and is now measured — a Release build carries `calendar.json` and
nothing else.

The check is `scripts/assert-release-has-no-fixtures.sh` rather than a test,
because XCTest only ever runs against Debug (the D36 limitation). The obvious
form does not work: `find … -name '*.json'` exits 0 whether it finds none or
ten, so a comment reading "→ calendar.json only" is documentation, not a gate.
The script resolves `BUILT_PRODUCTS_DIR` from the build system, exits non-zero
on any fixture, **and** cross-checks that the Debug build carries nine — without
that, "excluded from Release" and "never built at all" look identical.

### D63. `NADE/Fixtures/mail` is a manifest, not a directory listing

Nine files, byte-identical to `docs/contract/`, named in `FixtureSeed.names` and
asserted as a *set*. A per-file loop over whatever happens to be in the
directory passes over an empty one, which is the failure most likely to arrive:
a half-finished copy, or a rename on the contract side nobody mirrored.
`.gitattributes` gained `*.json -text`, so a claim about bytes is about bytes.

### D64. `import GRDB` and `URLSession` are each confined to one directory

The store returns NADE's own value types and an opaque cancellable, so every
test in this target reaches the database through `@testable import NADE` alone —
`@testable` exposes NADE's internals but does not re-export a package module,
and a record type leaking out of `Store/` would force GRDB into the test
target's dependencies and a second copy into the link.

`ModuleBoundaryTests` caught a real leak the first time it ran:
`HTTPMailSource`'s convenience initialiser took a `URLSession`.

### D65. The screenshot script is checked in, and pins three things

D25 kept P1's in a scratchpad because it was three `simctl` calls in a loop.
This one is not: `-NADENow` pins the clock (the fixture world is frozen but
`listTime` is a function of *now*), `TZ=UTC` pins the day boundary ("today" is
the device's calendar day), and `status_bar override` pins the clock glyph and
removes the notification-banner class of failure D45 found. A shot set that
cannot be regenerated identically twice is not evidence.

It also hash-checks for duplicates, which is what caught D45.

### D66. `ShellStateUITests`' mail leg was strengthened, not deleted

It used to tap `screen.mail.taps`, a counter on the placeholder screen that
existed only so the test had something to count. Replacing the placeholder
deleted that element — and deleting the assertion with it would have been the
wrong move, because the property D29 protects is now load-bearing rather than
hypothetical. Mail asserts the state it actually has: a pushed mail list, on a
mailbox that is not the default. No navigation stack could have lost a tap
counter; this one can lose a stack.

### D67. The copy sweep covers app-authored names only

DESIGN.md §4 forbids a control that promises an outbound action. A sweep over
every control trips on the fixture's own mail — `thread.json`'s body says "I'll
send the invites", and a mail row's accessibility label is composed from the
sender, subject and snippet, so the row *is* a button whose name is the server's
words. A test that fails on legitimate content gets weakened or deleted rather
than fixed.

The sweep excludes elements whose identifier is content-derived, and elements
with no identifier at all (XCUITest synthesises unnamed wrappers around rows).
`testTheCopySweepWouldCatchAnAppNamedControl` proves it can still fire: the
word is on screen, in the mail, right now.

### D68. Lora Italic, because `Font.italic()` on a roman family is a silent no-op

DESIGN.md sets 1e's caption, 1f's footer and every state caption in italic, and
`UIAppFonts` listed four faces, none of them italic. `Font.italic()` on a family
with no italic member renders the roman without complaining — the D48 failure
class, invisible in a screenshot.

The cut is pinned with `instantiateVariableFont(f, {"wght": 400},
updateFontNames=False)`. **Not** `True`: it rewrites the PostScript name to
`LoraItalic-Italic` and breaks the `Font.custom("Lora-Italic", …)` lookup. 400
is the italic VF's own default instance, so no rename is needed.

The test follows `RenderedFaceTests`' existing pattern — compared against Core
Text in the exact expected face *and* against the system face — plus one more:
a `bodyItalic` pointing at `Lora-Regular` would satisfy both of those at once.

### D69. Geometry found two real defects, and neither was fixed by widening a tolerance

`MailGeometryTests` derives every expectation from `Theme` constants and
DESIGN.md's numbers, with no term read back off the view under test. Two missed:

- the mail row was 2.17 pt short, because `agent_note` was the one label on 1e
  without an explicit line box and was rendering at Lora's natural height rather
  than the DS's inherited 1.55;
- the agent-card expectation assumed a one-line summary while the fixture's
  wraps to two, which made the expectation a fact about the text rather than
  about the spec.

The first was a bug in the view; the second was a bug in the test. Both were
fixed at the source. Two planted regressions — a 22 pt left inset, and a
`set unread = 0` in the detail write — were confirmed to go red.


---

## P2 — the post-implementation review pass (2026-08-19)

An adversarial Codex review of the finished lane returned 19 findings — 5
critical, 12 major, 2 minor — and essentially all of them were real. The ones
that changed the design:

### D70. The server field had nothing behind it

`PairingView` showed a server URL and `pair()` never read it: pairing always
went to a frozen `origin` defaulting to `http://localhost:8080`. On an installed
phone `localhost` **is** the phone, so Settings could not do the one job
DESIGN.md §1k gives it — connect a fresh install to the user's server — and a
text field that accepts typing and changes nothing is exactly what §4 forbids.

`HTTPMailSource.origin` is now computed from `ServerSetting`, which the pairing
screen writes. A `NADE_BASE_URL` in the environment still outranks it, and the
field says so rather than pretending otherwise.

### D71. Changing the server has to throw the old server's mail away

The Keychain item is origin-bound, so a token could never cross. The **database
could**: it is one file, and `INBOX` and `SENT` are the same ids on every
account, so server A's threads would sit under server B's mailboxes until each
one happened to be refreshed. Two accounts' mail in one list is a data-isolation
failure, not a stale cache. `MailSync.pair(origin:…)` unpairs and empties the
store when the origin actually changes.

### D72. An empty answer is not a deletion

`refresh()` called `replaceMailboxes(boxes)` before noticing `boxes.isEmpty`,
and replacement cascades every join row and every cursor away. So the two
minutes during which the server legitimately answers `mailboxes: []` would
destroy the cache that A16's offline behaviour depends on — on a device that
already had mail, mid-sync, for no reason. An empty list now means "wait", not
"delete".

### D73. Consent finishing needs something to notice

`.needsGmail` stops polling, and D29 keeps the Mail screens resident — so
returning from Safari after a successful Gmail sign-in re-ran no `.task` and the
app sat on "Needs sign-in" until the next launch. A `scenePhase` observer at the
root refreshes on `.active`. It also gives the first-run poll a second chance:
the interval table is ~5 minutes and exhausting it is no longer terminal.

### D74. Every failure path handles recovery, not just the first one

`409 needs_reauth` and `401` were handled inside `refresh()` alone. Hit while
paging or opening a thread, they never wrote `account.status`, never surfaced
"Sign in again" and never cleared a revoked token — so whether the user could
recover depended on which request happened to fail first. One `handle(_:from:)`
now carries the transitions, and the poll loop uses it too.

The cache-over-failure promotion widened at the same time: a 502 or a rate limit
on a cold launch is no more evidence that a device is unpaired than a dead Wi-Fi
is, and only `isUnreachable` was being forgiven.

### D75. One error slot was wrong in both directions

A successful mailbox refresh cleared a *thread's* failure while the thread
stayed blank, and a thread's failure appeared on the mail list, which was
working. Problems are keyed by `ProblemKind` and cleared by the same operation
succeeding. A mail list shows the account-level problem first, then its own; a
thread shows its own first, then the account's — never another screen's.

### D76. A failed `ValueObservation` is over

GRDB does not resume one. Every `onError` discarded the error, so a fetch or
decode failure froze the screen on its last value with no caption — and the
token stayed set, so the guard in `start()` refused to build a replacement even
after good rows were written.

**Superseded by D84.** The first fix wrote the policy three times in three
shapes and left it out of the fourth observation entirely.

### D77. Paging was one flag for every mailbox

`isLoadingMore` was shared, so a list still paging in mailbox A could swallow
B's first load-more and never re-run it, because B's sentinel row had already
appeared. Keyed by mailbox.

### D78. The renderer does not edit the mail

`ThreadMessageBlock` trimmed every paragraph and dropped the empty ones, which
quietly rewrote `body_text`: indentation in pasted code, deliberate blank
stanzas, trailing whitespace. The parser on the other side wrote three fixes to
produce this text faithfully. Splitting on blank lines is now the only thing
done to it.

### D79. Two bands were short because they had no line box

1k's header and 1f's nav, meta and footer omitted `nadeLineHeight`, so they
rendered at the face's natural height rather than the DS's inherited 1.55 — 1k's
divider visibly higher than 1g's for an identical `62 / 22 / 12`. Both bands
were extracted into `SettingsHeader` and `ThreadNavBar` so the measurement is of
a render rather than of a constant, which is the only reason the shortfall was
provable at all.

### D80. The pop-gesture delegate has to outlive the screen that installed it

`UINavigationController` holds it weakly, and every destination was installing
its own — so popping the top screen deallocated the delegate the *next* screen's
gesture depended on, leaving the recogniser enabled with nothing guarding it at
the root. One shared `PopGestureGuard`. The UI test now pops twice and then
pushes again, because a test that stops after the first pop cannot see this.

An edge swipe *at* the root is deliberately not asserted: with nothing to pop,
what happens next belongs to iOS, and pinning it would be testing UIKit.

### D81. `Character.isNumber` is not "an ASCII digit"

It is true of Arabic-Indic digits, Devanagari digits, fullwidth numerals and a
long tail more — none of which `POST /auth/pair` accepts. Enabling Pair for them
sends a request that cannot succeed.

### D82. Recovery has to cover the migration

Delete-and-retry surrounded only the open, so a database that opened cleanly and
then failed to migrate stayed on disk and failed again on every launch, with the
error swallowed by a `try?` in the composition root. `openWriter(preparing:)`
runs the migration inside the retry, and `MailStore.openingOrEmpty` is the one
place that falls back to memory.

### D83. `@State` with a default is not "assembled once"

A `@State` default is a stored-property initialiser: it runs every time the view
struct is constructed and SwiftUI keeps only the first result. `WindowGroup`'s
content closure re-runs on scene changes, so `@State private var composition =
Composition.live()` would open a second `DatabasePool` on the same file, and a
third, each discarded but holding its descriptors. `CompositionRoot.shared`.


---

## P2 — the cleanup pass (2026-08-19)

Four parallel reviews — reuse, simplification, efficiency, altitude — over the
finished lane. The findings that changed the code rather than a comment:

### D84. The observation policy is one object, not a habit

D76 named the rule (*an observation that errored is over: drop every token so
`start()` can rebuild, and put a caption up*) and then implemented it three
times in three shapes — a method taking `Error`, an inline closure ignoring it,
a method taking nothing — with two of the three strings byte-identical. The
fourth observation, `SettingsModel`'s, had `onError: { _ in }`. The policy went
missing from one of four call sites **inside the lane that wrote it down**.

`StoreObservation` owns the tokens, the problem slot and the rule; a model
declares what it watches and the copy is a parameter. P3's notes, P5's feed and
P7's agents are all `ValueObservation`-backed lists, and each would have been a
fifth, sixth and seventh copy.

### D85. The prerequisite chain is named, not retried

`loadThreads` and `loadThread` each carried a one-shot retry for the same rule —
**mailboxes → list row → detail** — in two different shapes, with the
termination counter exposed as an `allowingRetry` parameter on both public
signatures. Every call site could see a flag it must never pass.

Worse, the retry ran `refresh()`, the whole state machine, to satisfy a paging
prerequisite; and with a `Retry-After` outstanding `refresh()` returns having
done nothing, so the recursion failed silently and the screen stayed empty.

`ensureMailboxes()` and `ensureListRow(for:in:)` are narrow and idempotent, and
any future cold entry point — P5's feed→thread jump, P6's push deep link — gets
them without knowing they exist.

### D86. `MailRoute.thread` carries its mailbox

It was `.thread(id:)`, and the screen got the rest by reaching around the route:
`mailboxID` from app-global `selectedMailboxID`, and `backTitle` computed *in
the parent* by scanning the mailbox list. Three costs: two threads opened from
different mailboxes hashed identically, so `NavigationStack` could not tell them
apart; naming a destination took two writes, and one of the two call sites of
the method that keeps them in sync bypassed it; and a route constructed from
outside the view tree — which is exactly what P5 and P6 do — would open against
whichever mailbox happened to be selected.

### D87. `@State private var model = Model(…)` is not "built once"

`MailTabRoot` declared four models that way, and `screen(for:)` runs inside
`ForEach(NTab.allCases)` — so every tab switch and every push re-ran the
initialiser four times and discarded the results, one of which read the Keychain
and the process environment on the way. The same trap as D83, one layer down.
They live on the composition now, with the lifetime they always had.

### D88. Three formatters were being rebuilt on hot paths

Measured rather than assumed. `DateFormatter` construction is **145×** a reuse
(55 µs against 0.4), and `ListTime` built one per call while `MailRow` called it
twice per row — once to render and once inside the accessibility label — so a
25-row screen spent ~2.8 ms per render compiling ICU patterns, against an 8.3 ms
budget at 120 Hz. Memoised by `(format, time zone)`, which keeps the injected
calendar in `ListTimeTests` working and survives a device changing zone.

`ThreadMessageBlock` re-split the whole message body on every `body` evaluation,
in a plain `VStack` — so every message in a thread paid it, not just the visible
ones. Split once, at construction. `ProcessInfo.environment` materialises the
whole environment per access to read one key that cannot change after launch.

Three things the same review checked and cleared, with numbers:
`WireTime.decoder()` (~0.15 µs, once per response), `ByteCountFormatter`
(0.07 µs), and the Keychain query dictionary (0.21 µs against an XPC round
trip). The cost there was *how often* the Keychain was reached — from
`SettingsView.body` — which is fixed separately.

### D89. Every wire string was typed twice

Each `WireEnum` carried a `rawValue` switch and a mirror-image
`init?(rawValue:)` switch: ~50 paired literals across four types, with nothing
asserting the pairs agreed. `ErrorCode` happened to be covered because
`WireDecodeTests` drives off the thirteen `error_*.json` fixtures; `RunStatus`
has eight cases and fixtures for four, so `queued`, `running`, `waiting` and
`skipped` had never had their two literals compared in either direction.

`init?(rawValue:)` is derived from `allKnown`, and `WireEnumTests` asserts every
known case round-trips and that no unknown value collides with one.

### D90. The offline mode was a parallel composition root

`-NADESeed offline` hand-built an `HTTPMailSource`, an `APIClient` and a test
double inside the shipping composition, bypassing `Composition.live()` entirely.
A16 — the criterion that proves the app degrades correctly — was proving a
hand-built lookalike degrades correctly. `live()` now takes its three inputs and
every DEBUG mode substitutes into it, so P3's device registration reaches the
offline world too.

### D91. Two coverage lists argued against themselves

`FixtureParityTests` re-listed all ten fixtures by hand immediately after
correctly iterating `FixtureSeed.names` — in the file whose own header argues a
manifest must be asserted rather than a directory looped. `WireDecodeTests`'
round-trip hand-listed ten `(type, fixture)` pairs, for the test whose stated
purpose is that *a dropped field is invisible until something needs it*. Driving
both off the directory found **twelve** shapes, not ten: `me_needs_reauth` and
`search_empty` had been silently exempt.

### D92. The fixture world's mapping is a table with a test

`FixtureMailSource.threads` special-cased two mailbox ids in a `switch`, and the
reason `Label_12` mattered — that `mailboxes.json` names it "To Reply" and both
thread details carry that `mailbox_name` — lived only in a comment. Asserting
the table found a real gap: `thread_html_only.json` is filed under
"Subscriptions", which no page backed, so the one fixture exercising an emoji
subject, a synthesised `body_text` and `to: []` had no list row and could not be
opened at all.

### D93. P3's screens, and the six things the review found underneath them

The Ask tab's whole lane — 2a feed ⇄ focus, 1a's three route states, 1b, 1c,
1d — plus tappable attachments and the locked "View original". The screens
themselves are `DESIGN.md` transcribed; what is worth writing down is what an
adversarial Codex review found *beneath* them, because five of the six were
invisible from the screen and none would have been caught by looking.

**The outbox was never drained.** `OutboxDriver.drain`'s own doc comment said it
ran "on app foreground, when the feed appears, and after a successful pair", and
the only caller was `enqueue`. A queue whose sole trigger is a new enqueue is not
a queue: kill the app between the durable write and the request — the exact
window the durable write exists for — and the approval sat there until the user
happened to tap another one. `MailSync.drainOutbox` now owns the three triggers
the comment always claimed. **A comment that describes behaviour is a claim, and
an unasserted claim rots.**

**A calendar date is not an instant.** `ends.date` is `"YYYY-MM-DD"` with no time
and no zone, and it was being parsed to a `Date` at UTC midnight, shown through a
local-zone formatter and written back. An existing `2027-01-06` displayed as
5 January in New York, and a date picked near local midnight in a positive offset
serialised as the day before — pausing an agent a day early. It is
`DateComponents` now, and the wire string is built with `String(format:)` rather
than a `DateFormatter`, so there is no instant anywhere on the path. The test
runs the round trip through four zones on both sides of UTC.

**Two writes derived from one read.** Every agent `PATCH` builds its payload from
the `agent` in hand, so two in flight at once both read the pre-edit object:
toggling two tools quickly sent two full `allowed_tools` arrays and the second,
built before the first returned, put the first tool back. Writes are chained
through one `Task` now. The general rule: **a read-modify-write over a remote
object needs serialising even when each half looks atomic.**

**Dismiss-then-save loses the user's work.** Both the sentence editor and the
schedule sheet closed before awaiting the write, and reopening re-seeds from the
unchanged agent — so an offline save silently discarded everything typed. They
close only once the write lands.

**A fixture that always succeeds teaches the wrong lesson.** Approve and skip
returned canned successes and left `feed.json` untouched, so the outbox's own
refetch restored the card it had just resolved, token and all: a consumed token
could be replayed forever, and the 409 path `API.md` §7 is built around was
unreachable. The fixture world now moves the card, spends the token, and answers
a replay with `token_consumed`. **A stub that cannot reach the failure state
cannot exercise the code that handles it.**

**Invisible text is not empty text.** `trimmingCharacters(in: .whitespacesAndNewlines)`
leaves U+200B, U+FEFF and the rest of Unicode's `Cf` category, so a paste of them
lit the ask field's submit button and produced a query nobody could see.
`String.nadeIsBlank` is the one definition the field, the navigation guard and
agent creation all share.

Two findings became **deviations rather than fixes**, because the contract is the
limit and not the code (`DESIGN.md` 50–56): 1a's citation rows cannot be tappable
while `API.md` §4 gives a source a `gmail_id` — a *message* id — where the thread
route needs a thread id, and 1c's Invocation radios cannot write while `PATCH`
accepts no trigger kind. Both would have been a 404 and a dead control
respectively; recording them beats shipping either.

### D94. The `Send` sweep was a coin flip, and the fix made it 19× faster

`testNothingInTheGallerySaysSend` is the C1/C2 guard — the one test standing
between v1 and a button that promises an outbound action. It pulled
`allElementsBoundByIndex` for buttons *and* static texts and read `.label` off
each, which is two cross-process round trips per element over a gallery with
hundreds. It ran for ~190 s and intermittently killed the app with "Lost
connection to the application", so every full-suite run was a coin flip on the
assertion that matters most.

One `NSPredicate` over `descendants(matching: .any)` is evaluated on the far side
of that boundary: the tree is walked once and only offenders come back. 190 s →
9.9 s, and strictly broader, because `.any` covers element types the two
hand-listed queries did not.

The predicate also carries its **own** self-check — five strings it must catch
and five it must not — because swapping a hand-rolled regex for an anchored ICU
`MATCHES` is exactly the change that quietly turns an assertion into a tautology.
A green test that cannot go red is worse than no test.

### D95. What an unbiased reviewer found that two adversarial ones did not

Two review passes had already run over P3 — a Codex pass and a self-review —
before a reviewer with **no context from either** read the same diff. It found
two defects that made whole screens unusable, and both were invisible to
anything the other passes could see.

**1a and 1c were rebuilt empty on every tab tap.** Their models were constructed
inside a `navigationDestination` / `fullScreenCover` content closure and held in
a plain `let`. Those closures re-run on any re-render of the parent, and
`RootTabView.body` reads `navigation.selection` — so *every tab tap* built a
fresh `AskModel` with no prose, no route and no task. `.id(query)` kept the view
identity stable, which meant `.task` did **not** re-fire and the new model was
never started: submit a question, tap Mail, tap Ask, and the answer was gone
with no way back but retyping. `@State(wrappedValue:)` evaluates once per view
identity, which is the lifetime a streaming session wants.

The lesson generalises past SwiftUI: **`let` is not ownership.** A reference
handed in from a closure lives as long as the closure re-runs, not as long as
the thing that reads it.

**1b's "New" was worse than inert.** It called `ask("")`, which `AppNavigation`
correctly refuses for an empty query, and then cleared the path anyway — so the
button's entire effect was to throw the user off 1b having created nothing.

Neither is reachable by reading a diff for correctness, which is what the other
two passes did well. Both are one tap deep. `NADEUITests/HomeUITests.swift`
exists because of them: **a screen with forty accessibility identifiers and no
tap-through is a screen nobody has used.**

The same pass also caught `toggleTool` still losing updates *after* D93 claimed
it was fixed — serialising the sends was not enough, because the payload was
derived before the `await`. A comment asserting a fix is not the fix.

### D96. The Send guard could not see the keyboard

`.submitLabel(.send)` on the ask field maps to `UIReturnKeyType.send`, which
renders the literal word **Send** on the keyboard — live on 2a's two fields and
1a's docked one. PLAN C1/C2 and DESIGN §4 are absolute that no UI string may
promise an outbound action, and the project's own guard
(`testNothingInTheGallerySaysSend`) asserts exactly that.

It missed this because the system keyboard is not in the app's element tree, and
because the gallery contains no `NAskField`. The guard was never wrong; its
*reach* was smaller than its name implied. `.go` carries no such promise.

Worth remembering when a rule is enforced by a test: **the test bounds what it
can see, not what the rule covers.**

### D97. A test that guarded data loss had a time bomb, and was never testing a tie

`sweep::tests::a_truncated_listing_never_sweeps_a_message_tied_with_its_floor`
is the regression test for one of the two CRITICAL findings the P3 backend
commit records: the reconciliation sweep deleting a live message whose timestamp
ties the listing's floor.

It inserted two rows at `Utc::now() - Duration::days(3)` and expected neither to
be swept. But the re-sync *fetches everything it lists* and writes the fixture's
`internalDate` over the cached row — a hard-coded `2026-08-16T09:12:04Z`. So the
listed message silently moved to that instant while the unlisted one kept
`now() - 3d`, and the two were never tied at all. Whether the test passed was a
function of which side of 09:12:04Z the clock was on: green until
`2026-08-19T09:12:04Z`, red after, and asserting nothing about ties in either
state. It went red partway through a session that had already run it green.

Both rows now take the fixture's own instant, so the tie is real and the result
does not depend on the day. **A test that mixes a wall clock with a fixed
fixture is not testing what its name says; it is scheduling a failure.**

## Liquid Glass (branch `liquid-glass`)

### D98. The chrome layer went to Liquid Glass, and the content layer did not

The ruling: **chrome only.** Bars, header bands, the ask field and the sheets
take Apple's material; cards, rows, tags, chips and the `.primary` / `.secondary`
/ `.ghost` buttons keep the Classical grammar — colour as a stroke, no filled
buttons, elevation a whisper. That split is Apple's own model (glass is a
functional layer *above* content) and it is the only reading under which D31 —
never an average of two components — survives a second design system arriving.

Three decisions are reversed, deliberately, and each is worth stating rather
than quietly overwriting:

* **D22** kept `TabView` out because it reaches none of the design's bar: not
  the 1 pt top hairline, not the 18 pt light-stroke glyphs, not the 10.5 pt
  uppercase `0.09em` labels. Still true. That type is the price of the system
  bar and it is paid knowingly (deviations 58–59). What is bought is a bar that
  minimizes on scroll-down, and a real `UITabBar` for VoiceOver rather than our
  reconstruction of one.
* **D26** made the bar a *sibling* of the scrolling band so "content ends at the
  hairline and never scrolls underneath". Liquid Glass lenses what passes
  beneath it; a bar with nothing under it is a bar with nothing to refract.
* **D1** pinned the deployment target at 18.0. Every Liquid Glass API is iOS 26,
  and the ruling was one code path rather than `#available` branches, so it is
  26.0 (deviation 64). D1's warning — this will not install below the target —
  is accepted, not overlooked.

### D99. `safeAreaBar` clips, and this app's hit targets are built on overflow

`safeAreaBar` is the iOS 26 API written for exactly this: it insets the safe
area, applies the glass, brings the scroll edge effect. It was the first thing
tried, and it **clips its content**.

D28 built the 44 pt hit target out of an overflowing `Color.clear` background
precisely so it could grow the target "without moving a pixel". Clipped, that
background contributes nothing. Measured on iOS 26.5, the same two controls in
1e's header:

| | inside `safeAreaBar` | inside `safeAreaInset` |
|---|---|---|
| `maillist.chip.INBOX` | 58.3 × **30.0** | 58.3 × **44.0** |
| `maillist.back` | 7.0 × **28.0** | 44.0 × **44.0** |

Those are the drawn boxes, exactly — the targets were gone, not merely reduced.
`MailUITests.testTheChipAndTheBackChevronAreBothAtLeast44Points` caught it.

The alternative was to pay for the target in layout — `.frame(minHeight: 44)` on
every control in a bar — which grows every header band and moves the pixels D28
went out of its way not to move. `safeAreaInset` costs nothing and keeps both
properties; it applies no material, so `nadeChromeBar()` carries the glass.

**A convenience API that composes three behaviours can be wrong for one of
them.** Taking the three apart cost one modifier and kept a requirement.

### D100. Tearing a tab down is not the same as hiding it, and one model appended

The old shell kept all four screens in the tree and toggled their opacity (D29),
so `.onDisappear` never fired on a tab switch and `.task` never re-fired.
`TabView` really does tear the outgoing tab down. Both halves of that pair now
run on every departure and every return.

Every `start()` in the app was already guarded by `!observation.isRunning` and
every `refresh()` was a re-fetch — except `AskModel`, whose only guard was
`task == nil` and whose `cancel()` clears `task`. So leaving the Ask tab and
coming back re-asked the same question and `apply(.token)` **appended** to prose
that was already complete: the answer rendered twice, back to back, with no
seam. `HomeUITests.testAnAnswerSurvivesLeavingAndReturningToTheTab` caught it —
the test written for the *opposite* failure, an answer that vanished.

`AskModel.hasFinished` is the fix: set on every terminal outcome and pointedly
**not** set when the task returns because it was cancelled. A finished session
never runs again; an interrupted one resets its accumulated state before
re-asking, so a partial answer is replaced rather than spliced.

**A guard that says "not already running" is not the same as one that says "not
already done."** The distinction only became visible when the shell started
tearing views down.

### D101. Two `.toolbar` calls do not compose

`.toolbar(.hidden, for: .navigationBar)` followed by
`.toolbar(.hidden, for: .tabBar)` compiles, hides the nav bar, and leaves the
tab bar on screen over 1f. The variadic
`.toolbar(.hidden, for: .navigationBar, .tabBar)` works.

`HiddenBars` exists so the choice is made *before* the call rather than inside
its argument, and so both stacks go through one place — including the Ask stack,
where `hidesTabBar` is always `false`. `MailRoute.hidesTabBar` is still the only
place the rule is written down, and it is still read off the **top route**,
never off stack depth (D51).

### D102. UIKit's tab bar takes no identifier, and one assertion had to go

`.accessibilityIdentifier` was applied to the `Tab`, and then to an explicit
`Label` inside it. Both compiled; both arrived at the rendered button as the
empty string on iOS 26.5. Seven test files addressed tabs as
`app.buttons["tab.<rawValue>"]`; they now go through `XCUIApplication.tab(_:)`,
which scopes the query to `tabBars` and matches the title VoiceOver speaks.

That is not a new coupling to visible copy — `TabBarUITests` asserts those four
labels directly, so the suite fails loudly on a title change rather than quietly
finding nothing. Scoping to `tabBars` also makes the negative assertion exact:
on 1f there is no tab bar to look in.

`testEachTabAnnouncesItsPosition` is gone. A `UITabBar` item exposes no `value`
through XCUITest — verified, empty for all four — because the position is
composed by VoiceOver from the bar rather than stored on the item. The
announcement is the platform's now and is better for it, and it is **not
observable from a UI test**, so the assertion was deleted rather than weakened.

Recorded rather than dropped silently: this is a real loss of coverage, and the
next person to wonder why a tab has no spoken position should find this instead
of re-running the experiment.

### D103. A look-at loop that does not rebuild is a look at the last build

Three consecutive rounds of "fix the bottom bar, screenshot, still broken" were
three screenshots of one binary. The ad-hoc shot script resolved
`BUILT_PRODUCTS_DIR` with `xcodebuild -showBuildSettings` and installed from it,
which succeeds whether or not the source has been compiled since.

The conclusions drawn in those three rounds were written into a source comment
before the mistake surfaced, and the comment was wrong in the specific way that
is hardest to catch later: it cited measurements that had never been taken. It
has been rewritten to claim only what was tested.

`scripts/screenshots.sh` and `scripts/bench.sh` both build first, which is why
neither has ever had this problem. **A scratch tool that skips the slow step
is not a faster version of the real one; it is a different tool that can lie.**

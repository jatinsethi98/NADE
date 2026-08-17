# iOS decisions and recorded deviations

Every place the SwiftUI build departs from `docs/DESIGN.md` / the Classical DS,
and why. Phase 1 ([ios] foundations). Additions go at the bottom of each
section, newest last.

---

## P1 — project configuration

### D1. Deployment target 26.5 → 18.0
The template shipped `IPHONEOS_DEPLOYMENT_TARGET = 26.5`, which would refuse to
install on any phone not on the newest iOS. Set to **18.0** in all six build
configurations (project Debug/Release + three targets). 18.0 still gives
`@Observable`, `safeAreaInset`, `AccessibilityTraits.isToggle`,
`ScrollViewReader` and everything else this app uses. Asserted by
`InfoPlistTests.testDeploymentTargetIs18` (reads `MinimumOSVersion` from the
*built* bundle, so a config that drifts fails the test rather than the App
Store).

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

### D9. `Hairline` is one **device pixel**; component strokes are one **point**
The mockup frame is 402 CSS px wide and the phone is 402 pt wide, so a literal
port of the DS `1px` rule would be 1 pt — three device pixels at @3x, which
reads as a heavy line beside the design's fine rules. Two weights, deliberately:

- **`Hairline` / `VHairline` = `1 / displayScale`** — full-bleed rules between
  rows, the tab bar's top divider, the segmented control's internal dividers.
  This is the iOS separator idiom and matches how the design's rules read.
- **`Theme.Stroke.border = 1 pt`** — component outlines (button, input, card,
  tag, chip, segmented container). In this design colour *is* a stroke — there
  are no filled buttons — so these carry the component's whole visual weight. A
  ⅓-pt outline would make an outlined button nearly vanish.

`displayScale` is read from the SwiftUI environment, not from the deprecated
`UIScreen.main.scale`, so it is correct per-window and testable.

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

### D17. `NButton` gained a `corner` and a `box` knob
DESIGN.md §1 lists four button variants. Two extra parameters, both driven by
the design rather than by taste:

- `corner: .rounded | .pill` — the DS `.btn` is radius 4, but the ask field's
  send button is a circle. Adding it now means P3 does not have to reimplement
  the button.
- `box:` on the icon initialiser — DS `.btn-icon` is 36 × 36, but the ask
  field's circle is 38 (2a/1f), 40 (1a) or 44 (2b) depending on the screen.

A glyph beside a label renders at the label's 14 pt; a glyph on its own renders
at the mockups' 17 pt.

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

### D21. `NTextField` draws its own placeholder
`prompt:` does not reliably honour a custom colour and face. The placeholder is
an overlay `Text` in `ink62` at the design's 14 pt, with
`.allowsHitTesting(false)` and `.accessibilityHidden(true)`, and the field
carries an explicit `.accessibilityLabel` so nothing is lost to VoiceOver.
`showsFocusRing` exists for the gallery/previews only — it draws the accent
focus border without stealing first responder.

### D22. `TabView` is not used
SwiftUI's `TabView` cannot be given the design's 1 px top hairline, its 18 pt
light-stroke glyphs, its 10.5 pt uppercase 0.09 em labels or its accent /
`ink62` pairing. `NTabBar` is a plain `HStack` of buttons over an `NTab` enum,
hosted by `RootTabView` through `.safeAreaInset(edge: .bottom)` so the bar
clears the home indicator and the screens inset above it. P2/P3/P7 replace the
placeholder screens in `RootTabView.screen(for:)`; nothing else in the shell
should need to change.

---

## P1 — testing

### D23. The no-raw-values lint reads the source tree via `#filePath`
`NoRawValuesTests` walks `<repo>/NADE/**/*.swift` and fails on `Color(hex:`,
`.font(.system(`, `Font.custom(`, `fixedSize:`, `Color(red:`, `Color(.sRGB`, a
`#rrggbb` literal, or a system palette colour — anywhere except `Theme.swift`.
Two supporting details:

- **Comments are stripped before matching.** Theme.swift's own documentation
  names the APIs the rules ban ("`Font.custom(_:fixedSize:)` is banned here"),
  and a design note is allowed to quote a hex value. Line numbers are preserved
  so a failure still points at the right line.
- **It skips, loudly, when the tree is absent** (an artefact-only CI run)
  rather than passing vacuously. A companion test asserts `Theme.swift` still
  *contains* the raw values, so the lint cannot become true by the tokens
  moving somewhere the scan cannot see.

Verified non-vacuous by planting a violating file and watching the test fail
with the right file and line.

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
| `p1-gallery-01-color.png` … `-10-edges.png` | one per gallery section, iPhone 17 Pro |
| `p1-se-shell.png`, `p1-gallery-se-buttons.png`, `p1-gallery-se-tabbar.png` | the same on **iPhone SE (3rd gen), 375 × 667** — narrowest and shortest device the 18.0 target supports (E2) |

On SE the four tab labels still fit without truncation, no row overflows, and
nothing clips. The scratch SE simulator is deleted after the run; the script
that produced these lives in the session scratchpad, not the repo (it is
three `simctl` calls in a loop and would rot as a checked-in artefact —
regenerate with `simctl launch … -NADEGallery 1 -NADEGallerySection <id>`).

Reading the screenshots back against the DS CSS and the mockup renders turned
up two real mismatches, both fixed before this was reported: the leading glyph
on a text button was rendering at the icon-only 17 pt instead of the label's
14 pt, and the ask field's send button was locked to 36 pt instead of the
screen's 38/40/44.

---

## Known gaps left for later phases

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
`docs/screens/p1-shell-safearea-fixed.png`. All 37 tests still green.

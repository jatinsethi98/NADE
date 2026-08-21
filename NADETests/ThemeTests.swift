//
//  ThemeTests.swift
//  NADETests
//
//  The tokens in DESIGN.md §1 are the design's own numbers. This asserts the
//  Swift side still holds them.
//

import SwiftUI
import UIKit
import XCTest
@testable import NADE

@MainActor
final class ThemeTests: XCTestCase {

    // MARK: Colour

    /// sRGB components of a token, so a hex typo cannot survive.
    private func rgba(_ color: Color) -> (r: Double, g: Double, b: Double, a: Double) {
        var r: CGFloat = 0, g: CGFloat = 0, b: CGFloat = 0, a: CGFloat = 0
        XCTAssertTrue(UIColor(color).getRed(&r, green: &g, blue: &b, alpha: &a))
        return (Double(r), Double(g), Double(b), Double(a))
    }

    private func assertColor(
        _ color: Color,
        _ hex: UInt32,
        alpha: Double = 1,
        _ name: String,
        file: StaticString = #filePath,
        line: UInt = #line
    ) {
        let got = rgba(color)
        let want = (
            r: Double((hex >> 16) & 0xff) / 255,
            g: Double((hex >> 8) & 0xff) / 255,
            b: Double(hex & 0xff) / 255
        )
        let tol = 1.0 / 512
        XCTAssertEqual(got.r, want.r, accuracy: tol, "\(name) red", file: file, line: line)
        XCTAssertEqual(got.g, want.g, accuracy: tol, "\(name) green", file: file, line: line)
        XCTAssertEqual(got.b, want.b, accuracy: tol, "\(name) blue", file: file, line: line)
        XCTAssertEqual(got.a, alpha, accuracy: tol, "\(name) alpha", file: file, line: line)
    }

    func testCoreRolesResolveToTheDesignValues() {
        assertColor(Theme.Color.bg, 0xf3f2f2, "bg")
        assertColor(Theme.Color.surface, 0xeae9e9, "surface")
        assertColor(Theme.Color.ink, 0x201f1d, "ink")
        assertColor(Theme.Color.accent, 0xb68235, "accent")
        assertColor(Theme.Color.accent2, 0xac803e, "accent2")
        // divider is `color-mix(in srgb, ink 16%, transparent)`
        assertColor(Theme.Color.divider, 0x201f1d, alpha: 0.16, "divider")
    }

    func testInkLadderIsTheDesignsOpacities() {
        let ladder: [(Color, Double, String)] = [
            (Theme.Color.ink50, 0.50, "ink50"),
            (Theme.Color.ink55, 0.55, "ink55"),
            (Theme.Color.ink60, 0.60, "ink60"),
            (Theme.Color.ink62, 0.62, "ink62"),
            (Theme.Color.ink68, 0.68, "ink68"),
            (Theme.Color.ink70, 0.70, "ink70"),
        ]
        for (color, alpha, name) in ladder {
            assertColor(color, 0x201f1d, alpha: alpha, name)
        }
    }

    /// Every step of every ramp, to the exact hex in
    /// `docs/MockUps/_ds/classical-*/styles.css`. Endpoints-only coverage left
    /// 200/300/400/600/700/800 free to be anything at all.
    func testEveryNeutralStepResolvesToTheDesignValue() {
        let ramp: [(Color, UInt32, String)] = [
            (Theme.Color.neutral100, 0xf8f4f4, "neutral100"),
            (Theme.Color.neutral200, 0xeae7e7, "neutral200"),
            (Theme.Color.neutral300, 0xd7d3d3, "neutral300"),
            (Theme.Color.neutral400, 0xbab6b6, "neutral400"),
            (Theme.Color.neutral500, 0x9b9797, "neutral500"),
            (Theme.Color.neutral600, 0x7d7979, "neutral600"),
            (Theme.Color.neutral700, 0x605d5d, "neutral700"),
            (Theme.Color.neutral800, 0x444141, "neutral800"),
            (Theme.Color.neutral900, 0x2d2b2b, "neutral900"),
        ]
        for (color, hex, name) in ramp { assertColor(color, hex, name) }
    }

    func testEveryAccentStepResolvesToTheDesignValue() {
        let ramp: [(Color, UInt32, String)] = [
            (Theme.Color.accent100, 0xfff3e4, "accent100"),
            (Theme.Color.accent200, 0xffe3bf, "accent200"),
            (Theme.Color.accent300, 0xfacb8d, "accent300"),
            (Theme.Color.accent400, 0xe1ad66, "accent400"),
            (Theme.Color.accent500, 0xc28d41, "accent500"),
            (Theme.Color.accent600, 0xa06f24, "accent600"),
            (Theme.Color.accent700, 0x7d5411, "accent700"),
            (Theme.Color.accent800, 0x5a3b0a, "accent800"),
            (Theme.Color.accent900, 0x3a270d, "accent900"),
        ]
        for (color, hex, name) in ramp { assertColor(color, hex, name) }
    }

    func testEveryAccent2StepResolvesToTheDesignValue() {
        let ramp: [(Color, UInt32, String)] = [
            (Theme.Color.accent2_100, 0xfff3e4, "accent2-100"),
            (Theme.Color.accent2_200, 0xffe3be, "accent2-200"),
            (Theme.Color.accent2_300, 0xf5cd96, "accent2-300"),
            (Theme.Color.accent2_400, 0xdbaf70, "accent2-400"),
            (Theme.Color.accent2_500, 0xbc8f4e, "accent2-500"),
            (Theme.Color.accent2_600, 0x9b7232, "accent2-600"),
            (Theme.Color.accent2_700, 0x79561f, "accent2-700"),
            (Theme.Color.accent2_800, 0x573d14, "accent2-800"),
            (Theme.Color.accent2_900, 0x382810, "accent2-900"),
        ]
        for (color, hex, name) in ramp { assertColor(color, hex, name) }
    }

    /// The two ramps share their 100 step and diverge from 200. The previous
    /// version of this test only asserted that two components differed, which
    /// both values being wrong also satisfies; every step now has an exact
    /// expectation above, so this asserts the *shape* those exact values make.
    func testAccentAndAccent2ShareTheir100StepAndDivergeAfterIt() {
        assertColor(Theme.Color.accent2_100, 0xfff3e4, "accent2-100 shares accent-100")
        for (a, b, name) in [
            (Theme.Color.accent200, Theme.Color.accent2_200, "200"),
            (Theme.Color.accent300, Theme.Color.accent2_300, "300"),
            (Theme.Color.accent400, Theme.Color.accent2_400, "400"),
            (Theme.Color.accent500, Theme.Color.accent2_500, "500"),
            (Theme.Color.accent600, Theme.Color.accent2_600, "600"),
            (Theme.Color.accent700, Theme.Color.accent2_700, "700"),
            (Theme.Color.accent800, Theme.Color.accent2_800, "800"),
            (Theme.Color.accent900, Theme.Color.accent2_900, "900"),
        ] {
            XCTAssertNotEqual(
                UIColor(a), UIColor(b),
                "accent-\(name) and accent-2-\(name) resolved to the same colour"
            )
        }
    }

    /// `.card-body { opacity: 0.8 }` — the one raw opacity that used to live in
    /// `NCard` rather than in the ladder.
    func testInkLadderCoversTheCardBodyOpacity() {
        assertColor(Theme.Color.ink80, 0x201f1d, alpha: 0.80, "ink80")
    }

    // MARK: Space, radius, stroke, shadow

    func testSpaceScaleIsExact() {
        XCTAssertEqual(Theme.Space.s1, 4.6)
        XCTAssertEqual(Theme.Space.s2, 9.2)
        XCTAssertEqual(Theme.Space.s3, 13.8)
        XCTAssertEqual(Theme.Space.s4, 18.4)
        XCTAssertEqual(Theme.Space.s6, 27.6)
        XCTAssertEqual(Theme.Space.s8, 36.8)
        XCTAssertEqual(Theme.Space.screen, 22)
        XCTAssertEqual(Theme.Space.screenDetail, 20)
    }

    func testRadiusScaleIsExact() {
        XCTAssertEqual(Theme.Radius.sm, 2)
        XCTAssertEqual(Theme.Radius.md, 4)
        XCTAssertEqual(Theme.Radius.lg, 7)
        XCTAssertEqual(Theme.Radius.pill, 999)
        XCTAssertEqual(Theme.Radius.tag, 3)   // calc(radius-md * 0.75)
    }

    func testStrokeWidths() {
        XCTAssertEqual(Theme.Stroke.border, 1)
        XCTAssertEqual(Theme.Stroke.radioRing, 1.5)
        XCTAssertEqual(Theme.Stroke.rule, 2)
    }

    func testShadowsMatchTheCSS() {
        // CSS blur is ~2× a SwiftUI shadow radius; the offsets are 1:1.
        XCTAssertEqual(Theme.Shadow.sm.y, 1)
        XCTAssertEqual(Theme.Shadow.sm.radius, 1)
        XCTAssertEqual(Theme.Shadow.md.y, 3)
        XCTAssertEqual(Theme.Shadow.md.radius, 5)
        XCTAssertEqual(Theme.Shadow.lg.y, 12)
        XCTAssertEqual(Theme.Shadow.lg.radius, 16)
        assertColor(Theme.Shadow.sm.color, 0x2d2b2b, alpha: 0.14, "shadow-sm")
        assertColor(Theme.Shadow.md.color, 0x2d2b2b, alpha: 0.16, "shadow-md")
        assertColor(Theme.Shadow.lg.color, 0x2d2b2b, alpha: 0.22, "shadow-lg")
    }

    // MARK: Type

    /// Every design size must land on a sensible Dynamic Type anchor, and the
    /// mapping must be monotonic — a bigger design size can never map to a
    /// smaller text style.
    func testTextStyleMappingCoversTheDesignScaleMonotonically() {
        let scale: [CGFloat] = [27, 26, 25, 24, 23, 22, 20, 19, 17, 16, 15, 14.5,
                                14, 13.5, 13, 12.5, 12, 11.5, 11, 10.5, 10, 9.5]
        let order: [Font.TextStyle] = [.caption2, .caption, .footnote, .subheadline,
                                       .callout, .body, .title3, .title2, .title, .largeTitle]

        var previousRank = Int.max
        for size in scale {
            let style = Theme.Font.textStyle(for: size)
            let index = order.firstIndex(of: style) ?? -1
            XCTAssertGreaterThanOrEqual(index, 0, "\(size) mapped to an unranked style")
            XCTAssertLessThanOrEqual(index, previousRank, "\(size) mapped upward — the mapping is not monotonic")
            previousRank = index
        }

        // Spot checks against the anchors named in Theme.Font.textStyle(for:).
        XCTAssertEqual(Theme.Font.textStyle(for: 27), .title)
        XCTAssertEqual(Theme.Font.textStyle(for: 23), .title2)
        XCTAssertEqual(Theme.Font.textStyle(for: 20), .title3)
        XCTAssertEqual(Theme.Font.textStyle(for: 17), .body)
        XCTAssertEqual(Theme.Font.textStyle(for: 15), .subheadline)
        XCTAssertEqual(Theme.Font.textStyle(for: 13), .footnote)
        XCTAssertEqual(Theme.Font.textStyle(for: 12), .caption)
        XCTAssertEqual(Theme.Font.textStyle(for: 10.5), .caption2)
    }

    /// EDGE (E1): the two families must actually be reachable from the API the
    /// app uses, and heading/body must not be the same face.
    func testHeadingAndBodyResolveToDifferentFaces() {
        XCTAssertNotEqual(Theme.Font.heading(15), Theme.Font.body(15))
        XCTAssertNotEqual(
            Theme.Font.heading(15, weight: .semibold),
            Theme.Font.heading(15, weight: .regular)
        )
        XCTAssertNotEqual(Theme.Font.body(15, weight: .regular), Theme.Font.body(15, weight: .semibold))
    }

    // MARK: Motion

    /// EDGE (E7)
    func testToggleAnimationRespectsReduceMotion() {
        XCTAssertNil(Theme.Motion.toggle(reduceMotion: true))
        XCTAssertNotNil(Theme.Motion.toggle(reduceMotion: false))
    }

    // MARK: Chrome ceiling

    /// EDGE (E1): chrome stops growing before it can break the layout, but the
    /// ceiling must still be an accessibility size, not a cop-out at `.xxxLarge`.
    func testChromeTypeCeilingIsAnAccessibilitySize() {
        XCTAssertTrue(Theme.Metrics.chromeTypeCeiling.isAccessibilitySize)
        XCTAssertEqual(Theme.Metrics.chromeTypeCeiling, .accessibility1)
    }

    // MARK: Tabs

    // `testTabBarMetricsMatchTheDesign` was here, pinning the seven numbers in
    // `Theme.Metrics.TabBar`. Both are gone with `NTabBar` (D98): the bar is a
    // native `TabView` now and its metrics are UIKit's, not this design's.
    // Asserting our own retired constants against nothing on screen is the
    // "test nobody has seen fail" that D39 is about.
    //
    // What survives is the part that is still ours — which four tabs, in which
    // order, with which glyphs.

    func testTabsAreTheFourInTheDesign() {
        XCTAssertEqual(NTab.allCases.map(\.title), ["Ask", "Mail", "Notes", "Calendar"])
        XCTAssertEqual(NTab.allCases.map(\.symbol), ["sparkles", "envelope", "doc.text", "calendar"])
        for tab in NTab.allCases {
            XCTAssertNotNil(
                UIImage(systemName: tab.symbol),
                "SF Symbol \"\(tab.symbol)\" does not exist on this OS"
            )
        }
    }

    // MARK: Line height and hit targets

    /// DS `body { line-height: 1.55 }`, `.btn` and `.card-title` 1.2.
    func testLineHeightTokensAreTheDSValues() {
        XCTAssertEqual(Theme.LineHeight.body, 1.55)
        XCTAssertEqual(Theme.LineHeight.button, 1.2)
        XCTAssertEqual(Theme.LineHeight.cardTitle, 1.2)
        XCTAssertEqual(Theme.LineHeight.heading, 1.12)
    }

    /// EDGE (E17). DESIGN.md never states a minimum touch target — it draws a
    /// 30 × 30 stepper box and a 16 pt radio — so this is the HIG's number, not
    /// the design's, and it is applied without changing a pixel.
    func testMinimumHitTargetIsTheHIGValue() {
        XCTAssertEqual(Theme.Metrics.minimumHitTarget, 44)
    }

    // MARK: Accessibility composition

    /// EDGE (E6). `NButton("")` used to copy the empty string straight into
    /// `.accessibilityLabel`, producing an interactive element VoiceOver could
    /// not name.
    func testNoButtonOrChipCanEndUpWithAnEmptyAccessibilityLabel() {
        XCTAssertEqual(NButton.resolvedAccessibilityLabel(title: "Approve", explicit: nil), "Approve")
        XCTAssertEqual(NButton.resolvedAccessibilityLabel(title: "Approve", explicit: "Save note"), "Save note")
        XCTAssertFalse(NButton.resolvedAccessibilityLabel(title: "", explicit: nil).isEmpty)
        XCTAssertFalse(NButton.resolvedAccessibilityLabel(title: nil, explicit: "").isEmpty)
        XCTAssertFalse(NButton.resolvedAccessibilityLabel(title: nil, explicit: nil).isEmpty)

        XCTAssertEqual(NChip.resolvedAccessibilityLabel(title: "Receipts", explicit: nil), "Receipts")
        XCTAssertFalse(NChip.resolvedAccessibilityLabel(title: "", explicit: nil).isEmpty)
        XCTAssertFalse(NChip.resolvedAccessibilityLabel(title: "", explicit: "").isEmpty)
    }

    /// EDGE (E6). 1d's three "Ends" rows differ only in their trailing value;
    /// the label carries the label and the hint, the value carries the rest.
    func testRadioRowSpokenLabelKeepsTheHint() {
        XCTAssertEqual(NRadioRow.spokenLabel(label: "Never", hint: nil), "Never")
        XCTAssertEqual(NRadioRow.spokenLabel(label: "Never", hint: ""), "Never")
        XCTAssertEqual(
            NRadioRow.spokenLabel(label: "On a schedule", hint: "Daily, weekly, or a custom repeat"),
            "On a schedule, Daily, weekly, or a custom repeat"
        )
    }

    /// EDGE (E6). VoiceOver's own tab bars say "Tab 1 of 4", and `NTabBar` had
    /// to synthesise that string because SwiftUI does not derive it for four
    /// loose buttons. A native `TabView` does derive it, so the string is no
    /// longer ours to build or to assert — `TabBarUITests` checks what is
    /// actually spoken instead, which is the stronger evidence anyway.
    ///
    /// The count stays: "of 4" is only right while there are four.
    func testThereAreFourTabs() {
        XCTAssertEqual(NTab.allCases.count, 4)
    }

    // MARK: Component geometry that the design pins exactly

    func testToggleGeometryMatchesTheDesign() {
        XCTAssertEqual(NToggle.trackWidth, 46)
        XCTAssertEqual(NToggle.trackHeight, 26)
        XCTAssertEqual(NToggle.knobDiameter, 20)
        // The mockup's `left: 2px` is measured inside the 1 pt border.
        XCTAssertEqual(NToggle.knobInset, Theme.Stroke.border + 2)
        // Off x = 2 and on x = 22 inside the border ⇒ symmetric 2 pt gaps.
        let insideBorder = NToggle.trackWidth - 2 * Theme.Stroke.border
        XCTAssertEqual(insideBorder - 22 - NToggle.knobDiameter, 2)
    }

    func testStepperAndTagAndChipGeometry() {
        XCTAssertEqual(NStepper.box, 30)
        XCTAssertEqual(NStepper.countMinWidth, 22)
        XCTAssertEqual(NStepper.countSize, 15)
        // DESIGN.md §3 1d: the row is `align-items: center; gap: 12`.
        XCTAssertEqual(NStepper.gap, 12)

        XCTAssertEqual(NTag.fontSize, 11)
        XCTAssertEqual(NTag.paddingV, 3)
        XCTAssertEqual(NTag.paddingH, 10)

        XCTAssertEqual(NChip.fontSize, 12.5)
        XCTAssertEqual(NChip.paddingV, 5)
        XCTAssertEqual(NChip.paddingH, 12)

        XCTAssertEqual(NButton.fontSize, 14)
        XCTAssertEqual(NButton.iconBox, 36)
        XCTAssertEqual(NButton.disabledOpacity, 0.45)
        XCTAssertEqual(NButton.labelGap, 6)
    }

    /// DESIGN.md §2's ask-field table, one preset per row. Rendered heights are
    /// in `ComponentGeometryTests`; this is the table itself.
    func testEveryAskFieldPresetIsItsScreensNumbers() {
        let expected: [(String, NTextField.Metrics, CGFloat, CGFloat, CGFloat, CGFloat, NTextField.Border)] = [
            ("DS .input", .input, 14, 6, 10, 36, .divider),
            ("2a feed", .askPinned, 13.5, 8, 15, 38, .divider),
            ("1a docked", .askDocked, 14, 9, 15, 40, .divider),
            // The one row of DESIGN.md §2's table whose Border column is not
            // "divider" — the mockup gives 2b's field its accent at rest.
            ("2a focus / 2b", .askCentred, 14, 10, 16, 44, .accent),
            ("1f thread", .askThread, 13.5, 10, 15, 38, .divider),
            ("1h search", .searchPill, 13.5, 8, 14, 38, .divider),
        ]
        for (name, m, font, padV, padH, minH, border) in expected {
            XCTAssertEqual(m.fontSize, font, "\(name) font size")
            XCTAssertEqual(m.paddingV, padV, "\(name) vertical padding")
            XCTAssertEqual(m.paddingH, padH, "\(name) horizontal padding")
            XCTAssertEqual(m.minHeight, minH, "\(name) min-height")
            XCTAssertEqual(m.border, border, "\(name) resting border")
        }
        XCTAssertEqual(
            expected.filter { $0.6 == .accent }.count, 1,
            "exactly one preset is gold before it is focused"
        )
        XCTAssertEqual(NTextField.Metrics.input.shape, .rounded)
        for m in [NTextField.Metrics.askPinned, .askDocked, .askCentred, .askThread, .searchPill] {
            XCTAssertEqual(m.shape, .pill)
        }
        // No two ask variants may collapse into one another.
        XCTAssertEqual(Set(expected.map { "\($0.1.fontSize)/\($0.1.paddingV)/\($0.1.paddingH)" }).count, 6)
    }

    /// DESIGN.md §1 Components — Segmented: DS `7 / 12` @13, 1c `6 / 14` @13,
    /// 1i `5 / 12` @12.
    func testSegmentedMetricsCoverTheDSAndBothOverrides() {
        XCTAssertEqual(NSegmentedMetrics.ds, NSegmentedMetrics(fontSize: 13, paddingV: 7, paddingH: 12))
        XCTAssertEqual(NSegmentedMetrics.status, NSegmentedMetrics(fontSize: 13, paddingV: 6, paddingH: 14))
        XCTAssertEqual(NSegmentedMetrics.noteMode, NSegmentedMetrics(fontSize: 12, paddingV: 5, paddingH: 12))
    }

    /// DESIGN.md §3 1c and 1d. One set of numbers cannot serve both screens.
    func testRadioRowMetricsAreTheTwoDesignRows() {
        XCTAssertEqual(
            NRadioRow.Metrics.invocation,
            NRadioRow.Metrics(labelSize: 15, hintSize: 12, valueSize: 13, gap: 11, paddingV: 11, alignsToFirstBaseline: true)
        )
        XCTAssertEqual(
            NRadioRow.Metrics.ends,
            NRadioRow.Metrics(labelSize: 14, hintSize: 12, valueSize: 13, gap: 11, paddingV: 9, alignsToFirstBaseline: false)
        )
    }
}

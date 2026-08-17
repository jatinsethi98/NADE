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

    func testRampEndpointsResolveToTheDesignValues() {
        assertColor(Theme.Color.neutral100, 0xf8f4f4, "neutral100")
        assertColor(Theme.Color.neutral500, 0x9b9797, "neutral500")
        assertColor(Theme.Color.neutral900, 0x2d2b2b, "neutral900")

        assertColor(Theme.Color.accent100, 0xfff3e4, "accent100")
        assertColor(Theme.Color.accent500, 0xc28d41, "accent500")
        assertColor(Theme.Color.accent800, 0x5a3b0a, "accent800")
        assertColor(Theme.Color.accent900, 0x3a270d, "accent900")

        assertColor(Theme.Color.accent2_100, 0xfff3e4, "accent2-100")
        assertColor(Theme.Color.accent2_500, 0xbc8f4e, "accent2-500")
        assertColor(Theme.Color.accent2_900, 0x382810, "accent2-900")
    }

    /// The two ramps share their 100 step but diverge from 200 — an easy place
    /// for a copy-paste to go unnoticed.
    func testAccentAndAccent2RampsAreDistinct() {
        XCTAssertNotEqual(rgba(Theme.Color.accent200).b, rgba(Theme.Color.accent2_200).b)
        XCTAssertNotEqual(rgba(Theme.Color.accent700).r, rgba(Theme.Color.accent2_700).r)
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

    // MARK: Tab bar geometry

    func testTabBarMetricsMatchTheDesign() {
        XCTAssertEqual(Theme.Metrics.TabBar.iconSize, 18)
        XCTAssertEqual(Theme.Metrics.TabBar.labelSize, 10.5)
        XCTAssertEqual(Theme.Metrics.TabBar.labelTracking, 0.09)
        XCTAssertEqual(Theme.Metrics.TabBar.iconLabelGap, 5)
        XCTAssertEqual(Theme.Metrics.TabBar.paddingTop, 9)
        XCTAssertEqual(Theme.Metrics.TabBar.paddingHorizontal, 10)
        // The mockup's `padding: 9px 10px 26px`, verbatim. The 26 is measured
        // from the display edge, so the bar extends under the home indicator
        // rather than sitting above it — DESIGN.md §Safe area.
        XCTAssertEqual(Theme.Metrics.TabBar.paddingBottom, 26)
    }

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

        XCTAssertEqual(NTextField.fontSize, 14)
        XCTAssertEqual(NSegmented<String>.fontSize, 13)
        XCTAssertEqual(NSegmented<String>.paddingV, 7)
        XCTAssertEqual(NSegmented<String>.paddingH, 12)
    }
}

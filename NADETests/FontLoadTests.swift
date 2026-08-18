//
//  FontLoadTests.swift
//  NADETests
//
//  EDGE (E8) / (E13): a missing, mis-named or unregistered TTF must fail here,
//  loudly. The failure mode we are guarding against is the silent one — the app
//  quietly rendering in the system face and nobody noticing until a screenshot
//  review months later.
//
//  Registration is necessary but not sufficient. `UIFont(name:)` returning
//  non-nil only proves the face is *installed*; it says nothing about whether
//  `Theme.Font.heading` / `Theme.Font.body` actually reach it. Both wrappers
//  could be swapped for `Font.system` and every registration test would stay
//  green. `RenderedFaceTests` (below) closes that by measuring real rendered
//  text and comparing it with what Core Text lays out in the expected face.
//

import CoreGraphics
import CoreText
import SwiftUI
import UIKit
import XCTest
@testable import NADE

final class FontLoadTests: XCTestCase {

    /// PostScript name → the family name prefix the face must report.
    private static let expected: [(psName: String, familyPrefix: String, file: String)] = [
        ("Lora-Regular", "Lora", "Lora-Regular.ttf"),
        ("Lora-SemiBold", "Lora", "Lora-SemiBold.ttf"),
        ("CormorantGaramond-Regular", "Cormorant Garamond", "CormorantGaramond-Regular.ttf"),
        ("CormorantGaramond-SemiBold", "Cormorant Garamond", "CormorantGaramond-SemiBold.ttf"),
    ]

    // MARK: The four faces resolve

    func testAllFourPostScriptNamesResolve() {
        for expected in Self.expected {
            let font = UIFont(name: expected.psName, size: 17)
            XCTAssertNotNil(
                font,
                """
                Font "\(expected.psName)" did not resolve. Either \
                NADE/Resources/Fonts/\(expected.file) is missing from the bundle, \
                or it is not listed in UIAppFonts (NADE/Info.plist), or the TTF's \
                PostScript name is not "\(expected.psName)".
                """
            )
            XCTAssertEqual(font?.fontName, expected.psName)
        }
    }

    func testResolvedFamiliesAreTheDesignFamilies() {
        for expected in Self.expected {
            guard let font = UIFont(name: expected.psName, size: 17) else {
                XCTFail("\(expected.psName) did not resolve"); continue
            }
            XCTAssertTrue(
                font.familyName.hasPrefix(expected.familyPrefix),
                "\(expected.psName) reported family \"\(font.familyName)\", expected a \"\(expected.familyPrefix)\" family"
            )
        }
    }

    /// The point of the whole suite: prove it is NOT the system face.
    func testResolvedFacesAreNotTheSystemFallback() {
        let system = UIFont.systemFont(ofSize: 17)
        for expected in Self.expected {
            guard let font = UIFont(name: expected.psName, size: 17) else {
                XCTFail("\(expected.psName) did not resolve"); continue
            }
            XCTAssertNotEqual(
                font.familyName, system.familyName,
                "\(expected.psName) fell back to the system face"
            )
            XCTAssertFalse(
                font.familyName.hasPrefix("."),
                "\(expected.psName) resolved to a private system face (\(font.familyName))"
            )
            XCTAssertFalse(
                font.fontName.hasPrefix(".") || font.fontName.contains("SFUI") || font.fontName.contains("SFPro"),
                "\(expected.psName) resolved to an SF face (\(font.fontName))"
            )
        }
    }

    /// `UIAppFonts` registration is what puts the families into the global list.
    /// If the plist key were wrong, `UIFont(name:)` might still work in a
    /// preview but would fail on device — this catches that.
    func testFamiliesAreRegisteredWithTheSystem() {
        let families = UIFont.familyNames
        XCTAssertTrue(
            families.contains { $0.hasPrefix("Lora") },
            "No Lora family registered. UIFont.familyNames = \(families.filter { !$0.hasPrefix(".") })"
        )
        XCTAssertTrue(
            families.contains { $0.hasPrefix("Cormorant Garamond") },
            "No Cormorant Garamond family registered."
        )
    }

    // MARK: The bundle and the plist agree

    func testUIAppFontsMatchesBundledFiles() throws {
        let declared = try XCTUnwrap(
            Bundle.main.object(forInfoDictionaryKey: "UIAppFonts") as? [String],
            "UIAppFonts is missing from the built Info.plist"
        )
        XCTAssertEqual(Set(declared), Set(Self.expected.map(\.file)))

        for file in declared {
            let name = (file as NSString).deletingPathExtension
            let ext = (file as NSString).pathExtension
            XCTAssertNotNil(
                Bundle.main.url(forResource: name, withExtension: ext),
                "UIAppFonts declares \(file) but it is not in the app bundle"
            )
        }
    }

    /// Reads the PostScript name straight out of each TTF, so a re-cut font
    /// whose internal name drifted from its filename fails here rather than
    /// silently at runtime.
    func testEachFileDeclaresItsExpectedPostScriptName() throws {
        for expected in Self.expected {
            let name = (expected.file as NSString).deletingPathExtension
            let url = try XCTUnwrap(
                Bundle.main.url(forResource: name, withExtension: "ttf"),
                "\(expected.file) is not in the app bundle"
            )
            let provider = try XCTUnwrap(CGDataProvider(url: url as CFURL))
            let cgFont = try XCTUnwrap(CGFont(provider), "\(expected.file) is not a readable font")
            XCTAssertEqual(
                cgFont.postScriptName as String?, expected.psName,
                "\(expected.file) declares a different PostScript name"
            )
        }
    }

    /// Both families must ship the **monospaced-numbers selector**, not merely
    /// a number-spacing feature type. `kNumberSpacingType` (6) is also the type
    /// that carries *proportional* numbers; a face offering only selector 1
    /// would satisfy "the type exists" while `.monospacedDigit()` did nothing.
    func testBothFamiliesOfferTheMonospacedNumbersSelector() throws {
        // <CoreText/SFNTLayoutTypes.h>: kNumberSpacingType = 6,
        // kMonospacedNumbersSelector = 0.
        let numberSpacingType = 6
        let monospacedNumbersSelector = 0

        for expected in Self.expected {
            let font = try XCTUnwrap(UIFont(name: expected.psName, size: 17))
            let features = CTFontCopyFeatures(font as CTFont) as? [[String: Any]] ?? []

            let numberSpacing = features.first { feature in
                (feature[kCTFontFeatureTypeIdentifierKey as String] as? Int) == numberSpacingType
            }
            let spacing = try XCTUnwrap(
                numberSpacing,
                "\(expected.psName) has no number-spacing feature (type \(numberSpacingType))"
            )

            let selectors = spacing[kCTFontFeatureTypeSelectorsKey as String] as? [[String: Any]] ?? []
            let identifiers = selectors.compactMap {
                $0[kCTFontFeatureSelectorIdentifierKey as String] as? Int
            }
            XCTAssertTrue(
                identifiers.contains(monospacedNumbersSelector),
                """
                \(expected.psName) exposes number-spacing selectors \(identifiers) but not \
                \(monospacedNumbersSelector) (monospaced) — .tabularNumerals() would be a no-op.
                """
            )
        }
    }

    // MARK: Theme uses exactly these names

    func testThemeReferencesTheBundledNames() {
        XCTAssertEqual(
            Set(Theme.Font.PostScriptName.all),
            Set(Self.expected.map(\.psName))
        )
    }
}

// MARK: - What actually renders

/// Registration proves the face is installed. These prove `Theme.Font` reaches
/// it: each one renders a real `Text` through the Theme API and compares the
/// size SwiftUI resolves with the size Core Text lays out in the expected face —
/// and separately with the size the **system** face would produce.
///
/// Swap `Theme.Font.heading`/`body` for `Font.system` and both fail.
@MainActor
final class RenderedFaceTests: XCTestCase {

    /// Wide letters and a few numerals: Cormorant, Lora and SF separate by
    /// 7–15 pt of width on this string at 17 pt, and by 1.4–1.7 pt of height.
    private static let sample = "MWmw@#%&Wjgy 0123"
    private static let size: CGFloat = 17

    private struct Case {
        let name: String
        let font: SwiftUI.Font
        let psName: String
    }

    private static let cases: [Case] = [
        Case(name: "heading semibold", font: Theme.Font.heading(size), psName: Theme.Font.PostScriptName.headingSemibold),
        Case(name: "heading regular", font: Theme.Font.heading(size, weight: .regular), psName: Theme.Font.PostScriptName.headingRegular),
        Case(name: "body regular", font: Theme.Font.body(size), psName: Theme.Font.PostScriptName.bodyRegular),
        Case(name: "body semibold", font: Theme.Font.body(size, weight: .semibold), psName: Theme.Font.PostScriptName.bodySemibold),
    ]

    private func rendered(_ font: SwiftUI.Font) -> CGSize {
        RenderMeasure.size(of: Text(Self.sample).font(font).lineLimit(1))
    }

    func testEveryThemeFontRendersInItsBundledFace() throws {
        for c in Self.cases {
            let face = try XCTUnwrap(UIFont(name: c.psName, size: Self.size), "\(c.psName) missing")
            let got = rendered(c.font)

            // Two roundings onto the device pixel grid: ⅔ pt at @3x, 1 pt on
            // the @2x SE. The faces are 15–20 pt apart on this sample, so this
            // cannot swallow the wrong one.
            let grid = 2 * RenderMeasure.snap
            XCTAssertMeasures(
                got.width, RenderMeasure.typesetWidth(Self.sample, font: face),
                accuracy: grid,
                "Theme.Font.\(c.name) rendered width (expected \(c.psName))"
            )
            XCTAssertMeasures(
                got.height, face.lineHeight, accuracy: grid,
                "Theme.Font.\(c.name) rendered line height (expected \(c.psName))"
            )
        }
    }

    /// The regression this suite exists for: someone replaces the wrapper's
    /// body with `.system(size:)` and the registration tests stay green.
    func testNoThemeFontRendersInTheSystemFace() {
        let system = UIFont.systemFont(ofSize: Self.size)
        let systemSize = CGSize(
            width: RenderMeasure.typesetWidth(Self.sample, font: system),
            height: system.lineHeight
        )

        for c in Self.cases {
            let got = rendered(c.font)
            let widthGap = abs(got.width - systemSize.width)
            let heightGap = abs(got.height - systemSize.height)
            XCTAssertTrue(
                widthGap > 1 || heightGap > 1,
                """
                Theme.Font.\(c.name) rendered \(got), which is what the *system* face \
                renders (\(systemSize)). It is not going through \(c.psName).
                """
            )
        }
    }

    /// Cormorant and Lora must not be the same face — a copy-paste in
    /// `Theme.Font` that pointed both at Lora would otherwise be invisible.
    func testHeadingAndBodyRenderDifferently() {
        let heading = rendered(Theme.Font.heading(Self.size))
        let body = rendered(Theme.Font.body(Self.size))
        XCTAssertGreaterThan(
            abs(heading.width - body.width), 5,
            "heading \(heading) and body \(body) render the same width — both are resolving to one face"
        )
    }

    /// Every Theme style is `relativeTo:`, so it must actually grow with
    /// Dynamic Type. A `Font.custom(_:fixedSize:)` regression freezes it.
    func testThemeFontsGrowWithDynamicType() {
        for c in Self.cases {
            let atLarge = RenderMeasure.size(
                of: Text(Self.sample).font(c.font).lineLimit(1),
                dynamicTypeSize: .large
            )
            let atAX5 = RenderMeasure.size(
                of: Text(Self.sample).font(c.font).lineLimit(1),
                dynamicTypeSize: .accessibility5
            )
            XCTAssertGreaterThan(
                atAX5.width, atLarge.width * 1.5,
                "Theme.Font.\(c.name) barely grew from .large \(atLarge) to AX5 \(atAX5) — it is not dynamic"
            )
        }
    }
}

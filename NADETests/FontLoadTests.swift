//
//  FontLoadTests.swift
//  NADETests
//
//  EDGE (E8) / (E13): a missing, mis-named or unregistered TTF must fail here,
//  loudly. The failure mode we are guarding against is the silent one — the app
//  quietly rendering in the system face and nobody noticing until a screenshot
//  review months later.
//

import CoreGraphics
import CoreText
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

    /// Both families ship a `tnum` feature, which is what makes
    /// `.tabularNumerals()` a real substitution rather than a no-op.
    func testBothFamiliesSupportTabularFigures() throws {
        for expected in Self.expected {
            let font = try XCTUnwrap(UIFont(name: expected.psName, size: 17))
            let ctFont = font as CTFont
            let features = CTFontCopyFeatures(ctFont) as? [[String: Any]] ?? []
            let hasNumberSpacing = features.contains { feature in
                (feature[kCTFontFeatureTypeIdentifierKey as String] as? Int) == 6  // kNumberSpacingType
            }
            XCTAssertTrue(
                hasNumberSpacing,
                "\(expected.psName) has no number-spacing feature — .tabularNumerals() would be a no-op"
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

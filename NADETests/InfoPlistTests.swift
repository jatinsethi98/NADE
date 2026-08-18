//
//  InfoPlistTests.swift
//  NADETests
//
//  The app's Info.plist is hand-written and merged with Xcode's generated keys
//  (GENERATE_INFOPLIST_FILE stays YES). These assert the merge actually
//  happened — a mis-set INFOPLIST_FILE would otherwise silently drop the fonts
//  and the ATS exception.
//

import XCTest

final class InfoPlistTests: XCTestCase {

    func testGeneratedKeysAreStillMergedIn() throws {
        // Proves the hand-written plist did not *replace* the generated one.
        XCTAssertEqual(Bundle.main.bundleIdentifier, "fsaas.NADE")
        XCTAssertNotNil(Bundle.main.object(forInfoDictionaryKey: "CFBundleName"))
        XCTAssertNotNil(Bundle.main.object(forInfoDictionaryKey: "UIApplicationSceneManifest"))
        XCTAssertNotNil(Bundle.main.object(forInfoDictionaryKey: "UILaunchScreen"))
    }

    func testHandWrittenKeysSurvivedTheMerge() throws {
        let fonts = try XCTUnwrap(
            Bundle.main.object(forInfoDictionaryKey: "UIAppFonts") as? [String]
        )
        XCTAssertEqual(fonts.count, 4)
    }

    /// The dev backend is plain HTTP on the LAN. `NSAllowsLocalNetworking`
    /// permits local and link-local hosts only — it does not open ATS up to the
    /// public internet, which `NSAllowsArbitraryLoads` would.
    func testLocalNetworkingIsAllowedAndArbitraryLoadsAreNot() throws {
        let ats = try XCTUnwrap(
            Bundle.main.object(forInfoDictionaryKey: "NSAppTransportSecurity") as? [String: Any],
            "NSAppTransportSecurity is missing"
        )
        XCTAssertEqual(ats["NSAllowsLocalNetworking"] as? Bool, true)
        XCTAssertNil(ats["NSAllowsArbitraryLoads"], "ATS must not be disabled wholesale")
    }

    /// PLAN.md §iOS app pins the deployment target; 26.5 would refuse to
    /// install on any phone that is not on the newest iOS.
    ///
    /// This only sees the configuration the tests were built in — Debug.
    /// `testEveryBuildConfigurationTargets18` is the one that covers Release.
    func testTheBuiltConfigurationTargets18() throws {
        let minimum = try XCTUnwrap(
            Bundle.main.object(forInfoDictionaryKey: "MinimumOSVersion") as? String
        )
        XCTAssertEqual(minimum, "18.0")
    }

    /// `MinimumOSVersion` from `Bundle.main` is whatever configuration happened
    /// to be built. Release could drift to 26.5 — the exact bug D1 records —
    /// and every test would stay green because tests never run against Release.
    ///
    /// So this reads the project file itself and requires **every**
    /// `IPHONEOS_DEPLOYMENT_TARGET` in it to be 18.0: project Debug/Release
    /// plus three targets × two configurations.
    func testEveryBuildConfigurationTargets18() throws {
        let repoRoot = URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent()
            .deletingLastPathComponent()
        let pbxproj = repoRoot.appendingPathComponent("NADE.xcodeproj/project.pbxproj")
        guard FileManager.default.fileExists(atPath: pbxproj.path) else {
            throw XCTSkip("NADE.xcodeproj is not next to \(#filePath) — skipping the configuration sweep")
        }

        let text = try String(contentsOf: pbxproj, encoding: .utf8)
        let regex = try NSRegularExpression(pattern: #"IPHONEOS_DEPLOYMENT_TARGET\s*=\s*([^;]+);"#)
        let matches = regex.matches(in: text, range: NSRange(text.startIndex..., in: text))

        XCTAssertGreaterThanOrEqual(
            matches.count, 6,
            "found only \(matches.count) IPHONEOS_DEPLOYMENT_TARGET settings — expected at least 6 (project + 3 targets, Debug and Release)"
        )

        var values: [String] = []
        for match in matches {
            guard let r = Range(match.range(at: 1), in: text) else { continue }
            values.append(String(text[r]).trimmingCharacters(in: .whitespaces))
        }
        XCTAssertEqual(
            Set(values), ["18.0"],
            "IPHONEOS_DEPLOYMENT_TARGET is not 18.0 in every configuration: \(values)"
        )
    }
}

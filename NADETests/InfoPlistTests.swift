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
    /// So this reads the project file itself. A test runs inside the simulator
    /// and cannot shell out to `xcodebuild -showBuildSettings`, so the literal
    /// values in `project.pbxproj` are the only view of Release available from
    /// here; `testNoConfigurationDefersToAnXcconfig` is what makes those
    /// literals also the *effective* values.
    ///
    /// **The count is derived, not guessed.** Every `XCConfigurationList` in an
    /// Xcode project holds one `XCBuildConfiguration` per configuration, and
    /// this project has two (Debug, Release). Requiring
    /// `settings == lists × 2` means a new target cannot arrive without
    /// bringing its own pin, and an existing one cannot quietly drop it. The
    /// previous floor of "at least 6" was arithmetically wrong — the project
    /// plus three targets is four lists, so **eight** settings — and two of
    /// them could be deleted with the test still green.
    func testEveryBuildConfigurationTargets18() throws {
        let text = try projectFile()

        let lists = try NSRegularExpression(pattern: #"isa = XCConfigurationList;"#)
            .numberOfMatches(in: text, range: NSRange(text.startIndex..., in: text))
        XCTAssertGreaterThanOrEqual(lists, 4, "expected a configuration list for the project and each of the three targets")

        let regex = try NSRegularExpression(pattern: #"IPHONEOS_DEPLOYMENT_TARGET\s*=\s*([^;]+);"#)
        let matches = regex.matches(in: text, range: NSRange(text.startIndex..., in: text))

        XCTAssertEqual(
            matches.count, lists * 2,
            """
            \(matches.count) IPHONEOS_DEPLOYMENT_TARGET settings for \(lists) configuration lists — \
            expected \(lists * 2) (Debug and Release for the project and each target). \
            A configuration with no pin inherits, which is exactly how Release drifts unnoticed.
            """
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

    /// The sweep above reads the values written *in* the project file. An
    /// `.xcconfig` attached to any configuration would override them and the
    /// sweep would never know, so the honest way to make the literals the
    /// effective settings is to require that no configuration has one.
    ///
    /// (Still not claimed: `xcodebuild -xcconfig …` passed on the command line.
    /// Nothing readable from inside the simulator can see that.)
    func testNoConfigurationDefersToAnXcconfig() throws {
        let text = try projectFile()
        let references = try NSRegularExpression(pattern: #"baseConfigurationReference"#)
            .numberOfMatches(in: text, range: NSRange(text.startIndex..., in: text))
        XCTAssertEqual(
            references, 0,
            """
            \(references) build configuration(s) point at an .xcconfig. The deployment-target sweep \
            reads the project file's own values and cannot see through one.
            """
        )
    }

    /// EDGE (E16): an artefact-only run has no source tree.
    private func projectFile() throws -> String {
        let repoRoot = URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent()
            .deletingLastPathComponent()
        let pbxproj = repoRoot.appendingPathComponent("NADE.xcodeproj/project.pbxproj")
        guard FileManager.default.fileExists(atPath: pbxproj.path) else {
            throw XCTSkip("NADE.xcodeproj is not next to \(#filePath) — skipping the configuration sweep")
        }
        return try String(contentsOf: pbxproj, encoding: .utf8)
    }
}

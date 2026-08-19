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
@testable import NADE

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
        // Derived, not guessed — the same discipline as the deployment-target
        // sweep below. A face added to `Theme.Font` without a `UIAppFonts`
        // entry (or the reverse) fails here rather than rendering the system
        // font at runtime.
        XCTAssertEqual(fonts.count, Theme.Font.PostScriptName.all.count)
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

    /// P2 A19. `docs/DESIGN.md` defines exactly one frame — 402 × 874, an
    /// iPhone in portrait — and every geometry expectation in the suite is
    /// written against it. The target shipped as `TARGETED_DEVICE_FAMILY =
    /// "1,2"` with landscape enabled on both, which advertises three surfaces
    /// (iPad portrait, iPad landscape, iPhone landscape) that no criterion
    /// covers and no render exists for. Narrowing is the honest move; this is
    /// what stops it drifting back.
    ///
    /// Derived the same way as the deployment-target sweep: one setting per
    /// configuration, not "at least one somewhere".
    func testEveryBuildConfigurationTargetsIPhoneOnly() throws {
        let text = try projectFile()

        let lists = try NSRegularExpression(pattern: #"isa = XCConfigurationList;"#)
            .numberOfMatches(in: text, range: NSRange(text.startIndex..., in: text))

        let regex = try NSRegularExpression(pattern: #"TARGETED_DEVICE_FAMILY\s*=\s*([^;]+);"#)
        let matches = regex.matches(in: text, range: NSRange(text.startIndex..., in: text))
        XCTAssertEqual(
            matches.count, lists * 2,
            "\(matches.count) TARGETED_DEVICE_FAMILY settings for \(lists) configuration lists — expected \(lists * 2)"
        )

        var values: [String] = []
        for match in matches {
            guard let r = Range(match.range(at: 1), in: text) else { continue }
            values.append(String(text[r]).trimmingCharacters(in: .whitespaces))
        }
        XCTAssertEqual(
            Set(values), ["1"],
            "TARGETED_DEVICE_FAMILY is not iPhone-only in every configuration: \(values)"
        )
    }

    /// The other half of A19. `TARGETED_DEVICE_FAMILY = 1` removes iPad; this
    /// removes landscape, which the design has no layout for either.
    func testNoConfigurationAdvertisesLandscape() throws {
        let text = try projectFile()

        XCTAssertFalse(
            text.contains("UISupportedInterfaceOrientations_iPad"),
            "an iPad orientation list survives in a build that no longer ships to iPad"
        )

        let regex = try NSRegularExpression(pattern: #"INFOPLIST_KEY_UISupportedInterfaceOrientations_iPhone\s*=\s*([^;]+);"#)
        let matches = regex.matches(in: text, range: NSRange(text.startIndex..., in: text))
        XCTAssertFalse(matches.isEmpty, "no iPhone orientation setting at all — the default would re-enable landscape")

        for match in matches {
            guard let r = Range(match.range(at: 1), in: text) else { continue }
            let value = String(text[r]).trimmingCharacters(in: CharacterSet(charactersIn: " \""))
            XCTAssertEqual(
                value, "UIInterfaceOrientationPortrait",
                "the design has one frame and it is portrait; this configuration advertises \(value)"
            )
        }
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

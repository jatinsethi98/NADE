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
    func testDeploymentTargetIs18() throws {
        let minimum = try XCTUnwrap(
            Bundle.main.object(forInfoDictionaryKey: "MinimumOSVersion") as? String
        )
        XCTAssertEqual(minimum, "18.0")
    }
}

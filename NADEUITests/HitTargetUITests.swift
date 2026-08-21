//
//  HitTargetUITests.swift
//  NADEUITests
//
//  F17 / EDGE (E17). The design draws controls below the HIG's 44 pt minimum —
//  a 30 × 30 stepper box, a 46 × 26 switch, a ~29 pt chip, a 32–34 pt segmented
//  option, a 36–44 pt icon circle — and the pixels have to stay exactly as
//  drawn. `.nadeHitTarget()` therefore grows the *tappable* region only, in an
//  overflowing background that contributes nothing to layout.
//
//  Which is why this has to be a UI test. `contentShape` changes the shape of a
//  hit region, never its size, and a `.frame(minWidth:minHeight:)` would push
//  the design's rows apart — neither is visible to a unit test measuring the
//  view's own size. What XCUITest reports is the element's *hit* frame, so the
//  pair is:
//
//    ComponentGeometryTests → the drawn box is still the design's size
//    HitTargetUITests       → the hit frame is at least 44 × 44
//
//  Both must hold. Remove `.nadeHitTarget()` and every test here fails; replace
//  it with a real `.frame`, and the geometry tests fail instead.
//

import XCTest

final class HitTargetUITests: XCTestCase {

    private static let minimum: CGFloat = 44

    override func setUp() {
        continueAfterFailure = false
    }

    private func launchGallery(section: String) -> XCUIApplication {
        XCUIApplication.nade(seed: nil, now: nil, gallerySection: section)
    }

    private func assertTarget(
        _ element: XCUIElement,
        _ what: String,
        file: StaticString = #filePath,
        line: UInt = #line
    ) {
        XCTAssertTrue(element.waitForExistence(timeout: 10), "\(what) is not in the gallery", file: file, line: line)
        let frame = element.frame
        XCTAssertGreaterThanOrEqual(
            frame.height, Self.minimum,
            "\(what)'s hit region is only \(frame.height) pt tall", file: file, line: line
        )
        XCTAssertGreaterThanOrEqual(
            frame.width, Self.minimum,
            "\(what)'s hit region is only \(frame.width) pt wide", file: file, line: line
        )
    }

    /// The chip draws ~29 pt tall, the toggle 26, the ask circle 38, a 1c
    /// segmented option ~32 — every one of them under the minimum before this.
    func testEverySmallControlHasA44PointTarget() {
        let app = launchGallery(section: "edges")
        assertTarget(app.descendants(matching: .any)["hit.chip"].firstMatch, "the mail filter chip")
        assertTarget(app.descendants(matching: .any)["hit.toggle"].firstMatch, "the approval toggle")
        assertTarget(app.descendants(matching: .any)["hit.icon"].firstMatch, "the ask field's circular button")
        assertTarget(app.descendants(matching: .any)["hit.stepper"].firstMatch, "the 1d stepper")
    }

    /// …and the target is genuinely *outside* the drawn box: a tap 5 pt above
    /// the chip's top edge selects it. `contentShape` alone cannot do this.
    func testAChipRespondsToATapAboveItsDrawnEdge() {
        let app = launchGallery(section: "edges")
        let chip = app.descendants(matching: .any)["hit.chip"].firstMatch
        XCTAssertTrue(chip.waitForExistence(timeout: 10), "no chip in the Edge cases section")
        XCTAssertFalse(chip.isSelected, "the chip should start unselected")

        // 5 pt above the top of the 44 pt target is ~12 pt above the drawn
        // pill, i.e. squarely in the invisible part.
        chip.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0))
            .withOffset(CGVector(dx: 0, dy: 5))
            .tap()

        XCTAssertTrue(
            app.descendants(matching: .any)["hit.chip"].firstMatch.isSelected,
            "a tap inside the chip's target but outside its pill did nothing"
        )
    }

    /// A toggle is 46 × 26, so 9 pt above and below it are target-only.
    func testTheToggleRespondsToATapAboveItsDrawnEdge() {
        let app = launchGallery(section: "edges")
        let toggle = app.descendants(matching: .any)["hit.toggle"].firstMatch
        XCTAssertTrue(toggle.waitForExistence(timeout: 10), "no toggle in the Edge cases section")

        let before = toggle.value as? String
        XCTAssertNotNil(before, "the toggle exposes no value")

        toggle.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0))
            .withOffset(CGVector(dx: 0, dy: 3))
            .tap()

        XCTAssertNotEqual(
            app.descendants(matching: .any)["hit.toggle"].firstMatch.value as? String, before,
            "a tap inside the toggle's target but above its track did nothing"
        )
    }

    // MARK: - Controls inside a glass chrome bar (D99)

    /// 2a's submit circle is 38 pt drawn and depends on `.nadeHitTarget()`'s
    /// overflowing background for all six of its missing points. D99 put it
    /// inside a chrome bar and under a `.glassEffect`, either of which could
    /// have clipped that background away. This is the guard that says neither
    /// does.
    ///
    /// **It is a guard, not a reproduction, and the difference was checked.**
    /// Swapping 2a's bar back to the `safeAreaBar` that broke 1e leaves this
    /// test green — so whatever `safeAreaBar` clips, it did not reach this
    /// control. The reproduction lives in
    /// `MailUITests.testTheChipAndTheBackChevronAreBothAtLeast44Points`, which
    /// is where the failure actually appeared (D99 has the measurements).
    ///
    /// Kept anyway, and D39 is the reason it is worth being explicit about
    /// which of the two this is: a test nobody has seen fail is not evidence of
    /// the bug it names. It is still evidence for the property — the circle is
    /// the only control that moved into a bar and is watched by nothing else —
    /// and this comment is what stops the next reader mistaking one for the
    /// other.
    ///
    /// Not in the gallery, deliberately: the gallery's copy would prove the
    /// component works somewhere, and what is in question is whether it works
    /// *in a bar*.
    func testTheAskFieldsSubmitCircleIsStill44InsideTheGlassBar() {
        let app = XCUIApplication.nade(seed: .fixtures, screen: "2a")
        let submit = app.buttons["ask.field.submit"]
        XCTAssertTrue(submit.waitForExistence(timeout: 10), "2a's ask field has no submit button")

        let frame = submit.frame
        XCTAssertGreaterThanOrEqual(
            frame.height, Self.minimum,
            "the submit circle is \(frame.height) pt tall — its overflowing target is being clipped"
        )
        XCTAssertGreaterThanOrEqual(
            frame.width, Self.minimum,
            "the submit circle is \(frame.width) pt wide — its overflowing target is being clipped"
        )
    }

    // `testTabBarColumnsInTheGalleryAreAlso44` stood here. It asserted that
    // the gallery's copy of `NTabBar` had the same 44 pt columns as the shell's,
    // so a regression would show up twice. Both copies are gone (D98): the
    // shell's bar is a native `TabView` and the gallery now shows one live
    // sample of that same system bar rather than a second hand-drawn one.
    //
    // The 44 pt claim did not go away, it moved: `TabBarUITests` checks the bar
    // that actually ships. Checking UIKit's bar twice, once through the gallery,
    // would be two readings of one fact rather than two facts.
}

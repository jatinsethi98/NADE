//
//  ThreadNavigationUITests.swift
//  NADEUITests
//
//  P2 A11 — where the tab bar goes.
//
//  The rule under test is not "deeper than the root hides it". `docs/DESIGN.md`
//  §2's navigation map keeps the bar on 1e, 1g **and** 1k, all of which are
//  pushes, and removes it only on 1f. Three of the four assertions below fail
//  under any depth-based implementation, which is the point of having them.
//

import XCTest

final class ThreadNavigationUITests: XCTestCase {

    override func setUp() {
        continueAfterFailure = false
    }

    func testPushingAThreadHidesTheTabBarAndComingBackRestoresIt() {
        let app = XCUIApplication.nade(screen: "1e")
        XCTAssertTrue(app.buttons["mailrow.18f2a1b3c4d5e6f7"].waitForExistence(timeout: 10))
        XCTAssertTrue(app.tab("Mail").exists, "1e carries the bar")

        app.buttons["mailrow.18f2a1b3c4d5e6f7"].tap()
        XCTAssertTrue(app.staticTexts["thread.subject"].waitForExistence(timeout: 5))
        XCTAssertFalse(app.tab("Mail").exists, "1f is a pushed detail and carries no tab bar")

        app.buttons["thread.back"].tap()
        XCTAssertTrue(app.tab("Mail").waitForExistence(timeout: 5))
    }

    /// The assertion that kills a depth-based rule: 1e is a push, and it keeps
    /// the bar.
    func testPushingTheMailListDoesNotHideTheTabBar() {
        let app = XCUIApplication.nade()
        XCTAssertTrue(app.buttons["mailboxes.label.INBOX"].waitForExistence(timeout: 10))
        app.buttons["mailboxes.label.INBOX"].tap()
        XCTAssertTrue(app.staticTexts["maillist.title"].waitForExistence(timeout: 5))
        XCTAssertTrue(app.tab("Mail").exists)
    }

    /// …and so does 1k, which DESIGN.md §2 says keeps the Mail tab lit.
    func testPushingSettingsDoesNotHideTheTabBar() {
        let app = XCUIApplication.nade()
        XCTAssertTrue(app.buttons["mailboxes.settings"].waitForExistence(timeout: 10))
        app.buttons["mailboxes.settings"].tap()
        XCTAssertTrue(app.staticTexts["settings.title"].waitForExistence(timeout: 5))
        XCTAssertTrue(app.tab("Mail").exists)
    }

    /// State restoration across a tab rotation.
    ///
    /// The obvious version of this test — push a thread, tap Notes, come back —
    /// is **impossible**: while 1f is on screen the tab bar is gone, so
    /// the Notes tab does not exist to tap. So the thread is restored by launch
    /// argument, popped, rotated through the tabs, and re-pushed.
    func testTheMailStackSurvivesAFullTabRotation() {
        let app = XCUIApplication.nade(screen: "1f", thread: "18f2a1b3c4d5e6f7")
        XCTAssertTrue(app.staticTexts["thread.subject"].waitForExistence(timeout: 10))
        XCTAssertFalse(app.tab("Mail").exists)

        app.buttons["thread.back"].tap()
        XCTAssertTrue(app.buttons["maillist.chip.CATEGORY_UPDATES"].waitForExistence(timeout: 5))
        app.buttons["maillist.chip.CATEGORY_UPDATES"].tap()
        XCTAssertEqual(app.staticTexts["maillist.title"].label, "Updates")

        for tab in ["Ask", "Notes", "Calendar", "Mail"] {
            app.buttons[tab].tap()
        }

        XCTAssertTrue(app.staticTexts["maillist.title"].waitForExistence(timeout: 5),
                      "the Mail stack was rebuilt by a tab switch")
        XCTAssertEqual(app.staticTexts["maillist.title"].label, "Updates",
                       "the selected mailbox did not survive the rotation")
    }

    /// The design draws its own nav bars, so the system one is hidden
    /// throughout. Whether the interactive pop gesture survives that is a
    /// SwiftUI behaviour rather than something the code asserts, so it is
    /// measured rather than assumed.
    func testTheSwipeBackGestureStillPops() {
        let app = XCUIApplication.nade(screen: "1f", thread: "18f2a1b3c4d5e6f7")
        XCTAssertTrue(app.staticTexts["thread.subject"].waitForExistence(timeout: 10))

        // A screen-edge gesture, not a swipe across the middle: `swipeRight()`
        // starts in the centre, which `UIScreenEdgePanGestureRecognizer`
        // ignores by design, so it would fail whether or not the gesture works.
        let edge = app.coordinate(withNormalizedOffset: CGVector(dx: 0.0, dy: 0.5))
        let across = app.coordinate(withNormalizedOffset: CGVector(dx: 0.85, dy: 0.5))
        edge.press(forDuration: 0.1, thenDragTo: across, withVelocity: .slow,
                   thenHoldForDuration: 0.15)

        XCTAssertTrue(app.staticTexts["maillist.title"].waitForExistence(timeout: 5),
                      "swiping from the edge did not pop the thread")

        // **The second pop.** Each destination installs the shared gesture
        // delegate, and `UINavigationController` holds it weakly — so a
        // per-screen delegate would be deallocated by the first pop and leave
        // the recogniser enabled with nothing guarding it. A test that stops
        // after one swipe cannot see that.
        edge.press(forDuration: 0.1, thenDragTo: across, withVelocity: .slow,
                   thenHoldForDuration: 0.15)
        XCTAssertTrue(app.staticTexts["mailboxes.title"].waitForExistence(timeout: 5),
                      "swiping from the edge did not pop the mail list")

        // …and the stack is still usable afterwards, which is the property the
        // `viewControllers.count > 1` guard exists for: a gesture allowed to
        // complete at the root has historically wedged the navigation
        // controller, and the symptom is a push that no longer happens.
        //
        // (An edge swipe *at the root* is deliberately not asserted here. With
        // nothing to pop, what happens next belongs to iOS — the system can
        // absorb the gesture and background the app — and a test that pins
        // that is testing UIKit rather than this code.)
        XCTAssertTrue(app.buttons["mailboxes.label.INBOX"].waitForExistence(timeout: 5))
        app.buttons["mailboxes.label.INBOX"].tap()
        XCTAssertTrue(app.staticTexts["maillist.title"].waitForExistence(timeout: 5),
                      "the navigation stack stopped pushing after two edge-swipe pops")
    }

    /// The thread that `docs/contract/thread_partial.json` describes: fewer
    /// messages than its row counts, and a caption that says so. Before P2 no
    /// fixture carried this state, so nothing had ever rendered it.
    func testAPartialThreadSaysSoUnderItsSubject() {
        let app = XCUIApplication.nade(screen: "1f", mailbox: "Label_12", thread: "18f1d8e7c6b5a409")
        XCTAssertTrue(app.staticTexts["thread.subject"].waitForExistence(timeout: 10))
        XCTAssertTrue(app.staticTexts["thread.partial"].exists,
                      "a thread with gaps rendered as though it were complete")
    }
}

//
//  ShellStateUITests.swift
//  NADEUITests
//
//  F21. `RootTabView` used to `switch` on the selection, so only the active
//  screen existed. Switching tabs therefore destroyed the outgoing screen and
//  everything it owned: `@State`, scroll offset, `NavigationStack` path,
//  running `.task`s. With placeholder screens that is invisible; the moment P2
//  puts a mail list, a note draft or an SSE stream behind a tab it is a bug
//  report. This is the test that would have caught it in P1.
//

import XCTest

final class ShellStateUITests: XCTestCase {

    override func setUp() {
        continueAfterFailure = false
    }

    private func launchApp() -> XCUIApplication {
        XCUIApplication.nade(seed: nil, now: nil)
    }

    /// **Rewritten at P3**, for the reason P2 rewrote the Mail leg below:
    /// replacing the Ask placeholder with 2a deleted `screen.ask.taps`, and
    /// deleting the assertion with it would drop the property D29 exists to
    /// protect. 2a asserts the state it actually has — the feed ⇄ focus mode,
    /// which lives on `HomeFeedModel` and has process lifetime. That is
    /// strictly stronger than a tap counter: the mode drives which of two whole
    /// layouts is on screen.
    func testScreenStateSurvivesLeavingAndReturningToATab() {
        let app = launchApp()
        let grabber = app.buttons["home.grabber"]
        XCTAssertTrue(grabber.waitForHittable(timeout: 10))

        // Feed → focus.
        grabber.tap()
        XCTAssertTrue(app.staticTexts["home.greeting"].waitForExistence(timeout: 3),
                      "tapping the grabber did not reach the focus state")

        app.tab("Calendar").tap()
        XCTAssertTrue(app.buttons["screen.calendar.taps"].waitForExistence(timeout: 3))
        XCTAssertEqual(app.buttons["screen.calendar.taps"].label, "Taps: 0",
                       "the calendar screen should have its own state")

        app.tab("Ask").tap()
        XCTAssertTrue(
            app.staticTexts["home.greeting"].waitForExistence(timeout: 3),
            "the Ask screen was rebuilt when the tab changed — its model, scroll position and running tasks would all be gone"
        )
    }

    /// Every screen keeps its own state independently, and a round trip through
    /// all four does not disturb any of them.
    ///
    /// **Rewritten at P2.** The Mail leg used to tap `screen.mail.taps`, a
    /// counter on the placeholder screen that existed only so this test had
    /// something to count. Replacing the placeholder with the real Mail tab
    /// deleted that element — and deleting the assertion with it would have
    /// been the wrong move, because the property D29 protects is now load-
    /// bearing rather than hypothetical. So Mail asserts the state it actually
    /// has: a pushed mail list, on a mailbox that is not the default. That is
    /// strictly stronger than a tap counter, which no navigation stack could
    /// have lost.
    func testAllFourScreensKeepTheirOwnStateAcrossAFullRotation() {
        let app = XCUIApplication.nade()

        // Ask: move off the default state, the same way the test above does.
        //
        // **Select the tab first.** A seeded launch applies `-NADEScreen`, whose
        // default is the Mail tab, and `RootTabView` keeps all four screens in
        // the tree (D29) with `allowsHitTesting` off for the three that are not
        // showing. 2a therefore *exists* from launch and is not tappable until
        // its tab is selected — which is the property this test is about, so
        // asserting on existence alone would have quietly tested nothing.
        app.tab("Ask").tap()
        XCTAssertTrue(app.staticTexts["home.date"].waitForExistence(timeout: 10))
        let grabber = app.buttons["home.grabber"]
        XCTAssertTrue(grabber.waitForHittable(timeout: 10))
        grabber.tap()
        XCTAssertTrue(app.staticTexts["home.greeting"].waitForExistence(timeout: 3))

        // Mail: push a list and move off the default mailbox.
        app.tab("Mail").tap()
        XCTAssertTrue(app.buttons["mailboxes.label.CATEGORY_UPDATES"].waitForExistence(timeout: 5))
        app.buttons["mailboxes.label.CATEGORY_UPDATES"].tap()
        XCTAssertTrue(app.staticTexts["maillist.title"].waitForExistence(timeout: 5))
        XCTAssertEqual(app.staticTexts["maillist.title"].label, "Updates")

        // The two remaining placeholders still carry their counters.
        //
        // The tab is addressed by title and the screen by the tab's raw value;
        // the two are written out rather than derived from each other, because
        // UIKit's bar gives no identifier to match on (see
        // `XCUIApplication.tab(_:)`) and they agree only by capitalisation.
        let placeholders = [(title: "Notes", id: "notes"), (title: "Calendar", id: "calendar")]
        for (index, placeholder) in placeholders.enumerated() {
            app.tab(placeholder.title).tap()
            let counter = app.buttons["screen.\(placeholder.id).taps"]
            XCTAssertTrue(counter.waitForExistence(timeout: 3))
            for _ in 0..<(index + 1) { counter.tap() }
            XCTAssertEqual(counter.label, "Taps: \(index + 1)")
        }

        // Come back around; nothing was rebuilt.
        for (index, placeholder) in placeholders.enumerated() {
            app.tab(placeholder.title).tap()
            let counter = app.buttons["screen.\(placeholder.id).taps"]
            XCTAssertTrue(counter.waitForExistence(timeout: 3))
            XCTAssertEqual(counter.label, "Taps: \(index + 1)",
                           "the \(placeholder.id) screen lost its state")
        }

        app.tab("Ask").tap()
        XCTAssertTrue(app.staticTexts["home.greeting"].waitForExistence(timeout: 3),
                      "the Ask tab was rebuilt: it fell back to the feed state")

        app.tab("Mail").tap()
        XCTAssertTrue(app.staticTexts["maillist.title"].waitForExistence(timeout: 3),
                      "the Mail tab was rebuilt: its pushed list is gone")
        XCTAssertEqual(app.staticTexts["maillist.title"].label, "Updates",
                       "the Mail tab kept its stack but lost the mailbox it was showing")
    }
}

extension XCUIElement {
    /// Exists **and** can be tapped.
    ///
    /// `waitForExistence` answers a weaker question than every caller here
    /// wants: an element inside a scroll view exists the moment the list does,
    /// and tapping it before layout settles fails with "not hittable".
    func waitForHittable(timeout: TimeInterval) -> Bool {
        let predicate = NSPredicate(format: "exists == true AND isHittable == true")
        let expectation = XCTNSPredicateExpectation(predicate: predicate, object: self)
        return XCTWaiter().wait(for: [expectation], timeout: timeout) == .completed
    }
}

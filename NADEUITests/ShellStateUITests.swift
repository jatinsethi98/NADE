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

    func testScreenStateSurvivesLeavingAndReturningToATab() {
        let app = launchApp()
        let ask = app.buttons["screen.ask.taps"]
        XCTAssertTrue(ask.waitForExistence(timeout: 10))
        XCTAssertEqual(ask.label, "Taps: 0")

        ask.tap()
        ask.tap()
        ask.tap()
        XCTAssertEqual(ask.label, "Taps: 3")

        app.buttons["tab.calendar"].tap()
        XCTAssertTrue(app.buttons["screen.calendar.taps"].waitForExistence(timeout: 3))
        XCTAssertEqual(app.buttons["screen.calendar.taps"].label, "Taps: 0", "the calendar screen should have its own state")

        app.buttons["tab.ask"].tap()
        XCTAssertTrue(ask.waitForExistence(timeout: 3))
        XCTAssertEqual(
            ask.label, "Taps: 3",
            "the Ask screen was rebuilt when the tab changed — its @State, scroll position and running tasks would all be gone"
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

        XCTAssertTrue(app.buttons["screen.ask.taps"].waitForExistence(timeout: 10))

        // Mail: push a list and move off the default mailbox.
        app.buttons["tab.mail"].tap()
        XCTAssertTrue(app.buttons["mailboxes.label.CATEGORY_UPDATES"].waitForExistence(timeout: 5))
        app.buttons["mailboxes.label.CATEGORY_UPDATES"].tap()
        XCTAssertTrue(app.staticTexts["maillist.title"].waitForExistence(timeout: 5))
        XCTAssertEqual(app.staticTexts["maillist.title"].label, "Updates")

        // The other three still carry their placeholder counters.
        let placeholders = ["ask", "notes", "calendar"]
        for (index, id) in placeholders.enumerated() {
            app.buttons["tab.\(id)"].tap()
            let counter = app.buttons["screen.\(id).taps"]
            XCTAssertTrue(counter.waitForExistence(timeout: 3))
            for _ in 0..<(index + 1) { counter.tap() }
            XCTAssertEqual(counter.label, "Taps: \(index + 1)")
        }

        // Come back around; nothing was rebuilt.
        for (index, id) in placeholders.enumerated() {
            app.buttons["tab.\(id)"].tap()
            let counter = app.buttons["screen.\(id).taps"]
            XCTAssertTrue(counter.waitForExistence(timeout: 3))
            XCTAssertEqual(counter.label, "Taps: \(index + 1)", "the \(id) screen lost its state")
        }

        app.buttons["tab.mail"].tap()
        XCTAssertTrue(app.staticTexts["maillist.title"].waitForExistence(timeout: 3),
                      "the Mail tab was rebuilt: its pushed list is gone")
        XCTAssertEqual(app.staticTexts["maillist.title"].label, "Updates",
                       "the Mail tab kept its stack but lost the mailbox it was showing")
    }
}

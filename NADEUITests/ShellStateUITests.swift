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
        let app = XCUIApplication()
        app.launchArguments = ["-NADEGallery", "0"]
        app.launch()
        return app
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
    func testAllFourScreensKeepTheirOwnStateAcrossAFullRotation() {
        let app = launchApp()
        XCTAssertTrue(app.buttons["screen.ask.taps"].waitForExistence(timeout: 10))

        let tabs = ["ask", "mail", "notes", "calendar"]
        // Tap the counter a different number of times on each screen.
        for (index, id) in tabs.enumerated() {
            app.buttons["tab.\(id)"].tap()
            let counter = app.buttons["screen.\(id).taps"]
            XCTAssertTrue(counter.waitForExistence(timeout: 3))
            for _ in 0..<(index + 1) { counter.tap() }
            XCTAssertEqual(counter.label, "Taps: \(index + 1)")
        }

        // Come back around; nothing was rebuilt.
        for (index, id) in tabs.enumerated() {
            app.buttons["tab.\(id)"].tap()
            let counter = app.buttons["screen.\(id).taps"]
            XCTAssertTrue(counter.waitForExistence(timeout: 3))
            XCTAssertEqual(
                counter.label, "Taps: \(index + 1)",
                "the \(id) screen lost its state"
            )
        }
    }
}

//
//  TabBarUITests.swift
//  NADEUITests
//
//  The P1 smoke test: the app launches into the shell, the four-tab bar is
//  there, and tapping a tab moves the selection.
//
//  It leans on the accessibility work rather than on layout — `isSelected`
//  comes from `NTabBar`'s `.isSelected` trait, so this test also fails if the
//  VoiceOver traits regress. EDGE (E6).
//

import XCTest

final class TabBarUITests: XCTestCase {

    private static let tabs = ["ask", "mail", "notes", "calendar"]
    private static let titles = ["Ask", "Mail", "Notes", "Calendar"]

    override func setUp() {
        continueAfterFailure = false
    }

    private func launchApp() -> XCUIApplication {
        let app = XCUIApplication()
        // Explicitly *not* the gallery — this exercises the shipping shell.
        app.launchArguments = ["-NADEGallery", "0"]
        app.launch()
        return app
    }

    func testTabBarHasAllFourTabs() {
        let app = launchApp()
        for (id, title) in zip(Self.tabs, Self.titles) {
            let tab = app.buttons["tab.\(id)"]
            XCTAssertTrue(tab.waitForExistence(timeout: 10), "tab.\(id) is missing")
            XCTAssertEqual(tab.label, title, "tab.\(id) is labelled \"\(tab.label)\" — VoiceOver would read the uppercased form")
        }
    }

    func testAskIsSelectedOnLaunch() {
        let app = launchApp()
        XCTAssertTrue(app.buttons["tab.ask"].waitForExistence(timeout: 10))
        XCTAssertTrue(app.buttons["tab.ask"].isSelected, "Ask should be the launch tab")
        for id in Self.tabs.dropFirst() {
            XCTAssertFalse(app.buttons["tab.\(id)"].isSelected, "tab.\(id) should not start selected")
        }
    }

    func testTappingEachTabChangesTheSelection() {
        let app = launchApp()
        XCTAssertTrue(app.buttons["tab.ask"].waitForExistence(timeout: 10))

        for id in Self.tabs {
            app.buttons["tab.\(id)"].tap()

            let selected = app.buttons["tab.\(id)"]
            XCTAssertTrue(
                selected.isSelected,
                "tapping tab.\(id) did not select it"
            )
            for other in Self.tabs where other != id {
                XCTAssertFalse(
                    app.buttons["tab.\(other)"].isSelected,
                    "tab.\(other) stayed selected after tapping tab.\(id)"
                )
            }
        }
    }

    func testEachTabShowsItsOwnScreen() {
        let app = launchApp()
        XCTAssertTrue(app.buttons["tab.ask"].waitForExistence(timeout: 10))

        let notes: [String: String] = [
            "ask": "Ask, search, or describe an agent.",
            "mail": "Your mail, filtered by label.",
            "notes": "Notes your agents write.",
            "calendar": "Six days, each a compressed timeline.",
        ]

        for (id, note) in notes {
            app.buttons["tab.\(id)"].tap()
            XCTAssertTrue(
                app.staticTexts[note].waitForExistence(timeout: 3),
                "the \(id) screen did not appear"
            )
        }
    }
}

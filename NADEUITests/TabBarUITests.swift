//
//  TabBarUITests.swift
//  NADEUITests
//
//  The P1 smoke test: the app launches into the shell, the four-tab bar is
//  there, tapping a tab moves the selection, and the bar reads as a tab bar to
//  VoiceOver rather than as four loose buttons.
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
            let visible = app.staticTexts["screen.\(id).note"]
            XCTAssertTrue(visible.waitForExistence(timeout: 3), "the \(id) screen did not appear")
            XCTAssertEqual(visible.label, note)
            XCTAssertTrue(visible.isHittable, "the \(id) screen is not the one on screen")

            // The other three are still in the view tree — that is F21, and
            // what keeps their state — but none of them is reachable.
            for otherID in notes.keys where otherID != id {
                XCTAssertFalse(
                    app.staticTexts["screen.\(otherID).note"].isHittable,
                    "the \(otherID) screen is still reachable while \(id) is showing"
                )
            }
        }
    }

    // MARK: - Tab-bar semantics (F20)

    /// The four buttons had button semantics and nothing else: no container
    /// role, no grouping, no position. VoiceOver announced four unrelated
    /// controls floating at the bottom of the screen.
    ///
    /// (SwiftUI's `.isTabBar` trait does not change the element *type*
    /// XCUITest reports — the container comes back as `Other` — so the query
    /// is by identifier. What is assertable, and asserted, is that the
    /// container exists, is named, and is the parent of exactly the four tabs.)
    func testTheBarIsAGroupedTabBarAndNotFourLooseButtons() {
        let app = launchApp()
        XCTAssertTrue(app.buttons["tab.ask"].waitForExistence(timeout: 10))

        let bar = app.otherElements["tabbar"]
        XCTAssertTrue(bar.waitForExistence(timeout: 5), "there is no tab-bar container at all")
        XCTAssertEqual(bar.label, "Tabs", "the tab bar has no container label")

        let tabs = bar.buttons.allElementsBoundByIndex.filter { $0.identifier.hasPrefix("tab.") }
        XCTAssertEqual(
            Set(tabs.map(\.identifier)), Set(Self.tabs.map { "tab.\($0)" }),
            "the four tabs are not grouped inside the container: \(tabs.map(\.identifier))"
        )

        // And nothing outside the container claims to be a tab.
        for id in Self.tabs {
            XCTAssertTrue(bar.buttons["tab.\(id)"].exists, "tab.\(id) is not inside the tab bar container")
        }
    }

    /// "Ask, Tab 1 of 4, selected" — the position is what tells a VoiceOver
    /// user where they are in the bar.
    func testEachTabAnnouncesItsPosition() {
        let app = launchApp()
        XCTAssertTrue(app.buttons["tab.ask"].waitForExistence(timeout: 10))

        for (index, id) in Self.tabs.enumerated() {
            let tab = app.buttons["tab.\(id)"]
            XCTAssertEqual(
                tab.value as? String, "Tab \(index + 1) of 4",
                "tab.\(id) announces \(String(describing: tab.value))"
            )
        }
    }

    // MARK: - Hit target (F17)

    /// EDGE (E17): the bar's `9 / 10 / 26` padding used to sit *outside* the
    /// buttons, so a tab's own frame was the ~43 pt glyph-and-label stack with
    /// 35 pt of dead space under it. Moving the padding inside makes the whole
    /// column tappable without moving a pixel.
    func testEachTabsFrameCoversTheWholeColumn() {
        let app = launchApp()
        XCTAssertTrue(app.buttons["tab.ask"].waitForExistence(timeout: 10))

        for id in Self.tabs {
            let frame = app.buttons["tab.\(id)"].frame
            XCTAssertGreaterThanOrEqual(
                frame.height, 44,
                "tab.\(id) is only \(frame.height) pt tall — the bar's padding is outside the button again"
            )
            XCTAssertGreaterThanOrEqual(frame.width, 44, "tab.\(id) is only \(frame.width) pt wide")
        }

        // The four columns tile the bar's width without overlapping.
        let frames = Self.tabs.map { app.buttons["tab.\($0)"].frame }.sorted { $0.minX < $1.minX }
        for (a, b) in zip(frames, frames.dropFirst()) {
            XCTAssertLessThanOrEqual(a.maxX, b.minX + 0.5, "two tab columns overlap: \(a) and \(b)")
        }

        // F13. The mockup's tab bar is `padding: 9px 10px 26px`. The 9 and the
        // 26 change the bar's own height and are measured in
        // `ComponentGeometryTests.testTabBarHeightIsBuiltFromItsOwnMetrics`; the
        // **10** changes nothing a unit test can see, because the four columns
        // are `maxWidth: .infinity` and absorb it. It is only observable as the
        // gap between the screen edge and the first column, here, in the
        // running app. (The number is repeated rather than imported: a UI test
        // target cannot `@testable import NADE`. `ThemeTests` pins the constant
        // against the design; this pins the render against the same number.)
        let screen = app.windows.element(boundBy: 0).frame
        XCTAssertEqual(
            Double(frames[0].minX - screen.minX), 10, accuracy: 1,
            "the tab bar's leading padding renders at \(frames[0].minX - screen.minX), not the mockup's 10"
        )
        XCTAssertEqual(
            Double(screen.maxX - frames[3].maxX), 10, accuracy: 1,
            "the tab bar's trailing padding renders at \(screen.maxX - frames[3].maxX), not the mockup's 10"
        )
    }

    /// The bottom row of the bar — the 26 pt band the design puts under the
    /// labels — must belong to the tab, not to nothing.
    func testTappingLowInTheBarStillSelectsTheTab() {
        let app = launchApp()
        XCTAssertTrue(app.buttons["tab.ask"].waitForExistence(timeout: 10))

        let mail = app.buttons["tab.mail"]
        // 4 pt above the bottom edge of the button, i.e. deep in the 26 pt
        // display-edge padding, well below the label.
        mail.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 1))
            .withOffset(CGVector(dx: 0, dy: -4))
            .tap()
        XCTAssertTrue(mail.isSelected, "a tap in the tab bar's bottom padding did nothing")
    }
}

//
//  TabBarUITests.swift
//  NADEUITests
//
//  The P1 smoke test: the app launches into the shell, the four-tab bar is
//  there, tapping a tab moves the selection, and the bar reads as a tab bar to
//  VoiceOver rather than as four loose buttons.
//
//  It leans on the accessibility work rather than on layout. That used to make
//  it a test of `NTabBar`'s hand-written traits — `.isTabBar` on the container,
//  `.isSelected` on the active column, a synthesised "Tab 1 of 4" value. D98
//  replaced the bar with a native `TabView`, so the surviving assertions now
//  check that UIKit supplies what we used to build. Worth keeping rather than
//  deleting: it is exactly the claim the migration was sold on, and the day the
//  container stops reporting as a tab bar this is what says so. EDGE (E6).
//
//  Three tests went, each because its subject went with `NTabBar`:
//
//  * the mockup's 10 pt leading and trailing padding, and the tap deep in the
//    design's 26 pt bottom band — neither number is ours any more;
//  * `testEachTabAnnouncesItsPosition`. `NTabBar` had to *build* the string
//    "Tab 1 of 4" because SwiftUI derives nothing for four loose buttons. A
//    `UITabBar` item exposes no `value` at all through XCUITest (verified on
//    iOS 26.5: `value` is empty for all four), because the position is composed
//    by VoiceOver from the bar itself rather than stored on the item. The
//    announcement is the platform's now and is better for it — but it is not
//    observable from here, and a test that cannot see its subject is not a
//    test. Recorded in IOS_DECISIONS D98 rather than left as a silent deletion.
//
//  The tabs are addressed by title, through `XCUIApplication.tab(_:)` — see
//  that helper for why the identifiers had to go.
//

import XCTest

final class TabBarUITests: XCTestCase {

    /// Titles, not raw values: the title is the only handle UIKit's bar gives
    /// us. `NTab.title` is the source of these four.
    private static let titles = ["Ask", "Mail", "Notes", "Calendar"]

    override func setUp() {
        continueAfterFailure = false
    }

    private func launchApp() -> XCUIApplication {
        // Explicitly *not* the gallery — this exercises the shipping shell.
        XCUIApplication.nade(seed: nil, now: nil)
    }

    func testTabBarHasAllFourTabs() {
        let app = launchApp()
        for title in Self.titles {
            let tab = app.tab(title)
            XCTAssertTrue(tab.waitForExistence(timeout: 10), "the \(title) tab is missing")
            XCTAssertEqual(tab.label, title, "the \(title) tab is labelled \"\(tab.label)\"")
        }
    }

    func testAskIsSelectedOnLaunch() {
        let app = launchApp()
        XCTAssertTrue(app.tab("Ask").waitForExistence(timeout: 10))
        XCTAssertTrue(app.tab("Ask").isSelected, "Ask should be the launch tab")
        for title in Self.titles.dropFirst() {
            XCTAssertFalse(app.tab(title).isSelected, "the \(title) tab should not start selected")
        }
    }

    func testTappingEachTabChangesTheSelection() {
        let app = launchApp()
        XCTAssertTrue(app.tab("Ask").waitForExistence(timeout: 10))

        for title in Self.titles {
            app.tab(title).tap()

            XCTAssertTrue(
                app.tab(title).isSelected,
                "tapping the \(title) tab did not select it"
            )
            for other in Self.titles where other != title {
                XCTAssertFalse(
                    app.tab(other).isSelected,
                    "the \(other) tab stayed selected after tapping \(title)"
                )
            }
        }
    }

    func testEachTabShowsItsOwnScreen() {
        let app = launchApp()
        XCTAssertTrue(app.tab("Ask").waitForExistence(timeout: 10))

        // P2 replaced the Mail placeholder with 1g and P3 replaced the Ask one
        // with 2a, so both are identified by their real screens. Notes and
        // Calendar are still placeholders and are still checked by their copy.
        //
        // Both halves of the pair are written out — the tab addressed by its
        // title, the screen by the tab's raw value — rather than deriving one
        // from the other. They match only by coincidence of capitalisation.
        let placeholders = [
            (title: "Notes", id: "notes", note: "Notes your agents write."),
            (title: "Calendar", id: "calendar", note: "Six days, each a compressed timeline."),
        ]

        app.tab("Mail").tap()
        let mailboxes = app.staticTexts["mailboxes.title"]
        XCTAssertTrue(mailboxes.waitForExistence(timeout: 5), "the mail screen did not appear")
        XCTAssertTrue(mailboxes.isHittable)

        app.tab("Ask").tap()
        let home = app.staticTexts["home.date"]
        XCTAssertTrue(home.waitForExistence(timeout: 5), "the ask screen did not appear")
        XCTAssertTrue(home.isHittable)
        XCTAssertFalse(mailboxes.isHittable,
                       "the mail screen is still reachable while ask is showing")

        for placeholder in placeholders {
            app.tab(placeholder.title).tap()
            let visible = app.staticTexts["screen.\(placeholder.id).note"]
            XCTAssertTrue(visible.waitForExistence(timeout: 3), "the \(placeholder.id) screen did not appear")
            XCTAssertEqual(visible.label, placeholder.note)
            XCTAssertTrue(visible.isHittable, "the \(placeholder.id) screen is not the one on screen")

            XCTAssertFalse(app.staticTexts["mailboxes.title"].isHittable,
                           "the mail screen is still reachable while \(placeholder.id) is showing")

            // The others are still in the view tree — that is what keeps their
            // state — but none of them is reachable.
            for other in placeholders where other.id != placeholder.id {
                XCTAssertFalse(
                    app.staticTexts["screen.\(other.id).note"].isHittable,
                    "the \(other.id) screen is still reachable while \(placeholder.id) is showing"
                )
            }
        }
    }

    // MARK: - Tab-bar semantics (F20)

    /// The four buttons once had button semantics and nothing else: no
    /// container role, no grouping, no position. VoiceOver announced four
    /// unrelated controls floating at the bottom of the screen. `NTabBar` fixed
    /// that by hand, with an `.accessibilityElement(children: .contain)`
    /// container carrying `.isTabBar` and the label "Tabs".
    ///
    /// A native `TabView` is a real `UITabBar`, so XCUITest reports it as a
    /// first-class `tabBars` element rather than as an `Other` that had to be
    /// found by identifier. The claim is unchanged — one container, holding
    /// exactly these four tabs — and it is now checked against the shipping bar
    /// rather than against our own reconstruction of one.
    func testTheBarIsAGroupedTabBarAndNotFourLooseButtons() {
        let app = launchApp()
        XCTAssertTrue(app.tab("Ask").waitForExistence(timeout: 10))

        XCTAssertEqual(app.tabBars.count, 1, "expected exactly one tab bar")
        let bar = app.tabBars.firstMatch
        XCTAssertTrue(bar.waitForExistence(timeout: 5), "there is no tab-bar container at all")

        let labels = bar.buttons.allElementsBoundByIndex.map(\.label)
        XCTAssertEqual(
            Set(labels), Set(Self.titles),
            "the tab bar does not hold exactly the four tabs: \(labels)"
        )
    }

    // MARK: - Hit target (F17)

    /// EDGE (E17). Under `NTabBar` this was a real defect and a real fix: the
    /// bar's `9 / 10 / 26` padding sat *outside* the buttons, so a tab's frame
    /// was the ~43 pt glyph-and-label stack with 35 pt of dead space beneath
    /// it, and the fix was to move the padding inside each column.
    ///
    /// UIKit's bar does not have that bug to have. The test stays anyway, for
    /// the reason D39 gives: 44 pt is a requirement of this app, not a courtesy
    /// of whoever draws the bar, and a requirement nothing checks is one that
    /// quietly stops holding. What is gone is the pair of assertions that
    /// pinned the *mockup's* 10 pt leading and trailing padding — the columns
    /// are laid out by UIKit now, and that number was never going to survive.
    func testEachTabsFrameIsAtLeastTheMinimumTarget() {
        let app = launchApp()
        XCTAssertTrue(app.tab("Ask").waitForExistence(timeout: 10))

        for title in Self.titles {
            let frame = app.tab(title).frame
            XCTAssertGreaterThanOrEqual(
                frame.height, 44,
                "the \(title) tab is only \(frame.height) pt tall"
            )
            XCTAssertGreaterThanOrEqual(frame.width, 44, "the \(title) tab is only \(frame.width) pt wide")
        }

        // The non-overlap assertion that stood here went with the padding pair,
        // and for a sharper reason than "not our number any more": it is false
        // of a system bar and was true only of ours. `NTabBar`'s four columns
        // were `maxWidth: .infinity` and tiled the width exactly. UIKit's
        // reported frames deliberately overlap — measured on iOS 26.5, Ask ends
        // at x=120.0 and Mail begins at x=110.67 — because each item's touch
        // region is grown past its drawn column. Asserting they do not overlap
        // asserted a fact about the old bar's layout, and failed on the new
        // bar's hit-testing being generous. Nothing about the tabs being
        // distinct and tappable is lost: `testTappingEachTabChangesTheSelection`
        // taps all four and checks that each selects itself and only itself.
    }
}

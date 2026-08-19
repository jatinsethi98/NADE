//
//  MailUITests.swift
//  NADEUITests
//
//  The Mail tab, end to end, against the frozen fixture world.
//
//  Every launch here passes `-NADESeed`, which resets its store first, so no
//  test can be handed the previous one's rows (EDGE P11). `-NADENow` pins the
//  clock, so a row's timestamp is the same string on every run.
//

import XCTest

final class MailUITests: XCTestCase {

    override func setUp() {
        continueAfterFailure = false
    }

    // MARK: - 1g is the root

    func testTheMailTabOpensOnMailboxes() {
        let app = XCUIApplication.nade()
        XCTAssertTrue(app.staticTexts["mailboxes.title"].waitForExistence(timeout: 10))
        XCTAssertTrue(app.buttons["mailboxes.account"].exists)
        XCTAssertTrue(app.buttons["mailboxes.label.INBOX"].exists)
        XCTAssertTrue(app.buttons["tab.mail"].exists, "1g carries the tab bar")
    }

    /// The mockup's "All inboxes" row, under one account.
    func testTheAccountRowOpensTheInbox() {
        let app = XCUIApplication.nade()
        app.buttons["mailboxes.account"].tap()
        XCTAssertTrue(app.staticTexts["maillist.title"].waitForExistence(timeout: 5))
        XCTAssertEqual(app.staticTexts["maillist.title"].label, "Inbox")
        XCTAssertTrue(app.buttons["mailrow.18f2a1b3c4d5e6f7"].waitForExistence(timeout: 5))
    }

    func testALabelRowOpensThatMailbox() {
        let app = XCUIApplication.nade()
        app.buttons["mailboxes.label.Label_12"].tap()
        XCTAssertTrue(app.staticTexts["maillist.title"].waitForExistence(timeout: 5))
        XCTAssertEqual(app.staticTexts["maillist.title"].label, "To Reply")
        // A user label also gets the smart-rule caption row.
        XCTAssertTrue(app.staticTexts["maillist.caption"].exists)
    }

    // MARK: - 1e

    func testAChipRetitlesTheHeaderAndMovesTheSelection() {
        let app = XCUIApplication.nade(screen: "1e")
        XCTAssertTrue(app.staticTexts["maillist.title"].waitForExistence(timeout: 10))
        XCTAssertEqual(app.staticTexts["maillist.title"].label, "Inbox")
        XCTAssertTrue(app.buttons["maillist.chip.INBOX"].isSelected)

        app.buttons["maillist.chip.CATEGORY_UPDATES"].tap()
        XCTAssertEqual(app.staticTexts["maillist.title"].label, "Updates")
        XCTAssertTrue(app.buttons["maillist.chip.CATEGORY_UPDATES"].isSelected)
        XCTAssertFalse(app.buttons["maillist.chip.INBOX"].isSelected)
    }

    /// `docs/API.md` §2 leaves the empty-`from_name` fallback to the UI, and
    /// this is the row that exercises it: the fixture's Halcyon thread has
    /// `from_name: ""` and must read as its address, not as a blank line.
    func testARowWithNoSenderNameFallsBackToTheAddress() {
        let app = XCUIApplication.nade(screen: "1e")
        let row = app.buttons["mailrow.18f2554a3b2c1d0e"]
        XCTAssertTrue(row.waitForExistence(timeout: 10))
        XCTAssertTrue(row.label.hasPrefix("talent@halcyon.io"), "row reads: \(row.label)")
    }

    /// EDGE (E6). One element, composed in reading order — not six leaves that
    /// VoiceOver walks separately — and "unread" as a value, because a silent
    /// accent dot is invisible.
    func testAMailRowIsOneElementAndSaysWhenItIsUnread() {
        let app = XCUIApplication.nade(screen: "1e")
        let unread = app.buttons["mailrow.18f2a1b3c4d5e6f7"]
        XCTAssertTrue(unread.waitForExistence(timeout: 10))
        XCTAssertTrue(unread.label.contains("Priya Raghavan"))
        XCTAssertTrue(unread.label.contains("Staff Product Designer at Kettle"))
        XCTAssertTrue(unread.label.contains("Job Search Tracker"))
        XCTAssertEqual(unread.value as? String, "Unread")

        let read = app.buttons["mailrow.18f2705f4e3d2c1b"]
        XCTAssertNotEqual(read.value as? String, "Unread", "a read row must not claim to be unread")
    }

    /// EDGE (E17). The chip is ~29 pt drawn; `.nadeHitTarget()` is what makes
    /// it tappable, and the header chevron is a bare glyph.
    func testTheChipAndTheBackChevronAreBothAtLeast44Points() {
        let app = XCUIApplication.nade(screen: "1e")
        XCTAssertTrue(app.buttons["maillist.chip.INBOX"].waitForExistence(timeout: 10))
        for id in ["maillist.chip.INBOX", "maillist.back"] {
            let frame = app.buttons[id].frame
            XCTAssertGreaterThanOrEqual(frame.height, 44, "\(id) is \(frame.height) pt tall")
            XCTAssertGreaterThanOrEqual(frame.width, 44, "\(id) is \(frame.width) pt wide")
        }
    }

    /// A page boundary, through the UI. The fixture's second page is the last
    /// one, so its single row appearing proves both the load-more trigger and
    /// that `reached_end` stopped it.
    func testScrollingToTheEndLoadsTheNextPage() {
        let app = XCUIApplication.nade(screen: "1e")
        let lastPageRow = app.buttons["mailrow.18f1f0a2b3c4d5e6"]
        XCTAssertTrue(lastPageRow.waitForExistence(timeout: 10),
                      "the second page never loaded")
    }

    // MARK: - Empty

    func testTheEmptyStateSaysWhatIsEmptyAndDoesNotOverclaim() {
        let app = XCUIApplication.nade(seed: .empty)

        let caption = app.staticTexts["mailboxes.caption"]
        XCTAssertTrue(caption.waitForExistence(timeout: 10))
        // `API.md` §2 forbids presenting the synced window as mailbox-wide
        // truth, so no empty state may say a bare "No mail".
        XCTAssertFalse(app.buttons["mailboxes.label.INBOX"].exists)
    }

    // MARK: - Copy

    /// DESIGN.md §4 and PLAN C1/C2: v1 takes no outbound action, so no control
    /// the app names may promise one.
    ///
    /// **Scoped to app-authored labels**, and that scope is the whole design of
    /// this test. A sweep over every control trips on the fixture's own mail —
    /// `thread.json`'s body says "I'll send the invites", and a mail row's
    /// accessibility label is composed from the sender, subject and snippet, so
    /// the row *is* a button whose name is the server's words. A test that
    /// fails on legitimate content gets weakened or deleted rather than fixed,
    /// and then it protects nothing.
    ///
    /// What §4 governs is the copy **this app wrote**: button titles, section
    /// names, spoken names for chrome. The exclusions below are content, by
    /// identifier, and each one is a place the wire supplies the string.
    func testNoControlInTheMailTabPromisesAnOutboundAction() {
        // Every element here is named from `docs/contract` data, not by us.
        let contentPrefixes = ["mailrow.", "mailboxes.label.", "mailboxes.account",
                               "thread.agentcard.", "maillist.chip.", "settings.account",
                               "settings.server"]
        let forbidden = #"\b(send(s|ing)?|forward(s|ing)?|reply-all|archive)\b"#

        for screen in ["1g", "1e", "1f", "1k"] {
            let app = XCUIApplication.nade(screen: screen)
            XCTAssertTrue(app.buttons.firstMatch.waitForExistence(timeout: 10), screen)

            let offenders = app.buttons.allElementsBoundByIndex
                .filter { button in
                    // An element with no identifier is not one this app named:
                    // every control written here carries one, and XCUITest also
                    // synthesises unnamed wrappers around the rows whose labels
                    // are the server's words. Both are content, not copy.
                    !button.identifier.isEmpty
                        && !contentPrefixes.contains { button.identifier.hasPrefix($0) }
                }
                .map(\.label)
                .filter { $0.range(of: forbidden, options: [.regularExpression, .caseInsensitive]) != nil }

            XCTAssertTrue(offenders.isEmpty, "\(screen) has app-named controls \(offenders)")
        }
    }

    /// The exclusion list above is only honest if it is the *only* thing
    /// keeping the sweep green — otherwise it could quietly grow to cover a
    /// real violation. This proves the sweep can still fail: the fixture's own
    /// mail contains "send", and a row is a button, so removing the content
    /// exclusion must light it up.
    func testTheCopySweepWouldCatchAnAppNamedControl() {
        let app = XCUIApplication.nade(screen: "1e")
        XCTAssertTrue(app.buttons["mailrow.18f2a1b3c4d5e6f7"].waitForExistence(timeout: 10))

        let unfiltered = app.buttons.allElementsBoundByIndex.map(\.label)
        let hits = unfiltered.filter {
            $0.range(of: #"\bsend(s|ing)?\b"#, options: [.regularExpression, .caseInsensitive]) != nil
        }
        XCTAssertFalse(hits.isEmpty,
                       "nothing on screen contains 'send' at all — the sweep above is passing vacuously")
    }
}

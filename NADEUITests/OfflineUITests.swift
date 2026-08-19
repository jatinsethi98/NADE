//
//  OfflineUITests.swift
//  NADEUITests
//
//  P2 A16, and `docs/PLAN.md` §iOS app's own words: "Offline acceptance:
//  relaunch with unreachable base URL + XCUITest."
//
//  This has to be a UI test. An `APIClient` test can prove that a request
//  failed; it cannot prove what the app did about it — and the failure mode
//  worth catching is a well-meaning error handler that clears the store and
//  leaves the user looking at "No mail here in the last 30 days" while their
//  mail is fine and only the Wi-Fi is not.
//

import XCTest

final class OfflineUITests: XCTestCase {

    override func setUp() {
        continueAfterFailure = false
    }

    /// `-NADESeed offline` puts the fixture world in the **live** store and
    /// points the HTTP source at a closed port, which is the one combination
    /// that produces a transport failure over real cached rows.
    func testCachedMailSurvivesAnUnreachableServerAndTheAppSaysWhy() {
        let app = XCUIApplication.nade(seed: .offline, screen: "1e")

        // The rows are still there …
        XCTAssertTrue(app.buttons["mailrow.18f2a1b3c4d5e6f7"].waitForExistence(timeout: 15),
                      "an unreachable server emptied the mail list")

        // … and the app says the connection failed rather than implying there
        // is no mail.
        let banner = app.staticTexts["maillist.problem"]
        XCTAssertTrue(banner.waitForExistence(timeout: 10),
                      "the app went quiet about a server it could not reach")
        XCTAssertEqual(banner.label, "Couldn't reach the server.")

        // The empty-state caption must **not** be what an offline user sees.
        XCTAssertFalse(app.staticTexts["maillist.caption.empty"].exists)
    }

    func testTheMailboxesScreenAlsoKeepsItsRowsWhenOffline() {
        let app = XCUIApplication.nade(seed: .offline)

        XCTAssertTrue(app.buttons["mailboxes.label.INBOX"].waitForExistence(timeout: 15))
        XCTAssertTrue(app.staticTexts["mailboxes.problem"].exists)
    }
}

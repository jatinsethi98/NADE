//
//  ApprovalLoopUITests.swift
//  NADEUITests
//
//  P5's gate, tapped through: "**feed** live, approve/skip round-trip, outbox
//  replay. ✓ XCUITest e2e green."
//
//  **What this can and cannot reach.** The seeded fixture world is a whole
//  `MailSource` substituted at the composition root, and it is *stateful* under
//  a lock — approve spends the token and moves the card, exactly as the server
//  does — so the loop a person walks is really walked here: tap, card settles,
//  badge falls, buttons gone, and a second tap is refused.
//
//  What it cannot reach is a **relaunch**: a seeded launch deletes its store
//  file first (EDGE P11, `NADEApp.Composition`), so nothing survives to be
//  replayed. Outbox durability across a kill is `OutboxDurabilityTests` and
//  `OutboxTests` — a durable row written before the request, one action per
//  card, and a 409 read as success — which are unit tests because the fact they
//  assert lives in the database rather than on the screen.
//

import XCTest

final class ApprovalLoopUITests: XCTestCase {

    override func setUp() { continueAfterFailure = false }

    private func feed() -> XCUIApplication {
        let app = XCUIApplication.nade(screen: "2a")
        XCTAssertTrue(app.staticTexts["home.date"].waitForExistence(timeout: 10))
        return app
    }

    /// The badge, read off the header. `DESIGN.md` §2a: "{new_count} NEW".
    private func badge(_ app: XCUIApplication) -> String {
        app.staticTexts.matching(NSPredicate(format: "label CONTAINS[c] 'NEW'"))
            .firstMatch.label
    }

    // MARK: - Approve

    /// The whole round trip, in the order a person sees it.
    func testApprovingACardSettlesItAndDropsTheBadge() {
        let app = feed()
        let before = badge(app)
        // **Counted, not existence-checked.** The feed already contains a
        // settled card (`c0000002` is `resolved` in the fixture world), so
        // `feed.resolved` exists *before* the tap — an assertion on its
        // existence would pass against an Approve button wired to `{}`.
        let settledBefore = app.staticTexts.matching(identifier: "feed.resolved").count
        let approvalsBefore = app.buttons.matching(identifier: "feed.approve").count

        let approve = app.buttons["Save note"].firstMatch
        XCTAssertTrue(approve.waitForHittable(timeout: 5))
        approve.tap()

        // The card settles under the finger: `API.md` §7 empties `actions` the
        // moment the token is spent, so the buttons go with it.
        XCTAssertTrue(
            app.staticTexts.matching(identifier: "feed.resolved")
                .element(boundBy: settledBefore).waitForExistence(timeout: 5),
            "exactly one more card settled"
        )
        XCTAssertEqual(app.buttons.matching(identifier: "feed.approve").count,
                       approvalsBefore - 1,
                       "a spent card must not still offer to be approved")
        XCTAssertNotEqual(badge(app), before, "the badge counts what is still new")
    }

    /// EDGE (duplicate delivery), through the UI: the second tap has nothing to
    /// tap. `API.md` §7 says a replayed token is a 409 the client treats as
    /// success — and the card that produced it is already gone.
    func testASettledCardCannotBeApprovedTwice() {
        let app = feed()
        let approve = app.buttons["Save note"].firstMatch
        XCTAssertTrue(approve.waitForHittable(timeout: 5))
        let approvalsBefore = app.buttons.matching(identifier: "feed.approve").count
        approve.tap()
        XCTAssertEqual(app.buttons.matching(identifier: "feed.approve").count,
                       approvalsBefore - 1)

        // Pull to refresh: the server's answer, not the optimistic local move,
        // is what the card shows now.
        app.swipeDown()
        XCTAssertFalse(app.buttons["Save note"].firstMatch.waitForExistence(timeout: 3),
                       "the refetched card must not come back approvable")
    }

    // MARK: - Skip

    func testSkippingACardSaysSoAndSavesNothing() {
        let app = feed()
        let skip = app.buttons["feed.skip"].firstMatch
        XCTAssertTrue(skip.waitForHittable(timeout: 5))
        // The feed holds two live approvals, so "no skip button anywhere" is
        // the wrong assertion — one card settling must not take the other's
        // buttons with it. The count is what says exactly one moved.
        let before = app.buttons.matching(identifier: "feed.skip").count
        skip.tap()

        // A settled card explains itself (`API.md` §7: `resolved_note` "is set
        // for `resolved`, `skipped` **and** `expired`"), and the skip's own
        // wording is the server's, not the app's.
        let note = app.staticTexts["feed.resolved"].firstMatch
        XCTAssertTrue(note.waitForExistence(timeout: 5))
        XCTAssertTrue(note.label.localizedCaseInsensitiveContains("nothing was saved"), note.label)
        XCTAssertEqual(app.buttons.matching(identifier: "feed.skip").count, before - 1,
                       "exactly one card settled")
    }

    // MARK: - The recipient line

    /// `backend/testdata/injection/README.md` finding 10: a draft card is
    /// contained **only** if it shows who the draft is addressed to and flags a
    /// recipient this mailbox has never written to. The body is prose a model
    /// wrote after reading somebody else's email; the recipient list is not.
    func testADraftCardNamesItsRecipient() {
        let app = feed()
        XCTAssertTrue(app.buttons["Save draft"].firstMatch.waitForExistence(timeout: 5))
        let recipients = app.staticTexts["feed.recipients"].firstMatch
        XCTAssertTrue(recipients.waitForExistence(timeout: 5),
                      "a draft card must name the address it would be saved to")
        XCTAssertTrue(recipients.label.contains("@"), recipients.label)
    }

    /// …and a note card does not, which is the other half of the same rule.
    ///
    /// `ApprovalControls` draws the row only for a live card that *has*
    /// recipients — a note has none, and a settled card is a record rather than
    /// a question. Without this, "always render the row" would pass the test
    /// above while putting an empty line under every note.
    func testANoteCardHasNoRecipientRow() {
        let app = feed()
        XCTAssertTrue(app.buttons["Save note"].firstMatch.waitForExistence(timeout: 5))
        // The feed holds exactly one live draft card, so exactly one row.
        XCTAssertEqual(app.staticTexts.matching(identifier: "feed.recipients").count, 1,
                       "a note card must not carry an empty recipient line")
    }

    // MARK: - Pagination

    /// `home.loadmore` is a real sentinel on a real screen, and until P5 the
    /// fixture feed answered every request with the whole list and no cursor —
    /// so nothing could reach it. The second page is now a thing that exists.
    func testTheFeedReachesItsSecondPage() {
        let app = feed()

        // A card whose only route onto this screen is the **second** page: the
        // feed is six cards, the fixture source pages at four, and the expired
        // approval is the oldest of the six.
        //
        // Deliberately not "page two is absent, then present": the sentinel is
        // an `onAppear`, and with four short cards it is already on screen at
        // launch — so the second page arrives immediately and a
        // before/after assertion would be racing the thing it is testing. What
        // matters is that it arrives at all, which is what the cursor is for.
        let pageTwo = app.staticTexts.matching(
            NSPredicate(format: "label CONTAINS[c] 'systems interview'")).firstMatch
        XCTAssertTrue(pageTwo.waitForExistence(timeout: 10),
                      "the load-more sentinel never spent the cursor")

        // And the boundary held: the six cards are six cards, not five plus a
        // duplicate. The keyset cursor is what guarantees it.
        let bodies = app.staticTexts.matching(identifier: "feed.body")
        var labels: [String] = []
        for index in 0..<bodies.count {
            labels.append(bodies.element(boundBy: index).label)
        }
        XCTAssertEqual(labels.count, 6, "both pages, whole: \(labels)")
        XCTAssertEqual(Set(labels).count, labels.count, "a card appeared twice: \(labels)")
    }

    // MARK: - 1f's agent card

    /// `DESIGN.md` §1f: the thread's agent card renders buttons only when its
    /// run is `pending_approval` **and** it has a `feed_item_id`, and their
    /// labels come from `GET /feed/{id}` — "Save note", "Save draft", never
    /// "Approve" and never "Send".
    func testTheThreadAgentCardOffersTheSameLocalOnlyVerbs() {
        // The Kettle thread: its newest agent card is the live `write_note`
        // approval (`thread.json`), which is the state §1f's buttons exist for.
        let app = XCUIApplication.nade(screen: "1f", thread: "18f2a1b3c4d5e6f7")
        XCTAssertTrue(app.staticTexts["thread.subject"].waitForExistence(timeout: 10))

        let approve = app.buttons["thread.approve"].firstMatch
        XCTAssertTrue(approve.waitForExistence(timeout: 5),
                      "a pending run with a card must offer its buttons")
        XCTAssertTrue(approve.label == "Save note" || approve.label == "Save draft", approve.label)
        XCTAssertFalse(app.buttons["Send"].firstMatch.exists)
        XCTAssertFalse(app.buttons["Approve"].firstMatch.exists)

        // **The same view 2a's row draws.** `ApprovalControls` renders the
        // recipient line and the buttons together, so 1f cannot have the button
        // without the control `injection/README.md` finding 10 requires — which
        // is exactly what it had: a kicker, a summary and an Approve button,
        // and nothing saying where a draft would go (D83).
        //
        // This card is a `write_note` gate, so the row is correctly absent; the
        // fixture world's only live `draft_reply` card sits on a thread
        // `thread.json` does not describe, which is why the row itself is
        // asserted on 2a. What is provable here is that the same view is what
        // draws these buttons, and that its guard holds.
        XCTAssertEqual(app.staticTexts.matching(identifier: "thread.recipients").count, 0,
                       "a note gate has no recipient to name")

        approve.tap()
        // Same rule as the feed: the server empties `actions`, so the buttons
        // go. XCTest ships `waitForNonExistence`; an extension redeclaring it
        // as a `RunLoop` busy-loop shadowed it target-wide.
        XCTAssertTrue(
            app.buttons["thread.approve"].firstMatch.waitForNonExistence(timeout: 5),
            "an approved card must not still offer to be approved"
        )
    }
}

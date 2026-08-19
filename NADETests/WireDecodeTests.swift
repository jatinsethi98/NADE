//
//  WireDecodeTests.swift
//  NADETests
//
//  P2 A3. `NADE/CRITERIA.md` §D7 of P1 said this lane owes it:
//  "`docs/contract/validate.py` validates every field against `docs/API.md`.
//  P1 has no models, so `ContractFixturesTests` checks reachability, valid JSON
//  and stream **framing** only. P2 decodes them, and that is when the shapes
//  get asserted on this side."
//
//  Two rules this file follows, both learned the hard way elsewhere in the repo:
//
//  1. **Assert values, not "no throw".** `XCTAssertNoThrow(try decode(...))`
//     passes for a model whose every field is optional and every value nil.
//     Each fixture below is checked against the number, string or ordering the
//     contract promises.
//  2. **Prove the failures too.** "Non-optional" only means something if a
//     `null` and a missing key actually throw, so both are planted into a real
//     payload rather than a hand-written one that never resembled the wire.
//

import XCTest
@testable import NADE

final class WireDecodeTests: XCTestCase {

    // MARK: - GET /me

    func testMeDecodesBothStates() throws {
        let ok = try ContractFixture.decode(WireMe.self, from: "me")
        XCTAssertEqual(ok.email, "jatinsethi98@gmail.com")
        XCTAssertEqual(ok.status, .ok)

        let dead = try ContractFixture.decode(WireMe.self, from: "me_needs_reauth")
        XCTAssertEqual(dead.status, .needsReauth)
        XCTAssertEqual(dead.email, "jatinsethi98@gmail.com",
                       "a dead credential does not forget which account it was for")
    }

    // MARK: - GET /mailboxes

    func testMailboxesDecodeInTheServersOrder() throws {
        let boxes = try ContractFixture.decode(WireMailboxes.self, from: "mailboxes").mailboxes
        XCTAssertEqual(boxes.count, 14)

        // API.md §2 fixes the system half, in this order and no other.
        XCTAssertEqual(
            boxes.prefix(8).map(\.id),
            ["INBOX", "CATEGORY_PERSONAL", "CATEGORY_UPDATES", "CATEGORY_SOCIAL",
             "CATEGORY_PROMOTIONS", "CATEGORY_FORUMS", "STARRED", "SENT"]
        )
        XCTAssertEqual(
            boxes.prefix(8).map(\.name),
            ["Inbox", "Primary", "Updates", "Social", "Promotions", "Forums", "Starred", "Sent"],
            "Gmail's own system labels are named `CATEGORY_PERSONAL`; the server maps them"
        )
        XCTAssertTrue(boxes.prefix(8).allSatisfy { $0.kind == .system })

        // …and the user half is ordered by **raw byte value of the name**, not
        // by locale collation. API.md picks byte order precisely so the two
        // sides cannot disagree on a device with a different collation.
        let userNames = boxes.dropFirst(8).map(\.name)
        XCTAssertEqual(userNames, ["Awaiting Reply", "BnbLeverage", "Notes", "Subscriptions", "To Reply", "[Notion]"])
        XCTAssertEqual(
            userNames, userNames.sorted { Array($0.utf8).lexicographicallyPrecedes(Array($1.utf8)) },
            "user labels are not in raw byte order — `[Notion]` sorts last only by byte, not by locale"
        )
        XCTAssertTrue(boxes.dropFirst(8).allSatisfy { $0.kind == .user })

        XCTAssertEqual(boxes[0].unread, 12)
        XCTAssertEqual(boxes[0].total, 201)
    }

    // MARK: - GET /mailboxes/{id}/threads

    func testThreadsPageDecodesEveryAwkwardRow() throws {
        let page = try ContractFixture.decode(WireThreadPage.self, from: "threads")
        XCTAssertEqual(page.threads.count, 5)
        XCTAssertNotNil(page.nextCursor, "a page with a successor carries a cursor")

        let first = page.threads[0]
        XCTAssertEqual(first.id, "18f2a1b3c4d5e6f7")
        XCTAssertEqual(first.subject, "Staff Product Designer at Kettle")
        XCTAssertEqual(first.fromName, "Priya Raghavan")
        XCTAssertTrue(first.unread)
        XCTAssertEqual(first.msgCount, 3)
        XCTAssertEqual(first.agentNote, "Job Search Tracker · two next steps to approve")

        // `agent_note` is genuinely nullable …
        XCTAssertNil(page.threads[2].agentNote)
        // … and `from_name` is `""` rather than null when the sender had no
        // display name. The distinction is the whole reason `MailRow` has a
        // fallback: API.md leaves that call to the UI.
        XCTAssertEqual(page.threads[4].fromName, "")
        XCTAssertEqual(page.threads[4].fromEmail, "talent@halcyon.io")

        // Sorted ts descending — the keyset cursor depends on it.
        let stamps = page.threads.map(\.ts)
        XCTAssertEqual(stamps, stamps.sorted(by: >))
    }

    func testTheLastPageAndTheEmptyPageAreBothNormal() throws {
        let last = try ContractFixture.decode(WireThreadPage.self, from: "threads_last_page")
        XCTAssertNil(last.nextCursor)
        let row = try XCTUnwrap(last.threads.first)
        // Everything a string can be empty, is. None of it is null, and a row
        // that renders nothing must still render a time.
        XCTAssertEqual(row.subject, "")
        XCTAssertEqual(row.snippet, "")
        XCTAssertEqual(row.fromName, "")
        XCTAssertEqual(row.fromEmail, "hello@nade.local")
        XCTAssertEqual(row.msgCount, 1)

        let empty = try ContractFixture.decode(WireThreadPage.self, from: "threads_empty")
        XCTAssertEqual(empty.threads, [])
        XCTAssertNil(empty.nextCursor, "an empty collection is [] plus a null cursor, never a 404")
    }

    // MARK: - GET /search shares the shape and not the order

    func testSearchDecodesThroughTheSameTypeAndIsNotDateOrdered() throws {
        let page = try ContractFixture.decode(WireThreadPage.self, from: "search")
        XCTAssertEqual(page.threads.count, 4)

        // API.md: search "comes back in **Gmail's relevance order, not date
        // order**", so the array order *is* the answer and nothing may re-sort
        // it. What a decode test can prove is that the decoder preserves the
        // order it was given, byte position for array position.
        //
        // Stated plainly because it is a real limit: `search.json` as generated
        // happens to be date-descending, so it cannot distinguish "kept Gmail's
        // order" from "sorted by date". The rule is enforced where it can be —
        // `MailStore` gives search its own ordered read rather than reusing the
        // mailbox join, and that comment lives beside the join.
        let ids = try XCTUnwrap(ContractFixture.object("search")["threads"] as? [[String: Any]])
            .map { $0["id"] as? String }
        XCTAssertEqual(page.threads.map(\.id), ids.compactMap { $0 },
                       "the decoder reordered a page it was handed")

        XCTAssertEqual(
            try ContractFixture.decode(WireThreadPage.self, from: "search_empty").threads, []
        )
    }

    // MARK: - GET /threads/{id}

    func testThreadDetailDecodesItsOrderingAndItsNullables() throws {
        let thread = try ContractFixture.decode(WireThread.self, from: "thread")
        XCTAssertEqual(thread.mailboxName, "To Reply")
        XCTAssertEqual(thread.accountEmail, "jatinsethi98@gmail.com")
        XCTAssertFalse(thread.partial)

        // Messages oldest first …
        XCTAssertEqual(thread.messages.count, 3)
        let stamps = thread.messages.map(\.ts)
        XCTAssertEqual(stamps, stamps.sorted(), "messages are ordered oldest first")

        // … `body_html` genuinely nullable, and not a rendering of the text …
        XCTAssertNil(thread.messages[1].bodyHtml)
        XCTAssertFalse(thread.messages[1].bodyText.isEmpty)
        XCTAssertNotNil(thread.messages[0].bodyHtml)

        // … attachments carry `inline`, which is what marks a `cid:` part …
        let attachments = thread.messages[0].attachments
        XCTAssertEqual(attachments.count, 2)
        XCTAssertFalse(attachments[0].inline)
        XCTAssertTrue(attachments[1].inline)
        XCTAssertEqual(attachments[0].size, 245_760)
        XCTAssertEqual(attachments[0].mime, "application/pdf")

        // … and agent cards are newest first, with a genuinely null
        // `feed_item_id` on the run that never asked for anything.
        XCTAssertEqual(thread.agentCards.map(\.status), [.pendingApproval, .done, .failed])
        XCTAssertNil(thread.agentCards[2].feedItemId)
        XCTAssertNotNil(thread.agentCards[0].feedItemId)
    }

    func testTheHtmlOnlyThreadKeepsItsUnicodeAndItsEmptyRecipients() throws {
        let thread = try ContractFixture.decode(WireThread.self, from: "thread_html_only")
        // Combining diaeresis and an astral-plane emoji, byte for byte.
        XCTAssertEqual(thread.subject, "Föhn — Ihre Bestellung ist unterwegs 📦")
        let message = try XCTUnwrap(thread.messages.first)
        // Undisclosed recipients. "· to me" must not be invented from this.
        XCTAssertEqual(message.to, [])
        XCTAssertEqual(message.cc, [])
        // Despite the fixture's name, `body_text` is non-null here too — it is
        // synthesised from the HTML, which is the primary path for 47% of mail.
        XCTAssertFalse(message.bodyText.isEmpty)
        XCTAssertNotNil(message.bodyHtml)
    }

    /// The state API.md always required clients to surface and no fixture had
    /// ever carried. See `docs/contract/thread_partial.json`.
    func testThePartialThreadIsShorterThanItsRowSays() throws {
        let thread = try ContractFixture.decode(WireThread.self, from: "thread_partial")
        XCTAssertTrue(thread.partial)
        XCTAssertEqual(thread.messages.count, 1)

        // The row for the same thread counts two. Neither number is derived
        // from the other, and this is the fixture where that matters.
        let row = try XCTUnwrap(
            ContractFixture.decode(WireThreadPage.self, from: "search").threads
                .first { $0.id == thread.id }
        )
        XCTAssertEqual(row.msgCount, 2)
        XCTAssertLessThan(thread.messages.count, row.msgCount)

        // The only fixture anywhere that renders DESIGN.md's `expired` status.
        XCTAssertEqual(thread.agentCards.map(\.status), [.expired])
    }

    // MARK: - Auth and health

    func testAuthShapesDecode() throws {
        let pair = try ContractFixture.decode(WirePairResponse.self, from: "pair")
        XCTAssertTrue(pair.token.hasPrefix("nade_"))
        XCTAssertEqual(pair.token.count, 69, "API.md §0: `nade_` + 64 hex = 69 characters")

        let link = try ContractFixture.decode(WireGmailLink.self, from: "gmail_link")
        XCTAssertTrue(link.url.contains("/v1/auth/gmail/start?cap="))
        XCTAssertEqual(WireTime.formatter.string(from: link.expiresAt), "2026-08-16T09:22:04Z")

        XCTAssertEqual(try ContractFixture.decode(WireHealth.self, from: "healthz").status, "ok")
        XCTAssertEqual(try ContractFixture.decode(WireHealth.self, from: "healthz_db_down").db, "down")
    }

    /// Every error fixture decodes to its own code. P2 can only provoke some of
    /// them; modelling the rest now means a later phase does not discover its
    /// error path was never parsed.
    func testEveryErrorFixtureDecodesToItsCode() throws {
        let names = try ContractFixture.names(extension: "json").filter { $0.hasPrefix("error_") }
        XCTAssertEqual(names.count, 13, "the error table in API.md §0 has 13 rows")

        for name in names {
            let envelope = try ContractFixture.decode(WireErrorEnvelope.self, from: name)
            // The fixture's file name is the code, by construction.
            XCTAssertEqual(
                envelope.error.code.rawValue, String(name.dropFirst("error_".count)),
                "\(name) decodes to \(envelope.error.code.rawValue)"
            )
            if case .unknown = envelope.error.code {
                XCTFail("\(name) fell through to .unknown — the code table is missing a case")
            }
            XCTAssertFalse(envelope.error.message.isEmpty, "`message` is what the UI renders")
        }
    }

    // MARK: - What "non-optional" costs

    /// A model full of optionals decodes anything. These two plant the defect
    /// into a **real** payload, so they fail the moment a field is loosened.
    func testANullInANonNullFieldThrows() throws {
        var object = try ContractFixture.object("thread")
        object["mailbox_name"] = NSNull()
        let data = try JSONSerialization.data(withJSONObject: object)
        XCTAssertThrowsError(
            try WireTime.decoder().decode(WireThread.self, from: data),
            "`mailbox_name` has no `|null` in API.md, so a null must not decode"
        )
    }

    func testAMissingKeyThrows() throws {
        var object = try ContractFixture.object("thread")
        object.removeValue(forKey: "partial")
        let data = try JSONSerialization.data(withJSONObject: object)
        XCTAssertThrowsError(
            try WireTime.decoder().decode(WireThread.self, from: data),
            "API.md §0: \"Nothing is ever omitted\" — a missing key is a broken contract, not a default"
        )
    }

    /// The other direction, and the one that keeps the client shippable: a
    /// server that grows a field must not break a client that predates it.
    func testAnUnknownExtraKeyDecodesFine() throws {
        var object = try ContractFixture.object("thread")
        object["muted"] = true
        let data = try JSONSerialization.data(withJSONObject: object)
        XCTAssertNoThrow(try WireTime.decoder().decode(WireThread.self, from: data))
    }

    /// Forward compatibility on the enums, which is what stops one new server
    /// value blanking a whole screen. See `WireEnum` and P18.
    func testAnUnknownEnumValueSurvivesInsteadOfThrowing() throws {
        var object = try ContractFixture.object("mailboxes")
        var boxes = try XCTUnwrap(object["mailboxes"] as? [[String: Any]])
        boxes[0]["kind"] = "smart"
        object["mailboxes"] = boxes
        let data = try JSONSerialization.data(withJSONObject: object)

        let decoded = try WireTime.decoder().decode(WireMailboxes.self, from: data)
        XCTAssertEqual(decoded.mailboxes[0].kind, .unknown("smart"))
        XCTAssertEqual(decoded.mailboxes[0].kind.rawValue, "smart", "the raw value survives for the store")
        XCTAssertEqual(decoded.mailboxes.count, 14, "one unknown value must not cost the other thirteen rows")
    }

    // MARK: - Round trip

    /// A dropped field decodes cleanly and is invisible until something needs
    /// it. Re-encoding the model and comparing against the fixture as objects
    /// (so key order cannot matter) is what makes an omission fail here rather
    /// than at P5.
    /// Driven off the directory, not a hand-written list.
    ///
    /// The point of this test is that *a dropped field is invisible until
    /// something needs it* — and a hand-listed set has exactly that property
    /// about its own coverage: a new fixture is silently exempt from the only
    /// test that catches a missing `CodingKey`.
    func testEveryMailFixtureRoundTripsWithoutLosingAField() throws {
        let shapes: [(prefix: String, roundTrip: (String) throws -> Void)] = [
            ("mailboxes", { try self.assertRoundTrips(WireMailboxes.self, $0) }),
            ("threads", { try self.assertRoundTrips(WireThreadPage.self, $0) }),
            ("search", { try self.assertRoundTrips(WireThreadPage.self, $0) }),
            ("thread", { try self.assertRoundTrips(WireThread.self, $0) }),
            ("me", { try self.assertRoundTrips(WireMe.self, $0) }),
            ("gmail_link", { try self.assertRoundTrips(WireGmailLink.self, $0) }),
        ]

        var covered: Set<String> = []
        for name in try ContractFixture.names(extension: "json") {
            // Longest prefix wins, so `thread_partial` is a detail and
            // `threads_empty` is a page.
            guard let shape = shapes
                .filter({ name == $0.prefix || name.hasPrefix($0.prefix + "_") })
                .max(by: { $0.prefix.count < $1.prefix.count })
            else { continue }
            try shape.roundTrip(name)
            covered.insert(name)
        }

        // Non-vacuous: the sweep has to have found every mail and auth shape.
        // Twelve, not the ten the hand-written list covered — it was silently
        // missing `me_needs_reauth` and `search_empty`, which is the failure
        // mode this test exists to describe.
        XCTAssertEqual(covered.count, 12, "round-tripped \(covered.sorted())")
    }

    private func assertRoundTrips<T: Codable>(
        _ type: T.Type, _ name: String, file: StaticString = #filePath, line: UInt = #line
    ) throws {
        let decoded = try ContractFixture.decode(type, from: name)
        let reEncoded = try WireTime.encoder().encode(decoded)

        let original = try JSONSerialization.jsonObject(with: try ContractFixture.data(name))
        let round = try JSONSerialization.jsonObject(with: reEncoded)
        XCTAssertEqual(
            NSDictionary(dictionary: original as? [String: Any] ?? [:]),
            NSDictionary(dictionary: round as? [String: Any] ?? [:]),
            "\(name).json does not survive a decode/encode round trip — a field is being dropped",
            file: file, line: line
        )
    }
}

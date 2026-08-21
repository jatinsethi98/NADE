//
//  FeedActionTests.swift
//  NADETests
//
//  What the review found and this locks down: a terminal action has to *be*
//  terminal, a replay has to be a 409 rather than a second approval, and
//  invisible text is not input.
//

import XCTest
@testable import NADE

final class FeedActionTests: XCTestCase {

    private func source() -> FixtureMailSource { FixtureMailSource(isEmpty: false) }

    private func firstApprovable(_ source: FixtureMailSource) async throws -> WireFeedItem {
        let page = try await source.feed(cursor: nil)
        let item = page.items.first { $0.status == .new && $0.approvalToken != nil }
        return try XCTUnwrap(item, "the fixture feed has no approvable card")
    }

    // MARK: - Terminal means terminal

    /// The defect this replaces: approve returned a canned success and left the
    /// fixture untouched, so the outbox's own refetch restored the `new` card
    /// **with its token**, and the same approval could be replayed forever.
    func testApproveMovesTheCardAndSpendsTheToken() async throws {
        let source = source()
        let item = try await firstApprovable(source)
        _ = try await source.approve(feedItemID: item.id, approvalToken: item.approvalToken!)

        let after = try await source.feedItem(id: item.id)
        XCTAssertEqual(after.status, .resolved)
        XCTAssertNil(after.approvalToken, "a spent token must not come back")
        XCTAssertTrue(after.actions.isEmpty, "API.md §7: actions is [] once resolved")
        XCTAssertNotNil(after.resolvedNote, "a finished card explains its outcome")
    }

    func testSkipMovesTheCardToSkipped() async throws {
        let source = source()
        let item = try await firstApprovable(source)
        _ = try await source.skip(feedItemID: item.id, approvalToken: item.approvalToken!)

        let after = try await source.feedItem(id: item.id)
        XCTAssertEqual(after.status, .skipped)
        XCTAssertNil(after.approvalToken)
        XCTAssertTrue(after.actions.isEmpty)
    }

    /// `API.md` §7: replaying a consumed token is a **409**, which the client
    /// treats as success — never a second approval.
    func testReplayingASpentTokenIs409() async throws {
        let source = source()
        let item = try await firstApprovable(source)
        let token = item.approvalToken!
        _ = try await source.approve(feedItemID: item.id, approvalToken: token)

        do {
            _ = try await source.approve(feedItemID: item.id, approvalToken: token)
            XCTFail("expected token_consumed")
        } catch let failure as APIFailure {
            guard case .server(let code, let status, _, _) = failure else {
                return XCTFail("wrong shape: \(failure)")
            }
            XCTAssertEqual(code, .tokenConsumed)
            XCTAssertEqual(status, 409)
        }
        // And the outcome of the *first* approval is untouched.
        let final = try await source.feedItem(id: item.id)
        XCTAssertEqual(final.status, .resolved)
    }

    /// The badge is the server's number, and answering a card has to move it.
    func testNewCountFallsWhenACardIsAnswered() async throws {
        let source = source()
        let before = try await source.feed(cursor: nil).newCount
        let item = try await firstApprovable(source)
        _ = try await source.approve(feedItemID: item.id, approvalToken: item.approvalToken!)
        let after = try await source.feed(cursor: nil).newCount
        XCTAssertEqual(after, before - 1)
    }

    /// `POST /feed/seen` only touches **info** cards that are still new: an
    /// approval leaves `new` by being answered, and marking one seen would clear
    /// the badge for a card that still needs a tap.
    func testSeenIgnoresApprovals() async throws {
        let source = source()
        let item = try await firstApprovable(source)
        _ = try await source.seen(ids: [item.id])

        let after = try await source.feedItem(id: item.id)
        XCTAssertEqual(after.status, .new, "an approval must not be cleared by being looked at")
        XCTAssertNotNil(after.approvalToken)
    }

    // MARK: - The fixture recompiles what the contract says it recompiles

    /// `API.md` §5: changing `nl_definition` recompiles `spec` and returns fresh
    /// spans. Carrying the old ones made every sentence edit look like it failed.
    func testEditingTheSentenceReturnsFreshSpans() async throws {
        let source = source()
        let id = "a0000001-0000-4000-8000-000000000001"
        let sentence = AgentBuilderModel.compose(when: "Stripe emails",
                                                 doing: "save the invoice as a note",
                                                 trailing: "Ask me first.")
        let updated = try await source.updateAgent(id: id, patch: AgentPatch(nlDefinition: sentence))

        XCTAssertEqual(updated.nlDefinition, sentence)
        XCTAssertEqual(updated.whenSpan, "Stripe emails")
        XCTAssertEqual(updated.doSpan, "save the invoice as a note")
        XCTAssertEqual(updated.trailing, "Ask me first.")
        XCTAssertNil(updated.compileError)
    }

    /// A sentence that is not the composed shape is a compile failure — the
    /// state 1c renders without underlines.
    func testAnUnparseableSentenceBecomesACompileFailure() async throws {
        let source = source()
        let id = "a0000001-0000-4000-8000-000000000001"
        let updated = try await source.updateAgent(
            id: id, patch: AgentPatch(nlDefinition: "do the thing with the stuff")
        )
        XCTAssertNil(updated.whenSpan)
        XCTAssertNil(updated.doSpan)
        XCTAssertNotNil(updated.compileError)
    }

    /// A patch that does not carry a sentence leaves the spans alone.
    func testAPatchWithoutASentenceKeepsTheSpans() async throws {
        let source = source()
        let id = "a0000001-0000-4000-8000-000000000001"
        let before = try await source.agent(id: id)
        let after = try await source.updateAgent(id: id, patch: AgentPatch(approvalRequired: false))
        XCTAssertEqual(after.whenSpan, before.whenSpan)
        XCTAssertEqual(after.doSpan, before.doSpan)
    }

    // MARK: - Invisible input is not input

    /// U+200B and friends are not whitespace, so trimming leaves them — and the
    /// ask field's submit button lit up for a query nobody could see.
    func testFormatCharactersCountAsBlank() {
        for blank in ["", " ", "\n\t ", "\u{200B}", "\u{200B}\u{200C}\u{FEFF}", " \u{2060} "] {
            XCTAssertTrue(blank.nadeIsBlank, "\(blank.debugDescription) should be blank")
        }
        for real in ["a", " a ", "\u{200B}a", "。", "🙂"] {
            XCTAssertFalse(real.nadeIsBlank, "\(real.debugDescription) should not be blank")
        }
    }

    func testCreatingAnAgentFromInvisibleTextIsRejected() async {
        do {
            _ = try await source().createAgent(nlDefinition: "\u{200B}\u{FEFF}")
            XCTFail("expected a bad_request")
        } catch let failure as APIFailure {
            guard case .server(let code, _, _, _) = failure else { return XCTFail("wrong shape") }
            XCTAssertEqual(code, .badRequest)
        } catch {
            XCTFail("wrong error: \(error)")
        }
    }
}

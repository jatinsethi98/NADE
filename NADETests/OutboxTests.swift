//
//  OutboxTests.swift
//  NADETests
//
//  The outbox's whole job is to make a tap survive things: a dead network, the
//  app being killed, the same tap twice, and a server that has already made up
//  its mind.
//
//  Two of these encode decisions that are easy to get backwards, and both were
//  found by reading the contract rather than the code:
//
//  - `409 token_consumed` means **somebody** won, not that *we* did. Approve
//    and skip consume the same token and several devices may be paired, so the
//    item is refetched rather than assumed.
//  - `401` is the same code for a wrong approval token **and** a revoked
//    device credential (`API.md` §0 vs §7). Dropping the row on the second
//    would discard an approval that was never successfully attempted.
//

import XCTest
@testable import NADE

final class OutboxTests: XCTestCase {

    // MARK: - The outcome table

    private func serverFailure(_ code: ErrorCode, _ status: Int) -> APIFailure {
        .server(code: code, status: status, message: "…", retryAfter: nil)
    }

    /// A request the server will never accept must be **dropped**, not retried.
    /// A row that retried for ever would pin every action queued behind it —
    /// the same failure mode the backend's history walk has for a permanently
    /// unfetchable message.
    func testAPermanentlyRejectedRequestIsDroppedNotRetried() {
        for code: ErrorCode in [.badRequest, .notFound, .forbidden, .payloadTooLarge] {
            guard case .rejected = OutboxDriver.outcome(for: serverFailure(code, 400)) else {
                return XCTFail("\(code.rawValue) should be terminal, not retried for ever")
            }
        }
    }

    /// **The rule the contract states twice.**
    func testTokenConsumedIsSuccessNotFailure() {
        XCTAssertEqual(
            OutboxDriver.outcome(for: serverFailure(.tokenConsumed, 409)),
            .alreadyRecorded,
            "409 token_consumed means an earlier attempt already won"
        )
        // **Not the same outcome**, and the two share a status precisely so a
        // client can tell them apart. `token_consumed` is a decision that
        // landed; `conflict` is one that did not, and calling it "already
        // recorded" would tell the user their tap was taken when it was not.
        XCTAssertEqual(
            OutboxDriver.outcome(for: serverFailure(.conflict, 409)),
            .superseded,
            "409 conflict means the state moved on, not that we won"
        )
    }

    func testAnExpiredApprovalIsTerminal() {
        XCTAssertEqual(OutboxDriver.outcome(for: serverFailure(.approvalExpired, 410)), .expired)
        XCTAssertEqual(OutboxDriver.outcome(for: serverFailure(.gone, 410)), .expired)
    }

    /// `401` is ambiguous, and the ambiguity is the finding: it is both "wrong
    /// approval token" and "your device credential is dead".
    func testUnauthorizedIsAmbiguousAndNotTerminal() {
        XCTAssertEqual(
            OutboxDriver.outcome(for: serverFailure(.unauthorized, 401)),
            .unauthorized,
            "401 must not discard the queued intention on a guess"
        )
    }

    func testNeedsReauthLeavesTheRowQueued() {
        XCTAssertEqual(OutboxDriver.outcome(for: serverFailure(.needsReauth, 409)), .needsReauth)
    }

    func testTransientFailuresRetry() {
        for failure: APIFailure in [
            .unreachable(.notConnectedToInternet),
            .unreachable(.timedOut),
            serverFailure(.rateLimited, 429),
            serverFailure(.internalError, 500),
            serverFailure(.upstreamUnavailable, 502),
            .malformedResponse("nonsense"),
        ] {
            guard case .retry = OutboxDriver.outcome(for: failure) else {
                return XCTFail("\(failure) should be retried")
            }
        }
    }

    /// A cancelled request is the screen going away, not a server answer.
    func testCancellationIsARetryNotAFailure() {
        guard case .retry = OutboxDriver.outcome(for: .cancelled) else {
            return XCTFail("a cancelled request must stay queued")
        }
    }

    /// Every code the contract defines maps to something. A new code added to
    /// `ErrorCode` without a decision here falls into `retry`, which is the
    /// safe default — this asserts that rather than leaving it to luck.
    func testEveryErrorCodeMapsToAnOutcome() {
        for code in ErrorCode.allKnown {
            let outcome = OutboxDriver.outcome(for: serverFailure(code, 400))
            switch code {
            case .tokenConsumed: XCTAssertEqual(outcome, .alreadyRecorded)
            case .conflict: XCTAssertEqual(outcome, .superseded)
            case .approvalExpired, .gone: XCTAssertEqual(outcome, .expired)
            case .unauthorized: XCTAssertEqual(outcome, .unauthorized)
            case .needsReauth: XCTAssertEqual(outcome, .needsReauth)
            case .badRequest, .notFound, .forbidden, .payloadTooLarge:
                guard case .rejected = outcome else {
                    return XCTFail("\(code.rawValue) should be terminal")
                }
            default:
                guard case .retry = outcome else {
                    return XCTFail("\(code.rawValue) should default to retry")
                }
            }
        }
    }

    // MARK: - The queue itself

    private func store() throws -> MailStore {
        try MailStore.inMemory()
    }

    @MainActor
    private func item(id: String = "c1", token: String? = "t1") -> WireFeedItem {
        WireFeedItem(
            id: id,
            kind: .approval,
            title: "Job Search Tracker",
            body: "Two next steps found.",
            status: .new,
            runID: "r1",
            actions: [.approve, .skip],
            approvalToken: token,
            approvalExpiresAt: Date().addingTimeInterval(7 * 24 * 3600),
            resolvedNote: nil,
            data: .writeNote(.init(
                actionLabel: "Save note",
                noteTitle: "Kettle — next steps",
                noteID: "n1",
                threadID: "t-1"
            )),
            createdAt: Date()
        )
    }

    @MainActor
    private func action(feedItemID: String = "c1", origin: String = "https://a.test") -> PendingActionRecord {
        PendingActionRecord(
            id: UUID().uuidString,
            origin: origin,
            kind: "approve",
            feedItemId: feedItemID,
            approvalToken: "t1",
            createdAt: Date(),
            attempts: 0,
            lastError: nil
        )
    }

    /// Tapping Approve twice is one intention. The server would answer the
    /// second `409 token_consumed` anyway.
    @MainActor
    func testQueueingTheSameItemTwiceKeepsOneRow() async throws {
        let store = try store()
        try await store.enqueue(action())
        try await store.enqueue(action())

        let queued = try await store.pendingActions(origin: "https://a.test")
        XCTAssertEqual(queued.count, 1)
    }

    /// **The isolation rule.** A capability minted by one server must never be
    /// presented to another.
    @MainActor
    func testTheQueueIsScopedToItsOrigin() async throws {
        let store = try store()
        try await store.enqueue(action(feedItemID: "c1", origin: "https://a.test"))
        try await store.enqueue(action(feedItemID: "c2", origin: "https://b.test"))

        let a = try await store.pendingActions(origin: "https://a.test")
        let b = try await store.pendingActions(origin: "https://b.test")
        XCTAssertEqual(a.map(\.feedItemId), ["c1"])
        XCTAssertEqual(b.map(\.feedItemId), ["c2"])
    }

    /// ...and changing servers throws the queue away entirely, because the
    /// tokens in it belong to the old one.
    @MainActor
    func testChangingServersDiscardsTheQueue() async throws {
        let store = try store()
        try await store.enqueue(action())
        let before = try await store.pendingActions(origin: "https://a.test")
        XCTAssertFalse(before.isEmpty)

        try await store.removeEverything()

        let after = try await store.pendingActions(origin: "https://a.test")
        XCTAssertTrue(after.isEmpty, "an approval capability survived a server change")
    }

    @MainActor
    func testAFailureIsCountedAndKeptRatherThanDropped() async throws {
        let store = try store()
        let queued = action()
        try await store.enqueue(queued)

        try await store.recordPendingFailure(id: queued.id, error: "offline")
        try await store.recordPendingFailure(id: queued.id, error: "offline")

        let rows = try await store.pendingActions(origin: "https://a.test")
        XCTAssertEqual(rows.count, 1, "a failure must not drop the intention")
        XCTAssertEqual(rows.first?.attempts, 2)
        XCTAssertEqual(rows.first?.lastError, "offline")
    }

    /// Resolving locally spends the token, so a second tap cannot queue a
    /// second action against an approval that is already over.
    @MainActor
    func testResolvingAnItemDropsItsTokenAndItsButtons() async throws {
        let store = try store()
        try await store.saveFeedItem(item())

        try await store.resolveFeedItem(id: "c1", status: .resolved, note: "Saved to Notes.")

        let rows = try await store.feedForTests()
        let resolved = try XCTUnwrap(rows.first)
        XCTAssertEqual(resolved.status, .resolved)
        XCTAssertNil(resolved.approvalToken, "a spent token must not survive")
        XCTAssertEqual(resolved.actions, [], "a finished card has no buttons")
        XCTAssertEqual(resolved.resolvedNote, "Saved to Notes.")
    }

    /// EDGE (empty input): an item with no token is not actionable, and asking
    /// to approve it queues nothing rather than writing a row with an empty
    /// capability.
    @MainActor
    func testAnItemWithNoTokenQueuesNothing() async throws {
        let store = try store()
        let driver = OutboxDriver(
            store: store,
            source: FixtureMailSource(isEmpty: false),
            origin: { URL(string: "https://a.test")! }
        )

        await driver.approve(item: item(token: nil))

        let queued = try await store.pendingActions(origin: "https://a.test")
        XCTAssertTrue(queued.isEmpty)
    }
}

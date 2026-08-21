//
//  OutboxDurabilityTests.swift
//  NADETests
//
//  The parts of the outbox this changeset added and did not cover: the dedupe
//  answer that stops a second tap moving the card, the explanation an expired
//  card carries, and the `Retry-After` clamp.
//
//  A review pointed out that `OutboxTests` proves the *outcome table* and the
//  fixture source proves the *server's* 409 — but nothing exercised what the
//  client does with either.
//

import XCTest
@testable import NADE

final class OutboxDurabilityTests: XCTestCase {

    private var store: MailStore!

    override func setUpWithError() throws {
        store = try MailStore.inMemory()
    }

    override func tearDown() { store = nil }

    private func action(_ kind: String, item: String, id: String = UUID().uuidString)
        -> PendingActionRecord {
        PendingActionRecord(id: id, origin: "http://localhost:8080", kind: kind,
                            feedItemId: item, approvalToken: "tok_\(item)",
                            createdAt: Date(), attempts: 0, lastError: nil)
    }

    // MARK: - The dedupe answer

    /// **Save-then-Skip.** `on conflict(feed_item_id) do nothing` always stopped
    /// the second *request* — but `enqueue` returned nothing, so the caller
    /// moved the card anyway. Tap Save then Skip quickly and the queue held the
    /// approve while the screen said "skipped": the server would save the note
    /// and the UI claimed the opposite.
    func testASecondActionForTheSameCardIsRefused() async throws {
        let first = try await store.enqueue(action("approve", item: "f1"))
        XCTAssertTrue(first, "the first tap owns the card")

        let second = try await store.enqueue(action("skip", item: "f1"))
        XCTAssertFalse(second, "the second must be told it lost, so the UI does not move")

        let queued = try await store.pendingActions(origin: "http://localhost:8080")
        XCTAssertEqual(queued.count, 1)
        XCTAssertEqual(queued.first?.kind, "approve", "the winner is the one that was queued first")
    }

    /// A different card is not a conflict.
    func testADifferentCardEnqueuesNormally() async throws {
        let first = try await store.enqueue(action("approve", item: "f1"))
        let second = try await store.enqueue(action("skip", item: "f2"))
        XCTAssertTrue(first)
        XCTAssertTrue(second)
        let queued = try await store.pendingActions(origin: "http://localhost:8080")
        XCTAssertEqual(queued.count, 2)
    }

    /// And once the winner is gone, the card is free again — which is what
    /// makes a genuine retry after a rejection possible.
    func testTheCardIsFreeAgainOnceItsActionIsRemoved() async throws {
        let first = action("approve", item: "f1")
        let queued = try await store.enqueue(first)
        XCTAssertTrue(queued)
        try await store.removePendingAction(id: first.id)
        let requeued = try await store.enqueue(action("skip", item: "f1"))
        XCTAssertTrue(requeued, "a removed action must not block the card for ever")
    }

    // MARK: - Expiry carries an explanation

    /// `API.md` §7 sets `resolved_note` on expired cards too — "the last two
    /// need it most, or they render an outcome with no explanation". The 410
    /// path used to write `note: nil`.
    func testTheExpiredFallbackNoteIsAnExplanationAndNotAnOutboundPromise() {
        let note = OutboxDriver.expiredNote
        XCTAssertFalse(note.isEmpty)
        XCTAssertNil(note.range(of: #"\bsend(s|ing)?\b"#,
                                options: [.regularExpression, .caseInsensitive]),
                     "v1 takes no outbound action, so no copy may imply one")
    }

    // MARK: - Retry-After

    /// EDGE (429 / clock skew). `Retry-After` is a duration, so it is measured
    /// on a monotonic clock — and clamped, because a hostile or broken value
    /// would otherwise strand the queue for the life of the process.
    func testRetryAfterIsClampedToAnHour() {
        XCTAssertEqual(OutboxDriver.clampRetryAfter(-5), 0, "a past window is simply open")
        XCTAssertEqual(OutboxDriver.clampRetryAfter(0), 0)
        XCTAssertEqual(OutboxDriver.clampRetryAfter(30), 30)
        XCTAssertEqual(OutboxDriver.clampRetryAfter(3600), 3600)
        XCTAssertEqual(OutboxDriver.clampRetryAfter(86_400), 3600,
                       "a day-long backoff is not a rate limit, it is a hang")

        // An infinite value is an unusable instruction, not the absence of one,
        // so it takes the **maximum**. Returning 0 would answer a server asking
        // us to back off by retrying immediately.
        XCTAssertEqual(OutboxDriver.clampRetryAfter(.infinity), 3600)
        // NaN carries no instruction at all.
        XCTAssertEqual(OutboxDriver.clampRetryAfter(.nan), 0)
    }
}

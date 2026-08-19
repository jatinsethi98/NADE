//
//  MailObservationTests.swift
//  NADETests
//
//  P2 A9.
//
//  The claim is deliberately narrow, because the obvious one is false. "An
//  unrelated write does not emit" would stay green with `.removeDuplicates()`
//  deleted: GRDB tracks the *region* a fetch touched, so a transaction on a
//  table the observation never read does not trigger a re-fetch in the first
//  place. What `removeDuplicates` actually buys is suppression of a
//  re-delivery when a write **inside** the region leaves the fetched value
//  unchanged — a re-applied page, which is this app's most common write.
//
//  So both are tested, and only the second is credited to `removeDuplicates`.
//

import XCTest
import GRDB
@testable import NADE

@MainActor
final class MailObservationTests: XCTestCase {

    private var store: MailStore!
    private let now = ContractFixture.pinnedNow

    override func setUpWithError() throws {
        store = try MailStore.inMemory()
    }

    override func tearDown() {
        store = nil
    }

    private func seed() async throws {
        try await ContractFixture.seedInbox(store, now: now)
    }

    /// `.immediate` means the first value is delivered **during** `start`, not
    /// after it. Two things follow: a screen never renders "nothing yet" for a
    /// frame before its rows, and a test does not need an expectation to prove
    /// something that is not asynchronous.
    func testTheFirstValueArrivesSynchronously() async throws {
        try await seed()

        var received: [[WireMailbox]] = []
        let token = store.observeMailboxes(
            onError: { XCTFail("\($0)") },
            onChange: { received.append($0) }
        )
        defer { token.cancel() }

        XCTAssertEqual(received.count, 1, "the first value was not delivered synchronously")
        XCTAssertEqual(received.first?.count, 14)
    }

    func testAWriteInTheRegionDeliversOnTheMainActor() async throws {
        try await seed()

        var values: [[WireThreadRow]] = []
        let delivered = expectation(description: "second value")
        let token = store.observeThreads(
            mailboxID: "INBOX",
            onError: { XCTFail("\($0)") },
            onChange: { rows in
                MainActor.assertIsolated("ValueObservation delivered off the main actor")
                values.append(rows)
                if values.count == 2 { delivered.fulfill() }
            }
        )
        defer { token.cancel() }

        XCTAssertEqual(values.first?.count, 5)

        // A genuinely different value inside the observed region.
        try await store.applyThreadPage(
            try ContractFixture.decode(WireThreadPage.self, from: "threads_last_page"),
            mailboxID: "INBOX", resetting: true, now: now
        )

        await fulfillment(of: [delivered], timeout: 2)
        XCTAssertEqual(values.last?.count, 1)
    }

    /// The claim `.removeDuplicates()` earns. Re-applying the same page writes
    /// to `thread` and `thread_mailbox` — both inside the tracked region — so
    /// GRDB re-fetches. Without the operator the view model would be handed an
    /// identical array and SwiftUI would re-render for nothing, on every
    /// refresh, forever.
    func testAValuePreservingWriteInsideTheRegionDoesNotReDeliver() async throws {
        try await seed()

        var count = 0
        let token = store.observeThreads(
            mailboxID: "INBOX",
            onError: { XCTFail("\($0)") },
            onChange: { _ in count += 1 }
        )
        defer { token.cancel() }
        XCTAssertEqual(count, 1)

        try await store.applyThreadPage(
            try ContractFixture.decode(WireThreadPage.self, from: "threads"),
            mailboxID: "INBOX", resetting: true, now: now
        )
        try await Task.sleep(for: .milliseconds(150))

        XCTAssertEqual(count, 1, "an identical value was re-delivered — removeDuplicates is not doing its job")
    }

    /// The other half of the same coin, stated so nobody credits it to
    /// `removeDuplicates`: GRDB's region tracking is what makes this true.
    func testAWriteToAnUnrelatedTableDoesNotDeliver() async throws {
        try await seed()

        var count = 0
        let token = store.observeThreads(
            mailboxID: "INBOX",
            onError: { XCTFail("\($0)") },
            onChange: { _ in count += 1 }
        )
        defer { token.cancel() }

        try await store.upsertAccount(WireMe(email: "someone@example.com", status: .ok))
        try await Task.sleep(for: .milliseconds(150))

        XCTAssertEqual(count, 1, "a write to `account` reached an observation over `thread`")
    }

    func testCancellingStopsDelivery() async throws {
        try await seed()

        var count = 0
        let token = store.observeThreads(
            mailboxID: "INBOX",
            onError: { XCTFail("\($0)") },
            onChange: { _ in count += 1 }
        )
        XCTAssertEqual(count, 1)
        token.cancel()

        try await store.applyThreadPage(
            try ContractFixture.decode(WireThreadPage.self, from: "threads_last_page"),
            mailboxID: "INBOX", resetting: true, now: now
        )
        try await Task.sleep(for: .milliseconds(150))

        XCTAssertEqual(count, 1, "a cancelled observation kept delivering")
    }

    /// The detail observation's own state machine: nil until the detail lands,
    /// then a value. The UI relies on that difference to avoid saying "no
    /// messages" before it has asked.
    func testTheThreadObservationGoesFromNilToADetail() async throws {
        try await seed()
        let detail = try ContractFixture.decode(WireThread.self, from: "thread")

        var values: [WireThread?] = []
        let arrived = expectation(description: "detail")
        let token = store.observeThread(
            id: detail.id,
            onError: { XCTFail("\($0)") },
            onChange: { value in
                values.append(value)
                if value != nil { arrived.fulfill() }
            }
        )
        defer { token.cancel() }

        XCTAssertEqual(values.count, 1)
        XCTAssertNil(values[0], "a thread whose detail has never been fetched is nil, not empty")

        try await store.applyThreadDetail(detail, now: now)
        await fulfillment(of: [arrived], timeout: 2)
        XCTAssertEqual(values.last??.messages.count, 3)
        XCTAssertEqual(values.last??.partial, false)
    }
}

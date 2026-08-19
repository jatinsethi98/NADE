//
//  MailStoreTests.swift
//  NADETests
//
//  P2 A7, A8, A15 and the store's share of the edge-case checklist.
//
//  All against an in-memory database running **the same migrator that ships**,
//  so these exercise the real schema rather than a convenient copy. Constraint
//  checks are behavioural — an insert that must fail — rather than reads of
//  `sqlite_master`, because a schema string can say `NOT NULL` while the column
//  it describes was never created.
//

import XCTest
import GRDB
@testable import NADE

final class MailStoreTests: XCTestCase {

    private var store: MailStore!
    private let now = ContractFixture.pinnedNow

    override func setUpWithError() throws {
        store = try MailStore.inMemory()
    }

    override func tearDown() {
        store = nil
    }

    // MARK: - Fixtures as input

    private func mailboxes() throws -> [WireMailbox] { try ContractFixture.mailboxes() }
    private func page(_ name: String = "threads") throws -> WireThreadPage { try ContractFixture.page(name) }
    private func detail(_ name: String = "thread") throws -> WireThread { try ContractFixture.thread(name) }
    private func seedInbox() async throws { try await ContractFixture.seedInbox(store, now: now) }

    // MARK: - A7 · the schema is the contract's

    /// Every column `API.md` marks non-null is `NOT NULL` here. Checked by
    /// trying to write a null into each one — a row the non-optional records
    /// cannot decode would end a `ValueObservation`, which blanks a whole
    /// screen rather than losing one row.
    func testEveryNonNullWireFieldIsNotNullInTheSchema() async throws {
        try await seedInbox()

        let nullable: [(String, [String])] = [
            ("thread", ["subject", "snippet", "from_name", "from_email", "ts", "unread", "msg_count"]),
            ("mailbox", ["name", "kind", "unread", "total", "position"]),
        ]

        try await store.writer.write { db in
            for (table, columns) in nullable {
                for column in columns {
                    XCTAssertThrowsError(
                        try db.execute(sql: "update \(table) set \(column) = NULL"),
                        "\(table).\(column) accepts NULL, and the contract says it is never null"
                    )
                }
            }
        }
    }

    func testForeignKeysAreEnforcedAndCascade() async throws {
        try await seedInbox()
        let threadID = try XCTUnwrap(try page().threads.first?.id)

        try await store.writer.write { db in
            // Enforced …
            XCTAssertThrowsError(
                try db.execute(
                    sql: "insert into thread_mailbox (mailbox_id, thread_id) values ('Label_nope', ?)",
                    arguments: [threadID]
                ),
                "a join row for a mailbox that does not exist was accepted"
            )
        }

        try await store.applyThreadDetail(try detail(), now: now)

        // … and cascading. Deleting the mailbox takes its join rows and its
        // cursor; deleting a thread takes its messages and cards.
        try await store.writer.write { db in
            XCTAssertGreaterThan(try MessageRecord.fetchCount(db), 0)
            try MailboxRecord.filter(Column("id") == "INBOX").deleteAll(db)
            XCTAssertEqual(try ThreadMailboxRecord.filter(Column("mailbox_id") == "INBOX").fetchCount(db), 0)
            XCTAssertEqual(try MailboxSyncRecord.filter(Column("mailbox_id") == "INBOX").fetchCount(db), 0)

            try ThreadRecord.filter(Column("id") == threadID).deleteAll(db)
            XCTAssertEqual(try MessageRecord.filter(Column("thread_id") == threadID).fetchCount(db), 0)
            XCTAssertEqual(try AgentCardRecord.filter(Column("thread_id") == threadID).fetchCount(db), 0)
        }
    }

    /// The account table is a singleton by constraint, not by convention.
    func testASecondAccountRowCannotExist() async throws {
        try await store.upsertAccount(try ContractFixture.decode(WireMe.self, from: "me"))
        try await store.writer.write { db in
            XCTAssertThrowsError(
                try db.execute(sql: "insert into account (id, email, status) values (2, 'x@y.z', 'ok')")
            )
        }
    }

    /// EDGE (P3). A throw part-way through a page must leave nothing behind,
    /// not half a list.
    func testAThrowMidWriteLeavesTheStoreUntouched() async throws {
        try await store.replaceMailboxes(try mailboxes())

        struct Boom: Error {}
        do {
            try await store.writer.write { db in
                try MailStoreTests.insertMinimalThread(db, id: "aaa", ts: self.now)
                throw Boom()
            }
            XCTFail("the write should have thrown")
        } catch is Boom {
            // expected
        }

        let count = try await store.writer.read { try ThreadRecord.fetchCount($0) }
        XCTAssertEqual(count, 0, "a failed transaction left rows behind")
    }

    // MARK: - A8 · replay, pagination, and the two-way column scoping

    /// EDGE (P4). The same page applied twice changes nothing.
    func testReapplyingAPageIsANoOp() async throws {
        try await seedInbox()
        let first = try await store.writer.read { db in
            (try ThreadRecord.fetchAll(db), try ThreadMailboxRecord.fetchAll(db))
        }

        try await store.applyThreadPage(try page(), mailboxID: "INBOX", resetting: true, now: now)
        let second = try await store.writer.read { db in
            (try ThreadRecord.fetchAll(db), try ThreadMailboxRecord.fetchAll(db))
        }

        XCTAssertEqual(first.0.count, second.0.count)
        XCTAssertEqual(Set(first.0.map(\.id)), Set(second.0.map(\.id)))
        XCTAssertEqual(Set(first.1), Set(second.1))
    }

    /// **The defect this pair exists to catch.** A whole-record upsert built
    /// from a `WireThreadRow` writes nil into `detail_*`, so a routine list
    /// refresh after opening a thread makes it "never loaded" again — the
    /// footer, the `partial` caption and every message disappear until the
    /// detail is fetched a second time. One direction alone does not see it.
    func testListRefreshPreservesDetailAndDetailWritePreservesList() async throws {
        try await seedInbox()
        try await store.applyThreadDetail(try detail(), now: now)

        let id = try detail().id
        let beforeRow = try await store.writer.read { try self.store.threadRow($0, id: id) }
        let detailBefore = try await store.writer.read { try self.store.thread($0, id: id) }
        XCTAssertNotNil(detailBefore)

        // A list refresh must not erase the detail …
        try await store.applyThreadPage(try page(), mailboxID: "INBOX", resetting: true, now: now)
        let afterRefresh = try await store.writer.read { try self.store.thread($0, id: id) }
        XCTAssertNotNil(afterRefresh, "a list refresh erased the thread's detail")
        XCTAssertEqual(afterRefresh?.messages.count, 3)
        XCTAssertEqual(afterRefresh?.mailboxName, "To Reply")

        // … and a detail write must not erase the list row.
        try await store.applyThreadDetail(try detail(), now: now)
        let afterDetail = try await store.writer.read { try self.store.threadRow($0, id: id) }
        XCTAssertEqual(afterDetail, beforeRow, "a detail write changed the list row")
    }

    /// A15 / EDGE (P16). `API.md` §2 bans a local read-marker outright: it
    /// would disagree with Gmail within minutes and leave the user with two
    /// contradictory inboxes. The schema has no column for one; this proves the
    /// detail path does not reach for the one that exists for Gmail's state.
    func testDetailWriteNeverTouchesUnread() async throws {
        try await seedInbox()
        let id = try detail().id
        let before = try await store.writer.read { try ThreadRecord.fetchOne($0, key: id)?.unread }
        XCTAssertEqual(before, true, "the fixture thread is unread — otherwise this proves nothing")

        try await store.applyThreadDetail(try detail(), now: now)

        let after = try await store.writer.read { try ThreadRecord.fetchOne($0, key: id)?.unread }
        XCTAssertEqual(after, true, "opening a thread cleared `unread`, which v1 cannot do")
    }

    /// EDGE (P5). `next_cursor IS NULL` cannot tell "never fetched" from
    /// "reached the last page", which is exactly the question load-more asks.
    func testReachedEndIsDistinctFromNeverFetched() async throws {
        try await store.replaceMailboxes(try mailboxes())

        let untouched = try await store.syncState(for: "INBOX")
        XCTAssertNil(untouched, "nothing fetched yet")

        try await store.applyThreadPage(try page(), mailboxID: "INBOX", resetting: true, now: now)
        var stateValue = try await store.syncState(for: "INBOX")
        var state = try XCTUnwrap(stateValue)
        XCTAssertFalse(state.reachedEnd)
        XCTAssertNotNil(state.lastPageAt)
        XCTAssertNotNil(state.nextCursor)

        try await store.applyThreadPage(try page("threads_last_page"), mailboxID: "INBOX",
                                        resetting: false, now: now)
        stateValue = try await store.syncState(for: "INBOX")
        state = try XCTUnwrap(stateValue)
        XCTAssertTrue(state.reachedEnd)
        XCTAssertNil(state.nextCursor)
    }

    /// EDGE (P5). A page that left before the mailbox was refreshed describes a
    /// list that no longer exists.
    func testAPageFromAnEarlierGenerationIsDiscarded() async throws {
        try await store.replaceMailboxes(try mailboxes())
        let stale = try await store.applyThreadPage(try page(), mailboxID: "INBOX",
                                                    resetting: true, now: now)

        // The user pulls the list back to the top: generation moves on.
        let fresh = try await store.applyThreadPage(try page("threads_empty"), mailboxID: "INBOX",
                                                    resetting: true, now: now)
        XCTAssertGreaterThan(fresh, stale)

        // The in-flight second page from before the refresh now lands.
        try await store.applyThreadPage(try page("threads_last_page"), mailboxID: "INBOX",
                                        resetting: false, expectedGeneration: stale, now: now)

        let rows = try await store.writer.read { try self.store.threads($0, mailboxID: "INBOX") }
        XCTAssertEqual(rows, [], "a page from a superseded read was stitched onto the current list")
    }

    /// An empty page onto a populated mailbox is only a wipe when it is a
    /// deliberate reset. Otherwise it is the tail of a list.
    func testAnEmptyFollowUpPageDoesNotWipeTheMailbox() async throws {
        try await seedInbox()
        try await store.applyThreadPage(try page("threads_empty"), mailboxID: "INBOX",
                                        resetting: false, now: now)
        let rows = try await store.writer.read { try self.store.threads($0, mailboxID: "INBOX") }
        XCTAssertEqual(rows.count, 5)
    }

    // MARK: - Ordering and replacement

    func testThreadsComeBackNewestFirstWhateverOrderTheyWentIn() async throws {
        try await store.replaceMailboxes(try mailboxes())
        var shuffled = try page()
        shuffled = WireThreadPage(threads: shuffled.threads.reversed(), nextCursor: shuffled.nextCursor)
        try await store.applyThreadPage(shuffled, mailboxID: "INBOX", resetting: true, now: now)

        let rows = try await store.writer.read { try self.store.threads($0, mailboxID: "INBOX") }
        XCTAssertEqual(rows.map(\.id), try page().threads.map(\.id))
        XCTAssertEqual(rows.map(\.ts), rows.map(\.ts).sorted(by: >))
    }

    /// `position` is data — the wire order is unreproducible from the columns.
    func testMailboxReplacementKeepsServerOrderAndDropsWhatTheServerDropped() async throws {
        try await store.replaceMailboxes(try mailboxes())
        let full = try await store.writer.read { try self.store.mailboxes($0) }
        XCTAssertEqual(full.map(\.id), try mailboxes().map(\.id))

        let fewer = Array(try mailboxes().prefix(3))
        try await store.replaceMailboxes(fewer)
        let after = try await store.writer.read { try self.store.mailboxes($0) }
        XCTAssertEqual(after.map(\.id), fewer.map(\.id), "replacement must not merge with what was there")
    }

    // MARK: - EDGE (P2) · hostile text

    /// The backend lane lost time to an embedded NUL twice. Same hazard, one
    /// layer up: SQLite's C string handling truncates at a NUL unless the value
    /// is bound as a real text blob, and a subject silently cut in half looks
    /// like a parser bug for a week.
    func testHostileTextRoundTripsByteExact() async throws {
        try await store.replaceMailboxes(try mailboxes())

        let hostile = [
            "Föhn — Ihre Bestellung ist unterwegs 📦",
            "e\u{0301}gale\u{0301}",                             // combining acutes
            "مرحبا بالعالم",                                      // RTL
            String(repeating: "A", count: 200),                  // unbroken token
            "",                                                  // and nothing at all
        ]

        let rows = hostile.enumerated().map { index, text in
            WireThreadRow(
                id: "hostile-\(index)", subject: text, snippet: text,
                fromName: text, fromEmail: "x@y.z",
                ts: now.addingTimeInterval(TimeInterval(-index)),
                unread: false, msgCount: 1, agentNote: text
            )
        }
        try await store.applyThreadPage(
            WireThreadPage(threads: rows, nextCursor: nil),
            mailboxID: "INBOX", resetting: true, now: now
        )

        let back = try await store.writer.read { try self.store.threads($0, mailboxID: "INBOX") }
        XCTAssertEqual(back.count, hostile.count)
        for row in back {
            let index = Int(row.id.dropFirst("hostile-".count))!
            XCTAssertEqual(Array(row.subject.utf8), Array(hostile[index].utf8),
                           "subject \(index) did not survive byte-exact")
            XCTAssertEqual(row.agentNote, hostile[index])
        }
    }

    /// EDGE (P2), the NUL half. The wire cannot carry one — PostgreSQL rejects
    /// `0x00` in a `text` column, so the server cannot store one and three
    /// separate backend fixes make sure it never tries. But SQLite **truncates
    /// silently** at a NUL, so a client that passed one through would turn
    /// "before␀after" into "before" and look like a parser bug for a week.
    ///
    /// So the store strips it, the way the server does, and this proves the
    /// text *after* the NUL survives rather than being cut off with it.
    func testANulIsStrippedRatherThanTruncatingTheRestOfTheString() async throws {
        try await store.replaceMailboxes(try mailboxes())
        let row = WireThreadRow(
            id: "nul", subject: "before\u{0000}after", snippet: "a\u{0000}b",
            fromName: "x\u{0000}y", fromEmail: "x@y.z", ts: now,
            unread: false, msgCount: 1, agentNote: "n\u{0000}o"
        )
        try await store.applyThreadPage(WireThreadPage(threads: [row], nextCursor: nil),
                                        mailboxID: "INBOX", resetting: true, now: now)

        let rows = try await store.writer.read { try self.store.threads($0, mailboxID: "INBOX") }
        let back = try XCTUnwrap(rows.first)
        XCTAssertEqual(back.subject, "beforeafter", "the text after the NUL was lost, not the NUL")
        XCTAssertEqual(back.snippet, "ab")
        XCTAssertEqual(back.fromName, "xy")
        XCTAssertEqual(back.agentNote, "no")
    }

    /// EDGE (P10). A file that opens cleanly and then fails to migrate is just
    /// as unusable as one that will not open — and surrounding only the open
    /// leaves it on disk to fail again on every launch, forever.
    func testAFileThatCannotBeMigratedIsDeletedAndRetried() throws {
        let location = StoreLocation.fixtures
        try location.removeFiles()
        defer { try? location.removeFiles() }

        // A database at the right path carrying a `thread` table that the
        // migration cannot create over.
        let poisoned = try location.openWriter { writer in
            try writer.write { db in
                try db.execute(sql: "create table thread (nonsense integer)")
            }
        }
        // Non-vacuous: the poisoned file really does defeat a plain migration.
        // Without this the test would pass over a file that migrated fine.
        XCTAssertThrowsError(try MailStore(writer: poisoned),
                             "the planted table did not actually break the migration")

        // Opening for real must recover rather than throw.
        let store = try MailStore.opening(location)
        try store.writer.read { db in
            XCTAssertTrue(try db.tableExists("mailbox"), "the retry did not run the migration")
        }
    }

    // MARK: - EDGE (P18) · a value from a future server

    /// A `CHECK` pinned to today's enum values would turn a forward-compatible
    /// decode into a failed transaction — the same blank screen the fallback
    /// exists to prevent, one layer down.
    func testAnUnknownEnumValuePersistsInsteadOfFailingTheWrite() async throws {
        let invented = WireMailbox(id: "Label_x", name: "Someday", kind: .unknown("smart"),
                                   unread: 0, total: 0)
        try await store.replaceMailboxes([invented])
        let back = try await store.writer.read { try self.store.mailboxes($0) }
        XCTAssertEqual(back.first?.kind, .unknown("smart"))

        try await store.upsertAccount(WireMe(email: "a@b.c", status: .unknown("suspended")))
        let me = try await store.writer.read { try self.store.account($0) }
        XCTAssertEqual(me?.status, .unknown("suspended"))
    }

    // MARK: - EDGE (P19) · msg_count is not a count of what we hold

    func testAPartialThreadKeepsTheServersCountNotOurs() async throws {
        try await store.replaceMailboxes(try mailboxes())
        let partial = try detail("thread_partial")
        let row = try XCTUnwrap(
            try ContractFixture.decode(WireThreadPage.self, from: "search").threads
                .first { $0.id == partial.id }
        )
        try await store.applyThreadPage(WireThreadPage(threads: [row], nextCursor: nil),
                                        mailboxID: "INBOX", resetting: true, now: now)
        try await store.applyThreadDetail(partial, now: now)

        let stored = try await store.writer.read { try self.store.thread($0, id: partial.id) }
        let storedRow = try await store.writer.read { try self.store.threadRow($0, id: partial.id) }
        XCTAssertEqual(stored?.messages.count, 1)
        XCTAssertEqual(storedRow?.msgCount, 2, "msg_count was overwritten with the number of rows we hold")
        XCTAssertEqual(stored?.partial, true)
    }

    // MARK: - Detail for a thread no list mentioned

    func testADetailForAnUnknownThreadIsSkippedRatherThanInvented() async throws {
        try await store.replaceMailboxes(try mailboxes())
        try await store.applyThreadDetail(try detail(), now: now)
        let count = try await store.writer.read { try ThreadRecord.fetchCount($0) }
        XCTAssertEqual(count, 0, "a detail with no list row invented one")
    }

    // MARK: - "Never fetched" is not "empty"

    /// The distinction the UI needs to avoid saying "No mail" before it has
    /// asked anything.
    func testDetailReadsNilUntilTheDetailHasLanded() async throws {
        try await seedInbox()
        let id = try detail().id
        let noDetailYet = try await store.writer.read { try self.store.thread($0, id: id) }
        XCTAssertNil(noDetailYet)
        let listRow = try await store.writer.read { try self.store.threadRow($0, id: id) }
        XCTAssertNotNil(listRow)

        try await store.applyThreadDetail(try detail(), now: now)
        let detailNow = try await store.writer.read { try self.store.thread($0, id: id) }
        XCTAssertNotNil(detailNow)
    }

    // MARK: -

    private static func insertMinimalThread(_ db: Database, id: String, ts: Date) throws {
        try db.execute(
            sql: """
                insert into thread (id, subject, snippet, from_name, from_email, ts, unread, msg_count)
                values (?, '', '', '', 'x@y.z', ?, 0, 1)
                """,
            arguments: [id, WireTime.formatter.string(from: ts)]
        )
    }
}

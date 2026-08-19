//
//  MailStore.swift
//  NADE
//
//  The cache, and the only file in the app that imports GRDB.
//
//  That boundary is not tidiness. It means (a) `NADETests` can exercise every
//  one of these paths with `@testable import NADE` alone — `@testable` exposes
//  NADE's internals but does not re-export the GRDB module — and (b) no view
//  model or networking type can grow a database dependency by accident.
//  `ModuleBoundaryTests` is what keeps it true.
//
//  Everything here is `nonisolated`. Writes run on GRDB's own writer queue;
//  reads reach the main actor through `ValueObservation`'s `@MainActor`
//  overload, which is the one place isolation is asserted.
//

import Foundation
import GRDB

nonisolated enum MailStoreError: Error, Equatable {
    /// A page arrived for a mailbox the store has never seen. The join row
    /// would violate its foreign key, and the raw SQLite error ("FOREIGN KEY
    /// constraint failed") says nothing a caller can act on. Naming it lets
    /// `MailSync` do the one thing that helps: fetch the mailbox list first.
    case unknownMailbox(String)
}

/// A live observation, with GRDB's type kept on this side of the wall.
nonisolated final class MailStoreCancellable: Sendable {
    private let inner: AnyDatabaseCancellable
    init(_ inner: AnyDatabaseCancellable) { self.inner = inner }
    func cancel() { inner.cancel() }
    deinit { inner.cancel() }
}

nonisolated final class MailStore: Sendable {

    let writer: any DatabaseWriter

    init(writer: any DatabaseWriter) throws {
        self.writer = writer
        try Schema.migrator().migrate(writer)
    }

    /// Opens and migrates, deleting and retrying once if either step fails.
    static func opening(_ location: StoreLocation) throws -> MailStore {
        MailStore(alreadyMigrated: try location.openWriter(preparing: {
            try Schema.migrator().migrate($0)
        }))
    }

    private init(alreadyMigrated writer: any DatabaseWriter) {
        self.writer = writer
    }

    /// EDGE (P10). If even the delete-and-retry fails, an in-memory database
    /// keeps the app running and honest — the screens render their error
    /// caption — rather than crashing on launch with nothing to explain it.
    static func openingOrEmpty(_ location: StoreLocation) -> MailStore {
        if let store = try? opening(location) { return store }
        // An in-memory open has no I/O to fail at.
        // swiftlint:disable:next force_try
        return try! inMemory()
    }

    /// An in-memory database running **the same migrator**, so a test
    /// exercises the schema that ships rather than a convenient copy of it.
    static func inMemory() throws -> MailStore {
        try MailStore(writer: try DatabaseQueue())
    }

    // MARK: - Writes
    //
    // Every one is a single transaction (EDGE P3) and an upsert on the wire's
    // own primary key (EDGE P4), so a replayed page changes nothing and a throw
    // part-way leaves no half-written page behind.

    func upsertAccount(_ me: WireMe) async throws {
        try await writer.write { db in
            try AccountRecord(me).upsert(db)
        }
    }

    /// Replace, not merge. A mailbox the server stopped returning has to
    /// disappear locally, and `position` has to come from this response rather
    /// than from whatever order the previous one left behind.
    func replaceMailboxes(_ boxes: [WireMailbox]) async throws {
        try await writer.write { db in
            let keep = Set(boxes.map(\.id))
            // The cascade takes `thread_mailbox` and `mailbox_sync` with it.
            for stale in try MailboxRecord.fetchAll(db) where !keep.contains(stale.id) {
                try stale.delete(db)
            }
            for (index, box) in boxes.enumerated() {
                try MailboxRecord(box, position: index).upsert(db)
            }
        }
    }

    /// The list half of a thread, plus its membership and the mailbox's cursor.
    ///
    /// **Column-scoped on purpose.** A whole-record upsert built from a
    /// `WireThreadRow` would write nil into `detail_*`, so refreshing the list
    /// after opening a thread would make it "never loaded" again — and the
    /// footer, the `partial` caption and every message would vanish until the
    /// detail was fetched a second time. `MailStoreTests` asserts both
    /// directions of that.
    ///
    /// - Parameter resetting: true for a first page. Bumps `generation`, so a
    ///   page still in flight from before the refresh is discarded (EDGE P5).
    /// - Returns: the generation this page was written under.
    @discardableResult
    func applyThreadPage(
        _ page: WireThreadPage,
        mailboxID: String,
        resetting: Bool,
        expectedGeneration: Int? = nil,
        now: Date
    ) async throws -> Int {
        try await writer.write { db in
            // Checked up front so the failure names the cause. Without it the
            // caller sees a foreign-key violation from the join insert and has
            // no way to tell "the mailbox list has not loaded yet" from a real
            // corruption.
            guard try MailboxRecord.fetchOne(db, key: mailboxID) != nil else {
                throw MailStoreError.unknownMailbox(mailboxID)
            }
            let existing = try MailboxSyncRecord.fetchOne(db, key: mailboxID)
            let generation = (existing?.generation ?? 0) + (resetting ? 1 : 0)

            // A page that left before the mailbox was refreshed describes a
            // list that no longer exists. Dropping it is the whole point of the
            // counter; stitching it on is how a list ends up with rows from two
            // different reads interleaved.
            if let expected = expectedGeneration, expected != generation {
                return generation
            }

            if resetting {
                try ThreadMailboxRecord
                    .filter(Column("mailbox_id") == mailboxID)
                    .deleteAll(db)
            }

            for row in page.threads {
                try Self.upsertThreadRow(row, in: db)
                try ThreadMailboxRecord(mailboxId: mailboxID, threadId: row.id).upsert(db)
            }

            try MailboxSyncRecord(
                mailboxId: mailboxID,
                nextCursor: page.nextCursor,
                reachedEnd: page.nextCursor == nil,
                generation: generation,
                lastPageAt: now
            ).upsert(db)

            return generation
        }
    }

    /// The detail half. Touches **no** list column, and in particular never
    /// `unread` — `docs/API.md` §2 forbids a local read-marker outright, and
    /// the schema has no column for one, so this is belt and braces on a rule
    /// that already has no way to be broken.
    func applyThreadDetail(_ thread: WireThread, now: Date) async throws {
        try await writer.write { db in
            guard try ThreadRecord.fetchOne(db, key: thread.id) != nil else {
                // A detail for a thread no list has ever mentioned. It can
                // happen — a deep link, or a search hit whose page was dropped
                // — and the honest response is to skip rather than invent the
                // list half out of the detail's first message.
                return
            }

            try db.execute(
                sql: """
                    update thread
                       set detail_mailbox_name = ?, detail_account_email = ?,
                           detail_partial = ?, detail_fetched_at = ?
                     where id = ?
                    """,
                arguments: [
                    thread.mailboxName, thread.accountEmail,
                    thread.partial, WireTime.formatter.string(from: now),
                    thread.id,
                ]
            )

            // Messages and cards are replaced wholesale: the server's array is
            // the answer, and a message that left the window should leave here.
            try MessageRecord.filter(Column("thread_id") == thread.id).deleteAll(db)
            for (index, message) in thread.messages.enumerated() {
                try MessageRecord(message, threadId: thread.id, position: index).insert(db)
            }

            try AgentCardRecord.filter(Column("thread_id") == thread.id).deleteAll(db)
            for (index, card) in thread.agentCards.enumerated() {
                try AgentCardRecord(card, threadId: thread.id, position: index).insert(db)
            }
        }
    }

    /// The list columns, and only those.
    private static func upsertThreadRow(_ row: WireThreadRow, in db: Database) throws {
        try db.execute(
            sql: """
                insert into thread
                    (id, subject, snippet, from_name, from_email, ts, unread, msg_count, agent_note)
                values (?, ?, ?, ?, ?, ?, ?, ?, ?)
                on conflict(id) do update set
                    subject = excluded.subject,
                    snippet = excluded.snippet,
                    from_name = excluded.from_name,
                    from_email = excluded.from_email,
                    ts = excluded.ts,
                    unread = excluded.unread,
                    msg_count = excluded.msg_count,
                    agent_note = excluded.agent_note
                """,
            arguments: [
                row.id, row.subject.databaseSafe, row.snippet.databaseSafe,
                row.fromName.databaseSafe, row.fromEmail.databaseSafe,
                WireTime.formatter.string(from: row.ts), row.unread, row.msgCount,
                row.agentNote?.databaseSafe,
            ]
        )
    }

    /// Whether `GET /threads/{id}`'s answer is actually stored — which is not
    /// the same as "the write did not throw". `applyThreadDetail` skips a
    /// thread no list row mentions, and the caller needs to know that happened.
    func hasDetail(id: String) async throws -> Bool {
        try await writer.read { db in try self.thread(db, id: id) != nil }
    }

    /// Whether a thread's **list row** exists — the prerequisite for storing
    /// its detail, since `applyThreadDetail` refuses to invent one.
    func hasThreadRow(id: String) async throws -> Bool {
        try await writer.read { db in try ThreadRecord.fetchOne(db, key: id) != nil }
    }

    /// Whether there is anything cached to show. Asked when the network fails:
    /// rows already in hand are a better answer than any state that implies
    /// there are none.
    func hasMailboxes() async throws -> Bool {
        try await writer.read { db in try MailboxRecord.fetchCount(db) > 0 }
    }

    func syncState(for mailboxID: String) async throws -> MailboxSyncRecord? {
        try await writer.read { db in try MailboxSyncRecord.fetchOne(db, key: mailboxID) }
    }

    /// EDGE (P11): a `-NADESeed` launch starts from nothing, so a UI test
    /// asserting the empty state cannot be shown the previous run's mail.
    func removeEverything() async throws {
        try await writer.write { db in
            // `mailbox` cascades to the join and the cursors; `thread` cascades
            // to messages and cards.
            try MailboxRecord.deleteAll(db)
            try ThreadRecord.deleteAll(db)
            try AccountRecord.deleteAll(db)
        }
    }

    // MARK: - Reads

    func mailboxes(_ db: Database) throws -> [WireMailbox] {
        try MailboxRecord.order(Column("position")).fetchAll(db).map(\.wire)
    }

    /// Ordered `ts DESC, id DESC`, which `API.md` §2 guarantees for a mailbox
    /// page and `thread_by_ts` reproduces exactly.
    ///
    /// **`GET /search` must not reuse this.** Search comes back in Gmail's
    /// relevance order, and a search screen built on this join would silently
    /// re-rank it by date. When a later phase adds one, it needs its own
    /// ordered table.
    func threads(_ db: Database, mailboxID: String) throws -> [WireThreadRow] {
        try ThreadRecord
            .joining(required: ThreadRecord.hasMany(ThreadMailboxRecord.self,
                                                    using: ForeignKey(["thread_id"]))
                .filter(Column("mailbox_id") == mailboxID))
            .order(Column("ts").desc, Column("id").desc)
            .fetchAll(db)
            .map(\.wireRow)
    }

    func threadRow(_ db: Database, id: String) throws -> WireThreadRow? {
        try ThreadRecord.fetchOne(db, key: id)?.wireRow
    }

    /// The detail, or nil when `GET /threads/{id}` has never landed for it —
    /// which is a different state from "landed and empty" and is what stops the
    /// UI showing "no messages" before it has asked.
    func thread(_ db: Database, id: String) throws -> WireThread? {
        guard let record = try ThreadRecord.fetchOne(db, key: id),
              let mailboxName = record.detailMailboxName,
              let accountEmail = record.detailAccountEmail,
              let partial = record.detailPartial,
              record.detailFetchedAt != nil
        else { return nil }

        let messages = try MessageRecord
            .filter(Column("thread_id") == id)
            .order(Column("position"))
            .fetchAll(db)
            .map { try $0.wire }
        let cards = try AgentCardRecord
            .filter(Column("thread_id") == id)
            .order(Column("position"))
            .fetchAll(db)
            .map(\.wire)

        return WireThread(
            id: record.id, subject: record.subject,
            mailboxName: mailboxName, accountEmail: accountEmail,
            messages: messages, agentCards: cards, partial: partial
        )
    }

    func account(_ db: Database) throws -> WireMe? {
        try AccountRecord.fetchOne(db, key: AccountRecord.singletonID)?.wire
    }

    // MARK: - Observations
    //
    // GRDB 7's `@MainActor` overload delivers straight onto the main actor, so
    // there is no continuation plumbing and no `assumeIsolated` of ours.
    //
    // Every observation carries `.removeDuplicates()`. What that buys is
    // narrower than it looks and `MailObservationTests` says so: GRDB's tracked
    // *region* already means an unrelated table's write never triggers a fetch.
    // `removeDuplicates` suppresses a re-delivery when a write **inside** the
    // region leaves the fetched value unchanged — a re-applied page, which is
    // the common case here.

    @MainActor
    func observeMailboxes(
        onError: @escaping @MainActor (Error) -> Void,
        onChange: @escaping @MainActor ([WireMailbox]) -> Void
    ) -> MailStoreCancellable {
        observe({ try self.mailboxes($0) }, onError: onError, onChange: onChange)
    }

    @MainActor
    func observeThreads(
        mailboxID: String,
        onError: @escaping @MainActor (Error) -> Void,
        onChange: @escaping @MainActor ([WireThreadRow]) -> Void
    ) -> MailStoreCancellable {
        observe({ try self.threads($0, mailboxID: mailboxID) }, onError: onError, onChange: onChange)
    }

    @MainActor
    func observeThread(
        id: String,
        onError: @escaping @MainActor (Error) -> Void,
        onChange: @escaping @MainActor (WireThread?) -> Void
    ) -> MailStoreCancellable {
        observe({ try self.thread($0, id: id) }, onError: onError, onChange: onChange)
    }

    @MainActor
    func observeThreadRow(
        id: String,
        onError: @escaping @MainActor (Error) -> Void,
        onChange: @escaping @MainActor (WireThreadRow?) -> Void
    ) -> MailStoreCancellable {
        observe({ try self.threadRow($0, id: id) }, onError: onError, onChange: onChange)
    }

    @MainActor
    func observeAccount(
        onError: @escaping @MainActor (Error) -> Void,
        onChange: @escaping @MainActor (WireMe?) -> Void
    ) -> MailStoreCancellable {
        observe({ try self.account($0) }, onError: onError, onChange: onChange)
    }

    /// `.immediate` so the first value arrives synchronously: a screen that
    /// renders "nothing yet" for one frame and then its rows is a flash the
    /// design has no state for, and a test that has to wait for value #1 needs
    /// an expectation to prove something that is not actually asynchronous.
    @MainActor
    private func observe<T: Equatable & Sendable>(
        _ fetch: @escaping @Sendable (Database) throws -> T,
        onError: @escaping @MainActor (Error) -> Void,
        onChange: @escaping @MainActor (T) -> Void
    ) -> MailStoreCancellable {
        MailStoreCancellable(
            ValueObservation
                .tracking(fetch)
                .removeDuplicates()
                .start(in: writer, scheduling: .immediate, onError: onError, onChange: onChange)
        )
    }
}

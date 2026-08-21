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
    /// thread no list row mentions, and `MailSync.loadThread` asks this to turn
    /// that silent skip into 1f's problem caption.
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
            // P3's tables. `pending_action` especially: it holds approval
            // capabilities minted by the old server, and presenting one to a
            // new server is a data-isolation failure, not a stale cache.
            try FeedItemRecord.deleteAll(db)
            try FeedSyncRecord.deleteAll(db)
            try AgentRecord.deleteAll(db)
            try PendingActionRecord.deleteAll(db)
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

    // MARK: - P3: feed, agents, outbox

    /// Replace or extend the feed.
    ///
    /// - Parameter resetting: true for a first page, which bumps `generation`
    ///   and clears the list. A page still in flight across that refresh
    ///   carries the old generation and is discarded rather than stitched onto
    ///   a list it no longer belongs to — the same rule the mailbox pages use.
    @discardableResult
    func saveFeed(
        _ page: WireFeedPage,
        resetting: Bool,
        expectedGeneration: Int? = nil
    ) async throws -> Bool {
        try await writer.write { db in
            let existing = try FeedSyncRecord.fetchOne(db, key: 1)
            // The generation was stored but never compared until the review
            // pointed out that the comment promised a check the code did not
            // make. A load-more that left before a refresh and landed after it
            // would otherwise write stale items **and** a stale cursor.
            if let expectedGeneration, !resetting,
               (existing?.generation ?? 0) != expectedGeneration {
                return false
            }
            let generation = (existing?.generation ?? 0) + (resetting ? 1 : 0)
            if resetting {
                try FeedItemRecord.deleteAll(db)
            }
            for item in page.items {
                try Self.upsertFeedItem(item, in: db)
            }
            try FeedSyncRecord(
                id: 1,
                nextCursor: page.nextCursor,
                reachedEnd: page.nextCursor == nil,
                generation: generation,
                // The server's mailbox-wide count, kept rather than recomputed.
                // `GET /feed` is paginated, so counting cached rows undercounts
                // the badge the moment there is more than one page.
                newCount: page.newCount,
                lastPageAt: Date()
            )
            .save(db)
            return true
        }
    }

    /// Replace the agent list wholesale.
    ///
    /// A replace and not a merge: `GET /agents` is unpaginated (`API.md` §5), so
    /// the response *is* the list, and an agent deleted on the server has to
    /// disappear here. The one rule borrowed from `replaceMailboxes` does not
    /// apply - there is no "empty means still syncing" case for agents, because
    /// zero agents is the state every new account is in.
    func saveAgents(_ rows: [WireAgentRow]) async throws {
        try await writer.write { db in
            try AgentRecord.deleteAll(db)
            for (index, row) in rows.enumerated() {
                try AgentRecord(row, position: index).insert(db)
            }
        }
    }

    /// One agent, from the response to a write.
    ///
    /// `POST`/`PATCH /agents` return the full object, so a write does not need a
    /// `GET /agents` behind it. A new agent goes on the **end**: `API.md` §5
    /// orders the list oldest first, and `position` is what preserves that.
    func upsertAgent(_ agent: WireAgent) async throws {
        let row = WireAgentRow(id: agent.id, name: agent.name, nlDefinition: agent.nlDefinition,
                               status: agent.status, triggerSummary: agent.triggerSummary,
                               schedule: agent.schedule, lastRunAt: agent.lastRunAt,
                               approvalRequired: agent.approvalRequired)
        try await writer.write { db in
            let existing = try AgentRecord.fetchOne(db, key: agent.id)?.position
            let next = try Int.fetchOne(
                db, sql: "select coalesce(max(position) + 1, 0) from agent"
            ) ?? 0
            try AgentRecord(row, position: existing ?? next).save(db)
        }
    }

    func removeAgent(id: String) async throws {
        _ = try await writer.write { db in try AgentRecord.deleteOne(db, key: id) }
    }

    /// The feed as the screens see it, read once rather than observed. Tests
    /// only — the outbox's post-`409` reconciliation goes through
    /// `source.feedItem` + `saveFeedItem` (`OutboxDriver.refresh(feedItemID:)`),
    /// not through this.
    func feedForTests() async throws -> [WireFeedItem] {
        try await writer.read { db in try self.feed(db) }
    }

    /// `POST /feed/seen`'s local half: the rows stop being new, and the badge
    /// takes the server's own `new_count`.
    ///
    /// **One transaction**, because `observeFeed` fires per commit and each fire
    /// re-decodes the whole feed. The count comes from the response rather than
    /// from counting local rows, which would undercount as soon as the feed has
    /// more than one page.
    func markFeedItemsSeen(ids: [String], newCount: Int) async throws {
        try await writer.write { db in
            for id in ids {
                guard var item = try FeedItemRecord.fetchOne(db, key: id) else { continue }
                item.status = FeedStatus.resolved.rawValue
                try item.update(db)
            }
            if var record = try FeedSyncRecord.fetchOne(db, key: 1) {
                record.newCount = newCount
                try record.update(db)
            }
        }
    }

    /// One item, refreshed on its own — what the outbox does after a `409`,
    /// and what P6's push deep link will do.
    func saveFeedItem(_ item: WireFeedItem) async throws {
        try await writer.write { db in try Self.upsertFeedItem(item, in: db) }
    }

    /// The generation a page must still match to be accepted.
    func feedGeneration() async throws -> Int {
        try await writer.read { db in try FeedSyncRecord.fetchOne(db, key: 1)?.generation ?? 0 }
    }

    func feedCursor() async throws -> FeedSyncRecord? {
        try await writer.read { db in try FeedSyncRecord.fetchOne(db, key: 1) }
    }

    private static func upsertFeedItem(_ item: WireFeedItem, in db: Database) throws {
        try FeedItemRecord(
            id: item.id,
            kind: item.kind.rawValue,
            title: item.title.databaseSafe,
            body: item.body.databaseSafe,
            status: item.status.rawValue,
            runId: item.runID,
            actionsJson: try JSONCodec.encode(item.actions.map(\.rawValue)),
            approvalToken: item.approvalToken,
            approvalExpiresAt: item.approvalExpiresAt,
            resolvedNote: item.resolvedNote?.databaseSafe,
            dataJson: try item.data.map { try JSONCodec.encode($0) },
            createdAt: item.createdAt
        )
        .save(db)
    }

    /// Move one item to a terminal state locally, without waiting for a refresh.
    ///
    /// The token is dropped at the same time: it is spent, and a row that kept
    /// it could queue a second action against an approval that is already over.
    func resolveFeedItem(id: String, status: FeedStatus, note: String?) async throws {
        try await writer.write { db in
            // Only a card that was still `new` moves the badge, and only once.
            let wasNew = try Bool.fetchOne(
                db,
                sql: "select status = ? from feed_item where id = ?",
                arguments: [FeedStatus.new.rawValue, id]
            ) ?? false
            if wasNew {
                try db.execute(
                    sql: "update feed_sync set new_count = max(0, new_count - 1) where id = 1"
                )
            }
            try db.execute(
                sql: """
                    update feed_item
                       set status = ?, approval_token = null, actions_json = '[]',
                           resolved_note = coalesce(?, resolved_note)
                     where id = ?
                    """,
                arguments: [status.rawValue, note, id]
            )
        }
    }

    // MARK: The outbox

    /// Queue an approve or a skip.
    ///
    /// One row per feed item: tapping Approve twice is one intention, and the
    /// server answers the second attempt `409 token_consumed` regardless.
    /// - Returns: false when a pending action for that card already exists.
    ///
    /// The caller needs the answer. `on conflict do nothing` is what stops a
    /// double tap becoming two requests — but the optimistic UI move used to run
    /// regardless, so tapping Save then Skip quickly left the *first* row queued
    /// while the card said "skipped". The server would save the note and the
    /// screen would claim the opposite.
    @discardableResult
    func enqueue(_ action: PendingActionRecord) async throws -> Bool {
        try await writer.write { db in
            try db.execute(
                sql: """
                    insert into pending_action
                        (id, origin, kind, feed_item_id, approval_token, created_at, attempts, last_error)
                    values (?, ?, ?, ?, ?, ?, 0, null)
                    on conflict(feed_item_id) do nothing
                    """,
                arguments: [
                    action.id, action.origin, action.kind, action.feedItemId,
                    action.approvalToken, action.createdAt,
                ]
            )
            return db.changesCount > 0
        }
    }

    /// Everything queued for `origin`, oldest first.
    ///
    /// Scoped by origin because a row minted by another server must never be
    /// sent — `removeEverything` clears them on a deliberate change, and this
    /// filter is the belt to that braces.
    func pendingActions(origin: String) async throws -> [PendingActionRecord] {
        try await writer.read { db in
            try PendingActionRecord
                .filter(Column("origin") == origin)
                .order(Column("created_at"))
                .fetchAll(db)
        }
    }

    func removePendingAction(id: String) async throws {
        try await writer.write { db in
            _ = try PendingActionRecord.deleteOne(db, key: id)
        }
    }

    func recordPendingFailure(id: String, error: String) async throws {
        try await writer.write { db in
            try db.execute(
                sql: "update pending_action set attempts = attempts + 1, last_error = ? where id = ?",
                arguments: [error.databaseSafe, id]
            )
        }
    }

    // MARK: Reads

    /// Newest first, which is the order `API.md` §7 guarantees.
    func feed(_ db: Database) throws -> [WireFeedItem] {
        try FeedItemRecord
            .order(Column("created_at").desc, Column("id").desc)
            .fetchAll(db)
            .compactMap(\.wire)
    }

    /// The badge.
    ///
    /// The **server's** `new_count`, not a count of cached rows: `GET /feed` is
    /// paginated and the count is mailbox-wide, so counting locally undercounts
    /// the badge as soon as there is more than one page. Local terminal actions
    /// decrement it, so a tap still moves the badge without a round trip.
    func feedNewCount(_ db: Database) throws -> Int {
        try FeedSyncRecord.fetchOne(db, key: 1)?.newCount
            ?? FeedItemRecord.filter(Column("status") == FeedStatus.new.rawValue).fetchCount(db)
    }

    func agents(_ db: Database) throws -> [WireAgentRow] {
        try AgentRecord.order(Column("position")).fetchAll(db).compactMap(\.wire)
    }

    // MARK: Observations

    @MainActor
    func observeFeed(
        onError: @escaping @MainActor (Error) -> Void,
        onChange: @escaping @MainActor ([WireFeedItem]) -> Void
    ) -> MailStoreCancellable {
        observe({ try self.feed($0) }, onError: onError, onChange: onChange)
    }

    @MainActor
    func observeFeedNewCount(
        onError: @escaping @MainActor (Error) -> Void,
        onChange: @escaping @MainActor (Int) -> Void
    ) -> MailStoreCancellable {
        observe({ try self.feedNewCount($0) }, onError: onError, onChange: onChange)
    }

    @MainActor
    func observeAgents(
        onError: @escaping @MainActor (Error) -> Void,
        onChange: @escaping @MainActor ([WireAgentRow]) -> Void
    ) -> MailStoreCancellable {
        observe({ try self.agents($0) }, onError: onError, onChange: onChange)
    }

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

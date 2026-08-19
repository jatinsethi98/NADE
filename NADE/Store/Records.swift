//
//  Records.swift
//  NADE
//
//  One record per table, and the wire ⇄ row conversions.
//
//  **Every record is `nonisolated`.** The target builds with
//  `SWIFT_DEFAULT_ACTOR_ISOLATION = MainActor`, so a plain `struct` is
//  MainActor-isolated and its synthesised `init(row:)` cannot satisfy
//  `FetchableRecord`'s nonisolated requirement. These rows are read on GRDB's
//  own queue; main-actor isolation would be a lie as well as a compile error.
//

import Foundation
import GRDB

/// Column names are the wire's, so a row and a payload can be compared by eye.
nonisolated protocol MailRecord: Codable, FetchableRecord, PersistableRecord, Sendable, Hashable {}

nonisolated extension MailRecord {
    static var databaseColumnDecodingStrategy: DatabaseColumnDecodingStrategy { .convertFromSnakeCase }
    static var databaseColumnEncodingStrategy: DatabaseColumnEncodingStrategy { .convertToSnakeCase }

    /// **The same formatter the wire uses.** Three things follow, and all three
    /// are the reason not to take GRDB's default:
    ///
    /// - SQLite holds the exact bytes the server sent, so a row and a payload
    ///   can be diffed without a conversion in between;
    /// - the format is fixed-width, so `ORDER BY ts DESC` is chronological
    ///   without a function call, which is what `thread_by_ts` relies on;
    /// - a malformed timestamp is rejected on both sides by one rule rather
    ///   than two that can drift apart.
    static var databaseDateDecodingStrategy: DatabaseDateDecodingStrategy { .formatted(WireTime.formatter) }
    static var databaseDateEncodingStrategy: DatabaseDateEncodingStrategy { .formatted(WireTime.formatter) }
}

// MARK: - account

nonisolated struct AccountRecord: MailRecord {
    static let databaseTableName = "account"

    /// The singleton's only legal value; the table's CHECK enforces it.
    static let singletonID = 1

    var id: Int = AccountRecord.singletonID
    var email: String
    var status: String

    init(email: String, status: String) {
        self.email = email
        self.status = status
    }

    init(_ me: WireMe) {
        self.init(email: me.email, status: me.status.rawValue)
    }

    var wire: WireMe {
        WireMe(email: email, status: AccountStatus(rawValue: status) ?? .unknown(status))
    }
}

// MARK: - mailbox

nonisolated struct MailboxRecord: MailRecord {
    static let databaseTableName = "mailbox"

    var id: String
    var name: String
    var kind: String
    var unread: Int
    var total: Int
    var position: Int

    init(_ box: WireMailbox, position: Int) {
        self.id = box.id
        self.name = box.name.databaseSafe
        self.kind = box.kind.rawValue
        self.unread = box.unread
        self.total = box.total
        self.position = position
    }

    var wire: WireMailbox {
        WireMailbox(
            id: id, name: name,
            kind: MailboxKind(rawValue: kind) ?? .unknown(kind),
            unread: unread, total: total
        )
    }
}

// MARK: - thread

nonisolated struct ThreadRecord: MailRecord {
    static let databaseTableName = "thread"

    var id: String
    var subject: String
    var snippet: String
    var fromName: String
    var fromEmail: String
    var ts: Date
    var unread: Bool
    var msgCount: Int
    var agentNote: String?
    var detailMailboxName: String?
    var detailAccountEmail: String?
    var detailPartial: Bool?
    var detailFetchedAt: Date?

    var wireRow: WireThreadRow {
        WireThreadRow(
            id: id, subject: subject, snippet: snippet,
            fromName: fromName, fromEmail: fromEmail,
            ts: ts, unread: unread, msgCount: msgCount, agentNote: agentNote
        )
    }
}

nonisolated struct ThreadMailboxRecord: MailRecord {
    static let databaseTableName = "thread_mailbox"

    var mailboxId: String
    var threadId: String
}

// MARK: - message

nonisolated struct MessageRecord: MailRecord {
    static let databaseTableName = "message"

    var gmailId: String
    var threadId: String
    var position: Int
    var fromName: String
    var fromEmail: String
    var ts: Date
    var bodyText: String
    var bodyHtml: String?
    var toJson: String
    var ccJson: String
    var labelIdsJson: String
    var attachmentsJson: String

    init(_ message: WireMessage, threadId: String, position: Int) throws {
        self.gmailId = message.gmailId
        self.threadId = threadId
        self.position = position
        self.fromName = message.fromName.databaseSafe
        self.fromEmail = message.fromEmail.databaseSafe
        self.ts = message.ts
        self.bodyText = message.bodyText.databaseSafe
        self.bodyHtml = message.bodyHtml?.databaseSafe
        self.toJson = try JSONCodec.encode(message.to)
        self.ccJson = try JSONCodec.encode(message.cc)
        self.labelIdsJson = try JSONCodec.encode(message.labelIds)
        self.attachmentsJson = try JSONCodec.encode(message.attachments)
    }

    var wire: WireMessage {
        get throws {
            WireMessage(
                gmailId: gmailId, fromName: fromName, fromEmail: fromEmail,
                to: try JSONCodec.decode([String].self, from: toJson),
                cc: try JSONCodec.decode([String].self, from: ccJson),
                ts: ts, bodyText: bodyText, bodyHtml: bodyHtml,
                labelIds: try JSONCodec.decode([String].self, from: labelIdsJson),
                attachments: try JSONCodec.decode([WireAttachment].self, from: attachmentsJson)
            )
        }
    }
}

// MARK: - agent_card

nonisolated struct AgentCardRecord: MailRecord {
    static let databaseTableName = "agent_card"

    var runId: String
    var threadId: String
    var position: Int
    var agentName: String
    var status: String
    var summary: String
    var feedItemId: String?

    init(_ card: WireAgentCard, threadId: String, position: Int) {
        self.runId = card.runId
        self.threadId = threadId
        self.position = position
        self.agentName = card.agentName.databaseSafe
        self.status = card.status.rawValue
        self.summary = card.summary.databaseSafe
        self.feedItemId = card.feedItemId
    }

    var wire: WireAgentCard {
        WireAgentCard(
            runId: runId, agentName: agentName,
            status: RunStatus(rawValue: status) ?? .unknown(status),
            summary: summary, feedItemId: feedItemId
        )
    }
}

// MARK: - mailbox_sync

nonisolated struct MailboxSyncRecord: MailRecord {
    static let databaseTableName = "mailbox_sync"

    var mailboxId: String
    var nextCursor: String?
    var reachedEnd: Bool
    var generation: Int
    /// `nil` means never fetched — distinct from "fetched, and that was the
    /// last page", which is `reachedEnd == true`. EDGE (P5). The screens answer
    /// the same question from `MailListModel.hasFetched`, which does not need a
    /// third observation to reach it.
    var lastPageAt: Date?
}

// MARK: - Leaf arrays

/// The four leaf arrays are stored as JSON text: they are always read whole,
/// never queried and never joined, so a table apiece would buy nothing and
/// cost four more writes per message.
nonisolated enum JSONCodec {
    // Shared: four of each per message, and `MailStore.thread` maps over every
    // message on every observation fire.
    private static let encoder = JSONEncoder()
    private static let decoder = JSONDecoder()

    static func encode<T: Encodable>(_ value: T) throws -> String {
        String(decoding: try encoder.encode(value), as: UTF8.self)
    }

    static func decode<T: Decodable>(_ type: T.Type, from text: String) throws -> T {
        try decoder.decode(type, from: Data(text.utf8))
    }
}

// MARK: - Text the database can actually hold

nonisolated extension String {
    /// A `NUL` removed, because SQLite silently **truncates** at one.
    ///
    /// This should never fire. `NUL` is the bug the backend lane wrote three
    /// separate fixes for — PostgreSQL rejects `0x00` in a `text` column, so
    /// the server cannot store one and therefore cannot serve one, and
    /// `mail::parse` and `sync::store` both assert it. Defence in depth, then:
    /// if one ever does arrive, the failure mode without this is a subject
    /// silently cut in half, which reads as a parser bug for a week. Dropping
    /// the character keeps everything after it; truncating does not.
    var databaseSafe: String {
        contains("\u{0000}") ? replacingOccurrences(of: "\u{0000}", with: "") : self
    }
}

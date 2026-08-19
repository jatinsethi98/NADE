//
//  WireMail.swift
//  NADE
//
//  `docs/API.md` §2, as Swift. One type per response shape.
//
//  **Nullability is the contract's, not a guess.** API.md §0: "A field with no
//  `|null` is always present and never null. Nothing is ever omitted — the iOS
//  models can use non-optional properties wherever this document does." So the
//  optionals here are exactly four: `agent_note`, `body_html`, `feed_item_id`
//  and `next_cursor`. Everything else is non-optional, and a `null` arriving in
//  one of those places is a decode failure rather than a silently empty row.
//
//  `CodingKeys` are spelled out rather than converted. A grep for
//  `"from_email"` finds the wire name, the model and API.md in one pass, and a
//  mismatch names the key it could not find instead of a converted guess.
//

import Foundation

// MARK: - Enums

/// `API.md` §2: `system` | `user`.
nonisolated enum MailboxKind: RawRepresentable, WireEnum {
    case system
    case user
    case unknown(String)

    var rawValue: String {
        switch self {
        case .system: "system"
        case .user: "user"
        case .unknown(let raw): raw
        }
    }

    static let allKnown: [Self] = [.system, .user]

    init(unknown raw: String) { self = .unknown(raw) }
}

/// `API.md` §1: `ok` | `needs_reauth`.
nonisolated enum AccountStatus: RawRepresentable, WireEnum {
    case ok
    case needsReauth
    case unknown(String)

    var rawValue: String {
        switch self {
        case .ok: "ok"
        case .needsReauth: "needs_reauth"
        case .unknown(let raw): raw
        }
    }

    static let allKnown: [Self] = [.ok, .needsReauth]

    init(unknown raw: String) { self = .unknown(raw) }
}

/// The run lifecycle from `API.md` §6, as it reaches a thread's agent cards.
///
/// The display strings live in `ThreadAgentCard`, not here: DESIGN.md owns
/// them ("waiting on you" for `pending_approval`) and a model that carried
/// copy would put a UI decision behind a wire type.
nonisolated enum RunStatus: RawRepresentable, WireEnum {
    case queued
    case running
    case pendingApproval
    case waiting
    case done
    case failed
    case expired
    case skipped
    case unknown(String)

    var rawValue: String {
        switch self {
        case .queued: "queued"
        case .running: "running"
        case .pendingApproval: "pending_approval"
        case .waiting: "waiting"
        case .done: "done"
        case .failed: "failed"
        case .expired: "expired"
        case .skipped: "skipped"
        case .unknown(let raw): raw
        }
    }

    static let allKnown: [Self] = [.queued, .running, .pendingApproval, .waiting,
                                    .done, .failed, .expired, .skipped]

    init(unknown raw: String) { self = .unknown(raw) }
}

// MARK: - GET /mailboxes

nonisolated struct WireMailboxes: Codable, Hashable, Sendable {
    let mailboxes: [WireMailbox]
}

nonisolated struct WireMailbox: Codable, Hashable, Sendable {
    let id: String
    let name: String
    let kind: MailboxKind
    /// Threads, not messages, and **within the synced window** — API.md is
    /// explicit that the client must not present these as mailbox-wide truth.
    let unread: Int
    let total: Int
}

// MARK: - GET /mailboxes/{id}/threads  ·  GET /search

/// One type for both endpoints: `API.md` §2 says search has "the same body
/// shape as the threads list". The difference is the *order* — search comes
/// back in Gmail's relevance order and must not be re-sorted — which is a
/// caller's concern, not a shape.
nonisolated struct WireThreadPage: Codable, Hashable, Sendable {
    let threads: [WireThreadRow]
    let nextCursor: String?

    enum CodingKeys: String, CodingKey {
        case threads
        case nextCursor = "next_cursor"
    }
}

nonisolated struct WireThreadRow: Codable, Hashable, Sendable {
    let id: String
    /// `""` when the mail has none — never null.
    let subject: String
    /// ≤ 200 chars, whitespace-collapsed.
    let snippet: String
    /// The **newest** message's sender. `""` when absent; API.md leaves the
    /// fallback-to-address decision to the UI, and `MailRow` makes it.
    let fromName: String
    let fromEmail: String
    /// The newest message in the thread.
    let ts: Date
    /// True when **any** message in the thread is unread. Gmail's state, which
    /// v1 cannot change — see `MailStore`.
    let unread: Bool
    /// The server's count. Never derived from a detail's `messages.count`, and
    /// `thread_partial.json` is the fixture where the two differ.
    let msgCount: Int
    let agentNote: String?

    enum CodingKeys: String, CodingKey {
        case id, subject, snippet, ts, unread
        case fromName = "from_name"
        case fromEmail = "from_email"
        case msgCount = "msg_count"
        case agentNote = "agent_note"
    }
}

// MARK: - GET /threads/{id}

nonisolated struct WireThread: Codable, Hashable, Sendable {
    let id: String
    let subject: String
    /// The label the thread is filed under, for 1f's footer.
    let mailboxName: String
    let accountEmail: String
    /// Ordered **oldest first**.
    let messages: [WireMessage]
    /// Ordered **newest first**.
    let agentCards: [WireAgentCard]
    /// `true` ⇒ `messages` is not the whole conversation. API.md: "Clients must
    /// surface it rather than render the rows as a complete thread."
    let partial: Bool

    enum CodingKeys: String, CodingKey {
        case id, subject, messages, partial
        case mailboxName = "mailbox_name"
        case accountEmail = "account_email"
        case agentCards = "agent_cards"
    }
}

nonisolated struct WireMessage: Codable, Hashable, Sendable {
    /// The identity. API.md: "There is **no `id` field on a message**."
    let gmailId: String
    let fromName: String
    let fromEmail: String
    /// May be empty — undisclosed recipients.
    let to: [String]
    let cc: [String]
    let ts: Date
    /// Never null; may be `""`. Synthesised from the HTML when the message
    /// carried no `text/plain` part, which is 47% of real mail here.
    let bodyText: String
    /// Null when the message genuinely has no `text/html` part. **Not** a
    /// rendering of the plain text — "View original" keys off exactly this.
    let bodyHtml: String?
    let labelIds: [String]
    let attachments: [WireAttachment]

    enum CodingKeys: String, CodingKey {
        case to, cc, ts, attachments
        case gmailId = "gmail_id"
        case fromName = "from_name"
        case fromEmail = "from_email"
        case bodyText = "body_text"
        case bodyHtml = "body_html"
        case labelIds = "label_ids"
    }
}

nonisolated struct WireAttachment: Codable, Hashable, Sendable {
    let id: String
    let name: String
    let mime: String
    let size: Int
    /// Referenced by `cid:` from the HTML.
    let inline: Bool
}

nonisolated struct WireAgentCard: Codable, Hashable, Sendable {
    let runId: String
    let agentName: String
    let status: RunStatus
    let summary: String
    /// Null when the run never asked for anything — there is no card to tap
    /// through to.
    let feedItemId: String?

    enum CodingKeys: String, CodingKey {
        case status, summary
        case runId = "run_id"
        case agentName = "agent_name"
        case feedItemId = "feed_item_id"
    }
}

// MARK: - Explicit encoding
//
// Swift's synthesised `encode(to:)` uses `encodeIfPresent` for optionals, so a
// nil is **omitted** rather than written as `null`. That is the opposite of
// what `API.md` §0 promises — "Nothing is ever omitted" — and it matters for
// exactly one reason: `WireDecodeTests` re-encodes each fixture and compares it
// to the original to prove no field was silently dropped. With omission, a
// genuinely dropped nullable field and a correctly-decoded nil produce the same
// output, and the test that exists to catch the first cannot see it.
//
// So the four types with nullable fields spell their encoding out.

extension WireThreadPage {
    func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(threads, forKey: .threads)
        try c.encode(nextCursor, forKey: .nextCursor)
    }
}

extension WireThreadRow {
    func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(id, forKey: .id)
        try c.encode(subject, forKey: .subject)
        try c.encode(snippet, forKey: .snippet)
        try c.encode(fromName, forKey: .fromName)
        try c.encode(fromEmail, forKey: .fromEmail)
        try c.encode(ts, forKey: .ts)
        try c.encode(unread, forKey: .unread)
        try c.encode(msgCount, forKey: .msgCount)
        try c.encode(agentNote, forKey: .agentNote)
    }
}

extension WireMessage {
    func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(gmailId, forKey: .gmailId)
        try c.encode(fromName, forKey: .fromName)
        try c.encode(fromEmail, forKey: .fromEmail)
        try c.encode(to, forKey: .to)
        try c.encode(cc, forKey: .cc)
        try c.encode(ts, forKey: .ts)
        try c.encode(bodyText, forKey: .bodyText)
        try c.encode(bodyHtml, forKey: .bodyHtml)
        try c.encode(labelIds, forKey: .labelIds)
        try c.encode(attachments, forKey: .attachments)
    }
}

extension WireAgentCard {
    func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(runId, forKey: .runId)
        try c.encode(agentName, forKey: .agentName)
        try c.encode(status, forKey: .status)
        try c.encode(summary, forKey: .summary)
        try c.encode(feedItemId, forKey: .feedItemId)
    }
}

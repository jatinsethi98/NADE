//
//  WireFeed.swift
//  NADE
//
//  `docs/API.md` §7, as Swift.
//
//  The feed is the only place NADE asks for anything, so its shape carries more
//  rules than any other response — and every one of them is a rule the client
//  would otherwise have to infer:
//
//  - `actions` is *exactly* the buttons to render, in order. The client never
//    derives them from `kind` and `status`.
//  - the primary button's label is `data.action_label`, which says "Save note"
//    or "Save draft" and **never** "Send". v1 takes no outbound action.
//  - `approval_token` is non-null only while an approval is `new`.
//

import Foundation

// MARK: - Enums

/// `API.md` §7: `approval` | `info`.
nonisolated enum FeedKind: RawRepresentable, WireEnum {
    case approval
    case info
    case unknown(String)

    var rawValue: String {
        switch self {
        case .approval: "approval"
        case .info: "info"
        case .unknown(let raw): raw
        }
    }

    static let allKnown: [Self] = [.approval, .info]

    init(unknown raw: String) { self = .unknown(raw) }
}

/// `API.md` §7: `new` | `resolved` | `skipped` | `expired`.
nonisolated enum FeedStatus: RawRepresentable, WireEnum {
    case new
    case resolved
    case skipped
    case expired
    case unknown(String)

    var rawValue: String {
        switch self {
        case .new: "new"
        case .resolved: "resolved"
        case .skipped: "skipped"
        case .expired: "expired"
        case .unknown(let raw): raw
        }
    }

    static let allKnown: [Self] = [.new, .resolved, .skipped, .expired]

    init(unknown raw: String) { self = .unknown(raw) }
}

/// One button. `API.md` §7 lists three, in the order they are rendered.
nonisolated enum FeedAction: RawRepresentable, WireEnum {
    case approve
    case edit
    case skip
    case unknown(String)

    var rawValue: String {
        switch self {
        case .approve: "approve"
        case .edit: "edit"
        case .skip: "skip"
        case .unknown(let raw): raw
        }
    }

    static let allKnown: [Self] = [.approve, .edit, .skip]

    init(unknown raw: String) { self = .unknown(raw) }
}

// MARK: - The typed `data`

/// `API.md` §7.1 — `data` is typed by `data.action`.
///
/// An enum rather than one struct of optionals, because the three shapes have
/// almost no fields in common and a flat struct would make every one of them
/// optional — which is precisely the guesswork `actions` exists to remove.
///
/// An **unknown** action decodes to `.unrecognised` rather than throwing. A feed
/// that fails to decode is a blank home screen, and a card whose action we do
/// not understand is still worth showing: it has a title, a body and a time.
nonisolated enum WireFeedData: Codable, Hashable, Sendable {
    case writeNote(WriteNote)
    case draftReply(DraftReply)
    case none(Info)
    case unrecognised(action: String)

    nonisolated struct WriteNote: Codable, Hashable, Sendable {
        /// "Save note". Never "Send" — `validate.py` sweeps for outbound verbs.
        let actionLabel: String
        let noteTitle: String
        /// The **deterministic** `effect_id` of the step awaiting approval, so
        /// the client can deep-link straight after approving.
        let noteID: String
        let threadID: String?

        enum CodingKeys: String, CodingKey {
            case actionLabel = "action_label"
            case noteTitle = "note_title"
            case noteID = "note_id"
            case threadID = "thread_id"
        }

        /// Written explicitly, like every nullable-bearing type here. The
        /// synthesised encoder uses `encodeIfPresent`, which **omits** a nil
        /// rather than writing `null` - and `API.md` §0 says nothing is ever
        /// omitted. The round-trip test caught this one.
        func encode(to encoder: Encoder) throws {
            var container = encoder.container(keyedBy: CodingKeys.self)
            try container.encode(actionLabel, forKey: .actionLabel)
            try container.encode(noteTitle, forKey: .noteTitle)
            try container.encode(noteID, forKey: .noteID)
            try container.encode(threadID, forKey: .threadID)
        }
    }

    nonisolated struct DraftReply: Codable, Hashable, Sendable {
        /// "Save draft". The draft lives in NADE and never in Gmail.
        let actionLabel: String
        let draftID: String
        let threadID: String
        let to: [String]
        let subject: String
        /// True → the UI flags the recipient in red.
        let neverMessaged: Bool

        enum CodingKeys: String, CodingKey {
            case actionLabel = "action_label"
            case draftID = "draft_id"
            case threadID = "thread_id"
            case to
            case subject
            case neverMessaged = "never_messaged"
        }
    }

    nonisolated struct Info: Codable, Hashable, Sendable {
        let noteID: String?
        let threadID: String?

        enum CodingKeys: String, CodingKey {
            case noteID = "note_id"
            case threadID = "thread_id"
        }

        func encode(to encoder: Encoder) throws {
            var container = encoder.container(keyedBy: CodingKeys.self)
            try container.encode(noteID, forKey: .noteID)
            try container.encode(threadID, forKey: .threadID)
        }
    }

    private enum ActionKey: String, CodingKey { case action }

    init(from decoder: Decoder) throws {
        let action = try decoder.container(keyedBy: ActionKey.self).decode(String.self, forKey: .action)
        switch action {
        case "write_note": self = .writeNote(try WriteNote(from: decoder))
        case "draft_reply": self = .draftReply(try DraftReply(from: decoder))
        case "none": self = .none(try Info(from: decoder))
        default: self = .unrecognised(action: action)
        }
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: ActionKey.self)
        switch self {
        case .writeNote(let payload):
            try container.encode("write_note", forKey: .action)
            try payload.encode(to: encoder)
        case .draftReply(let payload):
            try container.encode("draft_reply", forKey: .action)
            try payload.encode(to: encoder)
        case .none(let payload):
            try container.encode("none", forKey: .action)
            try payload.encode(to: encoder)
        case .unrecognised(let action):
            try container.encode(action, forKey: .action)
        }
    }

    /// The verb the primary button shows, when there is one.
    var actionLabel: String? {
        switch self {
        case .writeNote(let payload): payload.actionLabel
        case .draftReply(let payload): payload.actionLabel
        case .none, .unrecognised: nil
        }
    }

    /// The thread this card is about, if any. `API.md` §7.1.
    var threadID: String? {
        switch self {
        case .writeNote(let payload): payload.threadID
        case .draftReply(let payload): payload.threadID
        case .none(let payload): payload.threadID
        case .unrecognised: nil
        }
    }
}

// MARK: - The item

nonisolated struct WireFeedItem: Codable, Hashable, Sendable, Identifiable {
    let id: String
    let kind: FeedKind
    /// The agent's name.
    let title: String
    let body: String
    let status: FeedStatus
    let runID: String?
    /// Exactly the buttons to render, in order. `[]` for anything resolved,
    /// skipped, expired, or `kind: "info"`.
    let actions: [FeedAction]
    /// Non-null only while `status == .new` **and** `kind == .approval`.
    let approvalToken: String?
    /// Non-null on **every** approval regardless of status, so an expired card
    /// can say *when* it expired. Null on every info item.
    let approvalExpiresAt: Date?
    /// The italic line under a finished card. Set for resolved, skipped **and**
    /// expired — the last two need it most, or they render an outcome with no
    /// explanation.
    let resolvedNote: String?
    let data: WireFeedData?
    let createdAt: Date

    enum CodingKeys: String, CodingKey {
        case id, kind, title, body, status, actions, data
        case runID = "run_id"
        case approvalToken = "approval_token"
        case approvalExpiresAt = "approval_expires_at"
        case resolvedNote = "resolved_note"
        case createdAt = "created_at"
    }

    /// Nullable fields are written explicitly so a round-trip cannot drop one.
    /// `WireDecodeTests` re-encodes every fixture and compares.
    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(id, forKey: .id)
        try container.encode(kind, forKey: .kind)
        try container.encode(title, forKey: .title)
        try container.encode(body, forKey: .body)
        try container.encode(status, forKey: .status)
        try container.encode(runID, forKey: .runID)
        try container.encode(actions, forKey: .actions)
        try container.encode(approvalToken, forKey: .approvalToken)
        try container.encode(approvalExpiresAt, forKey: .approvalExpiresAt)
        try container.encode(resolvedNote, forKey: .resolvedNote)
        try container.encode(data, forKey: .data)
        try container.encode(createdAt, forKey: .createdAt)
    }
}

nonisolated struct WireFeedPage: Codable, Hashable, Sendable {
    let items: [WireFeedItem]
    let nextCursor: String?
    /// Items with `status: "new"` — approvals *and* unseen info items. The badge.
    let newCount: Int

    enum CodingKeys: String, CodingKey {
        case items
        case nextCursor = "next_cursor"
        case newCount = "new_count"
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(items, forKey: .items)
        try container.encode(nextCursor, forKey: .nextCursor)
        try container.encode(newCount, forKey: .newCount)
    }
}

// MARK: - The action responses

nonisolated struct WireApproveResponse: Codable, Hashable, Sendable {
    let runID: String
    let status: String

    enum CodingKeys: String, CodingKey {
        case runID = "run_id"
        case status
    }
}

nonisolated struct WireSkipResponse: Codable, Hashable, Sendable {
    let status: String
}

nonisolated struct WireSeenResponse: Codable, Hashable, Sendable {
    let newCount: Int

    enum CodingKeys: String, CodingKey {
        case newCount = "new_count"
    }
}

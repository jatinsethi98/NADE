//
//  WireAsk.swift
//  NADE
//
//  `docs/API.md` §4, as Swift.
//
//  The Ask screen's state **is** the `route` event: the first frame of every
//  stream says which of three screens to draw. Every stream then ends with
//  exactly one terminal event — `done` **or** `error`, never both — and nothing
//  follows it.
//

import Foundation

/// Which of 1a's three states this stream is.
nonisolated enum AskRoute: RawRepresentable, WireEnum {
    case answer
    case results
    case agentDraft
    case unknown(String)

    var rawValue: String {
        switch self {
        case .answer: "answer"
        case .results: "results"
        case .agentDraft: "agent_draft"
        case .unknown(let raw): raw
        }
    }

    static let allKnown: [Self] = [.answer, .results, .agentDraft]

    init(unknown raw: String) { self = .unknown(raw) }
}

// MARK: - Payloads

nonisolated struct AskRoutePayload: Codable, Hashable, Sendable {
    let kind: AskRoute
}

nonisolated struct AskTokenPayload: Codable, Hashable, Sendable {
    let text: String
}

nonisolated struct AskResultsPayload: Codable, Hashable, Sendable {
    /// The threads-list shape, deliberately with **no** cursor: an SSE frame is
    /// not a page.
    let threads: [WireThreadRow]
}

/// A proposed agent. The two spans are separate fields rather than left for the
/// client to find in a sentence, because the design underlines them and makes
/// each one independently tappable.
nonisolated struct AskDraftPayload: Codable, Hashable, Sendable {
    let name: String
    let nlDefinition: String
    let whenSpan: String
    let doSpan: String
    let trailing: String?
    let tool: AgentTool
    let approvalRequired: Bool
    let status: AgentStatus

    enum CodingKeys: String, CodingKey {
        case name, trailing, tool, status
        case nlDefinition = "nl_definition"
        case whenSpan = "when_span"
        case doSpan = "do_span"
        case approvalRequired = "approval_required"
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(name, forKey: .name)
        try container.encode(nlDefinition, forKey: .nlDefinition)
        try container.encode(whenSpan, forKey: .whenSpan)
        try container.encode(doSpan, forKey: .doSpan)
        try container.encode(trailing, forKey: .trailing)
        try container.encode(tool, forKey: .tool)
        try container.encode(approvalRequired, forKey: .approvalRequired)
        try container.encode(status, forKey: .status)
    }
}

nonisolated struct AskSource: Codable, Hashable, Sendable {
    /// A source carries a **message** id and a subject, and nothing else.
    let gmailID: String
    let subject: String

    enum CodingKeys: String, CodingKey {
        case gmailID = "gmail_id"
        case subject
    }
}

nonisolated struct AskDonePayload: Codable, Hashable, Sendable {
    let sources: [AskSource]
}

nonisolated struct AskErrorPayload: Codable, Hashable, Sendable {
    let code: ErrorCode
    let message: String
}

// MARK: - The event

/// One frame. `API.md` §4 defines exactly six names.
nonisolated enum AskEvent: Hashable, Sendable {
    case route(AskRoute)
    case token(String)
    case results([WireThreadRow])
    case draft(AskDraftPayload)
    case done(sources: [AskSource])
    case error(code: ErrorCode, message: String)

    /// `done` and `error` are terminal, and a stream has exactly one.
    var isTerminal: Bool {
        switch self {
        case .done, .error: true
        case .route, .token, .results, .draft: false
        }
    }
}

// MARK: - The request

nonisolated struct AskRequest: Codable, Hashable, Sendable {
    let query: String
    let threadID: String?
    /// Forces the route instead of letting the server classify. It exists for
    /// the one button that cannot otherwise work: "Make this an agent" on the
    /// results state. `nil` — the normal case — means classify.
    let routeHint: AskRoute?

    enum CodingKeys: String, CodingKey {
        case query
        case threadID = "thread_id"
        case routeHint = "route_hint"
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(query, forKey: .query)
        try container.encode(threadID, forKey: .threadID)
        try container.encode(routeHint, forKey: .routeHint)
    }
}

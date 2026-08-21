//
//  WireAgent.swift
//  NADE
//
//  `docs/API.md` §5, as Swift.
//
//  Two shapes, on purpose: `GET /agents` returns list rows without
//  `allowed_tools`, `spec` or the spans, and `GET /agents/{id}` returns the
//  full object. Modelling them as one type with five optionals would lose the
//  distinction the contract is drawing.
//
//  `when_span` / `do_span` / `trailing` are the reason 1c can underline: the
//  sentence is **composed** from them, never parsed out of `nl_definition`.
//  They are null exactly when `spec` is null — a compile failure — and that is
//  the only state in which the builder renders plain, unstyled text.
//

import Foundation

// MARK: - Enums

/// `API.md` §5: `draft` | `published` | `paused`.
nonisolated enum AgentStatus: RawRepresentable, WireEnum {
    case draft
    case published
    case paused
    case unknown(String)

    var rawValue: String {
        switch self {
        case .draft: "draft"
        case .published: "published"
        case .paused: "paused"
        case .unknown(let raw): raw
        }
    }

    static let allKnown: [Self] = [.draft, .published, .paused]

    init(unknown raw: String) { self = .unknown(raw) }
}

/// `API.md` §5.1: `mail` | `schedule` | `manual`.
nonisolated enum TriggerKind: RawRepresentable, WireEnum {
    case mail
    case schedule
    case manual
    case unknown(String)

    var rawValue: String {
        switch self {
        case .mail: "mail"
        case .schedule: "schedule"
        case .manual: "manual"
        case .unknown(let raw): raw
        }
    }

    static let allKnown: [Self] = [.mail, .schedule, .manual]

    init(unknown raw: String) { self = .unknown(raw) }
}

/// v1's entire tool set (`API.md` §5.1). Notion, Slack and web access are
/// post-v1; the builder shows only real tools.
nonisolated enum AgentTool: RawRepresentable, WireEnum {
    case searchMail
    case readThread
    case writeNote
    case draftReply
    case unknown(String)

    var rawValue: String {
        switch self {
        case .searchMail: "search_mail"
        case .readThread: "read_thread"
        case .writeNote: "write_note"
        case .draftReply: "draft_reply"
        case .unknown(let raw): raw
        }
    }

    static let allKnown: [Self] = [.searchMail, .readThread, .writeNote, .draftReply]

    init(unknown raw: String) { self = .unknown(raw) }
}

// MARK: - The schedule

/// `API.md` §5.2. The 1d sheet maps onto this 1:1.
nonisolated struct WireSchedule: Codable, Hashable, Sendable {
    /// `day` | `week` | `month`.
    let freq: String
    /// ≥ 1.
    let interval: Int
    /// `freq == "week"` only. `[]` means "the anchor's weekday", and is what
    /// day and month send.
    let byweekday: [String]
    /// `freq == "month"` only, 1…28 or -1. v1 has no control and always sends
    /// null.
    let bymonthday: Int?
    /// 24-hour local wall clock, always `HH:MM`.
    let at: String
    /// IANA. Captured from the device at creation; no control in v1.
    let tz: String
    let ends: WireEnds
    /// Server-maintained and read-only; **ignored** if sent.
    let runsDone: Int

    enum CodingKeys: String, CodingKey {
        case freq, interval, byweekday, bymonthday, at, tz, ends
        case runsDone = "runs_done"
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(freq, forKey: .freq)
        try container.encode(interval, forKey: .interval)
        try container.encode(byweekday, forKey: .byweekday)
        try container.encode(bymonthday, forKey: .bymonthday)
        try container.encode(at, forKey: .at)
        try container.encode(tz, forKey: .tz)
        try container.encode(ends, forKey: .ends)
        try container.encode(runsDone, forKey: .runsDone)
    }
}

nonisolated struct WireEnds: Codable, Hashable, Sendable {
    /// `never` | `on` | `after`.
    let kind: String
    /// `YYYY-MM-DD`, inclusive, interpreted in the schedule's `tz`.
    let date: String?
    /// A **total since creation**, compared against `runs_done`.
    let count: Int?

    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(kind, forKey: .kind)
        try container.encode(date, forKey: .date)
        try container.encode(count, forKey: .count)
    }
}

// MARK: - The compiled spec

nonisolated struct WireSpec: Codable, Hashable, Sendable {
    let version: Int
    let trigger: WireTrigger
    let instruction: String
    let tools: [AgentTool]
    let output: WireOutput
}

nonisolated struct WireTrigger: Codable, Hashable, Sendable {
    let kind: TriggerKind
    let filters: WireFilters
    /// Only rows the deterministic filters kept ever reach the cheap model.
    let semantic: String?

    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(kind, forKey: .kind)
        try container.encode(filters, forKey: .filters)
        try container.encode(semantic, forKey: .semantic)
    }
}

/// The deterministic SQL prefilter. All optional, all ANDed.
nonisolated struct WireFilters: Codable, Hashable, Sendable {
    let fromDomains: [String]
    let fromContains: [String]
    let subjectContains: [String]
    let bodyContains: [String]
    let labelIDs: [String]
    let hasAttachment: Bool?
    let newerThanDays: Int

    enum CodingKeys: String, CodingKey {
        case fromDomains = "from_domains"
        case fromContains = "from_contains"
        case subjectContains = "subject_contains"
        case bodyContains = "body_contains"
        case labelIDs = "label_ids"
        case hasAttachment = "has_attachment"
        case newerThanDays = "newer_than_days"
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(fromDomains, forKey: .fromDomains)
        try container.encode(fromContains, forKey: .fromContains)
        try container.encode(subjectContains, forKey: .subjectContains)
        try container.encode(bodyContains, forKey: .bodyContains)
        try container.encode(labelIDs, forKey: .labelIDs)
        try container.encode(hasAttachment, forKey: .hasAttachment)
        try container.encode(newerThanDays, forKey: .newerThanDays)
    }
}

nonisolated struct WireOutput: Codable, Hashable, Sendable {
    /// `note` | `draft` | `none`.
    let kind: String
    let titleTemplate: String?

    enum CodingKeys: String, CodingKey {
        case kind
        case titleTemplate = "title_template"
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(kind, forKey: .kind)
        try container.encode(titleTemplate, forKey: .titleTemplate)
    }
}

// MARK: - The agent

/// A row of `GET /agents`. Not paginated, oldest first, so the list does not
/// reshuffle under the user every time an agent runs.
nonisolated struct WireAgentRow: Codable, Hashable, Sendable, Identifiable {
    let id: String
    let name: String
    let nlDefinition: String
    let status: AgentStatus
    /// A human string, rendered by the server.
    let triggerSummary: String
    let schedule: WireSchedule?
    let lastRunAt: Date?
    let approvalRequired: Bool

    enum CodingKeys: String, CodingKey {
        case id, name, status, schedule
        case nlDefinition = "nl_definition"
        case triggerSummary = "trigger_summary"
        case lastRunAt = "last_run_at"
        case approvalRequired = "approval_required"
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(id, forKey: .id)
        try container.encode(name, forKey: .name)
        try container.encode(nlDefinition, forKey: .nlDefinition)
        try container.encode(status, forKey: .status)
        try container.encode(triggerSummary, forKey: .triggerSummary)
        try container.encode(schedule, forKey: .schedule)
        try container.encode(lastRunAt, forKey: .lastRunAt)
        try container.encode(approvalRequired, forKey: .approvalRequired)
    }
}

nonisolated struct WireAgentList: Codable, Hashable, Sendable {
    let agents: [WireAgentRow]
}

/// The full object: a list row plus the five fields only the detail carries.
nonisolated struct WireAgent: Codable, Hashable, Sendable, Identifiable {
    let id: String
    let name: String
    let nlDefinition: String
    let status: AgentStatus
    let triggerSummary: String
    let schedule: WireSchedule?
    let lastRunAt: Date?
    let approvalRequired: Bool
    let allowedTools: [AgentTool]
    let compileError: String?
    /// Null exactly when `spec` is null. The builder underlines these two spans
    /// and makes each independently tappable.
    let whenSpan: String?
    let doSpan: String?
    let trailing: String?
    let spec: WireSpec?

    enum CodingKeys: String, CodingKey {
        case id, name, status, schedule, trailing, spec
        case nlDefinition = "nl_definition"
        case triggerSummary = "trigger_summary"
        case lastRunAt = "last_run_at"
        case approvalRequired = "approval_required"
        case allowedTools = "allowed_tools"
        case compileError = "compile_error"
        case whenSpan = "when_span"
        case doSpan = "do_span"
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(id, forKey: .id)
        try container.encode(name, forKey: .name)
        try container.encode(nlDefinition, forKey: .nlDefinition)
        try container.encode(status, forKey: .status)
        try container.encode(triggerSummary, forKey: .triggerSummary)
        try container.encode(schedule, forKey: .schedule)
        try container.encode(lastRunAt, forKey: .lastRunAt)
        try container.encode(approvalRequired, forKey: .approvalRequired)
        try container.encode(allowedTools, forKey: .allowedTools)
        try container.encode(compileError, forKey: .compileError)
        try container.encode(whenSpan, forKey: .whenSpan)
        try container.encode(doSpan, forKey: .doSpan)
        try container.encode(trailing, forKey: .trailing)
        try container.encode(spec, forKey: .spec)
    }
}

// MARK: - Writes

/// `API.md` §5: `PATCH /agents/{id}` "accepts any subset of `nl_definition`,
/// `status`, `allowed_tools`, `approval_required`, `schedule`".
///
/// A subset, so every field is optional **and unset fields must not appear on
/// the wire at all** — this is the one place in the app where `encodeIfPresent`
/// is right rather than a bug. Everywhere else `API.md` §0 forbids omitting a
/// nullable; here omission is the whole point, because sending
/// `"schedule": null` would *clear* a schedule that the caller never touched.
///
/// `runs_done` has no field here because the server ignores it if sent, and a
/// field the server ignores is a control that does nothing (`DESIGN.md` §4).
nonisolated struct AgentPatch: Codable, Hashable, Sendable {
    var nlDefinition: String?
    var status: AgentStatus?
    var allowedTools: [AgentTool]?
    var approvalRequired: Bool?
    var schedule: WireSchedule?

    enum CodingKeys: String, CodingKey {
        case status, schedule
        case nlDefinition = "nl_definition"
        case allowedTools = "allowed_tools"
        case approvalRequired = "approval_required"
    }

    /// True when the patch would send `{}`. Callers use it to skip a request
    /// that could only ever be a no-op round trip.
    var isEmpty: Bool {
        nlDefinition == nil && status == nil && allowedTools == nil
            && approvalRequired == nil && schedule == nil
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encodeIfPresent(nlDefinition, forKey: .nlDefinition)
        try container.encodeIfPresent(status, forKey: .status)
        try container.encodeIfPresent(allowedTools, forKey: .allowedTools)
        try container.encodeIfPresent(approvalRequired, forKey: .approvalRequired)
        try container.encodeIfPresent(schedule, forKey: .schedule)
    }
}

/// `POST /agents/{id}/run` → `{"run_id": "…"}`. The builder's "Run once now"
/// needs the id and nothing else; the run itself is read back through `/runs`,
/// which is P7.
nonisolated struct WireRunStarted: Codable, Hashable, Sendable {
    let runID: String

    enum CodingKeys: String, CodingKey {
        case runID = "run_id"
    }
}

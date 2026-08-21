//
//  AgentBuilderModel.swift
//  NADE
//
//  1c and 1d. One agent, loaded on open and written straight through — no
//  `ValueObservation`, because editing the sentence returns a **recompiled**
//  `spec` and the screen has to render the server's answer, not a cached echo
//  of what it sent.
//

import SwiftUI

@MainActor
@Observable
final class AgentBuilderModel {

    /// Which of the four sections is open. At most one, like the mockup.
    enum Section: Equatable, CaseIterable { case invocation, inputs, tools, settings }

    /// Which span the single-line editor is over. `whole` is the compile-failure
    /// fallback, where `DESIGN.md` §1c says a tap edits the entire sentence.
    enum Editing: Equatable { case when, doing, whole }

    private(set) var agent: WireAgent?
    /// `ConnectionProblem`, like every other screen. See `AskModel.problem`.
    private(set) var problem: ConnectionProblem?
    private(set) var isBusy = false

    var openSection: Section?
    var editing: Editing?
    var editorText = ""
    var isSchedulePresented = false
    /// The sheet edits a copy; only Save writes it back (`DESIGN.md` §1d's
    /// footer is Cancel + the summary button).
    var draftSchedule = ScheduleDraft()

    private let sync: MailSync
    private let agentID: String

    init(sync: MailSync, agentID: String) {
        self.sync = sync
        self.agentID = agentID
    }

    // MARK: - Load

    func load() async {
        do {
            agent = try await sync.agent(id: agentID)
            problem = nil
        } catch let failure as APIFailure {
            problem = ConnectionProblem(failure)
        } catch {
            problem = ConnectionProblem(message: StateCopy.storeUnreadable)
        }
    }

    // MARK: - The sentence

    /// True when the sentence renders with underlines. `API.md` §5:
    /// `when_span` / `do_span` are "null when `spec` is null", and a compile
    /// failure is the only state in which the underlines are absent.
    var isCompiled: Bool {
        agent?.whenSpan != nil && agent?.doSpan != nil
    }

    /// What the editor opens with.
    func beginEditing(_ target: Editing) {
        guard let agent else { return }
        switch target {
        case .when: editorText = agent.whenSpan ?? ""
        case .doing: editorText = agent.doSpan ?? ""
        case .whole: editorText = agent.nlDefinition
        }
        editing = target
    }

    /// Re-composes the sentence and PATCHes it, which recompiles `spec`.
    ///
    /// `DESIGN.md` §1c: "The sentence is **composed, not parsed**". Editing one
    /// span therefore rebuilds the whole `nl_definition` from the other two
    /// fields rather than trying to splice text back into the original string —
    /// which is what makes the round trip lossless.
    func commitEdit() async {
        guard let target = editing, let agent else { return }
        let text = editorText.trimmingCharacters(in: .whitespacesAndNewlines)
        // EDGE: empty *or invisible* input. An empty span would compose
        // "When , ." — a sentence that cannot compile and that the user did not
        // ask for — and a paste of zero-width characters composes something that
        // looks identical. This site used to check only `isEmpty`, a weaker rule
        // than its two siblings used.
        guard !text.nadeIsBlank else { editing = nil; return }

        let sentence: String
        switch target {
        case .whole:
            sentence = text
        case .when:
            sentence = Self.compose(when: text, doing: agent.doSpan ?? "",
                                    trailing: agent.trailing)
        case .doing:
            sentence = Self.compose(when: agent.whenSpan ?? "", doing: text,
                                    trailing: agent.trailing)
        }
        guard sentence != agent.nlDefinition else { editing = nil; return }

        // **Dismiss only once the write lands.** Closing first threw the typed
        // sentence away the moment the PATCH failed - offline, 429, timeout -
        // because reopening the editor re-seeds `editorText` from the *old*
        // agent. The edit is the user's work; it stays on screen until the
        // server has it.
        if await patch({ _ in AgentPatch(nlDefinition: sentence) }) {
            editing = nil
        }
    }

    /// Abandon the edit without writing it.
    func cancelEdit() {
        editing = nil
        problem = nil
    }

    /// `When {when_span}, {do_span}.` plus `trailing` when it is non-null —
    /// the exact string both 1c and the `/ask` draft card render.
    static func compose(when: String, doing: String, trailing: String?) -> String {
        var sentence = "When \(when), \(doing)."
        if let trailing, !trailing.isEmpty { sentence += " \(trailing)" }
        return sentence
    }

    var sentenceTrailing: String? {
        guard let trailing = agent?.trailing, !trailing.isEmpty else { return nil }
        return trailing
    }

    // MARK: - The sections

    func setStatus(_ status: AgentStatus) async {
        await patch { current in
            current.status == status ? AgentPatch() : AgentPatch(status: status)
        }
    }

    func setApprovalRequired(_ required: Bool) async {
        await patch { current in
            current.approvalRequired == required ? AgentPatch()
                : AgentPatch(approvalRequired: required)
        }
    }

    /// The toggle that proved the point: the array is derived from `current`,
    /// which is the agent as it stands **after** the previous write.
    func toggleTool(_ tool: AgentTool) async {
        await patch { current in
            var tools = current.allowedTools
            if let index = tools.firstIndex(of: tool) { tools.remove(at: index) }
            else { tools.append(tool) }
            return AgentPatch(allowedTools: tools)
        }
    }

    /// `DESIGN.md` §1c Settings: the note under the toggle. Both strings are the
    /// v1 rewrites — the mockup's "Nothing leaves your account without your tap."
    /// and "This agent will send and file on its own." describe outbound actions
    /// v1 does not take (`DESIGN.md` §4, PLAN C1/C2).
    var approvalNote: String {
        (agent?.approvalRequired ?? true)
            ? String(localized: "Nothing is saved without your tap.",
                     comment: "1c Settings, when approval is required")
            : String(localized: "This agent saves notes and drafts on its own.",
                     comment: "1c Settings, when approval is not required")
    }

    /// `{Status} · asks first | · runs alone`.
    var settingsSummary: String {
        let status = (agent?.status ?? .draft).rawValue.capitalized
        let mode = (agent?.approvalRequired ?? true)
            ? String(localized: "asks first", comment: "1c Settings summary, approval on")
            : String(localized: "runs alone", comment: "1c Settings summary, approval off")
        return "\(status) · \(mode)"
    }

    var toolsSummary: String {
        let names = (agent?.allowedTools ?? []).map(AgentToolCopy.name(for:))
        return names.isEmpty
            ? String(localized: "None", comment: "1c Tools summary when no tool is enabled")
            : names.joined(separator: ", ")
    }

    /// `Automatic | Manual | Scheduled`, from `spec.trigger.kind`.
    var invocationSummary: String {
        switch agent?.spec?.trigger.kind {
        case .mail: String(localized: "Automatic", comment: "1c Invocation summary, mail trigger")
        case .manual: String(localized: "Manual", comment: "1c Invocation summary, manual trigger")
        case .schedule: String(localized: "Scheduled", comment: "1c Invocation summary, schedule trigger")
        default: String(localized: "Not set", comment: "1c Invocation summary when the spec failed to compile")
        }
    }

    /// 1c's filter table, `v1 →` two rows rather than three.
    var fromFilter: String {
        let values = (agent?.spec?.trigger.filters.fromDomains ?? [])
            + (agent?.spec?.trigger.filters.fromContains ?? [])
        return values.isEmpty
            ? String(localized: "Anyone", comment: "1c filter table, no sender filter")
            : values.joined(separator: ", ")
    }

    var aboutFilter: String {
        var values = (agent?.spec?.trigger.filters.subjectContains ?? [])
            + (agent?.spec?.trigger.filters.bodyContains ?? [])
        if let semantic = agent?.spec?.trigger.semantic, !semantic.isEmpty { values.append(semantic) }
        return values.isEmpty
            ? String(localized: "Anything", comment: "1c filter table, no subject or body filter")
            : values.joined(separator: ", ")
    }

    // MARK: - The schedule sheet

    func presentSchedule() {
        draftSchedule = ScheduleDraft(agent?.schedule)
        isSchedulePresented = true
    }

    /// Same rule as the sentence editor: the sheet stays up until the write
    /// lands, or a failed save silently discards everything the user set and
    /// reopening re-seeds the draft from the unchanged agent.
    func saveSchedule() async {
        let draft = draftSchedule
        if await patch({ current in AgentPatch(schedule: draft.wire(existing: current.schedule)) }) {
            isSchedulePresented = false
        }
    }

    // MARK: - The footer

    func runOnce() async {
        isBusy = true
        defer { isBusy = false }
        do {
            // The run's *outcome* is `GET /runs`, which is P7. Until that
            // screen exists there is nothing to hold the id for.
            _ = try await sync.runAgent(id: agentID)
            problem = nil
        } catch let failure as APIFailure {
            problem = ConnectionProblem(failure)
        } catch {
            problem = ConnectionProblem(message: StateCopy.storeUnreadable)
        }
    }

    /// - Returns: true when the agent is gone and the modal should close.
    func delete() async -> Bool {
        isBusy = true
        defer { isBusy = false }
        do {
            try await sync.deleteAgent(id: agentID)
            return true
        } catch let failure as APIFailure {
            problem = ConnectionProblem(failure)
            return false
        } catch {
            problem = ConnectionProblem(message: StateCopy.storeUnreadable)
            return false
        }
    }

    // MARK: -

    /// Every write goes through here, and they are **serialised**.
    ///
    /// The payload is built by a closure that runs **after** the previous write
    /// has landed, not before. Ordering the sends alone was not enough and the
    /// first version of this comment was wrong about it: `toggleTool` read
    /// `agent.allowedTools` at tap time, so two taps inside one round trip both
    /// derived their array from the same pre-edit object and the second undid
    /// the first. Both taps run on `@MainActor`, and the suspension point is
    /// *inside* this method — so tap B built its payload while tap A was parked
    /// at `await`. Deriving inside the chain is what makes the second write see
    /// the result of the first.
    ///
    /// - Returns: true when the server accepted it.
    @discardableResult
    private func patch(_ build: @escaping @MainActor (WireAgent) -> AgentPatch) async -> Bool {
        let previous = writes
        writes = Task { @MainActor [weak self] in
            await previous?.value
            guard let self, let current = self.agent else { return }
            let payload = build(current)
            guard !payload.isEmpty else { return }
            await self.send(payload)
        }
        await writes?.value
        return problem == nil
    }

    /// The tail of the write chain. Held so the next patch can await it.
    private var writes: Task<Void, Never>?

    private func send(_ patch: AgentPatch) async {
        isBusy = true
        defer { isBusy = false }
        do {
            agent = try await sync.updateAgent(id: agentID, patch: patch)
            problem = nil
        } catch let failure as APIFailure {
            problem = ConnectionProblem(failure)
        } catch {
            problem = ConnectionProblem(message: StateCopy.storeUnreadable)
        }
    }
}

/// The four v1 tools, named as `DESIGN.md` §1c's parity table names them.
nonisolated enum AgentToolCopy {
    static func name(for tool: AgentTool) -> String {
        switch tool {
        case .searchMail: String(localized: "Search mail", comment: "Agent tool name")
        case .readThread: String(localized: "Read thread", comment: "Agent tool name")
        case .writeNote: String(localized: "Write note", comment: "Agent tool name")
        case .draftReply: String(localized: "Draft reply", comment: "Agent tool name")
        case .unknown(let raw): raw
        }
    }

    static func note(for tool: AgentTool) -> String {
        switch tool {
        case .searchMail: String(localized: "your synced mail", comment: "Agent tool note")
        case .readThread: String(localized: "one thread in full", comment: "Agent tool note")
        case .writeNote: String(localized: "into Notes", comment: "Agent tool note")
        case .draftReply: String(localized: "saved in NADE", comment: "Agent tool note")
        case .unknown: ""
        }
    }

    /// The order the rows are drawn in, which is the parity table's order and
    /// not `allowed_tools`' — a checkbox list that reorders as you tick is
    /// unusable.
    static let all: [AgentTool] = [.searchMail, .readThread, .writeNote, .draftReply]
}

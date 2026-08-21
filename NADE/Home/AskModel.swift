//
//  AskModel.swift
//  NADE
//
//  1a — the three query states, driven by the SSE `route` event (`API.md` §4).
//
//  The screen is a function of the stream, not of the query: `route` arrives
//  first and decides which of the three layouts renders, and everything after
//  it appends. That is why there is one model rather than three — the states
//  are phases of one session, and a half-arrived answer has to render.
//

import SwiftUI

@MainActor
@Observable
final class AskModel {

    private(set) var route: AskRoute?
    /// Accumulated `token` events. Both `answer` and `results` stream prose.
    private(set) var prose = ""
    private(set) var results: [WireThreadRow] = []
    private(set) var draft: AskDraftPayload?
    private(set) var sources: [AskSource] = []
    /// `ConnectionProblem`, like every other screen — not a bare `String?`.
    /// Two vocabularies meant two ways to render the same thing, and the
    /// cache-over-failure rule (A16/D74) restated per screen instead of once.
    private(set) var problem: ConnectionProblem?
    private(set) var isStreaming = false
    /// Set once "Save as draft" has succeeded, so 1a can render its italic
    /// confirmation instead of the button.
    private(set) var savedAgentID: String?

    let query: String
    private let sync: MailSync
    private let routeHint: AskRoute?
    private var task: Task<Void, Never>?

    init(sync: MailSync, query: String, routeHint: AskRoute?) {
        self.sync = sync
        self.query = query
        self.routeHint = routeHint
    }

    /// `DESIGN.md` §1a: the `agent_draft` state's confirmation line.
    static let savedCopy = String(
        localized: "Saved to Agents as a draft. It won't run until you publish it.",
        comment: "1a, after an agent draft is saved"
    )

    func start() {
        guard task == nil else { return }
        isStreaming = true
        task = Task { [weak self] in
            guard let self else { return }
            do {
                for try await event in sync.ask(query: query, threadID: nil, routeHint: routeHint) {
                    if Task.isCancelled { return }
                    apply(event)
                }
            } catch let failure as APIFailure {
                problem = ConnectionProblem(failure)
            } catch {
                // EDGE: a malformed stream. `SSEParser` throws rather than
                // guessing, and the screen says so rather than rendering half a
                // session as if it were whole.
                problem = ConnectionProblem(message: StateCopy.storeUnreadable)
            }
            isStreaming = false
        }
    }

    func cancel() {
        task?.cancel()
        task = nil
        isStreaming = false
    }

    private func apply(_ event: AskEvent) {
        switch event {
        case .route(let kind):
            route = kind
        case .token(let text):
            prose += text
        case .results(let threads):
            results = threads
        case .draft(let payload):
            draft = payload
        case .done(let sources):
            self.sources = sources
            isStreaming = false
        case .error(_, let message):
            // The server writes these for the user (`API.md` §0), so it is
            // rendered rather than replaced — **beside** whatever already
            // arrived, never instead of it. A stream that fails after
            // `route → token → token` has produced a real partial answer, and
            // throwing it away to show a caption loses the only thing the user
            // got.
            problem = ConnectionProblem(message: message)
            isStreaming = false
        }
    }

    /// True while `POST /agents` is in flight. **Not** derived from
    /// `savedAgentID`: that only becomes non-nil when the request *returns*, so
    /// a second tap during a slow round trip passed the `== nil` guard and
    /// created a second agent from the same draft. Creation is not idempotent.
    private(set) var isSaving = false

    /// Which span the editor is over, and what it holds.
    ///
    /// `DESIGN.md` §1a's lead line says "Tap anything underlined to change it."
    /// The card is **pre-save** — there is no agent id yet, so there is nothing
    /// to `PATCH` — so the edit happens here and the recomposed sentence is what
    /// `POST /agents` sends. Without this the sentence promised an edit the
    /// screen could not perform, which §4 does not allow.
    enum Editing: Equatable { case when, doing }

    var editing: Editing?
    var editorText = ""
    private(set) var editedWhen: String?
    private(set) var editedDo: String?

    var whenSpan: String { editedWhen ?? draft?.whenSpan ?? "" }
    var doSpan: String { editedDo ?? draft?.doSpan ?? "" }

    func beginEditing(_ target: Editing) {
        editorText = target == .when ? whenSpan : doSpan
        editing = target
    }

    func commitEdit() {
        defer { editing = nil }
        guard let target = editing else { return }
        let text = editorText.trimmingCharacters(in: .whitespacesAndNewlines)
        // EDGE: an empty span would compose "When , ." — a sentence that cannot
        // compile and that the user did not ask for. Cancel instead.
        guard !text.nadeIsBlank else { return }
        switch target {
        case .when: editedWhen = text
        case .doing: editedDo = text
        }
    }

    /// What "Save as draft" actually sends: the **edited** sentence, composed
    /// the same way 1c composes it, so the two screens cannot disagree.
    var sentenceToSave: String {
        guard let draft else { return "" }
        guard editedWhen != nil || editedDo != nil else { return draft.nlDefinition }
        return AgentBuilderModel.compose(when: whenSpan, doing: doSpan, trailing: draft.trailing)
    }

    /// "Save as draft" → `POST /agents {nl_definition}`. The server forces
    /// `status: "draft"`, which is what the confirmation line promises.
    func saveDraft() async {
        guard let draft, savedAgentID == nil, !isSaving else { return }
        isSaving = true
        defer { isSaving = false }
        do {
            savedAgentID = try await sync.createAgent(nlDefinition: sentenceToSave).id
            problem = nil
        } catch let failure as APIFailure {
            problem = ConnectionProblem(failure)
        } catch {
            problem = ConnectionProblem(message: StateCopy.storeUnreadable)
        }
    }
}

//
//  AgentSourceTests.swift
//  NADETests
//
//  P3's writes: the agent CRUD the builder drives, the `/ask` route the fixture
//  world picks, and the two composed-sentence rules both 1a and 1c depend on.
//

import XCTest
@testable import NADE

final class AgentSourceTests: XCTestCase {

    private func source() -> FixtureMailSource { FixtureMailSource(isEmpty: false) }

    // MARK: - The list and the detail agree

    /// The four detail fixtures are the four rows of `agents.json`. If they ever
    /// drift, opening a row shows a different agent from the one that was
    /// tapped — and nothing else in the suite would notice.
    func testEveryListedAgentHasADetail() async throws {
        let source = source()
        let rows = try await source.agents()
        XCTAssertEqual(rows.count, 4)
        for row in rows {
            let detail = try await source.agent(id: row.id)
            XCTAssertEqual(detail.name, row.name)
            XCTAssertEqual(detail.status, row.status)
            XCTAssertEqual(detail.nlDefinition, row.nlDefinition)
        }
    }

    func testUnknownAgentIsA404() async {
        do {
            _ = try await source().agent(id: "nope")
            XCTFail("expected a not_found")
        } catch let failure as APIFailure {
            XCTAssertEqual(failure.isNotFound, true)
        } catch {
            XCTFail("wrong error: \(error)")
        }
    }

    // MARK: - Create

    /// `API.md` §5: "**A newly created agent is always `status: "draft"`.**"
    /// This is what the Ask screen's promise depends on — "It won't run until
    /// you publish it."
    func testCreatedAgentIsAlwaysADraft() async throws {
        let source = source()
        let created = try await source.createAgent(nlDefinition: "When Stripe emails, save a note.")
        XCTAssertEqual(created.status, .draft)
        XCTAssertNil(created.lastRunAt, "a brand-new agent has never run")
    }

    func testCreatedAgentJoinsTheList() async throws {
        let source = source()
        let before = try await source.agents().count
        let created = try await source.createAgent(nlDefinition: "When Stripe emails, save a note.")
        let after = try await source.agents()
        XCTAssertEqual(after.count, before + 1)
        XCTAssertTrue(after.contains { $0.id == created.id })
    }

    /// EDGE: empty input. An agent with no sentence cannot compile and must not
    /// be created.
    func testCreatingWithAnEmptySentenceIsRejected() async {
        for blank in ["", "   ", "\n\t "] {
            do {
                _ = try await source().createAgent(nlDefinition: blank)
                XCTFail("expected a bad_request for \(blank.debugDescription)")
            } catch let failure as APIFailure {
                XCTAssertEqual(failure.isBadRequest, true)
            } catch {
                XCTFail("wrong error: \(error)")
            }
        }
    }

    /// EDGE: unicode. A sentence of emoji is legal input and must survive.
    func testUnicodeSentenceSurvivesCreation() async throws {
        let sentence = "When 📧 arrives from Ω, save a note — always."
        let created = try await source().createAgent(nlDefinition: sentence)
        XCTAssertEqual(created.nlDefinition, sentence)
    }

    // MARK: - Patch

    func testPatchWritesOnlyWhatItCarries() async throws {
        let source = source()
        let before = try await source.agent(id: "a0000001-0000-4000-8000-000000000001")
        let after = try await source.updateAgent(id: before.id, patch: AgentPatch(status: .paused))

        XCTAssertEqual(after.status, .paused)
        XCTAssertEqual(after.nlDefinition, before.nlDefinition, "an unset field must not be cleared")
        XCTAssertEqual(after.allowedTools, before.allowedTools)
        XCTAssertEqual(after.approvalRequired, before.approvalRequired)
        XCTAssertEqual(after.schedule, before.schedule, "sending no schedule must not clear one")
    }

    /// The reason `AgentPatch` uses `encodeIfPresent` while every other type in
    /// the app writes nullables explicitly: an omitted field means "leave it",
    /// and `"schedule": null` would mean "clear it".
    func testAnEmptyPatchEncodesToAnEmptyObject() throws {
        let data = try JSONEncoder().encode(AgentPatch())
        XCTAssertEqual(String(decoding: data, as: UTF8.self), "{}")
        XCTAssertTrue(AgentPatch().isEmpty)
        XCTAssertFalse(AgentPatch(status: .draft).isEmpty)
    }

    func testPatchIsVisibleInTheListAfterwards() async throws {
        let source = source()
        let id = "a0000003-0000-4000-8000-000000000003"
        _ = try await source.updateAgent(id: id, patch: AgentPatch(status: .published))
        let row = try await source.agents().first { $0.id == id }
        XCTAssertEqual(row?.status, .published, "1b reads the list, so a write has to reach it")
    }

    // MARK: - Delete

    func testDeleteRemovesTheAgentAndIsNotIdempotent() async throws {
        let source = source()
        let id = "a0000004-0000-4000-8000-000000000004"
        try await source.deleteAgent(id: id)
        let remaining = try await source.agents()
        XCTAssertFalse(remaining.contains { $0.id == id })

        do {
            try await source.deleteAgent(id: id)
            XCTFail("expected a not_found on the second delete")
        } catch let failure as APIFailure {
            XCTAssertEqual(failure.isNotFound, true)
        }
    }

    // MARK: - Run

    /// `API.md` §6: a manual run works on a **draft** too — that is what the
    /// builder's "Run once now" is for.
    func testRunWorksOnADraft() async throws {
        let source = source()
        let draft = try await source.agent(id: "a0000003-0000-4000-8000-000000000003")
        XCTAssertEqual(draft.status, .draft)
        let run = try await source.runAgent(id: draft.id)
        XCTAssertFalse(run.runID.isEmpty)
    }

    // MARK: - Ask routing

    /// `route_hint` "forces the route instead of letting the server classify"
    /// (`API.md` §4) — the whole reason 1a's "Make this an agent" can exist.
    func testRouteHintWinsOverTheHeuristics() async throws {
        // A query the heuristics would classify as a search.
        let events = try await collect(source().ask(query: "from:priya@acme.com",
                                                    threadID: nil, routeHint: .agentDraft))
        XCTAssertEqual(events.first, .route(.agentDraft))
    }

    func testHeuristicsPickTheDocumentedRoutes() async throws {
        let search = try await collect(source().ask(query: "from:priya@acme.com",
                                                    threadID: nil, routeHint: nil))
        XCTAssertEqual(search.first, .route(.results))

        let agent = try await collect(source().ask(query: "When a recruiter emails, save a note",
                                                   threadID: nil, routeHint: nil))
        XCTAssertEqual(agent.first, .route(.agentDraft))

        let answer = try await collect(source().ask(query: "What did I miss from Stripe?",
                                                    threadID: nil, routeHint: nil))
        XCTAssertEqual(answer.first, .route(.answer))
    }

    /// `API.md` §4: `route` is first, and there is exactly one terminal event.
    func testEveryStreamIsWellFormed() async throws {
        for query in ["from:x@y.z", "When mail arrives, save a note", "What did I miss?"] {
            let events = try await collect(source().ask(query: query, threadID: nil, routeHint: nil))
            guard case .route = events.first else {
                return XCTFail("\(query): first event was not a route")
            }
            let terminals = events.filter {
                if case .done = $0 { return true }
                if case .error = $0 { return true }
                return false
            }
            XCTAssertEqual(terminals.count, 1, "\(query): expected exactly one terminal event")
            switch events.last {
            case .done, .error: break
            default: XCTFail("\(query): the terminal event was not last")
            }
        }
    }

    /// EDGE: empty input. The field disables its button, so reaching here is a
    /// caller bug — and the contract has an error stream for exactly it.
    func testEmptyQueryStreamsTheErrorFixture() async throws {
        let events = try await collect(source().ask(query: "   ", threadID: nil, routeHint: nil))
        guard case .error = events.last else {
            return XCTFail("expected an error event, got \(String(describing: events.last))")
        }
    }

    private func collect(
        _ stream: AsyncThrowingStream<AskEvent, any Error>
    ) async throws -> [AskEvent] {
        var events: [AskEvent] = []
        for try await event in stream { events.append(event) }
        return events
    }

    // MARK: - The composed sentence

    /// `DESIGN.md` §1c: "The sentence is **composed, not parsed**." Editing one
    /// span rebuilds the whole string from the other fields, which is what makes
    /// the round trip lossless.
    func testComposeMatchesTheDesignsFormat() {
        XCTAssertEqual(
            AgentBuilderModel.compose(when: "a recruiter emails",
                                      doing: "save the next steps", trailing: nil),
            "When a recruiter emails, save the next steps."
        )
        XCTAssertEqual(
            AgentBuilderModel.compose(when: "a recruiter emails",
                                      doing: "save the next steps", trailing: "Ask me first."),
            "When a recruiter emails, save the next steps. Ask me first."
        )
    }

    /// EDGE: an empty trailing is the same as none — not a stray trailing space.
    func testComposeTreatsAnEmptyTrailingAsAbsent() {
        XCTAssertEqual(
            AgentBuilderModel.compose(when: "x", doing: "y", trailing: ""),
            "When x, y."
        )
    }
}

private extension APIFailure {
    var isNotFound: Bool {
        if case .server(let code, _, _, _) = self { return code == .notFound }
        return false
    }

    var isBadRequest: Bool {
        if case .server(let code, _, _, _) = self { return code == .badRequest }
        return false
    }
}

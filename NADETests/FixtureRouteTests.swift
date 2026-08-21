//
//  FixtureRouteTests.swift
//  NADETests
//
//  2a's three focus prompts, and the one property they exist for.
//
//  `DESIGN.md` §4 replaced the mockup's prompts with three of its own, "one per
//  route the classifier produces — `answer`, `results`, `agent_draft` — so
//  tapping each demonstrates a different state". The strings live in
//  `HomeFeedView` and the classifier lives in `FixtureMailSource`; nothing else
//  binds them, and the first version of the list quietly had two prompts on the
//  same route.
//

import XCTest
@testable import NADE

final class FixtureRouteTests: XCTestCase {

    private func route(of query: String) async throws -> AskRoute? {
        let stream = FixtureMailSource(isEmpty: false).ask(query: query, threadID: nil,
                                                          routeHint: nil)
        for try await event in stream {
            if case .route(let kind) = event { return kind }
        }
        return nil
    }

    func testEachFocusPromptReachesItsOwnRoute() async throws {
        let prompts = HomeFeedView.focusPrompts
        XCTAssertEqual(prompts.count, 3)

        var seen: [AskRoute] = []
        for prompt in prompts {
            let kind = try await route(of: prompt)
            XCTAssertNotNil(kind, "\(prompt.debugDescription) produced no route event")
            seen.append(try XCTUnwrap(kind))
        }

        XCTAssertEqual(Set(seen.map(\.rawValue)).count, 3,
                       "the three prompts must demonstrate three different states, got \(seen)")
        XCTAssertEqual(seen, [.answer, .results, .agentDraft],
                       "and in the order DESIGN.md §4 lists them")
    }

    /// The heuristics `PLAN.md` §Ask routing names, plus the fixture-only
    /// `find ` branch that stands in for the cheap model the fixture cannot run.
    func testDocumentedHeuristicsWin() async throws {
        let cases: [(String, AskRoute)] = [
            ("from:priya@acme.com", .results),
            ("\"design review\"", .results),
            ("Find the invoice", .results),
            ("When mail arrives, save a note", .agentDraft),
            ("Every weekday, build a list", .agentDraft),
            ("What did I miss?", .answer),
        ]
        for (query, expected) in cases {
            let kind = try await route(of: query)
            XCTAssertEqual(kind, expected, "\(query.debugDescription) routed to \(String(describing: kind))")
        }
    }

    /// An imperative that starts with a schedule word is an agent, not a search,
    /// even though "every" reads like a quantifier.
    func testAgentPrefixesOutrankTheSearchBranch() async throws {
        let kind = try await route(of: "Every receipt from: acme")
        XCTAssertEqual(kind, .agentDraft)
    }
}

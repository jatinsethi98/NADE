//
//  ContractFixturesTests.swift
//  NADETests
//
//  `docs/contract/` is wired into this target as a
//  PBXFileSystemSynchronizedRootGroup, so every fixture — including the `.sse`
//  streams — is copied into NADETests.xctest verbatim and stays current as the
//  backend lane adds files. No copy phase to maintain, no path to keep in sync.
//
//  P2 onward decodes these into real models. P1's job is only to prove they are
//  reachable and well-formed, so a rename or a drop fails here rather than four
//  phases later. EDGE (E15).
//

import XCTest

final class ContractFixturesTests: XCTestCase {

    private var bundle: Bundle { Bundle(for: ContractFixturesTests.self) }

    /// Named in PLAN.md §Canonical API Contract.
    private static let jsonFixtures = [
        "me", "pair", "mailboxes", "threads", "threads_last_page", "thread",
        "search", "agents", "agent", "run", "feed", "feed_item", "approve",
        "skip", "notes", "note", "drafts", "draft",
        "error_unauthorized", "error_token_consumed", "error_approval_expired",
    ]

    private static let sseFixtures = ["ask_answer", "ask_results", "ask_agent_draft"]

    func testEveryJSONFixtureIsPresentAndParses() throws {
        for name in Self.jsonFixtures {
            let url = try XCTUnwrap(
                bundle.url(forResource: name, withExtension: "json"),
                "docs/contract/\(name).json is not in the test bundle"
            )
            let data = try Data(contentsOf: url)
            XCTAssertFalse(data.isEmpty, "\(name).json is empty")
            XCTAssertNoThrow(
                try JSONSerialization.jsonObject(with: data),
                "\(name).json is not valid JSON"
            )
        }
    }

    func testEverySSEFixtureIsPresentAndLooksLikeAStream() throws {
        for name in Self.sseFixtures {
            let url = try XCTUnwrap(
                bundle.url(forResource: name, withExtension: "sse"),
                "docs/contract/\(name).sse is not in the test bundle"
            )
            let text = try String(contentsOf: url, encoding: .utf8)
            XCTAssertTrue(text.contains("event:"), "\(name).sse has no `event:` lines")
            XCTAssertTrue(text.contains("data:"), "\(name).sse has no `data:` lines")
            // The contract puts `route` first for every ask stream.
            XCTAssertTrue(text.contains("event: route"), "\(name).sse does not open with a route event")
        }
    }
}

//
//  WireEnumTests.swift
//  NADETests
//
//  The half of `WireEnum` that used to be unchecked.
//
//  Every conformer once carried a `rawValue` switch and a mirror-image
//  `init?(rawValue:)` switch — fifty-odd paired literals across four types,
//  with nothing asserting the pairs agreed. `ErrorCode`'s thirteen happened to
//  be covered because `WireDecodeTests` drives off the thirteen `error_*.json`
//  fixtures; `RunStatus` has eight cases and fixtures for four, so `queued`,
//  `running`, `waiting` and `skipped` had never had their two literals
//  compared in either direction.
//
//  Deriving `init?(rawValue:)` from `allKnown` removed one half. This is the
//  other: one generic assertion, applied to every conformer.
//

import XCTest
@testable import NADE

final class WireEnumTests: XCTestCase {

    private func assertRoundTrips<T: WireEnum>(
        _ type: T.Type, expecting rawValues: [String],
        file: StaticString = #filePath, line: UInt = #line
    ) {
        XCTAssertEqual(T.allKnown.map(\.rawValue), rawValues,
                       "\(T.self)'s wire strings changed", file: file, line: line)

        for known in T.allKnown {
            XCTAssertEqual(T(rawValue: known.rawValue), known,
                           "\(T.self).\(known.rawValue) does not survive its own raw value",
                           file: file, line: line)
        }

        // An unknown value keeps its string rather than throwing (P18) — and is
        // never mistaken for a known one.
        let invented = "a-value-from-a-later-phase"
        XCTAssertNil(T(rawValue: invented), file: file, line: line)
        let fallback = T(unknown: invented)
        XCTAssertEqual(fallback.rawValue, invented, file: file, line: line)
        XCTAssertFalse(T.allKnown.contains(fallback),
                       "\(T.self)'s unknown case collides with a known one", file: file, line: line)
    }

    /// The expected strings are written out from `docs/API.md`, not read off the
    /// type — a list derived from `allKnown` would agree with itself.
    func testEveryWireEnumRoundTripsEveryCaseItClaims() {
        assertRoundTrips(MailboxKind.self, expecting: ["system", "user"])
        assertRoundTrips(AccountStatus.self, expecting: ["ok", "needs_reauth"])
        assertRoundTrips(RunStatus.self, expecting: [
            "queued", "running", "pending_approval", "waiting",
            "done", "failed", "expired", "skipped",
        ])
        assertRoundTrips(ErrorCode.self, expecting: [
            "bad_request", "unauthorized", "forbidden", "not_found", "conflict",
            "token_consumed", "gone", "approval_expired", "payload_too_large",
            "rate_limited", "needs_reauth", "upstream_unavailable", "internal",
        ])
    }

    /// `docs/DESIGN.md` §1f gives a display string for all eight run statuses.
    /// Nothing else pins the mapping, and the mockup shows only one of them.
    func testEveryRunStatusHasItsDesignString() {
        let expected = [
            "queued", "running", "waiting on you", "scheduled",
            "done", "failed", "expired", "skipped",
        ]
        XCTAssertEqual(RunStatus.allKnown.map(ThreadAgentCard.statusText), expected)
        // An unrecognised status shows the server's own word rather than
        // nothing, which is the only honest thing to render.
        XCTAssertEqual(ThreadAgentCard.statusText(.unknown("paused")), "paused")
    }
}

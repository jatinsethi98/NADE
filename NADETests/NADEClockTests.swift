//
//  NADEClockTests.swift
//  NADETests
//
//  `nowToTheMinute()` — the floor both tab roots hand their screens instead of
//  a raw `clock.now()`, so that a tab tap's re-render compares equal to the
//  last one. The property the screens depend on is the fourth test: two
//  instants inside one minute quantise to the same `Date`.
//

import XCTest
@testable import NADE

final class NADEClockTests: XCTestCase {

    private func date(_ iso: String) throws -> Date {
        try XCTUnwrap(WireTime.formatter.date(from: iso))
    }

    func testAnExactMinuteIsUnchanged() throws {
        let onTheMinute = try date("2026-08-17T12:00:00Z")
        XCTAssertEqual(NADEClock.fixed(onTheMinute).nowToTheMinute(), onTheMinute)
    }

    func testSecondsFloorToTheStartOfTheMinute() throws {
        XCTAssertEqual(NADEClock.fixed(try date("2026-08-17T12:07:59Z")).nowToTheMinute(),
                       try date("2026-08-17T12:07:00Z"))
    }

    /// Sub-second parts too — the wire formatter cannot express them, so this
    /// one is built from a raw interval.
    func testFractionalSecondsFloorWithTheRest() throws {
        let base = try date("2026-08-17T12:07:00Z")
        let withFraction = base.addingTimeInterval(59.999)
        XCTAssertEqual(NADEClock.fixed(withFraction).nowToTheMinute(), base)
    }

    /// The reason the helper exists: every render inside one minute must see
    /// the same `Date`, or every `MailRow` differs from its previous self on
    /// every tab tap.
    func testTwoInstantsInsideOneMinuteQuantiseEqual() throws {
        let early = try date("2026-08-17T12:07:01Z")
        let late = early.addingTimeInterval(57.9)
        XCTAssertEqual(NADEClock.fixed(early).nowToTheMinute(),
                       NADEClock.fixed(late).nowToTheMinute())
    }

    /// EDGE: a pre-2001 instant has a *negative* reference interval, and
    /// `.down` floors toward the past. Truncation toward zero would round such
    /// a date *up* — to a moment after the one the clock reported.
    func testAPre2001DateFloorsTowardThePastNotTowardZero() {
        let date = Date(timeIntervalSinceReferenceDate: -90.5)
        let floored = NADEClock.fixed(date).nowToTheMinute()
        XCTAssertEqual(floored.timeIntervalSinceReferenceDate, -120)
        XCTAssertLessThanOrEqual(floored, date, "the floor moved a date into its own future")
    }
}

//
//  ListTimeTests.swift
//  NADETests
//
//  Every branch of `docs/DESIGN.md` §1 Type's `listTime` table, plus the one it
//  does not have (EDGE P6).
//
//  The clock and the calendar are both injected. Without that this suite would
//  pass or fail depending on the day it ran and the region the simulator was
//  set to — which is the same reason `-NADENow` exists for the screenshots.
//

import XCTest
@testable import NADE

final class ListTimeTests: XCTestCase {

    /// A pinned reader: Gregorian, POSIX, UTC. `TimeZone.current` on a CI
    /// machine is not a constant, and the day boundary is the whole subject.
    private var calendar: Calendar {
        var calendar = Calendar(identifier: .gregorian)
        calendar.locale = Locale(identifier: "en_US_POSIX")
        calendar.timeZone = TimeZone(secondsFromGMT: 0)!
        return calendar
    }

    /// Monday 17 August 2026, 12:00 UTC.
    private let now = ContractFixture.pinnedNow

    private func listTime(_ iso: String) throws -> String {
        let date = try XCTUnwrap(WireTime.formatter.date(from: iso))
        return ListTime.string(for: date, now: now, calendar: calendar)
    }

    func testTodayIsATime() throws {
        XCTAssertEqual(try listTime("2026-08-17T09:12:04Z"), "9:12")
        XCTAssertEqual(try listTime("2026-08-17T00:00:00Z"), "0:00", "midnight is still today")
        // The 24-hour half of `H:mm`, against a `now` late enough that 14:31 is
        // in the past — a future stamp clamps, which is the next test's job.
        let evening = try XCTUnwrap(WireTime.formatter.date(from: "2026-08-17T20:00:00Z"))
        let afternoon = try XCTUnwrap(WireTime.formatter.date(from: "2026-08-17T14:31:09Z"))
        XCTAssertEqual(ListTime.string(for: afternoon, now: evening, calendar: calendar), "14:31")
    }

    func testYesterdayIsAWord() throws {
        XCTAssertEqual(try listTime("2026-08-16T23:59:59Z"), "Yesterday")
        XCTAssertEqual(try listTime("2026-08-16T00:00:00Z"), "Yesterday")
    }

    func testTheRestOfTheWeekIsAWeekday() throws {
        XCTAssertEqual(try listTime("2026-08-15T18:19:40Z"), "Sat")
        XCTAssertEqual(try listTime("2026-08-11T08:00:00Z"), "Tue", "six days back is still a weekday")
    }

    /// The boundary the table implies and does not spell out. Seven days back
    /// is the same weekday as today, so "Mon" would be actively misleading.
    func testTheSeventhDayFallsThroughToADate() throws {
        XCTAssertEqual(try listTime("2026-08-11T08:00:00Z"), "Tue")
        XCTAssertEqual(try listTime("2026-08-10T08:00:00Z"), "10 Aug")
    }

    func testThisYearIsADayAndMonthAndOtherYearsCarryTheYear() throws {
        XCTAssertEqual(try listTime("2026-01-02T08:00:00Z"), "2 Jan")
        XCTAssertEqual(try listTime("2025-12-31T23:00:00Z"), "31 Dec 2025")
        XCTAssertEqual(try listTime("2024-03-09T10:00:00Z"), "9 Mar 2024")
    }

    /// EDGE (P6). DESIGN.md's table has no branch for a stamp after `now`, and
    /// clock skew produces them — the server stamps `internalDate`, the device
    /// has its own clock. The design never says "in 3 hours", so the rule is
    /// that a future stamp reads as the current time rather than as a date, a
    /// negative weekday, or a crash.
    func testAFutureTimestampClampsToNowInsteadOfInventingAWord() throws {
        XCTAssertEqual(try listTime("2026-08-17T13:00:00Z"), "12:00")
        XCTAssertEqual(try listTime("2027-01-01T00:00:00Z"), "12:00")
    }

    /// The shared calendar is memoised by zone (D88's pattern), and the memo
    /// must not change what it answers: current zone, POSIX locale, Gregorian,
    /// and the same value on every read. The zone-change path itself cannot be
    /// exercised here — `TimeZone.current` is not settable in-process — which
    /// is why the cache is keyed on the zone identifier rather than proven by
    /// a test.
    func testTheSharedCalendarIsStableAndCorrectAcrossReads() {
        let first = ListTime.calendar
        let second = ListTime.calendar
        XCTAssertEqual(first.identifier, .gregorian)
        XCTAssertEqual(first.timeZone, TimeZone.current)
        XCTAssertEqual(first.locale?.identifier, "en_US_POSIX")
        XCTAssertEqual(second, first, "the memoised calendar drifted between reads")
    }

    /// The fixture world, rendered against the clock the screenshots pin.
    func testTheFixtureRowsRenderTheStringsTheScreenshotsShow() throws {
        let page = try ContractFixture.decode(WireThreadPage.self, from: "threads")
        let rendered = page.threads.map { ListTime.string(for: $0.ts, now: now, calendar: calendar) }
        XCTAssertEqual(rendered, ["8:22", "Yesterday", "Yesterday", "Yesterday", "Sat"])
    }
}

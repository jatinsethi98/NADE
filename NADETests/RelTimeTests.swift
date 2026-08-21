//
//  RelTimeTests.swift
//  NADETests
//
//  `DESIGN.md` §1 Type's `relTime`, band by band — including the two the system
//  formatter got wrong and the one it could not express at all.
//

import XCTest
@testable import NADE

final class RelTimeTests: XCTestCase {

    private var calendar: Calendar {
        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = TimeZone(secondsFromGMT: 0)!
        return calendar
    }

    private func now() -> Date {
        calendar.date(from: DateComponents(year: 2026, month: 8, day: 20, hour: 12))!
    }

    private func string(ago seconds: TimeInterval) -> String {
        RelTime.string(for: now().addingTimeInterval(-seconds), now: now(), calendar: calendar)
    }

    func testEveryBandMatchesTheSpec() {
        XCTAssertEqual(string(ago: 0), "just now")
        XCTAssertEqual(string(ago: 59), "just now")
        XCTAssertEqual(string(ago: 60), "1 min ago")
        XCTAssertEqual(string(ago: 45 * 60), "45 min ago")
        XCTAssertEqual(string(ago: 60 * 60), "1h ago")
        XCTAssertEqual(string(ago: 5 * 3600), "5h ago")
    }

    /// `DESIGN.md` §1's bands are an **ordered** list, and `< 24 h → "Nh ago"`
    /// comes before `yesterday`. So 23:00 yesterday read at 01:00 today is
    /// "2h ago", not "yesterday" — the elapsed band wins while it applies, and
    /// "yesterday" covers the previous calendar day once more than 24 hours have
    /// passed. (This test asserted the opposite first, and the code was right.)
    func testTheElapsedBandOutranksYesterdayWhileItApplies() {
        let lateYesterday = calendar.date(from: DateComponents(year: 2026, month: 8, day: 19,
                                                               hour: 23))!
        let earlyToday = calendar.date(from: DateComponents(year: 2026, month: 8, day: 20,
                                                            hour: 1))!
        XCTAssertEqual(RelTime.string(for: lateYesterday, now: earlyToday, calendar: calendar),
                       "2h ago")

        // Past 24 hours, the calendar day is what is left to say.
        let yesterdayNoon = calendar.date(from: DateComponents(year: 2026, month: 8, day: 19,
                                                               hour: 11))!
        XCTAssertEqual(RelTime.string(for: yesterdayNoon, now: now(), calendar: calendar),
                       "yesterday")
    }

    func testDayAndDateBands() {
        XCTAssertEqual(string(ago: 3 * 86_400), "3 days ago")
        XCTAssertEqual(string(ago: 6 * 86_400), "6 days ago")
        // Past a week, an absolute date — and this year carries no year.
        XCTAssertEqual(string(ago: 30 * 86_400), "21 Jul")
        XCTAssertEqual(string(ago: 400 * 86_400), "16 Jul 2025")
    }

    /// **The one `RelativeDateTimeFormatter` could not express.** Server
    /// timestamps are UTC and a device clock can trail them, so a run that has
    /// already happened routinely arrives stamped in the future. There is no
    /// "in 2 minutes" in the design's vocabulary, and a completed run must never
    /// read as a scheduled one.
    func testAFutureTimestampNeverReadsAsTheFuture() {
        for ahead in [1.0, 120.0, 86_400.0, 400 * 86_400.0] {
            let future = now().addingTimeInterval(ahead)
            let text = RelTime.string(for: future, now: now(), calendar: calendar)
            XCTAssertEqual(text, "just now", "\(ahead)s ahead rendered as \(text)")
            XCTAssertFalse(text.contains("in "), "leaked a future tense")
        }
    }

    /// 1b's own string, which is what the screen actually renders.
    func testAgentsListLastRunCopy() {
        XCTAssertEqual(AgentsListView.lastRun(nil, now: now()), "never run")
        XCTAssertEqual(AgentsListView.lastRun(now().addingTimeInterval(-3600), now: now()),
                       "ran 1h ago")
    }
}

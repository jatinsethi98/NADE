//
//  ScheduleDraftTests.swift
//  NADETests
//
//  1d's model. The summary string and the `byweekday` rule are the two places
//  `DESIGN.md` §1d and `API.md` §5.2 disagree with what the controls show, so
//  they carry the most tests.
//

import XCTest
@testable import NADE

final class ScheduleDraftTests: XCTestCase {

    // MARK: - The summary string

    func testSummaryMatchesTheDesignsOwnExamples() {
        var draft = ScheduleDraft()
        draft.unit = .week
        draft.days = ScheduleDraft.weekdaySet
        XCTAssertEqual(draft.summary, "Every week · weekdays",
                       "the default, and the mockup's own first example")

        draft.unit = .day
        draft.days = []
        XCTAssertEqual(draft.summary, "Every day")

        draft.unit = .month
        draft.interval = 3
        XCTAssertEqual(draft.summary, "Every 3 months")

        draft.unit = .week
        draft.interval = 1
        draft.days = ["mon", "wed"]
        XCTAssertEqual(draft.summary, "Every week · Mon Wed")
    }

    /// `DESIGN.md` §1d is explicit: "**It never contains a time.**"
    func testSummaryNeverContainsATime() {
        var draft = ScheduleDraft()
        draft.at = "08:00"
        for unit in ScheduleDraft.Unit.allCases {
            draft.unit = unit
            XCTAssertFalse(draft.summary.contains("08"), "\(unit) summary leaked the time")
            XCTAssertFalse(draft.summary.contains(":"), "\(unit) summary leaked the time")
        }
    }

    /// Selected days are named in **display** order, not set order, or the
    /// string would shuffle between renders.
    func testSummaryOrdersDaysByTheWeekNotBySetIteration() {
        var draft = ScheduleDraft()
        draft.unit = .week
        draft.days = ["fri", "mon", "wed"]
        XCTAssertEqual(draft.summary, "Every week · Mon Wed Fri")
    }

    /// A day is only "weekdays" when it is **exactly** Mon–Fri.
    func testWeekdaysNeedsTheExactSet() {
        var draft = ScheduleDraft()
        draft.unit = .week
        draft.days = ScheduleDraft.weekdaySet.union(["sat"])
        XCTAssertFalse(draft.summary.contains("weekdays"))
        XCTAssertEqual(draft.summary, "Every week · Mon Tue Wed Thu Fri Sat")
    }

    // MARK: - byweekday

    /// `API.md` §5.2: written **only** when the unit is week, `[]` otherwise.
    /// The circles stay visible and selectable at every unit, so this is the
    /// only thing that keeps a day/month schedule from carrying stale days.
    func testByweekdayIsWrittenOnlyForWeek() {
        var draft = ScheduleDraft()
        draft.days = ["mon", "wed"]

        draft.unit = .week
        XCTAssertEqual(draft.wire(existing: nil).byweekday, ["mon", "wed"])

        draft.unit = .day
        XCTAssertEqual(draft.wire(existing: nil).byweekday, [])

        draft.unit = .month
        XCTAssertEqual(draft.wire(existing: nil).byweekday, [])
    }

    func testByweekdayIsOrderedSundayFirst() {
        var draft = ScheduleDraft()
        draft.unit = .week
        draft.days = ["sat", "sun", "wed"]
        XCTAssertEqual(draft.wire(existing: nil).byweekday, ["sun", "wed", "sat"])
    }

    // MARK: - Clamping

    /// "The count clamps to **1…30**."
    func testIntervalClamps() {
        var draft = ScheduleDraft()
        draft.interval = 0
        XCTAssertEqual(draft.interval, 1)
        draft.interval = 99
        XCTAssertEqual(draft.interval, 30)
        draft.interval = -5
        XCTAssertEqual(draft.interval, 1)
    }

    /// An out-of-range value **from the server** is clamped too, not trusted.
    func testIntervalFromTheWireIsClamped() {
        let hostile = WireSchedule(freq: "week", interval: 5000, byweekday: [], bymonthday: nil,
                                   at: "08:00", tz: "UTC",
                                   ends: WireEnds(kind: "never", date: nil, count: nil),
                                   runsDone: 0)
        XCTAssertEqual(ScheduleDraft(hostile).interval, 30)
    }

    // MARK: - `at`

    func testAtRoundTripsThroughTheDatePicker() {
        var draft = ScheduleDraft()
        draft.at = "07:05"
        let date = draft.atDate
        draft.setAt(date)
        XCTAssertEqual(draft.at, "07:05", "a round trip must not drop the leading zero")

        draft.at = "23:59"
        draft.setAt(draft.atDate)
        XCTAssertEqual(draft.at, "23:59")
    }

    /// EDGE: a malformed `at` from the wire. Falling back beats crashing, and
    /// 08:00 is the design's own default.
    func testMalformedAtFallsBack() {
        var draft = ScheduleDraft()
        draft.at = "not-a-time"
        draft.setAt(draft.atDate)
        XCTAssertEqual(draft.at, "08:00")
    }

    // MARK: - ends

    func testEndsWritesOnlyTheFieldItsKindUses() {
        var draft = ScheduleDraft()

        draft.endKind = .never
        var ends = draft.wire(existing: nil).ends
        XCTAssertEqual(ends.kind, "never")
        XCTAssertNil(ends.date)
        XCTAssertNil(ends.count, "a `never` schedule must not carry a count")

        draft.endKind = .after
        draft.endCount = 13
        ends = draft.wire(existing: nil).ends
        XCTAssertEqual(ends.count, 13)
        XCTAssertNil(ends.date)

        draft.endKind = .on
        ends = draft.wire(existing: nil).ends
        XCTAssertNotNil(ends.date)
        XCTAssertNil(ends.count)
    }

    /// `ends.date` is a **calendar** date, not an instant: the day the user
    /// picked is the day that is written, in every time zone.
    func testEndDateIsTheDayThatWasPicked() {
        var draft = ScheduleDraft()
        draft.endKind = .on
        draft.endDate = DateComponents(year: 2027, month: 1, day: 6)
        XCTAssertEqual(draft.wire(existing: nil).ends.date, "2027-01-06")
    }

    /// The round trip that used to lose a day: parse, hand to the picker, take
    /// it back, write.
    ///
    /// **This asserts the instant, not a time zone.** An earlier version looped
    /// over four zone identifiers and was a tautology: `endDatePicked` pins UTC
    /// on both sides, and the identifier only reached the unrelated `tz:` field
    /// and the failure message, so all four iterations were the same test. What
    /// actually mattered is that the `Date` the picker is handed lands on the
    /// intended calendar day **in every offset a device might be in**, which is
    /// what the interpretation below checks directly.
    /// The wheel shows the day that was picked, in **every** inhabited offset.
    ///
    /// This is the assertion that caught the real bug: the picker used to anchor
    /// at UTC noon, so at UTC+13 and +14 the instant fell on the following day
    /// and Auckland saw 7 January for a schedule that said the 6th.
    func testThePickerShowsAndReturnsTheDayThatWasPicked() {
        var draft = ScheduleDraft()
        draft.endKind = .on
        draft.endDate = DateComponents(year: 2027, month: 1, day: 6)

        // UTC-11 through UTC+14 is the whole inhabited range.
        for offsetHours in stride(from: -11, through: 14, by: 1) {
            var calendar = Calendar(identifier: .gregorian)
            calendar.timeZone = TimeZone(secondsFromGMT: offsetHours * 3600)!

            let shown = calendar.dateComponents([.year, .month, .day],
                                                from: draft.endDatePicked(in: calendar))
            XCTAssertEqual(shown.day, 6, "UTC\(offsetHours) showed day \(shown.day!)")
            XCTAssertEqual(shown.month, 1)
            XCTAssertEqual(shown.year, 2027)

            // And taking it back from the wheel writes the same day.
            var round = draft
            round.setEndDatePicked(round.endDatePicked(in: calendar), in: calendar)
            XCTAssertEqual(round.wire(existing: nil).ends.date, "2027-01-06",
                           "UTC\(offsetHours) lost a day on the round trip")
        }
    }

    func testEndDateFormatIsAlwaysTenCharacters() {
        var draft = ScheduleDraft()
        draft.endKind = .on
        draft.endDate = DateComponents(year: 2027, month: 1, day: 6)
        XCTAssertEqual(draft.wire(existing: nil).ends.date?.count, 10,
                       "single-digit months and days must be zero-padded")
    }

    /// EDGE: a malformed `ends.date` leaves the default rather than landing on
    /// year zero.
    func testMalformedEndDateIsIgnored() {
        for bad in ["", "2027", "2027-13-01", "2027-01-99", "not-a-date"] {
            let schedule = WireSchedule(freq: "week", interval: 1, byweekday: [], bymonthday: nil,
                                        at: "08:00", tz: "UTC",
                                        ends: WireEnds(kind: "on", date: bad, count: nil),
                                        runsDone: 0)
            XCTAssertEqual(ScheduleDraft(schedule).endDate.year, 2027,
                           "\(bad.debugDescription) should have been ignored")
        }
    }

    // MARK: - Fields with no control

    /// `tz` is captured at creation and never shown, so editing the repeat must
    /// not silently move an agent's timezone.
    func testEditingPreservesTimezoneAndRunsDone() {
        let existing = WireSchedule(freq: "week", interval: 1, byweekday: ["mon"], bymonthday: nil,
                                    at: "08:00", tz: "Asia/Kolkata",
                                    ends: WireEnds(kind: "never", date: nil, count: nil),
                                    runsDone: 7)
        var draft = ScheduleDraft(existing)
        draft.unit = .day
        let written = draft.wire(existing: existing)
        XCTAssertEqual(written.tz, "Asia/Kolkata")
        XCTAssertEqual(written.runsDone, 7, "runs_done is server-maintained, not reset by an edit")
    }

    /// A brand-new schedule takes the device's zone; an edit keeps the one the
    /// agent already had.
    ///
    /// The first assertion restates the implementation and is kept only as the
    /// foil for the second, which is the behaviour that matters: `tz` has no
    /// control in v1, so an edit must never quietly move it.
    func testTimezoneIsTakenOnCreationAndPreservedOnEdit() {
        XCTAssertEqual(ScheduleDraft().wire(existing: nil).tz, TimeZone.current.identifier)

        let elsewhere = TimeZone.current.identifier == "Pacific/Auckland"
            ? "Asia/Kolkata" : "Pacific/Auckland"
        let existing = WireSchedule(freq: "day", interval: 1, byweekday: [], bymonthday: nil,
                                    at: "08:00", tz: elsewhere,
                                    ends: WireEnds(kind: "never", date: nil, count: nil),
                                    runsDone: 3)
        var draft = ScheduleDraft(existing)
        draft.unit = .month
        let written = draft.wire(existing: existing)
        XCTAssertEqual(written.tz, elsewhere)
        XCTAssertNotEqual(written.tz, TimeZone.current.identifier,
                          "an edit must not adopt the editing device's zone")
    }

    /// `bymonthday` has no control in v1 and is always null.
    func testBymonthdayIsAlwaysNull() {
        var draft = ScheduleDraft()
        for unit in ScheduleDraft.Unit.allCases {
            draft.unit = unit
            XCTAssertNil(draft.wire(existing: nil).bymonthday)
        }
    }

    // MARK: - The unit chip

    /// It **cycles**, it is not a menu — and the cycle is closed.
    func testUnitCycles() {
        XCTAssertEqual(ScheduleDraft.Unit.day.next, .week)
        XCTAssertEqual(ScheduleDraft.Unit.week.next, .month)
        XCTAssertEqual(ScheduleDraft.Unit.month.next, .day)
    }

    /// EDGE: a `freq` this build does not know falls back rather than throwing
    /// the sheet away.
    func testUnknownFreqFallsBackToWeek() {
        let odd = WireSchedule(freq: "fortnight", interval: 1, byweekday: [], bymonthday: nil,
                               at: "08:00", tz: "UTC",
                               ends: WireEnds(kind: "never", date: nil, count: nil), runsDone: 0)
        XCTAssertEqual(ScheduleDraft(odd).unit, .week)
    }
}

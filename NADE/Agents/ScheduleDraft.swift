//
//  ScheduleDraft.swift
//  NADE
//
//  1d's editable copy of `agents.schedule` (PLAN §Schedule model, `API.md` §5.2).
//
//  A separate type from `WireSchedule` because the sheet's footer is Cancel +
//  Save: the controls have to move without anything being written, and mutating
//  the wire object in place would leave a cancelled edit already applied.
//

import Foundation

nonisolated struct ScheduleDraft: Equatable, Sendable {

    /// `freq`. The unit chip **cycles** rather than opening a menu
    /// (`DESIGN.md` §1d), so the order here is the cycle order.
    enum Unit: String, CaseIterable, Sendable {
        case day, week, month

        /// Total, and no force unwrap. Three cases do not need `allCases`
        /// arithmetic to say what comes next.
        var next: Unit {
            switch self {
            case .day: .week
            case .week: .month
            case .month: .day
            }
        }

        var singular: String {
            switch self {
            case .day: String(localized: "day", comment: "Schedule unit, singular")
            case .week: String(localized: "week", comment: "Schedule unit, singular")
            case .month: String(localized: "month", comment: "Schedule unit, singular")
            }
        }

        var plural: String {
            switch self {
            case .day: String(localized: "days", comment: "Schedule unit, plural")
            case .week: String(localized: "weeks", comment: "Schedule unit, plural")
            case .month: String(localized: "months", comment: "Schedule unit, plural")
            }
        }
    }

    enum EndKind: String, CaseIterable, Sendable { case never, on, after }

    /// The seven circles, `S M T W T F S` **starting Sunday** — the display
    /// order. `API.md` §5.2's `byweekday` uses these three-letter codes.
    static let weekdayCodes = ["sun", "mon", "tue", "wed", "thu", "fri", "sat"]
    static let weekdayInitials = ["S", "M", "T", "W", "T", "F", "S"]
    /// What "weekdays" means in the summary string.
    static let weekdaySet: Set<String> = ["mon", "tue", "wed", "thu", "fri"]

    var unit: Unit = .week
    /// `DESIGN.md` §1d: "The count clamps to **1…30**."
    var interval = 1 { didSet { interval = min(max(interval, 1), 30) } }
    var days: Set<String> = weekdaySet
    var at = "08:00"
    var endKind: EndKind = .never
    /// **A calendar date, not an instant.** `ends.date` is `"YYYY-MM-DD"` with
    /// no time and no zone (`API.md` §5.2), so it is held as its components.
    /// Storing a `Date` and formatting it was wrong in both directions: an
    /// existing `2027-01-06` parsed at UTC midnight displayed as 5 January in
    /// New York, and a date picked near local midnight in a positive offset
    /// serialised as the day before — pausing the agent a day early.
    var endDate = DateComponents(year: 2027, month: 1, day: 6)
    var endCount = 13

    init() {}

    init(_ schedule: WireSchedule?) {
        guard let schedule else { return }
        unit = Unit(rawValue: schedule.freq) ?? .week
        interval = min(max(schedule.interval, 1), 30)
        days = Set(schedule.byweekday)
        at = schedule.at
        endKind = EndKind(rawValue: schedule.ends.kind) ?? .never
        if let date = schedule.ends.date, let parsed = Self.components(from: date) {
            endDate = parsed
        }
        if let count = schedule.ends.count { endCount = count }
    }

    /// The primary button's label, exactly as `DESIGN.md` §1d computes it.
    ///
    /// **It never contains a time.** "Every weekday at 8:00" belongs to the
    /// builder behind the sheet, which renders `nl_definition`.
    var summary: String {
        var text = "Every " + (interval > 1 ? "\(interval) \(unit.plural)" : unit.singular)
        if unit == .week, !days.isEmpty {
            let selected = Self.weekdayCodes.filter(days.contains)
            if Set(selected) == Self.weekdaySet {
                text += " · " + String(localized: "weekdays",
                                       comment: "1d summary when exactly Mon–Fri are selected")
            } else {
                text += " · " + selected.map { $0.capitalized }.joined(separator: " ")
            }
        }
        return text
    }

    /// `byweekday` is written **only when the unit is week** (`API.md` §5.2) and
    /// is `[]` for day and month. The circles stay visible and selectable at
    /// every unit — the mockup renders them unconditionally and applies no
    /// disabled styling — so this is the only place the distinction exists.
    func wire(existing: WireSchedule?) -> WireSchedule {
        WireSchedule(
            freq: unit.rawValue,
            interval: interval,
            byweekday: unit == .week ? Self.weekdayCodes.filter(days.contains) : [],
            bymonthday: nil,
            at: at,
            // `tz` has no control in v1: captured from the device when the agent
            // is created and never shown. Preserved on edit so changing the
            // repeat does not silently move an agent's timezone.
            tz: existing?.tz ?? TimeZone.current.identifier,
            ends: WireEnds(
                kind: endKind.rawValue,
                date: endKind == .on ? Self.string(from: endDate) : nil,
                count: endKind == .after ? endCount : nil
            ),
            // Server-maintained and ignored if sent; carried so a round trip
            // does not appear to reset it.
            runsDone: existing?.runsDone ?? 0
        )
    }

    /// The value column beside each Ends radio.
    func endValue(for kind: EndKind) -> String {
        switch kind {
        case .never: ""
        case .on: Self.display(endDate)
        case .after: String(localized: "\(endCount) runs", comment: "1d Ends, the run count")
        }
    }

    /// `"YYYY-MM-DD"` straight from the components. No `DateFormatter`, no
    /// zone, no instant — the three things that could shift the day.
    static func string(from components: DateComponents) -> String {
        String(format: "%04d-%02d-%02d",
               components.year ?? 2027, components.month ?? 1, components.day ?? 1)
    }

    /// The inverse. Returns nil for anything that is not three integers, so a
    /// malformed value leaves the default rather than landing on 1 January
    /// year zero.
    static func components(from text: String) -> DateComponents? {
        let parts = text.split(separator: "-").compactMap { Int($0) }
        guard parts.count == 3, (1...12).contains(parts[1]), (1...31).contains(parts[2]) else {
            return nil
        }
        return DateComponents(year: parts[0], month: parts[1], day: parts[2])
    }

    /// "6 Jan 2027", the mockup's own format for the Ends value column. The
    /// components are resolved in the **current** calendar only to be shown;
    /// nothing round-trips back through this.
    static func display(_ components: DateComponents) -> String {
        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = TimeZone(secondsFromGMT: 0) ?? .gmt
        guard let date = calendar.date(from: components) else { return string(from: components) }
        // Memoised (D88). 1d re-renders on every stepper tick, every day-circle
        // tap and every unit cycle, and `setLocalizedDateFormatFromTemplate` is
        // the most expensive formatter form there is.
        return DateFormatterCache.localized(template: "d MMM yyyy", calendar: calendar)
            .string(from: date)
    }

    /// What the `DatePicker` binds to.
    ///
    /// **The device's own calendar, on both sides** — not UTC. A `DatePicker`
    /// renders an instant in the device's zone, so anchoring at UTC noon showed
    /// the *next* day to anyone at UTC+13 or +14: pick "6 January" in Auckland
    /// and the wheel reads 7 January. Building the instant in `Calendar.current`
    /// makes the day on the wheel the day in `endDate`, and reading it back the
    /// same way keeps that true. The wire string is still built straight from
    /// the components, so no instant reaches the server.
    ///
    /// Noon, not midnight, so a DST spring-forward that deletes 00:00 cannot
    /// make the day unrepresentable.
    func endDatePicked(in calendar: Calendar = .current) -> Date {
        var midday = endDate
        midday.hour = 12
        midday.minute = 0
        return calendar.date(from: midday) ?? Date()
    }

    mutating func setEndDatePicked(_ date: Date, in calendar: Calendar = .current) {
        endDate = calendar.dateComponents([.year, .month, .day], from: date)
    }

    /// `at` is `"HH:MM"` on the wire. These two convert to and from the
    /// `DatePicker` the At row expands into.
    var atDate: Date {
        let parts = at.split(separator: ":").compactMap { Int($0) }
        var components = DateComponents()
        components.hour = parts.count == 2 ? parts[0] : 8
        components.minute = parts.count == 2 ? parts[1] : 0
        return Calendar.current.date(from: components) ?? Date()
    }

    mutating func setAt(_ date: Date) {
        let components = Calendar.current.dateComponents([.hour, .minute], from: date)
        at = String(format: "%02d:%02d", components.hour ?? 0, components.minute ?? 0)
    }
}

//
//  RelTime.swift
//  NADE
//
//  `DESIGN.md` §1 Type's second formatter, in full:
//
//    < 60 s → "just now" · < 60 min → "N min ago" · < 24 h → "Nh ago" ·
//    yesterday → "yesterday" · < 7 days → "N days ago" · this year → "D MMM" ·
//    else "D MMM YYYY"
//
//  §1 also says v1 uses **two** formatters and nothing else, and this is the
//  second — `ListTime` is the first. It exists because
//  `RelativeDateTimeFormatter` is not it: that produced "45 seconds ago" where
//  the design says "just now", "3 months ago" where the design says "16 May",
//  and — with no floor at zero — **"in 2 minutes"** for a server timestamp a
//  device with a slow clock sees as the future.
//

import Foundation

nonisolated enum RelTime {

    /// Shares `ListTime`'s calendar, so both formatters agree about what "day"
    /// means and `-NADENow` pins them together.
    static var calendar: Calendar { ListTime.calendar }

    static func string(for date: Date, now: Date, calendar: Calendar = RelTime.calendar) -> String {
        // EDGE (clock skew): the server stamps in UTC and the device's clock can
        // be behind it, so `date` is routinely a little in the future. There is
        // no "in 2 minutes" in the design's vocabulary, and a run that has
        // already happened must never read as scheduled — the future clamps to
        // "just now".
        let seconds = now.timeIntervalSince(date)
        if seconds < 60 { return justNow }

        let minutes = Int(seconds / 60)
        if minutes < 60 {
            return String(localized: "\(minutes) min ago", comment: "relTime, under an hour")
        }

        let hours = minutes / 60
        if hours < 24 {
            return String(localized: "\(hours)h ago", comment: "relTime, under a day")
        }

        // Calendar days, not 24-hour blocks — and measured from the **injected**
        // `now`, not the wall clock. `Calendar.isDateInYesterday` consults the
        // real current date, which ignores `-NADENow`, makes a screenshot
        // undeterministic and makes this function untestable.
        let days = calendar.dateComponents([.day], from: calendar.startOfDay(for: date),
                                           to: calendar.startOfDay(for: now)).day ?? 0
        if days == 1 {
            return String(localized: "yesterday", comment: "relTime, the previous calendar day")
        }
        if days < 7 {
            return String(localized: "\(days) days ago", comment: "relTime, under a week")
        }

        let sameYear = calendar.component(.year, from: date) == calendar.component(.year, from: now)
        return formatter(sameYear ? "d MMM" : "d MMM yyyy", calendar).string(from: date)
    }

    static let justNow = String(localized: "just now", comment: "relTime, under a minute")

    /// `dateFormat`, not a localized template, for the same reason 2a's header
    /// uses one: the design fixes the order as day-then-month. Month names still
    /// come from the locale.
    ///
    /// Memoised (D88). The `≥ 7 days` branch is the common case for an agent's
    /// `last_run_at`, and 1b re-renders on every filter tap and every write.
    private static func formatter(_ format: String, _ calendar: Calendar) -> DateFormatter {
        DateFormatterCache.fixed(format, calendar: calendar)
    }
}

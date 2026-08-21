//
//  ListTime.swift
//  NADE
//
//  `docs/DESIGN.md` §1 Type, the `listTime` row:
//
//    today → `H:mm` · yesterday → "Yesterday" · < 7 days → weekday abbr
//    ("Fri") · this year → "D MMM" · else → "D MMM YYYY"
//
//  Used by the mail row and the thread's meta rows. (The other formatter,
//  `relTime`, belongs to notes and agents and arrives with P7.)
//
//  Two decisions the table does not make, both recorded because they change
//  what a reader sees:
//
//  * **"Today" is the device's calendar day, while the wire is UTC.** A message
//    stamped 23:40 UTC is "today" in Phoenix and "yesterday" in Berlin, and the
//    right answer is the reader's, not the server's.
//  * **The formats are pinned to `en_US_POSIX`.** v1 ships one language, and
//    the alternative is a screenshot set that differs by simulator region —
//    "17 Aug" here, "17 août" there — for reasons unrelated to the code.
//    Revisit with localisation, not before.
//

import Foundation
import os

nonisolated enum ListTime {

    /// The reader's day boundaries, and a stable rendering of them.
    ///
    /// Memoised by time zone, `DateFormatterCache`'s own pattern (D88):
    /// building a `Calendar` plus its locale is not free, this sits on the
    /// same hot path as the formatters — `MailRow` reaches it twice per row
    /// per render — and the value never changes until the zone does. Keying on
    /// the zone identifier is what keeps a device that changes zone re-binning
    /// its rows. `Calendar` is a value type, so callers get copies and cannot
    /// mutate the cached one.
    static var calendar: Calendar {
        let zone = TimeZone.current
        if let known = calendars.withLock({ $0[zone.identifier] }) { return known }
        var calendar = Calendar(identifier: .gregorian)
        calendar.locale = Locale(identifier: "en_US_POSIX")
        calendar.timeZone = zone
        calendars.withLock { $0[zone.identifier] = calendar }
        return calendar
    }

    private static let calendars = OSAllocatedUnfairLock(initialState: [String: Calendar]())

    /// Built from the **same** calendar that decides the day boundary. Reading
    /// the zone from `TimeZone.current` instead is a bug that hides in plain
    /// sight: the branch and the rendering disagree by the offset, so a message
    /// is filed under "today" and then printed with yesterday's clock time.
    /// Memoised by `(format, time zone)`.
    ///
    /// Setting `dateFormat` compiles an ICU pattern, which measures ~55 µs
    /// against ~0.4 µs for a formatter that already exists. Every mail row
    /// formats a timestamp, so a 25-row screen was paying ~1.4 ms of pure
    /// formatter construction per render — a sixth of a 120 Hz frame, for a
    /// string that is four possible shapes.
    ///
    /// Keyed on the zone rather than cached outright so the injected calendar
    /// in `ListTimeTests` still works and a device that changes time zone still
    /// re-bins its rows.
    /// The memo moved to `DateFormatterCache` when P3 added three more callers
    /// that needed it. Same keying, same reason.
    private static func formatter(_ format: String, _ calendar: Calendar) -> DateFormatter {
        DateFormatterCache.fixed(format, calendar: calendar)
    }

    static func string(for date: Date, now: Date, calendar: Calendar = ListTime.calendar) -> String {
        // EDGE (P6): DESIGN.md's table has no branch for a timestamp *after*
        // now, and clock skew produces them — the server stamps `internalDate`,
        // the device's clock is its own. "In 3 hours" is not a thing this
        // design says, so a future stamp reads as today's time.
        let effective = min(date, now)

        let today = calendar.startOfDay(for: now)
        let day = calendar.startOfDay(for: effective)
        let days = calendar.dateComponents([.day], from: day, to: today).day ?? 0

        switch days {
        case ..<1:
            return formatter("H:mm", calendar).string(from: effective)
        case 1:
            return String(localized: "Yesterday", comment: "Mail row timestamp for the previous day")
        case 2...6:
            return formatter("EEE", calendar).string(from: effective)
        default:
            let sameYear = calendar.component(.year, from: effective) == calendar.component(.year, from: now)
            return formatter(sameYear ? "d MMM" : "d MMM yyyy", calendar).string(from: effective)
        }
    }
}

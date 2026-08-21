//
//  DateFormatterCache.swift
//  NADE
//
//  One memo for every `DateFormatter` in the app.
//
//  Setting `dateFormat` compiles an ICU pattern — ~55 µs on device against
//  ~0.4 µs for a formatter that already exists — and
//  `setLocalizedDateFormatFromTemplate` is worse, because it resolves a locale
//  template first. IOS_DECISIONS D88 measured this and memoised `ListTime`;
//  P3 then wrote `HomeDate`, `RelTime` and `ScheduleDraft.display` without it,
//  putting fresh formatters back on the render path — 2a's header rebuilt one
//  per keystroke in the ask field, and 1a's rebuilt one **per streamed token**.
//
//  Keyed on the pattern *and* the zone and locale, not cached outright, so an
//  injected calendar in a test still works and a device that changes time zone
//  still re-bins its rows.
//

import Foundation
import os

nonisolated enum DateFormatterCache {

    private static let cache = OSAllocatedUnfairLock(initialState: [String: DateFormatter]())

    /// A formatter for an explicit `dateFormat` pattern.
    ///
    /// The pattern is fixed rather than templated wherever `DESIGN.md` pins the
    /// field order (2a's "Sunday 16 August", `relTime`'s "D MMM"); month and
    /// weekday **names** still come from the locale.
    static func fixed(_ format: String, calendar: Calendar) -> DateFormatter {
        formatter(key: "f|\(format)", calendar: calendar) { $0.dateFormat = format }
    }

    /// A formatter for a locale-ordered template, for the one place the design
    /// does not pin the order (1d's "6 Jan 2027").
    static func localized(template: String, calendar: Calendar) -> DateFormatter {
        formatter(key: "t|\(template)", calendar: calendar) {
            $0.setLocalizedDateFormatFromTemplate(template)
        }
    }

    private static func formatter(
        key prefix: String,
        calendar: Calendar,
        configure: (DateFormatter) -> Void
    ) -> DateFormatter {
        let key = "\(prefix)|\(calendar.timeZone.identifier)|\(calendar.locale?.identifier ?? "")"
        if let known = cache.withLock({ $0[key] }) { return known }

        // **`Calendar(identifier:).locale` is not nil — it is the ICU *root*
        // locale**, whose `identifier` is the empty string. A `??` fallback
        // therefore never fires, the formatter resolves month symbols against
        // root, and `"d MMM"` renders "16 M07" instead of the design's
        // "16 Jul". Emptiness is the test, not nil-ness.
        //
        // The calendar carries the locale too, because `DateFormatter` looks its
        // symbols up through `calendar` as well; resolving both from one value
        // is what keeps them in step.
        let locale = calendar.locale.flatMap { $0.identifier.isEmpty ? nil : $0 }
            ?? Locale(identifier: "en_US_POSIX")
        var resolved = calendar
        resolved.locale = locale

        let formatter = DateFormatter()
        formatter.calendar = resolved
        formatter.locale = locale
        formatter.timeZone = calendar.timeZone
        configure(formatter)
        cache.withLock { $0[key] = formatter }
        return formatter
    }
}

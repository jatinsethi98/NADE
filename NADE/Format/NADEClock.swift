//
//  NADEClock.swift
//  NADE
//
//  `now`, as something a test and a screenshot can pin.
//
//  Not a convenience. The fixture world is frozen — `docs/contract/generate.py`
//  is forbidden from importing anything that can read a clock — but `listTime`
//  is a function of *now*, so the same fixture renders "Yesterday" today and
//  "17 Aug" next week. Every screenshot would differ from the last for reasons
//  that have nothing to do with the code.
//

import Foundation

nonisolated struct NADEClock: Sendable {
    let now: @Sendable () -> Date

    static let live = NADEClock(now: { Date() })

    static func fixed(_ date: Date) -> NADEClock {
        NADEClock(now: { date })
    }

    /// `now`, floored to the start of its minute.
    ///
    /// What the tab roots hand their screens instead of `now()`.
    /// `RootTabView.body` reads `navigation.selection`, so every tab tap
    /// re-runs both tab-root bodies — and a fresh `Date` there made every
    /// visible `MailRow` differ from its previous self and re-format, and every
    /// `ThreadMessageBlock` re-render, for a change nothing on 1e/1f/2a can
    /// show: no screen renders finer than a minute. Floored, equal renders
    /// compare equal. (`CRITERIA.md` §Known limits 9 already accepts that `now`
    /// is captured at build time and can go stale until something invalidates
    /// the screen; this only widens "stale" from an instant to a minute.)
    func nowToTheMinute() -> Date {
        let seconds = now().timeIntervalSinceReferenceDate
        // EDGE: `.down` rounds toward negative infinity, so a pre-2001 stamp
        // (a negative interval) still floors to the start of its own minute
        // rather than rounding toward 1 Jan 2001.
        return Date(timeIntervalSinceReferenceDate: (seconds / 60).rounded(.down) * 60)
    }

    /// `-NADENow 2026-08-17T12:00:00Z`, DEBUG only. Parsed with the wire's own
    /// formatter, so an unparseable value falls back to the real clock rather
    /// than silently pinning the app to 1970.
    static func fromLaunchArgumentsOrLive() -> NADEClock {
        #if DEBUG
        if let raw = UserDefaults.standard.string(forKey: "NADENow"),
           let date = WireTime.formatter.date(from: raw) {
            return .fixed(date)
        }
        #endif
        return .live
    }
}

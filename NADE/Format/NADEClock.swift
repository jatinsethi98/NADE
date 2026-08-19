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

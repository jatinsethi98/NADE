//
//  WireEnum.swift
//  NADE
//
//  Closed-looking enums that are open on the wire.
//
//  `mailbox.kind`, `account.status` and a run's `status` are fixed sets in
//  `docs/API.md` today. A client that throws on an unrecognised value turns
//  "the server grew a case" into a blank screen — and the blank screen is the
//  whole list, not the one row, because a decode failure fails the response.
//
//  So every one of them keeps the raw string. The `CHECK` constraints that
//  would have pinned these columns to today's values are deliberately absent
//  from `Schema.swift` for the same reason: a forward-compatible decode that
//  then fails the write is not forward compatible.
//

import Foundation

/// A wire enum that survives a value it has never heard of.
nonisolated protocol WireEnum: RawRepresentable, Codable, Hashable, Sendable where RawValue == String {
    /// Every case the contract defines today, in `docs/API.md`'s order.
    /// `unknown` is not one of them.
    static var allKnown: [Self] { get }
    init(unknown raw: String)
}

nonisolated extension WireEnum {
    /// Derived from `allKnown` rather than hand-written.
    ///
    /// Each conformer used to carry a `rawValue` switch **and** a mirror-image
    /// `init?(rawValue:)` switch — every wire string typed twice, fifty-odd
    /// paired literals across four types, with nothing asserting the pairs
    /// agreed. A case added to one and missed in the other decodes silently to
    /// `.unknown` with no compiler help and no test.
    ///
    /// `WireEnumTests` closes the other half: every known case round-trips.
    init?(rawValue: String) {
        guard let match = Self.allKnown.first(where: { $0.rawValue == rawValue }) else { return nil }
        self = match
    }

    init(from decoder: Decoder) throws {
        let raw = try decoder.singleValueContainer().decode(String.self)
        self = Self(rawValue: raw) ?? Self(unknown: raw)
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.singleValueContainer()
        try container.encode(rawValue)
    }
}

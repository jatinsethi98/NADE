//
//  WireTime.swift
//  NADE
//
//  The one time format on the wire, and the decoder that enforces it.
//
//  `docs/API.md` §0: "ISO-8601 UTC, second precision, always `Z`-suffixed …
//  Never a local offset, never milliseconds. The iOS side decodes with a fixed
//  formatter, not `.iso8601` with fractional seconds."
//
//  The instruction is about *rejection*, not convenience. `.iso8601` accepts
//  several shapes the contract forbids, so a server that started emitting
//  `+00:00` or fractional seconds would keep decoding and the drift would only
//  show up as timestamps that sort wrong. A fixed format turns that into a
//  decode failure at the seam, which is where the contract says it belongs.
//

import Foundation

nonisolated enum WireTime {

    /// `2026-08-16T09:12:04Z` and nothing else.
    ///
    /// `en_US_POSIX` because a device on a non-Gregorian calendar or a
    /// 12-hour locale would otherwise reinterpret the pattern — the classic
    /// bug where an app parses dates correctly everywhere except Thailand.
    /// `isLenient = false` because the default is true and lenient parsing
    /// would quietly accept most of what this exists to reject.
    static let formatter: DateFormatter = {
        let f = DateFormatter()
        f.locale = Locale(identifier: "en_US_POSIX")
        f.timeZone = TimeZone(secondsFromGMT: 0)
        f.calendar = Calendar(identifier: .gregorian)
        f.dateFormat = "yyyy-MM-dd'T'HH:mm:ss'Z'"
        f.isLenient = false
        return f
    }()

    /// The decoder every response goes through.
    ///
    /// Deliberately **not** `.convertFromSnakeCase`: every model spells its
    /// `CodingKeys` out, so a field can be grepped against `docs/API.md` and a
    /// mismatch names the key it could not find instead of a converted guess.
    static func decoder() -> JSONDecoder {
        let d = JSONDecoder()
        d.dateDecodingStrategy = .custom { decoder in
            let raw = try decoder.singleValueContainer().decode(String.self)
            // `DateFormatter` is more forgiving than its pattern looks. It
            // parses "2026-08-16T09:12:04" against `…ss'Z'` — a trailing
            // literal that runs off the end of the input is simply not
            // required — and it accepts single-digit months against `MM`. Both
            // are exactly the drift API.md is trying to make loud, so the
            // parse is not the check: **the round trip is.** Anything the
            // formatter would not itself emit, byte for byte, is not the
            // contract's timestamp.
            guard let date = formatter.date(from: raw), formatter.string(from: date) == raw else {
                throw DecodingError.dataCorrupted(.init(
                    codingPath: decoder.codingPath,
                    debugDescription: """
                        "\(raw)" is not an API.md timestamp. The contract is \
                        ISO-8601 UTC at second precision with a literal Z — \
                        no fractional seconds, no numeric offset.
                        """
                ))
            }
            return date
        }
        return d
    }

    /// The mirror of `decoder()`, used by the round-trip test that proves a
    /// model does not silently drop a field it decoded.
    static func encoder() -> JSONEncoder {
        let e = JSONEncoder()
        e.dateEncodingStrategy = .custom { date, encoder in
            var container = encoder.singleValueContainer()
            try container.encode(formatter.string(from: date))
        }
        return e
    }
}

//
//  WireTimeTests.swift
//  NADETests
//
//  P2 A4. `docs/API.md` §0 does not just describe the timestamp format, it
//  forbids the near misses: "Never a local offset, never milliseconds. The iOS
//  side decodes with a fixed formatter, not `.iso8601` with fractional
//  seconds."
//
//  A test that only proves the good case would stay green under `.iso8601`,
//  which accepts two of the three shapes below. The rejections are the point.
//

import XCTest
@testable import NADE

final class WireTimeTests: XCTestCase {

    private struct Box: Codable, Equatable { let ts: Date }

    private func decode(_ raw: String) throws -> Date {
        let json = Data(#"{"ts":"\#(raw)"}"#.utf8)
        return try WireTime.decoder().decode(Box.self, from: json).ts
    }

    func testTheContractShapeDecodesToTheRightInstant() throws {
        let date = try decode("2026-08-16T09:12:04Z")
        var utc = Calendar(identifier: .gregorian)
        utc.timeZone = TimeZone(secondsFromGMT: 0)!
        let parts = utc.dateComponents([.year, .month, .day, .hour, .minute, .second], from: date)
        XCTAssertEqual(parts.year, 2026)
        XCTAssertEqual(parts.month, 8)
        XCTAssertEqual(parts.day, 16)
        XCTAssertEqual(parts.hour, 9)
        XCTAssertEqual(parts.minute, 12)
        XCTAssertEqual(parts.second, 4)
    }

    /// The three shapes API.md rules out. `.iso8601` accepts the first two.
    func testEveryForbiddenShapeThrows() {
        for raw in [
            "2026-08-16T09:12:04.123Z",   // fractional seconds
            "2026-08-16T09:12:04+00:00",  // numeric offset, even though it is UTC
            "2026-08-16T09:12:04",        // no zone marker at all
            "2026-08-16 09:12:04Z",       // space instead of T
            "2026-8-16T09:12:04Z",        // single-digit month against MM
            "2026-08-16T09:12:04ZZ",      // trailing junk
            "16/08/2026 09:12:04",        // a locale's idea of a date
            "",
        ] {
            XCTAssertThrowsError(try decode(raw), "\"\(raw)\" decoded, and the contract says it must not")
        }
    }

    /// The formatter is pinned to POSIX and UTC, not the device's locale. A
    /// simulator on a Buddhist or Japanese calendar re-dates every timestamp in
    /// the app otherwise — the classic bug that only shows up in one region.
    func testTheFormatterIgnoresTheDeviceLocaleAndCalendar() {
        XCTAssertEqual(WireTime.formatter.locale.identifier, "en_US_POSIX")
        XCTAssertEqual(WireTime.formatter.timeZone.secondsFromGMT(), 0)
        XCTAssertEqual(WireTime.formatter.calendar.identifier, .gregorian)
        XCTAssertFalse(WireTime.formatter.isLenient, "lenient parsing accepts most of what this rejects")
    }

    /// Encoder and decoder agree, which is what lets the round-trip test in
    /// `WireDecodeTests` compare a re-encoded model against the fixture.
    func testEncodingIsTheInverseOfDecoding() throws {
        let date = try decode("2026-08-17T08:22:40Z")
        let out = try WireTime.encoder().encode(Box(ts: date))
        XCTAssertEqual(String(decoding: out, as: UTF8.self), #"{"ts":"2026-08-17T08:22:40Z"}"#)
    }
}

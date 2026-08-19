//
//  ContractFixture.swift
//  NADETests
//
//  Locating `docs/contract/` fixtures, in one place.
//
//  These were `private` methods on `ContractFixturesTests` until P2 needed them
//  for decoding too. The rules they encode are the ones D35 settled and are
//  worth restating, because both are load-bearing:
//
//  1. **The set is enumerated from the directory, never listed here.** A
//     hardcoded list of 24 names stayed green while `docs/contract/` grew to
//     57, and would let a brand-new malformed fixture into the bundle unchecked.
//  2. **An artefact-only run skips loudly.** Deriving the repo root from
//     `#filePath` means a CI job with no source tree has nothing to read;
//     `XCTSkip` says so instead of passing vacuously (EDGE E16).
//

import XCTest
@testable import NADE

enum ContractFixture {

    /// Not fixtures: the generator, the validator and the prose.
    static let nonFixtures: Set<String> = ["generate.py", "validate.py", "README.md"]

    private static let testBundle = Bundle(for: ContractFixturesTests.self)

    /// **The pinned now.**
    ///
    /// One instant, named once. It was written two ways — this ISO string in
    /// six places including `scripts/screenshots.sh`, and
    /// `Date(timeIntervalSince1970: 1_787_000_000)` in three — and the two were
    /// **not the same moment**, with nothing declaring they should be.
    static let pinnedNowText = "2026-08-17T12:00:00Z"
    static let pinnedNow = WireTime.formatter.date(from: pinnedNowText)!

    /// A directory under the repo root, or a loud skip.
    ///
    /// EDGE (E16): deriving the root from `#filePath` means an artefact-only CI
    /// run has nothing to read, and `XCTSkip` says so instead of passing
    /// vacuously. Five copies of this block existed before it was one.
    static func repoDirectory(_ relative: String, file: StaticString = #filePath) throws -> URL {
        let repoRoot = URL(fileURLWithPath: "\(file)")
            .deletingLastPathComponent()   // NADETests/
            .deletingLastPathComponent()   // repo root
        let dir = repoRoot.appendingPathComponent(relative, isDirectory: true)
        var isDirectory: ObjCBool = false
        guard FileManager.default.fileExists(atPath: dir.path, isDirectory: &isDirectory),
              isDirectory.boolValue
        else { throw XCTSkip("\(relative) is not next to \(file) — skipping") }
        return dir
    }

    /// `docs/contract/` in the working tree.
    static func directory(file: StaticString = #filePath, line: UInt = #line) throws -> URL {
        try repoDirectory("docs/contract", file: file)
    }

    /// Every fixture of one extension, by name, sorted. Enumerated, not listed.
    static func names(extension ext: String, file: StaticString = #filePath, line: UInt = #line) throws -> [String] {
        let dir = try directory(file: file, line: line)
        return try FileManager.default
            .contentsOfDirectory(atPath: dir.path)
            .filter { !nonFixtures.contains($0) }
            .filter { ($0 as NSString).pathExtension == ext }
            .map { ($0 as NSString).deletingPathExtension }
            .sorted()
    }

    /// The fixture as it sits in the **test bundle** — which is what the app
    /// under test would see, and what a stale copy would poison.
    static func url(_ name: String, ext: String = "json") throws -> URL {
        try XCTUnwrap(
            testBundle.url(forResource: name, withExtension: ext),
            "docs/contract/\(name).\(ext) is not in the test bundle"
        )
    }

    static func data(_ name: String, ext: String = "json") throws -> Data {
        try Data(contentsOf: try url(name, ext: ext))
    }

    static func text(_ name: String, ext: String) throws -> String {
        try String(contentsOf: try url(name, ext: ext), encoding: .utf8)
    }

    /// Decode a fixture through the app's own decoder — the same one the API
    /// client uses, so a decoder change cannot pass here and fail on the wire.
    static func decode<T: Decodable>(_ type: T.Type, from name: String) throws -> T {
        try WireTime.decoder().decode(type, from: try data(name))
    }

    // MARK: - The shapes every suite loads

    static func mailboxes() throws -> [WireMailbox] {
        try decode(WireMailboxes.self, from: "mailboxes").mailboxes
    }

    static func page(_ name: String = "threads") throws -> WireThreadPage {
        try decode(WireThreadPage.self, from: name)
    }

    static func thread(_ name: String = "thread") throws -> WireThread {
        try decode(WireThread.self, from: name)
    }

    /// The mailbox list plus one page of INBOX — what every store-backed suite
    /// starts from.
    static func seedInbox(_ store: MailStore, now: Date = ContractFixture.pinnedNow) async throws {
        try await store.replaceMailboxes(try mailboxes())
        try await store.applyThreadPage(try page(), mailboxID: "INBOX", resetting: true, now: now)
    }

    /// The raw JSON, for tests that need to plant a defect in a real payload
    /// rather than hand-write one that never resembled the wire.
    static func object(_ name: String) throws -> [String: Any] {
        try XCTUnwrap(
            JSONSerialization.jsonObject(with: try data(name)) as? [String: Any],
            "\(name).json is not a JSON object"
        )
    }
}

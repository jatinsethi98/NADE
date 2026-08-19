//
//  ModuleBoundaryTests.swift
//  NADETests
//
//  P2 A12. Two imports are confined to one directory each.
//
//  `import GRDB` lives only in `NADE/Store/`. That is what lets every test in
//  this target reach the database through `@testable import NADE` alone —
//  `@testable` exposes NADE's internals but does **not** re-export a package
//  module, so a record type leaking out of `Store/` would force GRDB into the
//  test target's own dependencies and a second copy into the link.
//
//  `URLSession` lives only in `NADE/API/`. A view model that reached for the
//  network directly would bypass the error envelope, the bearer header and the
//  offline handling that `OfflineUITests` depends on.
//
//  A separate file from `NoRawValuesTests` on purpose: that lint makes one
//  narrow claim about colours and fonts, and P1's §D1 is careful about its
//  scope. Widening it to mean "and also architecture" would blur both.
//

import XCTest

final class ModuleBoundaryTests: XCTestCase {

    private struct Rule {
        let symbol: String
        let allowedPrefix: String
        let why: String
    }

    private static let rules = [
        Rule(symbol: "import GRDB", allowedPrefix: "Store/",
             why: "the store returns NADE's own value types so nothing else needs the module"),
        Rule(symbol: "URLSession", allowedPrefix: "API/",
             why: "every request goes through APIClient, which owns the envelope and the bearer"),
    ]

    private func swiftFiles() throws -> [(path: String, text: String)] {
        let appRoot = try ContractFixture.repoDirectory("NADE")

        var found: [(String, String)] = []
        let enumerator = FileManager.default.enumerator(at: appRoot, includingPropertiesForKeys: nil)
        while let url = enumerator?.nextObject() as? URL {
            guard url.pathExtension == "swift" else { continue }
            let relative = url.path.replacingOccurrences(of: appRoot.path + "/", with: "")
            found.append((relative, try String(contentsOf: url, encoding: .utf8)))
        }
        XCTAssertGreaterThan(found.count, 20, "the sweep found almost nothing — it is not reading the app")
        return found
    }

    func testEachConfinedSymbolStaysInItsDirectory() throws {
        let files = try swiftFiles()

        for rule in Self.rules {
            let offenders = files
                .filter { !$0.path.hasPrefix(rule.allowedPrefix) }
                .filter { Self.mentions(rule.symbol, in: $0.text) }
                .map(\.path)

            XCTAssertEqual(
                offenders, [],
                """
                \(offenders) use `\(rule.symbol)` outside NADE/\(rule.allowedPrefix) — \(rule.why).
                """
            )

            // Non-vacuous: the symbol has to actually be somewhere, or the rule
            // is passing because the feature was deleted.
            let inPlace = files.filter { $0.path.hasPrefix(rule.allowedPrefix) }
                .contains { Self.mentions(rule.symbol, in: $0.text) }
            XCTAssertTrue(inPlace, "`\(rule.symbol)` appears nowhere at all — the rule is vacuous")
        }
    }

    /// Comments are stripped first: `MailStore`'s header explains the boundary
    /// and would otherwise trip the rule it describes.
    private static func mentions(_ symbol: String, in text: String) -> Bool {
        text
            .split(separator: "\n", omittingEmptySubsequences: false)
            .map { line -> Substring in
                if let comment = line.range(of: "//") { return line[line.startIndex..<comment.lowerBound] }
                return line
            }
            .contains { $0.contains(symbol) }
    }
}

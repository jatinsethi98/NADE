//
//  NoRawValuesTests.swift
//  NADETests
//
//  A source-tree lint. The design system is only a system if nothing routes
//  around it, so this fails the build when a raw design value appears in the
//  app target outside Theme.swift.
//
//  EDGE (E16): if the source tree is not present (an artefact-only CI run) the
//  test skips with a message rather than passing silently.
//

import XCTest

final class NoRawValuesTests: XCTestCase {

    private struct Rule {
        let pattern: String
        let why: String
    }

    /// The patterns are assembled from fragments so this file does not trip its
    /// own lint when it is ever moved under `NADE/`.
    private static let rules: [Rule] = [
        Rule(pattern: #"Color\(hex:"#,
             why: "hex literals belong in Theme.Color"),
        Rule(pattern: #"\.font\(\s*\.system\("#,
             why: "system fonts belong behind Theme.Font.icon / NIcon"),
        Rule(pattern: #"Font\.custom\("#,
             why: "custom fonts belong behind Theme.Font.heading / Theme.Font.body"),
        Rule(pattern: #"fixedSize:"#,
             why: "Font.custom(_:fixedSize:) freezes Dynamic Type — use relativeTo:"),
        Rule(pattern: #"(UI)?Color\(\s*red:"#,
             why: "component colours belong in Theme.Color"),
        Rule(pattern: #"Color\(\s*\.sRGB"#,
             why: "component colours belong in Theme.Color"),
        Rule(pattern: "#" + #"[0-9a-fA-F]{6}\b"#,
             why: "hex literals belong in Theme.Color"),
        Rule(pattern: #"\.foregroundStyle\(\s*\.(red|blue|green|orange|yellow|purple|pink|gray|secondary|primary)\b"#,
             why: "system palette colours are not in this design — use Theme.Color"),
    ]

    /// Only this file may hold raw design values.
    private static let allowedFiles: Set<String> = ["Theme.swift"]

    func testNoRawDesignValuesOutsideTheme() throws {
        let sources = try swiftSources()
        try XCTSkipIf(sources.isEmpty, "No app sources found next to \(#filePath) — skipping the source lint")

        var offences: [String] = []

        for url in sources where !Self.allowedFiles.contains(url.lastPathComponent) {
            let text = Self.strippingComments(try String(contentsOf: url, encoding: .utf8))
            let lines = text.components(separatedBy: .newlines)

            for rule in Self.rules {
                let regex = try NSRegularExpression(pattern: rule.pattern)
                for (index, line) in lines.enumerated() {
                    let range = NSRange(line.startIndex..., in: line)
                    guard regex.firstMatch(in: line, range: range) != nil else { continue }
                    offences.append("\(url.lastPathComponent):\(index + 1): \(line.trimmingCharacters(in: .whitespaces))\n    → \(rule.why)")
                }
            }
        }

        XCTAssertTrue(
            offences.isEmpty,
            "Raw design values outside Theme.swift:\n" + offences.joined(separator: "\n")
        )
    }

    /// Theme.swift is the one place these values are allowed — and it must
    /// actually contain them, otherwise the lint above is vacuously true
    /// because the tokens moved somewhere the scan cannot see.
    func testThemeIsWhereTheRawValuesLive() throws {
        let sources = try swiftSources()
        try XCTSkipIf(sources.isEmpty, "No app sources found — skipping")
        let theme = try XCTUnwrap(
            sources.first { $0.lastPathComponent == "Theme.swift" },
            "NADE/Theme.swift is missing"
        )
        let text = Self.strippingComments(try String(contentsOf: theme, encoding: .utf8))
        XCTAssertTrue(text.contains("Color(hex:"), "Theme.swift no longer defines colours from hex")
        XCTAssertTrue(text.contains(".system(size:"), "Theme.Font.icon no longer wraps the system font")
    }

    /// Every `Font.custom` in Theme.swift must pass `relativeTo:`, which is
    /// what makes every text style in the app honour Dynamic Type. EDGE (E1).
    func testEveryCustomFontIsDynamic() throws {
        let sources = try swiftSources()
        try XCTSkipIf(sources.isEmpty, "No app sources found — skipping")
        let theme = try XCTUnwrap(sources.first { $0.lastPathComponent == "Theme.swift" })
        let text = Self.strippingComments(try String(contentsOf: theme, encoding: .utf8))

        let customCalls = try NSRegularExpression(pattern: #"\.custom\([^)]*\)"#)
        let range = NSRange(text.startIndex..., in: text)
        let matches = customCalls.matches(in: text, range: range)
        XCTAssertFalse(matches.isEmpty, "Theme.swift has no .custom( font calls at all")

        for match in matches {
            guard let r = Range(match.range, in: text) else { continue }
            let call = String(text[r])
            XCTAssertTrue(
                call.contains("relativeTo:"),
                "Non-dynamic font call in Theme.swift: \(call)"
            )
        }
    }

    // MARK: Comment stripping

    /// The lint is about code, not prose: Theme.swift's own doc comments name
    /// the very APIs the rules ban ("`Font.custom(_:fixedSize:)` is banned
    /// here"), and a design note is allowed to quote a hex value. Comments are
    /// removed before matching, with line numbers preserved so a failure still
    /// points at the right line.
    static func strippingComments(_ source: String) -> String {
        var out = ""
        out.reserveCapacity(source.count)

        var inBlockComment = false
        var inString = false
        var escaped = false
        var index = source.startIndex

        while index < source.endIndex {
            let ch = source[index]
            let next = source.index(after: index)
            let peek: Character? = next < source.endIndex ? source[next] : nil

            if inBlockComment {
                if ch == "*" && peek == "/" {
                    inBlockComment = false
                    index = source.index(after: next)
                    continue
                }
                // Keep newlines so line numbers survive.
                if ch == "\n" { out.append(ch) }
                index = next
                continue
            }

            if inString {
                out.append(ch)
                if escaped { escaped = false }
                else if ch == "\\" { escaped = true }
                else if ch == "\"" { inString = false }
                index = next
                continue
            }

            if ch == "\"" {
                inString = true
                out.append(ch)
                index = next
                continue
            }

            if ch == "/" && peek == "/" {
                while index < source.endIndex && source[index] != "\n" {
                    index = source.index(after: index)
                }
                continue
            }

            if ch == "/" && peek == "*" {
                inBlockComment = true
                index = source.index(after: next)
                continue
            }

            out.append(ch)
            index = next
        }

        return out
    }

    // MARK: Source discovery

    /// `#filePath` is `<repo>/NADETests/NoRawValuesTests.swift`, so the app
    /// sources are `<repo>/NADE`.
    private func swiftSources() throws -> [URL] {
        let here = URL(fileURLWithPath: #filePath)
        let repoRoot = here.deletingLastPathComponent().deletingLastPathComponent()
        let appRoot = repoRoot.appendingPathComponent("NADE", isDirectory: true)

        var isDirectory: ObjCBool = false
        guard FileManager.default.fileExists(atPath: appRoot.path, isDirectory: &isDirectory),
              isDirectory.boolValue
        else { return [] }

        guard let walker = FileManager.default.enumerator(
            at: appRoot,
            includingPropertiesForKeys: [.isRegularFileKey],
            options: [.skipsHiddenFiles]
        ) else { return [] }

        return walker
            .compactMap { $0 as? URL }
            .filter { $0.pathExtension == "swift" }
            .sorted { $0.path < $1.path }
    }
}

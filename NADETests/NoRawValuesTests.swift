//
//  NoRawValuesTests.swift
//  NADETests
//
//  A source-tree lint over `NADE/**/*.swift`.
//
//  **What it claims, exactly.** No raw **colour** and no raw **font** outside
//  `NADE/Theme.swift`. That means: no hex literal, no `Color(red:…)`/
//  `UIColor(red:…)`, no `Color(.sRGB…)`, no system palette colour, no
//  `.system(size:)`, no `.custom(…)`, no `Font.custom(_:fixedSize:)`.
//
//  **What it does not claim.** It does not police geometry. `NChip.paddingH =
//  12` and `NStepper.gap = 12` are raw numbers that live beside the component
//  they describe, on purpose: they are per-component design facts, not shared
//  tokens, and hoisting them into `Theme` would make `Theme` a dictionary of
//  one-use constants. The earlier version of this file claimed "the design
//  system is only a system if nothing routes around it" while `NCard` carried
//  `.opacity(0.8)` and every component carried its own sizes. The claim is now
//  the narrower true one; `ComponentGeometryTests` is what holds the geometry.
//
//  **Why it is not evadable.** Matching is done on the whole file with newlines
//  folded to spaces, so a call split across lines is caught; the exemption is
//  by full path, not by `lastPathComponent`, so a second file called
//  `Theme.swift` is not exempt; and `testTheLintCatchesEveryEvadedForm` plants
//  one of each evasion and requires the matcher to find it.
//
//  EDGE (E16): if the source tree is not present (an artefact-only CI run) the
//  test skips with a message rather than passing silently.
//

import XCTest

final class NoRawValuesTests: XCTestCase {

    // MARK: - The matcher

    struct Rule {
        let pattern: String
        let why: String
    }

    struct Offence: Equatable, CustomStringConvertible {
        let file: String
        let line: Int
        let snippet: String
        let why: String

        var description: String { "\(file):\(line): \(snippet)\n    → \(why)" }
    }

    /// The patterns are assembled from fragments so this file does not trip its
    /// own lint if it is ever moved under `NADE/`.
    static let rules: [Rule] = [
        Rule(pattern: #"\bColor\(\s*hex:"#,
             why: "hex literals belong in Theme.Color"),
        // Catches `Font.system(`, `.font(.system(`, `.system(size:` on its own
        // line, and any of them split across lines.
        Rule(pattern: #"\.system\(\s*size:"#,
             why: "system fonts belong behind Theme.Font.icon / NIcon"),
        // Catches `Font.custom(` *and* the leading-dot `.font(.custom(` form.
        Rule(pattern: #"\.custom\(\s*""#,
             why: "custom fonts belong behind Theme.Font.heading / Theme.Font.body"),
        Rule(pattern: #"fixedSize:"#,
             why: "Font.custom(_:fixedSize:) freezes Dynamic Type — use relativeTo:"),
        Rule(pattern: #"\b(UI)?Color\(\s*red:"#,
             why: "component colours belong in Theme.Color"),
        Rule(pattern: #"\bColor\(\s*\.sRGB"#,
             why: "component colours belong in Theme.Color"),
        Rule(pattern: "#" + #"[0-9a-fA-F]{6}\b"#,
             why: "hex literals belong in Theme.Color"),
        // Any way a system palette colour can be reached, as a value or as an
        // argument to the modifiers that take one.
        Rule(pattern: #"\bU?I?Color\.(red|blue|green|orange|yellow|purple|pink|gray|grey|brown|mint|teal|cyan|indigo|black|white)\b"#,
             why: "system palette colours are not in this design — use Theme.Color"),
        Rule(pattern: #"\bUIColor\.system"#,
             why: "system palette colours are not in this design — use Theme.Color"),
        Rule(pattern: #"\.(foregroundStyle|foregroundColor|tint|accentColor|fill|stroke|strokeBorder|background|shadow)\(\s*\.(red|blue|green|orange|yellow|purple|pink|gray|grey|brown|mint|teal|cyan|indigo|black|white|secondary|primary)\b"#,
             why: "system palette colours are not in this design — use Theme.Color"),
    ]

    /// The one file allowed to hold raw colour and font values, matched on its
    /// **path**. `lastPathComponent` would exempt any second `Theme.swift`
    /// anywhere in the tree.
    static let allowedPathSuffixes = ["/NADE/Theme.swift"]

    /// The whole matcher, over a single file's text. Pure, so
    /// `testTheLintCatchesEveryEvadedForm` can feed it planted violations
    /// without touching the working tree.
    static func offences(in source: String, file: String = "<planted>") throws -> [Offence] {
        let stripped = strippingComments(source)
        // Newlines become spaces — same length, so match offsets still map to
        // the original — which is what lets a pattern span a line break.
        let flat = String(stripped.map { $0 == "\n" ? " " : $0 })
        let lineStarts = Self.lineStarts(of: stripped)

        var found: [Offence] = []
        for rule in rules {
            let regex = try NSRegularExpression(pattern: rule.pattern)
            let range = NSRange(flat.startIndex..., in: flat)
            for match in regex.matches(in: flat, range: range) {
                let line = lineStarts.lastIndex { $0 <= match.range.location }.map { $0 + 1 } ?? 1
                guard let r = Range(match.range, in: flat) else { continue }
                found.append(Offence(
                    file: file,
                    line: line,
                    snippet: String(flat[r]).trimmingCharacters(in: .whitespaces),
                    why: rule.why
                ))
            }
        }
        return found
    }

    private static func lineStarts(of text: String) -> [Int] {
        var starts = [0]
        for (offset, ch) in text.utf16.enumerated() where ch == 10 {
            starts.append(offset + 1)
        }
        return starts
    }

    // MARK: - The lint itself

    func testNoRawColoursOrFontsOutsideTheme() throws {
        let sources = try swiftSources()
        try XCTSkipIf(sources.isEmpty, "No app sources found next to \(#filePath) — skipping the source lint")

        var offences: [Offence] = []
        for url in sources where !Self.isAllowed(url) {
            offences += try Self.offences(
                in: try String(contentsOf: url, encoding: .utf8),
                file: url.lastPathComponent
            )
        }

        XCTAssertTrue(
            offences.isEmpty,
            "Raw colour/font values outside Theme.swift:\n"
                + offences.map(\.description).joined(separator: "\n")
        )
    }

    private static func isAllowed(_ url: URL) -> Bool {
        allowedPathSuffixes.contains { url.path.hasSuffix($0) }
    }

    /// The exemption must be a *path*, so a second `Theme.swift` anywhere else
    /// in the tree is still linted.
    func testTheExemptionIsAPathNotAFilename() {
        XCTAssertTrue(Self.isAllowed(URL(fileURLWithPath: "/repo/NADE/Theme.swift")))
        XCTAssertFalse(Self.isAllowed(URL(fileURLWithPath: "/repo/NADE/Gallery/Theme.swift")))
        XCTAssertFalse(Self.isAllowed(URL(fileURLWithPath: "/repo/NADE/Components/Theme.swift")))
    }

    // MARK: - Proof the lint is not evadable

    /// Every one of these slipped through the previous matcher. Each is a
    /// planted violation; the test fails if the lint does not find it.
    func testTheLintCatchesEveryEvadedForm() throws {
        let evasions: [(name: String, source: String)] = [
            ("Font.custom",
             #"let f = Font.custom("Lora-Regular", size: 14, relativeTo: .body)"#),
            ("leading-dot .font(.custom(",
             #"Text("x").font(.custom("Lora-Regular", size: 14, relativeTo: .body))"#),
            (".custom split across lines",
             """
             Text("x").font(
                 .custom(
                     "Lora-Regular",
                     size: 14
                 )
             )
             """),
            ("Font.system",
             #"let f = Font.system(size: 18, weight: .light)"#),
            (".font(.system( split across lines",
             """
             Text("x")
                 .font(
                     .system(
                         size: 18
                     )
                 )
             """),
            ("Font.custom fixedSize",
             #"let f = Font.custom("Lora-Regular", fixedSize: 14)"#),
            ("Color(hex:",
             "let c = Color(hex: 0xb68235)"),
            ("Color(hex: split across lines",
             """
             let c = Color(
                 hex: 0xb68235
             )
             """),
            ("Color(red:green:blue:)",
             "let c = Color(red: 0.71, green: 0.51, blue: 0.21)"),
            ("Color(red: split across lines",
             """
             let c = Color(
                 red: 0.71,
                 green: 0.51,
                 blue: 0.21
             )
             """),
            ("UIColor(red:",
             "let c = UIColor(red: 0.71, green: 0.51, blue: 0.21, alpha: 1)"),
            ("Color(.sRGB",
             "let c = Color(.sRGB, red: 0.7, green: 0.5, blue: 0.2, opacity: 1)"),
            ("hex string literal",
             ##"let c = "#b68235""##),
            ("system palette value",
             "let c = Color.red"),
            ("system palette in foregroundStyle",
             #"Text("x").foregroundStyle(.secondary)"#),
            ("system palette in fill",
             "Circle().fill(.blue)"),
            ("UIColor.systemBackground",
             "let c = UIColor.systemBackground"),
        ]

        for evasion in evasions {
            let found = try Self.offences(in: evasion.source, file: evasion.name)
            XCTAssertFalse(
                found.isEmpty,
                "The lint does not catch \"\(evasion.name)\":\n\(evasion.source)"
            )
        }
    }

    /// …and does not fire on the things it must not: prose, and legitimate
    /// non-colour numbers.
    func testTheLintDoesNotFireOnCommentsOrGeometry() throws {
        let benign = """
        // Font.custom(_:fixedSize:) is banned here; the DS accent is #b68235.
        /* Color(hex: 0x201f1d) appears only in Theme.swift. */
        struct X {
            static let paddingH: CGFloat = 12
            static let gap: CGFloat = 9.2
            var body: some View {
                Text("x")
                    .font(Theme.Font.body(14))
                    .foregroundStyle(Theme.Color.ink62)
            }
        }
        """
        XCTAssertEqual(try Self.offences(in: benign), [])
    }

    /// Line numbers survive comment stripping and the newline fold, so a real
    /// failure still points at the right line.
    func testOffencesReportTheirRealLineNumber() throws {
        let source = """
        import SwiftUI
        // a comment
        let a = 1

        let c = Color(hex: 0xb68235)
        """
        let found = try Self.offences(in: source)
        XCTAssertEqual(found.count, 1)
        XCTAssertEqual(found.first?.line, 5)
    }

    // MARK: - Theme still holds the values

    /// Theme.swift is the one place these values are allowed — and it must
    /// actually contain them, otherwise the lint above is vacuously true
    /// because the tokens moved somewhere the scan cannot see.
    func testThemeIsWhereTheRawValuesLive() throws {
        let sources = try swiftSources()
        try XCTSkipIf(sources.isEmpty, "No app sources found — skipping")
        let theme = try XCTUnwrap(
            sources.first { Self.isAllowed($0) },
            "NADE/Theme.swift is missing"
        )
        let text = Self.strippingComments(try String(contentsOf: theme, encoding: .utf8))
        XCTAssertTrue(text.contains("Color(hex:"), "Theme.swift no longer defines colours from hex")
        XCTAssertTrue(text.contains(".system(size:"), "Theme.Font.icon no longer wraps the system font")

        // And it is genuinely the only exempt file.
        XCTAssertEqual(sources.filter { Self.isAllowed($0) }.count, 1)
    }

    /// Every `Font.custom` in Theme.swift must pass `relativeTo:`, which is
    /// what makes every text style in the app honour Dynamic Type. EDGE (E1).
    func testEveryCustomFontIsDynamic() throws {
        let sources = try swiftSources()
        try XCTSkipIf(sources.isEmpty, "No app sources found — skipping")
        let theme = try XCTUnwrap(sources.first { Self.isAllowed($0) })
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

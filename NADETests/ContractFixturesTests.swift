//
//  ContractFixturesTests.swift
//  NADETests
//
//  `docs/contract/` is wired into this target as a
//  PBXFileSystemSynchronizedRootGroup, so every fixture — including the `.sse`
//  streams — is copied into NADETests.xctest verbatim and stays current as the
//  backend lane adds files. No copy phase to maintain, no path to keep in sync.
//
//  The set of fixtures is **enumerated from the directory**, never listed here.
//  A hardcoded list means the group can synchronise a brand-new malformed
//  fixture into the bundle and nothing checks it; it also silently stops
//  covering anything the backend adds. `docs/contract/` grew from 24 files to
//  57 while a list of 24 names stayed green.
//
//  P2 onward decodes these into real models. P1's job is only to prove they are
//  reachable and well-formed, so a rename, a drop or a malformed stream fails
//  here rather than four phases later. EDGE (E15).
//

import XCTest

final class ContractFixturesTests: XCTestCase {

    private var bundle: Bundle { Bundle(for: ContractFixturesTests.self) }

    /// Not fixtures: the generator, the validator and the prose.
    private static let nonFixtures: Set<String> = ["generate.py", "validate.py", "README.md"]

    /// The fixture directory in the repo, from `#filePath`. EDGE (E16): an
    /// artefact-only run has no source tree, and skipping loudly beats
    /// pretending to have checked.
    private func contractDirectory() throws -> URL {
        let repoRoot = URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent()
            .deletingLastPathComponent()
        let dir = repoRoot.appendingPathComponent("docs/contract", isDirectory: true)
        var isDirectory: ObjCBool = false
        guard FileManager.default.fileExists(atPath: dir.path, isDirectory: &isDirectory),
              isDirectory.boolValue
        else { throw XCTSkip("docs/contract is not next to \(#filePath) — skipping the fixture sweep") }
        return dir
    }

    private func fixtureNames(extension ext: String) throws -> [String] {
        let dir = try contractDirectory()
        return try FileManager.default
            .contentsOfDirectory(atPath: dir.path)
            .filter { !Self.nonFixtures.contains($0) }
            .filter { ($0 as NSString).pathExtension == ext }
            .map { ($0 as NSString).deletingPathExtension }
            .sorted()
    }

    // MARK: - Every file in the directory reaches the bundle

    func testEveryJSONFixtureInTheDirectoryIsBundledAndParses() throws {
        let names = try fixtureNames(extension: "json")
        // A floor, not a pin: the set is enumerated, and the backend lane is
        // free to add to it. This only catches enumeration itself breaking.
        XCTAssertGreaterThanOrEqual(names.count, 50, "docs/contract has \(names.count) JSON fixtures — enumeration is broken")

        for name in names {
            let url = try XCTUnwrap(
                bundle.url(forResource: name, withExtension: "json"),
                "docs/contract/\(name).json is in the repo but not in the test bundle"
            )
            let data = try Data(contentsOf: url)
            XCTAssertFalse(data.isEmpty, "\(name).json is empty")
            XCTAssertNoThrow(
                try JSONSerialization.jsonObject(with: data),
                "\(name).json is not valid JSON"
            )
        }
    }

    /// The bundle must not contain a fixture the repo no longer has, which is
    /// how a stale copy survives a rename.
    func testTheBundleHasNothingTheDirectoryDoesNot() throws {
        let onDisk = Set(try fixtureNames(extension: "json"))
        let bundled = Set(
            (bundle.urls(forResourcesWithExtension: "json", subdirectory: nil) ?? [])
                .map { $0.deletingPathExtension().lastPathComponent }
        )
        // The test bundle carries only these fixtures plus Xcode's own plists,
        // which are not `.json`.
        XCTAssertEqual(bundled.subtracting(onDisk), [], "stale JSON fixtures in the test bundle")
        XCTAssertEqual(onDisk.subtracting(bundled), [], "fixtures on disk that never reached the bundle")
    }

    // MARK: - SSE streams are real streams

    /// Every way a stream can be wrong, as a list of problems. Pure, so
    /// `testTheStreamValidatorRejectsEveryMalformedShape` can feed it planted
    /// streams — the same discipline as `NoRawValuesTests`.
    ///
    /// `docs/contract/README.md`: "SSE files are exact wire bytes:
    /// `event: <name>\ndata: <json>\n`, a blank line between events, `route`
    /// first, exactly one terminal `done` **or** `error`, and a trailing blank
    /// line." That is what gets checked — not `contains("event: route")`, which
    /// any file holding the three substrings in any order satisfies.
    ///
    /// The framing half mirrors `docs/contract/validate.py`'s `parse_sse`
    /// block-for-block, so the two validators cannot disagree about what a
    /// well-formed stream is.
    ///
    /// **Not claimed:** the *payloads*. `validate.py` checks every frame against
    /// the shapes in API.md; P1 has no models yet, so this checks only that each
    /// payload is valid JSON. P2 decodes them.
    static func problems(inStream text: String) -> [String] {
        var problems: [String] = []
        var events: [(name: String, data: String)] = []

        // The framing is parsed in **blocks split on the blank line**, exactly
        // as `docs/contract/validate.py` does, not line by line.
        //
        // A line-by-line reader pairs each `event:` with the next `data:` and
        // skips blank lines, so it silently accepts three streams the contract
        // forbids and `validate.py` rejects: two events with no blank line
        // between them, a blank line *inside* an event, and a doubled blank
        // line. All three passed here before.
        guard text.hasSuffix("\n\n") else {
            return ["does not end with the blank line that terminates the last event"]
        }

        for (index, block) in text.components(separatedBy: "\n\n").enumerated() {
            // The trailing blank line leaves one empty component at the end.
            if block.isEmpty { continue }

            let lines = block.components(separatedBy: "\n")
            guard lines.count == 2 else {
                problems.append(
                    "event \(index) is not an `event:`/`data:` pair separated from its "
                        + "neighbours by exactly one blank line: \(String(reflecting: block))"
                )
                continue
            }
            guard let name = lines[0].dropPrefixIfPresent("event: "),
                  let payload = lines[1].dropPrefixIfPresent("data: ")
            else {
                problems.append("event \(index) is malformed: \(String(reflecting: block))")
                continue
            }
            if name.isEmpty { problems.append("event \(index) has an empty event name") }
            if (try? JSONSerialization.jsonObject(with: Data(payload.utf8))) == nil {
                problems.append("event \(index): `\(name)` payload is not valid JSON: \(payload)")
            }
            events.append((name, payload))
        }

        if events.isEmpty {
            problems.append("no events at all")
            return problems
        }

        // `route` is **first**, and appears exactly once.
        if events[0].name != "route" { problems.append("opens with `\(events[0].name)`, not `route`") }
        let routes = events.filter { $0.name == "route" }.count
        if routes != 1 { problems.append("has \(routes) `route` events; the contract allows exactly one") }

        // Exactly one terminal event, and it is the last one.
        let terminals = events.enumerated().filter { $0.element.name == "done" || $0.element.name == "error" }
        if terminals.count != 1 {
            problems.append("has \(terminals.count) terminal events; the contract allows exactly one")
        } else if terminals[0].offset != events.count - 1 {
            problems.append("the terminal `\(terminals[0].element.name)` is not the last event")
        }

        return problems
    }

    func testEverySSEFixtureIsAWellFormedStream() throws {
        let names = try fixtureNames(extension: "sse")
        // Four as of the settled contract: answer, results, agent_draft, error.
        // The hardcoded list this replaces named three and never saw the fourth.
        XCTAssertGreaterThanOrEqual(names.count, 4, "docs/contract has \(names.count) SSE fixtures — enumeration is broken")

        for name in names {
            let url = try XCTUnwrap(
                bundle.url(forResource: name, withExtension: "sse"),
                "docs/contract/\(name).sse is in the repo but not in the test bundle"
            )
            let text = try String(contentsOf: url, encoding: .utf8)
            let problems = Self.problems(inStream: text)
            XCTAssertTrue(
                problems.isEmpty,
                "\(name).sse is not a well-formed stream:\n  " + problems.joined(separator: "\n  ")
            )
        }
    }

    /// The validator has to be able to *fail*, or every assertion above is
    /// vacuous. Each of these passed the previous
    /// `contains("event: route") && contains("data:")` check.
    func testTheStreamValidatorRejectsEveryMalformedShape() {
        let malformed: [(name: String, stream: String)] = [
            ("route is not first",
             "event: token\ndata: {\"text\":\"x\"}\n\nevent: route\ndata: {\"kind\":\"answer\"}\n\nevent: done\ndata: {}\n\n"),
            ("no route at all",
             "event: token\ndata: {\"text\":\"x\"}\n\nevent: done\ndata: {}\n\n"),
            ("two terminal events",
             "event: route\ndata: {\"kind\":\"answer\"}\n\nevent: done\ndata: {}\n\nevent: error\ndata: {}\n\n"),
            ("no terminal event",
             "event: route\ndata: {\"kind\":\"answer\"}\n\nevent: token\ndata: {\"text\":\"x\"}\n\n"),
            ("terminal is not last",
             "event: route\ndata: {\"kind\":\"answer\"}\n\nevent: done\ndata: {}\n\nevent: token\ndata: {\"text\":\"x\"}\n\n"),
            ("payload is not JSON",
             "event: route\ndata: kind=answer\n\nevent: done\ndata: {}\n\n"),
            ("data with no event",
             "data: {\"kind\":\"answer\"}\n\nevent: done\ndata: {}\n\n"),
            ("event with no data",
             "event: route\n\nevent: done\ndata: {}\n\n"),
            ("a stray line",
             "event: route\ndata: {\"kind\":\"answer\"}\nid: 7\n\nevent: done\ndata: {}\n\n"),
            ("no trailing blank line",
             "event: route\ndata: {\"kind\":\"answer\"}\n\nevent: done\ndata: {}\n"),
            ("repeated route",
             "event: route\ndata: {\"kind\":\"answer\"}\n\nevent: route\ndata: {\"kind\":\"results\"}\n\nevent: done\ndata: {}\n\n"),
            // The three the line-by-line reader could not see. `validate.py`
            // rejects all three; this validator used to accept all three.
            ("no blank line between events",
             "event: route\ndata: {\"kind\":\"answer\"}\nevent: done\ndata: {}\n\n"),
            ("a blank line inside an event",
             "event: route\n\ndata: {\"kind\":\"answer\"}\n\nevent: done\ndata: {}\n\n"),
            ("two blank lines between events",
             "event: route\ndata: {\"kind\":\"answer\"}\n\n\nevent: done\ndata: {}\n\n"),
        ]

        for case_ in malformed {
            XCTAssertFalse(
                Self.problems(inStream: case_.stream).isEmpty,
                "the validator accepts a stream that is malformed — \(case_.name):\n\(case_.stream)"
            )
        }

        // …and accepts the shape the contract describes.
        XCTAssertEqual(
            Self.problems(inStream: "event: route\ndata: {\"kind\":\"answer\"}\n\nevent: token\ndata: {\"text\":\"x\"}\n\nevent: done\ndata: {}\n\n"),
            []
        )
    }
}

private extension String {
    func dropPrefixIfPresent(_ prefix: String) -> String? {
        hasPrefix(prefix) ? String(dropFirst(prefix.count)) : nil
    }
}

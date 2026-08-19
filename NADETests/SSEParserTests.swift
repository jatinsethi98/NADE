//
//  SSEParserTests.swift
//  NADETests
//
//  The parser against the four real streams, and against the ways a stream
//  arrives that a whole-file test never exercises.
//
//  The chunking tests are the point. A stream arrives in whatever pieces TCP
//  chooses, and `ask_results.sse` contains multi-byte characters — so a parser
//  that decoded each chunk as UTF-8 on arrival would pass every test here that
//  feeds it a whole file, and corrupt real traffic.
//

import XCTest
@testable import NADE

final class SSEParserTests: XCTestCase {
    private func stream(_ name: String) throws -> Data {
        let url = try ContractFixture.directory().appendingPathComponent("\(name).sse")
        return try Data(contentsOf: url)
    }

    /// Feed the whole stream at once.
    private func events(of name: String) throws -> [AskEvent] {
        var parser = SSEParser()
        var events = try parser.consume(try stream(name))
        events.append(contentsOf: try parser.finish())
        return events
    }

    // MARK: - The four contract streams

    func testTheAnswerStreamParses() throws {
        let events = try events(of: "ask_answer")

        guard case .route(let kind) = events.first else {
            return XCTFail("the first event is always route")
        }
        XCTAssertEqual(kind, .answer)

        let text = events.compactMap { if case .token(let t) = $0 { t } else { nil } }.joined()
        XCTAssertEqual(
            text,
            "Priya proposed a 30-minute intro with the head of design, then a portfolio session the week after."
        )

        guard case .done(let sources) = events.last else {
            return XCTFail("an answer stream ends with done")
        }
        XCTAssertEqual(sources.count, 2)
        XCTAssertEqual(sources.first?.subject, "Staff Product Designer at Kettle")
    }

    func testTheResultsStreamCarriesTokensThenHits() throws {
        let events = try events(of: "ask_results")

        guard case .route(.results) = events.first else {
            return XCTFail("expected a results route")
        }

        // The design renders a prose lead-in *above* the hits, so the tokens
        // must all precede the results frame.
        let resultsIndex = try XCTUnwrap(events.firstIndex { if case .results = $0 { true } else { false } })
        let tokensAfter = events[resultsIndex...].contains { if case .token = $0 { true } else { false } }
        XCTAssertFalse(tokensAfter, "a token arrived after the hits")

        guard case .results(let threads) = events[resultsIndex] else {
            return XCTFail("expected results")
        }
        XCTAssertEqual(threads.count, 4)
        XCTAssertFalse(threads[0].subject.isEmpty)
    }

    func testTheAgentDraftStreamCarriesBothSpans() throws {
        let events = try events(of: "ask_agent_draft")

        guard case .route(.agentDraft) = events.first else {
            return XCTFail("expected an agent_draft route")
        }
        guard case .draft(let draft) = events[1] else {
            return XCTFail("expected a draft frame")
        }

        XCTAssertEqual(draft.name, "Recruiter Next Steps")
        XCTAssertEqual(draft.status, .draft)
        XCTAssertEqual(draft.tool, .writeNote)
        XCTAssertTrue(draft.approvalRequired)
        // Both spans must be literal substrings, or the composed sentence would
        // not be the one the server compiled.
        XCTAssertTrue(draft.nlDefinition.contains(draft.whenSpan))
        XCTAssertTrue(draft.nlDefinition.contains(draft.doSpan))
    }

    /// An error may follow partial tokens, and **no `done` follows it**. The
    /// client keeps what it received and shows the error beneath.
    func testTheErrorStreamEndsWithoutADone() throws {
        let events = try events(of: "ask_error")

        XCTAssertTrue(events.contains { if case .token = $0 { true } else { false } })
        guard case .error(let code, let message) = events.last else {
            return XCTFail("an error stream ends on error")
        }
        XCTAssertEqual(code, .upstreamUnavailable)
        XCTAssertFalse(message.isEmpty)
        XCTAssertFalse(
            events.contains { if case .done = $0 { true } else { false } },
            "done must not follow error"
        )
    }

    /// Every stream opens with exactly one `route` and closes with exactly one
    /// terminal event.
    func testEveryContractStreamHasOneRouteAndOneTerminal() throws {
        for name in try ContractFixture.names(extension: "sse") {
            let events = try events(of: name)

            let routes = events.filter { if case .route = $0 { true } else { false } }
            XCTAssertEqual(routes.count, 1, "\(name): exactly one route")
            guard case .route = events.first else {
                return XCTFail("\(name): the first event is always route")
            }

            let terminals = events.filter(\.isTerminal)
            XCTAssertEqual(terminals.count, 1, "\(name): exactly one terminal event")
            XCTAssertTrue(events.last?.isTerminal == true, "\(name): nothing follows the terminal")
        }
    }

    // MARK: - Chunking

    /// **The test the parser exists for.** Split every stream at every byte
    /// boundary; the events must be identical however it arrives.
    func testEveryStreamParsesIdenticallyAtEveryChunkBoundary() throws {
        for name in try ContractFixture.names(extension: "sse") {
            let whole = try stream(name)
            let expected = try events(of: name)

            for split in 1..<whole.count {
                var parser = SSEParser()
                var events = try parser.consume(whole.prefix(split))
                events.append(contentsOf: try parser.consume(whole.suffix(from: split)))
                events.append(contentsOf: try parser.finish())

                XCTAssertEqual(
                    events, expected,
                    "\(name) parsed differently when split at byte \(split)"
                )
            }
        }
    }

    /// One byte at a time — the worst case, and the one that proves nothing is
    /// emitted before its blank-line terminator arrives.
    func testAStreamDeliveredOneByteAtATimeIsIdentical() throws {
        for name in try ContractFixture.names(extension: "sse") {
            let whole = try stream(name)
            var parser = SSEParser()
            var events: [AskEvent] = []
            for byte in whole {
                events.append(contentsOf: try parser.consume(Data([byte])))
            }
            events.append(contentsOf: try parser.finish())
            XCTAssertEqual(events, try self.events(of: name), "\(name) byte by byte")
        }
    }

    /// A multi-byte codepoint split across two chunks must survive. This is the
    /// concrete failure mode of decoding each chunk as it arrives.
    func testACodepointSplitAcrossChunksSurvives() throws {
        let text = "\u{1F680} \u{8BBE}\u{8BA1}"
        let block = Data("event: route\ndata: {\"kind\":\"answer\"}\n\nevent: token\ndata: {\"text\":\"\(text)\"}\n\n".utf8)

        // Split inside the emoji's four bytes.
        let emojiStart = try XCTUnwrap(block.firstRange(of: Data("\u{1F680}".utf8))).lowerBound
        var parser = SSEParser()
        var events = try parser.consume(block.prefix(emojiStart + 2))
        events.append(contentsOf: try parser.consume(block.suffix(from: emojiStart + 2)))

        guard case .token(let received) = events.last else {
            return XCTFail("expected a token")
        }
        XCTAssertEqual(received, text, "a codepoint was corrupted by a chunk boundary")
    }

    // MARK: - Malformed input

    func testNothingIsEmittedBeforeABlockIsComplete() throws {
        var parser = SSEParser()
        XCTAssertEqual(try parser.consume(Data("event: route\n".utf8)), [])
        XCTAssertEqual(try parser.consume(Data("data: {\"kind\":\"answer\"}".utf8)), [])
        XCTAssertEqual(try parser.consume(Data("\n\n".utf8)), [.route(.answer)])
    }

    func testAnEventAfterTheTerminalIsRefused() throws {
        var parser = SSEParser()
        _ = try parser.consume(Data("event: route\ndata: {\"kind\":\"answer\"}\n\n".utf8))
        _ = try parser.consume(Data("event: done\ndata: {\"sources\":[]}\n\n".utf8))

        XCTAssertThrowsError(
            try parser.consume(Data("event: token\ndata: {\"text\":\"late\"}\n\n".utf8))
        ) { error in
            XCTAssertEqual(error as? SSEParser.Failure, .eventAfterTerminal("token"))
        }
    }

    func testMalformedBlocksAreRejected() throws {
        let cases: [(String, String)] = [
            ("data: {\"kind\":\"answer\"}\n\n", "no event line"),
            ("event: route\n\n", "no data line"),
            ("event: route\nevent: token\ndata: {}\n\n", "two event lines"),
            ("event: route\ndata: {}\ndata: {}\n\n", "two data lines"),
            ("route\ndata: {}\n\n", "a line that is neither"),
        ]
        for (raw, why) in cases {
            var parser = SSEParser()
            XCTAssertThrowsError(try parser.consume(Data(raw.utf8)), why)
        }
    }

    func testAnUnknownEventNameIsRejected() throws {
        var parser = SSEParser()
        XCTAssertThrowsError(
            try parser.consume(Data("event: teleport\ndata: {}\n\n".utf8))
        ) { error in
            XCTAssertEqual(error as? SSEParser.Failure, .unknownEvent("teleport"))
        }
    }

    func testAPayloadThatIsNotTheEventsShapeIsRejected() throws {
        for raw in [
            "event: route\ndata: not json\n\n",
            "event: route\ndata: {\"nope\":1}\n\n",
            "event: token\ndata: {\"text\":42}\n\n",
        ] {
            var parser = SSEParser()
            XCTAssertThrowsError(try parser.consume(Data(raw.utf8)), raw)
        }
    }

    /// A stream that stops mid-frame is an error, not a silent truncation. The
    /// contract requires a trailing blank line; this notices its absence.
    func testAStreamThatEndsMidFrameIsAnError() throws {
        var parser = SSEParser()
        _ = try parser.consume(Data("event: route\ndata: {\"kind\":\"answer\"}\n\nevent: token\n".utf8))
        XCTAssertThrowsError(try parser.finish())
    }

    /// EDGE (empty input): nothing in yields nothing — but closing there is a
    /// **truncated stream**, not a successful empty one. Every stream ends with
    /// exactly one terminal event, so a connection that produced none did not
    /// finish. The first version of this test asserted the opposite and
    /// enshrined the bug.
    func testAnEmptyStreamYieldsNothingAndIsTruncatedAtEOF() throws {
        var parser = SSEParser()
        XCTAssertEqual(try parser.consume(Data()), [])
        XCTAssertThrowsError(try parser.finish()) { error in
            XCTAssertEqual(error as? SSEParser.Failure, .truncatedStream)
        }
    }

    /// The dangerous case: a connection that drops after a **complete** frame.
    /// Every frame parsed, so nothing else would notice.
    func testAStreamThatEndsAfterACompleteFrameIsTruncated() throws {
        var parser = SSEParser()
        let events = try parser.consume(
            Data("event: route\ndata: {\"kind\":\"answer\"}\n\nevent: token\ndata: {\"text\":\"half an \"}\n\n".utf8)
        )
        XCTAssertEqual(events.count, 2, "both frames are complete and parse")

        XCTAssertThrowsError(try parser.finish()) { error in
            XCTAssertEqual(error as? SSEParser.Failure, .truncatedStream)
        }
    }

    /// Keep-alive comments and CRLF framing are both legal SSE.
    func testCommentsAndCarriageReturnsAreTolerated() throws {
        var keepAliveOnly = SSEParser()
        XCTAssertEqual(try keepAliveOnly.consume(Data(": keep-alive\n\n".utf8)), [], "keep-alive")

        for terminator in ["\n\n", "\r\n\r\n"] {
            var crlf = SSEParser()
            XCTAssertEqual(
                try crlf.consume(
                    Data("event: route\r\ndata: {\"kind\":\"results\"}\r\n\(terminator)".utf8)
                ),
                [.route(.results)],
                "CRLF framing terminated by \(terminator.debugDescription)"
            )
        }

        var both = SSEParser()
        XCTAssertEqual(
            try both.consume(
                Data(": keep-alive\n\nevent: route\r\ndata: {\"kind\":\"results\"}\r\n\n".utf8)
            ),
            [.route(.results)],
            "both together"
        )
    }

    /// A zero-token answer is legal — "zero or more, in order" — and must not
    /// look like a failure.
    func testAStreamWithNoTokensIsLegal() throws {
        var parser = SSEParser()
        let events = try parser.consume(
            Data("event: route\ndata: {\"kind\":\"answer\"}\n\nevent: done\ndata: {\"sources\":[]}\n\n".utf8)
        )
        XCTAssertEqual(events, [.route(.answer), .done(sources: [])])
    }
}

//
//  SSEParser.swift
//  NADE
//
//  `text/event-stream` → [`AskEvent`], incrementally.
//
//  Pure and synchronous on purpose. The *transport* is P6's — `POST /ask` does
//  not exist yet — but the screen it feeds is P3's, and a streaming screen
//  built against static states is the wrong screen. So the parser ships now,
//  tested byte for byte against `docs/contract/*.sse`, and P6 adds a
//  `URLSession.bytes` source in front of it.
//
//  It lives in `NADE/API/` because `ModuleBoundaryTests` confines `URLSession`
//  to that directory, and P6's transport will sit beside it.
//
//  **Bytes in, not strings.** A stream arrives in arbitrary chunks, and a chunk
//  boundary lands wherever TCP puts it — including the middle of a multi-byte
//  codepoint. `ask_results.sse` contains such characters, so a parser that
//  decoded each chunk as UTF-8 on arrival would corrupt real traffic while
//  passing every whole-file test.
//

import Foundation

/// Accumulates bytes and yields whole events as they complete.
nonisolated struct SSEParser {
    /// Bytes received but not yet part of a complete block.
    private var buffer = Data()
    /// Set once a terminal event has been seen, so anything after it is
    /// refused rather than rendered.
    private(set) var isFinished = false

    init() {}

    /// A frame the parser could not make sense of.
    enum Failure: Error, Equatable {
        /// A block that was not exactly an `event:`/`data:` pair.
        case malformedBlock(String)
        /// `data:` was not JSON, or not the shape the event name implies.
        case undecodable(event: String)
        /// An event name `API.md` §4 does not define.
        case unknownEvent(String)
        /// Anything after `done` or `error`.
        case eventAfterTerminal(String)
        /// The connection closed without a terminal event, so what is on screen
        /// is not the whole answer.
        case truncatedStream
    }

    /// Feed the next chunk and take whatever completed.
    ///
    /// A block is complete only once its blank-line terminator has arrived, so
    /// a half-received frame stays in the buffer rather than being guessed at.
    mutating func consume(_ bytes: Data) throws -> [AskEvent] {
        buffer.append(bytes)
        var events: [AskEvent] = []

        // Both terminators, because a CRLF stream ends its blocks with
        // `\r\n\r\n` - which contains no `\n\n` at all (0D 0A 0D 0A).
        while let range = buffer.firstBlockTerminator() {
            let block = buffer.subdata(in: buffer.startIndex..<range.lowerBound)
            buffer.removeSubrange(buffer.startIndex..<range.upperBound)

            // A leading blank line, or a keep-alive, is not a block.
            guard !block.isEmpty else { continue }
            guard let text = String(data: block, encoding: .utf8) else {
                throw Failure.malformedBlock("not valid UTF-8")
            }
            // A keep-alive is comments only, and is not a frame.
            if let event = try parse(block: text) {
                events.append(event)
            }
        }
        return events
    }

    /// Called when the connection closes. Yields nothing; it exists to notice
    /// what is *missing*.
    ///
    /// `API.md` §4: every stream ends with **exactly one** terminal event,
    /// `done` or `error`. A connection that drops after a complete `token`
    /// frame is a truncated answer — the frames all parsed, so nothing else
    /// would notice, and the user would be shown half a reply as though it were
    /// the whole one. So EOF without a terminal is a failure, and so is a
    /// leftover half-frame.
    mutating func finish() throws -> [AskEvent] {
        if !buffer.isEmpty {
            let leftover = String(data: buffer, encoding: .utf8) ?? "<invalid utf-8>"
            buffer.removeAll()
            throw Failure.malformedBlock("the stream ended mid-frame: \(leftover)")
        }
        guard isFinished else {
            throw Failure.truncatedStream
        }
        return []
    }

    private mutating func parse(block: String) throws -> AskEvent? {
        var name: String?
        var payload: String?

        // **`\r\n` is one `Character` in Swift**, not two. Splitting on `"\n"`
        // therefore never splits a CRLF line, and the whole block arrives as a
        // single "line" whose `event:` prefix swallows the data line with it.
        // Normalising first is the fix; splitting on unicode scalars would be
        // the other one.
        let normalised = block.replacingOccurrences(of: "\r\n", with: "\n")

        for line in normalised.split(separator: "\n", omittingEmptySubsequences: false) {
            let line = line.hasSuffix("\r") ? String(line.dropLast()) : String(line)
            if let value = line.strippingPrefix("event:") {
                guard name == nil else { throw Failure.malformedBlock("two event lines") }
                name = value
            } else if let value = line.strippingPrefix("data:") {
                guard payload == nil else { throw Failure.malformedBlock("two data lines") }
                payload = value
            } else if line.hasPrefix(":") {
                continue // a comment / keep-alive
            } else if !line.isEmpty {
                throw Failure.malformedBlock(line)
            }
        }

        // Neither present can only mean comments and blank lines: any other
        // line already threw in the loop above. That is a keep-alive, not a
        // frame. Half a frame is a frame missing half of itself.
        if name == nil, payload == nil {
            return nil
        }
        guard let eventName = name, let eventData = payload else {
            throw Failure.malformedBlock("a block must be one event line and one data line")
        }
        if isFinished {
            throw Failure.eventAfterTerminal(eventName)
        }

        let data = Data(eventData.utf8)
        let decoder = WireTime.decoder()
        let event: AskEvent
        do {
            switch eventName {
            case "route":
                event = .route(try decoder.decode(AskRoutePayload.self, from: data).kind)
            case "token":
                event = .token(try decoder.decode(AskTokenPayload.self, from: data).text)
            case "results":
                event = .results(try decoder.decode(AskResultsPayload.self, from: data).threads)
            case "draft":
                event = .draft(try decoder.decode(AskDraftPayload.self, from: data))
            case "done":
                event = .done(sources: try decoder.decode(AskDonePayload.self, from: data).sources)
            case "error":
                let payload = try decoder.decode(AskErrorPayload.self, from: data)
                event = .error(code: payload.code, message: payload.message)
            default:
                throw Failure.unknownEvent(eventName)
            }
        } catch let failure as Failure {
            throw failure
        } catch {
            throw Failure.undecodable(event: eventName)
        }

        if event.isTerminal { isFinished = true }
        return event
    }
}

private extension Data {
    /// The first `\n\n` or `\r\n\r\n`, whichever comes first.
    func firstBlockTerminator() -> Range<Index>? {
        let lf = firstRange(of: Data("\n\n".utf8))
        let crlf = firstRange(of: Data("\r\n\r\n".utf8))
        switch (lf, crlf) {
        case (let lf?, let crlf?): return lf.lowerBound <= crlf.lowerBound ? lf : crlf
        case (let lf?, nil): return lf
        case (nil, let crlf?): return crlf
        case (nil, nil): return nil
        }
    }
}

private extension String {
    /// `event: x` and `event:x` are both legal; the single optional space after
    /// the colon is part of the SSE grammar, not part of the value.
    func strippingPrefix(_ prefix: String) -> String? {
        guard hasPrefix(prefix) else { return nil }
        var value = dropFirst(prefix.count)
        if value.hasPrefix(" ") { value = value.dropFirst() }
        return String(value)
    }
}

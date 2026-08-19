//
//  APIClientTests.swift
//  NADETests
//
//  P2's share of A16 and EDGE (P5, P7, P8). What an `APIClient` test *can*
//  prove is the transport: the header, the URL, the envelope, the retry hint.
//  What it cannot prove is what the app does about a failure — that is
//  `AppStateTests` and `OfflineUITests`, and the split is deliberate, because
//  a destructive error handler passes every test in this file.
//

import XCTest
@testable import NADE

final class APIClientTests: XCTestCase {

    private let origin = URL(string: "http://localhost:8080")!
    private var credentials: InMemoryCredentialStore!
    private var client: APIClient!

    override func setUp() {
        super.setUp()
        credentials = InMemoryCredentialStore(Credential(baseURL: origin, token: "nade_test"))
        client = StubURLProtocol.client(credentials: credentials)
    }

    override func tearDown() {
        StubURLProtocol.reset()
        super.tearDown()
    }

    // MARK: - The request

    func testEveryGuardedRequestCarriesTheBearer() async throws {
        StubURLProtocol.enqueue(try .fixture("mailboxes"))
        _ = try await client.mailboxes(origin: origin)

        let request = try XCTUnwrap(StubURLProtocol.requests.first)
        XCTAssertEqual(request.value(forHTTPHeaderField: "Authorization"), "Bearer nade_test")
        XCTAssertEqual(request.url?.path, "/v1/mailboxes")
    }

    /// `POST /auth/pair` is how a token is obtained, so it must not require one.
    func testPairingDoesNotRequireACredential() async throws {
        try credentials.clear()
        StubURLProtocol.enqueue(try .fixture("pair"))

        _ = try await client.pair(origin: origin, code: "203120", deviceName: "test")
        let request = try XCTUnwrap(StubURLProtocol.requests.first)
        XCTAssertNil(request.value(forHTTPHeaderField: "Authorization"))
        XCTAssertEqual(request.httpMethod, "POST")
    }

    /// EDGE (P5). API.md: a cursor is opaque, "clients must not parse it, and
    /// its contents differ per endpoint". It goes back exactly as it came.
    func testACursorIsEchoedVerbatimAndNeverParsed() async throws {
        let cursor = try XCTUnwrap(
            ContractFixture.decode(WireThreadPage.self, from: "threads").nextCursor
        )
        StubURLProtocol.enqueue(try .fixture("threads_last_page"))

        _ = try await client.threads(origin: origin, mailboxID: "INBOX", cursor: cursor)
        let sent = try XCTUnwrap(StubURLProtocol.requests.first?.url)
        let items = URLComponents(url: sent, resolvingAgainstBaseURL: false)?.queryItems ?? []
        XCTAssertEqual(items.first { $0.name == "cursor" }?.value, cursor)
    }

    /// `[Gmail]All Mail` is a real Gmail label id, and an unescaped `[` makes
    /// the URL nil — which would look like a server failure rather than a
    /// client bug.
    func testAMailboxIdWithAwkwardCharactersStillBuildsAURL() async throws {
        StubURLProtocol.enqueue(try .fixture("threads_empty"))
        _ = try await client.threads(origin: origin, mailboxID: "[Notion] label", cursor: nil)
        let path = try XCTUnwrap(StubURLProtocol.requests.first?.url?.absoluteString)
        XCTAssertTrue(path.contains("/v1/mailboxes/"))
        XCTAssertTrue(path.hasSuffix("/threads"))
    }

    // MARK: - The answers

    func testEveryErrorStatusMapsToItsCode() async throws {
        let cases: [(String, Int, ErrorCode)] = [
            ("error_unauthorized", 401, .unauthorized),
            ("error_not_found", 404, .notFound),
            ("error_needs_reauth", 409, .needsReauth),
            ("error_rate_limited", 429, .rateLimited),
            ("error_upstream_unavailable", 502, .upstreamUnavailable),
            ("error_bad_request", 400, .badRequest),
        ]

        for (fixture, status, expected) in cases {
            StubURLProtocol.reset()
            StubURLProtocol.enqueue(try .fixture(fixture, status: status))
            do {
                _ = try await client.mailboxes(origin: origin)
                XCTFail("\(fixture) decoded as success")
            } catch let failure as APIFailure {
                guard case .server(let code, let gotStatus, let message, _) = failure else {
                    return XCTFail("\(fixture) produced \(failure)")
                }
                XCTAssertEqual(code, expected)
                XCTAssertEqual(gotStatus, status)
                XCTAssertFalse(message.isEmpty, "the server's message is what the UI renders")
            }
        }
    }

    /// EDGE (P8). `Retry-After` is in seconds and the client has to keep it —
    /// a fixed backoff would either hammer a rate-limited server or wait far
    /// longer than it asked for.
    func testRetryAfterIsCarriedOffThe429() async throws {
        StubURLProtocol.enqueue(
            try .fixture("error_rate_limited", status: 429).with(headers: ["Retry-After": "12"])
        )
        do {
            _ = try await client.mailboxes(origin: origin)
            XCTFail("a 429 decoded as success")
        } catch let failure as APIFailure {
            guard case .server(_, _, _, let retryAfter) = failure else { return XCTFail("\(failure)") }
            XCTAssertEqual(retryAfter, 12)
        }
    }

    /// An error body that is not the contract's envelope is still an error.
    /// Turning a proxy's HTML 502 into "malformed response" would lose the one
    /// piece of information the caller needs — that the server is unreachable
    /// through something, not that the app is broken.
    func testANonEnvelopeErrorBodyIsStillAnError() async throws {
        StubURLProtocol.enqueue(.json("<html>502 Bad Gateway</html>", status: 502))
        do {
            _ = try await client.mailboxes(origin: origin)
            XCTFail("a 502 decoded as success")
        } catch let failure as APIFailure {
            guard case .server(let code, let status, _, _) = failure else { return XCTFail("\(failure)") }
            XCTAssertEqual(status, 502)
            XCTAssertEqual(code, .internalError)
        }
    }

    /// EDGE (P8). The transport case, kept distinct from an HTTP error because
    /// only this one means "your rows are still the best answer we have".
    func testAnUnreachableHostIsItsOwnFailure() async throws {
        StubURLProtocol.enqueue(.offline())
        do {
            _ = try await client.mailboxes(origin: origin)
            XCTFail("an unreachable host decoded as success")
        } catch let failure as APIFailure {
            XCTAssertTrue(failure.isUnreachable)
            XCTAssertEqual(failure.userFacingMessage, "Couldn't reach the server.")
        }
    }

    /// D58, at the transport layer. Foundation reports a cancelled request as
    /// `URLError.cancelled`, and mapping it to `.unreachable` put "Couldn't
    /// reach the server." on screens that had simply gone away.
    func testACancelledRequestIsNotAnUnreachableOne() async throws {
        StubURLProtocol.enqueue(StubURLProtocol.Reply(failure: .cancelled))
        do {
            _ = try await client.mailboxes(origin: origin)
            XCTFail("a cancelled request decoded as success")
        } catch let failure as APIFailure {
            XCTAssertTrue(failure.isCancellation)
            XCTAssertFalse(failure.isUnreachable, "cancellation was reported as a connection failure")
            XCTAssertEqual(failure.userFacingMessage, "", "a cancelled request has nothing to say")
        }
    }

    /// A 2xx whose body is not the contract's shape is a contract failure, not
    /// a network one, and the two must not be confused: one is worth a retry
    /// and the other never will be.
    func testAWellFormedSuccessWithTheWrongShapeIsMalformed() async throws {
        StubURLProtocol.enqueue(.json(#"{"mailboxes": "not an array"}"#))
        do {
            _ = try await client.mailboxes(origin: origin)
            XCTFail("nonsense decoded as mailboxes")
        } catch let failure as APIFailure {
            guard case .malformedResponse = failure else { return XCTFail("\(failure)") }
            XCTAssertFalse(failure.isUnreachable)
        }
    }

    /// Not paired *for this origin* answers in the server's own 401 shape, so
    /// the caller has one branch instead of two that mean the same thing.
    func testAMissingCredentialLooksLikeA401() async throws {
        try credentials.clear()
        do {
            _ = try await client.mailboxes(origin: origin)
            XCTFail("an unpaired client made a guarded request")
        } catch let failure as APIFailure {
            XCTAssertTrue(failure.isUnauthorized)
            XCTAssertTrue(StubURLProtocol.requests.isEmpty, "it went to the network without a token")
        }
    }

    func testTheHappyPathsDecodeTheirFixtures() async throws {
        StubURLProtocol.enqueue(try .fixture("me"))
        let me = try await client.me(origin: origin)
        XCTAssertEqual(me.status, .ok)

        StubURLProtocol.reset()
        StubURLProtocol.enqueue(try .fixture("thread"))
        let thread = try await client.thread(origin: origin, id: "18f2a1b3c4d5e6f7")
        XCTAssertEqual(thread.messages.count, 3)

    }
}

private extension StubURLProtocol.Reply {
    func with(headers: [String: String]) -> Self {
        var copy = self
        copy.headers = headers
        return copy
    }
}

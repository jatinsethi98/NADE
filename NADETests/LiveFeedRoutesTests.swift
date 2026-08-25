//
//  LiveFeedRoutesTests.swift
//  NADETests
//
//  P5's iOS half: the eleven routes that answered `notServedYet` until the
//  backend served them.
//
//  What a test here *can* prove without a server is the part that was actually
//  missing before: the URL, the method, the body, and that the answer decodes
//  into the app's own type. `APIClientTests` established the harness — a
//  stubbed `URLProtocol` under a real `URLSession` — and this is the same one
//  aimed at the routes P5 turned on.
//

import XCTest
@testable import NADE

final class LiveFeedRoutesTests: XCTestCase {

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

    private func sentJSON(_ index: Int = 0) throws -> [String: Any] {
        let body = try XCTUnwrap(StubURLProtocol.bodies.indices.contains(index)
            ? StubURLProtocol.bodies[index] : nil, "no body was sent")
        return try XCTUnwrap(JSONSerialization.jsonObject(with: body) as? [String: Any])
    }

    // MARK: - The feed

    func testTheFeedIsAGetAndItsCursorIsEchoedVerbatim() async throws {
        let cursor = "eyJ0cyI6IjIwMjYtMDgtMTZUMDk6MTI6MDRaIn0"
        StubURLProtocol.enqueue(try .fixture("feed"))
        let page = try await client.feed(origin: origin, cursor: cursor)

        let request = try XCTUnwrap(StubURLProtocol.requests.first)
        XCTAssertEqual(request.httpMethod, "GET")
        XCTAssertEqual(request.url?.path, "/v1/feed")
        let items = URLComponents(url: try XCTUnwrap(request.url), resolvingAgainstBaseURL: false)?
            .queryItems ?? []
        XCTAssertEqual(items.first { $0.name == "cursor" }?.value, cursor)
        XCTAssertEqual(page.items.count, 6)
        XCTAssertEqual(page.newCount, 3)
    }

    func testTheFeedWithoutACursorSendsNoQueryAtAll() async throws {
        StubURLProtocol.enqueue(try .fixture("feed"))
        _ = try await client.feed(origin: origin, cursor: nil)
        XCTAssertNil(try XCTUnwrap(StubURLProtocol.requests.first?.url).query)
    }

    func testTheDeepLinkIsTheItemsOwnPath() async throws {
        StubURLProtocol.enqueue(try .fixture("feed_item"))
        let item = try await client.feedItem(origin: origin,
                                             id: "c0000001-0000-4000-8000-000000000001")

        XCTAssertEqual(StubURLProtocol.requests.first?.url?.path,
                       "/v1/feed/c0000001-0000-4000-8000-000000000001")
        XCTAssertEqual(item.actions, [.approve, .skip])
        XCTAssertNotNil(item.approvalToken)
    }

    /// The token travels in the **body**, never in the path or a query. It is a
    /// capability, and a URL is logged by proxies, kept in caches and read over
    /// a shoulder.
    func testApproveIsAPostCarryingTheTokenInItsBody() async throws {
        StubURLProtocol.enqueue(try .fixture("approve"))
        let response = try await client.approve(
            origin: origin, feedItemID: "c0000001-0000-4000-8000-000000000001",
            approvalToken: "f0000001-0000-4000-8000-000000000001"
        )

        let request = try XCTUnwrap(StubURLProtocol.requests.first)
        XCTAssertEqual(request.httpMethod, "POST")
        XCTAssertEqual(request.url?.path,
                       "/v1/feed/c0000001-0000-4000-8000-000000000001/approve")
        XCTAssertNil(request.url?.query, "a capability must not reach a URL")
        XCTAssertEqual(try sentJSON()["approval_token"] as? String,
                       "f0000001-0000-4000-8000-000000000001")
        XCTAssertEqual(response.status, "queued")
    }

    func testSkipIsTheSameShapeAtItsOwnPath() async throws {
        StubURLProtocol.enqueue(try .fixture("skip"))
        let response = try await client.skip(origin: origin, feedItemID: "abc", approvalToken: "t")
        XCTAssertEqual(StubURLProtocol.requests.first?.url?.path, "/v1/feed/abc/skip")
        XCTAssertEqual(try sentJSON()["approval_token"] as? String, "t")
        XCTAssertEqual(response.status, "skipped")
    }

    /// `/v1/feed/seen` is a literal segment, not an item id. A client that
    /// built it through the item path would post a read receipt to a card
    /// called "seen".
    func testSeenPostsToTheCollectionAndNotToAnItem() async throws {
        StubURLProtocol.enqueue(try .fixture("seen"))
        let response = try await client.seen(origin: origin, ids: ["a", "b"])

        let request = try XCTUnwrap(StubURLProtocol.requests.first)
        XCTAssertEqual(request.httpMethod, "POST")
        XCTAssertEqual(request.url?.path, "/v1/feed/seen")
        XCTAssertEqual(try sentJSON()["ids"] as? [String], ["a", "b"])
        XCTAssertEqual(response.newCount, 2)
    }

    // MARK: - The agents

    func testTheAgentRoutesUseTheRightMethodsAndPaths() async throws {
        StubURLProtocol.enqueue(try .fixture("agents"))
        _ = try await client.agents(origin: origin)
        XCTAssertEqual(StubURLProtocol.requests.first?.httpMethod, "GET")
        XCTAssertEqual(StubURLProtocol.requests.first?.url?.path, "/v1/agents")

        StubURLProtocol.reset()
        StubURLProtocol.enqueue(try .fixture("agent"))
        _ = try await client.createAgent(origin: origin, nlDefinition: "when a recruiter emails…")
        XCTAssertEqual(StubURLProtocol.requests.first?.httpMethod, "POST")
        XCTAssertEqual(StubURLProtocol.requests.first?.url?.path, "/v1/agents")
        XCTAssertEqual(try sentJSON()["nl_definition"] as? String, "when a recruiter emails…")

        StubURLProtocol.reset()
        StubURLProtocol.enqueue(try .fixture("agent"))
        _ = try await client.updateAgent(origin: origin, id: "a1",
                                         patch: AgentPatch(status: .published))
        XCTAssertEqual(StubURLProtocol.requests.first?.httpMethod, "PATCH")
        XCTAssertEqual(StubURLProtocol.requests.first?.url?.path, "/v1/agents/a1")

        StubURLProtocol.reset()
        StubURLProtocol.enqueue(.json(#"{"run_id":"r1"}"#))
        let started = try await client.runAgent(origin: origin, id: "a1")
        XCTAssertEqual(StubURLProtocol.requests.first?.url?.path, "/v1/agents/a1/run")
        XCTAssertEqual(started.runID, "r1")
    }

    /// `DELETE /agents/{id}` answers `204`. A client that tried to decode a
    /// body would turn a success into `malformedResponse`.
    func testDeletingAnAgentToleratesAnEmptyBody() async throws {
        StubURLProtocol.enqueue(.init(status: 204, body: Data()))
        try await client.deleteAgent(origin: origin, id: "a1")
        let request = try XCTUnwrap(StubURLProtocol.requests.first)
        XCTAssertEqual(request.httpMethod, "DELETE")
        XCTAssertEqual(request.url?.path, "/v1/agents/a1")
    }

    /// …and still reports a failure, which is the half a "just ignore the body"
    /// shortcut loses.
    func testDeletingAnAgentStillSurfacesAServerError() async throws {
        StubURLProtocol.enqueue(try .fixture("error_not_found", status: 404))
        do {
            try await client.deleteAgent(origin: origin, id: "a1")
            XCTFail("a 404 must not read as success")
        } catch let failure as APIFailure {
            guard case .server(let code, _, _, _) = failure else {
                return XCTFail("\(failure)")
            }
            XCTAssertEqual(code, .notFound)
        }
    }

    /// Every guarded route carries the bearer. The eleven added at P5 are
    /// checked as a set rather than one at a time: the failure mode is a route
    /// that forgot, and a per-route test would have to be remembered per route.
    func testEveryNewRouteCarriesTheBearer() async throws {
        StubURLProtocol.enqueue(try .fixture("feed"))
        _ = try? await client.feed(origin: origin, cursor: nil)
        StubURLProtocol.enqueue(try .fixture("feed_item"))
        _ = try? await client.feedItem(origin: origin, id: "a")
        StubURLProtocol.enqueue(try .fixture("approve"))
        _ = try? await client.approve(origin: origin, feedItemID: "a", approvalToken: "t")
        StubURLProtocol.enqueue(try .fixture("skip"))
        _ = try? await client.skip(origin: origin, feedItemID: "a", approvalToken: "t")
        StubURLProtocol.enqueue(try .fixture("seen"))
        _ = try? await client.seen(origin: origin, ids: [])
        StubURLProtocol.enqueue(try .fixture("agents"))
        _ = try? await client.agents(origin: origin)
        StubURLProtocol.enqueue(try .fixture("agent"))
        _ = try? await client.agent(origin: origin, id: "a")
        StubURLProtocol.enqueue(try .fixture("agent"))
        _ = try? await client.createAgent(origin: origin, nlDefinition: "x")
        StubURLProtocol.enqueue(try .fixture("agent"))
        _ = try? await client.updateAgent(origin: origin, id: "a", patch: AgentPatch(status: .paused))
        StubURLProtocol.enqueue(.init(status: 204, body: Data()))
        try? await client.deleteAgent(origin: origin, id: "a")
        StubURLProtocol.enqueue(.json(#"{"run_id":"r"}"#))
        _ = try? await client.runAgent(origin: origin, id: "a")

        XCTAssertEqual(StubURLProtocol.requests.count, 11)
        for request in StubURLProtocol.requests {
            XCTAssertEqual(request.value(forHTTPHeaderField: "Authorization"), "Bearer nade_test",
                           "\(request.httpMethod ?? "?") \(request.url?.path ?? "?")")
        }
    }

    // MARK: - The four codes P5 answers with

    /// `API.md` §0's approval codes, end to end. The envelope has to decode
    /// into the enum the outbox switches on, or `token_consumed` — which the
    /// contract says a client treats as **success** — would fall through to
    /// `retry` and the app would ask again forever.
    func testTheApprovalErrorCodesReachTheOutcomeTheyMean() async throws {
        let cases: [(String, Int, ErrorCode, OutboxOutcome)] = [
            // `conflict` and `token_consumed` share a status and mean opposite
            // things: the run moved on, versus an earlier attempt won.
            ("error_conflict", 409, .conflict, .superseded),
            ("error_token_consumed", 409, .tokenConsumed, .alreadyRecorded),
            ("error_gone", 410, .gone, .expired),
            ("error_approval_expired", 410, .approvalExpired, .expired),
        ]
        for (name, status, expected, outcome) in cases {
            StubURLProtocol.reset()
            StubURLProtocol.enqueue(try .fixture(name, status: status))
            do {
                _ = try await client.approve(origin: origin, feedItemID: "a", approvalToken: "t")
                XCTFail("\(name) must not read as success")
            } catch let failure as APIFailure {
                guard case .server(let code, _, _, _) = failure else {
                    return XCTFail("\(name): \(failure)")
                }
                XCTAssertEqual(code, expected, name)
                XCTAssertEqual(OutboxDriver.outcome(for: failure), outcome, name)
            }
        }
    }
}

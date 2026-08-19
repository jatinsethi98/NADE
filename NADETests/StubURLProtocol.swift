//
//  StubURLProtocol.swift
//  NADETests
//
//  A `URLProtocol` that answers from a queue of canned responses, so the API
//  client can be exercised without a server — including the shapes a real one
//  is hard to provoke: a 429 with `Retry-After`, a 409 `needs_reauth`, and a
//  connection that never completes.
//

import Foundation
@testable import NADE

final class StubURLProtocol: URLProtocol, @unchecked Sendable {

    struct Reply {
        var status: Int = 200
        var body: Data = Data("{}".utf8)
        var headers: [String: String] = [:]
        /// When set, the request fails at the transport layer instead.
        var failure: URLError.Code?

        static func json(_ text: String, status: Int = 200, headers: [String: String] = [:]) -> Reply {
            Reply(status: status, body: Data(text.utf8), headers: headers)
        }

        static func fixture(_ name: String, status: Int = 200) throws -> Reply {
            Reply(status: status, body: try ContractFixture.data(name))
        }

        static func offline() -> Reply {
            Reply(failure: .notConnectedToInternet)
        }
    }

    private static let lock = NSLock()
    nonisolated(unsafe) private static var queue: [Reply] = []
    nonisolated(unsafe) private static var recorded: [URLRequest] = []

    static func enqueue(_ replies: Reply...) {
        lock.lock(); defer { lock.unlock() }
        queue.append(contentsOf: replies)
    }

    static func reset() {
        lock.lock(); defer { lock.unlock() }
        queue = []
        recorded = []
    }

    static var requests: [URLRequest] {
        lock.lock(); defer { lock.unlock() }
        return recorded
    }

    private static func next(for request: URLRequest) -> Reply {
        lock.lock(); defer { lock.unlock() }
        recorded.append(request)
        // The last reply repeats, so a poll loop does not need one entry per
        // attempt to describe "and it stayed empty".
        return queue.count > 1 ? queue.removeFirst() : (queue.first ?? Reply())
    }

    /// A client that answers from this protocol, with the queue reset.
    /// Assembled in three suites before it was here.
    static func client(credentials: any CredentialStore) -> APIClient {
        reset()
        return APIClient(session: APIClient.makeSession(protocolClasses: [StubURLProtocol.self]),
                         credentials: credentials)
    }

    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }

    override func startLoading() {
        let reply = Self.next(for: request)

        if let failure = reply.failure {
            client?.urlProtocol(self, didFailWithError: URLError(failure))
            return
        }

        let response = HTTPURLResponse(
            url: request.url!, statusCode: reply.status,
            httpVersion: "HTTP/1.1", headerFields: reply.headers
        )!
        client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
        client?.urlProtocol(self, didLoad: reply.body)
        client?.urlProtocolDidFinishLoading(self)
    }

    override func stopLoading() {}
}

//
//  APIClient.swift
//  NADE
//
//  The only file in the app that imports `URLSession` — `ModuleBoundaryTests`
//  keeps it that way, so no view model or store can grow a network dependency
//  by accident.
//
//  Everything it knows about the wire comes from `docs/API.md` §0: the bearer
//  header, the error envelope, `Retry-After` on 429, and the rule that a cursor
//  is **opaque** — echoed back exactly as received, never parsed, because "its
//  contents differ per endpoint".
//

import Foundation

// MARK: - Failures

nonisolated enum APIFailure: Error, Equatable {
    /// The server answered with the standard envelope.
    case server(code: ErrorCode, status: Int, message: String, retryAfter: TimeInterval?)
    /// The request never got an answer: offline, DNS, refused, timed out.
    /// EDGE (P8) — this is the one the UI renders *over* cached rows.
    case unreachable(URLError.Code)
    /// The caller went away. Foundation reports this as `URLError.cancelled`,
    /// which would otherwise arrive as `.unreachable` and put "Couldn't reach
    /// the server." on a screen that is simply no longer on screen — the same
    /// defect D58 fixed for `CancellationError`, one layer down.
    case cancelled
    /// A 2xx whose body was not the shape the contract promises.
    case malformedResponse(String)

    var isUnreachable: Bool {
        if case .unreachable = self { return true }
        return false
    }

    var isCancellation: Bool {
        if case .cancelled = self { return true }
        return false
    }

    /// Seconds the server asked us to wait, if it did.
    var retryAfter: TimeInterval? {
        if case .server(_, _, _, let retryAfter) = self { return retryAfter }
        return nil
    }

    var needsReauth: Bool {
        if case .server(let code, _, _, _) = self { return code == .needsReauth }
        return false
    }

    var isUnauthorized: Bool {
        if case .server(let code, _, _, _) = self { return code == .unauthorized }
        return false
    }

    /// What the user is shown. The server's `message` is written for them —
    /// "it says what happened and what to do, never a stack trace" — so it is
    /// rendered rather than replaced.
    var userFacingMessage: String {
        switch self {
        case .server(_, _, let message, _): message
        case .unreachable: String(localized: "Couldn't reach the server.",
                                  comment: "Shown over cached mail when the app cannot reach its NADE server")
        case .cancelled: ""
        case .malformedResponse: String(localized: "The server sent something unexpected.",
                                        comment: "Shown when a response does not match the API contract")
        }
    }
}

// MARK: - Endpoints

nonisolated enum Endpoint {
    case pair
    case me
    case gmailLink
    case mailboxes
    case threads(mailboxID: String, cursor: String?, limit: Int?)
    case thread(id: String)
    // No `search`: `GET /v1/search` is a real endpoint with no screen in v1
    // (DESIGN.md §1e draws no search field, and the mockup has none), and an
    // unreachable URL builder is not a head start — it is untested code that
    // reads as a shipped capability. `WireThreadPage` already decodes the
    // response, which is the part with a contract obligation today.

    var method: String {
        switch self {
        case .pair, .gmailLink: "POST"
        default: "GET"
        }
    }

    /// `API.md` §0's exception table, for the routes this client speaks:
    /// `POST /auth/pair` is how a token is obtained, so it cannot carry one.
    /// Everything else is bearer-guarded, and a request without one is a 401.
    var requiresBearer: Bool {
        switch self {
        case .pair: false
        default: true
        }
    }

    func url(base: URL) -> URL? {
        var components = URLComponents(url: base, resolvingAgainstBaseURL: false)
        var items: [URLQueryItem] = []

        switch self {
        case .pair:
            components?.path = "/v1/auth/pair"
        case .me:
            components?.path = "/v1/me"
        case .gmailLink:
            components?.path = "/v1/auth/gmail/link"
        case .mailboxes:
            components?.path = "/v1/mailboxes"
        case .threads(let mailboxID, let cursor, let limit):
            // A Gmail label id can contain characters that need escaping
            // (`[Gmail]All Mail` is a real one), so it goes through path
            // encoding rather than straight into the string.
            let escaped = mailboxID.addingPercentEncoding(withAllowedCharacters: .urlPathAllowed) ?? mailboxID
            components?.path = "/v1/mailboxes/\(escaped)/threads"
            // Opaque. Echoed exactly as received — API.md: "Clients must not
            // parse it, and its contents differ per endpoint."
            if let cursor { items.append(URLQueryItem(name: "cursor", value: cursor)) }
            if let limit { items.append(URLQueryItem(name: "limit", value: String(limit))) }
        case .thread(let id):
            components?.path = "/v1/threads/\(id)"
        }

        components?.queryItems = items.isEmpty ? nil : items
        return components?.url
    }
}

// MARK: - Client

nonisolated final class APIClient: Sendable {

    /// 30 s. Long enough for a cold `messages.list` behind the quota bucket,
    /// short enough that a dead LAN address does not look like a hang.
    static let timeout: TimeInterval = 30

    private let session: URLSession
    private let credentials: any CredentialStore

    init(session: URLSession = .shared, credentials: any CredentialStore) {
        self.session = session
        self.credentials = credentials
    }

    /// The shipping client. The session is built here so nothing outside this
    /// file has to name `URLSession` to get one.
    static func live(credentials: any CredentialStore) -> APIClient {
        APIClient(session: makeSession(), credentials: credentials)
    }

    static func makeSession(protocolClasses: [AnyClass]? = nil) -> URLSession {
        let configuration = URLSessionConfiguration.ephemeral
        configuration.timeoutIntervalForRequest = timeout
        configuration.waitsForConnectivity = false
        if let protocolClasses { configuration.protocolClasses = protocolClasses }
        return URLSession(configuration: configuration)
    }

    // MARK: Reads

    func me(origin: URL) async throws -> WireMe {
        try await send(.me, origin: origin, as: WireMe.self)
    }

    func mailboxes(origin: URL) async throws -> [WireMailbox] {
        try await send(.mailboxes, origin: origin, as: WireMailboxes.self).mailboxes
    }

    func threads(origin: URL, mailboxID: String, cursor: String?, limit: Int? = nil) async throws -> WireThreadPage {
        try await send(.threads(mailboxID: mailboxID, cursor: cursor, limit: limit),
                       origin: origin, as: WireThreadPage.self)
    }

    func thread(origin: URL, id: String) async throws -> WireThread {
        try await send(.thread(id: id), origin: origin, as: WireThread.self)
    }

    // MARK: Auth

    /// Exchanges the one-time code for a token and **stores it before
    /// returning**.
    ///
    /// The order matters and is EDGE (P17): the code is single-use, and the
    /// plaintext token exists exactly once, in this response. If the Keychain
    /// write fails after the server has already spent the code, the token is
    /// gone — so that is reported as its own failure rather than as a pairing
    /// error, because the user needs a *fresh code*, not another attempt with
    /// the same one.
    func pair(origin: URL, code: String, deviceName: String) async throws -> Credential {
        let body = try JSONSerialization.data(
            withJSONObject: ["code": code, "device_name": deviceName]
        )
        let response = try await send(.pair, origin: origin, body: body, as: WirePairResponse.self)
        let credential = Credential(baseURL: origin, token: response.token)
        do {
            try credentials.save(credential)
        } catch let error as CredentialError {
            if case .keychain(let status) = error {
                throw CredentialError.tokenLostAfterPairing(status)
            }
            throw error
        }
        return credential
    }

    func gmailLink(origin: URL) async throws -> WireGmailLink {
        try await send(.gmailLink, origin: origin, body: Data("{}".utf8), as: WireGmailLink.self)
    }

    // MARK: Transport

    private func send<T: Decodable>(
        _ endpoint: Endpoint, origin: URL, body: Data? = nil, as type: T.Type
    ) async throws -> T {
        guard let url = endpoint.url(base: origin) else {
            throw APIFailure.malformedResponse("could not build a URL for \(endpoint)")
        }

        var request = URLRequest(url: url, timeoutInterval: Self.timeout)
        request.httpMethod = endpoint.method
        if let body {
            request.httpBody = body
            request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        }
        if endpoint.requiresBearer {
            guard let credential = try credentials.credential(for: origin) else {
                // Not paired *for this origin*. Reported as the server's own
                // 401 shape so one code path handles both.
                throw APIFailure.server(code: .unauthorized, status: 401,
                                        message: ErrorCode.unauthorized.rawValue, retryAfter: nil)
            }
            request.setValue("Bearer \(credential.token)", forHTTPHeaderField: "Authorization")
        }

        let data: Data
        let response: URLResponse
        do {
            (data, response) = try await session.data(for: request)
        } catch let error as URLError {
            throw error.code == .cancelled ? APIFailure.cancelled : APIFailure.unreachable(error.code)
        }

        guard let http = response as? HTTPURLResponse else {
            throw APIFailure.malformedResponse("not an HTTP response")
        }

        guard (200..<300).contains(http.statusCode) else {
            throw Self.failure(status: http.statusCode, headers: http, data: data)
        }

        do {
            return try WireTime.decoder().decode(type, from: data)
        } catch {
            throw APIFailure.malformedResponse("\(type) did not decode: \(error)")
        }
    }

    private static func failure(status: Int, headers: HTTPURLResponse, data: Data) -> APIFailure {
        // `Retry-After` is in seconds, per API.md §0's rate-limit note.
        let retryAfter = (headers.value(forHTTPHeaderField: "Retry-After")).flatMap(TimeInterval.init)

        if let envelope = try? WireTime.decoder().decode(WireErrorEnvelope.self, from: data) {
            return .server(code: envelope.error.code, status: status,
                           message: envelope.error.message, retryAfter: retryAfter)
        }
        // A non-envelope error body is still an error. Falling back on the
        // status keeps the caller's branch simple rather than turning a 502
        // from a proxy into "malformed".
        let code: ErrorCode = switch status {
        case 401: .unauthorized
        case 403: .forbidden
        case 404: .notFound
        case 429: .rateLimited
        case 500...599: .internalError
        default: .badRequest
        }
        return .server(code: code, status: status,
                       message: String(localized: "The server could not answer that request.",
                                       comment: "Fallback when an error response is not the contract's envelope"),
                       retryAfter: retryAfter)
    }
}

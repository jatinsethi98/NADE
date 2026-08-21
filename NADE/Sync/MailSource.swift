//
//  MailSource.swift
//  NADE
//
//  Where mail comes from — the live server, or the frozen fixture world.
//
//  **The protocol covers every endpoint, including pairing and the Gmail
//  link.** If it covered only the read endpoints, a fixture-mode Settings
//  screen would still call the real network to pair, and the UI tests and
//  screenshots that depend on a hermetic world would quietly stop being
//  hermetic. The seam has to be complete to be a seam.
//

import Foundation

nonisolated protocol MailSource: Sendable {
    /// Which server this source speaks for. `StoreLocation` is chosen to match,
    /// so live rows and fixture rows never share a database file.
    var origin: URL { get }
    var storeLocation: StoreLocation { get }

    func isPaired() throws -> Bool
    func pair(code: String, deviceName: String) async throws -> Credential
    func unpair() throws

    func me() async throws -> WireMe
    func gmailLink() async throws -> WireGmailLink
    func mailboxes() async throws -> [WireMailbox]
    func threads(mailboxID: String, cursor: String?) async throws -> WireThreadPage
    func thread(id: String) async throws -> WireThread
    /// Downloads an attachment to a temporary file. P3: 1f's attachment tags
    /// become tappable, and the proxy that makes that possible shipped in P2.
    func downloadAttachment(gmailID: String, attachmentID: String, filename: String)
        async throws -> URL
    /// Raw bytes, for "View original"'s inline images.
    ///
    /// `MailSource` is already `Sendable`, which is exactly the property the
    /// caller needs: the inline-image handler runs on WebKit's own queue, not
    /// the main actor. That is why the web view holds a source rather than a
    /// closure threaded down through four layers.
    func attachmentData(gmailID: String, attachmentID: String) async throws -> Data

    // P3. The feed and the agents live behind endpoints that land at P5 and
    // P4; the seam exists now so the screens are written once. `FixtureMailSource`
    // implements them today, and `HTTPMailSource` gains its half when the routes
    // do — which is why these throw `notImplemented` rather than being absent:
    // a screen that calls a method the live source cannot answer must fail
    // loudly in DEBUG, not silently render an empty list in production.
    func feed(cursor: String?) async throws -> WireFeedPage
    func feedItem(id: String) async throws -> WireFeedItem
    func approve(feedItemID: String, approvalToken: String) async throws -> WireApproveResponse
    func skip(feedItemID: String, approvalToken: String) async throws -> WireSkipResponse
    func seen(ids: [String]) async throws -> WireSeenResponse
    func agents() async throws -> [WireAgentRow]
    func agent(id: String) async throws -> WireAgent
    func createAgent(nlDefinition: String) async throws -> WireAgent
    func updateAgent(id: String, patch: AgentPatch) async throws -> WireAgent
    func deleteAgent(id: String) async throws
    func runAgent(id: String) async throws -> WireRunStarted

    /// `POST /ask`, as the typed SSE session `API.md` §4 describes.
    ///
    /// A stream rather than a returned array, because the screen renders while
    /// the tokens arrive - and because the transport lands at P6 while the
    /// screen lands at P3, the seam has to be the streaming one now or the
    /// screen gets written twice. `route` is guaranteed first.
    func ask(query: String, threadID: String?, routeHint: AskRoute?)
        -> AsyncThrowingStream<AskEvent, any Error>
}

// MARK: - Live

nonisolated final class HTTPMailSource: MailSource {

    /// **Computed, not stored.** Settings can point the app at a different
    /// server, and a frozen origin makes that field a control with nothing
    /// behind it — which DESIGN.md §4 does not allow and which, on a real
    /// phone, leaves `localhost` pointing at the phone itself.
    var origin: URL { resolveOrigin() }
    let storeLocation: StoreLocation = .live

    private let resolveOrigin: @Sendable () -> URL
    private let client: APIClient
    private let credentials: any CredentialStore

    init(origin: @escaping @Sendable () -> URL, client: APIClient, credentials: any CredentialStore) {
        self.resolveOrigin = origin
        self.client = client
        self.credentials = credentials
    }

    /// The shipping composition. Deliberately does **not** take a session:
    /// naming `URLSession` here would put transport back outside `NADE/API/`,
    /// which `ModuleBoundaryTests` caught the first time this was written.
    /// Tests build an `APIClient` over a stubbed protocol and use the
    /// designated initialiser above.
    convenience init(origin: @escaping @Sendable () -> URL,
                     credentials: any CredentialStore = KeychainCredentialStore()) {
        self.init(origin: origin,
                  client: APIClient.live(credentials: credentials),
                  credentials: credentials)
    }

    /// Paired **for this origin**. A token minted by a different server is not
    /// a credential here, and asking clears it — see `Credential`.
    func isPaired() throws -> Bool {
        try credentials.credential(for: origin) != nil
    }

    func pair(code: String, deviceName: String) async throws -> Credential {
        try await client.pair(origin: origin, code: code, deviceName: deviceName)
    }

    func unpair() throws {
        try credentials.clear()
    }

    func me() async throws -> WireMe { try await client.me(origin: origin) }
    func gmailLink() async throws -> WireGmailLink { try await client.gmailLink(origin: origin) }
    func mailboxes() async throws -> [WireMailbox] { try await client.mailboxes(origin: origin) }

    func threads(mailboxID: String, cursor: String?) async throws -> WireThreadPage {
        try await client.threads(origin: origin, mailboxID: mailboxID, cursor: cursor)
    }

    func thread(id: String) async throws -> WireThread {
        try await client.thread(origin: origin, id: id)
    }

    func downloadAttachment(gmailID: String, attachmentID: String, filename: String)
        async throws -> URL {
        try await client.downloadAttachment(origin: origin, gmailID: gmailID,
                                            attachmentID: attachmentID, filename: filename)
    }

    func attachmentData(gmailID: String, attachmentID: String) async throws -> Data {
        try await client.attachmentData(origin: origin, gmailID: gmailID,
                                        attachmentID: attachmentID)
    }

    // MARK: - P3's endpoints, which the backend does not serve yet
    //
    // `APIClient` deliberately refuses to carry a URL builder no screen calls
    // (see its `Endpoint` comment: "an unreachable URL builder is not a head
    // start - it is untested code that reads as a shipped capability"). Here
    // the screens *do* call them, and the routes are what is missing — so the
    // seam is real and the live half is a loud, single-line failure until
    // `/feed` (P5) and `/agents` (P4) exist.

    func feed(cursor: String?) async throws -> WireFeedPage { throw APIFailure.notServedYet("GET /feed") }
    func feedItem(id: String) async throws -> WireFeedItem { throw APIFailure.notServedYet("GET /feed/{id}") }

    func approve(feedItemID: String, approvalToken: String) async throws -> WireApproveResponse {
        throw APIFailure.notServedYet("POST /feed/{id}/approve")
    }

    func skip(feedItemID: String, approvalToken: String) async throws -> WireSkipResponse {
        throw APIFailure.notServedYet("POST /feed/{id}/skip")
    }

    func seen(ids: [String]) async throws -> WireSeenResponse {
        throw APIFailure.notServedYet("POST /feed/seen")
    }

    func agents() async throws -> [WireAgentRow] { throw APIFailure.notServedYet("GET /agents") }
    func agent(id: String) async throws -> WireAgent { throw APIFailure.notServedYet("GET /agents/{id}") }

    func createAgent(nlDefinition: String) async throws -> WireAgent {
        throw APIFailure.notServedYet("POST /agents")
    }

    func updateAgent(id: String, patch: AgentPatch) async throws -> WireAgent {
        throw APIFailure.notServedYet("PATCH /agents/{id}")
    }

    func deleteAgent(id: String) async throws {
        throw APIFailure.notServedYet("DELETE /agents/{id}")
    }

    func runAgent(id: String) async throws -> WireRunStarted {
        throw APIFailure.notServedYet("POST /agents/{id}/run")
    }

    /// P6 gives this a `URLSession` byte stream. Until then it fails the same
    /// loud, nameable way as every other route this phase outruns - and it
    /// fails *inside* the stream rather than at the call site, so the screen's
    /// error state is the one path both halves take.
    func ask(query: String, threadID: String?, routeHint: AskRoute?)
        -> AsyncThrowingStream<AskEvent, any Error> {
        AsyncThrowingStream { $0.finish(throwing: APIFailure.notServedYet("POST /ask")) }
    }
}

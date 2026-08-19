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
}

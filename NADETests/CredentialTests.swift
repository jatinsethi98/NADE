//
//  CredentialTests.swift
//  NADETests
//
//  P2 A18 and EDGE (P17).
//
//  The defect these exist for is not exotic: a token and a base URL kept as two
//  independent settings can be recombined, and then a bearer minted by one
//  server is sent to another. Binding them into one item makes the mismatch
//  unrepresentable — you ask for a credential *for an origin*, and you either
//  get one that belongs to it or you get nothing.
//

import XCTest
@testable import NADE

final class CredentialTests: XCTestCase {

    private let a = URL(string: "http://localhost:8080")!
    private let b = URL(string: "http://192.168.1.40:8080")!

    private func freshKeychain() -> KeychainCredentialStore {
        let store = KeychainCredentialStore(service: "fsaas.NADE.tests.\(UUID().uuidString)",
                                            account: "device-token")
        try? store.clear()
        return store
    }

    // MARK: - A18

    func testATokenIsOnlyOfferedToTheServerThatMintedIt() throws {
        let store = freshKeychain()
        defer { try? store.clear() }

        try store.save(Credential(baseURL: a, token: "nade_aaa"))
        XCTAssertEqual(try store.credential(for: a)?.token, "nade_aaa")

        // The user edits the server URL. The old token is not valid there.
        XCTAssertNil(try store.credential(for: b),
                     "a bearer minted by \(a) was offered to \(b)")
    }

    /// …and the mismatch **clears** it, so a later edit back to the original
    /// URL cannot resurrect a credential the user has already moved away from.
    func testAMismatchedOriginClearsTheStoredToken() throws {
        let store = freshKeychain()
        defer { try? store.clear() }

        try store.save(Credential(baseURL: a, token: "nade_aaa"))
        _ = try store.credential(for: b)
        XCTAssertNil(try store.credential(for: a), "the superseded token was still reachable")
    }

    /// Re-pairing replaces the item rather than adding a second one. Without
    /// the delete-then-add, `SecItemAdd` returns `errSecDuplicateItem` and the
    /// *old* token keeps being served.
    func testRePairingReplacesRatherThanDuplicating() throws {
        let store = freshKeychain()
        defer { try? store.clear() }

        try store.save(Credential(baseURL: a, token: "nade_first"))
        try store.save(Credential(baseURL: a, token: "nade_second"))
        XCTAssertEqual(try store.credential(for: a)?.token, "nade_second")
    }

    /// Origins compare by scheme, host and port. A trailing slash is not a
    /// different server, and treating it as one would orphan a working token
    /// the first time somebody typed the URL by hand.
    func testATrailingSlashIsTheSameOrigin() throws {
        let store = freshKeychain()
        defer { try? store.clear() }

        try store.save(Credential(baseURL: a, token: "nade_aaa"))
        XCTAssertNotNil(try store.credential(for: URL(string: "http://localhost:8080/")!))
    }

    func testClearingLeavesNothingBehind() throws {
        let store = freshKeychain()
        try store.save(Credential(baseURL: a, token: "nade_aaa"))
        try store.clear()
        XCTAssertNil(try store.credential(for: a))
        XCTAssertNoThrow(try store.clear(), "clearing an empty store is not an error")
    }

    // MARK: - EDGE (P17)

    /// The pairing code is single-use and the token exists exactly once, in the
    /// response. If the Keychain write fails after the server has spent the
    /// code, the token is gone — and the user needs a *fresh code*, not another
    /// attempt with the same one. Reporting that as an ordinary pairing failure
    /// would send them round a loop that cannot succeed.
    func testAKeychainFailureAfterPairingIsReportedAsTokenLoss() async throws {
        defer { StubURLProtocol.reset() }
        let store = InMemoryCredentialStore()
        store.saveFailure = errSecIO
        let client = StubURLProtocol.client(credentials: store)
        StubURLProtocol.enqueue(try .fixture("pair"))

        do {
            _ = try await client.pair(origin: a, code: "203120", deviceName: "test")
            XCTFail("pairing reported success while the token was being dropped")
        } catch let error as CredentialError {
            guard case .tokenLostAfterPairing = error else {
                return XCTFail("reported \(error), which does not tell the user to get a new code")
            }
        }
    }

    func testPairingStoresTheTokenBeforeReportingSuccess() async throws {
        defer { StubURLProtocol.reset() }
        let store = InMemoryCredentialStore()
        let client = StubURLProtocol.client(credentials: store)
        StubURLProtocol.enqueue(try .fixture("pair"))

        let credential = try await client.pair(origin: a, code: "203120", deviceName: "test")
        XCTAssertEqual(credential.token.count, 69)
        XCTAssertEqual(try store.credential(for: a)?.token, credential.token)
    }
}

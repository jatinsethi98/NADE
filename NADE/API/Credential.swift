//
//  Credential.swift
//  NADE
//
//  The device token, bound to the server that minted it.
//
//  **One item, two fields, on purpose.** A token and a base URL stored
//  independently can be recombined: set only `NADE_BASE_URL`, or edit the URL
//  in Settings, and the next request sends a bearer minted by server A to
//  server B. `docs/API.md` §1 says the plaintext token "exists exactly once, in
//  this response" — it is not a value to hand out speculatively. Keeping the
//  origin *inside* the credential makes the mismatch unrepresentable rather
//  than merely unlikely: ask for a credential for an origin and you either get
//  one that belongs to it or you get nothing.
//

import Foundation
import Security

nonisolated struct Credential: Equatable, Sendable {
    let baseURL: URL
    let token: String
}

nonisolated protocol CredentialStore: Sendable {
    /// The stored credential, **only** if it was minted by `origin`.
    /// A mismatch is not an error — it is an unpaired device, and the stored
    /// item is cleared so it cannot be reached again (P2 A18).
    func credential(for origin: URL) throws -> Credential?
    func save(_ credential: Credential) throws
    func clear() throws
}

nonisolated enum CredentialError: Error, Equatable {
    /// The pairing code has already been spent by the time the token could not
    /// be stored. EDGE (P17): the code is single-use and the token exists
    /// exactly once, so this is unrecoverable *for this code* and the user
    /// needs a fresh one. Saying so beats a silent failure that looks like a
    /// wrong code.
    case tokenLostAfterPairing(OSStatus)
    case keychain(OSStatus)
}

nonisolated final class KeychainCredentialStore: CredentialStore {

    private let service: String
    private let account: String

    init(service: String = "fsaas.NADE.server", account: String = "device-token") {
        self.service = service
        self.account = account
    }

    private var query: [String: Any] {
        [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account,
        ]
    }

    func credential(for origin: URL) throws -> Credential? {
        var query = self.query
        query[kSecReturnData as String] = true
        query[kSecReturnAttributes as String] = true
        query[kSecMatchLimit as String] = kSecMatchLimitOne

        var item: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &item)
        if status == errSecItemNotFound { return nil }
        guard status == errSecSuccess else { throw CredentialError.keychain(status) }

        guard let attributes = item as? [String: Any],
              let data = attributes[kSecValueData as String] as? Data,
              let token = String(data: data, encoding: .utf8),
              // `kSecAttrGeneric` carries the origin the token was minted for.
              let originData = attributes[kSecAttrGeneric as String] as? Data,
              let storedOrigin = String(data: originData, encoding: .utf8)
        else {
            try clear()
            return nil
        }

        guard storedOrigin == Self.canonical(origin) else {
            // The server moved. The old token is not valid here and must not be
            // offered to the new one, so it goes rather than lingering for a
            // later edit to point back at it.
            try clear()
            return nil
        }
        return Credential(baseURL: origin, token: token)
    }

    func save(_ credential: Credential) throws {
        try clear()
        var attributes = query
        attributes[kSecValueData as String] = Data(credential.token.utf8)
        attributes[kSecAttrGeneric as String] = Data(Self.canonical(credential.baseURL).utf8)
        // P3's push-driven sync runs while the device is locked, so `.complete`
        // would make the token unreadable exactly when it is needed. This still
        // keeps it encrypted until the first unlock after a boot.
        attributes[kSecAttrAccessible as String] = kSecAttrAccessibleAfterFirstUnlock

        let status = SecItemAdd(attributes as CFDictionary, nil)
        guard status == errSecSuccess else { throw CredentialError.keychain(status) }
    }

    func clear() throws {
        let status = SecItemDelete(query as CFDictionary)
        guard status == errSecSuccess || status == errSecItemNotFound else {
            throw CredentialError.keychain(status)
        }
    }

    /// Origins compare by scheme, host and port — not by the exact string, so a
    /// trailing slash does not orphan a working credential.
    static func canonical(_ url: URL) -> String {
        var components = URLComponents(url: url, resolvingAgainstBaseURL: false)
        components?.path = ""
        components?.query = nil
        components?.fragment = nil
        return components?.string ?? url.absoluteString
    }
}

/// For tests and previews. Same contract, no Keychain.
nonisolated final class InMemoryCredentialStore: CredentialStore, @unchecked Sendable {
    private let lock = NSLock()
    private var stored: Credential?
    /// Set to a failure status to exercise EDGE (P17).
    var saveFailure: OSStatus?

    init(_ credential: Credential? = nil) { stored = credential }

    func credential(for origin: URL) throws -> Credential? {
        lock.lock(); defer { lock.unlock() }
        guard let stored else { return nil }
        guard KeychainCredentialStore.canonical(stored.baseURL) == KeychainCredentialStore.canonical(origin) else {
            self.stored = nil
            return nil
        }
        return stored
    }

    func save(_ credential: Credential) throws {
        lock.lock(); defer { lock.unlock() }
        if let saveFailure { throw CredentialError.keychain(saveFailure) }
        stored = credential
    }

    func clear() throws {
        lock.lock(); defer { lock.unlock() }
        stored = nil
    }
}

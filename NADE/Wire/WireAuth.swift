//
//  WireAuth.swift
//  NADE
//
//  `docs/API.md` §1 and §10. These are wire shapes like any other and belong in
//  the decode suite — the P2 lane went live, so pairing and the Gmail link are
//  real responses this app parses, not prose.
//

import Foundation

/// `GET /me`.
///
/// Before any Gmail account is connected this is `{"email": "", "status":
/// "needs_reauth"}` — a 200, not a 404, because "a freshly paired device asking
/// who it is has not made an error and needs exactly the state it renders after
/// a token dies". So `email.isEmpty` is a real, expected state.
nonisolated struct WireMe: Codable, Hashable, Sendable {
    let email: String
    let status: AccountStatus
}

/// `POST /auth/pair` → the device token. 69 characters: `nade_` + 64 hex.
///
/// API.md: "The plaintext token exists exactly once, in this response." That
/// sentence is why `Credential` writes the Keychain **before** the pairing call
/// reports success — see P17.
nonisolated struct WirePairResponse: Codable, Hashable, Sendable {
    let token: String
}

/// `POST /auth/gmail/link` → the single-use URL to open in a browser.
nonisolated struct WireGmailLink: Codable, Hashable, Sendable {
    let url: String
    let expiresAt: Date

    enum CodingKeys: String, CodingKey {
        case url
        case expiresAt = "expires_at"
    }
}

/// `GET /healthz`.
nonisolated struct WireHealth: Codable, Hashable, Sendable {
    let status: String
    let db: String
    let version: String
}

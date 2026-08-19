//
//  WireError.swift
//  NADE
//
//  `docs/API.md` §0's error envelope, and the code table as a Swift enum.
//
//  `message` is end-user-facing English from the server — "it says what
//  happened and what to do, never a stack trace, never an internal identifier"
//  — so the UI renders it rather than inventing its own copy. `code` is the
//  stable machine value the app branches on.
//

import Foundation

nonisolated struct WireErrorEnvelope: Codable, Hashable, Sendable {
    let error: WireError
}

nonisolated struct WireError: Codable, Hashable, Sendable {
    let code: ErrorCode
    let message: String
}

/// The full table from `API.md` §0, including the codes P2 cannot yet provoke.
/// Decoding them now costs nothing and means a later phase does not discover
/// that its error path was never modelled.
nonisolated enum ErrorCode: RawRepresentable, WireEnum {
    case badRequest
    case unauthorized
    case forbidden
    case notFound
    case conflict
    /// 409, and **clients treat it as success** — the approval was already
    /// recorded. P5 relies on this; P2 only decodes it.
    case tokenConsumed
    case gone
    case approvalExpired
    case payloadTooLarge
    case rateLimited
    /// 409. Gmail credentials are dead and the user must re-consent.
    case needsReauth
    case upstreamUnavailable
    case internalError
    case unknown(String)

    var rawValue: String {
        switch self {
        case .badRequest: "bad_request"
        case .unauthorized: "unauthorized"
        case .forbidden: "forbidden"
        case .notFound: "not_found"
        case .conflict: "conflict"
        case .tokenConsumed: "token_consumed"
        case .gone: "gone"
        case .approvalExpired: "approval_expired"
        case .payloadTooLarge: "payload_too_large"
        case .rateLimited: "rate_limited"
        case .needsReauth: "needs_reauth"
        case .upstreamUnavailable: "upstream_unavailable"
        case .internalError: "internal"
        case .unknown(let raw): raw
        }
    }

    static let allKnown: [Self] = [
        .badRequest, .unauthorized, .forbidden, .notFound, .conflict,
        .tokenConsumed, .gone, .approvalExpired, .payloadTooLarge,
        .rateLimited, .needsReauth, .upstreamUnavailable, .internalError,
    ]

    init(unknown raw: String) { self = .unknown(raw) }
}

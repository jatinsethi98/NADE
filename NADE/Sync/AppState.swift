//
//  AppState.swift
//  NADE
//
//  The first-run state machine, and why P2 needs one at all.
//
//  The naive shape — fetch once at launch — is broken against this backend, and
//  not subtly. `docs/PLAN.md` §Gmail sync puts the initial sync at "~1–2 min",
//  and `api/mail.rs` deliberately answers `mailboxes: []` until it finishes. So
//  pair → link Gmail → fetch lands in an empty app, and with no poll, no
//  refresh control and no completion event, the finished sync never reaches the
//  screen until the next launch.
//
//  Hence an explicit enum rather than a handful of booleans: every state below
//  has exactly one rendering, and "empty because we have not asked" is a
//  different sentence from "empty because there is nothing".
//

import Foundation

nonisolated enum AppState: Equatable, Sendable {
    /// No credential for this origin. 1g shows its caption; Settings is the way out.
    case unpaired
    /// Paired, but `GET /me` says the Gmail credential is dead or absent.
    /// `API.md` §1: a fresh device and an expired token render the same row.
    case needsGmail
    /// Linked, and the server's first sync has not produced a mailbox yet.
    case syncPending
    /// Mail is here.
    case ready
}

/// The connection banner, kept **separate** from `AppState` on purpose.
///
/// `API.md` §2 says a thread's `partial` flag is *produced by* an upstream
/// failure — Gmail unreachable, throttled, or needing re-auth — so a network
/// error and a partial thread are routinely true at the same moment. One
/// string would have to overwrite one of those truths; two slots do not.
nonisolated struct ConnectionProblem: Equatable, Sendable {
    /// The server's own words where there are any — `API.md` §0 says `message`
    /// "is end-user-facing English: it says what happened and what to do".
    ///
    /// There is deliberately no `isUnreachable` discriminator beside it. There
    /// was one, and nothing on any screen read it: D74 widened the
    /// cache-over-failure rule to forgive *any* failure, which left the flag
    /// with no consumer. `APIFailure.isUnreachable` still exists where the
    /// distinction is actually made.
    let message: String

    init(_ failure: APIFailure) {
        self.message = failure.userFacingMessage
    }

    init(message: String) {
        self.message = message
    }
}

//
//  Outbox.swift
//  NADE
//
//  Approve and skip, made durable.
//
//  A tap on a feed card is a **state change on the server**, and the only one
//  v1 has. It has to survive a dead network, the app being killed on the way to
//  the request, and the same tap arriving twice — so it is written to
//  `pending_action` first and sent afterwards.
//
//  The rule the contract states twice, and the reason this file exists:
//  **`409 token_consumed` is success.** It means an earlier attempt already
//  won. Treating it as a failure would leave a row queued for ever against an
//  approval that is already recorded.
//
//  Nothing here schedules itself. `NADE/CRITERIA.md` is explicit that
//  `Retry-After` is honoured by *refusing to ask early*, not by scheduling a
//  retry — so the drain has named triggers (`OutboxDriver.drain` callers) and a
//  refusal simply leaves the row for the next one.
//

import Foundation

/// What the server said, reduced to what the client should do about it.
nonisolated enum OutboxOutcome: Equatable, Sendable {
    /// The action is recorded. Drop the row.
    case done
    /// Somebody won, but not necessarily us: approve and skip consume the same
    /// token, and the schema allows several paired devices. Drop the row and
    /// **refetch** rather than assuming our own outcome.
    case alreadyRecorded
    /// `409 conflict` — the state moved on and the decision was **not**
    /// recorded. Same handling as [`alreadyRecorded`] (drop the row, re-read
    /// the card), and a distinct case because it means the opposite thing.
    case superseded
    /// Past its deadline. Terminal, and the item is expired.
    case expired
    /// Ambiguous: `401` is the same code for a wrong approval token **and** a
    /// revoked or expired device credential (`API.md` §0 vs §7). Refetch to
    /// find out which; do not discard the intention on a guess.
    case unauthorized
    /// Gmail needs re-consent. Leave it queued.
    case needsReauth
    /// Try again on the next trigger, no earlier than `retryAfter` if the
    /// server named one.
    case retry(String, retryAfter: TimeInterval? = nil)
    /// The server will never accept this request: a malformed body, or an item
    /// it does not have. Retrying cannot help, and a row that retried for ever
    /// would pin every action queued behind it — the same failure the backend's
    /// history walk has for a permanently unfetchable message.
    case rejected(String)
}

/// Sends what the outbox holds.
@MainActor
final class OutboxDriver {
    private let store: MailStore
    private let source: any MailSource
    private let origin: () -> URL
    /// Re-entrancy guard: the drain runs on foreground, on appearance and after
    /// pairing, and two of those can land together.
    private var draining = false
    /// Honours `Retry-After`. Nothing schedules a wake-up for it; the next
    /// trigger simply finds the window has passed.
    ///
    /// A **monotonic** instant, not a `Date`. `Retry-After` is a duration, and
    /// measuring it against the wall clock meant an NTP correction or a manual
    /// clock change moved the window: forward, and the queue retried early into
    /// a server that had asked it not to; backward, and a 30-second backoff
    /// could park the queue for hours. `ContinuousClock` keeps counting across
    /// both, and across suspension.
    private var notBefore: ContinuousClock.Instant?

    /// Reports a problem worth showing on the feed, or `nil` to clear one.
    ///
    /// Injected for the same reason `onNeedsReauth` is: the outbox does not
    /// need to know what a `MailSync` is.
    var onProblem: ((String?) -> Void)?

    /// `onNeedsReauth` is how a dead Gmail credential reaches `MailSync`'s
    /// state machine. Injected rather than referenced, so the outbox does not
    /// need to know what a `MailSync` is.
    init(
        store: MailStore,
        source: any MailSource,
        origin: @escaping () -> URL,
        onNeedsReauth: (() -> Void)? = nil
    ) {
        self.store = store
        self.source = source
        self.origin = origin
        self.onNeedsReauth = onNeedsReauth
    }

    /// Settable after construction, because `MailSync` owns the driver and
    /// cannot hand it a closure capturing `self` before its own stored
    /// properties exist. Still injected rather than referenced: the outbox does
    /// not know what a `MailSync` is.
    var onNeedsReauth: (() -> Void)?

    /// Queue an approve, and try to send it now.
    func approve(item: WireFeedItem) async {
        await enqueue(kind: "approve", item: item)
    }

    /// Queue a skip, and try to send it now.
    func skip(item: WireFeedItem) async {
        await enqueue(kind: "skip", item: item)
    }

    private func enqueue(kind: String, item: WireFeedItem) async {
        guard let token = item.approvalToken else {
            // No token means the card is not actionable any more. The buttons
            // are rendered from `actions`, which is `[]` in that state, so this
            // is a race with a refresh rather than a bug.
            return
        }
        let action = PendingActionRecord(
            id: UUID().uuidString,
            origin: origin().absoluteString,
            kind: kind,
            feedItemId: item.id,
            approvalToken: token,
            createdAt: Date(),
            attempts: 0,
            lastError: nil
        )

        // **Durability first, and the order is the whole point.** With a `try?`
        // here, a disk or migration failure would be swallowed, the card would
        // still be moved to resolved, and its token cleared — so the UI would
        // say the action succeeded, nothing would ever be sent, and the
        // capability needed to retry would be gone from the only copy the
        // client had. The optimistic move is only honest once the intention is
        // on disk.
        do {
            // A second tap on a card that already has a pending action loses.
            // Moving the card anyway would let Skip overwrite a queued Save.
            guard try await store.enqueue(action) else { return }
        } catch {
            // **Surfaced, not just recorded.** `lastEnqueueError` had no
            // reader: a failed durable write returned before the optimistic
            // move, so the button stayed, nothing was queued, and nothing said
            // why — and every further tap did the same. The connection banner
            // is where the rest of this screen's problems already appear.
            lastEnqueueError = error
            onProblem?(StateCopy.approvalNotQueued)
            return
        }
        lastEnqueueError = nil
        onProblem?(nil)

        // Optimistic local move, so the card settles under the finger. The
        // drain corrects it if the server disagrees.
        try? await store.resolveFeedItem(
            id: item.id,
            status: kind == "approve" ? .resolved : .skipped,
            note: nil
        )
        await drain()
    }

    /// EDGE: a hostile or broken `Retry-After`.
    ///
    /// A negative value would set a window already in the past (harmless) and a
    /// huge one would strand the queue for the life of the process, so it is
    /// clamped to an hour — long enough to respect any real rate limit, short
    /// enough that a bad header cannot become a hang. `NaN`/infinity fall out of
    /// the clamp too, which `min`/`max` alone would not guarantee.
    static func clampRetryAfter(_ seconds: TimeInterval) -> TimeInterval {
        // NaN and anything at or below zero carry no instruction, so the window
        // is simply open. (`NaN > 0` is false, which is what catches it.)
        guard seconds > 0 else { return 0 }
        // An infinite value **is** an instruction — "wait" — just an unusable
        // one, so it takes the maximum rather than the minimum. Returning 0 here
        // would answer a server asking us to back off by retrying at once.
        guard seconds.isFinite else { return maxRetryAfter }
        return min(seconds, maxRetryAfter)
    }

    /// Long enough to respect any real rate limit, short enough that a bad
    /// header cannot become a hang.
    static let maxRetryAfter: TimeInterval = 3600

    /// Attempts before an action is given up on, matching the server queue's
    /// own `max_attempts` (`jobs.rs`). Without a ceiling one permanently
    /// failing card holds up every later one.
    static let maxAttempts = 5

    /// The fallback explanation for a card that expired while queued. Replaced
    /// by the server's own `resolved_note` as soon as the refetch lands.
    ///
    /// **"saved", not "sent".** The original read "before it could be sent",
    /// under a card whose own button says "Save draft" — a sentence asserting
    /// the mail would otherwise have gone out, which is what DESIGN §4 and
    /// PLAN C1/C2 forbid outright. Four guards were pointed at it and all four
    /// matched `\bsend(s|ing)?\b`, which does not match **`sent`**.
    static let expiredNote = String(
        localized: "This expired before it was saved.",
        comment: "Shown on a feed card whose approval window closed while it was queued"
    )

    /// Set when the queue itself could not be written. The feed screen shows it
    /// rather than pretending the tap landed.
    private(set) var lastEnqueueError: Error?

    /// Send everything queued for the current origin.
    ///
    /// Called on app foreground, when the feed appears, and after a successful
    /// pair. There is no timer: nothing in this app schedules its own retry,
    /// and inventing one here would be the only such thing.
    func drain() async {
        guard !draining else { return }
        draining = true
        defer { draining = false }

        if let notBefore, ContinuousClock.now < notBefore {
            // `Retry-After` is honoured by refusing to ask early, which is the
            // rule `NADE/CRITERIA.md` already states for the read paths. The
            // rows stay queued for the next trigger.
            return
        }

        let currentOrigin = origin().absoluteString
        guard let queued = try? await store.pendingActions(origin: currentOrigin) else { return }

        for action in queued {
            let outcome = await send(action)
            switch outcome {
            case .done, .alreadyRecorded, .superseded:
                try? await store.removePendingAction(id: action.id)
                // `alreadyRecorded` cannot tell us *which* action won, so the
                // item is refetched rather than assumed.
                await refresh(feedItemID: action.feedItemId)
            case .expired:
                try? await store.removePendingAction(id: action.id)
                // `API.md` §7 sets `resolved_note` on expired cards too - "the
                // last two need it most, or they render an outcome with no
                // explanation". Refetch so the server's own sentence is what is
                // shown; the local write below is the fallback for when that
                // refetch cannot happen either.
                try? await store.resolveFeedItem(
                    id: action.feedItemId,
                    status: .expired,
                    note: Self.expiredNote
                )
                await refresh(feedItemID: action.feedItemId)
            case .unauthorized:
                // Do not drop the row: it may be the device credential rather
                // than the token. A refetch that also fails tells us which.
                try? await store.recordPendingFailure(id: action.id, error: "unauthorized")
                await refresh(feedItemID: action.feedItemId)
            case .needsReauth:
                try? await store.recordPendingFailure(id: action.id, error: "needs_reauth")
                // The Gmail credential is dead. Recording it and carrying on
                // would leave the app in `.ready` with a queue that can never
                // drain, so the state change is part of the outcome.
                onNeedsReauth?()
                return
            case .rejected(let message):
                // Drop it, and refetch so the card shows what the server
                // actually thinks rather than the optimistic local move.
                try? await store.removePendingAction(id: action.id)
                tracingRejected(action, message)
                await refresh(feedItemID: action.feedItemId)
            case .retry(let message, let retryAfter):
                try? await store.recordPendingFailure(id: action.id, error: message)
                // **A queue with no dead letter is a queue that stops.**
                // `attempts` was written by `recordPendingFailure` and read by
                // nothing, so one card whose 500 mapped to `.retry` blocked
                // every action behind it on every foreground, for ever. The
                // server's own queue dead-letters at five (`jobs.rs`); this
                // matches it, and refetches so the card stops claiming the
                // action was taken.
                if action.attempts + 1 >= Self.maxAttempts {
                    try? await store.removePendingAction(id: action.id)
                    tracingRejected(action, "gave up after \(Self.maxAttempts) attempts")
                    await refresh(feedItemID: action.feedItemId)
                    continue
                }
                if let retryAfter {
                    notBefore = ContinuousClock.now
                        .advanced(by: .seconds(Self.clampRetryAfter(retryAfter)))
                }
                // Whatever stopped this one will stop the next; stop walking.
                return
            }
        }
    }

    private func send(_ action: PendingActionRecord) async -> OutboxOutcome {
        do {
            if action.kind == "approve" {
                _ = try await source.approve(
                    feedItemID: action.feedItemId,
                    approvalToken: action.approvalToken
                )
            } else {
                _ = try await source.skip(
                    feedItemID: action.feedItemId,
                    approvalToken: action.approvalToken
                )
            }
            return .done
        } catch let failure as APIFailure {
            return Self.outcome(for: failure)
        } catch {
            return .retry("\(error)")
        }
    }

    /// The whole 409/410/401 table, in one place and one direction.
    static func outcome(for failure: APIFailure) -> OutboxOutcome {
        guard case .server(let code, _, let message, let retryAfter) = failure else {
            if failure.isCancellation { return .retry("cancelled") }
            return .retry(failure.userFacingMessage)
        }
        switch code {
        case .tokenConsumed:
            return .alreadyRecorded
        case .conflict:
            // **Not the same answer**, and `error.rs` says so in its own words:
            // "one means 'reload', the other means 'you already won'".
            // `token_consumed` is a decision that landed; `conflict` is one
            // that did **not** — the run moved on to a different step, or is no
            // longer parked at all. Dropping the queued row is still right (a
            // blind retry would answer a question nobody asked), but the card
            // has to be re-read so the user sees what is actually pending
            // rather than being told their tap was recorded.
            return .superseded
        case .approvalExpired, .gone:
            return .expired
        case .unauthorized:
            return .unauthorized
        case .needsReauth:
            return .needsReauth
        case .badRequest, .notFound, .forbidden, .payloadTooLarge:
            // Permanent. Asking again produces the same answer for ever.
            return .rejected(message)
        default:
            return .retry(message, retryAfter: retryAfter)
        }
    }

    private func tracingRejected(_ action: PendingActionRecord, _ message: String) {
        #if DEBUG
        print("outbox: dropped \(action.kind) for \(action.feedItemId) — \(message)")
        #endif
    }

    private func refresh(feedItemID: String) async {
        guard let item = try? await source.feedItem(id: feedItemID) else { return }
        try? await store.saveFeedItem(item)
    }
}

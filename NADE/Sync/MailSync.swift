//
//  MailSync.swift
//  NADE
//
//  Source → store, and the state machine that decides when to ask again.
//
//  Nothing here renders. It writes rows and sets `state`; every screen reads
//  the store through `ValueObservation`, so a refresh that changes nothing
//  costs one query and no redraw.
//

import Foundation

/// Which request a failure came from.
///
/// One global error slot was wrong in two directions at once: a successful
/// mailbox refresh cleared a *thread's* failure while the thread stayed blank,
/// and a thread's failure appeared on the mail list, which is a different
/// screen that is working fine. Errors are recorded against the operation that
/// produced them, and cleared by the same one succeeding.
nonisolated enum ProblemKind: Hashable, Sendable {
    case account
    case list
    case thread
}

@MainActor
@Observable
final class MailSync {

    private(set) var state: AppState = .unpaired
    private(set) var problems: [ProblemKind: ConnectionProblem] = [:]

    /// What a mail list shows. An account-level failure outranks a paging one:
    /// "couldn't reach the server" explains the missing page as well.
    var connectionProblem: ConnectionProblem? {
        problems[.account] ?? problems[.list]
    }

    /// What a thread shows. EDGE (P13): this is a **second** slot beside the
    /// thread's own `partial` flag, because `API.md` §2 says `partial` is
    /// *produced by* an upstream failure — the two are routinely true together.
    var threadProblem: ConnectionProblem? {
        problems[.thread] ?? problems[.account]
    }

    let source: any MailSource
    let store: MailStore

    private let clock: NADEClock
    private let sleep: @Sendable (Duration) async throws -> Void
    private var pollTask: Task<Void, Never>?
    /// Remembered from the last successful `GET /me`, so a later
    /// `409 needs_reauth` can write `account.status` without blanking the email
    /// the Settings row is about to render beside it.
    private var currentEmail = ""
    /// EDGE (P8). `API.md` §0 says a 429 carries `Retry-After` in seconds, and
    /// a client that parses it and then asks again immediately has not honoured
    /// anything. Every entry point checks this before spending a request.
    private var retryNotBefore: Date?

    /// EDGE (P8) / `AppState.syncPending`: the server's first sync is minutes
    /// long, so the app asks again rather than waiting for a launch. Fast at
    /// first because the common case is "nearly done", then backing off so a
    /// genuinely stuck sync is not a busy loop.
    ///
    /// The total is ~5 minutes, which is what `PLAN.md`'s "~1–2 min for a
    /// 30-day window" needs headroom over. When it runs out the state stays
    /// `syncPending` — honest, it *is* still pending — and the next foreground
    /// starts a fresh round rather than stranding the app forever.
    static let pollIntervals: [Duration] = [
        .seconds(5), .seconds(5), .seconds(10), .seconds(10), .seconds(20),
        .seconds(30), .seconds(30), .seconds(30), .seconds(30), .seconds(30),
        .seconds(30), .seconds(30), .seconds(30),
    ]

    init(
        source: any MailSource,
        store: MailStore,
        clock: NADEClock = .live,
        sleep: @escaping @Sendable (Duration) async throws -> Void = { try await Task.sleep(for: $0) }
    ) {
        self.source = source
        self.store = store
        self.clock = clock
        self.sleep = sleep
    }

    // No `deinit { pollTask?.cancel() }`: `deinit` is nonisolated and cannot
    // touch main-actor state. The task holds `self` weakly and every iteration
    // re-checks `state`, so it stops on its own once nothing is listening.

    // MARK: - The pipeline

    /// One pass: who are we, what mailboxes are there, and which state does
    /// that put us in. Safe to call on every foreground and every appearance —
    /// D29 keeps all four screens in the tree, so this runs more often than a
    /// tab switch and has to be cheap and idempotent.
    @discardableResult
    func refresh() async -> AppState {
        guard (try? source.isPaired()) == true else {
            stopPolling()
            state = .unpaired
            return state
        }
        guard mayRequest() else { return state }

        do {
            let me = try await source.me()
            currentEmail = me.email
            try await store.upsertAccount(me)
            clear(.account)

            guard me.status == .ok, !me.email.isEmpty else {
                stopPolling()
                state = .needsGmail
                return state
            }

            let boxes = try await source.mailboxes()

            if boxes.isEmpty {
                // **An empty answer is not a deletion.** `api/mail.rs` returns
                // `mailboxes: []` for the one to two minutes its first sync
                // takes, and `replaceMailboxes([])` would cascade every join
                // row and every cursor away — so a device that already had mail
                // would lose the cache offline behaviour depends on, mid-sync,
                // for no reason. Keep what is there and wait.
                state = .syncPending
                startPollingIfNeeded()
            } else {
                try await store.replaceMailboxes(boxes)
                stopPolling()
                state = .ready
            }
        } catch let failure as APIFailure {
            await handle(failure, from: .account)
        } catch is CancellationError {
            // A screen went away mid-request. Not a failure.
        } catch {
            problems[.account] = ConnectionProblem(
                message: APIFailure.malformedResponse("\(error)").userFacingMessage
            )
        }
        return state
    }

    // MARK: Prerequisites
    //
    // There is one rule underneath all three entry points, and it is a chain:
    // **mailboxes → list row → detail.** A page cannot be filed against a
    // mailbox the store has never seen, and a detail cannot be stored for a
    // thread no list row mentions (`applyThreadDetail` refuses to invent one
    // out of a detail that carries no `snippet`, `unread`, `msg_count` or
    // `agent_note`).
    //
    // It used to be expressed twice, as two different one-shot retries with an
    // `allowingRetry` flag on both public signatures — so every call site could
    // see a parameter it must never pass, for a mechanism it should not know
    // about. Naming the prerequisite instead makes it one idempotent operation
    // that any future entry point gets for free. P5's feed→thread jump and P6's
    // push deep link are both cold opens on exactly this path.

    /// Fetches the mailbox list **only if the store has none**.
    ///
    /// Deliberately narrow: `refresh()` is the whole state machine, and a page
    /// landing one tick early should not drag `GET /me`, the poll timer and a
    /// possible transition to `.needsGmail` along with it.
    private func ensureMailboxes() async throws {
        if try await store.hasMailboxes() { return }
        let boxes = try await source.mailboxes()
        guard !boxes.isEmpty else { return }
        try await store.replaceMailboxes(boxes)
    }

    private func ensureListRow(for threadID: String, in mailboxID: String) async throws {
        if try await store.hasThreadRow(id: threadID) { return }
        _ = await loadThreads(mailboxID: mailboxID, resetting: true)
    }

    /// A page of a mailbox. `resetting` is a pull back to the top, which bumps
    /// the generation so a page still in flight from before is discarded.
    @discardableResult
    func loadThreads(mailboxID: String, resetting: Bool) async -> Bool {
        guard mayRequest() else { return false }
        do {
            try await ensureMailboxes()
            let cursor: String?
            let expected: Int?
            if resetting {
                cursor = nil
                expected = nil
            } else {
                let sync = try await store.syncState(for: mailboxID)
                // Nothing more to ask for. Not an error, and not a retry.
                guard let next = sync?.nextCursor, sync?.reachedEnd == false else { return false }
                cursor = next
                expected = sync?.generation
            }

            let page = try await source.threads(mailboxID: mailboxID, cursor: cursor)
            try await store.applyThreadPage(page, mailboxID: mailboxID, resetting: resetting,
                                            expectedGeneration: expected, now: clock.now())
            clear(.list)
            return true
        } catch let failure as APIFailure {
            await handle(failure, from: .list)
            return false
        } catch {
            // Cancellation, or a mailbox that vanished between the prerequisite
            // and the write. Neither is worth a banner.
            return false
        }
    }

    /// A thread's detail, with the mailbox it was opened from.
    ///
    /// The mailbox is what satisfies the prerequisite when a screenshot, a feed
    /// item or a push opens a thread cold: fetch the list it belongs to rather
    /// than fabricate a row out of a detail that does not carry one.
    func loadThread(id: String, in mailboxID: String) async {
        guard mayRequest() else { return }
        do {
            try await ensureListRow(for: id, in: mailboxID)
            let thread = try await source.thread(id: id)
            try await store.applyThreadDetail(thread, now: clock.now())
            clear(.thread)
        } catch let failure as APIFailure {
            await handle(failure, from: .thread)
        } catch is CancellationError {
            // The screen went away mid-request.
        } catch {
            problems[.thread] = ConnectionProblem(
                message: String(localized: "Couldn't open that conversation.",
                                comment: "Shown on the thread screen when its detail could not be loaded")
            )
        }
    }

    // MARK: - Failures

    /// Every failure path goes through here.
    ///
    /// It used to live inside `refresh()` alone, which meant a `409
    /// needs_reauth` hit while paging or opening a thread never wrote
    /// `account.status`, never surfaced "Sign in again", and never cleared a
    /// revoked token — the user saw an error with no way out, depending
    /// entirely on which request happened to fail first.
    private func handle(_ failure: APIFailure, from kind: ProblemKind) async {
        if failure.isCancellation { return }

        if let retryAfter = failure.retryAfter {
            retryNotBefore = clock.now().addingTimeInterval(retryAfter)
        }
        problems[kind] = ConnectionProblem(failure)

        if failure.needsReauth {
            // EDGE (P9): the recovery affordance — 1g's sub-label and 1k's
            // "Sign in again" row — is driven off `account.status`. Surfacing
            // this only as an error would leave it invisible.
            try? await store.upsertAccount(WireMe(email: currentEmail, status: .needsReauth))
            stopPolling()
            state = .needsGmail
            return
        }

        if failure.isUnauthorized {
            // A revoked device token. Not a network problem — the credential is
            // dead, and leaving it in place would keep presenting it.
            try? source.unpair()
            stopPolling()
            state = .unpaired
            return
        }

        // EDGE (P8) / A16. The server is out of reach, or answering badly, but
        // this device is paired and has mail cached. Leaving `state` at its
        // initial `.unpaired` would put "Not paired yet." over a full mailbox
        // list — telling the user the setup they completed did not happen,
        // because a 502 came back. Any failure counts, not only a transport
        // one: a rate limit and an unreachable host are equally not "unpaired".
        if state == .unpaired, (try? await store.hasMailboxes()) == true {
            state = .ready
        }
    }

    private func clear(_ kind: ProblemKind) {
        problems[kind] = nil
        retryNotBefore = nil
    }

    /// EDGE (P8). Honours `Retry-After` rather than merely parsing it.
    private func mayRequest() -> Bool {
        guard let retryNotBefore else { return true }
        if clock.now() >= retryNotBefore {
            self.retryNotBefore = nil
            return true
        }
        return false
    }

    // MARK: - Pairing and linking

    /// Points the app at `origin` and pairs with it.
    ///
    /// Changing the server throws the old one's mail away. The Keychain item is
    /// origin-bound, so the *token* could never cross — but the database is one
    /// file, and `INBOX` and `SENT` are the same ids on every account, so
    /// server A's threads would sit under server B's mailboxes until each one
    /// happened to be refreshed. Two servers' mail in one list is a data-
    /// isolation failure, not a stale cache.
    func pair(origin: URL, code: String, deviceName: String) async throws {
        if ServerSetting.setOrigin(origin) {
            try? source.unpair()
            try await store.removeEverything()
            state = .unpaired
        }
        _ = try await source.pair(code: code, deviceName: deviceName)
        await refresh()
    }

    func unpair() async throws {
        try source.unpair()
        try await store.removeEverything()
        stopPolling()
        state = .unpaired
    }

    func gmailLinkURL() async throws -> URL {
        let link = try await source.gmailLink()
        guard let url = URL(string: link.url) else {
            throw APIFailure.malformedResponse("gmail link URL was not a URL")
        }
        return url
    }

    // MARK: - Polling

    private func startPollingIfNeeded() {
        guard pollTask == nil else { return }
        pollTask = Task { [weak self] in
            guard let self else { return }
            for interval in Self.pollIntervals {
                do { try await self.sleep(interval) } catch { return }
                if Task.isCancelled { return }
                guard self.state == .syncPending else { return }

                // One tick is one `refresh()`. The loop used to re-implement
                // its mailbox path — the "empty means wait, not delete" rule
                // (D72) and the failure routing (D74) both written a second
                // time. Going through `refresh()` keeps them in one place and
                // strictly widens what a poll can notice: an unpaired device or
                // a `needs_reauth` on `/me` mid-sync is now caught too.
                //
                // Re-entrancy is safe: `startPollingIfNeeded` guards on
                // `pollTask == nil`, and the success path calls `stopPolling()`,
                // which cancels the task this loop is running in.
                await self.refresh()
                if self.state != .syncPending { return }
            }
            self.pollTask = nil
        }
    }

    private func stopPolling() {
        pollTask?.cancel()
        pollTask = nil
    }
}

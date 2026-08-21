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
    case feed
    case agents
}

@MainActor
@Observable
final class MailSync {

    private(set) var state: AppState = .unpaired
    private(set) var problems: [ProblemKind: ConnectionProblem] = [:]

    /// What a screen shows when both it and the account have a problem.
    ///
    /// **One rule, stated once.** An account-level failure outranks a
    /// screen-level one, because "couldn't reach the server" already explains
    /// the missing page, the missing cards and the missing agents. P3 added two
    /// more accessors that each restated it — and both restated it *backwards*,
    /// putting the screen first while their own doc comments claimed the
    /// opposite. A fourth screen would have been a fourth chance to get it
    /// wrong.
    func problem(for kind: ProblemKind) -> ConnectionProblem? {
        problems[.account] ?? problems[kind]
    }

    /// What a mail list shows.
    var connectionProblem: ConnectionProblem? { problem(for: .list) }
    /// What 2a shows.
    var feedProblem: ConnectionProblem? { problem(for: .feed) }
    /// What 1b shows.
    var agentsProblem: ConnectionProblem? { problem(for: .agents) }

    /// What a thread shows. EDGE (P13): this is a **second** slot beside the
    /// thread's own `partial` flag, because `API.md` §2 says `partial` is
    /// *produced by* an upstream failure — the two are routinely true together.
    ///
    /// The **one** screen that inverts the rule above, deliberately: 1f is
    /// already showing "some of this conversation is missing", and the specific
    /// reason that thread failed is more use there than the general one.
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
        // Built here rather than by the screens: an approve queued on 2a and a
        // drain triggered by a foreground have to share one driver, or two
        // drains race for the same rows. `onNeedsReauth` is how a dead Gmail
        // credential found while sending reaches this state machine without the
        // outbox needing to know what a `MailSync` is.
        self.outbox = OutboxDriver(store: store,
                                   source: source,
                                   origin: { source.origin })
        self.outbox.onNeedsReauth = { [weak self] in
            Task { @MainActor in await self?.markNeedsGmail() }
        }
    }

    /// The single approve/skip queue. See `Outbox.swift`.
    let outbox: OutboxDriver

    /// Send whatever the outbox is holding.
    ///
    /// `OutboxDriver.drain`'s own comment said it was "called on app foreground,
    /// when the feed appears, and after a successful pair" — and nothing called
    /// it but `enqueue`. A queue whose only trigger is a *new* enqueue is not a
    /// queue: kill the app between the durable write and the request, or take a
    /// 429, and the approval sits there until the user happens to tap another
    /// one. These three entry points are what the comment always claimed.
    func drainOutbox() async {
        guard (try? source.isPaired()) == true else { return }
        await outbox.drain()
    }

    /// `POST /feed/seen` — how an `info` card stops being new (`API.md` §7).
    ///
    /// Not routed through the outbox: seen is idempotent and carries no
    /// capability token, so a lost one costs a badge that corrects itself on the
    /// next refresh, and queueing it would put a durable row on disk per glance.
    func markSeen(ids: [String]) async {
        guard !ids.isEmpty, mayRequest() else { return }
        do {
            let response = try await source.seen(ids: ids)
            // **Not `loadFeed(resetting: true)`.** That deletes every cached
            // row and refetches page 1, so a read receipt — the most incidental
            // thing on the screen — threw away pages 2..n, shrank the list under
            // the user's finger, and the relayout scrolled more rows into view,
            // firing more receipts. The rows are already known; only their
            // status and the badge change.
            // One transaction, not one per id. `HomeFeedModel` coalesces a
            // flick into a single `POST /feed/seen` precisely so this is cheap;
            // fanning it back out into N writes would fire `observeFeed` N
            // times, and each fire re-decodes and re-renders the whole feed.
            try? await store.markFeedItemsSeen(ids: ids, newCount: response.newCount)
            clear(.feed)
        } catch let failure as APIFailure {
            await handle(failure, from: .feed)
        } catch {}
    }

    /// EDGE (P9), reached from the outbox: the Gmail credential died while a
    /// queued approve was in flight. Same landing as `handle`'s `needs_reauth`
    /// branch, so the recovery affordance appears wherever the failure happened.
    private func markNeedsGmail() async {
        try? await store.upsertAccount(WireMe(email: currentEmail, status: .needsReauth))
        stopPolling()
        state = .needsGmail
    }

    // No `deinit { pollTask?.cancel() }`: `deinit` is nonisolated and cannot
    // touch main-actor state. The task holds `self` weakly and every iteration
    // re-checks `state`, so it stops on its own once nothing is listening.

    // MARK: - The pipeline

    /// One pass: who are we, what mailboxes are there, and which state does
    /// that put us in. Safe to call on every foreground and every appearance —
    /// D29 keeps all four screens in the tree, so this runs more often than a
    /// tab switch and has to be cheap and idempotent.
    ///
    /// **Single-flight, not debounce-and-drop.** Three triggers fire this at
    /// launch alone — the root `.task`, the scenePhase observer and 1g's own
    /// `.task` — and each used to run the whole `GET /me` + `GET /mailboxes`
    /// chain for one answer. A caller arriving *while* one pass runs awaits
    /// that pass and shares its result; a caller arriving *after* completion
    /// still runs fresh, which is what keeps the scenePhase trigger's purpose
    /// — noticing a Gmail consent that completed in Safari — working (D73).
    @discardableResult
    func refresh() async -> AppState {
        if let running = refreshTask { return await running.value }
        let task = Task { await self.runRefreshClearingTheSlot() }
        refreshTask = task
        return await task.value
    }

    /// The in-flight pass, when there is one. Cleared by the pass itself.
    private var refreshTask: Task<AppState, Never>?

    /// The slot is cleared *inside* the task, in the same synchronous stretch
    /// as its return — so by the moment any awaiter resumes, the slot is empty
    /// and that awaiter's own follow-up `refresh()` starts a fresh pass rather
    /// than re-joining a finished one. Clearing from the creating caller
    /// instead would leave a window where a joiner resumes first, calls again,
    /// and is handed the stale result of a pass that already ended.
    private func runRefreshClearingTheSlot() async -> AppState {
        defer { refreshTask = nil }
        return await performRefresh()
    }

    private func performRefresh() async -> AppState {
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

            // Every foreground and every screen appearance runs `refresh()`, so
            // this is the drain trigger the outbox always documented. Detached
            // so a slow queue never delays the mailbox list.
            Task { await self.drainOutbox() }
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

    /// How long a landed page spares a mailbox the resetting re-fetch that a
    /// routine re-appearance would otherwise run.
    static let listFreshWindow: TimeInterval = 60

    /// Whether a page for `mailboxID` landed within `listFreshWindow`.
    ///
    /// 1e's `.task(id:)` re-fires on every pop back from 1f, and the
    /// `loadThreads(resetting: true)` under it deletes the join rows and
    /// truncates the list to page 1 — under the user's scroll position, for
    /// rows that arrived seconds ago. `lastPageAt` is written by
    /// `applyThreadPage` from this same clock, so the comparison is one clock
    /// against itself. Explicit refreshes keep calling
    /// `loadThreads(resetting: true)` directly and stay resetting.
    func hasFreshThreadPage(for mailboxID: String) async -> Bool {
        guard let lastPageAt = try? await store.syncState(for: mailboxID)?.lastPageAt else {
            return false
        }
        let age = clock.now().timeIntervalSince(lastPageAt)
        // EDGE: clock skew. A zone change or an NTP correction can put
        // `lastPageAt` ahead of `now`. A negative age reads *stale*, because
        // the failure modes are asymmetric: an extra fetch costs one request,
        // while "fresh forever" pins the list until the clock catches up.
        return age >= 0 && age < Self.listFreshWindow
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

    // MARK: - P3: the feed and the agents

    /// The home feed. `resetting` is the pull-to-top; otherwise this pages.
    ///
    /// Not folded into `refresh()`: the feed is the Ask tab's root and the
    /// mailbox list is the Mail tab's, and both tabs stay in the tree at all
    /// times (D29), so one combined refresh would make every mail launch pay for
    /// a feed it may never show - and hide a feed failure behind a mail one.
    @discardableResult
    func loadFeed(resetting: Bool) async -> Bool {
        guard mayRequest() else { return false }
        do {
            let cursor: String?
            if resetting {
                cursor = nil
            } else {
                guard let next = try await store.feedCursor()?.nextCursor else { return false }
                cursor = next
            }
            let page = try await source.feed(cursor: cursor)
            _ = try await store.saveFeed(page, resetting: resetting)
            clear(.feed)
            return true
        } catch let failure as APIFailure {
            await handle(failure, from: .feed)
            return false
        } catch {
            return false
        }
    }

    func loadAgents() async {
        guard mayRequest() else { return }
        do {
            try await store.saveAgents(try await source.agents())
            clear(.agents)
        } catch let failure as APIFailure {
            await handle(failure, from: .agents)
        } catch {}
    }

    /// The agent writes. They are **not** cached: 1c edits one agent at a time
    /// and needs the server's recompiled spans back, so a store round trip would
    /// only add a way for the screen and the database to disagree. The list is
    /// re-read afterwards, which is what keeps 1b honest.
    func agent(id: String) async throws -> WireAgent {
        try await source.agent(id: id)
    }

    /// `POST` and `PATCH` both **return the agent**, so the list is updated from
    /// the response rather than refetched. The `GET /agents` that used to follow
    /// every write was a second round trip per control tap — and because 1c's
    /// writes are a serialised chain, the footer stayed disabled for both.
    func createAgent(nlDefinition: String) async throws -> WireAgent {
        let created = try await source.createAgent(nlDefinition: nlDefinition)
        try? await store.upsertAgent(created)
        return created
    }

    func updateAgent(id: String, patch: AgentPatch) async throws -> WireAgent {
        let updated = try await source.updateAgent(id: id, patch: patch)
        try? await store.upsertAgent(updated)
        return updated
    }

    func deleteAgent(id: String) async throws {
        try await source.deleteAgent(id: id)
        try? await store.removeAgent(id: id)
    }

    func runAgent(id: String) async throws -> WireRunStarted {
        try await source.runAgent(id: id)
    }

    func ask(query: String, threadID: String? = nil, routeHint: AskRoute? = nil)
        -> AsyncThrowingStream<AskEvent, any Error> {
        source.ask(query: query, threadID: threadID, routeHint: routeHint)
    }

    func downloadAttachment(gmailID: String, attachmentID: String, filename: String)
        async throws -> URL {
        try await source.downloadAttachment(gmailID: gmailID, attachmentID: attachmentID,
                                            filename: filename)
    }

    /// The source itself, for the one caller that is not on the main actor.
    /// See `MailSource.attachmentData`.
    nonisolated var attachmentSource: any MailSource { source }

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
            // `applyThreadDetail` deliberately *skips* a detail whose thread no
            // list row mentions rather than inventing the list half — and a
            // skip has to be said out loud. This is P5's feed→thread jump and
            // P6's push deep link being built early: a cold open whose mailbox
            // page happens not to list the thread would otherwise land on 1f
            // with a nav bar over nothing — no rows, no caption, nothing to
            // act on. `hasDetail` is the read that tells a silent skip apart
            // from a write that landed.
            guard try await store.hasDetail(id: id) else {
                problems[.thread] = ConnectionProblem(message: StateCopy.threadOpenFailed)
                return
            }
            clear(.thread)
        } catch let failure as APIFailure {
            await handle(failure, from: .thread)
        } catch is CancellationError {
            // The screen went away mid-request.
        } catch {
            // EDGE: the store failing to write or read the detail lands here —
            // same sentence, same slot, because the screen's remedy is the same.
            problems[.thread] = ConnectionProblem(message: StateCopy.threadOpenFailed)
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
        // EDGE: a refresh already in flight when the pair landed started
        // before the credential existed — joining it would report `.unpaired`
        // for a device that just paired, until the next foreground. Let it
        // finish, then run a fresh pass that can see the new token.
        await refreshTask?.value
        await refresh()
        await drainOutbox()
    }

    func unpair() async throws {
        // EDGE: a refresh in flight holds pre-unpair answers, and left running
        // it could write the account row back after `removeEverything` and
        // lift `state` off `.unpaired`. Cancellation is cooperative — the
        // request throws `CancellationError`, which `performRefresh` treats as
        // "change nothing" — so this narrows the race rather than closing it;
        // the token below is already gone, so a request that does slip out
        // fails as unauthorized.
        refreshTask?.cancel()
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

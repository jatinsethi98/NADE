//
//  AppStateTests.swift
//  NADETests
//
//  P2 A17 and EDGE (P9, P13).
//
//  The failure this suite exists for is the one a "fetch once at launch" client
//  cannot see in testing and cannot survive in use: `docs/PLAN.md` puts the
//  server's first Gmail sync at ~1–2 minutes, and `api/mail.rs` deliberately
//  answers `mailboxes: []` for its duration. Pair, link, fetch — and the app is
//  empty, with no spinner, no refresh control and no completion event, until
//  somebody relaunches it.
//

import XCTest
import GRDB
@testable import NADE

/// A source whose answers are scripted, so a two-minute sync is a two-line test.
private final class ScriptedSource: MailSource, @unchecked Sendable {
    let origin = URL(string: "http://localhost:8080")!
    let storeLocation: StoreLocation = .fixtures

    var paired = true
    var meResult: Result<WireMe, Error> = .success(WireMe(email: "a@b.c", status: .ok))
    /// One entry per call; the last repeats.
    var mailboxPages: [Result<[WireMailbox], Error>] = [.success([])]
    var threadPage: Result<WireThreadPage, Error> = .success(WireThreadPage(threads: [], nextCursor: nil))
    var threadDetail: Result<WireThread, Error>?

    private(set) var mailboxCalls = 0
    private(set) var meCalls = 0
    private(set) var threadsCalls = 0
    private(set) var unpaired = false

    /// When true, `me()` parks after counting the call until `releaseMe()` —
    /// how the single-flight test holds one refresh pass verifiably in flight
    /// while a second caller arrives. A sleep instead would make the overlap a
    /// bet on the scheduler.
    var holdMe = false
    private let meLock = NSLock()
    private var meWaiters: [CheckedContinuation<Void, Never>] = []

    func releaseMe() {
        let waiters = meLock.withLock {
            let held = meWaiters
            meWaiters.removeAll()
            holdMe = false
            return held
        }
        waiters.forEach { $0.resume() }
    }

    func isPaired() throws -> Bool { paired }
    func pair(code: String, deviceName: String) async throws -> Credential {
        paired = true
        return Credential(baseURL: origin, token: "nade_x")
    }
    func unpair() throws { paired = false; unpaired = true }

    func me() async throws -> WireMe {
        meCalls += 1
        if meLock.withLock({ holdMe }) {
            await withCheckedContinuation { continuation in
                meLock.withLock { meWaiters.append(continuation) }
            }
        }
        return try meResult.get()
    }
    func gmailLink() async throws -> WireGmailLink {
        WireGmailLink(url: origin.absoluteString, expiresAt: Date())
    }

    func mailboxes() async throws -> [WireMailbox] {
        defer { mailboxCalls += 1 }
        let index = min(mailboxCalls, mailboxPages.count - 1)
        return try mailboxPages[index].get()
    }

    func threads(mailboxID: String, cursor: String?) async throws -> WireThreadPage {
        defer { threadsCalls += 1 }
        return try threadPage.get()
    }

    func thread(id: String) async throws -> WireThread {
        guard let threadDetail else {
            throw APIFailure.server(code: .notFound, status: 404, message: "no", retryAfter: nil)
        }
        return try threadDetail.get()
    }

    // P3's endpoints. `AppStateTests` is about the connection state machine,
    // which none of these participate in; a source that refuses them keeps that
    // scope honest rather than quietly widening it.
    func feed(cursor: String?) async throws -> WireFeedPage { throw APIFailure.notServedYet("feed") }
    func feedItem(id: String) async throws -> WireFeedItem { throw APIFailure.notServedYet("feed item") }
    func approve(feedItemID: String, approvalToken: String) async throws -> WireApproveResponse {
        throw APIFailure.notServedYet("approve")
    }
    func skip(feedItemID: String, approvalToken: String) async throws -> WireSkipResponse {
        throw APIFailure.notServedYet("skip")
    }
    func seen(ids: [String]) async throws -> WireSeenResponse { throw APIFailure.notServedYet("seen") }
    func agents() async throws -> [WireAgentRow] { throw APIFailure.notServedYet("agents") }
    func agent(id: String) async throws -> WireAgent { throw APIFailure.notServedYet("agent") }

    func createAgent(nlDefinition: String) async throws -> WireAgent {
        throw APIFailure.notServedYet("create agent")
    }

    func updateAgent(id: String, patch: AgentPatch) async throws -> WireAgent {
        throw APIFailure.notServedYet("update agent")
    }

    func deleteAgent(id: String) async throws { throw APIFailure.notServedYet("delete agent") }

    func runAgent(id: String) async throws -> WireRunStarted {
        throw APIFailure.notServedYet("run agent")
    }

    func ask(query: String, threadID: String?, routeHint: AskRoute?)
        -> AsyncThrowingStream<AskEvent, any Error> {
        AsyncThrowingStream { $0.finish(throwing: APIFailure.notServedYet("ask")) }
    }

    func downloadAttachment(gmailID: String, attachmentID: String, filename: String)
        async throws -> URL {
        throw APIFailure.notServedYet("attachment")
    }

    func attachmentData(gmailID: String, attachmentID: String) async throws -> Data {
        throw APIFailure.notServedYet("attachment bytes")
    }
}

@MainActor
final class AppStateTests: XCTestCase {

    private var store: MailStore!
    private var source: ScriptedSource!

    override func setUpWithError() throws {
        store = try MailStore.inMemory()
        source = ScriptedSource()
    }

    private func sync(sleepInstantly: Bool = true) -> MailSync {
        MailSync(
            source: source, store: store,
            clock: .fixed(ContractFixture.pinnedNow),
            sleep: sleepInstantly ? { _ in } : { try await Task.sleep(for: $0) }
        )
    }

    private func boxes() throws -> [WireMailbox] { try ContractFixture.mailboxes() }

    // MARK: - The pipeline

    func testAnUnpairedDeviceStopsAtUnpaired() async throws {
        source.paired = false
        let state = await sync().refresh()
        XCTAssertEqual(state, .unpaired)
    }

    /// `API.md` §1: before a Gmail account is connected, `/me` is a 200 with an
    /// empty email — "a freshly paired device asking who it is has not made an
    /// error". So this is a normal state, not a failure.
    func testAPairedDeviceWithNoGmailAccountNeedsGmail() async throws {
        source.meResult = .success(WireMe(email: "", status: .needsReauth))
        let state = await sync().refresh()
        XCTAssertEqual(state, .needsGmail)
    }

    /// A17, and the reason this whole file exists. The server answers with an
    /// empty mailbox list while its first sync runs; the app must get to
    /// `ready` on its own.
    func testSyncPendingResolvesToReadyWithoutARelaunch() async throws {
        source.mailboxPages = [.success([]), .success([]), .success(try boxes())]

        let sync = sync()
        let first = await sync.refresh()
        XCTAssertEqual(first, .syncPending, "an empty mailbox list right after linking is not 'ready'")

        // The poll is what closes the gap. Nothing else here calls refresh().
        try await waitFor { sync.state == .ready }
        XCTAssertGreaterThanOrEqual(source.mailboxCalls, 3)

        let stored = try await Self.read(store) { try $1.mailboxes($0) }
        XCTAssertEqual(stored.count, 14, "the mailboxes the poll found were never written")
    }

    func testAPopulatedMailboxListIsReadyImmediatelyAndDoesNotPoll() async throws {
        source.mailboxPages = [.success(try boxes())]
        let sync = sync()
        let readyState = await sync.refresh()
        XCTAssertEqual(readyState, .ready)

        try await Task.sleep(for: .milliseconds(50))
        XCTAssertEqual(source.mailboxCalls, 1, "a ready app kept polling")
    }

    // MARK: - EDGE (P9)

    /// A `409 needs_reauth` is not just something to show. The recovery
    /// affordance — 1g's "Needs sign-in" sub-label and 1k's "Sign in again" row
    /// — is driven off `account.status`, so a client that only surfaced the
    /// error would tell the user something is wrong and give them no way out.
    func testNeedsReauthWritesTheAccountStatusThatDrivesTheRecoveryRow() async throws {
        source.meResult = .success(WireMe(email: "a@b.c", status: .ok))
        source.mailboxPages = [.success(try boxes())]
        let sync = sync()
        let readyState = await sync.refresh()
        XCTAssertEqual(readyState, .ready)

        source.meResult = .failure(APIFailure.server(code: .needsReauth, status: 409,
                                                     message: "Sign in to Gmail again.", retryAfter: nil))
        let needsGmailState = await sync.refresh()
        XCTAssertEqual(needsGmailState, .needsGmail)

        let account = try await Self.read(store) { try $1.account($0) }
        XCTAssertEqual(account?.status, .needsReauth)
        XCTAssertEqual(account?.email, "a@b.c", "the email the Settings row renders was thrown away")
        XCTAssertEqual(sync.connectionProblem?.message, "Sign in to Gmail again.")
    }

    /// A revoked device token is not a network problem: the credential is dead
    /// and the honest state is "unpaired", with the token cleared so the app
    /// does not keep presenting it.
    func testA401ClearsTheCredentialAndReturnsToUnpaired() async throws {
        source.meResult = .failure(APIFailure.server(code: .unauthorized, status: 401,
                                                     message: "no", retryAfter: nil))
        let sync = sync()
        let unpairedState = await sync.refresh()
        XCTAssertEqual(unpairedState, .unpaired)
        XCTAssertTrue(source.unpaired)
    }

    // MARK: - EDGE (P13) and A16's model half

    /// The rows already on screen are still the best answer available. An error
    /// is **additive**: it sets the banner and leaves the store alone.
    func testAnUnreachableServerKeepsTheCachedRowsAndSaysSo() async throws {
        source.mailboxPages = [.success(try boxes())]
        let sync = sync()
        _ = await sync.refresh()
        let before = try await Self.read(store) { try $1.mailboxes($0) }
        XCTAssertEqual(before.count, 14)

        source.meResult = .failure(APIFailure.unreachable(.notConnectedToInternet))
        _ = await sync.refresh()

        let after = try await Self.read(store) { try $1.mailboxes($0) }
        XCTAssertEqual(after.count, 14, "an offline refresh cleared the cache")
        XCTAssertEqual(sync.connectionProblem?.message, "Couldn't reach the server.")
    }

    func testASuccessfulRefreshClearsTheBanner() async throws {
        source.meResult = .failure(APIFailure.unreachable(.timedOut))
        let sync = sync()
        _ = await sync.refresh()
        XCTAssertNotNil(sync.connectionProblem)

        source.meResult = .success(WireMe(email: "a@b.c", status: .ok))
        source.mailboxPages = [.success(try boxes())]
        _ = await sync.refresh()
        XCTAssertNil(sync.connectionProblem)
    }

    // MARK: - Pagination through the sync

    /// EDGE (P5). "No next cursor" is not a failure and not a retry — it is the
    /// end of the list, and load-more has to stop rather than ask again.
    func testLoadMoreStopsAtTheEndOfTheList() async throws {
        source.mailboxPages = [.success(try boxes())]
        let sync = sync()
        _ = await sync.refresh()

        source.threadPage = .success(try ContractFixture.decode(WireThreadPage.self, from: "threads"))
        let loadedFirst = await sync.loadThreads(mailboxID: "INBOX", resetting: true)
        XCTAssertTrue(loadedFirst)

        source.threadPage = .success(try ContractFixture.decode(WireThreadPage.self, from: "threads_last_page"))
        let loadedSecond = await sync.loadThreads(mailboxID: "INBOX", resetting: false)
        XCTAssertTrue(loadedSecond)

        // Nothing left to ask for.
        let loadedThird = await sync.loadThreads(mailboxID: "INBOX", resetting: false)
        XCTAssertFalse(loadedThird)
    }

    // MARK: - The post-implementation review's findings

    /// **An empty answer is not a deletion.** The server returns
    /// `mailboxes: []` while its first sync runs, and `replaceMailboxes([])`
    /// cascades away every join row and every cursor — so a device that already
    /// had mail would lose the cache its offline behaviour depends on, mid-sync,
    /// for no reason at all.
    func testAnEmptyMailboxListDoesNotDestroyWhatIsAlreadyCached() async throws {
        source.mailboxPages = [.success(try boxes())]
        let sync = sync()
        let ready = await sync.refresh()
        XCTAssertEqual(ready, .ready)

        source.mailboxPages = [.success([])]
        let pending = await sync.refresh()
        XCTAssertEqual(pending, .syncPending)

        let kept = try await Self.read(store) { try $1.mailboxes($0) }
        XCTAssertEqual(kept.count, 14, "an in-progress sync wiped the cache")
    }

    /// A `409 needs_reauth` used to be handled only by `refresh()`. Hit while
    /// paging or opening a thread, it left `account.status` untouched — so the
    /// user got an error and no "Sign in again" row to act on.
    func testNeedsReauthIsHandledFromEveryRequestPathNotJustRefresh() async throws {
        source.mailboxPages = [.success(try boxes())]
        let sync = sync()
        _ = await sync.refresh()

        let reauth = APIFailure.server(code: .needsReauth, status: 409,
                                        message: "Sign in to Gmail again.", retryAfter: nil)
        source.threadPage = .failure(reauth)
        _ = await sync.loadThreads(mailboxID: "INBOX", resetting: true)

        XCTAssertEqual(sync.state, .needsGmail, "a 409 while paging left the app in the wrong state")
        let account = try await Self.read(store) { try $1.account($0) }
        XCTAssertEqual(account?.status, .needsReauth)
    }

    func testARevokedTokenIsClearedFromEveryRequestPath() async throws {
        source.mailboxPages = [.success(try boxes())]
        let sync = sync()
        _ = await sync.refresh()

        source.threadDetail = .failure(APIFailure.server(code: .unauthorized, status: 401,
                                                         message: "no", retryAfter: nil))
        await sync.loadThread(id: "18f2a1b3c4d5e6f7", in: "INBOX")
        XCTAssertTrue(source.unpaired, "a 401 while opening a thread left a dead token in place")
        XCTAssertEqual(sync.state, .unpaired)
    }

    /// Cached rows outrank the initial `.unpaired` for **any** failure, not
    /// only a transport one. A 502 on a cold launch is no more evidence that
    /// the device is unpaired than a dead Wi-Fi is.
    func testAnyServerFailureWithCachedMailStillShowsTheCache() async throws {
        source.mailboxPages = [.success(try boxes())]
        _ = await sync().refresh()

        // A fresh MailSync, as after a relaunch: state starts at `.unpaired`.
        source.meResult = .failure(APIFailure.server(code: .upstreamUnavailable, status: 502,
                                                     message: "Gmail is unavailable.", retryAfter: nil))
        let relaunched = sync()
        let state = await relaunched.refresh()
        XCTAssertEqual(state, .ready, "a 502 on a cold launch hid a full mailbox list behind 'Not paired yet.'")
        XCTAssertEqual(relaunched.connectionProblem?.message, "Gmail is unavailable.")
    }

    /// EDGE (P8). `Retry-After` was parsed and then ignored, which is not
    /// honouring it.
    func testRetryAfterIsHonouredAndNotJustParsed() async throws {
        source.meResult = .failure(APIFailure.server(code: .rateLimited, status: 429,
                                                     message: "Too many attempts.", retryAfter: 60))
        let sync = sync()
        _ = await sync.refresh()
        let attempts = source.mailboxCalls

        // The server said wait a minute. The next call must not go out.
        _ = await sync.refresh()
        _ = await sync.loadThreads(mailboxID: "INBOX", resetting: true)
        XCTAssertEqual(source.mailboxCalls, attempts, "the app kept asking a server that told it to wait")
    }

    /// One global error slot let a successful mailbox refresh erase a thread's
    /// failure while the thread stayed blank, and put a paging error on screens
    /// that were working.
    func testAFailureIsRecordedAgainstTheRequestThatProducedIt() async throws {
        source.mailboxPages = [.success(try boxes())]
        let sync = sync()
        _ = await sync.refresh()

        source.threadDetail = .failure(APIFailure.unreachable(.timedOut))
        await sync.loadThread(id: "18f2a1b3c4d5e6f7", in: "INBOX")
        XCTAssertNotNil(sync.threadProblem, "the thread's failure was not recorded")
        XCTAssertNil(sync.connectionProblem, "a thread's failure appeared on the mail list")

        // A later successful page must not clear it — the thread is still blank.
        source.threadPage = .success(try ContractFixture.decode(WireThreadPage.self, from: "threads"))
        _ = await sync.loadThreads(mailboxID: "INBOX", resetting: true)
        XCTAssertNotNil(sync.threadProblem, "an unrelated success erased the thread's failure")
    }

    /// D58, one layer down: Foundation reports a cancelled request as
    /// `URLError.cancelled`, which used to arrive as `.unreachable` and put
    /// "Couldn't reach the server." on a screen that had simply gone away.
    func testACancelledRequestIsNotAConnectionError() async throws {
        source.meResult = .failure(APIFailure.cancelled)
        let sync = sync()
        _ = await sync.refresh()
        XCTAssertNil(sync.connectionProblem, "a cancelled request was reported as a failure")
    }

    /// Changing the server throws the old one's mail away. The Keychain item is
    /// origin-bound so the token could never cross — but `INBOX` and `SENT` are
    /// the same ids everywhere, so A's threads would sit under B's mailboxes.
    func testPointingAtANewServerClearsTheOldServersMail() async throws {
        // `ServerSetting` is backed by the shared `UserDefaults`, so a run of
        // this test leaves the origin where it put it. Pin a known starting
        // point rather than inheriting whatever the last run stored — the first
        // version of this test passed for a whole run and then silently stopped
        // changing anything.
        let previous = ServerSetting.origin()
        ServerSetting.setOrigin(ServerSetting.defaultOrigin)
        defer { ServerSetting.setOrigin(previous) }
        XCTAssertEqual(KeychainCredentialStore.canonical(ServerSetting.origin()),
                       KeychainCredentialStore.canonical(ServerSetting.defaultOrigin))

        source.mailboxPages = [.success(try boxes())]
        let sync = sync()
        _ = await sync.refresh()
        let before = try await Self.read(store) { try $1.mailboxes($0) }
        XCTAssertEqual(before.count, 14)

        // The new server cannot answer, so nothing can quietly repopulate the
        // store and make this pass for the wrong reason.
        source.meResult = .failure(APIFailure.unreachable(.cannotConnectToHost))
        source.mailboxPages = [.failure(APIFailure.unreachable(.cannotConnectToHost))]

        try await sync.pair(origin: URL(string: "http://192.168.1.40:8080")!,
                            code: "203120", deviceName: "test")

        let after = try await Self.read(store) { try $1.mailboxes($0) }
        XCTAssertEqual(after, [], "server A's mail survived a move to server B")
        let account = try await Self.read(store) { try $1.account($0) }
        XCTAssertNil(account, "server A's account survived a move to server B")
    }

    // MARK: - Single-flight refresh

    /// Three launch triggers — the root `.task`, the scenePhase observer, 1g's
    /// own `.task` — all call `refresh()` at once, and each used to run its own
    /// `GET /me` + `GET /mailboxes` chain. One pass must go out, every caller
    /// must get that pass's answer — and a call arriving **after** completion
    /// must still run fresh, because that is how a Gmail consent finished in
    /// Safari gets noticed (D73). Single-flight, not debounce-and-drop.
    func testConcurrentRefreshesShareOnePassAndALaterOneRunsFresh() async throws {
        source.mailboxPages = [.success(try boxes())]
        source.holdMe = true
        let sync = sync()

        let first = Task { await sync.refresh() }
        try await waitFor { self.source.meCalls == 1 }

        // Arrives while the first pass is verifiably parked inside `me()`.
        let second = Task { await sync.refresh() }
        // One yield hands the main actor to `second`, which runs to its join
        // (its first suspension) before the release below can finish the pass.
        await Task.yield()
        source.releaseMe()

        let firstState = await first.value
        let secondState = await second.value
        XCTAssertEqual(firstState, .ready)
        XCTAssertEqual(secondState, .ready, "a joining caller did not get the shared pass's answer")
        XCTAssertEqual(source.meCalls, 1, "concurrent refreshes each ran their own chain")

        // After completion is not during. The next call asks again.
        _ = await sync.refresh()
        XCTAssertEqual(source.meCalls, 2, "a refresh after completion was dropped, not run fresh")
    }

    // MARK: - A skipped detail is said, not blanked

    /// The cold-open path — P5's feed→thread jump and P6's push deep link,
    /// built early. The mailbox page fetched to satisfy the prerequisite does
    /// not list the thread, `applyThreadDetail` skips it (it refuses to invent
    /// the list half), and the screen used to render a nav bar over nothing:
    /// no rows, no caption, nothing to act on.
    func testADetailTheStoreSkipsBecomesTheThreadsProblemNotABlankScreen() async throws {
        source.mailboxPages = [.success(try boxes())]
        // The default `threadPage` is empty, so no fetch can produce a list
        // row for the id below.
        let detail = try ContractFixture.thread()
        source.threadDetail = .success(WireThread(
            id: "not-in-any-list", subject: detail.subject,
            mailboxName: detail.mailboxName, accountEmail: detail.accountEmail,
            messages: detail.messages, agentCards: detail.agentCards, partial: detail.partial
        ))
        let sync = sync()
        _ = await sync.refresh()

        await sync.loadThread(id: "not-in-any-list", in: "INBOX")
        XCTAssertEqual(sync.threadProblem?.message, "Couldn't open that conversation.")

        // EDGE: replay. The same cold open again is the same answer, not a crash.
        await sync.loadThread(id: "not-in-any-list", in: "INBOX")
        XCTAssertEqual(sync.threadProblem?.message, "Couldn't open that conversation.")

        // Recovery: once a page does list the thread, the open lands and the
        // caption clears — the same operation succeeding is what clears it (D75).
        source.threadPage = .success(try ContractFixture.page())
        source.threadDetail = .success(detail)
        await sync.loadThread(id: detail.id, in: "INBOX")
        XCTAssertNil(sync.threadProblem, "a successful open left the failure caption up")
    }

    // MARK: - A pop back is not a reset

    /// A ticking clock. `NADEClock` is a closure over state, so the box *is*
    /// the clock; the lock is because the closure is `@Sendable`.
    private final class SteppingClock: @unchecked Sendable {
        private let lock = NSLock()
        private var instant: Date
        init(_ instant: Date) { self.instant = instant }
        var nade: NADEClock { NADEClock(now: { [self] in lock.withLock { instant } }) }
        func advance(by seconds: TimeInterval) {
            lock.withLock { instant += seconds }
        }
    }

    /// 1e's `.task(id:)` re-fires on every pop back from 1f, and it used to run
    /// `loadThreads(resetting: true)` unconditionally — deleting the join rows
    /// and truncating the list to page 1 under the user's scroll. Inside
    /// `MailSync.listFreshWindow` the fetch is skipped; at the boundary and
    /// beyond, it runs. Explicit resets (`loadThreads(resetting: true)` called
    /// directly) are untouched by the gate.
    func testAPopBackInsideTheFreshWindowDoesNotResetTheList() async throws {
        source.mailboxPages = [.success(try boxes())]
        source.threadPage = .success(try ContractFixture.page())
        let clock = SteppingClock(ContractFixture.pinnedNow)
        let sync = MailSync(source: source, store: store, clock: clock.nade, sleep: { _ in })
        let model = MailListModel(sync: sync)

        await model.refresh(mailboxID: "INBOX")
        XCTAssertEqual(source.threadsCalls, 1)
        XCTAssertTrue(model.hasFetched)

        // The pop back, moments later.
        await model.refresh(mailboxID: "INBOX")
        XCTAssertEqual(source.threadsCalls, 1, "a pop back re-reset a fresh list")
        // EDGE: the skip must still count as fetched, or an empty-but-fresh
        // mailbox would show nothing instead of its earned "No mail here".
        XCTAssertTrue(model.hasFetched, "a skipped fetch left the screen claiming it never asked")

        // EDGE: the boundary. At exactly the window's edge the page is stale.
        clock.advance(by: MailSync.listFreshWindow)
        await model.refresh(mailboxID: "INBOX")
        XCTAssertEqual(source.threadsCalls, 2, "a stale list was never refreshed")
    }

    /// EDGE: clock skew. A zone change or NTP correction can put `lastPageAt`
    /// ahead of `now`; that must read *stale*, because an extra fetch costs one
    /// request while "fresh until the clock catches up" pins the list.
    func testAClockThatJumpedBackwardsReadsStaleRatherThanFreshForever() async throws {
        source.mailboxPages = [.success(try boxes())]
        source.threadPage = .success(try ContractFixture.page())
        let clock = SteppingClock(ContractFixture.pinnedNow)
        let sync = MailSync(source: source, store: store, clock: clock.nade, sleep: { _ in })
        let model = MailListModel(sync: sync)

        await model.refresh(mailboxID: "INBOX")
        clock.advance(by: -1)
        await model.refresh(mailboxID: "INBOX")
        XCTAssertEqual(source.threadsCalls, 2, "a backwards clock jump pinned the list as fresh")
    }

    // MARK: - The shared ThreadModel answers only for the observed thread

    /// The model has process lifetime, so when 1f is pushed for thread B it
    /// still holds thread A until the screen's `.task` runs `observe(id:)` —
    /// and the first frame used to render A's subject, messages and mailbox
    /// name over the thread just tapped. The gated accessors answer only for
    /// the id under observation.
    func testTheSharedThreadModelDoesNotAnswerForAThreadItIsNotObserving() async throws {
        try await ContractFixture.seedInbox(store)
        try await store.applyThreadDetail(try ContractFixture.thread(),
                                          now: ContractFixture.pinnedNow)
        let model = ThreadModel(sync: sync())

        let threadA = "18f2a1b3c4d5e6f7"
        model.observe(id: threadA)
        XCTAssertNotNil(model.thread(for: threadA))
        XCTAssertNotNil(model.row(for: threadA))

        // B is pushed. The screen renders before `.task` calls `observe`.
        let threadB = "18f28c5d6e7f8a9b"
        XCTAssertNil(model.thread(for: threadB), "the previous thread flashed under a new push")
        XCTAssertNil(model.row(for: threadB), "the previous row flashed under a new push")

        model.observe(id: threadB)
        XCTAssertNil(model.thread(for: threadB), "no detail has landed for B — nothing-yet, not A's detail")
        XCTAssertNotNil(model.row(for: threadB), "B's cached list row should answer immediately")
        XCTAssertNil(model.thread(for: threadA), "A is no longer the observed thread")

        // EDGE: re-observing the same id is a no-op that keeps the values.
        model.observe(id: threadB)
        XCTAssertNotNil(model.row(for: threadB))
    }

    // MARK: -


    /// The store is main-actor isolated here; GRDB's read closure is Sendable.
    /// Passing the store in as a parameter keeps `self` out of it.
    private nonisolated static func read<T: Sendable>(
        _ store: MailStore, _ body: @escaping @Sendable (Database, MailStore) throws -> T
    ) async throws -> T {
        try await store.writer.read { db in try body(db, store) }
    }

    private func waitFor(
        timeout: TimeInterval = 3, _ condition: @MainActor () -> Bool,
        file: StaticString = #filePath, line: UInt = #line
    ) async throws {
        let deadline = Date().addingTimeInterval(timeout)
        while Date() < deadline {
            if condition() { return }
            try await Task.sleep(for: .milliseconds(10))
        }
        XCTFail("condition never became true", file: file, line: line)
    }
}

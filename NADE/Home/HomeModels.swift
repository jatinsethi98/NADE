//
//  HomeModels.swift
//  NADE
//
//  The Ask tab's four screen models — 2a, 1a, 1b, 1c.
//
//  Same contract as `MailModels`: view state only, rows arrive through
//  `ValueObservation`, and every `start()` is cheap and idempotent because
//  `RootTabView` keeps all four tabs in the tree (D29).
//
//  The exception is `AgentBuilderModel`, which does **not** observe. 1c edits
//  one agent and needs the server's recompiled spans back in the same round
//  trip; putting that through the store would add a second copy of the truth
//  and a way for the screen and the database to disagree mid-edit.
//

import SwiftUI

// MARK: - 2a

@MainActor
@Observable
final class HomeFeedModel {

    /// The two stacked states `DESIGN.md` §2a describes, toggled by the grabber.
    enum Mode: Equatable { case feed, focus }

    private(set) var items: [WireFeedItem] = []
    private(set) var newCount = 0
    /// The same rows 1b renders. Counts are **derived**, not stored beside it:
    /// three properties that must be updated together is three chances to
    /// forget one, and `AgentsListModel` — the same observation, the same file —
    /// already derived them.
    private(set) var agents: [WireAgentRow] = []
    /// `internal(set)` so the composition can seed `-NADEScreen 2a-focus`.
    var mode: Mode = .feed
    var query = ""

    private let sync: MailSync
    private let observation = StoreObservation()

    init(sync: MailSync) { self.sync = sync }

    var connectionProblem: ConnectionProblem? { observation.problem ?? sync.feedProblem }

    var agentCount: Int { agents.count }

    /// `DESIGN.md` §2a's focus state: "{n} agents · {p} published, {d} drafts".
    var agentsSummary: String {
        let published = agents.count { $0.status == .published }
        let drafts = agents.count { $0.status == .draft }
        return String(localized: "\(published) published, \(drafts) drafts",
                      comment: "2a focus: the breakdown beside the agent count")
    }

    func start() {
        guard !observation.isRunning else { return }
        observation.keep(sync.store.observeFeed(
            onError: { [weak self] _ in self?.observation.failed(StateCopy.storeUnreadable) },
            onChange: { [weak self] in
                self?.observation.succeeded()
                self?.items = $0
            }
        ))
        observation.keep(sync.store.observeFeedNewCount(
            onError: { [weak self] _ in self?.observation.failed(StateCopy.storeUnreadable) },
            onChange: { [weak self] in self?.newCount = $0 }
        ))
        // The focus state's agents row is the only way into 1b, so it reads the
        // same cached list 1b renders rather than a second count endpoint.
        observation.keep(sync.store.observeAgents(
            onError: { [weak self] _ in self?.observation.failed(StateCopy.storeUnreadable) },
            onChange: { [weak self] in self?.agents = $0 }
        ))
    }

    func refresh() async {
        // Drain first: a queued approve that lands before the reload means the
        // feed comes back already correct, instead of showing a stale `new`
        // card that a later drain has to move a second time.
        // Drain first, by design: a queued approve that lands before the reload
        // means the feed comes back already correct.
        await sync.drainOutbox()
        // The feed and the agents share no data, and 2a's `.task` runs on every
        // arrival at the Ask tab — at launch, because Ask is the opening tab,
        // and again on every return to it since D98 (`TabView` rebuilds the
        // tab it shows). Serialising them cost an avoidable round trip on a
        // path that is now walked more than once.
        async let feed = sync.loadFeed(resetting: true)
        async let agents: Void = sync.loadAgents()
        _ = await (feed, agents)
        await refreshCursor()
    }

    /// Whether `GET /feed` has another page. Read from the stored cursor rather
    /// than guessed from the row count: the page size is the server's business.
    private(set) var canLoadMore = false
    private var isLoadingMore = false

    func loadMore() async {
        // EDGE: a sentinel can appear twice in one layout pass, and a second
        // call with the same cursor would fetch the same page again.
        guard canLoadMore, !isLoadingMore else { return }
        isLoadingMore = true
        defer { isLoadingMore = false }
        _ = await sync.loadFeed(resetting: false)
        await refreshCursor()
    }

    /// Marks one info card seen, as it scrolls into view.
    ///
    /// Only info, and only new: an approval leaves `new` by being approved or
    /// skipped, and marking one seen would clear the badge for a card that still
    /// needs a tap (`API.md` §7).
    ///
    /// **Coalesced.** A flick through the feed scrolls a dozen rows past in a
    /// moment, and `/feed/seen` takes an `ids` array precisely so that is one
    /// request rather than a dozen. Ids collect here and flush together.
    func markSeen(_ item: WireFeedItem) async {
        guard item.kind == .info, item.status == .new, !seenSent.contains(item.id) else { return }
        seenSent.insert(item.id)
        seenPending.insert(item.id)
        scheduleSeenFlush()
    }

    /// Ids already sent, so scrolling a row past twice does not re-post it.
    private var seenSent: Set<String> = []
    private var seenPending: Set<String> = []
    private var seenFlush: Task<Void, Never>?

    private func scheduleSeenFlush() {
        seenFlush?.cancel()
        seenFlush = Task { [weak self] in
            // Long enough to gather a flick, short enough that the badge does
            // not visibly lag. Cancellation is the coalescing.
            try? await Task.sleep(for: .milliseconds(400))
            guard !Task.isCancelled, let self else { return }
            await self.flushSeen()
        }
    }

    /// Sends whatever has gathered. Also called when 2a goes away, so a receipt
    /// is not lost to a screen change.
    func flushSeen() async {
        seenFlush?.cancel()
        seenFlush = nil
        let ids = seenPending
        guard !ids.isEmpty else { return }
        seenPending.removeAll()
        await sync.markSeen(ids: Array(ids))
    }

    private func refreshCursor() async {
        guard let record = try? await sync.store.feedCursor() else {
            canLoadMore = false
            return
        }
        canLoadMore = record.nextCursor != nil
    }

    /// A tap on the primary button. Goes through the outbox, so it survives the
    /// app being killed and a duplicate arriving (`API.md` §7: 409 is success).
    func approve(_ item: WireFeedItem) async {
        await sync.outbox.approve(item: item)
    }

    func skip(_ item: WireFeedItem) async {
        await sync.outbox.skip(item: item)
    }


}

// MARK: - 1b

@MainActor
@Observable
final class AgentsListModel {

    /// `DESIGN.md` §1b's three filters.
    enum Filter: Equatable, CaseIterable { case all, published, drafts }

    private(set) var agents: [WireAgentRow] = []
    var filter: Filter = .all

    private let sync: MailSync
    private let observation = StoreObservation()

    init(sync: MailSync) { self.sync = sync }

    var connectionProblem: ConnectionProblem? { observation.problem ?? sync.agentsProblem }

    var publishedCount: Int { agents.count { $0.status == .published } }
    var draftCount: Int { agents.count { $0.status == .draft } }

    /// `paused` agents appear under **All** only. 1b's filter row names three
    /// buckets and `ends.after` moves an agent to `paused` (`API.md` §5.2), so
    /// they would otherwise be unreachable from two of the three.
    var visible: [WireAgentRow] {
        switch filter {
        case .all: agents
        case .published: agents.filter { $0.status == .published }
        case .drafts: agents.filter { $0.status == .draft }
        }
    }

    func start() {
        guard !observation.isRunning else { return }
        observation.keep(sync.store.observeAgents(
            onError: { [weak self] _ in self?.observation.failed(StateCopy.storeUnreadable) },
            onChange: { [weak self] in
                self?.observation.succeeded()
                self?.agents = $0
            }
        ))
    }

    func refresh() async { await sync.loadAgents() }
}

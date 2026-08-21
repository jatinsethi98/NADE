//
//  MailModels.swift
//  NADE
//
//  One `@Observable` per screen. They hold view state and nothing else — rows
//  come from the store through `ValueObservation`, so a refresh that changes
//  nothing costs one query and no redraw.
//
//  **They start on first appearance, and again on every return.** This used to
//  read "at launch, not at tab selection": `RootTabView` held all four screens
//  in the tree at once (D29) and only changed their opacity, so `.task` ran for
//  Mail while Ask was on screen and never ran twice.
//
//  Under D98's native `TabView` a tab's content leaves the screen when you
//  leave it, so every `.task` here re-fires each time the tab comes back. The
//  models themselves are unaffected — they have process lifetime and are built
//  in the composition root, not by the views.
//
//  The requirement on this file survived the change and got stricter. Every
//  `start()` below has to be cheap and **idempotent** — each is guarded by
//  `!observation.isRunning`, so a second call adds no second observation — and
//  every `refresh()` has to be a re-fetch that replaces rather than appends.
//  `AskModel` was the one place that appended, and returning to the Ask tab
//  rendered its answer twice until `hasFinished` was added.
//

import SwiftUI

/// The four screen models, with process lifetime.
///
/// Built where the rest of the composition is, because a `@State` default is a
/// stored-property initialiser and re-runs on every construction of the view
/// struct that declares it.
@MainActor
struct MailModels {
    let mailboxes: MailboxesModel
    let list: MailListModel
    let thread: ThreadModel
    let settings: SettingsModel
    /// P3's two observing screens. 1c builds its own model per agent (it does
    /// not observe), and 1a builds one per query (it is a stream).
    let home: HomeFeedModel
    let agents: AgentsListModel

    init(sync: MailSync, openURL: @escaping @MainActor (URL) -> Void) {
        mailboxes = MailboxesModel(sync: sync)
        list = MailListModel(sync: sync)
        thread = ThreadModel(sync: sync)
        settings = SettingsModel(sync: sync, openURL: openURL)
        home = HomeFeedModel(sync: sync)
        #if DEBUG
        // `-NADEScreen 2a-focus`. Seeded at composition, not read from
        // `UserDefaults` inside the view: this was the app's only launch
        // argument read outside `LaunchOptions`, so a rename in the screenshot
        // script would have silently no-opped instead of failing.
        if LaunchOptions.startsOnFocus { home.mode = .focus }
        #endif
        agents = AgentsListModel(sync: sync)
    }
}

/// What a screen shows when its **local** store stops answering.
///
/// A `ValueObservation` that errored is over — GRDB does not resume it — so the
/// policy is: drop every token so the next `start()` can rebuild, and put a
/// caption up. It was written three times in three shapes and omitted entirely
/// from the fourth observation (`SettingsModel`), inside the same lane that
/// wrote the decision down. One place now.
@MainActor
@Observable
final class StoreObservation {
    private(set) var problem: ConnectionProblem?
    private var tokens: [MailStoreCancellable] = []

    var isRunning: Bool { !tokens.isEmpty }

    func keep(_ token: MailStoreCancellable) {
        tokens.append(token)
    }

    func succeeded() {
        problem = nil
    }

    func failed(_ copy: String) {
        tokens.removeAll()
        problem = ConnectionProblem(message: copy)
    }

    func stop() {
        tokens.removeAll()
        problem = nil
    }
}

@MainActor
@Observable
final class MailboxesModel {
    /// DESIGN.md §1g: system categories then user labels, in `GET /mailboxes`
    /// order. The store keeps `position` for exactly this.
    private(set) var mailboxes: [WireMailbox] = []
    private(set) var account: WireMe?

    private let sync: MailSync
    private let observation = StoreObservation()

    init(sync: MailSync) { self.sync = sync }

    var state: AppState { sync.state }
    var connectionProblem: ConnectionProblem? { observation.problem ?? sync.connectionProblem }

    /// The mailbox's display name, for a screen that only has its id.
    func name(of mailboxID: String) -> String {
        mailboxes.first { $0.id == mailboxID }?.name ?? ""
    }

    /// The count DESIGN.md §1g puts on the account row.
    ///
    /// `GET /me` carries no number, and summing the mailboxes would
    /// double-count: a thread is in `INBOX` *and* `CATEGORY_PERSONAL` *and* a
    /// user label, so the total would exceed the mail that exists. `INBOX` is
    /// the one mailbox that means "everything waiting for you", which is what
    /// the mockup's "All inboxes" row was counting.
    var accountUnread: Int {
        mailboxes.first { $0.id == "INBOX" }?.unread ?? 0
    }

    func start() {
        guard !observation.isRunning else { return }
        observation.keep(sync.store.observeMailboxes(
            onError: { [weak self] _ in self?.observation.failed(StateCopy.storeUnreadable) },
            onChange: { [weak self] in
                self?.observation.succeeded()
                self?.mailboxes = $0
            }
        ))
        observation.keep(sync.store.observeAccount(
            onError: { [weak self] _ in self?.observation.failed(StateCopy.storeUnreadable) },
            onChange: { [weak self] in self?.account = $0 }
        ))
    }

    func refresh() async {
        await sync.refresh()
    }
}

@MainActor
@Observable
final class MailListModel {
    private(set) var rows: [WireThreadRow] = []
    /// Keyed by mailbox. A single flag was shared across every mailbox, so a
    /// list still paging in A could swallow B's first load-more and never
    /// re-run it — B's sentinel row had already appeared.
    private var loadingMore: Set<String> = []

    private let sync: MailSync
    private let observation = StoreObservation()
    private var observedMailboxID: String?
    /// Set once the first page for this mailbox has landed. Until then the
    /// screen shows nothing at all — "No mail here" before asking would be a
    /// claim the app has not earned.
    private(set) var hasFetched = false

    init(sync: MailSync) { self.sync = sync }

    var connectionProblem: ConnectionProblem? { observation.problem ?? sync.connectionProblem }

    func observe(mailboxID: String) {
        guard observedMailboxID != mailboxID || !observation.isRunning else { return }
        observedMailboxID = mailboxID
        hasFetched = false
        observation.stop()
        observation.keep(sync.store.observeThreads(
            mailboxID: mailboxID,
            onError: { [weak self] _ in self?.observation.failed(StateCopy.storeUnreadable) },
            onChange: { [weak self] in
                self?.observation.succeeded()
                self?.rows = $0
            }
        ))
    }

    func refresh(mailboxID: String) async {
        observe(mailboxID: mailboxID)
        await sync.loadThreads(mailboxID: mailboxID, resetting: true)
        hasFetched = true
    }

    /// EDGE (P5): one request in flight **per mailbox**. Two overlapping
    /// load-mores would spend the same cursor twice and duplicate a page; one
    /// flag shared across mailboxes would drop a different list's first page.
    func loadMoreIfNeeded(mailboxID: String, after row: WireThreadRow) async {
        guard !loadingMore.contains(mailboxID), row.id == rows.last?.id else { return }
        loadingMore.insert(mailboxID)
        defer { loadingMore.remove(mailboxID) }
        await sync.loadThreads(mailboxID: mailboxID, resetting: false)
    }
}

@MainActor
@Observable
final class ThreadModel {
    private(set) var thread: WireThread?
    private(set) var row: WireThreadRow?

    private let sync: MailSync
    private let observation = StoreObservation()
    private var observedID: String?

    init(sync: MailSync) { self.sync = sync }

    var isPartial: Bool { thread?.partial ?? false }

    /// The thread's own slot, falling back to an account-level failure —
    /// never another screen's paging error. EDGE (P13): this is a *second* slot
    /// beside `isPartial`, because `API.md` §2 says `partial` is produced by an
    /// upstream failure, so the two are routinely true together.
    var connectionProblem: ConnectionProblem? { observation.problem ?? sync.threadProblem }

    // MARK: - 1f's attachment tap

    /// The file Quick Look is showing. Bound by the view; cleared by it on
    /// dismiss, which is why `downloadedFile` below exists separately.
    var previewURL: URL?
    private(set) var attachmentProblem: String?

    /// The in-flight download, so a second tap cannot start another.
    private var downloading: Task<Void, Never>?
    /// A second reference to the same file, and the reason is the bug it fixes:
    /// `.quickLookPreview` writes `nil` back to `previewURL` on dismiss, so
    /// cleaning up off that binding deleted nothing on the commonest path.
    private var downloadedFile: URL?

    /// Downloads through the authenticated proxy, then previews the file.
    ///
    /// **This lives in the model, not the view.** It is a single-flight guard, a
    /// task lifecycle, a temporary-directory lifetime and an error mapping —
    /// none of which is view state, and all of which is the edge-case class
    /// (repeat delivery, cancellation, cleanup) the tests exist to cover. As
    /// five `@State` properties in `ThreadView` none of it was reachable from a
    /// test at all.
    func openAttachment(gmailID: String, attachment: WireAttachment) {
        // EDGE: repeated taps. The ceiling is 25 MB (`API.md` §2) and each
        // request holds the whole body in memory, so N taps on a slow link is N
        // copies plus N temporary directories.
        guard downloading == nil else { return }
        downloading = Task { [weak self] in
            defer { self?.downloading = nil }
            do {
                let url = try await self?.sync.downloadAttachment(
                    gmailID: gmailID, attachmentID: attachment.id, filename: attachment.name
                )
                guard let self, let url, !Task.isCancelled else { return }
                self.discardDownloadedFile()
                self.downloadedFile = url
                self.previewURL = url
            } catch is CancellationError {
                // The screen went away. Not a failure worth a dialog.
            } catch let failure as APIFailure {
                // EDGE: over 25 MB is a 413, and a message Gmail has since
                // deleted is a 404. Both are the server's own sentence.
                if !failure.isCancellation { self?.attachmentProblem = failure.userFacingMessage }
            } catch {
                self?.attachmentProblem = String(
                    localized: "Couldn't open that attachment.",
                    comment: "Shown when an attachment cannot be downloaded"
                )
            }
        }
    }

    func dismissAttachmentProblem() { attachmentProblem = nil }

    /// Cancels anything in flight and removes the last downloaded file. Called
    /// when 1f goes away.
    func stopAttachments() {
        downloading?.cancel()
        downloading = nil
        discardDownloadedFile()
    }

    /// Each download gets its own UUID directory so two files of the same name
    /// cannot collide; without this the app leaves one behind per tap, and mail
    /// attachments are the largest thing it touches.
    private func discardDownloadedFile() {
        guard let previous = downloadedFile else { return }
        downloadedFile = nil
        previewURL = nil
        try? FileManager.default.removeItem(at: previous.deletingLastPathComponent())
    }

    /// Handed to "View original" so its scheme handler can fetch inline parts
    /// off the main actor. A value, not a closure: a fresh partial application
    /// per `body` meant `ThreadMessageBlock` could never diff equal.
    nonisolated var attachmentSource: any MailSource { sync.attachmentSource }

    func observe(id: String) {
        guard observedID != id || !observation.isRunning else { return }
        observedID = id
        observation.stop()
        observation.keep(sync.store.observeThread(
            id: id,
            onError: { [weak self] _ in self?.observation.failed(StateCopy.conversationUnreadable) },
            onChange: { [weak self] in
                self?.observation.succeeded()
                self?.thread = $0
            }
        ))
        observation.keep(sync.store.observeThreadRow(
            id: id,
            onError: { [weak self] _ in self?.observation.failed(StateCopy.conversationUnreadable) },
            onChange: { [weak self] in self?.row = $0 }
        ))
    }

    func load(id: String, in mailboxID: String) async {
        observe(id: id)
        await sync.loadThread(id: id, in: mailboxID)
    }
}

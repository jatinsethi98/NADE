//
//  NADEApp.swift
//  NADE
//
//  The composition root. Everything with a lifetime longer than a screen is
//  built here, once, **before first render** — which is what makes a
//  `simctl launch` followed immediately by `simctl io screenshot` produce the
//  same image every time.
//

import SwiftUI
import UIKit

@main
struct NADEApp: App {
    var body: some Scene {
        WindowGroup {
            RootView()
                // DESIGN.md §1 Color: the design ships one visual world, and
                // dark mode is out of scope for v1. Forced app-wide so the
                // palette always resolves as designed. EDGE (E12).
                .preferredColorScheme(.light)
        }
    }
}

private struct RootView: View {
    /// **Not** `@State private var composition = Composition.…()`.
    ///
    /// A `@State` default is a stored-property initialiser: it runs every time
    /// the view struct is constructed, and SwiftUI keeps only the first result.
    /// `WindowGroup`'s content closure re-runs on scene changes, so that form
    /// would open a second `DatabasePool` on the same file — and a third — with
    /// every one but the first thrown away holding its file descriptors.
    /// Assembling once, off a `let`, is the whole fix.
    private let composition = CompositionRoot.shared

    @Environment(\.scenePhase) private var scenePhase

    var body: some View {
        #if DEBUG
        // EDGE (E14): the gallery exists only in DEBUG. A release build has no
        // path to it at all — this whole branch is compiled out.
        //
        //   xcrun simctl launch <device> fsaas.NADE -NADEGallery 1
        //   xcrun simctl launch <device> fsaas.NADE -NADEGallery 1 -NADEGallerySection buttons
        if LaunchOptions.showsGallery {
            GalleryView(initialSection: LaunchOptions.gallerySection)
        } else {
            shell
        }
        #else
        shell
        #endif
    }

    private var shell: some View {
        RootTabView(
            sync: composition.sync,
            models: composition.models,
            clock: composition.clock
        )
        .environment(composition.navigation)
        .task { await composition.sync.refresh() }
        // Coming back from the browser after Gmail consent, and coming back at
        // all. Without this the app can sit in `needsGmail` after a successful
        // sign-in until something else happens to refresh, and a first sync
        // that outlasts the poll window never recovers.
        //
        // The reason used to be D29: all four screens stayed resident, so
        // returning from Safari re-ran no `.task` anywhere. D98's `TabView`
        // does rebuild the tab it shows, which weakens that argument without
        // retiring it — the screen you return to is whichever one you left, and
        // it may be a tab whose `.task` does not refresh the account. The scene
        // phase is still the only signal that means "the app is back".
        .onChange(of: scenePhase) { _, phase in
            guard phase == .active else { return }
            Task { await composition.sync.refresh() }
        }
    }
}

/// Assembled exactly once, for the lifetime of the process.
@MainActor
private enum CompositionRoot {
    static let shared: Composition = {
        #if DEBUG
        Composition.fromLaunchArguments()
        #else
        Composition.live()
        #endif
    }()
}

/// Everything the app needs, assembled.
@MainActor
private struct Composition {
    let sync: MailSync
    let models: MailModels
    let navigation: AppNavigation
    let clock: NADEClock

    init(sync: MailSync, navigation: AppNavigation, clock: NADEClock) {
        self.sync = sync
        self.navigation = navigation
        self.clock = clock
        // DESIGN.md §1k: open the Gmail link "with `ASWebAuthenticationSession`
        // or Safari — **never** a throwaway `WKWebView`", because `start` sets
        // an `HttpOnly` cookie that `callback` requires. The system browser
        // keeps that cookie across the redirect and across the app going to the
        // background; `ASWebAuthenticationSession` would be the choice if the
        // callback were a custom scheme, but it is an HTML page on the server's
        // own origin, so the session would never self-terminate anyway.
        //
        // Taken from `UIApplication` rather than the SwiftUI environment so the
        // models can be assembled here, once, with the rest of the composition.
        self.models = MailModels(sync: sync, openURL: { UIApplication.shared.open($0) })
    }

    /// The shipping assembly, with its three inputs named.
    ///
    /// Every DEBUG mode goes through here rather than hand-building its own
    /// source and store: a test world assembled differently from the product is
    /// a test world that stops proving things about the product the first time
    /// the composition grows (P3 adds device registration and background
    /// refresh, and would not reach a parallel root).
    static func live(
        origin: @escaping @Sendable () -> URL = { ServerSetting.origin() },
        credentials: any CredentialStore = KeychainCredentialStore(),
        location: StoreLocation = .live,
        navigation: AppNavigation? = nil,
        clock: NADEClock = .live
    ) -> Composition {
        Composition(
            sync: MailSync(source: HTTPMailSource(origin: origin, credentials: credentials),
                           store: MailStore.openingOrEmpty(location)),
            navigation: navigation ?? AppNavigation(),
            clock: clock
        )
    }

    #if DEBUG
    /// `-NADESeed fixtures|empty` swaps the **source and the database file**,
    /// not just the data. Two stores rather than one, because a live launch
    /// deliberately does not reset (it has to keep cached mail for offline), so
    /// fixture rows that no live id happened to overwrite would otherwise sit
    /// beside real mail indefinitely.
    static func fromLaunchArguments() -> Composition {
        let navigation = AppNavigation()
        let clock = NADEClock.fromLaunchArgumentsOrLive()

        guard let seed = LaunchOptions.seed else {
            // A live launch is the shipping composition, so it keeps the
            // shipping default tab — `-NADEScreen` only moves it when it was
            // actually passed. Without this the bench could deep-link into the
            // fixture world but not into the real one, which is backwards: the
            // live screens are the ones a phase gate has to look at.
            if LaunchOptions.hasExplicitScreen {
                LaunchOptions.applyInitialScreen(to: navigation)
            }
            let composition = live(navigation: navigation, clock: clock)
            LaunchOptions.pairIfAsked(composition.sync)
            return composition
        }

        LaunchOptions.applyInitialScreen(to: navigation)

        // EDGE (P11): a seeded launch starts from nothing, so a UI test
        // asserting the empty state cannot be shown the previous run's mail.
        let location: StoreLocation = seed == .offline ? .live : .fixtures
        try? location.removeFiles()

        guard seed == .offline else {
            let store = MailStore.openingOrEmpty(location)
            return Composition(
                sync: MailSync(source: FixtureMailSource(isEmpty: seed == .empty), store: store),
                navigation: navigation,
                clock: clock
            )
        }

        // A16: mail already cached, server unreachable. Assembled through
        // `live()` with two substitutions — port 1 refuses immediately, so the
        // failure is unambiguously a transport one rather than a timeout — so
        // what `OfflineUITests` proves degrades correctly is the shipping
        // composition, not a hand-built lookalike.
        let origin = URL(string: "http://127.0.0.1:1")!
        let composition = live(
            origin: { origin },
            credentials: InMemoryCredentialStore(Credential(baseURL: origin, token: "nade_offline")),
            location: location,
            navigation: navigation,
            clock: clock
        )

        // Seed, **then** refresh. The screens run their own `.task` on appear,
        // and every one of those can win a race against an async seed — leaving
        // a paired device looking at "Not paired yet." over a mailbox list that
        // arrived a moment later. One refresh after the rows land settles it.
        let sync = composition.sync
        let now = clock.now()
        Task { @MainActor in
            try? await FixtureSeed.seed(into: sync.store, now: now)
            await sync.refresh()
        }
        return composition
    }
    #endif

}

/// Where the app points, before anything is paired.
///
/// `NADE_BASE_URL` is read from the process environment rather than added to
/// the shared scheme: that file is committed, and the scheme's launch settings
/// are inherited by the test action, so a URL there would also point every unit
/// test at a live server.
nonisolated enum ServerSetting {
    static let defaultOrigin = URL(string: "http://localhost:8080")!
    private static let key = "NADEServerURL"

    /// Read once. `ProcessInfo.environment` materialises the whole environment
    /// as a dictionary on every access, and this key cannot change after launch.
    private static let environmentOverride: URL? = {
        ProcessInfo.processInfo.environment["NADE_BASE_URL"].flatMap(URL.init(string:))
    }()

    /// The environment wins, then whatever Settings last stored, then the
    /// default. The environment is a developer's override and should not be
    /// quietly overwritten by a tap in the app. The `UserDefaults` half stays
    /// live, because Settings writes it.
    static func origin() -> URL {
        if let environmentOverride { return environmentOverride }
        if let raw = UserDefaults.standard.string(forKey: key), let url = URL(string: raw) {
            return url
        }
        return defaultOrigin
    }

    static var isOverriddenByEnvironment: Bool { environmentOverride != nil }

    /// - Returns: true when the origin actually changed, which is the caller's
    ///   cue to throw away everything the old server told us.
    @discardableResult
    static func setOrigin(_ url: URL) -> Bool {
        let changed = KeychainCredentialStore.canonical(url) != KeychainCredentialStore.canonical(origin())
        UserDefaults.standard.set(url.absoluteString, forKey: key)
        return changed
    }
}

#if DEBUG
enum LaunchOptions {
    /// `-NADEGallery 1`. UserDefaults picks up `-key value` launch-argument
    /// pairs, so `-NADEGallery 0` correctly means *off* — which a bare
    /// `arguments.contains` check would get wrong.
    static var showsGallery: Bool {
        UserDefaults.standard.bool(forKey: "NADEGallery")
    }

    /// `-NADEGallerySection <id>` scrolls straight to a section, so each
    /// screenshot is deterministic instead of depending on a swipe.
    static var gallerySection: String? {
        UserDefaults.standard.string(forKey: "NADEGallerySection")
    }

    enum Seed: String {
        case fixtures
        case empty
        /// A16: mail already cached, server unreachable. The live store is
        /// seeded directly and the HTTP source points at a closed port, which
        /// is the only way to get a *transport* failure over real rows.
        case offline
    }

    /// `-NADESeed fixtures|empty`. Absent means the live server.
    static var seed: Seed? {
        UserDefaults.standard.string(forKey: "NADESeed").flatMap(Seed.init(rawValue:))
    }

    /// `-NADEPairCode <six digits>` pairs a live launch against the server the
    /// app already points at, so a reinstall does not need a human to retype a
    /// code that is only good for ten minutes anyway.
    ///
    /// DEBUG-only, and deliberately not a substitute for the real screen: it
    /// drives the very same `MailSync.pair` the Settings sheet drives, so what
    /// the bench exercises is the shipping path.
    static func pairIfAsked(_ sync: MailSync) {
        guard let code = UserDefaults.standard.string(forKey: "NADEPairCode"),
              !code.isEmpty else { return }
        Task { @MainActor in
            // EDGE: the code is single-use. Relaunching with the same argument
            // after a successful pair must not spend a second one, and must not
            // report a failure for a device that is already paired.
            guard (try? sync.source.isPaired()) != true else { return }
            do {
                try await sync.pair(origin: ServerSetting.origin(),
                                    code: code,
                                    deviceName: "bench")
            } catch {
                // EDGE: wrong, expired or already-spent codes are one 401 by
                // design. Say so on the console rather than failing silently -
                // the screen's own "Not paired" is the other half of the signal.
                print("[NADEPairCode] pairing failed: \(error)")
            }
        }
    }

    /// `-NADEScreen 2a-focus` names 2a's **focus** state.
    ///
    /// Read here with the rest of the launch vocabulary, but applied to the
    /// model rather than to `AppNavigation`: which of 2a's two stacked states is
    /// showing is one screen's private business, and app-global navigation is
    /// the wrong place to keep it.
    static var startsOnFocus: Bool {
        UserDefaults.standard.string(forKey: "NADEScreen") == "2a-focus"
    }

    /// Whether `-NADEScreen` was passed at all.
    ///
    /// A seeded launch applies the screen unconditionally (its default, 1g, is
    /// what every `-NADESeed` UI test that names no screen already expects). A
    /// live launch must not: defaulting there would silently move the shipping
    /// first screen off the Ask tab.
    static var hasExplicitScreen: Bool {
        UserDefaults.standard.string(forKey: "NADEScreen") != nil
    }

    /// `-NADEScreen 1e|1f|1g|1k`, using DESIGN.md's own anchors — the same
    /// vocabulary `docs/screens/` and the deviation register already use.
    /// `-NADEMailbox <label id>` and `-NADEThread <thread id>` say which.
    static func applyInitialScreen(to navigation: AppNavigation) {
        let mailbox = UserDefaults.standard.string(forKey: "NADEMailbox") ?? "INBOX"
        let screen = UserDefaults.standard.string(forKey: "NADEScreen")

        // The Ask tab's screens (P3). Handled before the Mail block because
        // that one lights the Mail tab unconditionally, which is right for
        // every anchor it knows and wrong for all of these.
        switch screen {
        case "2a", "2a-focus", "1a", "1b", "1c":
            navigation.selection = .ask
            navigation.selectedMailboxID = mailbox
            switch screen {
            case "1a":
                // `-NADEQuery` picks the route the fixture stream answers with,
                // so all three of 1a's states are reachable from a launch.
                let query = UserDefaults.standard.string(forKey: "NADEQuery")
                    ?? "What did I miss from Stripe this week?"
                navigation.homePath = [.query(query, routeHint: nil)]
            case "1b":
                navigation.homePath = [.agents]
            case "1c":
                navigation.editingAgentID = UserDefaults.standard.string(forKey: "NADEAgent")
                    ?? FixtureSeed.defaultAgentID
            default:
                navigation.homePath = []
            }
            return
        default:
            break
        }

        navigation.selection = .mail
        navigation.selectedMailboxID = mailbox

        switch screen {
        case "1e":
            navigation.mailPath = [.threads(mailboxID: mailbox)]
        case "1f":
            let thread = UserDefaults.standard.string(forKey: "NADEThread")
                ?? FixtureSeed.defaultThreadID
            navigation.mailPath = [.threads(mailboxID: mailbox),
                                   .thread(id: thread, mailboxID: mailbox)]
        case "1k":
            navigation.mailPath = [.settings]
        default:
            navigation.mailPath = []   // 1g, the root
        }
    }
}
#endif

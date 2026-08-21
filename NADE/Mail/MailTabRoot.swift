//
//  MailTabRoot.swift
//  NADE
//
//  The Mail tab: 1g at the root, 1e and 1f pushed from it, 1k pushed from 1g.
//
//  Every screen draws its own nav bar (DESIGN.md gives each one exact numbers),
//  so the system bar is hidden throughout. The stack's path lives in
//  `AppNavigation` rather than in an `@State` here, because `RootTabView` has
//  to read it to decide whether the tab bar is on screen — and because that is
//  what makes the path survive a tab switch, which is the property D29 exists
//  to protect.
//

import SwiftUI

struct MailTabRoot: View {

    @Environment(AppNavigation.self) private var navigation

    /// Handed in, not built here.
    ///
    /// `@State private var x = Model(...)` looks like "built once" and is not:
    /// the initialiser runs on every construction of the view struct and
    /// SwiftUI keeps the first result. `screen(for:)` runs inside
    /// `ForEach(NTab.allCases)`, so `MailTabRoot.init` re-ran on every tab
    /// switch and every push — four discarded models each time, one of which
    /// read the Keychain and the process environment on the way. These have
    /// process lifetime, so they live where the rest of it does.
    let models: MailModels
    let clock: NADEClock

    var body: some View {
        @Bindable var navigation = navigation

        NavigationStack(path: $navigation.mailPath) {
            MailboxesView(model: models.mailboxes)
                .toolbar(.hidden, for: .navigationBar)
                .navigationDestination(for: MailRoute.self) { route in
                    destination(route)
                        .toolbar(.hidden, for: .navigationBar)
                        // UIKit turns the edge swipe off with the bar. Every
                        // screen here draws its own back affordance, so the
                        // gesture is restored explicitly.
                        .nadeInteractivePopGesture()
                }
        }
    }

    /// Quantised to the minute — `NADEClock.nowToTheMinute()`, shared with
    /// `HomeTabRoot` and for its reason: this body re-runs on every tab tap,
    /// and a raw `clock.now()` re-rendered every visible `MailRow` and re-split
    /// every `ThreadMessageBlock` for a change no row can show.
    private var now: Date { clock.nowToTheMinute() }

    @ViewBuilder
    private func destination(_ route: MailRoute) -> some View {
        switch route {
        case .threads(let mailboxID):
            MailListView(
                model: models.list,
                mailboxes: models.mailboxes.mailboxes,
                accountEmail: models.mailboxes.account?.email ?? "",
                mailboxID: mailboxID,
                now: now
            )
        case .thread(let id, let mailboxID):
            ThreadView(model: models.thread, threadID: id, mailboxID: mailboxID,
                       backTitle: models.mailboxes.name(of: mailboxID),
                       now: now)
        case .settings:
            SettingsView(model: models.settings)
        }
    }
}

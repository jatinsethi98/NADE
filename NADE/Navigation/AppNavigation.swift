//
//  AppNavigation.swift
//  NADE
//
//  Where the app is, hoisted above the shell.
//
//  `RootTabView` keeps all four screens in the tree (D29), so navigation state
//  cannot live inside the Mail tab: the tab bar has to read it, and a
//  `PreferenceKey` would be emitted by the three inactive screens as well as
//  the visible one — a pushed note detail in Notes would hide the bar on Mail.
//  Hoisting also gives `-NADEScreen` somewhere to write for a deterministic
//  screenshot, and gives P6's push deep link a place to land.
//

import SwiftUI

nonisolated enum MailRoute: Hashable, Sendable {
    case threads(mailboxID: String)
    /// **The mailbox is part of the route.**
    ///
    /// It used to be read from `selectedMailboxID`, app-global mutable state,
    /// which cost three things: two threads opened from different mailboxes
    /// hashed identically so `NavigationStack` could not tell them apart; the
    /// parent had to look the mailbox's *name* up to hand the screen a back
    /// title; and naming a destination took two writes instead of one, which is
    /// the tell. P5's feed→thread jump and P6's push deep link both construct a
    /// route from outside the view tree, where there is no "selected" anything.
    case thread(id: String, mailboxID: String)
    case settings

    /// `docs/DESIGN.md` §2's navigation map, verbatim: 1e and 1g carry the bar
    /// with Mail lit, 1k carries it with **no** tab lit — which v1 renders as
    /// Mail lit, since Settings is pushed from the Mail tab — and 1f, a pushed
    /// detail, carries none.
    ///
    /// Note what this is *not*: a function of stack depth. Three of the four
    /// routes here are pushes that keep the bar, so "deeper than the root
    /// means hide it" is wrong on the very first push.
    var hidesTabBar: Bool {
        switch self {
        case .thread: true
        case .threads, .settings: false
        }
    }
}

@MainActor
@Observable
final class AppNavigation {
    /// Ask, as P1 shipped it — `docs/DESIGN.md` §3 makes 2a the app's opening
    /// screen. `-NADEScreen` moves it for a test or a screenshot; nothing in
    /// P2 does.
    var selection: NTab = .ask
    var mailPath: [MailRoute] = []

    /// The mailbox 1e is showing. Owned here rather than by the screen because
    /// 1g sets it on the way in, and because a screenshot launch argument has
    /// to be able to.
    var selectedMailboxID: String = "INBOX"

    /// Visibility follows the **active tab's** top route. Switching on
    /// `selection` first is what stops a thread pushed on Mail from hiding the
    /// bar while Notes is on screen — all four stacks stay alive underneath.
    var showsTabBar: Bool {
        switch selection {
        case .mail: !(mailPath.last?.hidesTabBar ?? false)
        default: true
        }
    }

    func openMailbox(_ id: String) {
        selectedMailboxID = id
        mailPath = [.threads(mailboxID: id)]
    }

    func openThread(_ id: String, in mailboxID: String) {
        mailPath.append(.thread(id: id, mailboxID: mailboxID))
    }

    func openSettings() {
        mailPath.append(.settings)
    }
}

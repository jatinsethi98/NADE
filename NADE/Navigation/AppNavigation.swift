//
//  AppNavigation.swift
//  NADE
//
//  Where the app is, hoisted above the shell.
//
//  The shell keeps all four screens in the tree (D29), so navigation state
//  cannot live inside the Mail tab. Hoisting gives `-NADEScreen` somewhere to
//  write for a deterministic screenshot, and gives P6's push deep link a place
//  to land.
//
//  It used to also carry `showsTabBar`, because the hand-built `NTabBar` was a
//  sibling of the screens and the shell had to decide whether to draw it. The
//  Liquid Glass pass moved to a native `TabView` (D98), where a screen hides
//  the bar for itself with `.toolbar(.hidden, for: .tabBar)`. The rule did not
//  change and did not move: `MailRoute.hidesTabBar` below is still the only
//  place that decides, and `MailTabRoot` is now the only place that reads it.
//  Deriving it here as well would have been a second answer to one question,
//  and the shell no longer has a use for the first.
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

/// The Ask tab's stack. Named `Home` and not `Ask` because `AskRoute` is
/// already the SSE `route` event's three kinds (`WireAsk.swift`) - two
/// different things called the same word in the same module is exactly the
/// confusion `MailRoute` avoided by not being called `Route`.
nonisolated enum HomeRoute: Hashable, Sendable {
    /// 1a, pushed when the ask field is submitted. The query travels in the
    /// route for the same reason a thread's mailbox does: the screen is
    /// constructed from outside the view tree by "Make this an agent", which
    /// re-asks the same words with a forced route.
    case query(String, routeHint: AskRoute?)
    /// 1b, reached from 2a focus's "All →".
    case agents

    /// `DESIGN.md` §2's map: 1a and 1b both keep the bar with **Ask** lit.
    /// 1c and 1d are absent here on purpose - they are modals, not pushes, so
    /// they cover the bar rather than hiding it.
    var hidesTabBar: Bool { false }
}

@MainActor
@Observable
final class AppNavigation {
    /// Ask, as P1 shipped it — `docs/DESIGN.md` §3 makes 2a the app's opening
    /// screen. `-NADEScreen` moves it for a test or a screenshot; nothing in
    /// P2 does.
    var selection: NTab = .ask
    var mailPath: [MailRoute] = []
    var homePath: [HomeRoute] = []

    /// 1c, the agent builder. A modal rather than a push (`DESIGN.md` §2), so
    /// it is an identity and not a path entry - and `nil` is "closed".
    var editingAgentID: String?

    /// The mailbox 1e is showing. Owned here rather than by the screen because
    /// 1g sets it on the way in, and because a screenshot launch argument has
    /// to be able to.
    var selectedMailboxID: String = "INBOX"

    /// 1a. A fresh submit replaces whatever query was on screen rather than
    /// stacking, so "Make this an agent" - which re-asks the same words with a
    /// forced route - does not leave a back button to a dead intermediate state.
    func ask(_ query: String, routeHint: AskRoute? = nil) {
        let trimmed = query.trimmingCharacters(in: .whitespacesAndNewlines)
        // `nadeIsBlank` covers empty too — a string with no scalars has no
        // visible one — so this is the whole guard, not half of it.
        guard !trimmed.nadeIsBlank else { return }
        homePath = [.query(trimmed, routeHint: routeHint)]
    }

    func openAgents() {
        // EDGE: reachable from 2a focus and from 1a's saved-draft line. Pushing
        // twice would put two identical screens on the stack.
        if homePath.last != .agents { homePath.append(.agents) }
    }

    func openAgent(_ id: String) {
        editingAgentID = id
    }

    /// Pop one screen off a stack, safely.
    ///
    /// `Array.removeLast()` on an empty array is a **fatal error**, and a back
    /// affordance is exactly the control a user double-taps. This started as a
    /// guard for 1b alone; the other six back affordances in the app — 1e, 1f,
    /// 1k and 1a's three dismissals — were all calling `removeLast()` or
    /// `removeAll()` on the array directly, so the guard covered one of seven.
    /// Popping is now something `AppNavigation` does, not something each screen
    /// re-implements.
    func popHome() { pop(&homePath) }
    func popMail() { pop(&mailPath) }

    /// 1a's "Clear", "Start over" and back all mean the same thing: leave the
    /// query behind and return to 2a. One name, so they cannot drift — and so
    /// the day 1a can be pushed over 1b, `removeAll()` does not silently pop two
    /// screens.
    func clearAsk() {
        homePath.removeAll { if case .query = $0 { return true } else { return false } }
    }

    private func pop<Route>(_ path: inout [Route]) {
        guard !path.isEmpty else { return }
        path.removeLast()
    }

    /// 1b's "New": back to 2a with the focus state's ask field waiting.
    /// `wantsFocus` is consumed by 2a on appear, so it cannot latch.
    func newAgent() {
        homePath = []
        selection = .ask
        wantsAskFocus = true
    }

    /// Set by `newAgent()`, read once by 2a. A request, not a mode.
    var wantsAskFocus = false

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

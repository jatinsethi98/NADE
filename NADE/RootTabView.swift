//
//  RootTabView.swift
//  NADE
//
//  The app shell: four tabs over `NTabBar`, with placeholder screens.
//
//  P2 replaced `.mail` with `MailTabRoot`; P3/P7 replace the other three
//  (AskView, NotesListView, CalendarView).
//
//  Two things did have to change here at P2, and the P1 note that said nothing
//  would was optimistic on both counts:
//
//  * The **selection moved out**, into `AppNavigation`. The tab bar has to know
//    the Mail stack's top route to decide whether to draw itself at all, and a
//    screenshot launch argument has to be able to write both.
//  * The bar is now **conditional**. DESIGN.md §2's map gives 1f — a pushed
//    detail — no tab bar, while 1e, 1g and 1k all keep it.
//
//  All four screens are **always in the view tree**; the inactive ones are
//  hidden, not discarded. A `switch` on the selection destroys the outgoing
//  screen, and with it every `@State` it owns, its scroll position, its
//  `NavigationStack` path and any `.task` it has running — which is exactly
//  what a mail list, a note draft and an SSE stream cannot survive. Getting
//  that wrong is invisible with placeholder screens and very visible in P2, so
//  it is fixed here rather than there. `ShellStateUITests` is what keeps
//  it fixed.
//

import SwiftUI

struct RootTabView: View {
    @Environment(AppNavigation.self) private var navigation

    let sync: MailSync
    let models: MailModels
    let clock: NADEClock

    var body: some View {
        @Bindable var navigation = navigation

        return ZStack {
            // The ground runs edge to edge, under the status bar and the home
            // indicator alike (DESIGN.md §1 Color: `bg` is every screen ground).
            Theme.Color.bg.ignoresSafeArea()

            // A plain VStack, not `safeAreaInset`. Two reasons, both from the
            // design rather than from taste:
            //
            // 1. DESIGN.md §Safe area — the tab bar's 26 pt is measured from
            //    the display edge. `safeAreaInset` places the bar *above* the
            //    34 pt indicator region, so the bar's own padding stacks on top
            //    of it and the labels land at 42.
            // 2. In the mockup the tab bar is a sibling of the scrolling band,
            //    not an overlay on it — content ends at the hairline, it never
            //    scrolls underneath. `safeAreaInset` propagates an inset that
            //    invites exactly that scroll-under behaviour in P2's lists.
            VStack(spacing: 0) {
                ZStack {
                    ForEach(NTab.allCases) { tab in
                        screen(for: tab)
                            // The inactive three are invisible, untappable and
                            // hidden from VoiceOver — but still constructed, so
                            // their state survives.
                            .accessibilityHidden(tab != navigation.selection)
                            .frame(maxWidth: .infinity, maxHeight: .infinity)
                            .opacity(tab == navigation.selection ? 1 : 0)
                            .zIndex(tab == navigation.selection ? 1 : 0)
                            .allowsHitTesting(tab == navigation.selection)
                    }
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
                // P2: 1f is a pushed detail and DESIGN.md §2's navigation map
                // gives it no tab bar. The condition is a property of the
                // **active tab's top route** (see `AppNavigation`), not of how
                // deep the stack is — 1e, 1g and 1k are all pushes that keep
                // the bar, so a depth rule would be wrong on the first one.
                if navigation.showsTabBar {
                    NTabBar(selection: $navigation.selection)
                }
            }
            .ignoresSafeArea(.container, edges: .bottom)
        }
        .foregroundStyle(Theme.Color.ink)
    }

    @ViewBuilder
    private func screen(for tab: NTab) -> some View {
        switch tab {
        case .ask: PlaceholderScreen(tab: .ask, note: "Ask, search, or describe an agent.")
        case .mail: MailTabRoot(models: models, clock: clock)
        case .notes: PlaceholderScreen(tab: .notes, note: "Notes your agents write.")
        case .calendar: PlaceholderScreen(tab: .calendar, note: "Six days, each a compressed timeline.")
        }
    }
}

/// A P1 stand-in. Deliberately plain: the point is that the shell works, not
/// that the tab has content.
///
/// It does carry one piece of `@State` — a tap counter — for exactly one
/// reason: it is the only way to prove from the outside that switching tabs
/// does not rebuild the screen. `ShellStateUITests` taps it, leaves, comes
/// back and expects the count to still be there.
struct PlaceholderScreen: View {
    let tab: NTab
    let note: String

    @State private var taps = 0

    var body: some View {
        VStack(spacing: Theme.Space.s2) {
            Spacer(minLength: 0)
            Text(tab.title)
                .font(Theme.Font.heading(23))
                .foregroundStyle(Theme.Color.ink)
                // Identifiers go on the leaves. An identifier on the container
                // overrides every descendant's, so `screen.ask` on the VStack
                // would rename the tap counter to `screen.ask` too.
                .accessibilityIdentifier("screen.\(tab.rawValue).title")
            Text(note)
                .font(Theme.Font.body(13))
                .foregroundStyle(Theme.Color.ink55)
                .multilineTextAlignment(.center)
                .fixedSize(horizontal: false, vertical: true)   // EDGE (E3)
                .accessibilityIdentifier("screen.\(tab.rawValue).note")

            NButton("Taps: \(taps)", variant: .secondary) { taps += 1 }
                .accessibilityIdentifier("screen.\(tab.rawValue).taps")

            Spacer(minLength: 0)
        }
        .padding(.horizontal, Theme.Space.screen)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
}

#if DEBUG
#Preview {
    // `FixtureMailSource(isEmpty:)` already answers "nothing, and not paired",
    // which is what a preview wants — a fourth hand-written `MailSource` for
    // one preview would have to be stubbed again for every method the protocol
    // grows, and it sat in the Release source that
    // `scripts/assert-release-has-no-fixtures.sh` exists to keep clean.
    let store = try! MailStore.inMemory()
    let sync = MailSync(source: FixtureMailSource(isEmpty: true, paired: false), store: store)
    return RootTabView(sync: sync, models: MailModels(sync: sync, openURL: { _ in }), clock: .live)
        .environment(AppNavigation())
        .preferredColorScheme(.light)
}
#endif


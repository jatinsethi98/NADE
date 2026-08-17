//
//  RootTabView.swift
//  NADE
//
//  The app shell: four tabs over `NTabBar`, with placeholder screens.
//
//  P2/P3/P7 replace `screen(for:)`'s cases with the real views (AskView,
//  MailListView, NotesListView, CalendarView). Nothing else here should need to
//  change — the shell owns the ground colour, the safe-area inset and the
//  selection, and the screens own everything above the bar.
//

import SwiftUI

struct RootTabView: View {
    @State private var selection: NTab = .ask

    var body: some View {
        ZStack {
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
                screen(for: selection)
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                NTabBar(selection: $selection)
            }
            .ignoresSafeArea(.container, edges: .bottom)
        }
        .foregroundStyle(Theme.Color.ink)
    }

    @ViewBuilder
    private func screen(for tab: NTab) -> some View {
        switch tab {
        case .ask: PlaceholderScreen(tab: .ask, note: "Ask, search, or describe an agent.")
        case .mail: PlaceholderScreen(tab: .mail, note: "Your mail, filtered by label.")
        case .notes: PlaceholderScreen(tab: .notes, note: "Notes your agents write.")
        case .calendar: PlaceholderScreen(tab: .calendar, note: "Six days, each a compressed timeline.")
        }
    }
}

/// A P1 stand-in. Deliberately plain: the point is that the shell works, not
/// that the tab has content.
struct PlaceholderScreen: View {
    let tab: NTab
    let note: String

    var body: some View {
        VStack(spacing: Theme.Space.s2) {
            Spacer(minLength: 0)
            Text(tab.title)
                .font(Theme.Font.heading(23))
                .foregroundStyle(Theme.Color.ink)
            Text(note)
                .font(Theme.Font.body(13))
                .foregroundStyle(Theme.Color.ink55)
                .multilineTextAlignment(.center)
                .fixedSize(horizontal: false, vertical: true)   // EDGE (E3)
            Spacer(minLength: 0)
        }
        .padding(.horizontal, Theme.Space.screen)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .accessibilityIdentifier("screen.\(tab.rawValue)")
    }
}

#Preview {
    RootTabView().preferredColorScheme(.light)
}

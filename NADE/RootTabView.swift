//
//  RootTabView.swift
//  NADE
//
//  The app shell: four tabs in a native `TabView`, with placeholder screens.
//
//  P2 replaced `.mail` with `MailTabRoot`; P3/P7 replace the other three
//  (AskView, NotesListView, CalendarView).
//
//  **This reverses D22 and D26.** Both were right for the design they were
//  written against, and the Liquid Glass ruling replaces that design's chrome
//  layer:
//
//  * D22 kept `TabView` out because it cannot be given the design's 1 pt top
//    hairline, its 18 pt light-stroke glyphs or its 10.5 pt uppercase 0.09 em
//    labels. Still true — that type is the price of the system bar, and it is
//    paid deliberately (DESIGN.md §6, IOS_DECISIONS D98).
//  * D26 kept the bar a **sibling** of the scrolling band, so content ended at
//    the hairline and never scrolled underneath. Liquid Glass is a floating
//    layer that lenses what passes beneath it; a bar with nothing under it is
//    a bar with nothing to refract. The sibling `VStack` is what had to go.
//
//  What survives the swap, and had to:
//
//  * **All four screens stay alive.** The old shell held them in a `ZStack` of
//    four `opacity`-toggled children because a `switch` would destroy the
//    outgoing screen's `@State`, scroll position and `NavigationStack` path.
//    `TabView` keeps each `Tab`'s content identity, so the property is
//    preserved rather than reimplemented — but *preserved* is a claim, and
//    `ShellStateUITests` is what checks it.
//
//    What the old shell also preserved, and this does not, is the **lifecycle**:
//    an off-screen tab used to keep running its `.task` and never saw
//    `.onDisappear`. Now both fire on every switch. Every model's `start()` was
//    already idempotent — except `AskModel`, which appended (D100).
//  * **Tab-bar visibility on 1f.** It used to be computed in the shell from
//    `AppNavigation.showsTabBar`. A system bar is hidden by the screen that
//    wants it hidden, so the rule moved to the destination — see
//    `MailTabRoot` — and `MailRoute.hidesTabBar` is still the only place that
//    decides. It is a property of the top route, never of stack depth.
//  * **The ground.** Four screens (1g, 1e, 1f, 1k) never set a background;
//    they inherited one from the old shell's `ZStack`. `TabView` gives its
//    content no such ground, so it is applied per tab below — and it ignores
//    the safe area, because the whole point is that `bg` runs under the glass
//    bar rather than stopping at it.
//

import SwiftUI

struct RootTabView: View {
    @Environment(AppNavigation.self) private var navigation

    let sync: MailSync
    let models: MailModels
    let clock: NADEClock

    var body: some View {
        @Bindable var navigation = navigation

        return TabView(selection: $navigation.selection) {
            ForEach(NTab.allCases) { tab in
                // No `.accessibilityIdentifier` here, and not for want of
                // trying: UIKit's tab bar does not carry one. It was applied to
                // the `Tab` and then to an explicit `Label`, and on iOS 26.5
                // both compiled and both arrived at the rendered button as the
                // empty string. The UI tests address the four tabs by the label
                // VoiceOver speaks instead — see `XCUIApplication.tab(_:)`.
                Tab(tab.title, systemImage: tab.symbol, value: tab) {
                    screen(for: tab)
                }
            }
        }
        // The bar shrinks to a pill on scroll-down and returns on scroll-up.
        // This is the behaviour that pays for losing the design's labels: the
        // bar gets out of the way of the content instead of reserving 75 pt of
        // it forever.
        .tabBarMinimizeBehavior(.onScrollDown)
        // `AccentColor` is already `#b68235` (D8) so the system would resolve
        // this correctly on its own. Stated anyway: the selected-tab tint is
        // now drawn by UIKit rather than by us, and an asset catalogue entry
        // is a long way from the call site that depends on it.
        .tint(Theme.Color.accent)
        .foregroundStyle(Theme.Color.ink)
    }

    @ViewBuilder
    private func screen(for tab: NTab) -> some View {
        Group {
            switch tab {
            case .ask: HomeTabRoot(sync: sync, models: models, clock: clock)
            case .mail: MailTabRoot(models: models, clock: clock)
            case .notes: PlaceholderScreen(tab: .notes, note: "Notes your agents write.")
            case .calendar: PlaceholderScreen(tab: .calendar, note: "Six days, each a compressed timeline.")
            }
        }
        // DESIGN.md §1 Color: `bg` is every screen ground, edge to edge, under
        // the status bar and under the tab bar alike.
        .background(Theme.Color.bg.ignoresSafeArea())
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

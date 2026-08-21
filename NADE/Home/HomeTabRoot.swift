//
//  HomeTabRoot.swift
//  NADE
//
//  The Ask tab: 2a at the root, 1a and 1b pushed from it, 1c presented over it.
//
//  Same shape as `MailTabRoot`, and for the same reasons — the path lives in
//  `AppNavigation` so it survives a tab switch (D29). 1c is a
//  `fullScreenCover` rather than a push because `DESIGN.md` §2's navigation map
//  gives the builder **no tab bar**: a modal covers the bar instead of asking
//  anything to hide it, which is why nothing below mentions 1c.
//
//  `HomeRoute.hidesTabBar` is `false` for both routes — 1a and 1b keep the bar
//  with Ask lit. The modifier is stated anyway rather than left off: with a
//  native `TabView` (D98) the bar's visibility is a property each destination
//  asserts, and "no modifier" would read as "nobody thought about it" the day
//  a third route arrives.
//

import SwiftUI

struct HomeTabRoot: View {

    @Environment(AppNavigation.self) private var navigation

    let sync: MailSync
    let models: MailModels
    let clock: NADEClock

    /// Quantised to the minute.
    ///
    /// `clock.now()` straight from `body` gives a new `Date` on every render,
    /// and `RootTabView.body` reads `navigation.selection` — so every tab tap
    /// produced a fresh value, every row differed from its previous self, and
    /// SwiftUI re-rendered and re-formatted all of them. Nothing on these
    /// screens is finer-grained than a minute (`CRITERIA.md` §9 already accepts
    /// that `now` is captured at build time), so equal renders now compare equal.
    private var now: Date {
        let seconds = clock.now().timeIntervalSinceReferenceDate
        return Date(timeIntervalSinceReferenceDate: (seconds / 60).rounded(.down) * 60)
    }

    var body: some View {
        @Bindable var navigation = navigation

        NavigationStack(path: $navigation.homePath) {
            HomeFeedView(model: models.home, now: now)
                .toolbar(.hidden, for: .navigationBar)
                .navigationDestination(for: HomeRoute.self) { route in
                    destination(route)
                        // `HomeRoute.hidesTabBar` is always false, so this is
                        // the nav bar alone — but it is written through the same
                        // helper as `MailTabRoot` so the two stacks cannot drift.
                        .modifier(HiddenBars(alsoTabBar: route.hidesTabBar))
                        .nadeInteractivePopGesture()
                }
        }
        .fullScreenCover(item: Binding(
            get: { navigation.editingAgentID.map(EditingAgent.init(id:)) },
            set: { navigation.editingAgentID = $0?.id }
        )) { editing in
            // Keyed by id: opening a different agent must build a different
            // model, not reuse the first one's loaded state. The view owns it
            // from there — see `AgentBuilderView`'s `@State`.
            AgentBuilderView(sync: sync, agentID: editing.id)
                .id(editing.id)
        }
    }

    @ViewBuilder
    private func destination(_ route: HomeRoute) -> some View {
        switch route {
        case .query(let query, let routeHint):
            // A fresh model per query — `AskModel` is one streaming session, and
            // reusing it would replay the previous answer under a new question.
            // `.id(query)` is what makes "fresh" mean *per query* rather than
            // per re-render; the view's own `@State` supplies the other half.
            AskView(sync: sync, query: query, routeHint: routeHint, now: now)
                .id(query)
        case .agents:
            AgentsListView(model: models.agents, now: now)
        }
    }
}

/// `fullScreenCover(item:)` needs an `Identifiable`, and a bare `String` is not
/// one. A wrapper rather than making `String` conform, which would be a global
/// change for one call site.
private struct EditingAgent: Identifiable, Hashable {
    let id: String
}

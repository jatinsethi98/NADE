//
//  HomeFeedView.swift
//  NADE
//
//  2a — the app's opening screen. Two stacked states, feed and focus, toggled
//  by the grabber (`DESIGN.md` §2a).
//
//  The two states are separate view bodies rather than one body with branches,
//  because they share only the ask field: the feed is a scrolling list under a
//  pinned header, and focus is a vertically centred block over a peek.
//

import SwiftUI

struct HomeFeedView: View {

    private enum M {
        // Feed header: `58 / 20 / 11`, bottom divider.
        static let headerTop: CGFloat = 58
        static let headerH = Theme.Space.screenDetail
        static let headerBottom: CGFloat = 11
        static let dateSize: CGFloat = 17
        static let newSize: CGFloat = 11
        static let newTracking: CGFloat = 0.06
        static let headerRowGap: CGFloat = 8
        static let headerRowBottom: CGFloat = 11

        // Grabber strip, inside the scroll view.
        static let grabberW: CGFloat = 34
        static let grabberH: CGFloat = 3
        static let grabberGap: CGFloat = 5
        static let grabberTop: CGFloat = 9
        static let grabberBottom: CGFloat = 8
        static let grabberLabel: CGFloat = 10
        static let grabberTracking: CGFloat = 0.08

        // The feed row's own numbers live in `FeedRow.M` and are **not**
        // repeated here. Only the two this file draws with are re-exported,
        // because the peek reuses them.
        static let rowBorder = FeedRow.M.border
        static let titleTracking = FeedRow.M.titleTracking

        static let footerSize: CGFloat = 12
        static let footerTop: CGFloat = 16
        static let footerBottom: CGFloat = 24

        // Focus.
        static let greetingSize: CGFloat = 26
        static let focusDateSize: CGFloat = 11
        static let eyebrowSize: CGFloat = 10.5
        static let eyebrowTracking: CGFloat = 0.09
        static let eyebrowBottom: CGFloat = 8
        static let promptRowV: CGFloat = 11
        static let promptGap: CGFloat = 12
        static let promptIndexSize: CGFloat = 11.5
        static let promptIndexTop: CGFloat = 3
        static let promptSize: CGFloat = 14.5
        static let promptLineHeight: CGFloat = 1.45
        static let agentsTop: CGFloat = 26
        static let agentsPaddingTop: CGFloat = 14
        static let agentsGap: CGFloat = 8
        static let agentsCountSize: CGFloat = 16
        static let agentsMetaSize: CGFloat = 13
        static let askTop: CGFloat = 22
        static let peekTop: CGFloat = 9
        static let peekBottom: CGFloat = 12
        static let peekGrabberBottom: CGFloat = 9
        static let peekGap: CGFloat = 7
        static let peekOpacity: CGFloat = 0.45
        static let peekSourceSize: CGFloat = 10.5
        static let peekSnippetSize: CGFloat = 12.5
        static let peekBorderInset: CGFloat = 9
        static let peekPlainInset: CGFloat = 11
    }

    /// `DESIGN.md` §2a focus: three prompts under a "TRY" eyebrow. Static copy —
    /// there is no endpoint behind them, and every one has to be a thing v1 can
    /// actually do (§4), so none of them promises an outbound action.
    /// `DESIGN.md` §4's three replacements, **verbatim** — "one per route the
    /// classifier produces (`answer`, `results`, `agent_draft`), so tapping each
    /// demonstrates a different state".
    ///
    /// `FixtureRouteTests` asserts that each of the three still reaches its own
    /// route, because the strings and the classifier live in different files and
    /// nothing else binds them: the first version of this list was improvised,
    /// and its second prompt routed to `answer` like the first.
    static let focusPrompts = [
        String(localized: "What did Priya say about the design review?",
               comment: "2a focus, suggested prompt 1 — the answer route"),
        String(localized: "Find every receipt from last month",
               comment: "2a focus, suggested prompt 2 — the results route"),
        String(localized: "When a recruiter emails, note the next steps",
               comment: "2a focus, suggested prompt 3 — the agent_draft route"),
    ]

    @Environment(AppNavigation.self) private var navigation
    let model: HomeFeedModel
    let now: Date

    var body: some View {
        Group {
            switch model.mode {
            case .feed: feedState
            case .focus: focusState
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
        .background(Theme.Color.bg)
        .task {
            model.start()
            consumeAskFocusRequest()
            await model.refresh()
        }
        // 2a is already on screen when 1b's "New" pops back to it, so `.task`
        // does not re-run — the request has to be observed as well as read.
        .onChange(of: navigation.wantsAskFocus) { _, _ in consumeAskFocusRequest() }
    }

    // MARK: - Feed

    private var feedState: some View {
        Group {
            ScrollView {
                LazyVStack(spacing: 0) {
                    grabberStrip
                    ForEach(model.items) { item in
                        FeedRow(item: item, now: now,
                                onApprove: { Task { await model.approve(item) } },
                                onSkip: { Task { await model.skip(item) } })
                            // `API.md` §7 marks an info card seen as it is
                            // scrolled to, not when the page loads. Marking the
                            // whole page on appear cleared the badge for rows
                            // below the fold that were never looked at.
                            //
                            // The guard is here rather than inside `markSeen` so
                            // an approval scrolling past does not allocate a
                            // `Task` only to no-op — most rows are approvals.
                            .onAppear {
                                guard item.kind == .info, item.status == .new else { return }
                                Task { await model.markSeen(item) }
                            }
                        Hairline()
                    }

                    // The pagination trigger. `GET /feed` is cursored, and
                    // without a sentinel every item past the first page was
                    // unreachable — `loadMore` existed and nothing called it.
                    if model.canLoadMore {
                        ProgressView()
                            .frame(maxWidth: .infinity)
                            .padding(.vertical, M.footerTop)
                            .onAppear { Task { await model.loadMore() } }
                            .accessibilityIdentifier("home.loadmore")
                    }
                    if let problem = model.connectionProblem {
                        StateCaption(problem.message)
                            .padding(.top, StateCaption.topPadding)
                            .accessibilityIdentifier("home.problem")
                    } else if model.items.isEmpty {
                        StateCaption(Self.emptyCopy)
                            .padding(.top, StateCaption.topPadding)
                            .accessibilityIdentifier("home.empty")
                    }

                    // Unconditional: `DESIGN.md` §2a lists the footer as part of
                    // the feed state, not as something that appears once there
                    // is content. "Older activity lives in the run log." is as
                    // true of an empty feed as a full one.
                    feedFooter
                }
            }
            .refreshable { await model.refresh() }
        }
        // The date, the "N NEW" count and the pinned ask field are chrome, so
        // they leave the content stack and become a bar floating over the feed.
        // `safeAreaInset` insets the safe area by the bar's height, gives it
        // Liquid Glass, and applies the scroll edge effect that keeps a feed row
        // legible as it passes underneath (D98). The `Hairline()` that used to
        // close the band goes with it — that separation is the edge effect's job
        // now, and it does it while the content is moving rather than only at
        // rest.
        .safeAreaInset(edge: .top, spacing: 0) { feedHeader.nadeChromeBar() }
        // A receipt gathered but not yet flushed must not be lost to a tab
        // switch or a push. Since D98 this also fires on every departure from
        // the Ask tab rather than only when 2a is torn down, which flushes
        // sooner and is strictly better — `flushSeen` was already idempotent.
        .onDisappear { Task { await model.flushSeen() } }
    }

    static let footerCopy = String(
        localized: "Older activity lives in the run log.",
        comment: "2a, the footer under the feed"
    )

    static let emptyCopy = String(
        localized: "Nothing waiting. Your agents will post here.",
        comment: "2a, when the feed has no items"
    )

    private var feedHeader: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack(alignment: .firstTextBaseline, spacing: M.headerRowGap) {
                Text(HomeDate.long(now))
                    .font(Theme.Font.heading(M.dateSize))
                    .foregroundStyle(Theme.Color.ink)
                    .accessibilityAddTraits(.isHeader)
                    .accessibilityIdentifier("home.date")
                Spacer(minLength: 0)
                // Always rendered, including at zero. `DESIGN.md` §2a's header
                // is a fixed row; hiding the count at zero moves the date's
                // baseline the moment the last approval is answered.
                Text("\(model.newCount) NEW")
                    .font(Theme.Font.body(M.newSize))
                    .nadeTracking(M.newTracking, at: M.newSize)
                    .tabularNumerals()
                    .foregroundStyle(Theme.Color.accent)
                    .accessibilityIdentifier("home.newcount")
            }
            .padding(.bottom, M.headerRowBottom)

            askField
        }
        .padding(.top, M.headerTop)
        .padding(.horizontal, M.headerH)
        .padding(.bottom, M.headerBottom)
    }

    private var askField: some View {
        @Bindable var model = model
        return NAskField(text: $model.query, metrics: .feed) { submit() }
    }

    private var grabberStrip: some View {
        Button {
            model.mode = .focus
        } label: {
            VStack(spacing: M.grabberGap) {
                Capsule()
                    .fill(Theme.Color.neutral400)
                    .frame(width: M.grabberW, height: M.grabberH)
                Text("PULL DOWN TO HIDE")
                    .font(Theme.Font.body(M.grabberLabel))
                    .nadeTracking(M.grabberTracking, at: M.grabberLabel)
                    .foregroundStyle(Theme.Color.ink55)
            }
            .frame(maxWidth: .infinity)
            .padding(.top, M.grabberTop)
            .padding(.bottom, M.grabberBottom)
            .contentShape(Rectangle())
            .overlay(alignment: .bottom) { Hairline() }
        }
        .buttonStyle(.plain)
        .accessibilityLabel(Text("Hide the feed", comment: "2a grabber, spoken name"))
        .accessibilityIdentifier("home.grabber")
    }

    private var feedFooter: some View {
        // `StateCaption`'s default *is* italic 12 / `ink55`, and it also carries
        // the italic line box the hand-rolled version dropped — which put this
        // footer on a different baseline from every other caption in the app.
        StateCaption(Self.footerCopy, alignment: .leading)
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.top, M.footerTop)
            .padding(.horizontal, M.headerH)
            .padding(.bottom, M.footerBottom)
            .accessibilityIdentifier("home.footer")
    }

    // MARK: - Focus

    private var focusState: some View {
        VStack(spacing: 0) {
            HStack(alignment: .firstTextBaseline, spacing: M.headerRowGap) {
                Text(Self.greeting(at: now))
                    .font(Theme.Font.heading(M.greetingSize, weight: .regular))
                    .foregroundStyle(Theme.Color.ink)
                    .accessibilityAddTraits(.isHeader)
                    .accessibilityIdentifier("home.greeting")
                Spacer(minLength: 0)
                Text(HomeDate.short(now))
                    .font(Theme.Font.body(M.focusDateSize))
                    .nadeTracking(M.newTracking, at: M.focusDateSize)
                    .tabularNumerals()
                    .foregroundStyle(Theme.Color.ink60)
            }
            .padding(.top, M.headerTop)
            .padding(.horizontal, M.headerH)

            Spacer(minLength: 0)

            VStack(alignment: .leading, spacing: 0) {
                SectionEyebrow("Try", color: Theme.Color.ink60,
                               size: M.eyebrowSize, tracking: M.eyebrowTracking)
                    .padding(.bottom, M.eyebrowBottom)

                ForEach(Array(Self.focusPrompts.enumerated()), id: \.offset) { index, prompt in
                    Hairline()
                    Button {
                        navigation.ask(prompt)
                    } label: {
                        HStack(alignment: .top, spacing: M.promptGap) {
                            Text(String(format: "%02d", index + 1))
                                .font(Theme.Font.body(M.promptIndexSize))
                                .tabularNumerals()
                                .foregroundStyle(Theme.Color.accent)
                                .padding(.top, M.promptIndexTop)
                            Text(prompt)
                                .font(Theme.Font.body(M.promptSize))
                                .foregroundStyle(Theme.Color.ink)
                                .nadeLineHeight(M.promptLineHeight, size: M.promptSize)
                                .fixedSize(horizontal: false, vertical: true)
                                .frame(maxWidth: .infinity, alignment: .leading)
                        }
                        .padding(.vertical, M.promptRowV)
                        .contentShape(Rectangle())
                    }
                    .buttonStyle(.plain)
                    .accessibilityIdentifier("home.prompt.\(index)")
                }

                agentsRow
                askFocusField
            }
            .padding(.horizontal, M.headerH)

            Spacer(minLength: 0)

            peek
        }
    }

    private var agentsRow: some View {
        VStack(spacing: 0) {
            Hairline()
            Button {
                navigation.openAgents()
            } label: {
                HStack(alignment: .firstTextBaseline, spacing: M.agentsGap) {
                    Text("\(model.agentCount) agents")
                        .font(Theme.Font.heading(M.agentsCountSize, weight: .regular))
                        .foregroundStyle(Theme.Color.ink)
                    Text("· \(model.agentsSummary)")
                        .font(Theme.Font.body(M.agentsMetaSize))
                        .foregroundStyle(Theme.Color.ink60)
                    Spacer(minLength: 0)
                    Text("All →")
                        .font(Theme.Font.body(M.agentsMetaSize))
                        .foregroundStyle(Theme.Color.accent)
                }
                .padding(.top, M.agentsPaddingTop)
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .accessibilityIdentifier("home.agents")
        }
        .padding(.top, M.agentsTop)
    }

    private var askFocusField: some View {
        @Bindable var model = model
        return NAskField(text: $model.query, metrics: .focus, identifier: "ask.field.focus") {
            submit()
        }
        .padding(.top, M.askTop)
    }

    private var peek: some View {
        VStack(spacing: 0) {
            Hairline()
            Button {
                model.mode = .feed
            } label: {
                VStack(spacing: M.grabberGap) {
                    Capsule()
                        .fill(Theme.Color.neutral400)
                        .frame(width: M.grabberW, height: M.grabberH)
                    Text("\(model.newCount) NEW · SWIPE UP")
                        .font(Theme.Font.body(M.grabberLabel))
                        .nadeTracking(M.grabberTracking, at: M.grabberLabel)
                        .tabularNumerals()
                        .foregroundStyle(Theme.Color.accent)
                }
                .frame(maxWidth: .infinity)
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .accessibilityLabel(Text("Show the feed", comment: "2a peek grabber, spoken name"))
            .accessibilityIdentifier("home.peek")
            .padding(.bottom, M.peekGrabberBottom)

            VStack(alignment: .leading, spacing: M.peekGap) {
                ForEach(Array(model.items.prefix(2).enumerated()), id: \.element.id) { index, item in
                    HStack(spacing: M.headerRowGap) {
                        Text(item.title)
                            .textCase(.uppercase)
                            .font(Theme.Font.body(M.peekSourceSize))
                            .nadeTracking(M.titleTracking, at: M.peekSourceSize)
                            .foregroundStyle(Theme.Color.accent)
                            .lineLimit(1)
                            .layoutPriority(1)
                        Text(item.body)
                            .font(Theme.Font.body(M.peekSnippetSize))
                            .foregroundStyle(Theme.Color.ink)
                            .lineLimit(1)
                            .truncationMode(.tail)
                        Spacer(minLength: 0)
                    }
                    .padding(.leading, index == 0 ? M.peekBorderInset : M.peekPlainInset)
                    // The first preview carries a 2 pt accent left border; the
                    // second is inset a little further with none. An overlay,
                    // not a sibling in the HStack: a `Rectangle` beside the text
                    // has no intrinsic height, so it stretched to fill the peek
                    // and dragged the whole focus layout off centre with it.
                    .overlay(alignment: .leading) {
                        if index == 0 {
                            Rectangle()
                                .fill(Theme.Color.accent)
                                .frame(width: M.rowBorder)
                        }
                    }
                }
            }
            .opacity(M.peekOpacity)
            .frame(maxWidth: .infinity, alignment: .leading)
            .accessibilityHidden(true)   // EDGE (E6): a dimmed teaser of rows the feed state speaks in full
        }
        .padding(.top, M.peekTop)
        .padding(.horizontal, M.headerH)
        .padding(.bottom, M.peekBottom)
    }

    // MARK: -

    /// Honours 1b's "New": show the focus state, whose ask field is the centred
    /// one. Consumed on read, so it is a request and never a mode.
    private func consumeAskFocusRequest() {
        guard navigation.wantsAskFocus else { return }
        navigation.wantsAskFocus = false
        model.mode = .focus
    }

    private func submit() {
        let query = model.query
        model.query = ""
        navigation.ask(query)
    }

    /// The mockup says "Good morning". A greeting fixed at morning would be
    /// wrong for most of the day, so it follows the clock — and the clock is
    /// the injected one, or `-NADENow` could not pin a screenshot.
    static func greeting(at now: Date) -> String {
        switch Calendar.current.component(.hour, from: now) {
        case ..<12: String(localized: "Good morning", comment: "2a focus greeting, before noon")
        case 12..<18: String(localized: "Good afternoon", comment: "2a focus greeting, afternoon")
        default: String(localized: "Good evening", comment: "2a focus greeting, evening")
        }
    }
}

/// 2a's two date strings. Separate from `ListTime`, which answers "how long
/// ago" for a mail row; these are absolute and never relative.
nonisolated enum HomeDate {
    /// "Sunday 16 August".
    static func long(_ date: Date) -> String {
        formatter(template: "EEEE d MMMM").string(from: date)
    }

    /// "SUN 16 AUG".
    static func short(_ date: Date) -> String {
        formatter(template: "EEE d MMM").string(from: date).uppercased()
    }

    /// `dateFormat`, not `setLocalizedDateFormatFromTemplate`.
    ///
    /// The template form respects the locale's *field order*, and in `en_US`
    /// that is "Monday, August 17" — the design says "Sunday 16 August", day
    /// before month and no comma, and `DESIGN.md` outranks the more
    /// idiomatic-looking call. Month and weekday **names** still come from the
    /// locale, so only the order is pinned.
    ///
    /// Memoised (D88): 2a's header is rebuilt on every keystroke in the ask
    /// field, and 1a's on every streamed token, for a date that cannot change.
    private static func formatter(template: String) -> DateFormatter {
        DateFormatterCache.fixed(template, calendar: ListTime.calendar)
    }
}

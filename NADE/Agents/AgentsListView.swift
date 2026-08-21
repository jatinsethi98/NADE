//
//  AgentsListView.swift
//  NADE
//
//  1b (`DESIGN.md` §1b). Pushed from 2a's focus state, and it keeps the tab bar
//  with **Ask** lit — the design's own navigation map puts Agents on the Ask
//  tab, not a tab of its own.
//

import SwiftUI

struct AgentsListView: View {

    private enum M {
        static let headerTop: CGFloat = 62
        static let horizontal = Theme.Space.screen
        static let headerBottom: CGFloat = 12
        static let headerGap: CGFloat = 10
        static let titleSize: CGFloat = 23
        static let actionSize: CGFloat = 13
        static let filterV: CGFloat = 11
        static let filterGap: CGFloat = 16
        static let filterSize: CGFloat = 12
        static let filterUnderline: CGFloat = 3
        static let rowV: CGFloat = 14
        static let nameGap: CGFloat = 9
        static let nameSize: CGFloat = 17
        static let statusSize: CGFloat = 10
        static let statusTracking: CGFloat = 0.06
        static let definitionSize: CGFloat = 13
        static let definitionLineHeight: CGFloat = 1.55
        static let definitionTop: CGFloat = 4
        static let metaTop: CGFloat = 8
        static let metaGap: CGFloat = 7
        static let metaSize: CGFloat = 11
    }

    @Environment(AppNavigation.self) private var navigation
    let model: AgentsListModel
    let now: Date

    var body: some View {
        VStack(spacing: 0) {
            header
            Hairline()
            filterRow
            Hairline()
            list
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
        .background(Theme.Color.bg)
        .task {
            model.start()
            await model.refresh()
        }
    }

    private var header: some View {
        HStack(alignment: .firstTextBaseline, spacing: M.headerGap) {
            BackChevron(label: String(localized: "Back to Home",
                                      comment: "1b back affordance, spoken name"),
                        identifier: "agents.back") {
                // `removeLast()` on an empty array is a **fatal error**, and two
                // fast taps before the pop commits reach it.
                navigation.popHome()
            }
            Text("Agents")
                .font(Theme.Font.heading(M.titleSize))
                .foregroundStyle(Theme.Color.ink)
                .accessibilityAddTraits(.isHeader)
            Spacer(minLength: 0)
            // "New" returns to 2a's **focus** state, where the ask field is
            // the centred, accent-bordered one.
            //
            // It used to call `ask("")`, which `AppNavigation.ask` correctly
            // refuses for an empty query — and then cleared the path anyway, so
            // the button's whole effect was to throw you off 1b having created
            // nothing. An agent is made from a sentence
            // (`POST /agents {nl_definition}`), so the only honest destination
            // is the field you type it into.
            Button {
                navigation.newAgent()
            } label: {
                Text("New")
                    .font(Theme.Font.body(M.actionSize))
                    .foregroundStyle(Theme.Color.accent)
                    .nadeHitTarget()   // EDGE (E17): ~18 pt of text
            }
            .buttonStyle(.plain)
            .accessibilityIdentifier("agents.new")
        }
        .padding(.top, M.headerTop)
        .padding(.horizontal, M.horizontal)
        .padding(.bottom, M.headerBottom)
    }

    private var filterRow: some View {
        HStack(spacing: M.filterGap) {
            filter(.all, "All \(model.agents.count)")
            filter(.published, "Published \(model.publishedCount)")
            filter(.drafts, "Drafts \(model.draftCount)")
            Spacer(minLength: 0)
        }
        .padding(.vertical, M.filterV)
        .padding(.horizontal, M.horizontal)
    }

    private func filter(_ value: AgentsListModel.Filter, _ label: String) -> some View {
        let isSelected = model.filter == value
        return Button {
            model.filter = value
        } label: {
            Text(label)
                .font(Theme.Font.body(M.filterSize))
                .foregroundStyle(isSelected ? Theme.Color.accent : Theme.Color.ink60)
                .padding(.bottom, M.filterUnderline)
                .overlay(alignment: .bottom) {
                    if isSelected {
                        Rectangle()
                            .fill(Theme.Color.accent)
                            .frame(height: Theme.Stroke.border)
                    }
                }
                .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityAddTraits(isSelected ? [.isSelected] : [])
        .accessibilityIdentifier("agents.filter.\(value)")
    }

    /// The caption renders **over** the cached rows, never instead of them.
    ///
    /// The first version branched on the problem first, so a 429 or a timeout on
    /// a populated list blanked it — and took the pull-to-refresh away with it,
    /// leaving no way to retry without leaving the screen. This is the same rule
    /// the mail list already follows (A16): a failure explains what is stale, it
    /// does not delete it.
    private var list: some View {
        ScrollView {
            LazyVStack(spacing: 0) {
                if let problem = model.connectionProblem {
                    StateCaption(problem.message)
                        .padding(.top, StateCaption.topPadding)
                        .accessibilityIdentifier("agents.problem")
                } else if model.visible.isEmpty {
                    StateCaption(Self.emptyCopy)
                        .padding(.top, StateCaption.topPadding)
                        .accessibilityIdentifier("agents.empty")
                }

                ForEach(model.visible) { agent in
                    row(agent)
                    Hairline()
                }
            }
        }
        .refreshable { await model.refresh() }
    }

    static let emptyCopy = String(
        localized: "No agents yet. Describe one in the ask field.",
        comment: "1b, when there are no agents"
    )

    private func row(_ agent: WireAgentRow) -> some View {
        Button {
            navigation.openAgent(agent.id)
        } label: {
            VStack(alignment: .leading, spacing: 0) {
                HStack(alignment: .firstTextBaseline, spacing: M.nameGap) {
                    Text(agent.name)
                        .font(Theme.Font.heading(M.nameSize))
                        .foregroundStyle(Theme.Color.ink)
                    Spacer(minLength: 0)
                    Text(agent.status.rawValue)
                        .textCase(.uppercase)
                        .font(Theme.Font.body(M.statusSize))
                        .nadeTracking(M.statusTracking, at: M.statusSize)
                        .foregroundStyle(Self.color(for: agent.status))
                }

                Text(agent.nlDefinition)
                    .font(Theme.Font.body(M.definitionSize))
                    .foregroundStyle(Theme.Color.ink68)
                    .nadeLineHeight(M.definitionLineHeight, size: M.definitionSize)
                    .fixedSize(horizontal: false, vertical: true)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(.top, M.definitionTop)

                HStack(spacing: M.metaGap) {
                    Text(agent.triggerSummary)
                        .foregroundStyle(Theme.Color.accent)
                    Text("·").foregroundStyle(Theme.Color.ink62)
                    Text(Self.lastRun(agent.lastRunAt, now: now))
                        .foregroundStyle(Theme.Color.ink62)
                    if agent.approvalRequired {
                        Text("·").foregroundStyle(Theme.Color.ink62)
                        Text("needs approval").foregroundStyle(Theme.Color.ink62)
                    }
                    Spacer(minLength: 0)
                }
                .font(Theme.Font.body(M.metaSize))
                .tabularNumerals()
                .padding(.top, M.metaTop)
            }
            .padding(.vertical, M.rowV)
            .padding(.horizontal, M.horizontal)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityIdentifier("agents.row.\(agent.id)")
    }

    /// `DESIGN.md` §1b's status colours. `paused` needs one because
    /// `ends.after` moves agents there (`API.md` §5.2), even though no agent in
    /// the mockup is paused.
    static func color(for status: AgentStatus) -> Color {
        switch status {
        case .published: Theme.Color.accent
        case .draft: Theme.Color.ink62
        case .paused: Theme.Color.neutral500
        case .unknown: Theme.Color.ink62
        }
    }

    /// "ran 2 days ago" or "never run".
    ///
    /// `RelTime`, which is what `DESIGN.md` §1b actually binds to — not
    /// `RelativeDateTimeFormatter`, which said "45 seconds ago" for "just now",
    /// "3 months ago" for "16 May", and "in 2 minutes" whenever the device's
    /// clock trailed the server's. It also allocated a formatter per row per
    /// render, inside `body`.
    static func lastRun(_ date: Date?, now: Date) -> String {
        guard let date else {
            return String(localized: "never run", comment: "1b meta, an agent that has not run")
        }
        return String(localized: "ran \(RelTime.string(for: date, now: now))",
                      comment: "1b meta, when an agent last ran")
    }
}

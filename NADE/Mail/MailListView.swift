//
//  MailListView.swift
//  NADE
//
//  Screen **1e**, pushed from 1g.
//
//  The header carries a leading `‹` in accent, which the mockup does not draw —
//  1e is an artboard there, with no way in or out. Making the chevron part of
//  the existing baseline row costs no height and no divider, so the rest of
//  DESIGN.md §1e's numbers stand exactly as written.
//

import SwiftUI

struct MailListView: View {

    enum Metrics {
        // Header `60 / 22 / 10`, baseline-aligned, gap 10, **no** bottom
        // divider — the chip row carries it.
        static let headerTop: CGFloat = 60
        static let headerBottom: CGFloat = 10
        static let headerGap: CGFloat = 10
        static let titleSize: CGFloat = 23
        static let accountSize: CGFloat = 11
        static let accountTracking: CGFloat = 0.06   // em

        // Chip row `6 / 22 / 12`, gap 8, bottom divider.
        static let chipRowTop: CGFloat = 6
        static let chipRowBottom: CGFloat = 12
        static let chipGap: CGFloat = 8

        // Optional caption row `10 / 22`, italic 12, bottom divider.
        static let captionPaddingV: CGFloat = 10
        static let captionSize: CGFloat = 12
    }

    @Environment(AppNavigation.self) private var navigation
    let model: MailListModel
    let mailboxes: [WireMailbox]
    let accountEmail: String
    let mailboxID: String
    let now: Date

    private var selected: WireMailbox? {
        mailboxes.first { $0.id == mailboxID }
    }

    var body: some View {
        VStack(spacing: 0) {
            MailListHeader(
                title: selected?.name ?? "",
                accountEmail: accountEmail,
                back: { navigation.popMail() }
            )
            chipRow
            Hairline()
            caption
            rows
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
        .task(id: mailboxID) { await model.refresh(mailboxID: mailboxID) }
    }

    // MARK: Chips

    private var chipRow: some View {
        ScrollViewReader { proxy in
            ScrollView(.horizontal, showsIndicators: false) {
                HStack(spacing: Metrics.chipGap) {
                    ForEach(mailboxes, id: \.id) { box in
                        NChip(box.name, isSelected: box.id == mailboxID) {
                            navigation.openMailbox(box.id)
                        }
                        .id(box.id)
                        .accessibilityIdentifier("maillist.chip.\(box.id)")
                    }
                }
                // A one-point inset so a selected chip's ring is not clipped by
                // the scroll view's own edge.
                .padding(.horizontal, Theme.Space.screen)
                .padding(.vertical, 1)
            }
            .padding(.top, Metrics.chipRowTop - 1)
            .padding(.bottom, Metrics.chipRowBottom - 1)
            .onAppear { proxy.scrollTo(mailboxID, anchor: .center) }
            .onChange(of: mailboxID) { _, id in
                withAnimation { proxy.scrollTo(id, anchor: .center) }
            }
        }
    }

    // MARK: Caption

    @ViewBuilder
    private var caption: some View {
        // `v1 →` the mockup's smart-rule sentence. v1 has no rules, so
        // DESIGN.md §1e says to render the selected mailbox's own name, and
        // only for a user label.
        if let selected, selected.kind == .user {
            StateCaption(selected.name, alignment: .leading)
                .padding(.vertical, Metrics.captionPaddingV)
                .padding(.horizontal, Theme.Space.screen)
                .accessibilityIdentifier("maillist.caption")
            Hairline()
        }
    }

    // MARK: Rows

    /// What an empty list says, in precedence order.
    ///
    /// EDGE (P8) / A16: a failure never turns into "no mail" — when there are
    /// cached rows they stay and the banner sits above them; when there are
    /// none, the banner is the whole answer. And "never fetched" says
    /// **nothing**: claiming "No mail here" before asking is a claim the app
    /// has not earned, and the store's first value is synchronous so the gap is
    /// a single frame at most.
    private var emptyCaption: String? {
        model.connectionProblem?.message ?? (model.hasFetched ? StateCopy.emptyMailbox : nil)
    }

    @ViewBuilder
    private var rows: some View {
        if model.rows.isEmpty {
            if let emptyCaption {
                StateCaption(emptyCaption)
                    .padding(.top, StateCaption.topPadding)
                    .accessibilityIdentifier("maillist.caption.empty")
            }
            Spacer(minLength: 0)
        } else {
            ScrollView {
                if let problem = model.connectionProblem {
                    StateCaption(problem.message)
                        .padding(.top, Theme.Space.s4)
                        .padding(.horizontal, Theme.Space.screen)
                        .accessibilityIdentifier("maillist.problem")
                }
                LazyVStack(spacing: 0) {
                    ForEach(model.rows, id: \.id) { row in
                        MailRow(row: row, now: now) { navigation.openThread(row.id, in: mailboxID) }
                        Hairline()
                    }
                    // One sentinel below the list, not one per row. Attaching a
                    // `.task` inside the `ForEach` spawned a Task for every row
                    // that scrolled into view, of which all but the last did
                    // nothing but fail a guard — and doubled the view count in
                    // the lazy stack.
                    if let last = model.rows.last {
                        Color.clear
                            .frame(height: 0)
                            .task(id: last.id) {
                                await model.loadMoreIfNeeded(mailboxID: mailboxID, after: last)
                            }
                    }
                }
            }
        }
    }
}

/// 1e's header band, lifted out of the screen so `MailGeometryTests` can measure
/// what it renders rather than assert that a constant equals itself (D33/D49).
struct MailListHeader: View {
    private typealias Metrics = MailListView.Metrics

    let title: String
    let accountEmail: String
    let back: () -> Void

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: Metrics.headerGap) {
            // `+ Added` — the mockup gives 1e no way back to 1g. A chevron
            // inside the existing baseline row adds no height and keeps the
            // header's "no bottom divider" rule intact.
            BackChevron(
                label: String(localized: "Mailboxes",
                              comment: "Back button on the mail list, returning to the mailboxes screen"),
                identifier: "maillist.back",
                action: back
            )

            // The title is the *selected mailbox's* name, not the word "Mail".
            Text(title)
                .font(Theme.Font.heading(Metrics.titleSize))
                .foregroundStyle(Theme.Color.ink)
                .lineLimit(1)
                .truncationMode(.tail)
                .nadeLineHeight(Theme.LineHeight.body, size: Metrics.titleSize,
                                face: Theme.Font.PostScriptName.headingSemibold)
                .accessibilityAddTraits(.isHeader)
                .accessibilityIdentifier("maillist.title")

            Spacer(minLength: Theme.Space.s2)

            // `v1 →` the mockup's accent dot + "2 accounts" become the single
            // account's address, at the same 11 pt uppercase `ink60`.
            Text(accountEmail)
                .textCase(.uppercase)
                .font(Theme.Font.body(Metrics.accountSize))
                .foregroundStyle(Theme.Color.ink60)
                .nadeTracking(Metrics.accountTracking, at: Metrics.accountSize)
                .lineLimit(1)
                .truncationMode(.middle)
                .accessibilityIdentifier("maillist.account")
        }
        .padding(.top, Metrics.headerTop)
        .padding(.horizontal, Theme.Space.screen)
        .padding(.bottom, Metrics.headerBottom)
    }
}

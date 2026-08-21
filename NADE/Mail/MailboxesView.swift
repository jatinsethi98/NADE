//
//  MailboxesView.swift
//  NADE
//
//  Screen **1g**, and the Mail tab's root.
//
//  The mockup draws 1e and 1g as independent artboards and never says how you
//  get from one to the other. v1 makes 1g the root: its ACCOUNTS row is the
//  mockup's own "All inboxes" entry, so tapping it opens the inbox, and every
//  LABELS row opens its own mailbox. 1e then has somewhere to go back to.
//
//  Metrics are `docs/DESIGN.md` §1g's, and they live here as named constants
//  rather than in `Theme` — they are per-screen design facts, not shared
//  tokens (P1 §D1). `MailGeometryTests` measures the render against them.
//

import SwiftUI

struct MailboxesView: View {

    enum Metrics {
        // Header `62 / 22 / 12` + divider.
        static let headerTop: CGFloat = 62
        static let headerBottom: CGFloat = 12
        static let titleSize: CGFloat = 23
        static let actionSize: CGFloat = 13

        // "Every later eyebrow on this screen is `20 / 22 / 6`."
        static let firstEyebrowTop: CGFloat = 16
        static let eyebrowTop: CGFloat = 20

        // Account row `12 / 22`, gap 10, 7 pt dot.
        static let accountPaddingV: CGFloat = 12
        static let accountGap: CGFloat = 10
        static let dot: CGFloat = 7
        static let emailSize: CGFloat = 15
        static let subLabelSize: CGFloat = 12
        static let countSize: CGFloat = 13

        // Label row `13 / 22`, second line 12 pt at top 3.
        static let rowPaddingV: CGFloat = 13
        static let nameSize: CGFloat = 15
        static let secondLineSize: CGFloat = 12
        static let secondLineTop: CGFloat = 3

        // STANDARD cell `12 / 22`, 14 pt, then a 24 pt spacer.
        static let standardPaddingV: CGFloat = 12
        static let standardSize: CGFloat = 14
        static let trailingSpacer: CGFloat = 24
    }

    @Environment(AppNavigation.self) private var navigation
    let model: MailboxesModel

    var body: some View {
        content
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
        // Chrome leaves the content stack and becomes a bar floating over it.
        // `safeAreaInset` insets the safe area by the bar's height and
        // `nadeChromeBar()` gives it
        // Liquid Glass, and applies the scroll edge effect that keeps content
        // legible as it passes underneath (D98).
        //
        // The `Hairline()` that used to close the band is gone with it. Its job
        // was to say where chrome ended and content began; that is what the
        // scroll edge effect now says, continuously and while moving.
            .safeAreaInset(edge: .top, spacing: 0) { header.nadeChromeBar() }
            .task { model.start(); await model.refresh() }
    }

    // MARK: Header

    private var header: some View {
        HStack(alignment: .firstTextBaseline, spacing: Theme.Space.s2) {
            Text("Mailboxes")
                .font(Theme.Font.heading(Metrics.titleSize))
                .foregroundStyle(Theme.Color.ink)
                .nadeLineHeight(Theme.LineHeight.body, size: Metrics.titleSize,
                                face: Theme.Font.PostScriptName.headingSemibold)
                .accessibilityAddTraits(.isHeader)
                .accessibilityIdentifier("mailboxes.title")

            Spacer(minLength: Theme.Space.s2)

            // `v1 →` DESIGN.md §1g replaces the mockup's "Edit" with
            // "Settings": nothing is editable under a single account, and 1k
            // has no other entry point.
            Button {
                navigation.openSettings()
            } label: {
                Text("Settings")
                    .font(Theme.Font.body(Metrics.actionSize))
                    .foregroundStyle(Theme.Color.accent)
                    .nadeHitTarget()
            }
            .buttonStyle(.plain)
            .accessibilityIdentifier("mailboxes.settings")
        }
        .padding(.top, Metrics.headerTop)
        .padding(.horizontal, Theme.Space.screen)
        .padding(.bottom, Metrics.headerBottom)
    }

    // MARK: Body

    private var content: some View {
        ScrollView {
            VStack(spacing: 0) {
                if let problem = model.connectionProblem {
                    StateCaption(problem.message)
                        .padding(.top, Theme.Space.s4)
                        .padding(.horizontal, Theme.Space.screen)
                        .accessibilityIdentifier("mailboxes.problem")
                }

                accountsSection

                if let caption = Self.caption(for: model.state) {
                    StateCaption(caption)
                        .padding(.top, StateCaption.topPadding)
                        .accessibilityIdentifier("mailboxes.caption")
                } else {
                    labelsSection
                    standardSection
                }

                Spacer(minLength: Metrics.trailingSpacer)
            }
        }
    }

    /// Each non-ready state has its own sentence — the distinctions are
    /// load-bearing, and `StateCopy` says why beside the strings.
    private static func caption(for state: AppState) -> String? {
        switch state {
        case .unpaired: StateCopy.notPaired
        case .needsGmail: StateCopy.noMailboxes
        case .syncPending: StateCopy.syncPending
        case .ready: nil
        }
    }

    private var accountsSection: some View {
        VStack(spacing: 0) {
            SectionBand(title: "Accounts", top: Metrics.firstEyebrowTop)
            if let account = model.account, !account.email.isEmpty {
                Hairline()
                AccountRow(
                    email: account.email,
                    status: account.status,
                    unread: model.accountUnread
                ) {
                    navigation.openMailbox("INBOX")
                }
                Hairline()
            }
            // `v1 →` the mockup's second account row and its "＋ Add mailbox"
            // are cut: one account, and nothing to add it with.
        }
    }

    private var labelsSection: some View {
        VStack(spacing: 0) {
            // `v1 →` "SMART MAILBOXES" becomes "LABELS": there is no rule
            // builder, so these are Gmail's own labels rather than smart boxes,
            // and the "＋ New" that made one is cut.
            SectionBand(title: "Labels", top: Metrics.eyebrowTop)
            ForEach(Array(model.mailboxes.enumerated()), id: \.element.id) { index, box in
                Hairline()
                MailboxRow(mailbox: box) { navigation.openMailbox(box.id) }
                // The mockup's last smart-mailbox row carries a bottom rule as
                // well as a top one, closing the group.
                if index == model.mailboxes.count - 1 { Hairline() }
            }
        }
    }

    private var standardSection: some View {
        VStack(spacing: 0) {
            SectionBand(title: "Standard", top: Metrics.eyebrowTop)
            Hairline()
            // `v1 →` DESIGN.md §1g: of Drafts / Sent / Archive / Junk only
            // `SENT` is synced, so the two-column grid becomes one full-width
            // cell at the same metrics. It duplicates the LABELS row above by
            // design — the mockup keeps STANDARD as its own band, and DESIGN.md
            // says to render the one cell rather than to drop the section.
            Button {
                navigation.openMailbox("SENT")
            } label: {
                Text("Sent")
                    .font(Theme.Font.body(Metrics.standardSize))
                    .foregroundStyle(Theme.Color.ink)
                    .nadeLineHeight(Theme.LineHeight.body, size: Metrics.standardSize)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(.vertical, Metrics.standardPaddingV)
                    .padding(.horizontal, Theme.Space.screen)
                    .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .accessibilityIdentifier("mailboxes.standard.sent")
            Hairline()
        }
    }

}

// MARK: - Rows

/// The mockup's "All inboxes" row, under a single account.
///
/// It keeps the mockup's *shape* — filled, dotted, counted — and changes only
/// what v1 knows: the account's address instead of "All inboxes", and its
/// connection state instead of "Both accounts, one list".
struct AccountRow: View {
    let email: String
    let status: AccountStatus
    let unread: Int
    let action: () -> Void

    private typealias M = MailboxesView.Metrics

    private var subLabel: String {
        switch status {
        case .ok: String(localized: "Connected", comment: "Account row sub-label when Gmail is linked")
        default: String(localized: "Needs sign-in", comment: "Account row sub-label when the Gmail credential is dead")
        }
    }

    var body: some View {
        Button(action: action) {
            HStack(alignment: .firstTextBaseline, spacing: M.accountGap) {
                Circle()
                    .fill(Theme.Color.accent)
                    .frame(width: M.dot, height: M.dot)
                    .accessibilityHidden(true)   // EDGE (E6): decorative

                VStack(alignment: .leading, spacing: 0) {
                    Text(email)
                        .font(Theme.Font.body(M.emailSize))
                        .foregroundStyle(Theme.Color.ink)
                        .nadeLineHeight(Theme.LineHeight.body, size: M.emailSize)
                    Text(subLabel)
                        .font(Theme.Font.body(M.subLabelSize))
                        .foregroundStyle(Theme.Color.ink55)
                        .nadeLineHeight(Theme.LineHeight.body, size: M.subLabelSize)
                }
                .frame(maxWidth: .infinity, alignment: .leading)

                Text("\(unread)")
                    .font(Theme.Font.body(M.countSize))
                    .foregroundStyle(Theme.Color.accent800)
                    .tabularNumerals()
                    .accessibilityHidden(true)
            }
            .padding(.vertical, M.accountPaddingV)
            .padding(.horizontal, Theme.Space.screen)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(Theme.Color.accent100)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(email)
        .nadeAccessibilityValue(unread > 0
            ? String(localized: "\(subLabel), \(unread) unread",
                     comment: "Account row spoken value: connection state and unread count")
            : subLabel)
        .accessibilityAddTraits(.isButton)
        .accessibilityIdentifier("mailboxes.account")
    }
}

struct MailboxRow: View {
    let mailbox: WireMailbox
    let action: () -> Void

    private typealias M = MailboxesView.Metrics

    /// `v1 →` the mockup's second line is a smart-mailbox rule sentence. There
    /// are no rules in v1, so it carries the count instead — which is what the
    /// row is otherwise silent about, since the number on the right is unread.
    private var secondLine: String {
        String(localized: "\(mailbox.total) threads",
               comment: "Mailbox row sub-label: how many threads are in the synced window")
    }

    var body: some View {
        Button(action: action) {
            VStack(alignment: .leading, spacing: 0) {
                HStack(alignment: .firstTextBaseline, spacing: Theme.Space.s2) {
                    Text(mailbox.name)
                        .font(Theme.Font.body(M.nameSize))
                        .foregroundStyle(Theme.Color.ink)
                        .lineLimit(1)
                        .truncationMode(.tail)   // EDGE (E3)
                        .nadeLineHeight(Theme.LineHeight.body, size: M.nameSize)
                    Spacer(minLength: Theme.Space.s2)
                    Text("\(mailbox.unread)")
                        .font(Theme.Font.body(M.countSize))
                        .foregroundStyle(mailbox.unread > 0 ? Theme.Color.accent : Theme.Color.ink60)
                        .tabularNumerals()
                }
                Text(secondLine)
                    .font(Theme.Font.body(M.secondLineSize))
                    .foregroundStyle(Theme.Color.ink62)
                    .nadeLineHeight(Theme.LineHeight.body, size: M.secondLineSize)
                    .padding(.top, M.secondLineTop)
            }
            .padding(.vertical, M.rowPaddingV)
            .padding(.horizontal, Theme.Space.screen)
            .frame(maxWidth: .infinity, alignment: .leading)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        // EDGE (E6): one element, not four, and the unread count is a value
        // rather than a number read out of context.
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(mailbox.name)
        .nadeAccessibilityValue(mailbox.unread > 0
            ? String(localized: "\(mailbox.unread) unread, \(mailbox.total) threads",
                     comment: "Mailbox row spoken value")
            : secondLine)
        .accessibilityAddTraits(.isButton)
        .accessibilityIdentifier("mailboxes.label.\(mailbox.id)")
    }
}

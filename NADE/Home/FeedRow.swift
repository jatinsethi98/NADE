//
//  FeedRow.swift
//  NADE
//
//  One card in 2a's feed (`DESIGN.md` §2a step 3).
//
//  **The buttons are rendered from `actions`, never derived.** `API.md` §7 is
//  explicit that the array is exactly the buttons to draw, in order, and that
//  it is `[]` for anything resolved, skipped, expired or `kind: "info"`.
//  Deriving them from `kind` and `status` would put an Approve button on a card
//  whose token the server has already consumed.
//

import SwiftUI

struct FeedRow: View {

    /// Internal, not private: 2a's peek draws the same 2 pt border and the same
    /// title tracking, and one copy of a design number is the point.
    enum M {
        static let top: CGFloat = 12
        static let horizontal = Theme.Space.screenDetail
        static let bottom: CGFloat = 13
        static let border: CGFloat = 2
        static let titleSize: CGFloat = 10.5
        static let titleTracking: CGFloat = 0.07
        static let timeSize: CGFloat = 11
        static let bodySize: CGFloat = 14
        static let bodyLineHeight: CGFloat = 1.6
        static let bodyTop: CGFloat = 5
        static let buttonsTop: CGFloat = 10
        static let buttonsGap: CGFloat = 8
        static let resolvedSize: CGFloat = 12.5
        static let resolvedTop: CGFloat = 8
    }

    let item: WireFeedItem
    let now: Date
    let onApprove: () -> Void
    let onSkip: () -> Void

    /// Accent only while the card is an approval that still wants a tap.
    private var borderColor: Color {
        item.kind == .approval && item.status == .new ? Theme.Color.accent : .clear
    }

    /// "Save note" / "Save draft" — **never** "Send". v1 takes no outbound
    /// action, so the primary button names the local effect (PLAN C2).
    /// The fallback is deliberately also local: a card whose `data` we cannot
    /// read must not invent a verb.
    private var primaryLabel: String {
        item.data?.actionLabel
            ?? String(localized: "Save", comment: "Fallback label for an approval whose data is unrecognised")
    }

    var body: some View {
        HStack(spacing: 0) {
            Rectangle()
                .fill(borderColor)
                .frame(width: M.border)
                .accessibilityHidden(true)

            VStack(alignment: .leading, spacing: 0) {
                HStack(alignment: .firstTextBaseline, spacing: Theme.Space.s2) {
                    Text(item.title)
                        .textCase(.uppercase)   // free; `.uppercased()` allocates per render
                        .font(Theme.Font.body(M.titleSize))
                        .nadeTracking(M.titleTracking, at: M.titleSize)
                        .foregroundStyle(Theme.Color.accent)
                        .accessibilityIdentifier("feed.title")
                    Spacer(minLength: 0)
                    Text(ListTime.string(for: item.createdAt, now: now))
                        .font(Theme.Font.body(M.timeSize))
                        .tabularNumerals()
                        .foregroundStyle(Theme.Color.ink60)
                }

                Text(item.body)
                    .font(Theme.Font.body(M.bodySize))
                    .foregroundStyle(Theme.Color.ink)
                    .nadeLineHeight(M.bodyLineHeight, size: M.bodySize)
                    .fixedSize(horizontal: false, vertical: true)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(.top, M.bodyTop)
                    .accessibilityIdentifier("feed.body")

                if !item.actions.isEmpty {
                    HStack(spacing: M.buttonsGap) {
                        ForEach(item.actions, id: \.rawValue) { action in
                            button(for: action)
                        }
                    }
                    .padding(.top, M.buttonsTop)
                }

                if let note = item.resolvedNote, !note.isEmpty {
                    Text(note)
                        .font(Theme.Font.bodyItalic(M.resolvedSize))
                        .foregroundStyle(Theme.Color.accent700)
                        .fixedSize(horizontal: false, vertical: true)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .padding(.top, M.resolvedTop)
                        .accessibilityIdentifier("feed.resolved")
                }
            }
            .padding(.top, M.top)
            .padding(.horizontal, M.horizontal)
            .padding(.bottom, M.bottom)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    @ViewBuilder
    private func button(for action: FeedAction) -> some View {
        switch action {
        case .approve:
            NButton(primaryLabel, variant: .primary, action: onApprove)
                .accessibilityIdentifier("feed.approve")
        case .edit:
            // PLAN §Approval semantics: "approve creates/updates the draft;
            // `PATCH /drafts/{id}` edits it after". There is no pre-approval
            // edit flow in v1, so Edit takes the same action and the draft is
            // edited from where it lands.
            NButton(String(localized: "Edit", comment: "2a, the secondary approval button"),
                    variant: .secondary, action: onApprove)
                .accessibilityIdentifier("feed.edit")
        case .skip:
            NButton(String(localized: "Skip", comment: "2a, the ghost approval button"),
                    variant: .ghost, action: onSkip)
                .accessibilityIdentifier("feed.skip")
        case .unknown:
            // EDGE: an action this build does not know. Rendering nothing is
            // right — a button whose behaviour is unknown must not be tappable.
            // `WireEnum` already decodes it to `.unknown(raw)` rather than
            // throwing, which is where the value is preserved; a zero-size view
            // existing only to hang a `print` off it is not worth the pixel.
            EmptyView()
        }
    }
}

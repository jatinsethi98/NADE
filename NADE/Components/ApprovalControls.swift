//
//  ApprovalControls.swift
//  NADE
//
//  Everything a card shows *below its body* when it is still asking: who a
//  draft would be addressed to, and the buttons that answer.
//
//  # Why this is one view and not two copies
//
//  NADE has two approval surfaces — 2a's feed row and 1f's thread agent card —
//  and they had the block typed out twice. That is not only duplication: the
//  copies had already diverged, and the half that went missing was the safety
//  half.
//
//  `backend/testdata/injection/README.md` finding 10 is explicit that
//  `identity-02` and `tool-01`/`tool-06` are contained **only** if the approval
//  card shows the actual recipient list and flags `never_messaged` — "an
//  approval card that renders only the body launders a redirected draft". The
//  feed row carried that row. The thread card, which offers the same Approve
//  button for the same draft, did not: it drew the model's prose and a button,
//  and nothing that said where the draft would go. A control that lives on one
//  of two surfaces is a control the next surface will not have either, and P6's
//  push detail and P7's draft sheet are the next two.
//
//  The button labels are the other half. `item.data.actionLabel` is what stops
//  the primary button ever reading "Approve" or "Send" (PLAN C1/C2), and its
//  local-only fallback was also written twice.
//

import SwiftUI

struct ApprovalControls: View {

    enum M {
        static let buttonsTop: CGFloat = 10
        static let buttonsGap: CGFloat = 8
        static let recipientSize: CGFloat = 12.5
        static let recipientTop: CGFloat = 8
    }

    let item: WireFeedItem
    /// `"feed"` or `"thread"` — the accessibility identifiers the UI tests
    /// address each surface by.
    let idPrefix: String
    /// 1f's card sits inside tighter padding than 2a's row, so its buttons
    /// carry the §1f number rather than the §2a one.
    var buttonsTop: CGFloat = M.buttonsTop
    let onApprove: () -> Void
    let onSkip: () -> Void

    /// "Save note" / "Save draft" — **never** "Send". v1 takes no outbound
    /// action, so the primary button names the local effect (PLAN C2). The
    /// fallback is deliberately also local: a card whose `data` we cannot read
    /// must not invent a verb.
    private var primaryLabel: String {
        item.data?.actionLabel
            ?? String(localized: "Save",
                      comment: "Fallback label for an approval whose data is unrecognised")
    }

    var body: some View {
        recipients

        if !item.actions.isEmpty {
            HStack(spacing: M.buttonsGap) {
                ForEach(item.actions, id: \.rawValue) { action in
                    button(for: action)
                }
            }
            .padding(.top, buttonsTop)
        }
    }

    /// **Who the draft is addressed to, above the button that saves it.**
    ///
    /// `+ Added` — the mockup's card has no such row, and
    /// `backend/testdata/injection/README.md`'s finding 10 is why it must:
    /// "`identity-02` and `tool-01`/`tool-06` are contained **only** if the
    /// approval card shows the actual recipient list and flags
    /// `never_messaged`. An approval card that renders only the body launders
    /// a redirected draft." The body is prose the model wrote after reading
    /// somebody else's mail; the recipient list is not.
    ///
    /// Only on a live `draft_reply` card: a note has no recipient, and a
    /// settled card is a record rather than a question.
    @ViewBuilder
    private var recipients: some View {
        let addresses = item.data?.recipients ?? []
        if !item.actions.isEmpty, !addresses.isEmpty {
            HStack(alignment: .firstTextBaseline, spacing: Theme.Space.s2) {
                Text(addresses.joined(separator: ", "))
                    .font(Theme.Font.body(M.recipientSize))
                    .foregroundStyle(Theme.Color.ink60)
                    .lineLimit(2)
                    .truncationMode(.middle)
                if item.data?.neverMessaged == true {
                    Text("Never messaged", comment: "An approval card's recipient warning")
                        .font(Theme.Font.bodyItalic(M.recipientSize))
                        .foregroundStyle(Theme.Color.accent700)
                        .fixedSize()
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.top, M.recipientTop)
            .accessibilityIdentifier("\(idPrefix).recipients")
        }
    }

    @ViewBuilder
    private func button(for action: FeedAction) -> some View {
        switch action {
        case .approve:
            NButton(primaryLabel, variant: .primary, action: onApprove)
                .accessibilityIdentifier("\(idPrefix).approve")
        case .edit:
            // PLAN §Approval semantics and deviation 54: "approve creates or
            // updates the draft; `PATCH /drafts/{id}` edits it after". There is
            // no pre-approval edit flow in v1, so Edit takes the same action and
            // the draft is edited from where it lands.
            NButton(String(localized: "Edit", comment: "The secondary approval button"),
                    variant: .secondary, action: onApprove)
                .accessibilityIdentifier("\(idPrefix).edit")
        case .skip:
            NButton(String(localized: "Skip", comment: "The ghost approval button"),
                    variant: .ghost, action: onSkip)
                .accessibilityIdentifier("\(idPrefix).skip")
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

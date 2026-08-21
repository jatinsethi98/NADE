//
//  NAskField.swift
//  NADE
//
//  `DESIGN.md` §2 "The ask field" — the pill input plus its circular accent
//  button, which four screens draw at four sizes.
//
//  One component rather than four hand-assembled `HStack`s, because the table
//  in §2 varies *five* things at once (field metrics, border, circle diameter,
//  glyph, placeholder) and a screen that re-typed them would drift from the
//  design the moment one row changed. The field's own numbers already live in
//  `NTextField.Metrics`; this adds only the button and the pairing.
//

import SwiftUI

struct NAskField: View {

    /// One row of `DESIGN.md` §2's table.
    struct Metrics: Sendable, Equatable {
        var field: NTextField.Metrics
        var circle: CGFloat
        var glyph: String
        var glyphSize: CGFloat

        /// 2a feed, pinned above the list.
        static let feed = Metrics(field: .askPinned, circle: 38,
                                  glyph: "sparkles", glyphSize: 16)
        /// 2a focus / 2b, centred. The only variant whose field border is accent
        /// at rest — the emphasis is the point of the state.
        static let focus = Metrics(field: .askCentred, circle: 44,
                                   glyph: "arrow.up", glyphSize: 18)
        /// 1a, docked over the tab bar.
        static let docked = Metrics(field: .askDocked, circle: 40,
                                    glyph: "arrow.up", glyphSize: 17)
    }

    /// `DESIGN.md` §2 gives three of the four rows the same placeholder, so it
    /// is the default rather than something every caller repeats.
    static let defaultPlaceholder = String(
        localized: "Ask, search, or describe an agent",
        comment: "Placeholder in the ask field on the home and ask screens"
    )

    private let placeholder: String
    @Binding private var text: String
    private let metrics: Metrics
    private let identifier: String
    private let onSubmit: () -> Void

    init(
        text: Binding<String>,
        metrics: Metrics,
        placeholder: String = NAskField.defaultPlaceholder,
        identifier: String = "ask.field",
        onSubmit: @escaping () -> Void
    ) {
        self._text = text
        self.metrics = metrics
        self.placeholder = placeholder
        self.identifier = identifier
        self.onSubmit = onSubmit
    }

    /// Whitespace is not a query, and neither is an invisible one. The button
    /// is disabled rather than hidden so the row does not change width as you
    /// type — and so the control that does nothing is *visibly* doing nothing
    /// (`DESIGN.md` §4).
    private var canSubmit: Bool { !text.nadeIsBlank }

    var body: some View {
        HStack(spacing: Theme.Space.s2 + 0.8) {   // §2: gap 10
            NTextField(placeholder, text: $text, metrics: metrics.field)
                .accessibilityIdentifier(identifier)
                .onSubmit(submit)
                // **Not `.send`.** `SubmitLabel.send` maps to
                // `UIReturnKeyType.send`, which renders the literal word "Send"
                // on the keyboard — an outbound promise v1 cannot keep (PLAN
                // C1/C2, DESIGN §4). The gallery's `Send` sweep cannot see it,
                // because the system keyboard is not in the app's element tree.
                .submitLabel(.go)

            // `NButtonStyle(variant: .icon, corner: .pill)` — the style whose own
            // doc comments read "the ask field's circular send button" and "the
            // ask field's circular button is 38 (2a/1f), 40 (1a) or 44 (2b)".
            // It was written for this component and this component was drawing
            // its own circle instead, which cost the DS press wash, the
            // `nadeHitTarget()` the 38 pt variant needs to clear 44 pt, and
            // `NButton.disabledOpacity` (the literal 0.45 was a fourth copy).
            Button(action: submit) {
                NIcon(metrics.glyph, size: metrics.glyphSize)
            }
            .buttonStyle(NButtonStyle(variant: .icon, corner: .pill, iconBox: metrics.circle))
            .nadeHitTarget()
            .disabled(!canSubmit)
            .accessibilityLabel(Text("Ask", comment: "The ask field's submit button"))
            .accessibilityIdentifier("\(identifier).submit")
        }
        .frame(maxWidth: .infinity)
    }

    private func submit() {
        guard canSubmit else { return }
        onSubmit()
    }
}

#if DEBUG
#Preview {
    @Previewable @State var text = ""
    return VStack(spacing: 24) {
        NAskField(text: $text, metrics: .feed) {}
        NAskField(text: $text, metrics: .focus) {}
        NAskField(text: $text, metrics: .docked) {}
    }
    .padding(22)
    .background(Theme.Color.bg)
}
#endif

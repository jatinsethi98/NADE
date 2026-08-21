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

    /// How near the field and the button have to be before their glass merges.
    ///
    /// `GlassEffectContainer(spacing:)` decides when two effects stop being two
    /// shapes and start being one: larger blends sooner. The row's own gap is
    /// 10 (§2), and this is deliberately **below** it — the pill and the circle
    /// are two controls that do two things, and the design draws them as two
    /// separate strokes. The container is here for the other thing it buys,
    /// which is one glass pass for both rather than two.
    static let glassSpacing: CGFloat = 4

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
        // The ask field is the app's most-looked-at control and it sits in the
        // chrome layer on all four screens that draw it, so it is the one
        // component that takes Liquid Glass (D98). Content keeps its Classical
        // stroke-not-fill grammar; this is chrome.
        //
        // The container is what makes the pill and the circle one glass pass
        // rather than two, and what lets them morph together if a future state
        // moves them — see `glassSpacing`.
        GlassEffectContainer(spacing: Self.glassSpacing) {
            row
        }
    }

    private var row: some View {
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
                // After the field's own modifiers, never before: the effect
                // captures what is behind the view it is attached to, and
                // attaching it first would have it capture the bare text.
                // The design's border still draws on top — `NTextField` strokes
                // its own capsule, accent when focused — so the stroke reads as
                // the rim of the glass rather than being replaced by it.
                .glassEffect(.regular, in: .capsule)

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
            // `.interactive()` is what makes the glass answer a touch the way
            // every system control does. The style keeps its own press wash
            // underneath, which is the design's, so the two read as one press.
            //
            // Not `.buttonStyle(.glass)`: that would take the whole control,
            // and with it the accent stroke, `NButton.disabledOpacity` and the
            // `nadeHitTarget()` the 38 pt variant needs to clear 44 pt.
            .glassEffect(.regular.interactive(), in: .circle)
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

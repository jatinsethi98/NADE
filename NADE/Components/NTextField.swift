//
//  NTextField.swift
//  NADE
//
//  DESIGN.md §1 Components — Input (DS `.input`) and §2 — the ask field.
//
//  The DS input is one thing; the ask field is five, and they differ in every
//  number the eye can see. `Metrics` carries them so a screen never has to fork
//  the component:
//
//  | Preset        | Screen            | Font | Pad     | Min-height | Shape | Border  |
//  |---------------|-------------------|------|---------|------------|-------|---------|
//  | `.input`      | DS `.input`       | 14   | 6 / 10  | 36         | md    | divider |
//  | `.askPinned`  | 2a feed           | 13.5 | 8 / 15  | 38         | pill  | divider |
//  | `.askDocked`  | 1a                | 14   | 9 / 15  | 40         | pill  | divider |
//  | `.askCentred` | 2a focus / 2b     | 14   | 10 / 16 | 44         | pill  | accent  |
//  | `.askThread`  | 1f                | 13.5 | 10 / 15 | 38*        | pill  | divider |
//  | `.searchPill` | 1h "Search notes" | 13.5 | 8 / 14  | 38*        | pill  | divider |
//
//  *1f and 1h set no `min-height` in the mockup; the CSS box is ~43 and ~39,
//  driven entirely by padding and the inherited `line-height: 1.55`. 38 is a
//  floor that never binds — it is there so an empty field cannot collapse
//  (EDGE E4) and so 1f's field can never draw shorter than the 38 pt circle
//  beside it.
//

import SwiftUI

struct NTextField: View {
    enum Shape: Sendable, Equatable {
        /// DS `.input` — radius-md.
        case rounded
        /// The ask field and 1h's search field — radius 999.
        case pill
    }

    /// What the field's outline is **at rest**. Focus always paints accent on
    /// top of it (DS `.input:focus-visible { border-color: var(--color-accent) }`),
    /// so this is only about the unfocused state.
    enum Border: Sendable, Equatable {
        /// DS `.input` — 1 pt `divider`.
        case divider
        /// 2a focus / 2b. The mockup sets `border-color: var(--color-accent)`
        /// on the field itself, and DESIGN.md §2 and §3-2a·4 both say accent —
        /// the field is gold *before* anyone taps it. The focus state is not
        /// how that screen gets its colour, so this cannot be folded into
        /// `isFocused`.
        case accent
    }

    struct Metrics: Sendable, Equatable {
        var fontSize: CGFloat
        var paddingV: CGFloat
        var paddingH: CGFloat
        var minHeight: CGFloat
        var shape: Shape
        var border: Border = .divider

        /// DS `.input` — 14 pt, `6 / 10`, min-height 36, radius 4.
        static let input = Metrics(fontSize: 14, paddingV: 6, paddingH: 10, minHeight: 36, shape: .rounded)
        /// 2a, pinned above the feed.
        static let askPinned = Metrics(fontSize: 13.5, paddingV: 8, paddingH: 15, minHeight: 38, shape: .pill)
        /// 1a, docked over the tab bar.
        static let askDocked = Metrics(fontSize: 14, paddingV: 9, paddingH: 15, minHeight: 40, shape: .pill)
        /// 2a focus / 2b, centred. The only variant with an accent border at rest.
        static let askCentred = Metrics(
            fontSize: 14, paddingV: 10, paddingH: 16, minHeight: 44, shape: .pill, border: .accent
        )
        /// 1f, the thread's reply bar.
        static let askThread = Metrics(fontSize: 13.5, paddingV: 10, paddingH: 15, minHeight: 38, shape: .pill)
        /// 1h, "Search notes".
        static let searchPill = Metrics(fontSize: 13.5, paddingV: 8, paddingH: 14, minHeight: 38, shape: .pill)

        /// The colour the outline is drawn in when the field is not focused.
        /// One source for the view and for the test that reads it back off a
        /// render, so neither can drift from the other.
        var restingBorderColor: Color {
            border == .accent ? Theme.Color.accent : Theme.Color.divider
        }
    }

    private let placeholder: String
    @Binding private var text: String
    private let metrics: Metrics
    /// Gallery / preview only: draws the focus ring without stealing focus.
    private let showsFocusRing: Bool

    @FocusState private var isFocused: Bool

    init(
        _ placeholder: String,
        text: Binding<String>,
        metrics: Metrics = .input,
        showsFocusRing: Bool = false
    ) {
        self.placeholder = placeholder
        self._text = text
        self.metrics = metrics
        self.showsFocusRing = showsFocusRing
    }

    private var focused: Bool { isFocused || showsFocusRing }

    private var shape: RoundedRectangle {
        RoundedRectangle(
            cornerRadius: metrics.shape == .pill ? Theme.Radius.pill : Theme.Radius.md,
            style: .circular
        )
    }

    var body: some View {
        TextField("", text: $text)
            .font(Theme.Font.body(metrics.fontSize))
            .foregroundStyle(Theme.Color.ink)
            // DS `caret-color: var(--color-accent)`
            .tint(Theme.Color.accent)
            .textFieldStyle(.plain)
            .focused($isFocused)
            // The placeholder is drawn rather than passed to `prompt:` so its
            // colour and face match the design exactly; the field keeps its
            // spoken label via `.accessibilityLabel` below. EDGE (E4), (E6).
            .overlay(alignment: .leading) {
                if text.isEmpty {
                    Text(placeholder)
                        .font(Theme.Font.body(metrics.fontSize))
                        .foregroundStyle(Theme.Color.ink62)
                        .lineLimit(1)
                        .truncationMode(.tail)   // EDGE (E3)
                        .allowsHitTesting(false)
                        .accessibilityHidden(true)
                }
            }
            // No `nadeLineHeight` here, deliberately. `.input` inherits the DS
            // `line-height: 1.55`, but SwiftUI's `TextField` already reserves a
            // line box a shade *taller* than the CSS one (≈22 pt at both 13.5
            // and 14), so padding + `min-height` lands within a point of the
            // CSS box on its own. Adding the modifier on top double-counts and
            // makes every field 2–3 pt too tall. Measured in
            // `ComponentGeometryTests.testAskFieldHeightsMatchEachScreen`.
            .padding(.horizontal, metrics.paddingH)
            .padding(.vertical, metrics.paddingV)
            .frame(minHeight: metrics.minHeight)
            .background(shape.fill(Color.clear))
            .overlay {
                shape.strokeBorder(
                    focused ? Theme.Color.accent : metrics.restingBorderColor,
                    lineWidth: Theme.Stroke.border
                )
            }
            .contentShape(shape)
            .accessibilityLabel(placeholder)
    }
}

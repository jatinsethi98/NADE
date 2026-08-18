//
//  NRadioRow.swift
//  NADE
//
//  DESIGN.md §1 Components — Radio; DS `.radio`, `.radio .dot`.
//  16 pt circle, 1.5 pt ring; selected = accent fill with a 4 pt inset ring in
//  `bg` (a donut).
//
//  Two rows, not one. The mockup uses different numbers on 1c and 1d and a
//  single "radio row" that averages them is wrong on both screens:
//
//  | Preset        | Screen | Label | Gap | Padding | Align    | Trailing |
//  |---------------|--------|-------|-----|---------|----------|----------|
//  | `.invocation` | 1c     | 15    | 11  | 11 / 0  | top      | —        |
//  | `.ends`       | 1d     | 14    | 11  | 9 / 0   | centre   | 13 pt    |
//

import SwiftUI

/// The dot on its own, for rows that build their own layout.
struct NRadioDot: View {
    private let isSelected: Bool
    @ScaledMetric(relativeTo: .callout) private var diameter: CGFloat = 16

    init(isSelected: Bool) {
        self.isSelected = isSelected
    }

    var body: some View {
        Group {
            if isSelected {
                // CSS: 16 box, 1.5 accent border, `inset 0 0 0 4px bg`, accent
                // fill. Outside-in that is accent ring → bg ring → accent core.
                ZStack {
                    Circle().fill(Theme.Color.accent)
                    Circle()
                        .fill(Theme.Color.bg)
                        .padding(Theme.Stroke.radioRing * ringScale)
                    Circle()
                        .fill(Theme.Color.accent)
                        .padding((Theme.Stroke.radioRing + 4) * ringScale)
                }
            } else {
                Circle().strokeBorder(
                    Theme.Color.divider,
                    lineWidth: Theme.Stroke.radioRing * ringScale
                )
            }
        }
        .frame(width: diameter, height: diameter)
        .accessibilityHidden(true)   // EDGE (E6): the row speaks, not the dot
    }

    /// The ring thicknesses are proportions of the 16 pt dot, so they grow with it.
    private var ringScale: CGFloat { diameter / 16 }
}

struct NRadioRow: View {
    struct Metrics: Sendable, Equatable {
        var labelSize: CGFloat
        var hintSize: CGFloat
        var valueSize: CGFloat
        var gap: CGFloat
        var paddingV: CGFloat
        /// 1c aligns the dot to the label's first baseline (the mockup's
        /// `align-items: flex-start` + `margin-top: 3` on the dot); 1d centres
        /// the whole row.
        var alignsToFirstBaseline: Bool

        /// 1c Invocation — DESIGN.md §3 1c: rows `11 / 0`, gap 11, label 15,
        /// hint 12 `ink55`, dot `margin-top: 3`.
        static let invocation = Metrics(
            labelSize: 15, hintSize: 12, valueSize: 13,
            gap: 11, paddingV: 11, alignsToFirstBaseline: true
        )
        /// 1d Ends — DESIGN.md §3 1d: rows `9 / 0`, gap 11,
        /// `align-items: center`, label 14, right value 13 tabular.
        static let ends = Metrics(
            labelSize: 14, hintSize: 12, valueSize: 13,
            gap: 11, paddingV: 9, alignsToFirstBaseline: false
        )
    }

    /// EDGE (E6): what VoiceOver actually says. 1d's rows differ only in their
    /// trailing value ("16 Sep 2026", "12 runs"); drop it and the three options
    /// are indistinguishable. Kept as a pure function so the composition is
    /// unit-testable without a render.
    static func spokenLabel(label: String, hint: String?) -> String {
        guard let hint, !hint.isEmpty else { return label }
        return "\(label), \(hint)"
    }

    private let label: String
    private let hint: String?
    private let value: String?
    private let isSelected: Bool
    private let metrics: Metrics
    private let action: () -> Void

    init(
        _ label: String,
        hint: String? = nil,
        value: String? = nil,
        isSelected: Bool,
        metrics: Metrics = .invocation,
        action: @escaping () -> Void
    ) {
        self.label = label
        self.hint = hint
        self.value = value
        self.isSelected = isSelected
        self.metrics = metrics
        self.action = action
    }

    var body: some View {
        Button(action: action) {
            HStack(
                alignment: metrics.alignsToFirstBaseline ? .firstTextBaseline : .center,
                spacing: metrics.gap
            ) {
                // Baseline-aligning a shape needs an explicit anchor, otherwise
                // the dot floats to the bottom of the row.
                NRadioDot(isSelected: isSelected)
                    .alignmentGuide(.firstTextBaseline) { $0.height * 0.82 }

                VStack(alignment: .leading, spacing: 2) {
                    Text(label)
                        .font(Theme.Font.body(metrics.labelSize))
                        .foregroundStyle(Theme.Color.ink)
                        .nadeLineHeight(Theme.LineHeight.body, size: metrics.labelSize)
                        .fixedSize(horizontal: false, vertical: true)   // EDGE (E3)
                    if let hint, !hint.isEmpty {
                        Text(hint)
                            .font(Theme.Font.body(metrics.hintSize))
                            .foregroundStyle(Theme.Color.ink55)
                            .nadeLineHeight(Theme.LineHeight.body, size: metrics.hintSize)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)

                if let value, !value.isEmpty {
                    NRadioValue(value, isSelected: isSelected, size: metrics.valueSize)
                }
            }
            .padding(.vertical, metrics.paddingV)
            .contentShape(Rectangle())
            // No `nadeHitTarget` here: these rows stack edge to edge, so a
            // target taller than the row would overlap its neighbours and the
            // last one drawn would quietly steal the other's top edge. 1c's
            // row measures ~66 and 1d's ~40, both full-width — the 1d row is
            // 4 pt shy of the minimum in one dimension and ~400 pt over it in
            // the other, which is not the failure mode E17 is about.
        }
        .buttonStyle(.plain)
        // EDGE (E6): one element, spoken as "<label>, <hint>: <value>, selected".
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(Self.spokenLabel(label: label, hint: hint))
        .nadeAccessibilityValue(value)
        .accessibilityAddTraits(isSelected ? [.isButton, .isSelected] : .isButton)
    }
}

/// The right-hand value on 1d's "Ends" rows: 13 pt tabular. DESIGN.md §3 1d:
/// "**ink when that option is selected and `ink62` when it is not** — it is the
/// muted state that is coloured, never the selected one."
struct NRadioValue: View {
    private let text: String
    private let isSelected: Bool
    private let size: CGFloat

    init(_ text: String, isSelected: Bool, size: CGFloat = 13) {
        self.text = text
        self.isSelected = isSelected
        self.size = size
    }

    var body: some View {
        Text(text)
            .font(Theme.Font.body(size))
            .foregroundStyle(isSelected ? Theme.Color.ink : Theme.Color.ink62)
            .tabularNumerals()
            .lineLimit(1)   // EDGE (E3)
    }
}

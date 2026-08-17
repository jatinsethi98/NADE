//
//  NRadioRow.swift
//  NADE
//
//  DESIGN.md §1 Components — Radio; DS `.radio`, `.radio .dot`.
//  16 pt circle, 1.5 pt ring; selected = accent fill with a 4 pt inset ring in
//  `bg` (a donut).
//
//  Row geometry from 1c Invocation / 1d Ends: padding `11 / 0` with a top
//  divider, label 15 pt + hint 12 pt `ink55`, optional right-aligned value.
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

struct NRadioRow<Trailing: View>: View {
    static var labelSize: CGFloat { 15 }
    static var hintSize: CGFloat { 12 }

    private let label: String
    private let hint: String?
    private let isSelected: Bool
    private let action: () -> Void
    private let trailing: Trailing

    init(
        _ label: String,
        hint: String? = nil,
        isSelected: Bool,
        action: @escaping () -> Void,
        @ViewBuilder trailing: () -> Trailing
    ) {
        self.label = label
        self.hint = hint
        self.isSelected = isSelected
        self.action = action
        self.trailing = trailing()
    }

    var body: some View {
        Button(action: action) {
            HStack(alignment: .firstTextBaseline, spacing: Theme.Space.s2) {
                // Baseline-aligning a shape needs an explicit anchor, otherwise
                // the dot floats to the bottom of the row.
                NRadioDot(isSelected: isSelected)
                    .alignmentGuide(.firstTextBaseline) { $0.height * 0.82 }

                VStack(alignment: .leading, spacing: 2) {
                    Text(label)
                        .font(Theme.Font.body(Self.labelSize))
                        .foregroundStyle(Theme.Color.ink)
                        .fixedSize(horizontal: false, vertical: true)   // EDGE (E3)
                    if let hint, !hint.isEmpty {
                        Text(hint)
                            .font(Theme.Font.body(Self.hintSize))
                            .foregroundStyle(Theme.Color.ink55)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)

                trailing
            }
            .padding(.vertical, 11)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        // EDGE (E6): one element, spoken as "<label>, <hint>, selected".
        .accessibilityElement(children: .combine)
        .accessibilityLabel(hint.map { "\(label), \($0)" } ?? label)
        .accessibilityAddTraits(isSelected ? [.isButton, .isSelected] : .isButton)
    }
}

extension NRadioRow where Trailing == EmptyView {
    init(_ label: String, hint: String? = nil, isSelected: Bool, action: @escaping () -> Void) {
        self.init(label, hint: hint, isSelected: isSelected, action: action) { EmptyView() }
    }
}

/// The right-hand value on 1d's "Ends" rows: 13 pt tabular, accent when that
/// option is selected, `ink55` otherwise.
struct NRadioValue: View {
    private let text: String
    private let isActive: Bool

    init(_ text: String, isActive: Bool) {
        self.text = text
        self.isActive = isActive
    }

    var body: some View {
        Text(text)
            .font(Theme.Font.body(13))
            .foregroundStyle(isActive ? Theme.Color.accent : Theme.Color.ink55)
            .tabularNumerals()
            .lineLimit(1)   // EDGE (E3)
    }
}

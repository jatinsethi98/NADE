//
//  NStepper.swift
//  NADE
//
//  DESIGN.md §1d Schedule sheet — the −/+ pair.
//  30 × 30 boxes, 1 pt divider border, radius 4, accent glyph, around a tabular
//  count (min-width 22, centred, 15 pt).
//

import SwiftUI

struct NStepper: View {
    static let box: CGFloat = 30
    static let countMinWidth: CGFloat = 22
    static let countSize: CGFloat = 15
    /// DESIGN.md §3 1d: the "Repeat every" row is `align-items: center, gap 12`
    /// — the same 12 separates the label, the `−` box, the count and the `＋`.
    static let gap: CGFloat = 12

    private let label: String
    @Binding private var value: Int
    private let range: ClosedRange<Int>
    private let step: Int

    init(_ label: String, value: Binding<Int>, in range: ClosedRange<Int> = 1...99, step: Int = 1) {
        self.label = label
        self._value = value
        self.range = range
        self.step = step
    }

    private var canDecrement: Bool { value - step >= range.lowerBound }
    private var canIncrement: Bool { value + step <= range.upperBound }

    var body: some View {
        HStack(spacing: Self.gap) {
            glyphBox("−", enabled: canDecrement) { value = max(range.lowerBound, value - step) }

            Text(value, format: .number)
                .font(Theme.Font.body(Self.countSize))
                .foregroundStyle(Theme.Color.ink)
                .tabularNumerals()
                .lineLimit(1)
                .frame(minWidth: Self.countMinWidth)
                .multilineTextAlignment(.center)

            glyphBox("＋", enabled: canIncrement) { value = min(range.upperBound, value + step) }
        }
        // EDGE (E1): the boxes are chrome and keep their 30 × 30 geometry; the
        // count still grows with Dynamic Type up to the same ceiling.
        .dynamicTypeSize(...Theme.Metrics.chromeTypeCeiling)
        // EDGE (E6): one adjustable element — VoiceOver swipes up/down to change it.
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(label)
        .accessibilityValue(Text(value, format: .number))
        .accessibilityAdjustableAction { direction in
            switch direction {
            case .increment: if canIncrement { value += step }
            case .decrement: if canDecrement { value -= step }
            @unknown default: break
            }
        }
    }

    @ViewBuilder
    private func glyphBox(_ glyph: String, enabled: Bool, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            Text(glyph)
                .font(Theme.Font.body(Self.countSize))
                .foregroundStyle(Theme.Color.accent)
                .frame(width: Self.box, height: Self.box)
                .overlay {
                    RoundedRectangle(cornerRadius: Theme.Radius.md, style: .circular)
                        .strokeBorder(Theme.Color.divider, lineWidth: Theme.Stroke.border)
                }
                .contentShape(Rectangle())
                // EDGE (E17): the design's box is 30 × 30. The row it sits in
                // is 30 tall, so the target has to grow without the box doing.
                .nadeHitTarget()
        }
        .buttonStyle(.plain)
        .disabled(!enabled)
        .opacity(enabled ? 1 : NButton.disabledOpacity)
        // The pair is one adjustable element to VoiceOver; the individual boxes
        // would only add noise.
        .accessibilityHidden(true)
    }
}

//
//  NSegmented.swift
//  NADE
//
//  DESIGN.md §1 Components — Segmented; DS `.seg`, `.seg-opt`.
//  Inline row, 1 pt divider border, radius 4, dividers between options; the
//  selected option gets an *inset* 1 pt accent ring + accent text.
//
//  The inset ring is a rectangle, not a rounded rect: in CSS it is
//  `box-shadow: inset 0 0 0 1px accent` on the option, clipped by the
//  container's `overflow: hidden`. Same construction here.
//

import SwiftUI

struct NSegmented<Value: Hashable>: View {
    static var fontSize: CGFloat { 13 }        // DS `.seg-opt`
    static var paddingV: CGFloat { 7 }
    static var paddingH: CGFloat { 12 }

    private let options: [Value]
    private let label: (Value) -> String
    @Binding private var selection: Value
    private let fontSize: CGFloat
    private let paddingV: CGFloat
    private let paddingH: CGFloat

    init(
        options: [Value],
        selection: Binding<Value>,
        fontSize: CGFloat = NSegmented.fontSize,
        paddingV: CGFloat = NSegmented.paddingV,
        paddingH: CGFloat = NSegmented.paddingH,
        label: @escaping (Value) -> String
    ) {
        self.options = options
        self._selection = selection
        self.fontSize = fontSize
        self.paddingV = paddingV
        self.paddingH = paddingH
        self.label = label
    }

    private var shape: RoundedRectangle {
        RoundedRectangle(cornerRadius: Theme.Radius.md, style: .circular)
    }

    var body: some View {
        HStack(spacing: 0) {
            ForEach(Array(options.enumerated()), id: \.element) { index, option in
                if index > 0 {
                    // DS `.seg-opt + .seg-opt { border-left: 1px … }`
                    VHairline()
                }
                optionView(option)
            }
        }
        .fixedSize(horizontal: true, vertical: false)
        .clipShape(shape)
        .overlay { shape.strokeBorder(Theme.Color.divider, lineWidth: Theme.Stroke.border) }
        // EDGE (E1): chrome ceiling — a segmented control has to stay inline.
        .dynamicTypeSize(...Theme.Metrics.chromeTypeCeiling)
    }

    @ViewBuilder
    private func optionView(_ value: Value) -> some View {
        let isSelected = value == selection
        Button {
            selection = value
        } label: {
            Text(label(value))
                .font(Theme.Font.body(fontSize))
                .foregroundStyle(isSelected ? Theme.Color.accent : Theme.Color.ink)
                .lineLimit(1)                       // EDGE (E3)
                .minimumScaleFactor(0.8)
                .padding(.vertical, paddingV)
                .padding(.horizontal, paddingH)
                .frame(minHeight: fontSize + paddingV * 2)   // EDGE (E4)
                .background {
                    if isSelected {
                        // `box-shadow: inset 0 0 0 1px accent`
                        Rectangle().strokeBorder(Theme.Color.accent, lineWidth: Theme.Stroke.border)
                    }
                }
                .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        // EDGE (E6)
        .accessibilityLabel(label(value))
        .accessibilityAddTraits(isSelected ? [.isButton, .isSelected] : .isButton)
    }
}

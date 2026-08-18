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

/// DESIGN.md §1 Components — Segmented: "DS option padding `7 / 12` at 13 pt;
/// **1c overrides to `6 / 14` at 13 pt**, 1i to `5 / 12` at 12 pt."
///
/// Free-standing rather than nested in `NSegmented` because `NSegmented` is
/// generic and Swift has no static stored properties there.
struct NSegmentedMetrics: Sendable, Equatable {
    var fontSize: CGFloat
    var paddingV: CGFloat
    var paddingH: CGFloat

    /// DS `.seg-opt`
    static let ds = NSegmentedMetrics(fontSize: 13, paddingV: 7, paddingH: 12)
    /// 1c Status — Draft / Published
    static let status = NSegmentedMetrics(fontSize: 13, paddingV: 6, paddingH: 14)
    /// 1i Read / Write (cut from v1, kept because the metrics are pinned)
    static let noteMode = NSegmentedMetrics(fontSize: 12, paddingV: 5, paddingH: 12)
}

struct NSegmented<Value: Hashable>: View {
    typealias Metrics = NSegmentedMetrics

    private let options: [Value]
    private let label: (Value) -> String
    @Binding private var selection: Value
    private let metrics: Metrics

    init(
        options: [Value],
        selection: Binding<Value>,
        metrics: Metrics = .ds,
        label: @escaping (Value) -> String
    ) {
        self.options = options
        self._selection = selection
        self.metrics = metrics
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
        // EDGE (E3)/(E2): NOT `.fixedSize(horizontal: true)`. That asks for the
        // full intrinsic width unconditionally, which means the options are
        // never given a constraint — so their `lineLimit(1)` and
        // `minimumScaleFactor` can never engage and a long pair of labels runs
        // straight off a 320 pt screen. Without it the row still takes its
        // intrinsic width whenever the width is there, and compresses when it
        // is not, which is what `display: inline-flex` does in a narrow box.
        //
        // Vertically it *is* fixed: `VHairline` is a `Rectangle`, which has no
        // ideal height and will happily fill whatever it is offered, so a
        // container proposing a tall box would stretch the whole control.
        .fixedSize(horizontal: false, vertical: true)
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
                .font(Theme.Font.body(metrics.fontSize))
                .foregroundStyle(isSelected ? Theme.Color.accent : Theme.Color.ink)
                .lineLimit(1)                       // EDGE (E3)
                .minimumScaleFactor(0.8)
                // The option's height in CSS is the inherited `line-height:
                // 1.55` plus its padding — 13 × 1.55 + 14 = 34.15 for the DS,
                // 32.15 for 1c. Without this it is the face's own tighter box.
                .nadeLineHeight(Theme.LineHeight.body, size: metrics.fontSize)
                .padding(.vertical, metrics.paddingV)
                .padding(.horizontal, metrics.paddingH)
                .frame(minHeight: metrics.fontSize + metrics.paddingV * 2)   // EDGE (E4)
                .background {
                    if isSelected {
                        // `box-shadow: inset 0 0 0 1px accent`
                        Rectangle().strokeBorder(Theme.Color.accent, lineWidth: Theme.Stroke.border)
                    }
                }
                .contentShape(Rectangle())
                // EDGE (E17): an option is 32–34 pt tall — under the minimum.
                .nadeHitTarget()
        }
        .buttonStyle(.plain)
        // EDGE (E6)
        .accessibilityLabel(label(value))
        .accessibilityAddTraits(isSelected ? [.isButton, .isSelected] : .isButton)
    }
}

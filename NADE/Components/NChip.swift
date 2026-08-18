//
//  NChip.swift
//  NADE
//
//  DESIGN.md §1e Mail list — the filter chip.
//  padding 5 × 12, radius 999, 12.5 pt.
//  selected  = accent ring + accent-100 fill + accent-800 text
//  unselected = divider ring + transparent fill + ink62 text
//
//  (DESIGN.md's prose says "accent text" for the selected chip; the mockup's
//  inline style is `var(--color-accent-800)`, which is what actually renders
//  and is far more legible on the accent-100 fill. Recorded in
//  IOS_DECISIONS.md.)
//

import SwiftUI

struct NChip: View {
    static let fontSize: CGFloat = 12.5
    static let paddingV: CGFloat = 5
    static let paddingH: CGFloat = 12

    /// EDGE (E6): same rule as `NButton` — a chip with no visible name is a
    /// layout edge case, never an unnamed accessibility element.
    static func resolvedAccessibilityLabel(title: String, explicit: String?) -> String {
        if let explicit, !explicit.isEmpty { return explicit }
        if !title.isEmpty { return title }
        return String(localized: "Filter", comment: "Fallback VoiceOver name for a mail filter chip with no visible title")
    }

    private let title: String
    private let isSelected: Bool
    private let a11yLabel: String
    private let action: () -> Void

    init(_ title: String, isSelected: Bool, accessibilityLabel: String? = nil, action: @escaping () -> Void) {
        self.title = title
        self.isSelected = isSelected
        self.a11yLabel = NChip.resolvedAccessibilityLabel(title: title, explicit: accessibilityLabel)
        self.action = action
    }

    var body: some View {
        Button(action: action) {
            Text(title)
                .font(Theme.Font.body(Self.fontSize))
                .foregroundStyle(isSelected ? Theme.Color.accent800 : Theme.Color.ink62)
                // EDGE (E3): long label names truncate; the chip row scrolls.
                .lineLimit(1)
                .truncationMode(.tail)
                // Inherited `line-height: 1.55` — 12.5 × 1.55 + 10 = 29.4, the
                // chip's real height in the mockup.
                .nadeLineHeight(Theme.LineHeight.body, size: Self.fontSize)
                .padding(.vertical, Self.paddingV)
                .padding(.horizontal, Self.paddingH)
                // EDGE (E4): an empty label keeps the pill's shape.
                .frame(minHeight: Self.fontSize + Self.paddingV * 2)
                .background(Capsule().fill(isSelected ? Theme.Color.accent100 : Color.clear))
                .overlay {
                    Capsule().strokeBorder(
                        isSelected ? Theme.Color.accent : Theme.Color.divider,
                        lineWidth: Theme.Stroke.border
                    )
                }
                .contentShape(Capsule())
                // EDGE (E17): the chip is ~29 pt tall — under the minimum.
                .nadeHitTarget()
        }
        .buttonStyle(.plain)
        // EDGE (E6): VoiceOver hears the name, and hears that it is selected.
        .accessibilityLabel(a11yLabel)
        .accessibilityAddTraits(isSelected ? [.isButton, .isSelected] : .isButton)
        // EDGE (E1): chrome ceiling — the chip row must stay a row.
        .dynamicTypeSize(...Theme.Metrics.chromeTypeCeiling)
    }
}

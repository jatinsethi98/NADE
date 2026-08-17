//
//  NTag.swift
//  NADE
//
//  DESIGN.md §1 Components — Tag; DS `.tag`, `.tag-neutral`, `.tag-outline`,
//  `.tag-accent`.
//  11 pt, padding 3 × 10, radius 3, letter-spacing 0.02 em.
//

import SwiftUI

struct NTag: View {
    enum Style: Sendable, CaseIterable {
        /// neutral-100 fill, neutral-800 text
        case neutral
        /// 1 pt accent border, accent text, transparent fill
        case outline
        /// accent-100 fill, accent-800 text
        case accent
    }

    static let fontSize: CGFloat = 11
    static let tracking: CGFloat = 0.02        // em
    static let paddingV: CGFloat = 3
    static let paddingH: CGFloat = 10

    private let text: String
    private let style: Style

    init(_ text: String, style: Style = .neutral) {
        self.text = text
        self.style = style
    }

    private var foreground: Color {
        switch style {
        case .neutral: Theme.Color.neutral800
        case .outline: Theme.Color.accent
        case .accent: Theme.Color.accent800
        }
    }

    private var fill: Color {
        switch style {
        case .neutral: Theme.Color.neutral100
        case .outline: .clear
        case .accent: Theme.Color.accent100
        }
    }

    private var border: Color? {
        style == .outline ? Theme.Color.accent : nil
    }

    private var shape: RoundedRectangle {
        RoundedRectangle(cornerRadius: Theme.Radius.tag, style: .circular)
    }

    var body: some View {
        Text(text)
            .font(Theme.Font.body(Self.fontSize))
            .foregroundStyle(foreground)
            .nadeTracking(Self.tracking, at: Self.fontSize)
            // EDGE (E3): a tag never grows past its row; it truncates.
            .lineLimit(1)
            .truncationMode(.tail)
            .padding(.vertical, Self.paddingV)
            .padding(.horizontal, Self.paddingH)
            // EDGE (E4): an empty tag keeps its pill rather than drawing a sliver.
            .frame(minHeight: Self.fontSize + Self.paddingV * 2)
            .background(shape.fill(fill))
            .overlay {
                if let border {
                    shape.strokeBorder(border, lineWidth: Theme.Stroke.border)
                }
            }
            // EDGE (E1): a tag is chrome — it stops growing at AX1 so a row of
            // them cannot swallow the screen.
            .dynamicTypeSize(...Theme.Metrics.chromeTypeCeiling)
    }
}

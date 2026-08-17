//
//  NTabBar.swift
//  NADE
//
//  DESIGN.md §2 Chrome — the four-tab bar on every root screen.
//  Four equal columns, 1 pt top divider, an 18 pt stroke-1.8 icon above a
//  10.5 pt uppercase label with 0.09 em tracking, gap 5.
//  Active = accent; inactive = `ink62`.
//
//  Built by hand rather than with `TabView`: UIKit's tab bar cannot be given
//  this hairline, this type, this tracking or these colours, and `TabView`'s
//  SwiftUI styling surface does not reach any of them.
//
//  Recorded deviation (PLAN.md §Design parity map): SF Symbols stand in for
//  the design's Lucide glyphs.
//

import SwiftUI

enum NTab: String, CaseIterable, Identifiable, Sendable {
    case ask, mail, notes, calendar

    var id: String { rawValue }

    var title: String {
        switch self {
        case .ask: "Ask"
        case .mail: "Mail"
        case .notes: "Notes"
        case .calendar: "Calendar"
        }
    }

    /// DESIGN.md §2: `sparkles` · `mail`→`envelope` · `file-text`→`doc.text` · `calendar`.
    var symbol: String {
        switch self {
        case .ask: "sparkles"
        case .mail: "envelope"
        case .notes: "doc.text"
        case .calendar: "calendar"
        }
    }

    var accessibilityIdentifier: String { "tab.\(rawValue)" }
}

struct NTabBar: View {
    @Binding private var selection: NTab

    init(selection: Binding<NTab>) {
        self._selection = selection
    }

    private typealias M = Theme.Metrics.TabBar

    var body: some View {
        VStack(spacing: 0) {
            Hairline()
            HStack(spacing: 0) {
                ForEach(NTab.allCases) { tab in
                    tabButton(tab)
                }
            }
            .padding(.top, M.paddingTop)
            .padding(.horizontal, M.paddingHorizontal)
            .padding(.bottom, M.paddingBottom)
        }
        .background(Theme.Color.bg)
        // EDGE (E1): four labels at AX5 would each be ~33 pt of uppercase and
        // the bar would eat the screen. Chrome stops growing at AX1; the label
        // then shrinks and truncates rather than wrapping the bar.
        .dynamicTypeSize(...Theme.Metrics.chromeTypeCeiling)
    }

    @ViewBuilder
    private func tabButton(_ tab: NTab) -> some View {
        let isSelected = tab == selection
        Button {
            selection = tab
        } label: {
            VStack(spacing: M.iconLabelGap) {
                NIcon(tab.symbol, size: M.iconSize, weight: .light, relativeTo: .caption2)
                Text(tab.title)
                    .textCase(.uppercase)   // display-only: VoiceOver still says "Calendar"
                    .font(Theme.Font.body(M.labelSize))
                    .nadeTracking(M.labelTracking, at: M.labelSize)
                    // EDGE (E2)/(E3): on a 320 pt phone, "CALENDAR" at 10.5 pt
                    // with tracking is already tight — shrink, never wrap.
                    .lineLimit(1)
                    .minimumScaleFactor(0.7)
            }
            .foregroundStyle(isSelected ? Theme.Color.accent : Theme.Color.ink62)
            .frame(maxWidth: .infinity)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        // EDGE (E6): every tab is a button, and the active one says so.
        .accessibilityLabel(tab.title)
        .accessibilityIdentifier(tab.accessibilityIdentifier)
        .accessibilityAddTraits(isSelected ? [.isButton, .isSelected] : .isButton)
    }
}

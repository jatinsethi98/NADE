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

    /// The label's line box, `10.5 × 1.55 = 16.275`, scaling with the label.
    ///
    /// Set directly rather than through `nadeLineHeight`, which derives its
    /// delta from `UIFont.lineHeight`. That premise holds at the 13–17 pt the
    /// modifier was measured against, but at 10.5 pt Lora reports 13.44 where
    /// SwiftUI lays out 14.33 — so the modifier overshot by 1.06 and the bar
    /// came out 76.33 against the mockup's 75.28. A tab label is a fixed
    /// one-line box, which is precisely what a `frame` expresses.
    @ScaledMetric(relativeTo: .caption2) private var labelBox: CGFloat =
        Theme.Metrics.TabBar.labelSize * Theme.LineHeight.body

    init(selection: Binding<NTab>) {
        self._selection = selection
    }

    private typealias M = Theme.Metrics.TabBar

    var body: some View {
        VStack(spacing: 0) {
            Hairline()
                // Belt and braces: the container's `children: .contain` below
                // re-derives the subtree, and a decorative rule that has only
                // hidden itself can resurface as an unlabelled element.
                .accessibilityHidden(true)
            HStack(spacing: 0) {
                ForEach(Array(NTab.allCases.enumerated()), id: \.element) { index, tab in
                    tabButton(tab, index: index)
                }
            }
            // EDGE (E17): the bar's `9 / 10 / 26` padding lives **inside** each
            // button, not around the row. Outside it, a tab's tappable region
            // was only as tall as its glyph-plus-label stack (~34 pt) with 35
            // pt of dead space beneath it; inside, every column is the full
            // height of the bar and the pixels are identical.
            .padding(.horizontal, M.paddingHorizontal)
            // EDGE (E6): the four buttons are a tab bar, not four loose
            // buttons. Without a container role VoiceOver announces them as
            // isolated controls with no sense of how many there are.
            // Applied to the row rather than the whole bar so the decorative
            // top hairline stays out of the container.
            //
            // `children: .contain` is not optional here: without it the
            // container's label, identifier and traits propagate *down* and
            // every tab is renamed "Tabs" with the identifier "tabbar".
            .accessibilityElement(children: .contain)
            .accessibilityAddTraits(.isTabBar)
            .accessibilityLabel(Text("Tabs", comment: "VoiceOver name for the app's tab bar"))
            .accessibilityIdentifier(NTabBar.accessibilityIdentifier)
        }
        .background(Theme.Color.bg)
        // EDGE (E1): four labels at AX5 would each be ~33 pt of uppercase and
        // the bar would eat the screen. Chrome stops growing at AX1; the label
        // then shrinks and truncates rather than wrapping the bar.
        .dynamicTypeSize(...Theme.Metrics.chromeTypeCeiling)
    }

    static let accessibilityIdentifier = "tabbar"

    /// EDGE (E6): VoiceOver's own tab bars say "Tab 1 of 4". SwiftUI does not
    /// derive it, so the position is spoken as the button's value. Pure so the
    /// UI test can assert the exact string it expects to hear.
    static func positionValue(index: Int, count: Int = NTab.allCases.count) -> String {
        String(
            localized: "Tab \(index + 1) of \(count)",
            comment: "VoiceOver position of a tab within the tab bar"
        )
    }

    @ViewBuilder
    private func tabButton(_ tab: NTab, index: Int) -> some View {
        let isSelected = tab == selection
        Button {
            selection = tab
        } label: {
            VStack(spacing: M.iconLabelGap) {
                // Both hidden: the `Button` carries the one spoken label, and
                // an SF Symbol brings its own ("Sparkle", "Get Mail", "Plain
                // Text Document") which must never be what a tab announces.
                NIcon(tab.symbol, size: M.iconSize, weight: .light, relativeTo: .caption2)
                    .accessibilityHidden(true)
                Text(tab.title)
                    .textCase(.uppercase)   // display-only: VoiceOver still says "Calendar"
                    .font(Theme.Font.body(M.labelSize))
                    .nadeTracking(M.labelTracking, at: M.labelSize)
                    // EDGE (E2)/(E3): on a 320 pt phone, "CALENDAR" at 10.5 pt
                    // with tracking is already tight — shrink, never wrap.
                    .lineLimit(1)
                    .minimumScaleFactor(0.7)
                    // The label is a flex item, so its box is the inherited
                    // `line-height: 1.55` — 10.5 × 1.55 = 16.275, not Lora's
                    // own ~13.4. Left off, the bar's height was decided by the
                    // face rather than by the design. See `labelBox`.
                    .frame(height: labelBox)
                    .accessibilityHidden(true)
            }
            .foregroundStyle(isSelected ? Theme.Color.accent : Theme.Color.ink62)
            .padding(.top, M.paddingTop)
            .padding(.bottom, M.paddingBottom)
            .frame(maxWidth: .infinity)
            .contentShape(Rectangle())
            // …and the label collapses to a single element, so the tab is one
            // stop for VoiceOver rather than three.
            .accessibilityElement(children: .ignore)
        }
        .buttonStyle(.plain)
        // EDGE (E6): every tab is a button, the active one says so, and each
        // one says where it sits in the four.
        .accessibilityLabel(tab.title)
        .accessibilityValue(NTabBar.positionValue(index: index))
        .accessibilityIdentifier(tab.accessibilityIdentifier)
        .accessibilityAddTraits(isSelected ? [.isButton, .isSelected] : .isButton)
    }
}

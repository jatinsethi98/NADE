//
//  NTab.swift
//  NADE
//
//  The four tabs, as data.
//
//  This file used to also hold `NTabBar`, a hand-built bar of four buttons.
//  The Liquid Glass pass replaced it with a native `TabView` (see
//  `RootTabView`, IOS_DECISIONS D98), which owns the drawing, the selection
//  chrome, the tab-bar accessibility container and the "Tab 1 of 4" position
//  announcement that `NTabBar` had to synthesise by hand.
//
//  What is left is the part that was never about drawing: the identity of the
//  four tabs. `AppNavigation`, `LaunchOptions`' `-NADEScreen` deep link, the
//  gallery and the UI tests all name tabs through this enum.
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
    ///
    /// Still asserted by `ThemeTests.testTabsAreTheFourInTheDesign`, so a
    /// rename in a future SF Symbols release fails a test rather than drawing
    /// a blank — that argument did not depend on who draws the bar.
    var symbol: String {
        switch self {
        case .ask: "sparkles"
        case .mail: "envelope"
        case .notes: "doc.text"
        case .calendar: "calendar"
        }
    }

    // `accessibilityIdentifier` used to live here, as `"tab.\(rawValue)"`, and
    // `NTabBar` put it on each of its four buttons. UIKit's tab bar takes no
    // such identifier — applied to a `Tab` or to its `Label`, it reaches the
    // rendered button empty (iOS 26.5) — so the property had no reader left.
    // The UI tests address tabs through `XCUIApplication.tab(_:)`, by title.
}

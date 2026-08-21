//
//  HiddenBars.swift
//  NADE
//
//  One `.toolbar` call, not two.
//
//  Every screen in this app draws its own nav bar, so the system one is hidden
//  on every pushed destination. Since D98 the tab bar is UIKit's too, and 1f —
//  a pushed detail — is the one route that carries none (`MailRoute.hidesTabBar`).
//
//  The obvious spelling was two modifiers:
//
//      .toolbar(.hidden, for: .navigationBar)
//      .toolbar(route.hidesTabBar ? .hidden : .visible, for: .tabBar)
//
//  It compiles, the nav bar goes, and **the tab bar stays on screen over 1f**.
//  `ThreadNavigationUITests.testPushingAThreadHidesTheTabBarAndComingBackRestoresIt`
//  is what caught it. The variadic `for:` form is the one SwiftUI documents and
//  the one that works, so the choice has to be made before the call rather than
//  inside its argument — which is what this modifier is for.
//
//  Both stacks go through it, including the Ask stack where `hidesTabBar` is
//  always `false`, so the two cannot drift apart.
//

import SwiftUI

struct HiddenBars: ViewModifier {
    /// `MailRoute.hidesTabBar` / `HomeRoute.hidesTabBar` — a property of the
    /// **top route**, never of stack depth. Three of the four mail routes are
    /// pushes that keep the bar, so "deeper than the root means hide it" is
    /// wrong on the very first push.
    let alsoTabBar: Bool

    func body(content: Content) -> some View {
        if alsoTabBar {
            content.toolbar(.hidden, for: .navigationBar, .tabBar)
        } else {
            content.toolbar(.hidden, for: .navigationBar)
        }
    }
}

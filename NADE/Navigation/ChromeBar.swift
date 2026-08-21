//
//  ChromeBar.swift
//  NADE
//
//  The Liquid Glass treatment every screen's own bar wears, in one place.
//
//  **Why `safeAreaInset` and not `safeAreaBar`.** `safeAreaBar` is the iOS 26
//  API written for exactly this — it insets the safe area, applies the glass and
//  brings the scroll edge effect with it — and it was the first thing tried. It
//  **clips its content**, and this app's 44 pt hit targets are built out of an
//  overflowing `Color.clear` background (D28: "the target rides in a
//  `background`, which is laid out inside the parent's slot, is allowed to
//  overflow it, and contributes nothing to the parent's measured size").
//  Clipped, that background contributes nothing at all. Measured on iOS 26.5,
//  inside a `safeAreaBar`:
//
//      maillist.chip.INBOX   58.3 × 30.0     (the drawn capsule, target gone)
//      maillist.back          7.0 × 28.0     (the drawn chevron, target gone)
//
//  and the same two controls inside a `safeAreaInset`:
//
//      maillist.chip.INBOX   58.3 × 44.0
//      maillist.back         44.0 × 44.0
//
//  `MailUITests.testTheChipAndTheBackChevronAreBothAtLeast44Points` is what
//  caught it. The alternative was to pay for the target in layout — a
//  `.frame(minHeight: 44)` on every control in a bar — which grows every header
//  band and moves pixels D28 went out of its way not to move. `safeAreaInset`
//  costs nothing and keeps both properties.
//
//  What it costs instead: `safeAreaInset` insets the safe area but applies no
//  material, so the glass is applied here rather than inherited. That is the
//  whole of this file.
//
//  **Top bars float; bottom bands do not.** 1a's dock and 1f's ask band stay
//  where they were — the last child of the screen's stack — and take the glass
//  without taking the overlay.
//
//  The top edge is where this design has content pass under chrome, and where
//  the refraction is therefore the point. At the bottom it does not: DESIGN.md
//  ends the thread and the answer *at* the band, and both bands' numbers
//  (`ThreadView.Metrics.barBottom` 30, 1a's `dockBottom` 12) are written
//  against the display edge rather than against the home indicator. A floating
//  band there would be glass with nothing moving behind it to bend, bought at
//  the price of re-deriving two display-edge measurements.
//

import SwiftUI

extension View {
    /// A screen's own chrome band, floating over its content.
    ///
    /// Applied to the band's content, not to the screen: the effect captures
    /// what is behind the view it is attached to, so it has to sit outside the
    /// band's own padding and inside nothing else.
    ///
    /// `.regular` rather than `.clear`: these bands carry the design's 23 pt
    /// Cormorant titles and 11 pt uppercase meta over a scrolling list, and
    /// `.clear` is for chrome sitting on top of media, where there is nothing
    /// small to read. The Classical palette is tuned at 3:1 for chrome
    /// (`_ds/classical-*/readme.md`), which leaves no contrast to spend.
    func nadeChromeBar() -> some View {
        frame(maxWidth: .infinity)
            .glassEffect(.regular, in: .rect)
    }
}

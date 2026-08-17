//
//  Theme.swift
//  NADE
//
//  The Classical design system, translated from docs/DESIGN.md §1 Tokens and
//  docs/MockUps/_ds/classical-*/styles.css.
//
//  THIS IS THE ONLY FILE IN THE APP TARGET ALLOWED TO CONTAIN RAW **COLOUR AND
//  FONT** VALUES. `NADETests/NoRawValuesTests` walks NADE/** and fails on a
//  `Color(hex:`, a `.system(size:`, a `.custom(`, a `#rrggbb` literal, a
//  `Color(red:`/`UIColor(red:`, or a system palette colour found anywhere else
//  — including calls split across lines. It does **not** police geometry: sizes,
//  paddings and gaps live beside the component they belong to, as named
//  `static let`s (see `NoRawValuesTests`' header for the exact scope of the
//  claim).
//

import SwiftUI

// MARK: - Namespace

enum Theme {}

// MARK: - Color

extension Theme {
    /// DESIGN.md §1 Color. Every value is an absolute sRGB colour, never a
    /// system semantic colour, so nothing can shift when the OS appearance
    /// changes. v1 is light-only (`.preferredColorScheme(.light)` at the root).
    enum Color {
        // — core roles —
        static let bg = SwiftUI.Color(hex: 0xf3f2f2)        // every screen ground
        static let surface = SwiftUI.Color(hex: 0xeae9e9)   // sheets, dialogs
        static let ink = SwiftUI.Color(hex: 0x201f1d)       // primary text
        static let accent = SwiftUI.Color(hex: 0xb68235)    // the single accent
        static let accent2 = SwiftUI.Color(hex: 0xac803e)   // secondary accent
        /// `color-mix(in srgb, ink 16%, transparent)` — every hairline.
        static let divider = ink.opacity(0.16)

        // — the muted-ink ladder. These are *the* greys; never invent one. —
        static let ink50 = ink.opacity(0.50)
        static let ink55 = ink.opacity(0.55)
        static let ink60 = ink.opacity(0.60)
        static let ink62 = ink.opacity(0.62)
        static let ink68 = ink.opacity(0.68)
        static let ink70 = ink.opacity(0.70)
        /// DS `.card-body { opacity: 0.8 }` — the element is faded, and the
        /// colour it fades is inherited ink.
        static let ink80 = ink.opacity(0.80)

        // — neutral ramp —
        static let neutral100 = SwiftUI.Color(hex: 0xf8f4f4)
        static let neutral200 = SwiftUI.Color(hex: 0xeae7e7)
        static let neutral300 = SwiftUI.Color(hex: 0xd7d3d3)
        static let neutral400 = SwiftUI.Color(hex: 0xbab6b6)
        static let neutral500 = SwiftUI.Color(hex: 0x9b9797)
        static let neutral600 = SwiftUI.Color(hex: 0x7d7979)
        static let neutral700 = SwiftUI.Color(hex: 0x605d5d)
        static let neutral800 = SwiftUI.Color(hex: 0x444141)
        static let neutral900 = SwiftUI.Color(hex: 0x2d2b2b)

        // — accent ramp —
        static let accent100 = SwiftUI.Color(hex: 0xfff3e4)
        static let accent200 = SwiftUI.Color(hex: 0xffe3bf)
        static let accent300 = SwiftUI.Color(hex: 0xfacb8d)
        static let accent400 = SwiftUI.Color(hex: 0xe1ad66)
        static let accent500 = SwiftUI.Color(hex: 0xc28d41)
        static let accent600 = SwiftUI.Color(hex: 0xa06f24)
        static let accent700 = SwiftUI.Color(hex: 0x7d5411)
        static let accent800 = SwiftUI.Color(hex: 0x5a3b0a)
        static let accent900 = SwiftUI.Color(hex: 0x3a270d)

        // — accent-2 ramp —
        static let accent2_100 = SwiftUI.Color(hex: 0xfff3e4)
        static let accent2_200 = SwiftUI.Color(hex: 0xffe3be)
        static let accent2_300 = SwiftUI.Color(hex: 0xf5cd96)
        static let accent2_400 = SwiftUI.Color(hex: 0xdbaf70)
        static let accent2_500 = SwiftUI.Color(hex: 0xbc8f4e)
        static let accent2_600 = SwiftUI.Color(hex: 0x9b7232)
        static let accent2_700 = SwiftUI.Color(hex: 0x79561f)
        static let accent2_800 = SwiftUI.Color(hex: 0x573d14)
        static let accent2_900 = SwiftUI.Color(hex: 0x382810)

        // — interaction washes (DS `.btn-*:active` / `:hover`) —
        static let pressAccentStrong = accent.opacity(0.22)  // .btn-primary:active
        static let pressAccentSoft = accent.opacity(0.18)    // .btn-ghost:active
        static let pressInk = ink.opacity(0.14)              // .btn-secondary:active
        /// Dialog / sheet scrim — DESIGN.md 1d says neutral-900 @45%.
        static let scrim = neutral900.opacity(0.45)
    }
}

extension SwiftUI.Color {
    /// 0xRRGGBB in sRGB. Private to the design system by convention and by the
    /// `NoRawValuesTests` lint.
    fileprivate init(hex: UInt32) {
        self.init(
            .sRGB,
            red: Double((hex >> 16) & 0xff) / 255,
            green: Double((hex >> 8) & 0xff) / 255,
            blue: Double(hex & 0xff) / 255,
            opacity: 1
        )
    }
}

// MARK: - Font

extension Theme {
    /// DESIGN.md §1 Type. Two OFL families, bundled as static TTFs and
    /// registered through `UIAppFonts` (see NADE/Info.plist).
    ///
    /// Every style goes through `Font.custom(_:size:relativeTo:)`, so every
    /// piece of text honours Dynamic Type. `Font.custom(_:fixedSize:)` is
    /// banned here — it would freeze the UI at 100 %.
    enum Font {
        /// PostScript names, exactly as the TTFs report them.
        enum PostScriptName {
            static let headingRegular = "CormorantGaramond-Regular"
            static let headingSemibold = "CormorantGaramond-SemiBold"
            static let bodyRegular = "Lora-Regular"
            static let bodySemibold = "Lora-SemiBold"

            static let all = [headingRegular, headingSemibold, bodyRegular, bodySemibold]
        }

        /// The DS retired bold: 600 is the ceiling.
        enum Weight: Sendable { case regular, semibold }

        // — heading: Cormorant Garamond —
        // 600 for interface headings; 400 for display text ("Good morning",
        // the OTP code, the calendar day numeral).
        static func heading(
            _ size: CGFloat,
            weight: Weight = .semibold,
            relativeTo style: SwiftUI.Font.TextStyle? = nil
        ) -> SwiftUI.Font {
            let name = weight == .semibold
                ? PostScriptName.headingSemibold
                : PostScriptName.headingRegular
            return .custom(name, size: size, relativeTo: style ?? textStyle(for: size))
        }

        // — body: Lora —
        static func body(
            _ size: CGFloat,
            weight: Weight = .regular,
            relativeTo style: SwiftUI.Font.TextStyle? = nil
        ) -> SwiftUI.Font {
            let name = weight == .semibold
                ? PostScriptName.bodySemibold
                : PostScriptName.bodyRegular
            return .custom(name, size: size, relativeTo: style ?? textStyle(for: size))
        }

        /// SF Symbols. DESIGN.md §2 pins the tab-bar glyphs at
        /// `.system(size: 18, weight: .light)` to match the design's 1.8 stroke.
        /// The one legitimate `.system(` in the app — see `NIcon` for the
        /// Dynamic-Type-aware wrapper you should normally use.
        static func icon(_ size: CGFloat, weight: SwiftUI.Font.Weight = .light) -> SwiftUI.Font {
            .system(size: size, weight: weight)
        }

        /// Nearest Apple text style for a design size, so `relativeTo:` scales
        /// each style at the rate a reader expects. Covers the whole scale in
        /// DESIGN.md §Type (9.5 … 27 pt).
        static func textStyle(for size: CGFloat) -> SwiftUI.Font.TextStyle {
            switch size {
            case 31...: .largeTitle   // 34
            case 25...: .title        // 28  — 27/26/25 greeting + OTP
            case 21...: .title2       // 22  — 24/23/22 subject, screen title
            case 18.5...: .title3     // 20  — 20/19 sheet title, draft name
            case 16.5...: .body       // 17  — list item title
            case 15.5...: .callout    // 16  — sheet body
            case 14.25...: .subheadline // 15 — list primary, body base
            case 12.75...: .footnote  // 13  — 14.5/14/13.5/13 body + meta
            case 11.25...: .caption   // 12  — 12.5/12/11.5 captions
            default: .caption2        // 11  — 11/10.5/10/9.5 timestamps, eyebrows
            }
        }
    }
}

// MARK: - Line height

extension Theme {
    /// CSS `line-height`, as a multiple of the font size. These are the values
    /// the DS sets globally or on a component class; per-screen multiples
    /// (1.45, 1.6, 1.65, 1.7, 1.75, 1.85, 2) are passed at the call site.
    enum LineHeight {
        /// DS `body { line-height: 1.55 }` — inherited by *everything* that
        /// does not override it, which is why it also decides the height of
        /// inputs, tags, chips and segmented options.
        static let body: CGFloat = 1.55
        /// DS `.btn { line-height: 1.2 }`
        static let button: CGFloat = 1.2
        /// DS `.card-title { line-height: 1.2 }`
        static let cardTitle: CGFloat = 1.2
        /// DS `h1…h6 { line-height: 1.12 }`
        static let heading: CGFloat = 1.12
    }
}

// MARK: - Space

extension Theme {
    /// DESIGN.md §Space & shape. The DS scale is fractional on purpose
    /// (`--space-1: 4.6px`); do not round it.
    enum Space {
        static let s1: CGFloat = 4.6
        static let s2: CGFloat = 9.2
        static let s3: CGFloat = 13.8
        static let s4: CGFloat = 18.4
        static let s6: CGFloat = 27.6
        static let s8: CGFloat = 36.8

        /// Screen horizontal padding — 22 pt, 20 pt on thread/feed detail.
        static let screen: CGFloat = 22
        static let screenDetail: CGFloat = 20
    }
}

// MARK: - Radius

extension Theme {
    enum Radius {
        static let sm: CGFloat = 2
        static let md: CGFloat = 4
        static let lg: CGFloat = 7
        /// The DS uses `999px`; in SwiftUI a `Capsule` is the honest shape, but
        /// this value exists for `RoundedRectangle` sites that need a number.
        static let pill: CGFloat = 999
        /// `.tag` is `calc(var(--radius-md) * 0.75)` = 3.
        static let tag: CGFloat = md * 0.75
    }
}

// MARK: - Stroke

extension Theme {
    enum Stroke {
        /// Component outlines — buttons, inputs, cards, tags, chips, the
        /// segmented control. In this design colour *is* a stroke (there are no
        /// filled buttons), so these carry the whole visual weight and stay at
        /// a full point, exactly as DESIGN.md §Components specifies.
        /// Full-bleed *rules* use `Hairline`, which is one device pixel.
        static let border: CGFloat = 1
        /// `.radio .dot` — 1.5 pt ring.
        static let radioRing: CGFloat = 1.5
        /// Feed / notes rows carry a 2 pt accent left border.
        static let rule: CGFloat = 2
    }
}

// MARK: - Shadow

extension Theme {
    /// DESIGN.md §Color shadows. CSS blur radius is roughly twice SwiftUI's
    /// shadow radius, so `0 3px 10px` becomes `radius 5, y 3`.
    struct Shadow: Sendable {
        let color: SwiftUI.Color
        let radius: CGFloat
        let x: CGFloat
        let y: CGFloat

        static let sm = Shadow(color: Theme.Color.neutral900.opacity(0.14), radius: 1, x: 0, y: 1)
        static let md = Shadow(color: Theme.Color.neutral900.opacity(0.16), radius: 5, x: 0, y: 3)
        static let lg = Shadow(color: Theme.Color.neutral900.opacity(0.22), radius: 16, x: 0, y: 12)
    }
}

extension View {
    func nadeShadow(_ shadow: Theme.Shadow) -> some View {
        self.shadow(color: shadow.color, radius: shadow.radius, x: shadow.x, y: shadow.y)
    }
}

// MARK: - Motion

extension Theme {
    enum Motion {
        /// The toggle knob: `transition: left .18s ease`.
        /// EDGE (E7): returns `nil` under Reduce Motion, so the knob jumps.
        static func toggle(reduceMotion: Bool) -> Animation? {
            reduceMotion ? nil : .easeInOut(duration: 0.18)
        }
    }
}

// MARK: - Metrics

extension Theme {
    enum Metrics {
        /// EDGE (E1): chrome that cannot grow without breaking the layout
        /// (tab bar, toggle, stepper, chips) is clamped here. Content text is
        /// never clamped — it scales all the way to AX5.
        static let chromeTypeCeiling: DynamicTypeSize = .accessibility1

        /// EDGE (E17): the HIG minimum touch target. DESIGN.md never mentions
        /// it — the design draws a 30 × 30 stepper box, a 16 pt radio and a
        /// 46 × 26 switch — so every control that renders smaller than this
        /// carries an **invisible** target of this size via `.nadeHitTarget()`.
        /// The pixels stay the design's; only the tappable region grows.
        static let minimumHitTarget: CGFloat = 44

        /// Tab bar, DESIGN.md §2 + the mockup's `padding:9px 10px 26px`.
        ///
        /// The 26 is measured **from the bottom edge of the display**, not from
        /// above the home indicator (DESIGN.md §Safe area). The design frame's
        /// indicator region is 34 pt tall, so deferring to the safe-area inset
        /// and adding 8 put the labels 42 pt up — visibly higher than the
        /// design and inconsistent with every other bottom band. The bar
        /// therefore extends under the indicator and owns the full 26 itself.
        enum TabBar {
            static let iconSize: CGFloat = 18
            static let labelSize: CGFloat = 10.5
            static let labelTracking: CGFloat = 0.09   // em
            static let iconLabelGap: CGFloat = 5
            static let paddingTop: CGFloat = 9
            static let paddingHorizontal: CGFloat = 10
            static let paddingBottom: CGFloat = 26
        }
    }
}

// MARK: - Hairline

/// **One point** of `divider`, the full width of its container.
///
/// The mockup frame is 402 CSS px wide and the device is 402 pt, so the mapping
/// is 1 : 1 and every `border: 1px` in the mockup — including every rule — is
/// 1 pt. DESIGN.md §Space & shape says the same thing in words: "Hairline =
/// 1 pt `divider`". P1 originally rendered `1 / displayScale` and recorded it
/// as deviation D9; that reasoning was wrong and D9 now says why.
///
/// `Hairline` and `Theme.Stroke.border` are therefore the same weight. Both
/// exist because they mean different things (a full-bleed rule vs a component
/// outline) and a later phase may want to change one without the other.
struct Hairline: View {
    var color: Color = Theme.Color.divider

    var body: some View {
        Rectangle()
            .fill(color)
            .frame(height: Theme.Stroke.border)
            .accessibilityHidden(true)   // EDGE (E6): decorative
    }
}

/// The vertical twin, for dividers inside a row (the segmented control).
struct VHairline: View {
    var color: Color = Theme.Color.divider

    var body: some View {
        Rectangle()
            .fill(color)
            .frame(width: Theme.Stroke.border)
            .accessibilityHidden(true)
    }
}

// MARK: - Hit target

extension View {
    /// EDGE (E17): grows the **tappable** region to at least
    /// `Theme.Metrics.minimumHitTarget` without changing a single pixel or
    /// moving a neighbour.
    ///
    /// `contentShape` alone changes the *shape* of the hit region, never its
    /// size, and a plain `.frame(minWidth:minHeight:)` would push the design's
    /// rows apart (an ask-field row is 38–44 pt tall around a 38–44 pt circle;
    /// a 1d stepper row is 30). The target therefore rides in a `background`,
    /// which is laid out inside the parent's slot, is allowed to overflow it,
    /// and contributes nothing to the parent's measured size.
    ///
    /// Apply this **inside** a `Button`'s label so the taps route to the button.
    func nadeHitTarget(_ minimum: CGFloat = Theme.Metrics.minimumHitTarget) -> some View {
        background(alignment: .center) {
            Color.clear
                .frame(minWidth: minimum, minHeight: minimum)
                .contentShape(Rectangle())
        }
    }
}

// MARK: - Icon

/// An SF Symbol at a design point size that still honours Dynamic Type.
///
/// `Font.system(size:)` is a frozen size, so it is wrapped in `@ScaledMetric`
/// here. Everything in the app that draws a glyph goes through this.
struct NIcon: View {
    private let systemName: String
    private let weight: Font.Weight
    @ScaledMetric private var size: CGFloat

    init(
        _ systemName: String,
        size: CGFloat,
        weight: Font.Weight = .light,
        relativeTo style: Font.TextStyle = .body
    ) {
        self.systemName = systemName
        self.weight = weight
        self._size = ScaledMetric(wrappedValue: size, relativeTo: style)
    }

    var body: some View {
        Image(systemName: systemName)
            .font(Theme.Font.icon(size, weight: weight))
    }
}

// MARK: - Text helpers

extension View {
    /// `font-variant-numeric: tabular-nums`. Both bundled families ship a
    /// `tnum` feature, so this is a real substitution, not a no-op.
    /// DESIGN.md §Type: every timestamp, count and numeral.
    func tabularNumerals() -> some View {
        self.monospacedDigit()
    }

    /// CSS `letter-spacing` in **em**, scaled with Dynamic Type.
    /// Uppercase eyebrows are 0.09–0.10 em; uppercase meta 0.06–0.08 em.
    func nadeTracking(_ em: CGFloat, at size: CGFloat, relativeTo style: Font.TextStyle? = nil) -> some View {
        modifier(NTrackingModifier(em: em, size: size, style: style ?? Theme.Font.textStyle(for: size)))
    }

    /// CSS `line-height` as a multiple of the font size.
    ///
    /// SwiftUI has no line-height. `lineSpacing` is only the *gap between*
    /// lines: it does nothing to a single line, so on its own it cannot change
    /// a control's height, and it never makes a line box *tighter* than the
    /// face's own. The faithful construction is baseline-relative — take the
    /// face's real line box at the current Dynamic Type size, work out the
    /// delta to the CSS target, put the whole delta between the lines and half
    /// of it above and below the block. An `n`-line paragraph then measures
    /// exactly `n × size × multiple`, and a one-line control measures exactly
    /// `size × multiple` — which is what decides the height of an input, a tag,
    /// a chip and a segmented option in CSS.
    func nadeLineHeight(
        _ multiple: CGFloat,
        size: CGFloat,
        face: String = Theme.Font.PostScriptName.bodyRegular,
        relativeTo style: Font.TextStyle? = nil
    ) -> some View {
        modifier(NLineHeightModifier(
            multiple: multiple,
            size: size,
            face: face,
            style: style ?? Theme.Font.textStyle(for: size)
        ))
    }
}

private struct NTrackingModifier: ViewModifier {
    let em: CGFloat
    @ScaledMetric private var size: CGFloat

    init(em: CGFloat, size: CGFloat, style: Font.TextStyle) {
        self.em = em
        self._size = ScaledMetric(wrappedValue: size, relativeTo: style)
    }

    func body(content: Content) -> some View {
        content.tracking(size * em)
    }
}

struct NLineHeightModifier: ViewModifier {
    let multiple: CGFloat
    let face: String
    @ScaledMetric private var size: CGFloat

    init(multiple: CGFloat, size: CGFloat, face: String, style: Font.TextStyle) {
        self.multiple = multiple
        self.face = face
        self._size = ScaledMetric(wrappedValue: size, relativeTo: style)
    }

    /// The delta between the CSS line box and the face's own, at the *current*
    /// Dynamic Type size. Exposed so `NLineHeightTests` can assert the
    /// arithmetic against a measured render rather than re-deriving it.
    static func delta(multiple: CGFloat, size: CGFloat, face: String) -> CGFloat {
        // EDGE (E8): if the face is missing this falls back to a nominal 1.2
        // rather than crashing — the font-load test is what fails loudly.
        let natural = UIFont(name: face, size: size)?.lineHeight ?? size * 1.2
        return size * multiple - natural
    }

    func body(content: Content) -> some View {
        let delta = Self.delta(multiple: multiple, size: size, face: face)
        return content
            // The gap between lines is the whole delta …
            .lineSpacing(delta)
            // … and half of it caps the block top and bottom, so
            //   n·natural + (n−1)·delta + 2·(delta/2) = n·(natural + delta).
            // Negative padding is legal and is how a *tighter* line height is
            // expressed — `lineSpacing` alone cannot go below the face's box.
            .padding(.vertical, delta / 2)
    }
}

// MARK: - Section eyebrow

/// DESIGN.md: 10 pt uppercase with 0.1 em tracking. Accent for card kickers,
/// `ink62` for section eyebrows.
///
/// The case change is `.textCase(.uppercase)` (a rendering transform) rather
/// than `String.uppercased()`, so VoiceOver still reads "Accounts" and not
/// "A-C-C-O-U-N-T-S". EDGE (E6).
struct SectionEyebrow: View {
    private let title: String
    private let color: Color
    private let size: CGFloat
    private let tracking: CGFloat

    init(
        _ title: String,
        color: Color = Theme.Color.ink62,
        size: CGFloat = 10,
        tracking: CGFloat = 0.1
    ) {
        self.title = title
        self.color = color
        self.size = size
        self.tracking = tracking
    }

    var body: some View {
        Text(title)
            .textCase(.uppercase)
            .font(Theme.Font.body(size))
            .foregroundStyle(color)
            .nadeTracking(tracking, at: size)
            .fixedSize(horizontal: false, vertical: true)   // EDGE (E3): wraps, never clips
    }
}

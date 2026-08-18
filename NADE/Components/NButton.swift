//
//  NButton.swift
//  NADE
//
//  DESIGN.md §1 Components — Button; DS `.btn`, `.btn-primary`,
//  `.btn-secondary`, `.btn-ghost`, `.btn-icon`.
//
//  There are no filled buttons anywhere in this design. Colour is a stroke.
//

import SwiftUI

// MARK: - Style

struct NButtonStyle: ButtonStyle {
    enum Variant: Sendable {
        /// accent text + 1 pt accent border, transparent fill
        case primary
        /// ink text + 1 pt divider border
        case secondary
        /// accent text, no border, tight horizontal padding
        case ghost
        /// 36 × 36 glyph box with the accent stroke — the design's icon button
        case icon
    }

    enum Corner: Sendable {
        case rounded   // radius-md, 4 pt
        case pill      // the ask field's circular send button
    }

    /// CSS `flex: 1` vs the DS `.btn`'s `display: inline-flex`.
    ///
    /// 1c's "Run once now" and both of 1d's footer buttons carry `flex: 1`;
    /// every other button in the mockup is intrinsic. This has to live *inside*
    /// the style: `.frame(maxWidth: .infinity)` on the `Button` only stretches
    /// the wrapper, because the style has already sized its border and its
    /// press wash to the label.
    enum Width: Sendable {
        case intrinsic
        case flexible
    }

    var variant: Variant = .primary
    var corner: Corner = .rounded
    var width: Width = .intrinsic
    /// `.icon` only. DS `.btn-icon` is 36 × 36; the ask field's circular button
    /// is 38 (2a/1f), 40 (1a) or 44 (2b) — DESIGN.md §2.
    var iconBox: CGFloat = NButton.iconBox

    func makeBody(configuration: Configuration) -> some View {
        Surface(
            configuration: configuration,
            variant: variant,
            corner: corner,
            width: width,
            iconBox: iconBox
        )
    }

    // `ButtonStyle` is not a `View`, so `@Environment` has to be read from an
    // inner view. This is the only way a style can see `\.isEnabled`.
    private struct Surface: View {
        let configuration: Configuration
        let variant: Variant
        let corner: Corner
        let width: Width
        let iconBox: CGFloat

        @Environment(\.isEnabled) private var isEnabled

        private var foreground: Color {
            switch variant {
            case .primary, .ghost, .icon: Theme.Color.accent
            case .secondary: Theme.Color.ink
            }
        }

        private var border: Color? {
            switch variant {
            case .primary, .icon: Theme.Color.accent
            case .secondary: Theme.Color.divider
            case .ghost: nil
            }
        }

        /// DS `:active` washes. There is no fill at rest — the press *is* the fill.
        private var pressedFill: Color {
            switch variant {
            case .primary, .icon: Theme.Color.pressAccentStrong
            case .secondary: Theme.Color.pressInk
            case .ghost: Theme.Color.pressAccentSoft
            }
        }

        private var horizontalPadding: CGFloat {
            switch variant {
            case .primary, .secondary: Theme.Space.s3 * 1.2   // 16.56, DS `.btn`
            case .ghost: Theme.Space.s1                        // 4.6, DS `.btn-ghost`
            case .icon: 0
            }
        }

        /// CSS `border-radius` is a circular arc, so `.circular` — not the
        /// continuous squircle iOS reaches for by default.
        private var shape: RoundedRectangle {
            RoundedRectangle(
                cornerRadius: corner == .pill ? Theme.Radius.pill : Theme.Radius.md,
                style: .circular
            )
        }

        var body: some View {
            configuration.label
                .font(Theme.Font.heading(NButton.fontSize))
                .foregroundStyle(foreground)
                .padding(.horizontal, horizontalPadding)
                .padding(.vertical, variant == .icon ? 0 : Theme.Space.s2)
                .frame(
                    width: variant == .icon ? iconBox : nil,
                    height: variant == .icon ? iconBox : nil
                )
                // EDGE (E4): an empty title must not collapse the box.
                .frame(minHeight: variant == .icon ? nil : NButton.minHeight)
                // `flex: 1` — applied here, *before* the background and the
                // border, so the stroke and the press wash grow with it.
                .frame(maxWidth: width == .flexible ? .infinity : nil)
                .background(shape.fill(configuration.isPressed ? pressedFill : Color.clear))
                .overlay {
                    if let border {
                        shape.strokeBorder(border, lineWidth: Theme.Stroke.border)
                    }
                }
                .contentShape(shape)
                // EDGE (E17): a 36 pt icon box is under the 44 pt minimum.
                .nadeHitTarget()
                // DS `.btn:disabled { opacity: 0.45 }`
                .opacity(isEnabled ? 1 : NButton.disabledOpacity)
        }
    }
}

// MARK: - View

/// The button to reach for. Wraps `NButtonStyle` and enforces that no button —
/// icon-only or empty-titled — can reach VoiceOver without a name (EDGE E6).
struct NButton: View {
    static let fontSize: CGFloat = 14          // DS `.btn`
    static let minHeight: CGFloat = 36         // pairs with `.input`'s 36
    static let iconBox: CGFloat = 36           // DS `.btn-icon`
    static let disabledOpacity: Double = 0.45  // DS `.btn:disabled`
    static let labelGap: CGFloat = 6           // DS `.btn { gap: 6px }`
    /// 1a's `arrow.up`. The other three screens differ — DESIGN.md §2 pins
    /// 16 (2a feed, 1f), 17 (1a) and 18 (2a focus / 2b) — so every icon
    /// initialiser takes `glyph:`.
    static let glyphSize: CGFloat = 17

    /// EDGE (E1)/(E23): an icon button is a fixed box, so its glyph has to stop
    /// growing before it collides with the border — at AX5 a 17 pt glyph is
    /// ~41 pt and the box is 36. Text buttons are content and keep scaling all
    /// the way. Pure so `ThemeTests` can assert which variants clamp and
    /// `ComponentGeometryTests` can assert the clamped glyph still fits.
    static func clampsGlyph(for variant: NButtonStyle.Variant) -> Bool {
        variant == .icon
    }

    /// EDGE (E6): a control VoiceOver cannot name is a control it cannot use.
    /// An empty title is a layout edge case (E4), not a licence to ship an
    /// unlabelled element, so it falls back to the trait's own noun.
    static func resolvedAccessibilityLabel(title: String?, explicit: String?) -> String {
        if let explicit, !explicit.isEmpty { return explicit }
        if let title, !title.isEmpty { return title }
        return String(localized: "Button", comment: "Fallback VoiceOver name for a button with no visible title")
    }

    private let title: String?
    private let systemImage: String?
    private let glyphSize: CGFloat
    private let variant: NButtonStyle.Variant
    private let corner: NButtonStyle.Corner
    private let width: NButtonStyle.Width
    private let iconBox: CGFloat
    private let a11yLabel: String
    private let action: () -> Void

    /// Text button, optionally with a leading glyph.
    init(
        _ title: String,
        systemImage: String? = nil,
        variant: NButtonStyle.Variant = .primary,
        corner: NButtonStyle.Corner = .rounded,
        width: NButtonStyle.Width = .intrinsic,
        accessibilityLabel: String? = nil,
        action: @escaping () -> Void
    ) {
        self.title = title
        self.systemImage = systemImage
        self.glyphSize = NButton.fontSize   // a glyph beside a label sits at 14
        self.variant = variant
        self.corner = corner
        self.width = width
        self.iconBox = NButton.iconBox
        self.a11yLabel = NButton.resolvedAccessibilityLabel(title: title, explicit: accessibilityLabel)
        self.action = action
    }

    /// Icon-only button. `label` is required — a lone glyph is silent to
    /// VoiceOver otherwise — and `glyph` is the design's own point size for
    /// that screen (DESIGN.md §2: 16 / 17 / 18).
    init(
        systemImage: String,
        label: String,
        glyph: CGFloat = NButton.glyphSize,
        variant: NButtonStyle.Variant = .icon,
        corner: NButtonStyle.Corner = .rounded,
        box: CGFloat = NButton.iconBox,
        action: @escaping () -> Void
    ) {
        self.title = nil
        self.systemImage = systemImage
        self.glyphSize = glyph
        self.variant = variant
        self.corner = corner
        self.width = .intrinsic
        self.iconBox = box
        self.a11yLabel = NButton.resolvedAccessibilityLabel(title: nil, explicit: label)
        self.action = action
    }

    var body: some View {
        Button(action: action) {
            HStack(spacing: NButton.labelGap) {
                if let systemImage {
                    NIcon(
                        systemImage,
                        size: glyphSize,
                        weight: .light,
                        relativeTo: .subheadline
                    )
                }
                if let title, !title.isEmpty {
                    Text(title)
                        // EDGE (E3): a long label shrinks a little, then wraps,
                        // rather than pushing the row off-screen.
                        .lineLimit(2)
                        .minimumScaleFactor(0.75)
                        .multilineTextAlignment(.center)
                        .fixedSize(horizontal: false, vertical: true)
                        // DS `.btn { line-height: 1.2 }` — this is what sets
                        // the button's height, not `minHeight`.
                        .nadeLineHeight(
                            Theme.LineHeight.button,
                            size: NButton.fontSize,
                            face: Theme.Font.PostScriptName.headingSemibold
                        )
                }
            }
        }
        .buttonStyle(NButtonStyle(variant: variant, corner: corner, width: width, iconBox: iconBox))
        .accessibilityLabel(a11yLabel)
        .dynamicTypeSize(...(NButton.clampsGlyph(for: variant) ? Theme.Metrics.chromeTypeCeiling : .accessibility5))
    }
}

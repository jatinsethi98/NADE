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

    var variant: Variant = .primary
    var corner: Corner = .rounded
    /// `.icon` only. DS `.btn-icon` is 36 × 36; the ask field's circular send
    /// button is 38 (2a/1f), 40 (1a) or 44 (2b) — DESIGN.md §2.
    var iconBox: CGFloat = NButton.iconBox

    func makeBody(configuration: Configuration) -> some View {
        Surface(configuration: configuration, variant: variant, corner: corner, iconBox: iconBox)
    }

    // `ButtonStyle` is not a `View`, so `@Environment` has to be read from an
    // inner view. This is the only way a style can see `\.isEnabled`.
    private struct Surface: View {
        let configuration: Configuration
        let variant: Variant
        let corner: Corner
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
                .background(shape.fill(configuration.isPressed ? pressedFill : Color.clear))
                .overlay {
                    if let border {
                        shape.strokeBorder(border, lineWidth: Theme.Stroke.border)
                    }
                }
                .contentShape(shape)
                // DS `.btn:disabled { opacity: 0.45 }`
                .opacity(isEnabled ? 1 : NButton.disabledOpacity)
        }
    }
}

// MARK: - View

/// The button to reach for. Wraps `NButtonStyle` and enforces that an icon-only
/// button carries an accessibility label (EDGE E6).
struct NButton: View {
    static let fontSize: CGFloat = 14          // DS `.btn`
    static let minHeight: CGFloat = 36         // pairs with `.input`'s 36
    static let iconBox: CGFloat = 36           // DS `.btn-icon`
    static let disabledOpacity: Double = 0.45  // DS `.btn:disabled`
    static let labelGap: CGFloat = 6           // DS `.btn { gap: 6px }`
    static let glyphSize: CGFloat = 17         // the mockups' 17 pt send glyph

    private let title: String?
    private let systemImage: String?
    private let variant: NButtonStyle.Variant
    private let corner: NButtonStyle.Corner
    private let iconBox: CGFloat
    private let a11yLabel: String
    private let action: () -> Void

    /// Text button, optionally with a leading glyph.
    init(
        _ title: String,
        systemImage: String? = nil,
        variant: NButtonStyle.Variant = .primary,
        corner: NButtonStyle.Corner = .rounded,
        action: @escaping () -> Void
    ) {
        self.title = title
        self.systemImage = systemImage
        self.variant = variant
        self.corner = corner
        self.iconBox = NButton.iconBox
        self.a11yLabel = title
        self.action = action
    }

    /// Icon-only button. `label` is required — a lone glyph is silent to
    /// VoiceOver otherwise.
    init(
        systemImage: String,
        label: String,
        variant: NButtonStyle.Variant = .icon,
        corner: NButtonStyle.Corner = .rounded,
        box: CGFloat = NButton.iconBox,
        action: @escaping () -> Void
    ) {
        self.title = nil
        self.systemImage = systemImage
        self.variant = variant
        self.corner = corner
        self.iconBox = box
        self.a11yLabel = label
        self.action = action
    }

    var body: some View {
        Button(action: action) {
            HStack(spacing: NButton.labelGap) {
                if let systemImage {
                    // A glyph beside a label sits at the label's 14 pt; a glyph
                    // on its own is the mockups' 17 pt send arrow.
                    NIcon(
                        systemImage,
                        size: title == nil ? NButton.glyphSize : NButton.fontSize,
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
                }
            }
        }
        .buttonStyle(NButtonStyle(variant: variant, corner: corner, iconBox: iconBox))
        .accessibilityLabel(a11yLabel)
    }
}

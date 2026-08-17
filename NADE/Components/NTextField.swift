//
//  NTextField.swift
//  NADE
//
//  DESIGN.md §1 Components — Input; DS `.input`.
//  1 pt divider border, radius 4 (pill 999 for the ask field), min-height
//  36–44, 14 pt, caret accent, focus border → accent.
//

import SwiftUI

struct NTextField: View {
    enum Style: Sendable {
        /// DS `.input` — radius-md, padding 6 × 10, min-height 36.
        case rounded
        /// The ask field — radius 999, padding 9 × 15, min-height 38/40/44.
        case pill
    }

    static let fontSize: CGFloat = 14

    private let placeholder: String
    @Binding private var text: String
    private let style: Style
    private let minHeight: CGFloat
    /// Gallery / preview only: draws the focus ring without stealing focus.
    private let showsFocusRing: Bool

    @FocusState private var isFocused: Bool

    init(
        _ placeholder: String,
        text: Binding<String>,
        style: Style = .rounded,
        minHeight: CGFloat? = nil,
        showsFocusRing: Bool = false
    ) {
        self.placeholder = placeholder
        self._text = text
        self.style = style
        self.minHeight = minHeight ?? (style == .pill ? 40 : 36)
        self.showsFocusRing = showsFocusRing
    }

    private var focused: Bool { isFocused || showsFocusRing }

    private var horizontalPadding: CGFloat { style == .pill ? 15 : 10 }
    private var verticalPadding: CGFloat { style == .pill ? 9 : 6 }

    private var shape: RoundedRectangle {
        RoundedRectangle(
            cornerRadius: style == .pill ? Theme.Radius.pill : Theme.Radius.md,
            style: .circular
        )
    }

    var body: some View {
        TextField("", text: $text)
            .font(Theme.Font.body(Self.fontSize))
            .foregroundStyle(Theme.Color.ink)
            // DS `caret-color: var(--color-accent)`
            .tint(Theme.Color.accent)
            .textFieldStyle(.plain)
            .focused($isFocused)
            // The placeholder is drawn rather than passed to `prompt:` so its
            // colour and face match the design exactly; the field keeps its
            // spoken label via `.accessibilityLabel` below. EDGE (E4), (E6).
            .overlay(alignment: .leading) {
                if text.isEmpty {
                    Text(placeholder)
                        .font(Theme.Font.body(Self.fontSize))
                        .foregroundStyle(Theme.Color.ink62)
                        .lineLimit(1)
                        .truncationMode(.tail)   // EDGE (E3)
                        .allowsHitTesting(false)
                        .accessibilityHidden(true)
                }
            }
            .padding(.horizontal, horizontalPadding)
            .padding(.vertical, verticalPadding)
            .frame(minHeight: minHeight)
            .background(shape.fill(Color.clear))
            .overlay {
                shape.strokeBorder(
                    focused ? Theme.Color.accent : Theme.Color.divider,
                    lineWidth: Theme.Stroke.border
                )
            }
            .contentShape(shape)
            .accessibilityLabel(placeholder)
    }
}

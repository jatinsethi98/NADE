//
//  NCard.swift
//  NADE
//
//  DESIGN.md §1 Components — Card; DS `.card`, `.card-kicker`, `.card-title`,
//  `.card-body`, `.card-meta`.
//  1 pt border, radius 4, padding 13.8, transparent fill, 9.2 between slots.
//

import SwiftUI

/// Card padding, by card. Free-standing rather than nested in `NCard` because
/// `NCard` is generic and Swift has no static stored properties there.
enum NCardPadding {
    /// DS `.card` — `padding: var(--space-3)` on all four sides.
    static let ds = EdgeInsets(
        top: Theme.Space.s3, leading: Theme.Space.s3,
        bottom: Theme.Space.s3, trailing: Theme.Space.s3
    )
    /// 1a's draft-agent card — DESIGN.md §3 1a: padding `15 / 15 / 13`, and it
    /// **closes before its buttons**.
    static let draftAgent = EdgeInsets(top: 15, leading: 15, bottom: 13, trailing: 15)
    /// 1f's agent card — DESIGN.md §3 1f: padding `14 / 15`, buttons inside.
    static let agentCard = EdgeInsets(top: 14, leading: 15, bottom: 14, trailing: 15)
}

struct NCard<Content: View>: View {
    enum Border: Sendable {
        case divider   // DS default
        case accent    // the draft-agent card (1a) and the thread agent card (1f)
    }

    private let border: Border
    private let padding: EdgeInsets
    private let spacing: CGFloat
    private let content: Content

    init(
        border: Border = .divider,
        padding: EdgeInsets = NCardPadding.ds,
        spacing: CGFloat = Theme.Space.s2,
        @ViewBuilder content: () -> Content
    ) {
        self.border = border
        self.padding = padding
        self.spacing = spacing
        self.content = content()
    }

    private var strokeColor: Color {
        switch border {
        case .divider: Theme.Color.divider
        case .accent: Theme.Color.accent
        }
    }

    private var shape: RoundedRectangle {
        RoundedRectangle(cornerRadius: Theme.Radius.md, style: .circular)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: spacing) {
            content
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(padding)
        .background(shape.fill(Color.clear))
        .overlay { shape.strokeBorder(strokeColor, lineWidth: Theme.Stroke.border) }
    }
}

extension NCard where Content == NCardSlots {
    /// The common case: kicker / title / body / meta, any of them omitted.
    init(
        kicker: String? = nil,
        title: String? = nil,
        body: String? = nil,
        meta: String? = nil,
        border: Border = .divider
    ) {
        self.init(border: border) {
            NCardSlots(kicker: kicker, title: title, body: body, meta: meta)
        }
    }
}

// MARK: - Slots

struct NCardSlots: View {
    let kicker: String?
    let title: String?
    let bodyText: String?
    let meta: String?

    init(kicker: String?, title: String?, body: String?, meta: String?) {
        self.kicker = kicker
        self.title = title
        self.bodyText = body
        self.meta = meta
    }

    var body: some View {
        // EDGE (E6): the four lines read as one element to VoiceOver rather
        // than four disconnected fragments.
        VStack(alignment: .leading, spacing: Theme.Space.s2) {
            if let kicker { NCardKicker(kicker) }
            if let title { NCardTitle(title) }
            if let bodyText { NCardBody(bodyText) }
            if let meta { NCardMeta(meta) }
        }
        .accessibilityElement(children: .combine)
    }
}

/// `.card-kicker` — 10 pt uppercase accent, 0.1 em.
struct NCardKicker: View {
    private let text: String
    private let color: Color

    init(_ text: String, color: Color = Theme.Color.accent) {
        self.text = text
        self.color = color
    }

    var body: some View {
        SectionEyebrow(text, color: color)
    }
}

/// `.card-title` — Cormorant 600, 17 pt, line-height 1.2.
struct NCardTitle: View {
    private let text: String
    private let size: CGFloat

    init(_ text: String, size: CGFloat = 17) {
        self.text = text
        self.size = size
    }

    var body: some View {
        Text(text)
            .font(Theme.Font.heading(size))
            .foregroundStyle(Theme.Color.ink)
            .nadeLineHeight(
                Theme.LineHeight.cardTitle,
                size: size,
                face: Theme.Font.PostScriptName.headingSemibold
            )
            // EDGE (E3): long unbroken titles wrap instead of clipping.
            .fixedSize(horizontal: false, vertical: true)
    }
}

/// `.card-body` — 13 pt at 80 % opacity.
struct NCardBody: View {
    private let text: String
    private let size: CGFloat

    init(_ text: String, size: CGFloat = 13) {
        self.text = text
        self.size = size
    }

    var body: some View {
        Text(text)
            .font(Theme.Font.body(size))
            .foregroundStyle(Theme.Color.ink80)
            .nadeLineHeight(Theme.LineHeight.body, size: size)
            .fixedSize(horizontal: false, vertical: true)   // EDGE (E3)
    }
}

/// `.card-meta` — 11 pt `ink50`, tabular.
struct NCardMeta: View {
    private let text: String
    private let size: CGFloat

    init(_ text: String, size: CGFloat = 11) {
        self.text = text
        self.size = size
    }

    var body: some View {
        Text(text)
            .font(Theme.Font.body(size))
            .foregroundStyle(Theme.Color.ink50)
            .tabularNumerals()
            .fixedSize(horizontal: false, vertical: true)   // EDGE (E3)
    }
}

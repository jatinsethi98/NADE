//
//  ThreadView.swift
//  NADE
//
//  Screen **1f**, the one pushed detail that carries no tab bar.
//
//  Two things DESIGN.md gets to be corrected on, both checked against the
//  mockup's markup rather than argued:
//
//  * Its "12 pt gaps between messages" is really a **paragraph** gap. In the
//    mockup that `margin-top: 12` sits between two `<div>`s of *one* message —
//    both halves of Priya's first mail. The mockup draws one message and has no
//    separator between two.
//  * The meta row binds to `messages.first`. The mockup's `9:12` is
//    `thread.json`'s `messages[0].ts` (`09:12:04Z`), and its body is message
//    one's text. Messages two onward repeat the same `meta → hr → body` unit
//    at the same numbers — no new visual vocabulary, just the design's own,
//    applied again.
//

import QuickLook
import SwiftUI

struct ThreadView: View {

    enum Metrics {
        // Nav `60 / 20 / 10` + divider, centred, gap 12, 13 pt.
        static let navTop: CGFloat = 60
        static let navBottom: CGFloat = 10
        static let navSize: CGFloat = 13

        // Body scroll padding `20 / 22 / 8`.
        static let bodyTop: CGFloat = 20
        static let bodyBottom: CGFloat = 8

        static let subjectSize: CGFloat = 24
        static let subjectLineHeight: CGFloat = 1.2

        /// DESIGN.md's own subject→meta gap. Subsequent messages open a new
        /// block instead, at the 20 the design uses for the agent card.
        static let firstMetaTop: CGFloat = 10
        static let laterMetaTop: CGFloat = 20
        static let metaGap: CGFloat = 9
        static let metaSize: CGFloat = 13

        static let ruleMargin: CGFloat = 14

        static let bodySize: CGFloat = 14.5
        static let bodyLineHeight: CGFloat = 1.75
        static let paragraphGap: CGFloat = 12

        static let attachmentsTop: CGFloat = 14
        static let attachmentsGap: CGFloat = 9
        static let attachmentSizeText: CGFloat = 11

        static let cardTop: CGFloat = 20

        static let footerRuleTop: CGFloat = 20
        static let footerRuleBottom: CGFloat = 8
        static let footerBottom: CGFloat = 10

        // Bottom bar: top divider, `11 / 20 / 30`, gap 10.
        static let barTop: CGFloat = 11
        static let barBottom: CGFloat = 30
        static let barGap: CGFloat = 10
        /// The field's own numbers come from `NTextField.Metrics.askThread` —
        /// the preset whose doc comment names 1f as its only screen. Re-typing
        /// them would leave two copies of 1f's geometry, and the picture the
        /// screenshots check would be the copy that P6 does *not* replace.
        static let barField = NTextField.Metrics.askThread
        static let barCircle: CGFloat = 38
        static let barGlyph: CGFloat = 16
    }

    @Environment(AppNavigation.self) private var navigation
    let model: ThreadModel
    let threadID: String
    /// Which list this thread was opened from — see `MailSync.loadThread`.
    let mailboxID: String
    /// What the back affordance reads before the detail's `mailbox_name`
    /// arrives. Without it the nav bar is a lone "‹" for as long as the fetch
    /// takes, which on a slow connection is a back button that looks broken.
    let backTitle: String
    let now: Date

    /// Which messages have their original open. The one genuinely view-shaped
    /// piece of state here — the download's lifecycle lives on `ThreadModel`.
    @State private var showsOriginal: Set<String> = []

    private var subject: String {
        model.thread?.subject ?? model.row?.subject ?? ""
    }

    var body: some View {
        VStack(spacing: 0) {
            body(for: model.thread)
            // Stays a sibling, and takes the glass without taking the overlay.
            // `Metrics.barBottom` is 30 measured from the display edge — see
            // `ChromeBar` for why that rules out `safeAreaInset` here.
            askBarChrome.nadeChromeBar()
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
            // Nav bar and ask band are chrome, so both float over the thread
            // (D98). This is the screen where the change reads loudest: 1f is
            // the app's longest scroll, and a message now fades out under each
            // bar rather than being cut off at a hairline.
            //
            // 1f carries no tab bar — `MailRoute.hidesTabBar`, applied in
            // `MailTabRoot` — so the bottom bar is the only thing down there and
            // does not stack glass on glass.
            .safeAreaInset(edge: .top, spacing: 0) {
                ThreadNavBar(title: model.thread?.mailboxName ?? backTitle) {
                    navigation.popMail()
                }
                .nadeChromeBar()
            }
            .task(id: threadID) { await model.load(id: threadID, in: mailboxID) }
            .quickLookPreview(Binding(get: { model.previewURL },
                                      set: { model.previewURL = $0 }))
            .onDisappear { model.stopAttachments() }
            .alert(model.attachmentProblem ?? "",
                   isPresented: Binding(get: { model.attachmentProblem != nil },
                                        set: { if !$0 { model.dismissAttachmentProblem() } })) {
                Button(String(localized: "OK", comment: "Dismisses an attachment error")) {
                    model.dismissAttachmentProblem()
                }
            }
    }

    // MARK: Body

    @ViewBuilder
    private func body(for thread: WireThread?) -> some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 0) {
                Text(subject)
                    .font(Theme.Font.heading(Metrics.subjectSize))
                    .foregroundStyle(Theme.Color.ink)
                    .fixedSize(horizontal: false, vertical: true)
                    .nadeLineHeight(Metrics.subjectLineHeight, size: Metrics.subjectSize,
                                    face: Theme.Font.PostScriptName.headingSemibold)
                    .accessibilityAddTraits(.isHeader)
                    .accessibilityIdentifier("thread.subject")

                // EDGE (P12/P13). Two slots, not one: `API.md` §2 says `partial`
                // is *produced by* an upstream failure, so a thread with gaps
                // and a connection that is currently down are routinely true at
                // the same time. One string would overwrite one of them.
                if model.isPartial {
                    StateCaption(StateCopy.partialThread, alignment: .leading,
                                 color: Theme.Color.ink62)
                        .padding(.top, Metrics.firstMetaTop)
                        .accessibilityIdentifier("thread.partial")
                }
                if let problem = model.connectionProblem {
                    StateCaption(problem.message, alignment: .leading, color: Theme.Color.ink62)
                        .padding(.top, Metrics.firstMetaTop)
                        .accessibilityIdentifier("thread.problem")
                }

                if let thread {
                    ForEach(Array(thread.messages.enumerated()), id: \.element.gmailId) { index, message in
                        ThreadMessageBlock(
                            message: message,
                            accountEmail: thread.accountEmail,
                            now: now,
                            topGap: index == 0 ? Metrics.firstMetaTop : Metrics.laterMetaTop,
                            showsOriginal: $showsOriginal,
                            onOpenAttachment: { gmailID, attachment in
                                model.openAttachment(gmailID: gmailID, attachment: attachment)
                            },
                            attachmentSource: model.attachmentSource
                        )
                    }

                    ForEach(thread.agentCards, id: \.runId) { card in
                        ThreadAgentCard(card: card)
                            .padding(.top, Metrics.cardTop)
                    }

                    Hairline()
                        .padding(.top, Metrics.footerRuleTop)
                        .padding(.bottom, Metrics.footerRuleBottom)

                    StateCaption("Filed in \(thread.mailboxName) · \(thread.accountEmail)",
                                 alignment: .leading, color: Theme.Color.ink62)
                        .padding(.bottom, Metrics.footerBottom)
                        .accessibilityIdentifier("thread.footer")
                }
            }
            .padding(.top, Metrics.bodyTop)
            .padding(.horizontal, Theme.Space.screen)
            .padding(.bottom, Metrics.bodyBottom)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
    }

    // MARK: The bottom bar

    /// **Chrome, not a control.** `POST /ask` is P6, and DESIGN.md §4 cuts an
    /// affordance whose backing is missing rather than disabling it.
    ///
    /// What makes rendering it correct rather than a violation is that the
    /// mockup's own bar is not a control either: 1f's field is a `<span>` and
    /// its circle is a `<span>`, where the agent card's three buttons are real
    /// `<button>`s — which is why *those* are cut. So this is a picture of a
    /// bar, exactly as the mockup is a picture of a bar: nothing to focus,
    /// nothing to type into, nothing to tap, and hidden from VoiceOver. P6
    /// replaces it with `NTextField.Metrics.askThread` and a live `NButton`.
    private var askBarChrome: some View {
        HStack(spacing: Metrics.barGap) {
            Text("Reply, or ask for a draft…")
                .font(Theme.Font.body(Metrics.barField.fontSize))
                .foregroundStyle(Theme.Color.ink62)
                .lineLimit(1)
                .nadeLineHeight(Theme.LineHeight.body, size: Metrics.barField.fontSize)
                .padding(.vertical, Metrics.barField.paddingV)
                .padding(.horizontal, Metrics.barField.paddingH)
                .frame(maxWidth: .infinity, alignment: .leading)
                .overlay { Capsule().strokeBorder(Theme.Color.divider, lineWidth: Theme.Stroke.border) }

            NIcon("sparkles", size: Metrics.barGlyph)
                .foregroundStyle(Theme.Color.accent)
                .frame(width: Metrics.barCircle, height: Metrics.barCircle)
                .overlay { Circle().strokeBorder(Theme.Color.accent, lineWidth: Theme.Stroke.border) }
        }
        .padding(.top, Metrics.barTop)
        .padding(.horizontal, Theme.Space.screenDetail)
        .padding(.bottom, Metrics.barBottom)
        // The whole band is decorative until P6. Nothing here announces itself
        // as a control, because nothing here is one.
        .accessibilityHidden(true)
        .allowsHitTesting(false)
    }
}

// MARK: - One message

struct ThreadMessageBlock: View {
    private typealias M = ThreadView.Metrics

    let message: WireMessage
    let accountEmail: String
    let now: Date
    let topGap: CGFloat
    /// Which messages have their original open. Owned by `ThreadView` rather
    /// than by each block, so a re-render of the list does not collapse one.
    @Binding var showsOriginal: Set<String>
    let onOpenAttachment: (String, WireAttachment) -> Void
    /// Where "View original" gets its inline parts.
    let attachmentSource: any MailSource

    /// Split once, at construction. As a computed property this ran on every
    /// `body` evaluation — and `ThreadView` lays messages out in a plain
    /// `VStack`, not a lazy one, so every message in the thread paid it on
    /// every render, not just the visible ones.
    private let paragraphs: [String]

    init(message: WireMessage, accountEmail: String, now: Date, topGap: CGFloat,
         showsOriginal: Binding<Set<String>>,
         onOpenAttachment: @escaping (String, WireAttachment) -> Void,
         attachmentSource: any MailSource) {
        self.message = message
        self.accountEmail = accountEmail
        self.now = now
        self.topGap = topGap
        self._showsOriginal = showsOriginal
        self.onOpenAttachment = onOpenAttachment
        self.attachmentSource = attachmentSource
        self.paragraphs = Self.paragraphs(of: message.bodyText)
    }

    /// Only when the account is genuinely a recipient. `thread_html_only.json`
    /// has `to: []` — undisclosed recipients — and inventing "to me" there
    /// would be the server's job, which it declined.
    private var addressedToMe: Bool {
        message.to.contains { $0.caseInsensitiveCompare(accountEmail) == .orderedSame }
    }

    /// The mockup's body is two `<div>`s of one message, separated by 12 pt.
    /// That is a paragraph gap, and this is what produces it.
    ///
    /// The split is the **only** thing done to `body_text`. An earlier version
    /// also trimmed each part and dropped empty ones, which quietly rewrote the
    /// mail: leading indentation in pasted code, a deliberately blank stanza,
    /// and trailing whitespace all disappeared. The parser on the other side
    /// went to some trouble to produce this text faithfully; the renderer does
    /// not get to edit it.
    ///
    /// A message that is entirely blank still renders one empty paragraph
    /// rather than nothing, so the meta row and rule do not float over a gap.
    static func paragraphs(of bodyText: String) -> [String] {
        let parts = bodyText.components(separatedBy: "\n\n")
        return parts.isEmpty ? [""] : parts
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack(alignment: .firstTextBaseline, spacing: M.metaGap) {
                Text(message.senderDisplayName)
                    .foregroundStyle(Theme.Color.ink)
                if addressedToMe {
                    Text("· to me").foregroundStyle(Theme.Color.ink60)
                }
                Spacer(minLength: Theme.Space.s2)
                Text(ListTime.string(for: message.ts, now: now))
                    .foregroundStyle(Theme.Color.ink60)
                    .tabularNumerals()
            }
            .font(Theme.Font.body(M.metaSize))
            .lineLimit(1)
            .nadeLineHeight(Theme.LineHeight.body, size: M.metaSize)
            .padding(.top, topGap)

            Hairline()
                .padding(.vertical, M.ruleMargin)

            // `v1 →` the mockup justifies with hyphenation. SwiftUI has no
            // justified alignment without a custom `TextRenderer`, so this is
            // leading — the deviation IOS_DECISIONS said P2 would resolve.
            VStack(alignment: .leading, spacing: M.paragraphGap) {
                ForEach(Array(paragraphs.enumerated()), id: \.offset) { _, paragraph in
                    Text(paragraph)
                        .font(Theme.Font.body(M.bodySize))
                        .foregroundStyle(Theme.Color.ink)
                        .multilineTextAlignment(.leading)
                        .fixedSize(horizontal: false, vertical: true)
                        .nadeLineHeight(M.bodyLineHeight, size: M.bodySize)
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)

            if !message.attachments.isEmpty {
                // **Tappable from P3.** The proxy shipped in P2 and the tap
                // lands here; the mockup draws no `onClick`, but a listed
                // attachment that cannot be opened is a control that does
                // nothing (DESIGN.md §4).
                //
                // **Inline parts are listed too.** `inline: true` marks a part
                // the HTML references with `cid:`, which "View original" now
                // renders — but the CSP there allows `cid:` and nothing else,
                // and a reader who wants the file itself still needs a way to
                // reach it. Deviation 47 stays closed by keeping both.
                //
                // The row scrolls: the mockup draws one attachment, real mail
                // carries several, and two long filenames overflow 375 pt
                // (EDGE E2/E3).
                ScrollView(.horizontal, showsIndicators: false) {
                    HStack(spacing: M.attachmentsGap) {
                        ForEach(message.attachments, id: \.id) { attachment in
                            Button {
                                onOpenAttachment(message.gmailId, attachment)
                            } label: {
                                HStack(spacing: M.attachmentsGap) {
                                    NTag(attachment.name, style: .neutral)
                                    Text(AttachmentSize.string(bytes: attachment.size))
                                        .font(Theme.Font.body(M.attachmentSizeText))
                                        .foregroundStyle(Theme.Color.ink62)
                                        .tabularNumerals()
                                }
                                .contentShape(Rectangle())
                            }
                            .buttonStyle(.plain)
                            .accessibilityIdentifier("thread.attachment.\(attachment.id)")
                        }
                    }
                }
                .padding(.top, M.attachmentsTop)
            }

            // "View original" — only when there is an original to view.
            // `body_html` is null on plain-text mail, and a button that opened
            // an empty page would be worse than no button.
            if let html = message.bodyHtml, !html.isEmpty {
                Button {
                    if showsOriginal.contains(message.gmailId) {
                        showsOriginal.remove(message.gmailId)
                    } else {
                        showsOriginal.insert(message.gmailId)
                    }
                } label: {
                    Text(showsOriginal.contains(message.gmailId) ? "Hide original" : "View original")
                        .font(Theme.Font.body(M.metaSize))
                        .foregroundStyle(Theme.Color.accent)
                        .padding(.top, M.attachmentsTop)
                        .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .accessibilityIdentifier("thread.vieworiginal.\(message.gmailId)")

                if showsOriginal.contains(message.gmailId) {
                    OriginalMessageView(html: html,
                                        source: attachmentSource,
                                        gmailID: message.gmailId)
                }
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

// MARK: - The agent card

struct ThreadAgentCard: View {

    let card: WireAgentCard

    /// DESIGN.md §1f's table. The mockup shows only "waiting on you"; the API
    /// returns all eight, so the rest are `+ Added` for completeness.
    static func statusText(_ status: RunStatus) -> String {
        switch status {
        case .queued: String(localized: "queued", comment: "Agent card status")
        case .running: String(localized: "running", comment: "Agent card status")
        case .pendingApproval: String(localized: "waiting on you", comment: "Agent card status")
        case .waiting: String(localized: "scheduled", comment: "Agent card status")
        case .done: String(localized: "done", comment: "Agent card status")
        case .failed: String(localized: "failed", comment: "Agent card status")
        case .expired: String(localized: "expired", comment: "Agent card status")
        case .skipped: String(localized: "skipped", comment: "Agent card status")
        case .unknown(let raw): raw
        }
    }

    enum Metrics {
        static let kickerBottom: CGFloat = 9
        static let statusSize: CGFloat = 11
        static let summarySize: CGFloat = 14
        static let summaryLineHeight: CGFloat = 1.7
    }

    var body: some View {
        NCard(border: .accent, padding: NCardPadding.agentCard, spacing: 0) {
            HStack(alignment: .firstTextBaseline, spacing: Theme.Space.s2) {
                // `NCardKicker` *is* 10 pt uppercase accent at 0.1 em — the
                // DS's card kicker, which is what DESIGN.md §1f asks for.
                NCardKicker(card.agentName)
                Spacer(minLength: Theme.Space.s2)
                Text(Self.statusText(card.status))
                    .font(Theme.Font.body(Metrics.statusSize))
                    .foregroundStyle(Theme.Color.ink62)
            }
            .padding(.bottom, Metrics.kickerBottom)

            Text(card.summary)
                .font(Theme.Font.body(Metrics.summarySize))
                .foregroundStyle(Theme.Color.ink)
                .fixedSize(horizontal: false, vertical: true)
                .nadeLineHeight(Metrics.summaryLineHeight, size: Metrics.summarySize)
                .frame(maxWidth: .infinity, alignment: .leading)

            // **No buttons.** DESIGN.md §1f: they render only when the run is
            // `pending_approval` *and* `feed_item_id != null`, and their labels
            // come from `GET /feed/{id}`'s `data.action_label` — "Save note",
            // "Save draft". That endpoint arrives at P5. Rendering a literal
            // "Approve" in the meantime is exactly what §4 and PLAN C1/C2
            // forbid, because it would promise an outbound action v1 does not
            // take.
        }
        .accessibilityElement(children: .combine)
        .accessibilityIdentifier("thread.agentcard.\(card.runId)")
    }
}


/// 1f's nav band, lifted out of the screen so `MailGeometryTests` can measure
/// the `60 / 20 / 10` the design specifies rather than take the constants'
/// word for it (D33/D49). It was several points short before that measurement
/// existed: the title had no explicit line box.
struct ThreadNavBar: View {
    let title: String
    let back: () -> Void

    var body: some View {
        HStack(alignment: .center, spacing: Theme.Space.s3) {
            Button {
                back()
            } label: {
                // `v1 →` the mockup's right-hand "Archive" and "⋯" are cut:
                // v1 performs no Gmail mutation, so Archive has nothing to call
                // and the menu would be empty. The bar is the back affordance.
                Text(verbatim: "‹ \(title)")
                    .font(Theme.Font.body(ThreadView.Metrics.navSize))
                    .foregroundStyle(Theme.Color.accent)
                    .lineLimit(1)
                    .truncationMode(.tail)
                    .nadeLineHeight(Theme.LineHeight.body, size: ThreadView.Metrics.navSize)
                    .nadeHitTarget()
            }
            .buttonStyle(.plain)
            .accessibilityLabel(String(localized: "Back to \(title.isEmpty ? "mail" : title)",
                                       comment: "Back button on the thread screen"))
            .accessibilityIdentifier("thread.back")

            Spacer(minLength: 0)
        }
        .padding(.top, ThreadView.Metrics.navTop)
        .padding(.horizontal, Theme.Space.screenDetail)
        .padding(.bottom, ThreadView.Metrics.navBottom)
    }
}

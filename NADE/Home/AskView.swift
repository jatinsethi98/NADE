//
//  AskView.swift
//  NADE
//
//  1a (`DESIGN.md` §1a) — the three query states, chosen by the SSE `route`
//  event, which `API.md` §4 guarantees arrives first.
//
//  `v1 →` **1a's idle state is not built.** 2a's focus state replaced it
//  (`DESIGN.md` §3), so this screen only ever exists over a submitted query.
//

import SwiftUI

struct AskView: View {

    private enum M {
        static let headerTop: CGFloat = 62
        static let horizontal = Theme.Space.screen
        static let headerBottom: CGFloat = 8
        static let greetingSize: CGFloat = 27
        static let dateSize: CGFloat = 11
        static let dateTracking: CGFloat = 0.08

        static let scrollTop: CGFloat = 14
        static let scrollBottom: CGFloat = 8

        static let bubbleMaxWidth: CGFloat = 0.78
        static let draftBubbleMaxWidth: CGFloat = 0.80
        static let bubblePadV: CGFloat = 9
        static let bubblePadH: CGFloat = 13
        static let bubbleSize: CGFloat = 14
        static let bubbleBottom: CGFloat = 16

        static let proseSize: CGFloat = 14
        static let proseLineHeight: CGFloat = 1.65
        static let proseBottom: CGFloat = 14

        static let hitRowV: CGFloat = 11
        static let hitNameSize: CGFloat = 14
        static let hitTimeSize: CGFloat = 11
        static let hitSubjectSize: CGFloat = 13
        static let hitSubjectTop: CGFloat = 2

        static let sourceRowV: CGFloat = 11
        static let sourceSize: CGFloat = 13

        static let buttonsTop: CGFloat = 16
        static let buttonsGap: CGFloat = 8

        static let cardPadTop: CGFloat = 15
        static let cardPadH: CGFloat = 15
        static let cardPadBottom: CGFloat = 13
        static let kickerSize: CGFloat = 10
        static let kickerTracking: CGFloat = 0.1
        static let kickerBottom: CGFloat = 10
        static let cardNameSize: CGFloat = 19
        static let cardNameBottom: CGFloat = 12
        static let cardSentenceSize: CGFloat = 15
        static let cardSentenceLineHeight: CGFloat = 1.85
        static let tagTop: CGFloat = 14
        static let tagGap: CGFloat = 6
        static let savedSize: CGFloat = 13
        static let savedTop: CGFloat = 14

        static let dockTop: CGFloat = 10
        static let dockH: CGFloat = 18
        static let dockBottom: CGFloat = 12
    }

    @Environment(AppNavigation.self) private var navigation

    /// **`@State`, not a `let`.**
    ///
    /// The model used to be constructed in `HomeTabRoot`'s
    /// `navigationDestination` closure and handed in. That closure re-runs on
    /// every re-render of the parent — and `RootTabView`'s body reads
    /// `navigation.selection`, so *every tab tap* re-ran it. The view kept its
    /// identity (`.id(query)`), so `.task` did not re-fire, and 1a was left
    /// pointing at a brand-new `AskModel` with no prose, no route and no task:
    /// submit a question, tap Mail, tap Ask, and the answer was gone with no
    /// way back but retyping.
    ///
    /// `State(wrappedValue:)` evaluates its autoclosure **once per view
    /// identity**, which is exactly the lifetime a streaming session wants.
    @State private var model: AskModel
    let now: Date
    @State private var followUp = ""

    init(sync: MailSync, query: String, routeHint: AskRoute?, now: Date) {
        _model = State(wrappedValue: AskModel(sync: sync, query: query, routeHint: routeHint))
        self.now = now
    }

    var body: some View {
        VStack(spacing: 0) {
            header
            ScrollView {
                VStack(alignment: .leading, spacing: 0) {
                    queryBubble
                    content
                }
                .padding(.top, M.scrollTop)
                .padding(.horizontal, M.horizontal)
                .padding(.bottom, M.scrollBottom)
                .frame(maxWidth: .infinity, alignment: .leading)
            }
            Hairline()
            dock
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
        .background(Theme.Color.bg)
        .task { model.start() }
        .onDisappear { model.cancel() }
        .alert(Self.editTitle, isPresented: Binding(get: { model.editing != nil },
                                                    set: { if !$0 { model.editing = nil } })) {
            TextField("", text: Binding(get: { model.editorText },
                                        set: { model.editorText = $0 }))
                .accessibilityIdentifier("ask.draft.editor")
            Button(String(localized: "Cancel", comment: "1a span editor")) { model.editing = nil }
            Button(String(localized: "Done", comment: "1a span editor")) { model.commitEdit() }
        }
    }

    private var header: some View {
        HStack(alignment: .firstTextBaseline) {
            BackChevron(label: String(localized: "Back to Home",
                                      comment: "1a back affordance, spoken name"),
                        identifier: "ask.back") {
                navigation.clearAsk()
            }
            Text(HomeFeedView.greeting(at: now))
                .font(Theme.Font.heading(M.greetingSize, weight: .regular))
                .foregroundStyle(Theme.Color.ink)
                .accessibilityAddTraits(.isHeader)
            Spacer(minLength: 0)
            Text(HomeDate.short(now))
                .font(Theme.Font.body(M.dateSize))
                .nadeTracking(M.dateTracking, at: M.dateSize)
                .tabularNumerals()
                .foregroundStyle(Theme.Color.ink62)
        }
        .padding(.top, M.headerTop)
        .padding(.horizontal, M.horizontal)
        .padding(.bottom, M.headerBottom)
    }

    /// The asked query, right-aligned. `agent_draft` allows a slightly wider
    /// bubble than the other two (80% against 78%) — the mockup's own numbers.
    private var queryBubble: some View {
        HStack {
            Spacer(minLength: 0)
            Text(model.query)
                .font(Theme.Font.body(M.bubbleSize))
                .foregroundStyle(Theme.Color.ink)
                .padding(.vertical, M.bubblePadV)
                .padding(.horizontal, M.bubblePadH)
                .background(
                    RoundedRectangle(cornerRadius: Theme.Radius.lg).fill(Theme.Color.neutral100)
                )
                .overlay {
                    RoundedRectangle(cornerRadius: Theme.Radius.lg)
                        .strokeBorder(Theme.Color.divider, lineWidth: Theme.Stroke.border)
                }
                // `containerRelativeFrame`, not `UIScreen.main.bounds`: the
                // latter is the app's only screen-global measurement inside a
                // leaf view, is read per render (i.e. per streamed token), and
                // is wrong under split view.
                .containerRelativeFrame(.horizontal, alignment: .trailing) { width, _ in
                    width * (model.route == .agentDraft ? M.draftBubbleMaxWidth : M.bubbleMaxWidth)
                }
        }
        .padding(.bottom, M.bubbleBottom)
        .accessibilityIdentifier("ask.query")
    }

    @ViewBuilder
    private var content: some View {
        VStack(alignment: .leading, spacing: 0) {
            // The route's own layout first, **then** the problem. An error that
            // arrives mid-stream (`route → token → error`) must not erase the
            // partial answer that already landed: the caption sits under it, so
            // the user keeps what they got and learns it stopped.
            switch model.route {
            case .answer: answerState
            case .results: resultsState
            case .agentDraft: draftState
            case .unknown, .none:
                // Before `route` arrives there is nothing to lay out — and an
                // unknown route is a server this build does not understand, so
                // guessing a layout would be worse than saying nothing.
                if model.isStreaming {
                    StateCaption(Self.thinkingCopy, alignment: .leading, color: Theme.Color.ink62)
                        .accessibilityIdentifier("ask.thinking")
                }
            }

            if let problem = model.problem {
                StateCaption(problem.message, alignment: .leading, color: Theme.Color.ink62)
                    .padding(.top, M.buttonsTop)
                    .accessibilityIdentifier("ask.problem")
            }
        }
    }

    static let thinkingCopy = String(localized: "Thinking…", comment: "1a, before the route arrives")
    static let editTitle = String(localized: "Edit this part",
                                  comment: "1a, the single-line editor over a draft span")

    // MARK: - answer

    private var answerState: some View {
        VStack(alignment: .leading, spacing: 0) {
            prose
            // Empty `sources` renders nothing (DESIGN.md §1a).
            //
            // `v1 →` **not tappable.** DESIGN.md §1a says "tapping opens the
            // thread", but `API.md` §4 gives a source `gmail_id` and `subject`
            // "and nothing else" — and `gmail_id` is a *message* id, which
            // `GET /threads/{id}` does not accept. Wiring the tap would send a
            // message id to the thread route and 404 for any citation that is
            // not the first message of its thread. The row stays, the tap does
            // not, until the contract carries a thread id (revisit at P6, where
            // `/ask` goes live). The `results` rows below are unaffected: those
            // are `WireThreadRow`s and their `id` really is a thread.
            ForEach(model.sources, id: \.gmailID) { source in
                Hairline()
                Text(source.subject)
                    .font(Theme.Font.body(M.sourceSize))
                    .foregroundStyle(Theme.Color.ink62)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(.vertical, M.sourceRowV)
                    .accessibilityIdentifier("ask.source.\(source.gmailID)")
            }
            buttons { clearButton }
        }
    }

    // MARK: - results

    private var resultsState: some View {
        VStack(alignment: .leading, spacing: 0) {
            prose
            ForEach(model.results, id: \.id) { row in
                Hairline()
                Button {
                    navigation.selection = .mail
                    navigation.openThread(row.id, in: navigation.selectedMailboxID)
                } label: {
                    VStack(alignment: .leading, spacing: 0) {
                        HStack(alignment: .firstTextBaseline) {
                            Text(row.senderDisplayName)
                                .font(Theme.Font.body(M.hitNameSize))
                                .foregroundStyle(Theme.Color.ink)
                            Spacer(minLength: 0)
                            Text(ListTime.string(for: row.ts, now: now))
                                .font(Theme.Font.body(M.hitTimeSize))
                                .tabularNumerals()
                                .foregroundStyle(Theme.Color.ink62)
                        }
                        Text(row.subject)
                            .font(Theme.Font.body(M.hitSubjectSize))
                            .foregroundStyle(Theme.Color.ink62)
                            .padding(.top, M.hitSubjectTop)
                            .frame(maxWidth: .infinity, alignment: .leading)
                    }
                    .padding(.vertical, M.hitRowV)
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .accessibilityIdentifier("ask.result.\(row.id)")
            }
            buttons {
                // Re-posts the same query with `route_hint: "agent_draft"`,
                // which forces the route instead of letting the server classify.
                NButton(String(localized: "Make this an agent", comment: "1a results"),
                        variant: .ghost) {
                    navigation.ask(model.query, routeHint: .agentDraft)
                }
                .accessibilityIdentifier("ask.makeagent")
                clearButton
            }
        }
    }

    // MARK: - agent_draft

    private var draftState: some View {
        VStack(alignment: .leading, spacing: 0) {
            Text("Here's what I'd build. Tap anything underlined to change it.")
                .font(Theme.Font.body(M.proseSize))
                .foregroundStyle(Theme.Color.ink)
                .nadeLineHeight(M.proseLineHeight, size: M.proseSize)
                .fixedSize(horizontal: false, vertical: true)
                .padding(.bottom, M.bubbleBottom)

            if let draft = model.draft {
                draftCard(draft)

                if model.savedAgentID != nil {
                    Text(AskModel.savedCopy)
                        .font(Theme.Font.bodyItalic(M.savedSize))
                        .foregroundStyle(Theme.Color.accent700)
                        .fixedSize(horizontal: false, vertical: true)
                        .padding(.top, M.savedTop)
                        .accessibilityIdentifier("ask.saved")
                } else {
                    buttons {
                        NButton(String(localized: "Save as draft", comment: "1a agent draft"),
                                variant: .primary) {
                            Task { await model.saveDraft() }
                        }
                        .disabled(model.isSaving)
                        .accessibilityIdentifier("ask.savedraft")
                        NButton(String(localized: "Start over", comment: "1a agent draft"),
                                variant: .secondary) {
                            navigation.clearAsk()
                        }
                        .accessibilityIdentifier("ask.startover")
                    }
                }
            }
        }
    }

    /// `NCard`, whose `Border.accent` and `NCardPadding.draftAgent` were both
    /// written for this card — their doc comments name it — and which the
    /// component gallery already renders.
    private func draftCard(_ draft: AskDraftPayload) -> some View {
        NCard(border: .accent, padding: NCardPadding.draftAgent, spacing: 0) {
            // `NCardKicker`, not a hand-rolled `Text`: it applies
            // `.textCase(.uppercase)` rather than baking the capitals into the
            // string, so VoiceOver reads "Draft agent" instead of spelling it
            // out letter by letter (EDGE E6).
            NCardKicker("Draft agent")
                .padding(.bottom, M.kickerBottom)

            Text(draft.name)
                .font(Theme.Font.heading(M.cardNameSize))
                .foregroundStyle(Theme.Color.ink)
                .padding(.bottom, M.cardNameBottom)
                .accessibilityIdentifier("ask.draft.name")

            // Same composed sentence 1c renders, so a draft looks identical
            // whether you arrived from here or from a saved agent — and, like
            // 1c, one flowing `Text` with `link` spans rather than stacked rows,
            // which wrap in the wrong places at 15 pt over line-height 1.85 and
            // cannot carry a tap target inside a paragraph.
            Text(sentence(for: draft))
                .font(Theme.Font.body(M.cardSentenceSize))
                .foregroundStyle(Theme.Color.ink)
                .tint(Theme.Color.ink)
                .nadeLineHeight(M.cardSentenceLineHeight, size: M.cardSentenceSize)
                .fixedSize(horizontal: false, vertical: true)
                .frame(maxWidth: .infinity, alignment: .leading)
                .environment(\.openURL, OpenURLAction { url in
                    switch url.absoluteString {
                    case Self.whenLink: model.beginEditing(.when)
                    case Self.doLink: model.beginEditing(.doing)
                    default: return .systemAction
                    }
                    return .handled
                })
                .accessibilityIdentifier("ask.draft.sentence")

            // `DESIGN.md` §1a: "tag row top 14, gap 6, **wrapping**". A plain
            // `HStack` compresses instead, which a longer tool name or a
            // localisation overflows off the card.
            FlowLayout(spacing: M.tagGap) {
                NTag(AgentToolCopy.name(for: draft.tool), style: .outline)
                if draft.approvalRequired {
                    NTag(String(localized: "Approval required", comment: "1a draft card tag"))
                }
                NTag(draft.status.rawValue.capitalized)
            }
            .padding(.top, M.tagTop)
        }
        .accessibilityIdentifier("ask.draft")
    }

    /// A private scheme, so a mail-borne `https:` link can never be confused
    /// for one of these.
    static let whenLink = "nade-ask-edit:when"
    static let doLink = "nade-ask-edit:do"

    /// The spans read from the **model**, not the payload, so an edit made
    /// before saving is what the card shows and what `POST /agents` sends.
    private func sentence(for draft: AskDraftPayload) -> AttributedString {
        var result = AttributedString("When ")
        result.append(Self.underlined(model.whenSpan, link: Self.whenLink))
        result.append(AttributedString(", "))
        result.append(Self.underlined(model.doSpan, link: Self.doLink))
        result.append(AttributedString("."))
        if let trailing = draft.trailing, !trailing.isEmpty {
            var tail = AttributedString(" \(trailing)")
            tail.foregroundColor = Theme.Color.ink60
            result.append(tail)
        }
        return result
    }

    private static func underlined(_ text: String, link: String) -> AttributedString {
        var span = AttributedString(text)
        span.link = URL(string: link)
        span.foregroundColor = Theme.Color.ink
        span.underlineStyle = Text.LineStyle(pattern: .solid, color: Theme.Color.accent)
        return span
    }

    // MARK: - Shared

    private var prose: some View {
        Text(model.prose)
            .font(Theme.Font.body(M.proseSize))
            .foregroundStyle(Theme.Color.ink)
            .nadeLineHeight(M.proseLineHeight, size: M.proseSize)
            .fixedSize(horizontal: false, vertical: true)
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.bottom, M.proseBottom)
            .accessibilityIdentifier("ask.prose")
    }

    private var clearButton: some View {
        NButton(String(localized: "Clear", comment: "1a, dismisses the query"), variant: .ghost) {
            navigation.clearAsk()
        }
        .accessibilityIdentifier("ask.clear")
    }

    @ViewBuilder
    private func buttons<Content: View>(@ViewBuilder _ content: () -> Content) -> some View {
        HStack(spacing: M.buttonsGap) {
            content()
            Spacer(minLength: 0)
        }
        .padding(.top, M.buttonsTop)
    }

    private var dock: some View {
        NAskField(text: $followUp, metrics: .docked, identifier: "ask.field.docked") {
            let query = followUp
            followUp = ""
            navigation.ask(query)
        }
        .padding(.top, M.dockTop)
        .padding(.horizontal, M.dockH)
        .padding(.bottom, M.dockBottom)
    }
}

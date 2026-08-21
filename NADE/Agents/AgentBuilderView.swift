//
//  AgentBuilderView.swift
//  NADE
//
//  1c (`DESIGN.md` §1c). A modal with no tab bar. Everything below the nav bar
//  is one scroll view — **including the footer buttons**, which are not pinned.
//

import SwiftUI

struct AgentBuilderView: View {

    private enum M {
        static let navTop: CGFloat = 60
        static let horizontal = Theme.Space.screen
        static let navBottom: CGFloat = 11
        static let navAction: CGFloat = 13
        static let navTitle: CGFloat = 15

        static let sentenceTop: CGFloat = 20
        static let sentenceBottom: CGFloat = 18
        static let nameSize: CGFloat = 22
        static let nameBottom: CGFloat = 14
        static let sentenceSize: CGFloat = 16
        static let sentenceLineHeight: CGFloat = 2
        static let hintSize: CGFloat = 12
        static let hintTop: CGFloat = 14

        static let sectionTop: CGFloat = 15
        static let sectionBottom: CGFloat = 14
        static let eyebrowSize: CGFloat = 10
        static let eyebrowTracking: CGFloat = 0.1
        static let summarySize: CGFloat = 12

        static let tableTop: CGFloat = 12
        static let tablePadV: CGFloat = 12
        static let tablePadH: CGFloat = 13
        static let tableRowV: CGFloat = 6
        static let tableSize: CGFloat = 13
        static let tableLabelWidth: CGFloat = 66

        static let promptPadV: CGFloat = 11
        static let promptPadH: CGFloat = 12
        static let promptSize: CGFloat = 14
        static let promptLineHeight: CGFloat = 1.6

        static let toolRowV: CGFloat = 11
        static let toolGap: CGFloat = 10
        static let toolSize: CGFloat = 14
        static let toolGlyphWidth: CGFloat = 14
        static let toolNoteSize: CGFloat = 11

        static let settingRowSize: CGFloat = 14
        static let settingBlockTop: CGFloat = 16
        static let settingBlockPadTop: CGFloat = 14
        static let settingHintSize: CGFloat = 12
        static let settingNoteSize: CGFloat = 12
        static let settingNoteLineHeight: CGFloat = 1.6

        static let footerTop: CGFloat = 18
        static let footerBottom: CGFloat = 30
        static let footerGap: CGFloat = 10
    }

    @Environment(AppNavigation.self) private var navigation

    /// `@State` for the same reason `AskView`'s is: the model was built inside
    /// the `fullScreenCover` content closure, so a re-render of the presenter
    /// replaced it with an unloaded one while `.task` — bound to an unchanged
    /// view identity — never re-ran. 1c rendered an empty name, no sentence,
    /// "Not set" and "None".
    @State private var model: AgentBuilderModel

    init(sync: MailSync, agentID: String) {
        _model = State(wrappedValue: AgentBuilderModel(sync: sync, agentID: agentID))
    }

    var body: some View {
        VStack(spacing: 0) {
            navBar
            Hairline()
            ScrollView {
                VStack(spacing: 0) {
                    sentenceBlock
                    Hairline()
                    section(.invocation, "Invocation", model.invocationSummary) { invocation }
                    section(.inputs, "Inputs",
                            String(localized: "Prompt", comment: "1c Inputs summary")) { inputs }
                    section(.tools, "Tools", model.toolsSummary) { tools }
                    section(.settings, "Settings", model.settingsSummary) { settings }
                    footer
                }
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
        .background(Theme.Color.bg)
        .task { await model.load() }
        .sheet(isPresented: Binding(get: { model.isSchedulePresented },
                                    set: { model.isSchedulePresented = $0 })) {
            ScheduleSheet(model: model)
        }
    }

    // MARK: - Nav bar

    private var navBar: some View {
        HStack(alignment: .firstTextBaseline) {
            Button { navigation.editingAgentID = nil } label: {
                Text("Cancel")
                    .font(Theme.Font.body(M.navAction))
                    .foregroundStyle(Theme.Color.accent)
                    .nadeHitTarget()   // EDGE (E17)
            }
            .buttonStyle(.plain)
            .accessibilityIdentifier("builder.cancel")

            Spacer(minLength: 0)
            Text("Edit agent")
                .font(Theme.Font.heading(M.navTitle))
                .foregroundStyle(Theme.Color.ink)
                .accessibilityAddTraits(.isHeader)
            Spacer(minLength: 0)

            // Every control writes through immediately, so "Save" closes rather
            // than submitting. A button that re-sent what is already saved
            // would be a control that does nothing (DESIGN.md §4).
            Button { navigation.editingAgentID = nil } label: {
                Text("Save")
                    .font(Theme.Font.body(M.navAction))
                    .foregroundStyle(Theme.Color.accent)
                    .nadeHitTarget()   // EDGE (E17)
            }
            .buttonStyle(.plain)
            .accessibilityIdentifier("builder.save")
        }
        .padding(.top, M.navTop)
        .padding(.horizontal, M.horizontal)
        .padding(.bottom, M.navBottom)
    }

    // MARK: - The sentence

    private var sentenceBlock: some View {
        VStack(alignment: .leading, spacing: 0) {
            Text(model.agent?.name ?? "")
                .font(Theme.Font.heading(M.nameSize))
                .foregroundStyle(Theme.Color.ink)
                .padding(.bottom, M.nameBottom)
                .accessibilityIdentifier("builder.name")

            if model.isCompiled {
                compiledSentence
            } else {
                // The compile-failure fallback: plain unstyled text, the error
                // beneath it, and a tap edits the whole sentence. The only
                // state in which the underlines are absent (DESIGN.md §1c).
                Button { model.beginEditing(.whole) } label: {
                    Text(model.agent?.nlDefinition ?? "")
                        .font(Theme.Font.body(M.sentenceSize))
                        .foregroundStyle(Theme.Color.ink)
                        .nadeLineHeight(M.sentenceLineHeight, size: M.sentenceSize)
                        .fixedSize(horizontal: false, vertical: true)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .accessibilityIdentifier("builder.sentence.plain")

                if let error = model.agent?.compileError {
                    StateCaption(error, alignment: .leading, color: Theme.Color.accent700)
                        .padding(.top, M.hintTop)
                        .accessibilityIdentifier("builder.compileerror")
                }
            }

            StateCaption(Self.hintCopy, alignment: .leading, color: Theme.Color.ink60)
                .padding(.top, M.hintTop)

            if let problem = model.problem {
                StateCaption(problem.message, alignment: .leading, color: Theme.Color.ink62)
                    .padding(.top, M.hintTop)
                    .accessibilityIdentifier("builder.problem")
            }
        }
        .padding(.top, M.sentenceTop)
        .padding(.horizontal, M.horizontal)
        .padding(.bottom, M.sentenceBottom)
        .alert(Self.editTitle, isPresented: Binding(get: { model.editing != nil },
                                                    set: { if !$0 { model.cancelEdit() } })) {
            TextField("", text: Binding(get: { model.editorText },
                                        set: { model.editorText = $0 }))
                .accessibilityIdentifier("builder.editor")
            Button(String(localized: "Cancel", comment: "1c span editor")) { model.cancelEdit() }
            Button(String(localized: "Done", comment: "1c span editor")) {
                Task { await model.commitEdit() }
            }
        }
    }

    static let hintCopy = String(
        localized: "Tap any underline to edit, or open a section below.",
        comment: "1c, under the sentence"
    )

    static let editTitle = String(localized: "Edit this part",
                                  comment: "1c, the single-line editor over a span")

    /// **Composed, not parsed** — `When {when_span}, {do_span}.` with both spans
    /// underlined and independently tappable, then `trailing` in `ink60`.
    ///
    /// One `Text` over an `AttributedString`, not a stack of views. The spans
    /// sit **inside a flowing paragraph**: the sentence wraps at 16 pt over
    /// line-height 2, and any `HStack` arrangement breaks it into rigid rows
    /// that wrap in the wrong places and stretch the underline across the gap.
    /// Per-span tapping survives because each span carries a `link` attribute,
    /// which `openURL` below intercepts — the only mechanism SwiftUI has for a
    /// tap target inside laid-out text.
    private var compiledSentence: some View {
        Text(sentence)
            .font(Theme.Font.body(M.sentenceSize))
            .foregroundStyle(Theme.Color.ink)
            .tint(Theme.Color.ink)
            .nadeLineHeight(M.sentenceLineHeight, size: M.sentenceSize)
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
            .accessibilityElement(children: .combine)
            .accessibilityIdentifier("builder.sentence")
    }

    /// A private scheme, so a mail-borne `https:` link can never be confused
    /// for one of these — `openURL` above only claims these two exact strings
    /// and passes everything else on.
    static let whenLink = "nade-edit:when"
    static let doLink = "nade-edit:do"

    private var sentence: AttributedString {
        var result = AttributedString("When ")
        result.append(Self.span(model.agent?.whenSpan ?? "", link: Self.whenLink))
        result.append(AttributedString(", "))
        result.append(Self.span(model.agent?.doSpan ?? "", link: Self.doLink))
        result.append(AttributedString("."))
        if let trailing = model.sentenceTrailing {
            var tail = AttributedString(" \(trailing)")
            tail.foregroundColor = Theme.Color.ink60
            result.append(tail)
        }
        return result
    }

    private static func span(_ text: String, link: String) -> AttributedString {
        var span = AttributedString(text)
        span.link = URL(string: link)
        span.foregroundColor = Theme.Color.ink
        span.underlineStyle = Text.LineStyle(pattern: .solid, color: Theme.Color.accent)
        return span
    }

    // MARK: - Sections

    @ViewBuilder
    private func section<Content: View>(
        _ id: AgentBuilderModel.Section,
        _ eyebrow: String,
        _ summary: String,
        @ViewBuilder content: () -> Content
    ) -> some View {
        VStack(alignment: .leading, spacing: 0) {
            Button {
                model.openSection = model.openSection == id ? nil : id
            } label: {
                HStack(alignment: .firstTextBaseline) {
                    SectionEyebrow(eyebrow, color: Theme.Color.accent)
                    Spacer(minLength: 0)
                    Text(summary)
                        .font(Theme.Font.body(M.summarySize))
                        .foregroundStyle(Theme.Color.ink55)
                }
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .accessibilityIdentifier("builder.section.\(id)")
            .accessibilityAddTraits(model.openSection == id ? [.isSelected] : [])

            if model.openSection == id {
                content().padding(.top, Self.contentTop(for: id))
            }
        }
        .padding(.top, M.sectionTop)
        .padding(.horizontal, M.horizontal)
        .padding(.bottom, M.sectionBottom)
        .overlay(alignment: .bottom) { Hairline() }
    }

    /// `DESIGN.md` §1c: "Expanded-content top margin is **per section**".
    static func contentTop(for id: AgentBuilderModel.Section) -> CGFloat {
        switch id {
        case .invocation: 14
        case .inputs: 13
        case .tools: 6
        case .settings: 13
        }
    }

    // MARK: Invocation

    /// The radios reflect `spec.trigger.kind` and do not write.
    ///
    /// `v1 →` **read-only.** `PATCH /agents/{id}` accepts `nl_definition`,
    /// `status`, `allowed_tools`, `approval_required` and `schedule` — and no
    /// trigger kind. The kind is compiled *from the sentence*, exactly like the
    /// filter table below it, so a writable radio would be a control with
    /// nothing behind it (`DESIGN.md` §4). Changing how an agent is invoked
    /// means editing the sentence, which is one tap away above.
    private var invocation: some View {
        VStack(alignment: .leading, spacing: 0) {
            ForEach(Self.invocationRows, id: \.kind.rawValue) { row in
                Hairline()
                NRadioRow(row.label,
                          hint: row.hint,
                          isSelected: model.agent?.spec?.trigger.kind == row.kind,
                          metrics: .invocation) {}
                    .disabled(true)
                    .accessibilityIdentifier("builder.trigger.\(row.kind.rawValue)")
            }

            NCard(padding: EdgeInsets(top: M.tablePadV, leading: M.tablePadH,
                                      bottom: M.tablePadV, trailing: M.tablePadH),
                  spacing: 0) {
                tableRow(String(localized: "From", comment: "1c filter table row"), model.fromFilter)
                Hairline()
                tableRow(String(localized: "About", comment: "1c filter table row"), model.aboutFilter)
            }
            .padding(.top, M.tableTop)

            if model.agent?.spec?.trigger.kind == .schedule {
                NButton(String(localized: "How often", comment: "1c, opens the schedule sheet"),
                        variant: .secondary) { model.presentSchedule() }
                    .padding(.top, M.tableTop)
                    .accessibilityIdentifier("builder.schedule")
            }
        }
    }

    static let invocationRows: [(label: String, hint: String, kind: TriggerKind)] = [
        (String(localized: "When mail arrives", comment: "1c invocation option"),
         String(localized: "Runs on its own, on matching mail", comment: "1c invocation hint"), .mail),
        (String(localized: "Only when I ask", comment: "1c invocation option"),
         String(localized: "You tap Run on the agent", comment: "1c invocation hint"), .manual),
        (String(localized: "On a schedule", comment: "1c invocation option"),
         String(localized: "Daily, weekly, or a custom repeat", comment: "1c invocation hint"), .schedule),
    ]

    private func tableRow(_ label: String, _ value: String) -> some View {
        HStack(alignment: .top, spacing: 0) {
            Text(label)
                .foregroundStyle(Theme.Color.ink60)
                .frame(width: M.tableLabelWidth, alignment: .leading)
            Text(value)
                .foregroundStyle(value.isEmpty ? Theme.Color.ink62 : Theme.Color.ink)
                .fixedSize(horizontal: false, vertical: true)
                .frame(maxWidth: .infinity, alignment: .leading)
        }
        .font(Theme.Font.body(M.tableSize))
        .padding(.vertical, M.tableRowV)
    }

    // MARK: Inputs

    private var inputs: some View {
        NCard(padding: EdgeInsets(top: M.promptPadV, leading: M.promptPadH,
                                  bottom: M.promptPadV, trailing: M.promptPadH),
              spacing: 0) {
            Text(model.agent?.spec?.instruction ?? "")
                .font(Theme.Font.body(M.promptSize))
                .foregroundStyle(Theme.Color.ink)
                .nadeLineHeight(M.promptLineHeight, size: M.promptSize)
                .fixedSize(horizontal: false, vertical: true)
                .frame(maxWidth: .infinity, alignment: .leading)
        }
        .accessibilityIdentifier("builder.instruction")
    }

    // MARK: Tools

    private var tools: some View {
        VStack(spacing: 0) {
            ForEach(AgentToolCopy.all, id: \.rawValue) { tool in
                Hairline()
                let isOn = model.agent?.allowedTools.contains(tool) ?? false
                Button {
                    Task { await model.toggleTool(tool) }
                } label: {
                    HStack(spacing: M.toolGap) {
                        Text(isOn ? "✓" : "")
                            .font(Theme.Font.body(M.toolSize))
                            .foregroundStyle(Theme.Color.accent)
                            .frame(width: M.toolGlyphWidth, alignment: .leading)
                        Text(AgentToolCopy.name(for: tool))
                            .font(Theme.Font.body(M.toolSize))
                            .foregroundStyle(Theme.Color.ink)
                        Spacer(minLength: 0)
                        Text(AgentToolCopy.note(for: tool))
                            .font(Theme.Font.body(M.toolNoteSize))
                            .foregroundStyle(Theme.Color.ink60)
                    }
                    .padding(.vertical, M.toolRowV)
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .accessibilityAddTraits(isOn ? [.isSelected] : [])
                .accessibilityIdentifier("builder.tool.\(tool.rawValue)")
            }
        }
    }

    // MARK: Settings

    private var settings: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack {
                Text("Status")
                    .font(Theme.Font.body(M.settingRowSize))
                    .foregroundStyle(Theme.Color.ink)
                Spacer(minLength: 0)
                NSegmented(
                    options: [AgentStatus.draft, AgentStatus.published],
                    selection: Binding(
                        get: { model.agent?.status ?? .draft },
                        set: { status in Task { await model.setStatus(status) } }
                    ),
                    metrics: .status,
                    label: { $0.rawValue.capitalized }
                )
                .accessibilityIdentifier("builder.status")
            }

            VStack(spacing: 0) {
                Hairline()
                HStack(alignment: .top) {
                    VStack(alignment: .leading, spacing: 0) {
                        Text("Ask before it acts")
                            .font(Theme.Font.body(M.settingRowSize))
                            .foregroundStyle(Theme.Color.ink)
                        Text("You approve each draft or change")
                            .font(Theme.Font.body(M.settingHintSize))
                            .foregroundStyle(Theme.Color.ink55)
                    }
                    Spacer(minLength: 0)
                    NToggle("", isOn: Binding(
                        get: { model.agent?.approvalRequired ?? true },
                        set: { value in Task { await model.setApprovalRequired(value) } }
                    ))
                    .accessibilityIdentifier("builder.approval")
                }
                .padding(.top, M.settingBlockPadTop)
            }
            .padding(.top, M.settingBlockTop)

            VStack(spacing: 0) {
                Hairline()
                Text(model.approvalNote)
                    .font(Theme.Font.bodyItalic(M.settingNoteSize))
                    .foregroundStyle(Theme.Color.ink55)
                    .nadeLineHeight(M.settingNoteLineHeight, size: M.settingNoteSize)
                    .fixedSize(horizontal: false, vertical: true)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(.top, M.settingBlockPadTop)
                    .accessibilityIdentifier("builder.approvalnote")
            }
            .padding(.top, M.settingBlockTop)
        }
    }

    // MARK: - Footer

    private var footer: some View {
        HStack(spacing: M.footerGap) {
            NButton(String(localized: "Run once now", comment: "1c footer"),
                    variant: .primary, width: .flexible) {
                Task { await model.runOnce() }
            }
            .disabled(model.isBusy)
            .accessibilityIdentifier("builder.run")

            NButton(String(localized: "Delete", comment: "1c footer"),
                    variant: .secondary) {
                Task { if await model.delete() { navigation.editingAgentID = nil } }
            }
            .disabled(model.isBusy)
            .accessibilityIdentifier("builder.delete")
        }
        .padding(.top, M.footerTop)
        .padding(.horizontal, M.horizontal)
        .padding(.bottom, M.footerBottom)
    }
}

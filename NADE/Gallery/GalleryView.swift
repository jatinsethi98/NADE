//
//  GalleryView.swift
//  NADE
//
//  Every token and every component, labelled, at its real size. This is the
//  artefact we screenshot-compare against the Classical design system.
//
//  DEBUG only. Reach it with:
//    xcrun simctl launch <device> fsaas.NADE -NADEGallery 1
//    xcrun simctl launch <device> fsaas.NADE -NADEGallery 1 -NADEGallerySection buttons
//

#if DEBUG

import SwiftUI

struct GalleryView: View {
    let initialSection: String?

    @State private var segmented: NoteMode = .read
    @State private var status = "Published"
    @State private var invocation: Invocation = .onMail
    @State private var ends: Ends = .never
    @State private var approvalOn = true
    @State private var approvalOff = false
    @State private var every = 2
    @State private var chip: String = "Recruiters"
    @State private var emptyField = ""
    @State private var filledField = "Priya Raghavan"
    @State private var longField = "supercalifragilisticexpialidocious-token-0123456789"
    @State private var tab: NTab = .ask
    @State private var hitChip = false

    enum NoteMode: String, CaseIterable { case read = "Read", write = "Write" }
    enum Invocation: String, CaseIterable { case onMail, onSchedule, manual }
    enum Ends: String, CaseIterable { case never, onDate, after }

    /// A 200-character unbroken token — the worst string this UI can be handed.
    private static let pathological = String(repeating: "Xq7", count: 67)

    /// 1a's draft sentence: `When {when_span}, {do_span}.` The agent object
    /// carries `when_span` / `do_span`, so the sentence is **composed**, never
    /// parsed — DESIGN.md §3 1a.
    ///
    /// Known deviation, recorded in IOS_DECISIONS.md: the mockup's spans carry
    /// `border-bottom: 1px solid accent`, a **rule** rather than a typographic
    /// underline, and SwiftUI's `Text` will not give an underline a colour of
    /// its own — `Text.underline(_:color:)`, SwiftUI's `underlineStyle` and the
    /// UIKit scope's `underlineColor` all lose to the run's foreground, so the
    /// rule renders in ink. The faithful accent rule needs per-span layout,
    /// which P3 has to build anyway because the spans are separately tappable.
    // `let`, not `var`: as a computed property this rebuilt five attributed runs
    // and four concatenations on every render of the card section.
    private static let draftSentence: AttributedString = {
        func run(_ text: String, underlined: Bool = false) -> AttributedString {
            var s = AttributedString(text)
            s.foregroundColor = Theme.Color.ink
            if underlined { s.underlineStyle = .single }
            return s
        }
        return run("When ")
            + run("a recruiter emails", underlined: true)
            + run(", ")
            + run("write a note with the next steps", underlined: true)
            + run(".")
    }()

    var body: some View {
        ScrollViewReader { proxy in
            ScrollView {
                VStack(alignment: .leading, spacing: 0) {
                    header
                    colorSection
                    typeSection
                    spaceSection
                    buttonSection
                    inputSection
                    cardSection
                    tagSection
                    controlSection
                    tabBarSection
                    edgeSection
                    accessibilitySizeSection
                    accessibilitySizeSectionContinued
                    motionSection

                    // One screen of tail, so `scrollTo(_:anchor: .top)` can put
                    // the **last** section at the top rather than clamping at
                    // the bottom of the content.
                    //
                    // Without it, `-NADEGallerySection motion` landed on the
                    // same frame as `ax5b` — the two screenshots came out
                    // byte-identical, and `docs/screens/` claimed a Reduce
                    // Motion shot that was really a second copy of AX5.
                    Color.clear
                        .frame(maxWidth: .infinity)
                        .containerRelativeFrame(.vertical)
                        .accessibilityHidden(true)
                }
                .padding(.bottom, Theme.Space.s8)
            }
            .background(Theme.Color.bg)
            .onAppear {
                if let initialSection {
                    proxy.scrollTo(initialSection, anchor: .top)
                }
            }
        }
    }

    // MARK: Header

    private var header: some View {
        VStack(alignment: .leading, spacing: Theme.Space.s1) {
            Text("NADE component gallery")
                .font(Theme.Font.heading(23))
                .foregroundStyle(Theme.Color.ink)
            Text("Classical design system · docs/DESIGN.md §1")
                .font(Theme.Font.body(12))
                .foregroundStyle(Theme.Color.ink55)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(.horizontal, Theme.Space.screen)
        .padding(.top, Theme.Space.s3)
        .padding(.bottom, Theme.Space.s3)
        .id("top")
    }

    // MARK: 1 · Colour

    private var colorSection: some View {
        GallerySection("Colour", id: "color") {
            GalleryLabel("Core roles")
            SwatchGrid(swatches: [
                .init("bg", Theme.Color.bg),
                .init("surface", Theme.Color.surface),
                .init("ink", Theme.Color.ink),
                .init("accent", Theme.Color.accent),
                .init("accent2", Theme.Color.accent2),
                .init("divider", Theme.Color.divider),
            ])

            GalleryLabel("Muted-ink ladder — the only greys")
            SwatchGrid(swatches: [
                .init("ink50", Theme.Color.ink50),
                .init("ink55", Theme.Color.ink55),
                .init("ink60", Theme.Color.ink60),
                .init("ink62", Theme.Color.ink62),
                .init("ink68", Theme.Color.ink68),
                .init("ink70", Theme.Color.ink70),
            ])

            GalleryLabel("Neutral ramp")
            SwatchGrid(swatches: [
                .init("100", Theme.Color.neutral100), .init("200", Theme.Color.neutral200),
                .init("300", Theme.Color.neutral300), .init("400", Theme.Color.neutral400),
                .init("500", Theme.Color.neutral500), .init("600", Theme.Color.neutral600),
                .init("700", Theme.Color.neutral700), .init("800", Theme.Color.neutral800),
                .init("900", Theme.Color.neutral900),
            ])

            GalleryLabel("Accent ramp")
            SwatchGrid(swatches: [
                .init("100", Theme.Color.accent100), .init("200", Theme.Color.accent200),
                .init("300", Theme.Color.accent300), .init("400", Theme.Color.accent400),
                .init("500", Theme.Color.accent500), .init("600", Theme.Color.accent600),
                .init("700", Theme.Color.accent700), .init("800", Theme.Color.accent800),
                .init("900", Theme.Color.accent900),
            ])

            GalleryLabel("Accent-2 ramp")
            SwatchGrid(swatches: [
                .init("100", Theme.Color.accent2_100), .init("200", Theme.Color.accent2_200),
                .init("300", Theme.Color.accent2_300), .init("400", Theme.Color.accent2_400),
                .init("500", Theme.Color.accent2_500), .init("600", Theme.Color.accent2_600),
                .init("700", Theme.Color.accent2_700), .init("800", Theme.Color.accent2_800),
                .init("900", Theme.Color.accent2_900),
            ])
        }
    }

    // MARK: 2 · Type

    private static let scale: [TypeSpec] = [
        .init(27, "greeting (1a)"), .init(26, "greeting (2a/2b)"), .init(25, "OTP code"),
        .init(24, "thread subject"), .init(23, "screen title"), .init(22, "agent name / cal day"),
        .init(20, "sheet title"), .init(19, "draft agent name"), .init(17, "list item title"),
        .init(16, "sheet body"), .init(15, "list primary / base"), .init(14.5, "mail body"),
        .init(14, "secondary body / buttons"), .init(13.5, "search placeholder"),
        .init(13, "meta, nav actions"), .init(12.5, "feed peek"), .init(12, "captions"),
        .init(11.5, "prompt numeral"), .init(11, "timestamps"), .init(10.5, "tab labels"),
        .init(10, "section eyebrows"), .init(9.5, "calendar item time"),
    ]

    private var typeSection: some View {
        GallerySection("Type", id: "type") {
            GalleryLabel("Cormorant Garamond 600 — interface headings")
            ForEach(Self.scale.prefix(12)) { spec in
                TypeRow(size: spec.size, use: spec.use, font: Theme.Font.heading(spec.size))
            }

            GalleryLabel("Cormorant Garamond 400 — display text")
            ForEach([CGFloat(27), 26, 25, 22], id: \.self) { size in
                TypeRow(size: size, use: "display", font: Theme.Font.heading(size, weight: .regular))
            }

            GalleryLabel("Lora 400 — all body text")
            ForEach(Self.scale.dropFirst(8)) { spec in
                TypeRow(size: spec.size, use: spec.use, font: Theme.Font.body(spec.size))
            }

            GalleryLabel("Lora 600")
            ForEach([CGFloat(15), 14], id: \.self) { size in
                TypeRow(size: size, use: "unread sender", font: Theme.Font.body(size, weight: .semibold))
            }

            GalleryLabel("Tracking (letter-spacing)")
            VStack(alignment: .leading, spacing: Theme.Space.s1) {
                SectionEyebrow("Section eyebrow · 10 pt · 0.10 em")
                SectionEyebrow("Card kicker · accent", color: Theme.Color.accent)
                Text("SUN 16 AUG · 11 pt · 0.06 em")
                    .font(Theme.Font.body(11))
                    .foregroundStyle(Theme.Color.ink60)
                    .nadeTracking(0.06, at: 11)
                    .tabularNumerals()
            }

            GalleryLabel("Tabular numerals (both faces ship `tnum`)")
            VStack(alignment: .leading, spacing: 2) {
                Text("111111  ·  000000   proportional")
                    .font(Theme.Font.body(15))
                Text("111111  ·  000000   tabular")
                    .font(Theme.Font.body(15))
                    .tabularNumerals()
                Text("9:12  ·  11:41  ·  4 new")
                    .font(Theme.Font.heading(15))
                    .tabularNumerals()
            }
            .foregroundStyle(Theme.Color.ink)

            GalleryLabel("Line height")
            Text("Wanted to check whether you're open to a conversation about a "
                 + "Staff Product Designer role — line-height 1.55 on Lora 15.")
                .font(Theme.Font.body(15))
                .nadeLineHeight(1.55, size: 15)
                .foregroundStyle(Theme.Color.ink)
        }
    }

    // MARK: 3 · Space, shape, elevation

    private var spaceSection: some View {
        GallerySection("Space · shape · elevation", id: "space") {
            GalleryLabel("Space scale")
            VStack(alignment: .leading, spacing: Theme.Space.s1) {
                SpaceRule("space-1", Theme.Space.s1)
                SpaceRule("space-2", Theme.Space.s2)
                SpaceRule("space-3", Theme.Space.s3)
                SpaceRule("space-4", Theme.Space.s4)
                SpaceRule("space-6", Theme.Space.s6)
                SpaceRule("space-8", Theme.Space.s8)
                SpaceRule("screen", Theme.Space.screen)
                SpaceRule("screen (detail)", Theme.Space.screenDetail)
            }

            GalleryLabel("Radius")
            HStack(spacing: Theme.Space.s3) {
                RadiusChip("sm 2", Theme.Radius.sm)
                RadiusChip("md 4", Theme.Radius.md)
                RadiusChip("lg 7", Theme.Radius.lg)
                RadiusChip("pill", Theme.Radius.pill)
            }

            GalleryLabel("Hairline — 1 device pixel of `divider`")
            VStack(alignment: .leading, spacing: Theme.Space.s2) {
                Hairline()
                Hairline(color: Theme.Color.accent)
                HStack(spacing: Theme.Space.s2) {
                    Text("1 pt component stroke")
                        .font(Theme.Font.body(12))
                        .foregroundStyle(Theme.Color.ink60)
                    RoundedRectangle(cornerRadius: Theme.Radius.md, style: .circular)
                        .strokeBorder(Theme.Color.divider, lineWidth: Theme.Stroke.border)
                        .frame(width: 60, height: 24)
                    Text("2 pt row rule")
                        .font(Theme.Font.body(12))
                        .foregroundStyle(Theme.Color.ink60)
                    Rectangle()
                        .fill(Theme.Color.accent)
                        .frame(width: Theme.Stroke.rule, height: 24)
                }
            }

            GalleryLabel("Elevation")
            HStack(spacing: Theme.Space.s4) {
                ShadowChip("sm", Theme.Shadow.sm)
                ShadowChip("md", Theme.Shadow.md)
                ShadowChip("lg", Theme.Shadow.lg)
            }
            .padding(.vertical, Theme.Space.s2)
        }
    }

    // MARK: 4 · Buttons

    private var buttonSection: some View {
        GallerySection("Buttons — outlined only, never filled", id: "buttons") {
            GalleryLabel("Normal")
            HStack(spacing: Theme.Space.s2) {
                NButton("Approve", variant: .primary) {}
                NButton("Edit", variant: .secondary) {}
                NButton("Skip", variant: .ghost) {}
                // v1 takes no outbound action: this button submits the query
                // (`POST /ask`), it does not send anything. DESIGN.md §4.
                NButton(systemImage: "arrow.up", label: "Ask", glyph: 17) {}
                NButton(systemImage: "sparkles", label: "Ask", glyph: 16, corner: .pill, box: 38) {}
            }

            GalleryLabel("Pressed (`:active` wash)")
            HStack(spacing: Theme.Space.s2) {
                PressedButton("Approve", variant: .primary)
                PressedButton("Edit", variant: .secondary)
                PressedButton("Skip", variant: .ghost)
            }

            GalleryLabel("Disabled (opacity 0.45)")
            HStack(spacing: Theme.Space.s2) {
                NButton("Approve", variant: .primary) {}.disabled(true)
                NButton("Edit", variant: .secondary) {}.disabled(true)
                NButton("Skip", variant: .ghost) {}.disabled(true)
                NButton(systemImage: "arrow.up", label: "Ask", glyph: 17) {}.disabled(true)
            }

            GalleryLabel("With a glyph · empty label (VoiceOver still hears “Button”)")
            HStack(spacing: Theme.Space.s2) {
                NButton("Add a schedule", systemImage: "plus", variant: .ghost) {}
                NButton("", variant: .secondary) {}
            }

            GalleryLabel("`flex: 1` — 1c’s “Run once now” beside an intrinsic “Delete”")
            HStack(spacing: 10) {
                NButton("Run once now", variant: .primary, width: .flexible) {}
                NButton("Delete", variant: .secondary) {}
            }

            GalleryLabel("`flex: 1` on both — 1d’s sheet footer")
            HStack(spacing: 10) {
                NButton("Cancel", variant: .secondary, width: .flexible) {}
                NButton("Every 2 weeks", variant: .primary, width: .flexible) {}
            }
        }
    }

    // MARK: 5 · Inputs

    private var inputSection: some View {
        GallerySection("Inputs", id: "inputs") {
            GalleryLabel("DS `.input` — empty, filled, focused (14 pt · 6/10 · 36)")
            VStack(spacing: Theme.Space.s2) {
                NTextField("Search notes", text: $emptyField)
                NTextField("Search notes", text: $filledField)
                NTextField("Search notes", text: $emptyField, showsFocusRing: true)
            }

            // Every ask field in the design, at its own screen's numbers.
            // DESIGN.md §2 — none of the four share a row of that table.
            GalleryLabel("2a feed — 13.5 pt · 8/15 · 38 · `sparkles` 16")
            HStack(spacing: 10) {
                NTextField("Ask, search, or describe an agent", text: $emptyField, metrics: .askPinned)
                NButton(systemImage: "sparkles", label: "Ask", glyph: 16, corner: .pill, box: 38) {}
            }

            GalleryLabel("1a docked — 14 pt · 9/15 · 40 · `arrow.up` 17")
            HStack(spacing: 10) {
                NTextField("Ask, search, or describe an agent", text: $emptyField, metrics: .askDocked)
                NButton(systemImage: "arrow.up", label: "Ask", glyph: 17, corner: .pill, box: 40) {}
            }

            GalleryLabel("2a focus / 2b — 14 pt · 10/16 · 44 · accent border · `arrow.up` 18")
            HStack(spacing: 10) {
                NTextField("Ask, search, or describe an agent", text: $emptyField,
                           metrics: .askCentred, showsFocusRing: true)
                NButton(systemImage: "arrow.up", label: "Ask", glyph: 18, corner: .pill, box: 44) {}
            }

            GalleryLabel("1f thread — 13.5 pt · 10/15 · `sparkles` 16")
            HStack(spacing: 10) {
                NTextField("Reply, or ask for a draft…", text: $emptyField, metrics: .askThread)
                NButton(systemImage: "sparkles", label: "Ask for a draft", glyph: 16, corner: .pill, box: 38) {}
            }

            GalleryLabel("1h search pill — 13.5 pt · 8/14 · no button")
            NTextField("Search notes", text: $emptyField, metrics: .searchPill)

            GalleryLabel("Very long unbroken value")
            NTextField("Token", text: $longField)
        }
    }

    // MARK: 6 · Cards

    private var cardSection: some View {
        GallerySection("Cards", id: "cards") {
            GalleryLabel("Divider border — kicker / title / body / meta")
            NCard(
                kicker: "Launch Digest",
                title: "4 announcements summarised",
                body: "Three of them mention the pricing change; the fourth is a "
                    + "reminder about Thursday's review.",
                meta: "9 minutes ago · 4 sources"
            )

            // 1a's draft card, to the mockup's own numbers: padding 15/15/13,
            // internal gaps 10 / 12 / 14 — and the card **closes before the
            // buttons**. The buttons are a sibling with margin-top 16, gap 8.
            GalleryLabel("Accent border — the draft-agent card (1a), buttons outside")
            VStack(alignment: .leading, spacing: 0) {
                NCard(border: .accent, padding: NCardPadding.draftAgent, spacing: 0) {
                    NCardKicker("Draft agent")
                    NCardTitle("Job Search Tracker", size: 19)
                        .padding(.top, 10)
                    // `When {when_span}, {do_span}.` — the two spans are
                    // underlined 1 pt accent and are separately tappable on the
                    // real screen. DESIGN.md §3 1a; the agent object carries
                    // `when_span` / `do_span`, so the sentence is composed,
                    // never parsed.
                    Text(Self.draftSentence)
                        .font(Theme.Font.body(15))
                        .nadeLineHeight(1.85, size: 15)
                        .fixedSize(horizontal: false, vertical: true)
                        .padding(.top, 12)
                    HStack(spacing: 6) {
                        NTag("write_note", style: .outline)
                        NTag("Approval required", style: .neutral)
                        NTag("Draft", style: .neutral)
                    }
                    .padding(.top, 14)
                }
                HStack(spacing: 8) {
                    NButton("Save as draft", variant: .primary) {}
                    NButton("Start over", variant: .secondary) {}
                }
                .padding(.top, 16)
            }

            GalleryLabel("1f agent card — padding 14/15, kicker row, buttons inside")
            NCard(border: .accent, padding: NCardPadding.agentCard, spacing: 0) {
                HStack(alignment: .firstTextBaseline, spacing: 8) {
                    SectionEyebrow("Job Search Tracker", color: Theme.Color.accent)
                    Spacer(minLength: Theme.Space.s2)
                    Text("waiting on you")
                        .font(Theme.Font.body(11))
                        .foregroundStyle(Theme.Color.ink62)
                }
                Text("Two next steps found. Ready to add them to Notion → Jobs.")
                    .font(Theme.Font.body(14))
                    .nadeLineHeight(1.7, size: 14)
                    .foregroundStyle(Theme.Color.ink)
                    .fixedSize(horizontal: false, vertical: true)
                    .padding(.top, 9)
                HStack(spacing: 8) {
                    NButton("Save note", variant: .primary) {}
                    NButton("Edit", variant: .secondary) {}
                    NButton("Skip", variant: .ghost) {}
                }
                .padding(.top, 12)
            }

            GalleryLabel("Long unbroken string")
            NCard(title: Self.pathological, body: Self.pathological, meta: "now")
        }
    }

    // MARK: 7 · Tags and chips

    private var tagSection: some View {
        GallerySection("Tags · chips", id: "tags") {
            GalleryLabel("Tag — neutral / outline / accent")
            HStack(spacing: Theme.Space.s2) {
                NTag("Draft", style: .neutral)
                NTag("search_mail", style: .outline)
                NTag("Published", style: .accent)
                NTag("", style: .neutral)
            }

            GalleryLabel("Chip — the mail filter row")
            ScrollView(.horizontal, showsIndicators: false) {
                HStack(spacing: 8) {
                    ForEach(["All mail", "Recruiters", "Receipts", "Flights", "Newsletters"], id: \.self) { name in
                        NChip(name, isSelected: chip == name) { chip = name }
                    }
                }
                .padding(.horizontal, 1)
            }

            GalleryLabel("Chip — selected and unselected, side by side")
            HStack(spacing: 8) {
                NChip("Selected", isSelected: true) {}
                NChip("Unselected", isSelected: false) {}
            }

            GalleryLabel("Section eyebrow")
            VStack(alignment: .leading, spacing: Theme.Space.s1) {
                SectionEyebrow("Accounts")
                SectionEyebrow("Labels")
                SectionEyebrow("Draft agent", color: Theme.Color.accent)
            }
        }
    }

    // MARK: 8 · Controls

    private var controlSection: some View {
        GallerySection("Controls", id: "controls") {
            GalleryLabel("Segmented — DS `7/12` @13 · 1c `6/14` @13 · 1i `5/12` @12")
            VStack(alignment: .leading, spacing: Theme.Space.s2) {
                NSegmented(options: ["Draft", "Published"], selection: $status, metrics: .ds) { $0 }
                NSegmented(options: ["Draft", "Published"], selection: $status, metrics: .status) { $0 }
                NSegmented(options: NoteMode.allCases, selection: $segmented, metrics: .noteMode) { $0.rawValue }
            }

            GalleryLabel("Segmented — long labels on a 320 pt row (they compress, never overflow)")
            NSegmented(
                options: ["Everything that ever arrived", "Only the unread ones"],
                selection: .constant("Only the unread ones")
            ) { $0 }
                .frame(width: 320, alignment: .leading)

            GalleryLabel("Radio rows — 1c Invocation (label 15 · gap 11 · padding 11 · top divider)")
            VStack(spacing: 0) {
                Hairline()
                NRadioRow("When mail arrives", hint: "Runs on its own, on matching mail",
                          isSelected: invocation == .onMail, metrics: .invocation) { invocation = .onMail }
                Hairline()
                NRadioRow("Only when I ask", hint: "You tap Run on the agent",
                          isSelected: invocation == .manual, metrics: .invocation) { invocation = .manual }
                Hairline()
                NRadioRow("On a schedule", hint: "Daily, weekly, or a custom repeat",
                          isSelected: invocation == .onSchedule, metrics: .invocation) { invocation = .onSchedule }
            }

            GalleryLabel("Radio rows — 1d Ends (label 14 · gap 11 · padding 9 · no dividers · value spoken)")
            VStack(spacing: 0) {
                NRadioRow("Never", isSelected: ends == .never, metrics: .ends) { ends = .never }
                    .accessibilityIdentifier("ends.never")
                NRadioRow("On", value: "6 Jan 2027",
                          isSelected: ends == .onDate, metrics: .ends) { ends = .onDate }
                    .accessibilityIdentifier("ends.onDate")
                NRadioRow("After", value: "13 runs",
                          isSelected: ends == .after, metrics: .ends) { ends = .after }
                    .accessibilityIdentifier("ends.after")
            }

            GalleryLabel("Toggle — 46 × 26, 20 pt knob, 0.18 s ease")
            VStack(alignment: .leading, spacing: Theme.Space.s3) {
                HStack(spacing: Theme.Space.s3) {
                    Text("On").font(Theme.Font.body(14)).frame(width: 34, alignment: .leading)
                    NToggle("Ask before it acts", isOn: $approvalOn)
                }
                HStack(spacing: Theme.Space.s3) {
                    Text("Off").font(Theme.Font.body(14)).frame(width: 34, alignment: .leading)
                    NToggle("Ask before it acts", isOn: $approvalOff)
                }
                HStack(alignment: .center, spacing: Theme.Space.s2) {
                    VStack(alignment: .leading, spacing: 2) {
                        Text("Ask before it acts").font(Theme.Font.body(14))
                        Text("You approve each draft or change")
                            .font(Theme.Font.body(12))
                            .foregroundStyle(Theme.Color.ink55)
                    }
                    Spacer(minLength: Theme.Space.s2)
                    NToggle("Ask before it acts", isOn: $approvalOn)
                }
            }
            .foregroundStyle(Theme.Color.ink)

            // 1d's row is `align-items: center; gap: 12` end to end — the same
            // 12 between the label, the boxes and the count.
            GalleryLabel("Stepper — 1d “Repeat every” (row gap 12, chip inner gap 8)")
            HStack(spacing: NStepper.gap) {
                Text("Repeat every").font(Theme.Font.body(14)).foregroundStyle(Theme.Color.ink)
                NStepper("Repeat every", value: $every, in: 1...30)
                Spacer(minLength: 0)
                UnitChip("week")
            }
        }
    }

    // MARK: 9 · Tab bar

    private var tabBarSection: some View {
        GallerySection("Tab bar", id: "tabbar") {
            GalleryLabel("Live — tap to change the selection")
            NTabBar(selection: $tab)

            GalleryLabel("Each tab active")
            VStack(spacing: Theme.Space.s3) {
                ForEach(NTab.allCases) { active in
                    NTabBar(selection: .constant(active))
                        .accessibilityHidden(true)
                }
            }
        }
    }

    // MARK: 10 · Edge cases

    private var edgeSection: some View {
        GallerySection("Edge cases", id: "edges") {
            GalleryLabel("Empty strings")
            HStack(spacing: Theme.Space.s2) {
                NButton("", variant: .primary) {}
                NTag("", style: .accent)
                NChip("", isSelected: true) {}
            }

            GalleryLabel("200-character unbroken token")
            VStack(alignment: .leading, spacing: Theme.Space.s2) {
                NTag(Self.pathological, style: .outline)
                NChip(Self.pathological, isSelected: false) {}
                NButton(Self.pathological, variant: .secondary) {}
            }

            GalleryLabel("Right-to-left layout")
            VStack(alignment: .leading, spacing: Theme.Space.s2) {
                NToggle("Ask before it acts", isOn: $approvalOn)
                HStack(spacing: NStepper.gap) {
                    NStepper("Repeat every", value: $every, in: 1...30)
                    Spacer(minLength: 0)
                    UnitChip("week")
                }
                NSegmented(options: NoteMode.allCases, selection: $segmented, metrics: .noteMode) { $0.rawValue }
                NTabBar(selection: .constant(.mail)).accessibilityHidden(true)
            }
            .environment(\.layoutDirection, .rightToLeft)

            // EDGE (E17). Driven by `HitTargetUITests`, which reads each
            // element's frame: the drawn box is the design's, the target is 44.
            GalleryLabel("Hit targets — every control below draws at its design "
                         + "size and answers taps over 44 × 44")
            HStack(spacing: Theme.Space.s6) {
                NChip("Receipts", isSelected: hitChip) { hitChip.toggle() }
                    .accessibilityIdentifier("hit.chip")
                NToggle("Ask before it acts", isOn: $approvalOff)
                    .accessibilityIdentifier("hit.toggle")
                NButton(systemImage: "arrow.up", label: "Ask", glyph: 17, corner: .pill, box: 38) {}
                    .accessibilityIdentifier("hit.icon")
            }
            HStack(spacing: Theme.Space.s6) {
                NStepper("Repeat every", value: $every, in: 1...30)
                    .accessibilityIdentifier("hit.stepper")
                NSegmented(options: ["Draft", "Published"], selection: $status, metrics: .status) { $0 }
                    .accessibilityIdentifier("hit.segmented")
            }
        }
    }

    // MARK: 11 · Largest accessibility size

    /// Its own section, and its own screenshot. The previous delivery folded
    /// this into "Edge cases", the screenshot cut off mid-card, and the AX5
    /// buttons, icon buttons and tab bar were never actually looked at.
    private var accessibilitySizeSection: some View {
        GallerySection("Largest accessibility size (AX5)", id: "ax5") {
            VStack(alignment: .leading, spacing: Theme.Space.s3) {
                NCard(kicker: "Launch Digest", title: "4 announcements", body: "Body text at AX5.", meta: "9m")
                HStack(spacing: Theme.Space.s2) {
                    NButton("Approve", variant: .primary) {}
                    NButton("Skip", variant: .ghost) {}
                }
                // Icon buttons were missing from the AX5 block, which is
                // exactly where a fixed box and a scaling glyph collide.
                HStack(spacing: Theme.Space.s2) {
                    NButton(systemImage: "sparkles", label: "Ask", glyph: 16, corner: .pill, box: 38) {}
                    NButton(systemImage: "arrow.up", label: "Ask", glyph: 17, corner: .pill, box: 40) {}
                    NButton(systemImage: "arrow.up", label: "Ask", glyph: 18, corner: .pill, box: 44) {}
                    NButton(systemImage: "plus", label: "New") {}
                }
                NTextField("Ask, search, or describe an agent", text: $emptyField, metrics: .askDocked)
                HStack(spacing: Theme.Space.s2) {
                    NTag("Draft", style: .neutral)
                    NChip("Recruiters", isSelected: true) {}
                }
            }
            .dynamicTypeSize(.accessibility5)
        }
    }

    /// The rest of AX5. Two sections, not one, because a single one is taller
    /// than the screen and its screenshot would be cut off — which is exactly
    /// how the first delivery came to ship an AX5 claim nobody had looked at.
    private var accessibilitySizeSectionContinued: some View {
        GallerySection("AX5, continued", id: "ax5b") {
            VStack(alignment: .leading, spacing: Theme.Space.s3) {
                NStepper("Repeat every", value: $every, in: 1...30)
                NSegmented(options: ["Draft", "Published"], selection: $status, metrics: .status) { $0 }
                NRadioRow("On a schedule", hint: "Daily, weekly, or a custom repeat",
                          isSelected: true, metrics: .invocation) {}
                NTabBar(selection: .constant(.calendar)).accessibilityHidden(true)
            }
            .dynamicTypeSize(.accessibility5)
        }
    }

    // MARK: 12 · Reduce Motion

    private var motionSection: some View {
        GallerySection("Reduce Motion", id: "motion") {
            // `\.accessibilityReduceMotion` is a read-only environment value,
            // so it cannot be forced here. `NToggle` reads it and asks
            // `Theme.Motion.toggle(reduceMotion:)`, which returns nil — asserted
            // in ThemeTests.testToggleAnimationRespectsReduceMotion.
            GalleryLabel("The knob jumps instead of sliding "
                         + "(Theme.Motion.toggle returns no animation)")
            NToggle("Ask before it acts", isOn: $approvalOff)
        }
    }
}

// MARK: - Gallery chrome

private struct GallerySection<Content: View>: View {
    let title: String
    let id: String
    let content: Content

    init(_ title: String, id: String, @ViewBuilder content: () -> Content) {
        self.title = title
        self.id = id
        self.content = content()
    }

    var body: some View {
        VStack(alignment: .leading, spacing: Theme.Space.s3) {
            Hairline()
            Text(title)
                .font(Theme.Font.heading(20))
                .foregroundStyle(Theme.Color.ink)
                .padding(.top, Theme.Space.s3)
            content
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(.horizontal, Theme.Space.screen)
        .padding(.bottom, Theme.Space.s6)
        .id(id)
    }
}

private struct GalleryLabel: View {
    let text: String
    init(_ text: String) { self.text = text }

    var body: some View {
        Text(text)
            .font(Theme.Font.body(11))
            .foregroundStyle(Theme.Color.ink50)
            .fixedSize(horizontal: false, vertical: true)
            .padding(.top, Theme.Space.s2)
    }
}

struct TypeSpec: Identifiable {
    let id = UUID()
    let size: CGFloat
    let use: String
    init(_ size: CGFloat, _ use: String) {
        self.size = size
        self.use = use
    }
}

private struct TypeRow: View {
    let size: CGFloat
    let use: String
    let font: Font

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: Theme.Space.s2) {
            Text(Double(size).formatted(.number.precision(.fractionLength(0...1))))
                .font(Theme.Font.body(10))
                .foregroundStyle(Theme.Color.ink50)
                .tabularNumerals()
                .frame(width: 26, alignment: .trailing)
            Text("Good morning")
                .font(font)
                .foregroundStyle(Theme.Color.ink)
                .lineLimit(1)
            Spacer(minLength: Theme.Space.s2)
            Text(use)
                .font(Theme.Font.body(9.5))
                .foregroundStyle(Theme.Color.ink50)
                .lineLimit(1)
        }
    }
}

private struct SwatchGrid: View {
    struct Swatch: Identifiable {
        let id = UUID()
        let name: String
        let color: Color
        init(_ name: String, _ color: Color) {
            self.name = name
            self.color = color
        }
    }

    let swatches: [Swatch]

    private let columns = [GridItem(.adaptive(minimum: 88), spacing: Theme.Space.s2)]

    var body: some View {
        LazyVGrid(columns: columns, alignment: .leading, spacing: Theme.Space.s2) {
            ForEach(swatches) { swatch in
                VStack(alignment: .leading, spacing: 3) {
                    RoundedRectangle(cornerRadius: Theme.Radius.sm, style: .circular)
                        .fill(swatch.color)
                        .frame(height: 34)
                        .overlay {
                            RoundedRectangle(cornerRadius: Theme.Radius.sm, style: .circular)
                                .strokeBorder(Theme.Color.divider, lineWidth: Theme.Stroke.border)
                        }
                    Text(swatch.name)
                        .font(Theme.Font.body(10))
                        .foregroundStyle(Theme.Color.ink)
                        .lineLimit(1)
                    // Resolved at runtime from the token itself, so this line is
                    // evidence rather than a comment that can drift.
                    Text(GalleryHex.string(for: swatch.color))
                        .font(Theme.Font.body(9.5))
                        .foregroundStyle(Theme.Color.ink50)
                        .tabularNumerals()
                        .lineLimit(1)
                }
            }
        }
    }
}

enum GalleryHex {
    static func string(for color: Color) -> String {
        let ui = UIColor(color)
        var r: CGFloat = 0, g: CGFloat = 0, b: CGFloat = 0, a: CGFloat = 0
        guard ui.getRed(&r, green: &g, blue: &b, alpha: &a) else { return "—" }
        let hex = String(
            format: "%02X%02X%02X",
            Int((r * 255).rounded()), Int((g * 255).rounded()), Int((b * 255).rounded())
        )
        return a < 0.999
            ? hex + " · " + Double(a * 100).formatted(.number.precision(.fractionLength(0))) + "%"
            : hex
    }
}

private struct SpaceRule: View {
    let name: String
    let value: CGFloat
    init(_ name: String, _ value: CGFloat) {
        self.name = name
        self.value = value
    }

    var body: some View {
        HStack(spacing: Theme.Space.s2) {
            Text(name)
                .font(Theme.Font.body(11))
                .foregroundStyle(Theme.Color.ink60)
                .frame(width: 92, alignment: .leading)
            Rectangle()
                .fill(Theme.Color.accent)
                .frame(width: value, height: 8)
            Text(Double(value).formatted(.number.precision(.fractionLength(0...1))))
                .font(Theme.Font.body(10))
                .foregroundStyle(Theme.Color.ink50)
                .tabularNumerals()
        }
    }
}

private struct RadiusChip: View {
    let name: String
    let radius: CGFloat
    init(_ name: String, _ radius: CGFloat) {
        self.name = name
        self.radius = radius
    }

    var body: some View {
        VStack(spacing: 4) {
            RoundedRectangle(cornerRadius: radius, style: .circular)
                .strokeBorder(Theme.Color.divider, lineWidth: Theme.Stroke.border)
                .frame(width: 56, height: 34)
            Text(name)
                .font(Theme.Font.body(10))
                .foregroundStyle(Theme.Color.ink50)
        }
    }
}

private struct ShadowChip: View {
    let name: String
    let shadow: Theme.Shadow
    init(_ name: String, _ shadow: Theme.Shadow) {
        self.name = name
        self.shadow = shadow
    }

    var body: some View {
        VStack(spacing: 6) {
            RoundedRectangle(cornerRadius: Theme.Radius.lg, style: .circular)
                .fill(Theme.Color.surface)
                .frame(width: 72, height: 44)
                .nadeShadow(shadow)
            Text(name)
                .font(Theme.Font.body(10))
                .foregroundStyle(Theme.Color.ink50)
        }
    }
}

/// 1d's unit chip: 1 pt border, radius 4, padding 6 × 12, 14 pt, **inner gap 8**,
/// 10 pt accent ▼ (DESIGN.md §3 1d; the mockup's own `gap:8px`).
private struct UnitChip: View {
    let unit: String
    init(_ unit: String) { self.unit = unit }

    var body: some View {
        HStack(spacing: 8) {
            Text(unit).font(Theme.Font.body(14)).foregroundStyle(Theme.Color.ink)
            NIcon("chevron.down", size: 10, weight: .light, relativeTo: .caption2)
                .foregroundStyle(Theme.Color.accent)
        }
        .padding(.vertical, 6)
        .padding(.horizontal, 12)
        .overlay {
            RoundedRectangle(cornerRadius: Theme.Radius.md, style: .circular)
                .strokeBorder(Theme.Color.divider, lineWidth: Theme.Stroke.border)
        }
    }
}

/// A button frozen in its pressed state, so the gallery can show the `:active`
/// wash without anyone having to hold a finger on the screen.
private struct PressedButton: View {
    let title: String
    let variant: NButtonStyle.Variant

    init(_ title: String, variant: NButtonStyle.Variant) {
        self.title = title
        self.variant = variant
    }

    var body: some View {
        NButton(title, variant: variant) {}
            .background {
                RoundedRectangle(cornerRadius: Theme.Radius.md, style: .circular)
                    .fill(pressWash)
            }
    }

    private var pressWash: Color {
        switch variant {
        case .primary, .icon: Theme.Color.pressAccentStrong
        case .secondary: Theme.Color.pressInk
        case .ghost: Theme.Color.pressAccentSoft
        }
    }
}

#Preview {
    GalleryView(initialSection: nil).preferredColorScheme(.light)
}

#endif

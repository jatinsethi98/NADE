//
//  MailGeometryTests.swift
//  NADETests
//
//  P2 A10 — the mail screens' geometry, measured.
//
//  Under D33/D40/D49: every expectation below is built from `Theme` constants
//  and `docs/DESIGN.md`'s own numbers, and **no term is read back off the view
//  under test**. `MailRow.Metrics.paddingTop` appearing on both sides of an
//  assertion would prove only that a constant equals itself, which is how
//  thirteen values in P1 stayed green while the views rendered something else.
//
//  Everything is in **points**. `RenderMeasure` hosts at 3 px/pt and its
//  `Bitmap.bounds` converts back, so a raw-pixel expectation mixed in here
//  would be three times the number it looks like.
//

import XCTest
import SwiftUI
@testable import NADE

@MainActor
final class MailGeometryTests: XCTestCase {

    /// One place that knows `ThreadMessageBlock`'s shape.
    ///
    /// Four call sites each repeated the same six arguments on a ~160-column
    /// line, so every signature change touched all four — which is exactly what
    /// happened twice in this change set.
    ///
    /// `attachmentSource` is a real `FixtureMailSource` rather than a stub: the
    /// measurements below all use messages whose `body_html` is nil, so no
    /// "View original" button enters the layout and nothing is ever fetched.
    private func block(
        _ message: WireMessage,
        accountEmail: String,
        now: Date,
        topGap: CGFloat
    ) -> ThreadMessageBlock {
        ThreadMessageBlock(message: message, accountEmail: accountEmail, now: now,
                           topGap: topGap, showsOriginal: .constant([]),
                           onOpenAttachment: { _, _ in },
                           attachmentSource: FixtureMailSource(isEmpty: false))
    }

    /// The design frame, and the two tolerances — all three from
    /// `RenderMeasure`, where `ComponentGeometryTests` derives them too.
    private let width = RenderMeasure.designWidth
    private var cssBox: CGFloat { RenderMeasure.cssBox }
    private var grid: CGFloat { RenderMeasure.grid }

    private let now = ContractFixture.pinnedNow

    private func row(agentNote: String?) throws -> WireThreadRow {
        let page = try ContractFixture.decode(WireThreadPage.self, from: "threads")
        let base = try XCTUnwrap(page.threads.first)
        return WireThreadRow(
            id: base.id, subject: base.subject, snippet: base.snippet,
            fromName: base.fromName, fromEmail: base.fromEmail, ts: base.ts,
            unread: base.unread, msgCount: base.msgCount, agentNote: agentNote
        )
    }

    // MARK: - 1e, the mail row

    /// `13 / 22 / 13 / 14`, gap 9, and a column of
    /// `from(15) + 2 + subject(14) + 2 + snippet(13 @ 1.5) [+ 7 + note(11) + 1]`.
    /// Every size and every gap is DESIGN.md §1e's, written out rather than
    /// referenced, so a change to `MailRow.Metrics` alone cannot keep this green.
    func testTheMailRowIsTheDesignsHeightWithAndWithoutAnAgentNote() throws {
        let body = Theme.LineHeight.body   // 1.55, the DS's inherited line-height

        let fromLine = 15 * body
        let subjectLine = 14 * body
        let snippetLine = 13 * 1.5         // §1e overrides the snippet to 1.5
        let noteLine = 11 * body

        let withoutNote = 13 + fromLine + 2 + subjectLine + 2 + snippetLine + 13
        let withNote = withoutNote + 7 + noteLine + 1   // + the note's 1 pt rule gap

        let bare = RenderMeasure.height(
            of: MailRow(row: try row(agentNote: nil), now: now, action: {}), width: width
        )
        XCTAssertMeasures(bare, withoutNote, accuracy: cssBox, "1e row without an agent note")

        let noted = RenderMeasure.height(
            of: MailRow(row: try row(agentNote: "Job Search Tracker · two next steps"),
                        now: now, action: {}),
            width: width
        )
        XCTAssertMeasures(noted, withNote, accuracy: cssBox, "1e row with an agent note")

        XCTAssertGreaterThan(noted, bare, "the agent note added no height at all")
    }

    /// The asymmetric inset, in pixels because no layout query can see it:
    /// "1e mail rows | 22 right, **14 left** (the unread dot hangs in the
    /// inset)". The dot is the leftmost ink on the row, and the time is the
    /// rightmost.
    func testTheUnreadDotHangsInTheFourteenPointInset() throws {
        let bitmap = try XCTUnwrap(
            RenderMeasure.bitmap(
                of: MailRow(row: try row(agentNote: nil), now: now, action: {})
                    .frame(width: width),
                over: Theme.Color.bg
            )
        )

        // The dot is the only accent-coloured ink on an unread row.
        let dot = try XCTUnwrap(
            bitmap.bounds(matching: RenderMeasure.components(of: Theme.Color.accent), tolerance: 0.08),
            "no accent dot on an unread row"
        )
        XCTAssertMeasures(dot.minX, 14, accuracy: grid, "the dot's left edge")
        XCTAssertMeasures(dot.width, 6, accuracy: grid, "the dot's diameter")
        XCTAssertMeasures(dot.height, 6, accuracy: grid, "the dot's diameter")
        // `padding(.top, 13)` on the row plus the dot's own `margin-top: 8`.
        XCTAssertMeasures(dot.minY, 13 + 8, accuracy: cssBox, "the dot's top edge")
    }

    /// A read row keeps the dot's space so both columns start at the same x.
    func testAReadRowKeepsTheDotsSpaceSoTheColumnsStayAligned() throws {
        let unread = try row(agentNote: nil)
        let read = WireThreadRow(
            id: unread.id, subject: unread.subject, snippet: unread.snippet,
            fromName: unread.fromName, fromEmail: unread.fromEmail, ts: unread.ts,
            unread: false, msgCount: unread.msgCount, agentNote: nil
        )
        XCTAssertMeasures(
            RenderMeasure.height(of: MailRow(row: read, now: now, action: {}), width: width),
            RenderMeasure.height(of: MailRow(row: unread, now: now, action: {}), width: width),
            accuracy: grid,
            "a read row is a different height from an unread one"
        )
    }

    /// EDGE (E3): a subject with no spaces truncates instead of widening the row.
    func testAPathologicalSubjectDoesNotWidenTheRow() throws {
        let base = try row(agentNote: nil)
        let hostile = WireThreadRow(
            id: base.id, subject: String(repeating: "A", count: 200),
            snippet: String(repeating: "B", count: 200),
            fromName: String(repeating: "C", count: 200), fromEmail: base.fromEmail,
            ts: base.ts, unread: base.unread, msgCount: base.msgCount, agentNote: nil
        )
        let size = RenderMeasure.size(
            of: MailRow(row: hostile, now: now, action: {}).frame(width: width),
            proposedWidth: width
        )
        XCTAssertMeasures(size.width, width, accuracy: grid, "the row grew past the screen")
        XCTAssertMeasures(
            size.height,
            RenderMeasure.height(of: MailRow(row: base, now: now, action: {}), width: width),
            accuracy: cssBox,
            "a 200-character token wrapped instead of truncating"
        )
    }

    // MARK: - 1e, the header

    /// `60 / 22 / 10`, around a 23 pt title on the DS's inherited line-height.
    func testTheMailListHeaderIsSixtyOverTheTitleOverTen() {
        let expected = 60 + 23 * Theme.LineHeight.body + 10
        let measured = RenderMeasure.height(
            of: MailListHeader(title: "Inbox", accountEmail: "jatinsethi98@gmail.com", back: {}),
            width: width
        )
        XCTAssertMeasures(measured, expected, accuracy: cssBox, "1e header band")
    }

    // MARK: - 1g

    /// `12 / 22`, `email(15)` over `sub-label(12)`, both at the inherited 1.55.
    func testTheAccountRowIsTheDesignsHeightAndIsFilledEdgeToEdge() {
        let expected = 12 + 15 * Theme.LineHeight.body + 12 * Theme.LineHeight.body + 12
        let view = AccountRow(email: "jatinsethi98@gmail.com", status: .ok, unread: 12, action: {})

        XCTAssertMeasures(
            RenderMeasure.height(of: view, width: width), expected,
            accuracy: cssBox, "1g account row"
        )

        // "**accent-100 fill**", and full-bleed rather than inset.
        let bitmap = try? XCTUnwrap(RenderMeasure.bitmap(of: view.frame(width: width), over: Theme.Color.bg))
        let fill = bitmap?.bounds(matching: RenderMeasure.components(of: Theme.Color.accent100), tolerance: 0.05)
        XCTAssertMeasures(fill?.width ?? 0, width, accuracy: grid, "the accent-100 fill is inset")
    }

    /// `13 / 22`, `name(15)` + a 12 pt second line at top 3.
    func testTheMailboxRowIsTheDesignsHeight() {
        let expected = 13 + 15 * Theme.LineHeight.body + 3 + 12 * Theme.LineHeight.body + 13
        let box = WireMailbox(id: "INBOX", name: "Inbox", kind: .system, unread: 12, total: 201)
        XCTAssertMeasures(
            RenderMeasure.height(of: MailboxRow(mailbox: box, action: {}), width: width),
            expected, accuracy: cssBox, "1g label row"
        )
    }

    // MARK: - 1f

    /// `padding 14 / 15` around a 10 pt kicker (with its 9 pt gap) and a 14 pt
    /// summary at line-height 1.7, inside a 1 pt accent border.
    func testTheAgentCardIsTheDesignsBoxAndItsBorderIsAccent() throws {
        // A one-line summary, so the arithmetic is the design's numbers and not
        // a guess at where a sentence wraps. The fixture's own summary wraps to
        // two lines at this width, which would make the expectation depend on
        // the text rather than on the spec.
        let card = WireAgentCard(runId: "r", agentName: "Job Search Tracker",
                                 status: .pendingApproval, summary: "Two next steps.",
                                 feedItemId: "f")
        // `item: nil` is the no-buttons card, which is what this measurement
        // is of: the box §1f draws. The buttons have their own row and their
        // own geometry, and mixing them in would make this assertion depend on
        // a button's height rather than on the card's padding.
        let view = ThreadAgentCard(card: card, item: nil, onApprove: {}, onSkip: {})
            .frame(width: width)

        // `padding 14 / 15`, a 10 pt kicker on the inherited 1.55 with its 9 pt
        // gap, then a 14 pt summary at line-height 1.7. `NCard` draws its border
        // with `strokeBorder` inside an overlay, so it adds no height.
        let kicker = 10 * Theme.LineHeight.body
        let summary = 14 * 1.7
        let expected = 14 + kicker + 9 + summary + 14

        XCTAssertMeasures(
            RenderMeasure.height(of: view, width: width), expected,
            accuracy: cssBox, "1f agent card"
        )

        // The accent border is what distinguishes this card from every other.
        let bitmap = try XCTUnwrap(RenderMeasure.bitmap(of: view, over: Theme.Color.bg))
        let accent = try XCTUnwrap(
            bitmap.bounds(matching: RenderMeasure.components(of: Theme.Color.accent), tolerance: 0.12),
            "the agent card has no accent ink at all — its border is not accent"
        )
        XCTAssertGreaterThan(accent.width, width * 0.8, "the accent border does not span the card")
    }

    /// `60 / 20 / 10` around a 13 pt title on the DS's inherited line height.
    /// It was short before this existed: the title carried no explicit box.
    func testTheThreadNavBandIsSixtyOverThirteenPointsOverTen() {
        let expected = 60 + 13 * Theme.LineHeight.body + 10
        XCTAssertMeasures(
            RenderMeasure.height(of: ThreadNavBar(title: "To Reply", back: {}), width: width),
            expected, accuracy: cssBox, "1f nav band"
        )
    }

    /// `62 / 22 / 12` around a 23 pt title — the *same* band as 1g's, which is
    /// how the shortfall was visible: the checked-in 1k screenshot put its
    /// divider higher than 1g's for an identical specification.
    func testTheSettingsHeaderIsTheSameBandAs1g() {
        let expected = 62 + 23 * Theme.LineHeight.body + 12
        XCTAssertMeasures(
            RenderMeasure.height(of: SettingsHeader(back: {}), width: width),
            expected, accuracy: cssBox, "1k header band"
        )
    }

    /// The subject is the one place in the app on a 1.2 line-height.
    func testTheThreadSubjectUsesTheHeadingsOwnLineHeight() {
        let expected = 24 * 1.2
        let measured = RenderMeasure.height(
            of: Text("Staff Product Designer at Kettle")
                .font(Theme.Font.heading(24))
                .nadeLineHeight(1.2, size: 24, face: Theme.Font.PostScriptName.headingSemibold)
                .lineLimit(1),
            width: width
        )
        XCTAssertMeasures(measured, expected, accuracy: cssBox, "1f subject line box")
    }

    func testAMessageBlockIsExactlyMetaRuleAndBody() throws {
        let message = WireMessage(
            gmailId: "m", fromName: "Priya", fromEmail: "p@k.com",
            to: [], cc: [], ts: now,
            bodyText: "One line.", bodyHtml: nil, labelIds: [], attachments: []
        )
        let meta = 13 * Theme.LineHeight.body
        let rule = 14 + Theme.Stroke.border + 14
        let body = 14.5 * 1.75
        let expected = 10 + meta + rule + body

        XCTAssertMeasures(
            RenderMeasure.height(
                of: block(message, accountEmail: "me@nade.local",
                                     now: now, topGap: 10),
                width: width - 2 * Theme.Space.screen
            ),
            expected, accuracy: cssBox, "1f message block"
        )
    }

    /// The paragraph gap is the mockup's 12, and it is real: the same text as
    /// one paragraph is shorter by exactly `(n − 1) × 12`.
    func testParagraphsAreSeparatedByTheMockupsTwelvePoints() throws {
        // Shadows the outer helper deliberately: this one varies the *text*.
        func measure(_ text: String) -> CGFloat {
            RenderMeasure.height(
                of: self.block(
                    WireMessage(gmailId: "m", fromName: "A", fromEmail: "a@b.c",
                                to: [], cc: [], ts: now, bodyText: text,
                                bodyHtml: nil, labelIds: [], attachments: []),
                    accountEmail: "a@b.c", now: now, topGap: 10
                ),
                width: width - 2 * Theme.Space.screen
            )
        }
        let one = measure("One.")
        let three = measure("One.\n\nTwo.\n\nThree.")
        let bodyLine = 14.5 * 1.75
        XCTAssertMeasures(three - one, 2 * (bodyLine + 12), accuracy: cssBox,
                          "the gap between paragraphs is not the design's 12")
    }

    // MARK: - The body is the sender's, not ours

    /// `ThreadMessageBlock` splits `body_text` on blank lines to produce the
    /// mockup's 12 pt paragraph gap. It used to also trim each part and drop
    /// empty ones, which quietly rewrote the mail: indentation in pasted code,
    /// a deliberately blank stanza, trailing whitespace. The parser on the
    /// other side went to some trouble to produce this text faithfully.
    func testSplittingIntoParagraphsDoesNotEditTheMessage() throws {
        let body = "  indented line\n\n\n\ntrailing spaces   \n\nlast"
        let message = WireMessage(
            gmailId: "m", fromName: "A", fromEmail: "a@b.c", to: [], cc: [],
            ts: now, bodyText: body, bodyHtml: nil, labelIds: [], attachments: []
        )
        // Reassembling the rendered paragraphs must give back the original.
        let parts = message.bodyText.components(separatedBy: "\n\n")
        XCTAssertEqual(parts.joined(separator: "\n\n"), body)
        XCTAssertTrue(parts.contains { $0.hasPrefix("  ") }, "indentation was the point of this case")
        XCTAssertTrue(parts.contains { $0.hasSuffix("   ") }, "trailing space was the point of this case")

        // And the view renders every one of them, including the empty ones.
        let height = RenderMeasure.height(
            of: block(message, accountEmail: "a@b.c",
                                     now: now, topGap: 10),
            width: width - 2 * Theme.Space.screen
        )
        let single = WireMessage(
            gmailId: "m", fromName: "A", fromEmail: "a@b.c", to: [], cc: [],
            ts: now, bodyText: "last", bodyHtml: nil, labelIds: [], attachments: []
        )
        let shortHeight = RenderMeasure.height(
            of: block(single, accountEmail: "a@b.c",
                                     now: now, topGap: 10),
            width: width - 2 * Theme.Space.screen
        )
        XCTAssertGreaterThan(height, shortHeight, "the blank paragraphs were dropped")
    }

    // MARK: - Captions

    /// The state captions are the design's caption voice: italic 12 pt.
    /// Rendered in the roman by mistake they would be a different width, which
    /// is the failure `Lora-Italic` exists to prevent.
    func testTheStateCaptionIsItalicAndNotTheRoman() {
        let text = StateCopy.emptyMailbox
        let italic = RenderMeasure.size(of: StateCaption(text).frame(width: width))
        let roman = RenderMeasure.size(
            of: Text(text).font(Theme.Font.body(12)).frame(width: width)
        )
        XCTAssertNotEqual(italic.height, roman.height, accuracy: 0.01,
                          "the caption renders identically to the roman — it is not italic")
    }
}

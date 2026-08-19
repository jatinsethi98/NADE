//
//  ComponentGeometryTests.swift
//  NADETests
//
//  Every number here is measured off a hosted view, not read back out of the
//  constant the view is supposed to be using. `ThemeTests` still pins the
//  tokens; this pins what the components *do* with them.
//
//  The CSS each expectation comes from is in the comment beside it. Where a
//  measurement lands a fraction off, that is SwiftUI snapping text to the @3x
//  device grid (⅓ pt); the tolerance is set just wide enough for that and
//  nowhere near wide enough to swallow a missing modifier.
//

import SwiftUI
import UIKit
import XCTest
@testable import NADE

@MainActor
final class ComponentGeometryTests: XCTestCase {

    /// Two roundings onto the device's pixel grid — ⅔ pt at @3x, 1 pt on the
    /// @2x SE. Everything below is stated as a multiple of the real grid rather
    /// than as a fixed fudge, so the expectation tightens on the denser screen
    /// instead of a phone-specific number being baked in.
    private var grid: CGFloat { RenderMeasure.grid }

    /// Grid snapping **plus** the SwiftUI/CSS box-model residual recorded in
    /// IOS_DECISIONS.md D32 (`strokeBorder` draws inside the frame where CSS's
    /// `border` adds; `TextField` reserves a slightly taller line box). ~1.9 pt
    /// at @3x, ~2.2 on the SE — still far below the 3–6 pt that separates any
    /// two of these components, which is what the mutation checks confirmed.
    private var cssBox: CGFloat { RenderMeasure.cssBox }

    /// A `Theme` colour as it lands **on the page**. `RenderMeasure.components`
    /// drops alpha, and half this palette is a translucent ink — `divider` is
    /// `ink` at 16 %, so what a render paints is that blend over `bg`. An
    /// opaque colour (`accent`) comes back unchanged, which is what lets a
    /// caller pass either one without knowing which it has.
    private static func onPageComponents(of color: SwiftUI.Color) -> (r: Double, g: Double, b: Double) {
        var r: CGFloat = 0, g: CGFloat = 0, b: CGFloat = 0, a: CGFloat = 0
        UIColor(color).getRed(&r, green: &g, blue: &b, alpha: &a)
        let ground = RenderMeasure.components(of: Theme.Color.bg)
        return (
            r: ground.r + Double(a) * (Double(r) - ground.r),
            g: ground.g + Double(a) * (Double(g) - ground.g),
            b: ground.b + Double(a) * (Double(b) - ground.b)
        )
    }

    // MARK: - Line height (F1)

    /// DS `body { line-height: 1.55 }`. One line of Lora 15 must occupy
    /// 15 × 1.55 = 23.25, not Lora's own 15 × 1.28 = 19.2.
    func testOneLineOccupiesTheCSSLineBox() {
        for (multiple, size, face, font) in [
            (Theme.LineHeight.body, CGFloat(15), Theme.Font.PostScriptName.bodyRegular, Theme.Font.body(15)),
            (Theme.LineHeight.body, CGFloat(13), Theme.Font.PostScriptName.bodyRegular, Theme.Font.body(13)),
            (Theme.LineHeight.button, CGFloat(14), Theme.Font.PostScriptName.headingSemibold, Theme.Font.heading(14)),
            (Theme.LineHeight.cardTitle, CGFloat(17), Theme.Font.PostScriptName.headingSemibold, Theme.Font.heading(17)),
        ] {
            let measured = RenderMeasure.height(
                of: Text("Handgloves").font(font).nadeLineHeight(multiple, size: size, face: face),
                width: 500
            )
            XCTAssertMeasures(measured, size * multiple, accuracy: grid, "one line at \(size) × \(multiple)")
        }
    }

    /// `n` lines must occupy `n × size × multiple` — which is what makes the
    /// modifier a line *height* rather than a line *gap*. `lineSpacing` alone
    /// gets the gaps right and the block wrong by one gap.
    func testAParagraphOccupiesNTimesTheCSSLineBox() {
        let size: CGFloat = 15
        let multiple = Theme.LineHeight.body
        let text = Text("Wanted to check whether you are open to a conversation about a role here.")
            .font(Theme.Font.body(size))
            .nadeLineHeight(multiple, size: size)

        // Narrow enough to wrap; the exact count is read back from the render.
        let measured = RenderMeasure.height(of: text, width: 120)
        let lines = (measured / (size * multiple)).rounded()
        XCTAssertGreaterThan(lines, 2, "the sample did not wrap — widen the assertion, not the frame")
        XCTAssertMeasures(measured, lines * size * multiple, accuracy: grid, "\(Int(lines)) lines at \(size) × \(multiple)")
    }

    /// The tighter-than-natural case, which `max(0, …)` on `lineSpacing` could
    /// never express: Cormorant's own line box is 1.211 em, and `.card-title`
    /// asks for 1.2.
    func testATighterLineHeightThanTheFaceIsAchievable() throws {
        // Cormorant's box is 1.211 em against a 1.2 target — 0.011 em. Magnified
        // to 200 pt that is 2.2 pt, which clears the ½ pt grid on an @2x screen
        // with room to spare; at 40 pt it does not, and the assertion is a
        // coin toss rather than a check.
        let size: CGFloat = 200
        let face = Theme.Font.PostScriptName.headingSemibold
        let natural = try XCTUnwrap(UIFont(name: face, size: size)).lineHeight
        XCTAssertGreaterThan(natural, size * Theme.LineHeight.cardTitle, "Cormorant is no longer the tighter case")

        // One line, and a frame wide enough that 200 pt type cannot wrap into
        // two — which would measure ~2× the box and pass the "tighter" check
        // for entirely the wrong reason.
        let measured = RenderMeasure.height(
            of: Text("Hg")
                .font(Theme.Font.heading(size))
                .nadeLineHeight(Theme.LineHeight.cardTitle, size: size, face: face)
                .lineLimit(1)
        )
        XCTAssertMeasures(measured, size * Theme.LineHeight.cardTitle, accuracy: grid, "tight line height")
        XCTAssertLessThan(measured, natural, "the box is not tighter than the face's own line")
    }

    // MARK: - Hairline (F9)

    /// The mockup frame is 402 CSS px and the device is 402 pt, so `1px` is
    /// 1 pt. At @3x, `1 / displayScale` would measure 0.333.
    func testHairlineIsOnePoint() {
        XCTAssertMeasures(RenderMeasure.size(of: Hairline(), proposedWidth: 300).height, 1, accuracy: 0.01, "Hairline")
        XCTAssertMeasures(RenderMeasure.size(of: VHairline(), proposedHeight: 300).width, 1, accuracy: 0.01, "VHairline")
        XCTAssertEqual(Theme.Stroke.border, 1, "component outlines are the same weight")
    }

    // MARK: - Buttons (F1, F4)

    /// DS `.btn` — `padding: 9.2 16.56`, `line-height: 1.2` on 14 pt Cormorant.
    func testTextButtonHeightIsPaddingPlusTheCSSLineBox() {
        let measured = RenderMeasure.size(of: NButton("Approve") {})
        XCTAssertMeasures(measured.height, NButton.minHeight, accuracy: grid, "primary button height")
        XCTAssertMeasures(
            measured.width,
            RenderMeasure.typesetWidth("Approve", font: UIFont(name: Theme.Font.PostScriptName.headingSemibold, size: 14)!)
                + 2 * Theme.Space.s3 * 1.2,
            accuracy: cssBox,
            "primary button width = label + 2 × 16.56"
        )
    }

    /// `flex: 1` — 1c's "Run once now", 1d's two footer buttons. The bug this
    /// replaces: `.frame(maxWidth: .infinity)` on the `Button` stretched the
    /// wrapper while the style kept the border at the label's width.
    func testFlexibleButtonFillsItsRowAndTakesItsBorderWithIt() {
        let row: CGFloat = 320
        let flexible = RenderMeasure.size(
            of: NButton("Run once now", variant: .primary, width: .flexible) {},
            proposedWidth: row
        )
        XCTAssertMeasures(flexible.width, row, accuracy: 0.5, "flexible button width")

        let intrinsic = RenderMeasure.size(
            of: NButton("Run once now", variant: .primary, width: .intrinsic) {},
            proposedWidth: row
        )
        XCTAssertLessThan(intrinsic.width, row - 100, "the intrinsic button should stay narrow: \(intrinsic)")

        // Two `flex: 1` buttons with a 10 pt gap share the row exactly.
        let pair = RenderMeasure.size(
            of: HStack(spacing: 10) {
                NButton("Cancel", variant: .secondary, width: .flexible) {}
                NButton("Every 2 weeks", variant: .primary, width: .flexible) {}
            },
            proposedWidth: row
        )
        XCTAssertMeasures(pair.width, row, accuracy: 0.5, "1d footer row width")
    }

    /// EDGE (E4): an empty label must not collapse the box.
    func testEmptyButtonKeepsItsBox() {
        let measured = RenderMeasure.size(of: NButton("", variant: .secondary) {})
        XCTAssertMeasures(measured.height, NButton.minHeight, accuracy: grid, "empty button height")
        XCTAssertGreaterThan(measured.width, 0)
    }

    /// DS `.btn-icon` is 36 × 36; DESIGN.md §2's circles are 38 / 40 / 44.
    func testIconButtonBoxesAreTheDesignSizes() {
        for box in [CGFloat(36), 38, 40, 44] {
            let measured = RenderMeasure.size(
                of: NButton(systemImage: "arrow.up", label: "Ask", corner: .pill, box: box) {}
            )
            XCTAssertMeasures(measured.width, box, accuracy: 0.01, "icon box width")
            XCTAssertMeasures(measured.height, box, accuracy: 0.01, "icon box height")
        }
    }

    /// DESIGN.md §2 gives the send/ask glyph a different size on every screen:
    /// 16 (2a feed, 1f) · 17 (1a) · 18 (2a focus, 2b). It used to be hardcoded
    /// at 17. The `.icon` variant's fixed box hides the difference, so this
    /// measures through the `.ghost` variant, which sizes to its content.
    func testTheIconGlyphSizeReachesTheGlyph() {
        var widths: [CGFloat: CGFloat] = [:]
        for glyph in [CGFloat(16), 17, 18] {
            widths[glyph] = RenderMeasure.size(
                of: NButton(systemImage: "arrow.up", label: "Ask", glyph: glyph, variant: .ghost) {}
            ).width
        }
        XCTAssertEqual(Set(widths.values).count, 3, "the three design glyph sizes render identically: \(widths)")
        XCTAssertLessThan(widths[16]!, widths[17]!)
        XCTAssertLessThan(widths[17]!, widths[18]!)
        XCTAssertEqual(NButton.glyphSize, 17, "the default is 1a's")
    }

    /// EDGE (E23): a fixed box with a scaling glyph collides at AX5 unless the
    /// glyph is capped. The box must not grow and the glyph must stay inside it.
    func testIconButtonBoxDoesNotGrowAtAX5() {
        let box: CGFloat = 38
        let measured = RenderMeasure.size(
            of: NButton(systemImage: "sparkles", label: "Ask", glyph: 16, corner: .pill, box: box) {},
            dynamicTypeSize: .accessibility5
        )
        XCTAssertMeasures(measured.height, box, accuracy: 0.01, "icon box at AX5")

        // The icon variant clamps its glyph; the text variants do not.
        XCTAssertTrue(NButton.clampsGlyph(for: .icon))
        for variant in [NButtonStyle.Variant.primary, .secondary, .ghost] {
            XCTAssertFalse(NButton.clampsGlyph(for: variant), "\(variant) must keep scaling — it is content")
        }

        // And the clamp is low enough that the glyph still fits every design
        // box. Unclamped, a 18 pt glyph at AX5 is ~44 pt inside a 38 pt circle.
        for (glyph, designBox) in [(CGFloat(16), CGFloat(38)), (17, 40), (18, 44), (17, 36)] {
            let clamped = RenderMeasure.size(
                of: NIcon("arrow.up", size: glyph, relativeTo: .subheadline)
                    .dynamicTypeSize(...Theme.Metrics.chromeTypeCeiling),
                dynamicTypeSize: .accessibility5
            )
            XCTAssertLessThan(
                clamped.height, designBox - 2 * Theme.Stroke.border,
                "a \(glyph) pt glyph clamped at \(Theme.Metrics.chromeTypeCeiling) is \(clamped.height) — it overruns the \(designBox) box"
            )
            let unclamped = RenderMeasure.size(
                of: NIcon("arrow.up", size: glyph, relativeTo: .subheadline),
                dynamicTypeSize: .accessibility5
            )
            XCTAssertGreaterThan(unclamped.height, clamped.height, "the chrome ceiling is doing nothing")
        }
    }

    /// EDGE (E23), the half `testIconButtonBoxDoesNotGrowAtAX5` does not reach.
    /// That test measures a bare `NIcon` with the ceiling applied by hand, and
    /// the box's own `.frame` pins its size whatever the glyph does — so with
    /// `NButton.clampsGlyph` returning `false`, every rendered assertion in it
    /// still passed and only the pure-function check failed.
    ///
    /// What F23 actually claims is that the glyph does not overrun its circle.
    /// A `.frame` does not clip, so the button is rendered inside a canvas three
    /// times its size: an overflowing glyph then paints *outside* the circle and
    /// the accent ink's bounding box says so.
    func testTheIconButtonsGlyphStaysInsideItsCircleAtAX5() throws {
        let accent = RenderMeasure.components(of: Theme.Color.accent)
        let canvas: CGFloat = 132

        // DESIGN.md §2's four circles, each with its screen's glyph size.
        for (glyph, box) in [(CGFloat(16), CGFloat(38)), (17, 40), (18, 44), (17, NButton.iconBox)] {
            let bitmap = try XCTUnwrap(
                RenderMeasure.bitmap(
                    of: NButton(systemImage: "arrow.up", label: "Ask", glyph: glyph, corner: .pill, box: box) {}
                        .frame(width: canvas, height: canvas),
                    dynamicTypeSize: .accessibility5
                ),
                "the \(glyph)/\(box) button rendered nothing"
            )
            let ink = try XCTUnwrap(
                bitmap.bounds(matching: accent),
                "nothing accent-coloured rendered for glyph \(glyph) in a \(box) box"
            )

            let circle = CGRect(
                x: (canvas - box) / 2, y: (canvas - box) / 2, width: box, height: box
            ).insetBy(dx: -pixels, dy: -pixels)
            XCTAssertTrue(
                circle.contains(ink),
                """
                a \(glyph) pt glyph at AX5 paints \(ink), which runs outside its \(box) pt circle \
                \(circle) — the icon button's Dynamic Type ceiling is not holding.
                """
            )
        }
    }

    // MARK: - Ask fields (F2)

    /// DESIGN.md §2's table, one row per screen. Before this they were all
    /// 14 pt / `9 × 15`, because the component could only express one variant.
    func testAskFieldHeightsMatchEachScreen() {
        let cases: [(String, NTextField.Metrics)] = [
            ("DS .input", .input),
            ("2a feed", .askPinned),
            ("1a docked", .askDocked),
            ("2a focus / 2b", .askCentred),
            ("1f thread", .askThread),
            ("1h search", .searchPill),
        ]
        for (name, m) in cases {
            let measured = RenderMeasure.height(
                of: NTextField("Ask, search, or describe an agent", text: .constant(""), metrics: m),
                width: 300
            )
            // The CSS box: the field's own line box (`line-height: 1.55`) plus
            // its vertical padding, floored at the screen's declared
            // `min-height`.
            //
            // ±1.5, for two known and opposite box-model differences that
            // mostly cancel: SwiftUI's `TextField` reserves a line box about a
            // point taller than the CSS one, and SwiftUI's `strokeBorder` draws
            // *inside* the frame where CSS's `border` adds 2 pt to an auto
            // height. The residual is ≤1.1 pt on any variant, and the four ask
            // fields are 2–6 pt apart, so this cannot swallow a wrong preset.
            let expected = max(m.minHeight, m.fontSize * Theme.LineHeight.body + 2 * m.paddingV)
            XCTAssertMeasures(measured, expected, accuracy: cssBox, "\(name) field height")
        }
    }

    /// The four ask variants must not all be the same height — which is exactly
    /// what the old single-variant component produced.
    func testTheFourAskVariantsAreFourDifferentFields() {
        let heights = [NTextField.Metrics.askPinned, .askDocked, .askCentred, .askThread].map { m in
            RenderMeasure.height(of: NTextField("Ask", text: .constant(""), metrics: m), width: 300)
        }
        XCTAssertEqual(Set(heights.map { ($0 * 2).rounded() }).count, 4, "ask field heights collapsed: \(heights)")
    }

    // MARK: - Tag, chip, segmented (F1, F8)

    /// DS `.tag` — `padding: 3px 10px`, 11 pt, inheriting `line-height: 1.55`.
    func testTagHeightIsTheCSSBox() {
        let measured = RenderMeasure.size(of: NTag("Draft"))
        XCTAssertMeasures(
            measured.height,
            NTag.fontSize * Theme.LineHeight.body + 2 * NTag.paddingV,
            accuracy: cssBox, "tag height"
        )
    }

    /// 1e's chip — `padding: 5px 12px`, 12.5 pt.
    func testChipHeightIsTheCSSBox() {
        let measured = RenderMeasure.size(of: NChip("Recruiters", isSelected: false) {})
        XCTAssertMeasures(
            measured.height,
            NChip.fontSize * Theme.LineHeight.body + 2 * NChip.paddingV,
            accuracy: cssBox, "chip height"
        )
    }

    /// DS `7 / 12` @ 13, **1c `6 / 14` @ 13**, 1i `5 / 12` @ 12. The gallery
    /// used to render 1c's Status control at the DS metrics.
    func testSegmentedHonoursEachScreensOverride() {
        let cases: [(String, NSegmentedMetrics)] = [
            ("DS", .ds), ("1c Status", .status), ("1i Read/Write", .noteMode),
        ]
        var heights: [CGFloat] = []
        for (name, m) in cases {
            let measured = RenderMeasure.size(
                of: NSegmented(options: ["Draft", "Published"], selection: .constant("Draft"), metrics: m) { $0 }
            )
            XCTAssertMeasures(
                measured.height,
                m.fontSize * Theme.LineHeight.body + 2 * m.paddingV,
                accuracy: cssBox, "\(name) segmented height"
            )
            heights.append(measured.height)
        }
        XCTAssertNotEqual(heights[0], heights[1], "1c's `6 / 14` override is not being applied")
    }

    /// EDGE (E2)/(E3) — F22. `fixedSize(horizontal: true)` asked for the full
    /// intrinsic width unconditionally, so `lineLimit(1)` and
    /// `minimumScaleFactor` never saw a constraint and long labels ran off a
    /// 320 pt screen.
    func testSegmentedCompressesRatherThanOverflowingASmallScreen() {
        let long = NSegmented(
            options: ["Everything that ever arrived", "Only the unread ones"],
            selection: .constant("Only the unread ones")
        ) { $0 }

        for screen in [CGFloat(320), 375] {
            let measured = RenderMeasure.width(of: long, proposedWidth: screen)
            XCTAssertLessThanOrEqual(
                measured, screen + 0.5,
                "segmented control is \(measured) wide on a \(screen) pt screen"
            )
        }

        // It still takes its intrinsic width when there is room — the DS's
        // `display: inline-flex`, not a full-width control.
        let roomy = RenderMeasure.width(
            of: NSegmented(options: ["Draft", "Published"], selection: .constant("Draft")) { $0 },
            proposedWidth: 402
        )
        XCTAssertLessThan(roomy, 250, "segmented control stretched to fill: \(roomy)")
    }

    /// `VHairline` has no ideal height, so a container proposing a tall box
    /// used to stretch the whole control to fill it.
    func testSegmentedDoesNotStretchToFillATallProposal() {
        let measured = RenderMeasure.size(
            of: NSegmented(options: ["Draft", "Published"], selection: .constant("Draft")) { $0 },
            proposedHeight: 2000
        )
        XCTAssertLessThan(measured.height, 60, "segmented control filled its proposal: \(measured)")
    }

    // MARK: - Toggle, stepper, radios (F5, F6)

    func testToggleRendersAt46By26() {
        let measured = RenderMeasure.size(of: NToggle("Ask before it acts", isOn: .constant(true)))
        XCTAssertMeasures(measured.width, 46, accuracy: 0.01, "toggle width")
        XCTAssertMeasures(measured.height, 26, accuracy: 0.01, "toggle height")
    }

    /// DESIGN.md §3 1d: boxes 30 × 30, count min-width 22, **row gap 12**.
    /// The component shipped with gap 8.
    func testStepperRowIsBoxGapCountGapBox() {
        let measured = RenderMeasure.size(of: NStepper("Repeat every", value: .constant(2)))
        XCTAssertMeasures(measured.height, NStepper.box, accuracy: 0.01, "stepper height")
        XCTAssertMeasures(
            measured.width,
            2 * NStepper.box + 2 * NStepper.gap + NStepper.countMinWidth,
            accuracy: 0.5, "stepper width"
        )
        XCTAssertEqual(NStepper.gap, 12, "1d's row is `gap: 12`")
    }

    /// 1c rows are `11 / 0` with a 15 pt label over a 12 pt hint; 1d rows are
    /// `9 / 0` with a 14 pt label. One shared set of numbers cannot be both.
    func testTheTwoRadioRowsAreTheTwoDesignRows() {
        let invocation = RenderMeasure.size(
            of: NRadioRow("On a schedule", hint: "Daily, weekly, or a custom repeat",
                          isSelected: true, metrics: .invocation) {},
            proposedWidth: 358
        )
        let expectedInvocation = 2 * NRadioRow.Metrics.invocation.paddingV
            + NRadioRow.Metrics.invocation.labelSize * Theme.LineHeight.body
            + 2   // the VStack's 2 pt label→hint gap
            + NRadioRow.Metrics.invocation.hintSize * Theme.LineHeight.body
        XCTAssertMeasures(invocation.height, expectedInvocation, accuracy: cssBox, "1c invocation row height")

        let ends = RenderMeasure.size(
            of: NRadioRow("After", value: "13 runs", isSelected: true, metrics: .ends) {},
            proposedWidth: 358
        )
        let expectedEnds = 2 * NRadioRow.Metrics.ends.paddingV
            + NRadioRow.Metrics.ends.labelSize * Theme.LineHeight.body
        XCTAssertMeasures(ends.height, expectedEnds, accuracy: cssBox, "1d ends row height")

        XCTAssertNotEqual(NRadioRow.Metrics.invocation, NRadioRow.Metrics.ends)
        XCTAssertEqual(NRadioRow.Metrics.invocation.gap, 11)
        XCTAssertEqual(NRadioRow.Metrics.ends.gap, 11)
    }

    // MARK: - Cards (F7)

    /// 1a's draft card is `15 / 15 / 13`, not the DS's uniform 13.8.
    func testDraftAgentCardUsesItsOwnPadding() {
        XCTAssertEqual(NCardPadding.draftAgent.top, 15)
        XCTAssertEqual(NCardPadding.draftAgent.leading, 15)
        XCTAssertEqual(NCardPadding.draftAgent.bottom, 13)
        XCTAssertEqual(NCardPadding.draftAgent.trailing, 15)
        XCTAssertNotEqual(NCardPadding.draftAgent.top, NCardPadding.ds.top)

        // A card with the draft padding is exactly 2 × (15 − 13.8) taller than
        // the same content in a DS card is short — i.e. the padding is real.
        let content = Text("x").font(Theme.Font.body(15))
        let ds = RenderMeasure.height(of: NCard { content }, width: 358)
        let draft = RenderMeasure.height(of: NCard(padding: NCardPadding.draftAgent) { content }, width: 358)
        XCTAssertMeasures(draft - ds, (15 - Theme.Space.s3) + (13 - Theme.Space.s3), accuracy: 0.5, "draft card padding delta")
    }

    // MARK: - Tab bar (F17)

    /// EDGE (E17): the bar's `9 / … / 26` now lives inside each column, so a
    /// tab's own frame is the full height of the bar rather than the ~43 pt
    /// glyph-and-label stack it used to be. `TabBarUITests` checks the shipped
    /// frame; this checks the arithmetic.
    func testTabBarIsTallEnoughForItsPaddingToBeInsideTheButtons() {
        let bar = RenderMeasure.size(of: NTabBar(selection: .constant(.ask)), proposedWidth: 402)
        let chrome = Theme.Stroke.border + Theme.Metrics.TabBar.paddingTop + Theme.Metrics.TabBar.paddingBottom
        XCTAssertGreaterThan(bar.height, chrome, "tab bar shorter than its own chrome: \(bar)")
        // A column is the bar minus its top hairline.
        XCTAssertAtLeast(bar.height - Theme.Stroke.border, Theme.Metrics.minimumHitTarget, "tab column height")
    }

    func testTabBarFitsTheNarrowestSupportedScreen() {
        for screen in [CGFloat(320), 375, 402] {
            let bar = RenderMeasure.size(of: NTabBar(selection: .constant(.calendar)), proposedWidth: screen)
            XCTAssertMeasures(bar.width, screen, accuracy: 0.5, "tab bar width at \(screen)")
        }
    }

    /// F13, and then a second time in the same place.
    ///
    /// The first version measured `icon.height` and `label.height` off the very
    /// same views the bar is built from, then asserted the bar equalled their
    /// sum plus the padding. That is an identity: it proves the padding is
    /// applied and nothing whatever about the two terms that matter. It stayed
    /// green with a 24.2 pt glyph box and a 13.4 pt label box — a bar measured
    /// at **78.67** pt against the mockup's 75.28.
    ///
    /// The mockup's terms are fixed and none of them is read back off the view:
    /// an `<svg width="18" height="18">`, a label whose flex box is the
    /// inherited `line-height: 1.55`, and `padding: 9px 10px 26px` under a
    /// 1 px `border-top`.
    func testTabBarHeightIsTheMockupsOwnArithmetic() {
        typealias M = Theme.Metrics.TabBar
        let bar = RenderMeasure.size(of: NTabBar(selection: .constant(.ask)), proposedWidth: 402)

        let expected = Theme.Stroke.border                // border-top: 1px
            + M.paddingTop                                // 9
            + M.iconSize                                  // the svg's own 18
            + M.iconLabelGap                              // gap: 5
            + M.labelSize * Theme.LineHeight.body         // 10.5 × 1.55 = 16.275
            + M.paddingBottom                             // 26

        XCTAssertMeasures(bar.height, expected, accuracy: grid, "tab bar height against the mockup's boxes")
        // The same number written out, so a `Theme` constant drifting fails
        // here and not only in `ThemeTests`.
        XCTAssertMeasures(bar.height, 75.275, accuracy: grid, "the mockup's bar is 75.275 pt")
    }

    /// Four columns, four SF Symbols, one `VStack` each: if the glyph boxes
    /// differ the labels land on different baselines, and in `p1-shell.png`
    /// they did — label ink tops 39.00 / 39.33 / 41.00 / 41.33 pt below the
    /// rule, a **2.33 pt** spread, because each column was sized by its own
    /// symbol's font line box (glyph ink 15.0 / 16.0 / 18.67 / 20.0).
    ///
    /// Every glyph in the mockup is an 18 × 18 `<svg>`. Equal boxes is the
    /// sufficient condition — the label below carries identical modifiers in
    /// all four columns — so it is the box that is asserted here.
    func testEveryTabGlyphOccupiesTheSameDesignBox() {
        typealias M = Theme.Metrics.TabBar
        var boxes: [String: CGSize] = [:]
        for tab in NTab.allCases {
            let glyph = RenderMeasure.size(
                of: NIcon(tab.symbol, size: M.iconSize, weight: .light, relativeTo: .caption2)
            )
            boxes[tab.rawValue] = glyph
            XCTAssertMeasures(glyph.height, M.iconSize, accuracy: RenderMeasure.snap, "\(tab.rawValue) glyph box height")
            XCTAssertMeasures(glyph.width, M.iconSize, accuracy: RenderMeasure.snap, "\(tab.rawValue) glyph box width")
        }
        XCTAssertEqual(
            Set(boxes.values.map(\.height)).count, 1,
            "the four tab glyph boxes are not the same height: \(boxes)"
        )
    }

    // MARK: - Padding that only a width can see (F13)

    /// F13. `NTag.paddingH` and `NChip.paddingH` were compared with themselves
    /// and with nothing else — both stayed green at a rendered 30 pt. The inner
    /// width is *rendered* through the face the component is supposed to use,
    /// so a component that quietly swapped Lora for Cormorant fails here too.
    func testTagAndChipHorizontalPaddingIsTheDesignsPadding() {
        let tagLabel = "Draft"
        let tagInner = RenderMeasure.width(
            of: Text(tagLabel)
                .font(Theme.Font.body(NTag.fontSize))
                .nadeTracking(NTag.tracking, at: NTag.fontSize)
                .lineLimit(1)
        )
        XCTAssertMeasures(
            RenderMeasure.size(of: NTag(tagLabel)).width,
            tagInner + 2 * NTag.paddingH,
            accuracy: grid, "tag width = label + 2 × \(NTag.paddingH)"
        )

        let chipLabel = "Recruiters"
        let chipInner = RenderMeasure.width(
            of: Text(chipLabel).font(Theme.Font.body(NChip.fontSize)).lineLimit(1)
        )
        XCTAssertMeasures(
            RenderMeasure.size(of: NChip(chipLabel, isSelected: false) {}).width,
            chipInner + 2 * NChip.paddingH,
            accuracy: grid, "chip width = label + 2 × \(NChip.paddingH)"
        )
    }

    /// F13. DS `.btn { gap: 6px }`. The constant was asserted; the `HStack`
    /// carrying it was not, and a rendered gap of 26 passed.
    func testTheButtonsLabelGapIsWhatSeparatesGlyphFromLabel() {
        let bare = RenderMeasure.size(of: NButton("Approve") {}).width
        let withGlyph = RenderMeasure.size(of: NButton("Approve", systemImage: "plus") {}).width
        // A glyph beside a label sits at the label's 14 pt, not the icon-only 17.
        let glyph = RenderMeasure.width(
            of: NIcon("plus", size: NButton.fontSize, weight: .light, relativeTo: .subheadline)
        )
        XCTAssertMeasures(
            withGlyph - bare, glyph + NButton.labelGap,
            accuracy: grid, "glyph + \(NButton.labelGap) pt gap"
        )
    }

    // MARK: - Geometry only the pixels can see (F13)

    /// One rendered device pixel at `RenderMeasure.bitmapScale`. Antialiasing on
    /// a curved edge blends the outermost ring, so a colour-matched bounding box
    /// is up to a pixel short on each side; two is the honest floor, and it is
    /// fixed by the renderer's scale rather than by the device's.
    private var pixels: CGFloat { 2 / RenderMeasure.bitmapScale }

    /// F13. `NToggle.knobDiameter` (20) and `knobInset` (3) are the last two
    /// numbers in the design that `sizeThatFits` cannot see — the knob lives
    /// *inside* a 46 × 26 track, so the component measures the same whatever it
    /// draws. Both stayed green at a rendered 8 pt knob with a 9 pt inset.
    /// This finds the knob in the render and measures it.
    func testTheToggleKnobIsTwentyPointsThreeIn() throws {
        let knobOff = RenderMeasure.components(of: Theme.Color.neutral400)
        let knobOn = RenderMeasure.components(of: Theme.Color.accent)

        for (isOn, knobColor) in [(false, knobOff), (true, knobOn)] {
            let bitmap = try XCTUnwrap(
                RenderMeasure.bitmap(of: NToggle("Ask before it acts", isOn: .constant(isOn))),
                "the toggle rendered nothing"
            )
            XCTAssertMeasures(bitmap.pointSize.width, NToggle.trackWidth, accuracy: pixels, "toggle bitmap width")
            XCTAssertMeasures(bitmap.pointSize.height, NToggle.trackHeight, accuracy: pixels, "toggle bitmap height")

            // On, the knob and the track's border are the *same* accent, so the
            // knob is picked out as the longest run across the track's middle —
            // 20 pt against the ring's 1.
            let across = try XCTUnwrap(
                bitmap.longestRun(matching: knobColor, onRowAt: NToggle.trackHeight / 2),
                "no knob on the centre row with isOn = \(isOn)"
            )
            let width = across.upperBound - across.lowerBound
            XCTAssertMeasures(width, NToggle.knobDiameter, accuracy: pixels, "knob diameter (isOn = \(isOn))")

            let down = try XCTUnwrap(
                bitmap.longestRun(matching: knobColor, onColumnAt: (across.lowerBound + across.upperBound) / 2),
                "no knob down the centre column with isOn = \(isOn)"
            )
            XCTAssertMeasures(
                down.upperBound - down.lowerBound, NToggle.knobDiameter,
                accuracy: pixels, "knob is a circle, not an oval (isOn = \(isOn))"
            )
            XCTAssertMeasures(
                down.lowerBound, (NToggle.trackHeight - NToggle.knobDiameter) / 2,
                accuracy: pixels, "knob is vertically centred (isOn = \(isOn))"
            )

            // The mockup's `left: 2px / 22px` are measured inside the 1 pt
            // border, so the knob sits `knobInset` from the outer edge at both
            // ends and the two gaps are symmetric.
            let leading = across.lowerBound
            let trailing = NToggle.trackWidth - across.upperBound
            XCTAssertMeasures(
                isOn ? trailing : leading, NToggle.knobInset,
                accuracy: pixels, "knob inset on its own side (isOn = \(isOn))"
            )
            XCTAssertMeasures(
                isOn ? leading : trailing,
                NToggle.trackWidth - NToggle.knobDiameter - NToggle.knobInset,
                accuracy: pixels, "knob travel (isOn = \(isOn))"
            )
        }
    }

    /// F13. A **shape** is not a size: `.pill` and `.rounded` measure identically,
    /// so `NTextField.Metrics.shape` and `NButton`'s `corner` were asserted only
    /// against themselves.
    ///
    /// The top row of the render tells them apart. A `RoundedRectangle` of
    /// radius `r` has a flat top edge inset by `r` at each end, so the ink on
    /// row 0 spans `r … width − r`; a pill's radius is half its height. On a
    /// 38–44 pt control that is a 15–18 pt difference per side.
    func testPillAndRoundedControlsActuallyDrawDifferentCorners() throws {
        var insets: [NTextField.Shape: [CGFloat]] = [:]
        for metrics in [NTextField.Metrics.input, .askPinned, .askDocked, .askCentred, .searchPill] {
            // Each preset is outlined in its own resting colour — `.askCentred`
            // is accent before it is ever focused — so the run to look for is
            // the one that preset actually paints.
            let border = Self.onPageComponents(of: metrics.restingBorderColor)
            let bitmap = try XCTUnwrap(
                RenderMeasure.bitmap(
                    of: NTextField("Ask, search, or describe an agent", text: .constant(""), metrics: metrics)
                        .frame(width: 300)
                ),
                "the \(metrics.shape) field rendered nothing"
            )
            let top = try XCTUnwrap(
                bitmap.longestRun(matching: border, onRowAt: 0, tolerance: 0.05),
                "no \(metrics.border) border on the top row of a \(metrics.shape) field"
            )

            let radius = metrics.shape == .pill ? bitmap.pointSize.height / 2 : Theme.Radius.md
            // On the outermost row the arc is still within one device pixel of
            // the flat edge for `d = sqrt(2r/scale)` either side of the tangent
            // point, so the measured run starts up to that much early. Derived
            // from the geometry, not chosen to fit.
            let arc = (2 * radius / RenderMeasure.bitmapScale).squareRoot()
            XCTAssertLessThanOrEqual(
                top.lowerBound, radius + pixels,
                "\(metrics.shape) field's top edge starts \(top.lowerBound) in, past its \(radius) pt corner"
            )
            XCTAssertGreaterThanOrEqual(
                top.lowerBound, radius - arc - pixels,
                "\(metrics.shape) field's top edge starts \(top.lowerBound) in — too flat for a \(radius) pt corner"
            )
            insets[metrics.shape, default: []].append(top.lowerBound)
        }

        // …and the two shapes are not quietly the same thing.
        let rounded = try XCTUnwrap(insets[.rounded]?.max())
        let pill = try XCTUnwrap(insets[.pill]?.min())
        XCTAssertGreaterThan(
            pill - rounded, 8,
            "a pill's corner (\(pill)) is barely deeper than radius-md's (\(rounded)) — the shapes have collapsed"
        )
    }

    /// The mockup's centred ask field (2a focus / 2b, line 57) carries
    /// `border-color: var(--color-accent)` **on the element**, and DESIGN.md
    /// §2's table and §3-2a·4 both repeat it. Nothing asserted it, `Metrics`
    /// had no field for it, and the gallery covered the hole by passing
    /// `showsFocusRing: true` — so the one preset that is gold at rest drew a
    /// hairline, and every test agreed with the bug.
    ///
    /// This reads the outline off the render, unfocused, for all six presets.
    func testEachPresetDrawsItsOwnRestingBorder() throws {
        for metrics in [
            NTextField.Metrics.input, .askPinned, .askDocked, .askCentred, .askThread, .searchPill,
        ] {
            let bitmap = try XCTUnwrap(
                RenderMeasure.bitmap(
                    of: NTextField("Ask, search, or describe an agent", text: .constant(""), metrics: metrics)
                        .frame(width: 300)
                ),
                "the \(metrics.border) field rendered nothing"
            )

            // Mid-height on the left edge is outline and nothing else: the
            // corner arc is at its widest there and the placeholder has not
            // started. Half a point in is the middle pixel of the 1 pt stroke,
            // so it is fully covered and carries no antialiasing.
            let scale = RenderMeasure.bitmapScale
            let edge = bitmap.rgb(
                x: Int((Theme.Stroke.border / 2 * scale).rounded(.down)),
                y: Int((metrics.minHeight / 2 * scale).rounded(.down))
            )
            let expected = Self.onPageComponents(of: metrics.restingBorderColor)
            let wrong = Self.onPageComponents(
                of: metrics.border == .accent ? Theme.Color.divider : Theme.Color.accent
            )

            let toExpected = Self.distance(edge, expected)
            let toWrong = Self.distance(edge, wrong)
            XCTAssertLessThan(
                toExpected, toWrong,
                """
                a \(metrics.border)-bordered field painted \(edge) — that is nearer the \
                other colour (\(toWrong)) than its own (\(toExpected))
                """
            )
        }

        // And the two are far enough apart that the comparison above means
        // something: accent against divider-over-bg is most of the ramp.
        XCTAssertGreaterThan(
            Self.distance(
                Self.onPageComponents(of: Theme.Color.accent),
                Self.onPageComponents(of: Theme.Color.divider)
            ),
            0.3,
            "accent and divider have collapsed into each other — this test proves nothing"
        )
    }

    private static func distance(
        _ a: (r: Double, g: Double, b: Double),
        _ b: (r: Double, g: Double, b: Double)
    ) -> Double {
        ((a.r - b.r) * (a.r - b.r) + (a.g - b.g) * (a.g - b.g) + (a.b - b.b) * (a.b - b.b)).squareRoot()
    }

    /// F13. `NStepper.countSize` (15) is invisible to `sizeThatFits`: the
    /// count's `min-width: 22` swallows any digit narrower than the box, so the
    /// row measures identically at 9 pt. The digits are the only `ink` in the
    /// row — the boxes are `divider`, the glyphs `accent` — so they can be found
    /// in the render and compared with the same text rendered on its own.
    func testTheStepperCountIsRenderedAtItsDesignSize() throws {
        let ink = RenderMeasure.components(of: Theme.Color.ink)

        let row = try XCTUnwrap(RenderMeasure.bitmap(of: NStepper("Repeat every", value: .constant(8))))
        let digits = try XCTUnwrap(row.bounds(matching: ink), "no ink-coloured count in the stepper")

        let reference = try XCTUnwrap(RenderMeasure.bitmap(
            of: Text(8, format: .number)
                .font(Theme.Font.body(NStepper.countSize))
                .foregroundStyle(Theme.Color.ink)
                .tabularNumerals()
        ))
        let expected = try XCTUnwrap(reference.bounds(matching: ink), "the reference digit drew no ink")

        XCTAssertMeasures(digits.height, expected.height, accuracy: pixels, "count digit height")
        XCTAssertMeasures(digits.width, expected.width, accuracy: pixels, "count digit width")
    }

    /// F13. DS `.btn:disabled { opacity: 0.45 }` is not a size, so no
    /// `sizeThatFits` can reach it — it was asserted only against itself.
    ///
    /// It is solved here from one fully covered pixel of the accent border,
    /// halfway along the button's straight top edge: `out = bg + α · (accent −
    /// bg)`. One pixel rather than an average over the render, because text is
    /// smoothed with a contrast-dependent gamma and a whole-button ink total is
    /// therefore *not* linear in the opacity.
    func testTheDisabledButtonIsRenderedAtTheDesignsOpacity() throws {
        let bg = RenderMeasure.components(of: Theme.Color.bg)
        let accent = RenderMeasure.components(of: Theme.Color.accent)

        let enabled = try XCTUnwrap(RenderMeasure.bitmap(of: NButton("Approve") {}))
        let disabled = try XCTUnwrap(RenderMeasure.bitmap(of: NButton("Approve") {}.disabled(true)))
        XCTAssertEqual(enabled.width, disabled.width, "disabling changed the button's size")
        XCTAssertEqual(enabled.height, disabled.height, "disabling changed the button's size")

        // The middle of the 1 pt border's three device rows, on the flat part of
        // the top edge — fully covered, so no antialiasing is in the sample.
        let probeX = enabled.pointSize.width / 2
        let probeY = Theme.Stroke.border / 2

        let full = try XCTUnwrap(
            enabled.alpha(atX: probeX, y: probeY, foreground: accent, over: bg, linearised: false),
            "the enabled button drew no border to sample"
        )
        XCTAssertEqual(full, 1, accuracy: 0.02, "the enabled button's border is not solid accent")

        let faded = try XCTUnwrap(
            disabled.alpha(atX: probeX, y: probeY, foreground: accent, over: bg, linearised: false)
        )
        XCTAssertEqual(
            faded, NButton.disabledOpacity, accuracy: 0.01,
            "the disabled button's border composites at α = \(faded), not \(NButton.disabledOpacity)"
        )
    }

}

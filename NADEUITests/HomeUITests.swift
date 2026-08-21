//
//  HomeUITests.swift
//  NADEUITests
//
//  The P3 lane, tapped through.
//
//  This file exists because a review found two defects that ~40 new
//  `accessibilityIdentifier`s could not catch, for want of anything driving
//  them: 1a's model was rebuilt empty on any parent re-render, and 1b's "New"
//  button was a no-op that then popped the screen. Both are one tap deep. A
//  screen with identifiers and no tap-through is a screen nobody has used.
//

import XCTest

final class HomeUITests: XCTestCase {

    override func setUp() { continueAfterFailure = false }

    private func launch(screen: String? = nil, query: String? = nil) -> XCUIApplication {
        XCUIApplication.nade(screen: screen, query: query)
    }

    // MARK: - 2a

    func testFeedShowsItsCardsAndTheirLocalOnlyButtons() {
        let app = launch(screen: "2a")
        XCTAssertTrue(app.staticTexts["home.date"].waitForExistence(timeout: 10))

        // `API.md` §7: the primary button is `data.action_label`. PLAN C1/C2:
        // it names a local effect and never an outbound one.
        XCTAssertTrue(app.buttons["Save draft"].firstMatch.waitForExistence(timeout: 5))
        XCTAssertTrue(app.buttons["Save note"].firstMatch.exists)
        XCTAssertFalse(app.buttons["Send"].firstMatch.exists)
        XCTAssertFalse(app.buttons["Approve"].firstMatch.exists,
                       "the literal word Approve is what action_label replaces")
    }

    /// The grabber crosses to focus and back, and the state is the model's.
    func testGrabberCrossesBetweenFeedAndFocus() {
        let app = launch(screen: "2a")
        let grabber = app.buttons["home.grabber"]
        XCTAssertTrue(grabber.waitForHittable(timeout: 10))
        grabber.tap()

        XCTAssertTrue(app.staticTexts["home.greeting"].waitForExistence(timeout: 3))
        XCTAssertTrue(app.buttons["home.agents"].exists, "focus is the only way into 1b")

        app.buttons["home.peek"].tap()
        XCTAssertTrue(app.staticTexts["home.date"].waitForExistence(timeout: 3))
    }

    // MARK: - 1a

    /// **The regression this file was written for.** 1a's model used to be built
    /// inside a `navigationDestination` closure, which re-runs whenever the
    /// parent re-renders — and `RootTabView`'s body reads `navigation.selection`,
    /// so every tab tap rebuilt it. The view kept its identity, so `.task` never
    /// re-fired and the answer was replaced by a blank body.
    func testAnAnswerSurvivesLeavingAndReturningToTheTab() {
        let app = launch(screen: "1a", query: "What did Priya say about the design review?")

        let prose = app.staticTexts["ask.prose"]
        XCTAssertTrue(prose.waitForExistence(timeout: 10))
        let answer = prose.label
        XCTAssertFalse(answer.isEmpty, "the answer stream produced nothing")

        app.buttons["tab.mail"].tap()
        XCTAssertTrue(app.staticTexts["mailboxes.title"].waitForExistence(timeout: 5))
        app.buttons["tab.ask"].tap()

        XCTAssertTrue(prose.waitForExistence(timeout: 5),
                      "1a was rebuilt: the answer is gone and cannot be recovered")
        XCTAssertEqual(prose.label, answer)
    }

    /// The `results` route, which 2a's second focus prompt is there to reach.
    func testTheResultsRouteRendersHits() {
        let app = launch(screen: "1a", query: "from:priya@acme.com")
        XCTAssertTrue(app.staticTexts["ask.query"].waitForExistence(timeout: 10))
        XCTAssertTrue(app.buttons["ask.makeagent"].waitForExistence(timeout: 5),
                      "results is the only state that offers Make this an agent")
    }

    /// The draft card's underlines are tappable, which is what makes its own
    /// lead line ("Tap anything underlined to change it") true.
    func testTheDraftCardSaysWhatItSavesAndCanBeEdited() {
        let app = launch(screen: "1a", query: "When a recruiter emails, note the next steps")
        XCTAssertTrue(app.descendants(matching: .any)["ask.draft"].waitForExistence(timeout: 10))
        XCTAssertTrue(app.buttons["ask.savedraft"].exists)
        XCTAssertFalse(app.buttons["Send"].firstMatch.exists)

        app.buttons["ask.savedraft"].tap()
        // The promise the server enforces: a created agent is always a draft.
        XCTAssertTrue(app.staticTexts["ask.saved"].waitForExistence(timeout: 5))
        XCTAssertTrue(app.staticTexts["ask.saved"].label.contains("draft"))
    }

    // MARK: - 1b

    func testAgentsListFiltersAndOpensTheBuilder() {
        let app = launch(screen: "1b")
        XCTAssertTrue(app.staticTexts["Agents"].waitForExistence(timeout: 10))

        app.buttons["agents.filter.drafts"].tap()
        XCTAssertTrue(app.buttons["agents.row.a0000003-0000-4000-8000-000000000003"]
                        .waitForExistence(timeout: 3))
        XCTAssertFalse(app.buttons["agents.row.a0000001-0000-4000-8000-000000000001"].exists,
                       "a published agent is not a draft")

        app.buttons["agents.filter.all"].tap()
        let published = app.buttons["agents.row.a0000001-0000-4000-8000-000000000001"]
        XCTAssertTrue(published.waitForExistence(timeout: 3))
        published.tap()

        // 1c is a modal with no tab bar (DESIGN §2's navigation map).
        XCTAssertTrue(app.staticTexts["builder.name"].waitForExistence(timeout: 5))
        XCTAssertFalse(app.buttons["tab.mail"].isHittable, "1c is modal: the tab bar is covered")
    }

    /// **The second regression.** "New" called `ask("")`, which is correctly
    /// refused for an empty query, and then cleared the path anyway — so its
    /// whole effect was to throw you off 1b having created nothing.
    func testNewGoesToAFieldYouCanTypeInto() {
        let app = launch(screen: "1b")
        XCTAssertTrue(app.buttons["agents.new"].waitForExistence(timeout: 10))
        app.buttons["agents.new"].tap()

        XCTAssertTrue(app.staticTexts["home.greeting"].waitForExistence(timeout: 5),
                      "New should land on 2a's focus state, where the ask field is")
        XCTAssertTrue(app.textFields["ask.field.focus"].waitForHittable(timeout: 5),
                      "and the field has to be reachable, or New still does nothing")
    }

    // MARK: - 1c

    func testBuilderRendersTheCompiledSentenceAndItsSections() {
        let app = launch(screen: "1c")
        XCTAssertTrue(app.staticTexts["builder.name"].waitForExistence(timeout: 10))
        XCTAssertFalse(app.staticTexts["builder.name"].label.isEmpty,
                       "an unloaded model renders an empty name")

        XCTAssertTrue(app.descendants(matching: .any)["builder.sentence"].exists,
                      "a compiled agent shows the underlined sentence, not the fallback")

        app.buttons["builder.section.tools"].tap()
        XCTAssertTrue(app.buttons["builder.tool.search_mail"].waitForExistence(timeout: 3))

        app.buttons["builder.section.settings"].tap()
        XCTAssertTrue(app.descendants(matching: .any)["builder.status"].waitForExistence(timeout: 3))
        // The note under the toggle must never promise an outbound action.
        let note = app.staticTexts["builder.approvalnote"]
        XCTAssertTrue(note.exists)
        XCTAssertFalse(note.label.lowercased().contains("send"))
        XCTAssertFalse(note.label.lowercased().contains("leaves your account"))
    }

    /// The compile-failure fallback: no underlines, and the error is shown.
    func testACompileFailureRendersItsErrorInsteadOfUnderlines() {
        let app = XCUIApplication.nade(screen: "1c", agent: "a0000004-0000-4000-8000-000000000004")
        XCTAssertTrue(app.staticTexts["builder.compileerror"].waitForExistence(timeout: 10))
        XCTAssertTrue(app.buttons["builder.sentence.plain"].exists,
                      "the whole sentence is one tap target when there are no spans")
    }
}

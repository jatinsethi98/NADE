//
//  AccessibilityUITests.swift
//  NADEUITests
//
//  What VoiceOver would actually be handed, read back out of the running app.
//  The unit tests pin the *composition* (`NRadioRow.spokenLabel`,
//  `NButton.resolvedAccessibilityLabel`); these prove the components then put
//  it on the element.
//

import XCTest

final class AccessibilityUITests: XCTestCase {

    override func setUp() {
        continueAfterFailure = false
    }

    private func launchGallery(section: String) -> XCUIApplication {
        XCUIApplication.nade(seed: nil, now: nil, gallerySection: section)
    }

    /// F19. 1d's three "Ends" rows differ *only* in their trailing value. With
    /// the value left off the element, "On" and "After" are two identical
    /// options and a VoiceOver user cannot tell which date or which count they
    /// are choosing.
    func testTrailingRadioValuesAreSpoken() {
        let app = launchGallery(section: "controls")

        let expected = [
            ("ends.never", "Never", nil as String?),
            ("ends.onDate", "On", "6 Jan 2027"),
            ("ends.after", "After", "13 runs"),
        ]
        for (id, label, value) in expected {
            let row = app.descendants(matching: .any)[id].firstMatch
            XCTAssertTrue(row.waitForExistence(timeout: 10), "\(id) is not in the Controls section")
            XCTAssertEqual(row.label, label, "\(id) is labelled \"\(row.label)\"")
            if let value {
                XCTAssertEqual(
                    row.value as? String, value,
                    "\(id) announces \(String(describing: row.value)) — the trailing value is silent"
                )
            }
        }

        // The three rows must be distinguishable from one another.
        let spoken = expected.map { id, _, _ -> String in
            let row = app.descendants(matching: .any)[id].firstMatch
            return "\(row.label)|\(row.value as? String ?? "")"
        }
        XCTAssertEqual(Set(spoken).count, 3, "two Ends rows sound identical: \(spoken)")
    }

    /// F19, the other half: 1c's rows carry a hint instead of a value, and the
    /// hint must not be lost either.
    func testInvocationRadioHintsAreSpoken() {
        let app = launchGallery(section: "controls")
        let row = app.buttons["On a schedule, Daily, weekly, or a custom repeat"].firstMatch
        XCTAssertTrue(
            row.waitForExistence(timeout: 10),
            "the 1c invocation row does not announce its hint"
        )
    }

    /// F18. `NButton("")` and `NChip("")` used to copy the empty string
    /// straight into `.accessibilityLabel`, leaving an interactive element
    /// VoiceOver could not name. The gallery renders both on purpose.
    func testEmptyLabelledControlsStillHaveANameToSpeak() {
        let app = launchGallery(section: "edges")
        XCTAssertTrue(app.buttons["Button"].firstMatch.waitForExistence(timeout: 10),
                      "the empty-title button has no spoken name")
        XCTAssertTrue(app.buttons["Filter"].firstMatch.exists,
                      "the empty-title chip has no spoken name")
    }

    /// F24. v1 takes no outbound action (DESIGN.md §4: the primary button
    /// "never" reads "Send"). The gallery exposed "Send" to VoiceOver twice and
    /// printed "send button" as visible copy.
    /// **Asked as one query, not as two arrays.**
    ///
    /// This used to pull `allElementsBoundByIndex` for buttons *and* static
    /// texts and read `.label` off every one — two cross-process round trips per
    /// element, over a gallery with hundreds of them. It took ~190 s and
    /// intermittently killed the app with "Lost connection", which made every
    /// full-suite run a coin flip. A predicate is evaluated on the other side of
    /// that boundary, so the whole tree is walked once and only the offenders
    /// come back. It is also strictly broader: `.any` covers element types the
    /// two hand-listed queries did not.
    func testNothingInTheGallerySaysSend() {
        // `MATCHES` is anchored, so the wildcards are part of the pattern. `\\b`
        // keeps "Sender" and "resend" from matching, exactly as the old
        // `.regularExpression` search did.
        let saysSend = OutboundCopy.predicate

        // **Prove the guard can fail.** Swapping a hand-rolled regex search for
        // an ICU `MATCHES` is exactly the kind of change that quietly turns an
        // assertion into a tautology, and a green test that cannot go red is
        // worse than no test. These run against the predicate itself, so they
        // cost nothing and fail loudly if the pattern stops meaning what it says.
        // `sent`, `forwarded`, `archived` and `deleted` are here because they
        // were **not** caught: every guard in the app matched
        // `\bsend(s|ing)?\b`, so "This expired before it could be sent." —
        // shipped copy, under a card whose button says "Save draft" — passed
        // all four. A past tense is the most natural way to claim an outbound
        // action, and it was the one form nothing looked for.
        for offender in ["Send", "send draft", "SENDING", "Sends it", "Tap Send now",
                         "it was sent", "Forwarded it", "Archived the thread"] {
            XCTAssertTrue(saysSend.evaluate(with: ["label": offender]),
                          "\(offender.debugDescription) should have been caught")
        }
        // `Delete` is a real v1 control — `DELETE /agents/{id}` — and DESIGN §4
        // forbids sending, archiving and Gmail mutation, none of which it is.
        for innocent in ["Sender", "Resend", "Save draft", "Save note", "sendest",
                         "Delete", "Delete this agent"] {
            XCTAssertFalse(saysSend.evaluate(with: ["label": innocent]),
                           "\(innocent.debugDescription) is not an outbound promise")
        }

        for section in ["buttons", "inputs"] {
            let app = launchGallery(section: section)
            XCTAssertTrue(app.staticTexts.firstMatch.waitForExistence(timeout: 10))

            let offenders = app.descendants(matching: .any).matching(saysSend)
            XCTAssertEqual(
                offenders.count, 0,
                "the \(section) section says \(offenders.allElementsBoundByIndex.map(\.label)) — v1 takes no outbound action"
            )
        }
    }
}

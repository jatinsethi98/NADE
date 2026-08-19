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
    func testNothingInTheGallerySaysSend() {
        for section in ["buttons", "inputs"] {
            let app = launchGallery(section: section)
            XCTAssertTrue(app.staticTexts.firstMatch.waitForExistence(timeout: 10))

            // Both the spoken names and the visible copy.
            let labels = app.buttons.allElementsBoundByIndex.map(\.label)
                + app.staticTexts.allElementsBoundByIndex.map(\.label)
            let offenders = labels.filter {
                $0.range(of: #"\bsend(s|ing)?\b"#, options: [.regularExpression, .caseInsensitive]) != nil
            }
            XCTAssertTrue(
                offenders.isEmpty,
                "the \(section) section says \(offenders) — v1 takes no outbound action"
            )
        }
    }
}

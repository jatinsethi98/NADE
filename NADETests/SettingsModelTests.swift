//
//  SettingsModelTests.swift
//  NADETests
//
//  The two rules `docs/API.md` §1 puts on a pairing code, and the one
//  `DESIGN.md` §4 puts on the server field.
//

import XCTest
@testable import NADE

@MainActor
final class SettingsModelTests: XCTestCase {

    private func model() throws -> SettingsModel {
        let store = try MailStore.inMemory()
        let source = FixtureMailSource(isEmpty: false)
        return SettingsModel(sync: MailSync(source: source, store: store), openURL: { _ in })
    }

    /// `API.md` §1: "Six ASCII digits; anything else is `400 bad_request`."
    ///
    /// `Character.isNumber` is true of Arabic-Indic digits, Devanagari digits,
    /// circled numerals and a long tail of others — none of which the server
    /// accepts. Enabling Pair for them sends a request that cannot succeed and
    /// spends nothing but the user's patience.
    func testOnlySixASCIIDigitsEnablePairing() throws {
        let model = try model()
        model.serverURLText = "http://localhost:8080"

        for good in ["203120", "000000", "999999"] {
            model.codeText = good
            XCTAssertTrue(model.canPair, "\(good) should pair")
        }

        for bad in ["20312", "2031200", "20312a", "", "٢٠٣١٢٠", "２０３１２０", "20 120"] {
            model.codeText = bad
            XCTAssertFalse(model.canPair, "\(bad) should not pair")
        }
    }

    /// DESIGN.md §4: no control that does nothing. A field the user can type a
    /// server into must be the server the app then talks to — and if it cannot
    /// be, it must not accept typing.
    func testAnUnparseableServerAddressBlocksPairingRatherThanFallingBack() throws {
        let model = try model()
        model.codeText = "203120"

        model.serverURLText = "http://localhost:8080"
        XCTAssertNotNil(model.typedOrigin)
        XCTAssertTrue(model.canPair)

        for bad in ["", "   ", "not a url", "localhost:8080", "/v1/me"] {
            model.serverURLText = bad
            XCTAssertNil(model.typedOrigin, "\(bad) parsed as a server address")
            XCTAssertFalse(model.canPair, "\(bad) allowed pairing")
        }
    }

    func testPairingReportsAWrongCodeWithoutGuessingWhyItWasWrong() async throws {
        let model = try model()
        model.serverURLText = "http://localhost:8080"
        model.codeText = "111111"

        // The fixture source mirrors the server: six digits is accepted, and
        // anything else is the same indistinguishable failure. `API.md` §1 is
        // explicit that wrong, expired and already-spent are one response
        // "deliberately indistinguishable so the response cannot be used as an
        // oracle", so the UI must not claim to know which.
        model.codeText = "abcdef"
        let ok = await model.pair()
        XCTAssertFalse(ok)
        let problem = model.pairingProblem ?? ""
        for oracle in ["expired", "already", "used", "wrong"] {
            XCTAssertFalse(problem.lowercased().contains(oracle),
                           "the failure message claims to know it was \(oracle): \(problem)")
        }
    }
}

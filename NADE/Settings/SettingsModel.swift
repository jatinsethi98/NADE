//
//  SettingsModel.swift
//  NADE
//

import SwiftUI

@MainActor
@Observable
final class SettingsModel {

    var isPairing = false
    var serverURLText: String
    var codeText = ""
    private(set) var pairingProblem: String?
    private(set) var account: WireMe?
    private(set) var gmailLinkURL: URL?

    /// Resolved once and after each pairing change, not read from `body`.
    /// `isPaired` reached the Keychain — an XPC round trip to `securityd` —
    /// through a computed property that `SettingsView.body` evaluated on every
    /// re-render.
    private(set) var isPaired = false

    private let sync: MailSync
    private let openURL: @MainActor (URL) -> Void
    private let observation = StoreObservation()

    init(sync: MailSync, openURL: @escaping @MainActor (URL) -> Void) {
        self.sync = sync
        self.openURL = openURL
        self.serverURLText = sync.source.origin.absoluteString
    }

    var needsGmail: Bool { sync.state == .needsGmail }
    var problem: String? { observation.problem?.message ?? sync.connectionProblem?.message }
    var accountEmail: String { account?.email ?? "" }
    var serverDisplay: String { sync.source.origin.absoluteString }

    /// DESIGN.md §1k: "Connected ›" or "Needs sign-in ›", the arrow being part
    /// of the string rather than a separate chevron.
    var accountValue: String {
        switch account?.status {
        case .ok: String(localized: "Connected ›", comment: "Settings account row value when Gmail is linked")
        default: String(localized: "Needs sign-in ›", comment: "Settings account row value when Gmail is not linked")
        }
    }

    /// Six **ASCII** digits. `API.md` §1: the code is a string, not a number,
    /// because it can have a leading zero — and `Character.isNumber` is true of
    /// Arabic-Indic and every other Unicode numeral, none of which the server
    /// accepts. `"0"..."9"` is the contract.
    var canPair: Bool {
        codeText.count == 6
            && codeText.allSatisfy { $0.isASCII && $0.isNumber }
            && typedOrigin != nil
    }

    /// The URL in the field, if it is one. `nil` disables Pair rather than
    /// silently falling back to a server the user is not looking at.
    var typedOrigin: URL? {
        let trimmed = serverURLText.trimmingCharacters(in: .whitespaces)
        guard let url = URL(string: trimmed), url.scheme != nil, url.host != nil else { return nil }
        return url
    }

    /// A developer override wins over the field, so the field says so rather
    /// than pretending to be in charge (DESIGN.md §4: no control that does
    /// nothing).
    var serverURLIsEditable: Bool { !ServerSetting.isOverriddenByEnvironment }

    func start() {
        refreshPairingState()
        guard !observation.isRunning else { return }
        observation.keep(sync.store.observeAccount(
            onError: { [weak self] _ in self?.observation.failed(StateCopy.storeUnreadable) },
            onChange: { [weak self] in
                self?.observation.succeeded()
                self?.account = $0
            }
        ))
    }

    private func refreshPairingState() {
        isPaired = (try? sync.source.isPaired()) == true
    }

    func pair() async -> Bool {
        pairingProblem = nil
        guard let origin = typedOrigin else {
            pairingProblem = String(localized: "That does not look like a server address.",
                                    comment: "Shown when the pairing screen's server URL cannot be parsed")
            return false
        }
        do {
            try await sync.pair(origin: origin, code: codeText, deviceName: Self.deviceName)
            codeText = ""
            refreshPairingState()
            return true
        } catch let error as CredentialError {
            // EDGE (P17). The code is spent and the token could not be kept, so
            // retrying with the same code cannot work. Say that, rather than
            // sending the user round a loop.
            if case .tokenLostAfterPairing = error {
                pairingProblem = String(
                    localized: "Paired, but the token could not be saved. Ask your server for a new code.",
                    comment: "Shown when the Keychain write fails after a pairing code has been consumed"
                )
            } else {
                pairingProblem = String(localized: "Couldn't save the pairing.",
                                        comment: "Shown when the device token could not be stored")
            }
            return false
        } catch let failure as APIFailure {
            // A wrong, expired or already-spent code is the same 401 by design,
            // so the message must not pretend to know which.
            pairingProblem = failure.userFacingMessage
            return false
        } catch {
            pairingProblem = String(localized: "Couldn't pair with that server.",
                                    comment: "Fallback pairing failure")
            return false
        }
    }

    /// Two calls, per DESIGN.md §1k and `API.md` §1: mint a single-use URL,
    /// then open it in a browser session the app does not discard — `start`
    /// sets an `HttpOnly` cookie that `callback` requires.
    func startGmailLink() async {
        do {
            let url = try await sync.gmailLinkURL()
            gmailLinkURL = url
            openURL(url)
        } catch let failure as APIFailure {
            pairingProblem = failure.userFacingMessage
        } catch {
            pairingProblem = String(localized: "Couldn't start the Gmail sign-in.",
                                    comment: "Shown when POST /auth/gmail/link fails")
        }
    }

    private static var deviceName: String {
        UIDevice.current.name
    }
}

//
//  SettingsView.swift
//  NADE
//
//  Screen **1k**, in part.
//
//  `docs/DESIGN.md` §1k lists four sections. Two of them ship here and two do
//  not, and the line between them is the same one §4 draws everywhere else —
//  an affordance whose backing is missing gets cut, not disabled:
//
//  * **ACCOUNT** ships. `GET /me` and `POST /auth/gmail/link` both exist.
//  * **CONNECTION** ships. It is `+ Added` in the design ("Without it a fresh
//    install cannot reach the backend at all"), and `POST /auth/pair` exists.
//  * **AGENTS** does not. Its toggle writes `PATCH /settings` and its "Run log"
//    row pushes `GET /runs`; neither route is mounted until P7.
//  * **READING** and the footer do not. "Text size" pushes nothing yet, and the
//    footer must render the **served** `disclosure` string (deviation #39) —
//    hardcoding the honest sentence is exactly what that deviation forbids,
//    because it could then drift from what the server actually does.
//

import SwiftUI

struct SettingsView: View {

    enum Metrics {
        static let headerTop: CGFloat = 62
        static let headerBottom: CGFloat = 12
        static let titleSize: CGFloat = 23

        /// "Section eyebrows … at `20 / 22 / 6` — except the **first**, which
        /// is `18 / 22 / 6`."
        static let firstEyebrowTop: CGFloat = 18
        static let eyebrowTop: CGFloat = 20

        static let rowPaddingV: CGFloat = 13
        static let labelSize: CGFloat = 14.5
        static let valueSize: CGFloat = 12
    }

    @Environment(AppNavigation.self) private var navigation
    let model: SettingsModel

    var body: some View {
        VStack(spacing: 0) {
            SettingsHeader { navigation.popMail() }
            Hairline()
            ScrollView {
                VStack(spacing: 0) {
                    account
                    connection
                    Spacer(minLength: Theme.Space.s6)
                }
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
        .sheet(isPresented: Bindable(model).isPairing) {
            PairingView(model: model)
        }
        .task { model.start() }
    }

    private var account: some View {
        VStack(spacing: 0) {
            SectionBand(title: "Account", top: Metrics.firstEyebrowTop)
            Hairline()
            SettingsRow(
                label: model.accountEmail.isEmpty
                    ? String(localized: "No mailbox connected",
                             comment: "Settings account row before Gmail is linked")
                    : model.accountEmail,
                value: model.accountValue
            )
            .accessibilityIdentifier("settings.account")

            // DESIGN.md §1k: the "Sign in again" row appears **only** in the
            // needs-sign-in state, and starting the flow is two calls, not one
            // — `POST /auth/gmail/link` mints a single-use URL and the app
            // opens it in a browser session it does not discard.
            if model.needsGmail {
                Hairline()
                Button {
                    Task { await model.startGmailLink() }
                } label: {
                    Text("Sign in again")
                        .font(Theme.Font.body(Metrics.labelSize))
                        .foregroundStyle(Theme.Color.accent)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .padding(.vertical, Metrics.rowPaddingV)
                        .padding(.horizontal, Theme.Space.screen)
                        .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .accessibilityIdentifier("settings.signin")
            }
            Hairline()

            if let problem = model.problem {
                StateCaption(problem, alignment: .leading)
                    .padding(.top, Theme.Space.s2)
                    .padding(.horizontal, Theme.Space.screen)
                    .accessibilityIdentifier("settings.problem")
            }
        }
    }

    private var connection: some View {
        VStack(spacing: 0) {
            SectionBand(title: "Connection", top: Metrics.eyebrowTop)
            Hairline()
            SettingsRow(label: String(localized: "Server", comment: "Settings row label for the NADE server URL"),
                        value: model.serverDisplay)
                .accessibilityIdentifier("settings.server")
            Hairline()
            Button {
                model.isPairing = true
            } label: {
                SettingsRow(
                    label: String(localized: "Pair this device", comment: "Settings row that opens the pairing screen"),
                    value: model.isPaired
                        ? String(localized: "Paired ›", comment: "Settings pairing row value when a token is stored")
                        : String(localized: "Not paired ›", comment: "Settings pairing row value with no token"),
                    isInteractive: true
                )
            }
            .buttonStyle(.plain)
            .accessibilityIdentifier("settings.pair")
            Hairline()
        }
    }

}

/// DESIGN.md §1k's row: label 14.5 pt, value 12 pt `ink62` "ending in '›'
/// (part of the string, not a separate chevron)".
struct SettingsRow: View {
    private typealias M = SettingsView.Metrics

    let label: String
    let value: String
    var isInteractive: Bool = false

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: Theme.Space.s2) {
            Text(label)
                .font(Theme.Font.body(M.labelSize))
                .foregroundStyle(Theme.Color.ink)
                .lineLimit(1)
                .truncationMode(.middle)
                .nadeLineHeight(Theme.LineHeight.body, size: M.labelSize)
            Spacer(minLength: Theme.Space.s2)
            Text(value)
                .font(Theme.Font.body(M.valueSize))
                .foregroundStyle(Theme.Color.ink62)
                .lineLimit(1)
                .truncationMode(.head)
        }
        .padding(.vertical, M.rowPaddingV)
        .padding(.horizontal, Theme.Space.screen)
        .frame(maxWidth: .infinity, alignment: .leading)
        .contentShape(Rectangle())
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(label)
        .nadeAccessibilityValue(value)
        .accessibilityAddTraits(isInteractive ? .isButton : [])
    }
}


/// 1k's header band. Extracted for the same reason as 1f's: it is specified
/// `62 / 22 / 12`, identically to 1g's, and it was rendering shorter because
/// its title had no explicit line box — visible in the checked-in screenshot as
/// a divider sitting higher than 1g's.
struct SettingsHeader: View {
    let back: () -> Void

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: Theme.Space.s2) {
            BackChevron(
                label: String(localized: "Mailboxes", comment: "Back button on Settings"),
                identifier: "settings.back",
                action: back
            )

            Text("Settings")
                .font(Theme.Font.heading(SettingsView.Metrics.titleSize))
                .foregroundStyle(Theme.Color.ink)
                // The DS's inherited `line-height: 1.55`, like 1g's identical
                // header. Without it the band is short by ~8 pt and 1k's
                // divider sits visibly higher than 1g's for the same `62/22/12`.
                .nadeLineHeight(Theme.LineHeight.body, size: SettingsView.Metrics.titleSize,
                                face: Theme.Font.PostScriptName.headingSemibold)
                .accessibilityAddTraits(.isHeader)
                .accessibilityIdentifier("settings.title")
            Spacer(minLength: 0)
        }
        .padding(.top, SettingsView.Metrics.headerTop)
        .padding(.horizontal, Theme.Space.screen)
        .padding(.bottom, SettingsView.Metrics.headerBottom)
    }
}

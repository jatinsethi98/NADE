//
//  PairingView.swift
//  NADE
//
//  `+ Added`. DESIGN.md §1k gives the row that pushes this screen — "server URL
//  + 'Pair this device ›', which is where the pairing-code entry screen lives"
//  — and nothing beyond it: no pixels, no copy, no error layout. So this is
//  assembled from the design system's own parts at the design's own numbers,
//  and it is the one screen in the lane a reviewer cannot check against a
//  render. `NADE/CRITERIA.md` §D says so rather than implying otherwise.
//
//  The copy follows `API.md` §1 exactly where it constrains us: the code is a
//  **string** of six ASCII digits ("it can have a leading zero"), and a wrong,
//  expired or already-spent code is the same `401` — "deliberately
//  indistinguishable so the response cannot be used as an oracle" — so the UI
//  must not guess at which of the three it was.
//

import SwiftUI

struct PairingView: View {

    enum Metrics {
        static let titleSize: CGFloat = 20
        static let bodySize: CGFloat = 14
        static let topPadding: CGFloat = 24
        static let fieldGap = Theme.Space.s4
    }

    @Environment(\.dismiss) private var dismiss
    let model: SettingsModel

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            Text("Pair this device")
                .font(Theme.Font.heading(Metrics.titleSize))
                .foregroundStyle(Theme.Color.ink)
                .accessibilityAddTraits(.isHeader)
                .accessibilityIdentifier("pairing.title")

            Text("Your NADE server prints a six-digit code when it starts. It works once, for ten minutes.")
                .font(Theme.Font.body(Metrics.bodySize))
                .foregroundStyle(Theme.Color.ink62)
                .fixedSize(horizontal: false, vertical: true)
                .nadeLineHeight(Theme.LineHeight.body, size: Metrics.bodySize)
                .padding(.top, Theme.Space.s2)

            NTextField("http://localhost:8080", text: Bindable(model).serverURLText)
                .padding(.top, Metrics.fieldGap)
                // A `NADE_BASE_URL` in the environment is a developer override
                // and outranks the field. Saying so beats a field that accepts
                // typing and changes nothing (DESIGN.md §4).
                .disabled(!model.serverURLIsEditable)
                .accessibilityIdentifier("pairing.server")

            if !model.serverURLIsEditable {
                StateCaption("Set by NADE_BASE_URL in this build's environment.",
                             alignment: .leading)
                    .padding(.top, Theme.Space.s1)
            }

            NTextField("000000", text: Bindable(model).codeText)
                .padding(.top, Theme.Space.s2)
                .accessibilityIdentifier("pairing.code")

            if let problem = model.pairingProblem {
                StateCaption(problem, alignment: .leading)
                    .padding(.top, Theme.Space.s2)
                    .accessibilityIdentifier("pairing.problem")
            }

            HStack(spacing: Theme.Space.s2) {
                NButton("Pair", variant: .primary) {
                    Task {
                        if await model.pair() { dismiss() }
                    }
                }
                .disabled(!model.canPair)
                .accessibilityIdentifier("pairing.submit")

                NButton("Cancel", variant: .ghost) { dismiss() }
                    .accessibilityIdentifier("pairing.cancel")
            }
            .padding(.top, Metrics.fieldGap)

            Spacer(minLength: 0)
        }
        .padding(.top, Metrics.topPadding)
        .padding(.horizontal, Theme.Space.screen)
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        // No `.background(Theme.Color.bg)`, for the same reason `ScheduleSheet`
        // dropped its `surface` fill: this is presented as a sheet, and on iOS
        // 26 a sheet's own material, radius and edge inset are the chrome. A
        // flat ground painted over them throws all three away.

    }
}

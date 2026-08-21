//
//  MailRow.swift
//  NADE
//
//  `docs/DESIGN.md` §1e's row, to the point.
//
//  The asymmetric inset is the design's, not a rounding: "1e mail rows | 22
//  right, **14 left** (the unread dot hangs in the inset)".
//

import SwiftUI

struct MailRow: View {

    enum Metrics {
        static let paddingTop: CGFloat = 13
        static let paddingBottom: CGFloat = 13
        static let paddingLeading: CGFloat = 14
        static let paddingTrailing: CGFloat = 22
        static let gap: CGFloat = 9

        static let dot: CGFloat = 6
        static let dotTop: CGFloat = 8

        static let fromSize: CGFloat = 15
        static let timeSize: CGFloat = 11
        static let subjectSize: CGFloat = 14
        static let subjectTop: CGFloat = 2
        static let snippetSize: CGFloat = 13
        static let snippetLineHeight: CGFloat = 1.5
        static let snippetTop: CGFloat = 2
        static let noteSize: CGFloat = 11
        static let noteTop: CGFloat = 7
        /// The mockup's `padding-bottom: 1` under `agent_note`, which sets the
        /// gap between the text and its rule. An overlay alone adds no height,
        /// so the padding is what makes the rule sit where the design draws it.
        static let noteRuleGap: CGFloat = 1
    }

    let row: WireThreadRow
    let now: Date
    let action: () -> Void

    /// `docs/API.md` §2: `from_name` is `""` rather than null when the sender
    /// had no display name, and "the UI decides whether to fall back to the
    /// address; the server does not invent one". This is that decision — a row
    /// with a blank sender and a visible subject reads as a bug.
    private var sender: String {
        row.senderDisplayName
    }

    /// Formatted once. `body` rendered it and then `spokenLabel` formatted the
    /// same `(ts, now)` again for the accessibility label — two ICU pattern
    /// compilations per row, whether or not VoiceOver was running.
    private var time: String {
        ListTime.string(for: row.ts, now: now)
    }

    var body: some View {
        Button(action: action) {
            HStack(alignment: .top, spacing: Metrics.gap) {
                // Read rows keep a `clear` dot rather than dropping it, so the
                // text column starts at the same x whether or not it is unread.
                Circle()
                    .fill(row.unread ? Theme.Color.accent : Color.clear)
                    .frame(width: Metrics.dot, height: Metrics.dot)
                    .padding(.top, Metrics.dotTop)
                    .accessibilityHidden(true)

                VStack(alignment: .leading, spacing: 0) {
                    HStack(alignment: .firstTextBaseline, spacing: Theme.Space.s2) {
                        Text(sender)
                            .font(Theme.Font.body(Metrics.fromSize,
                                                  weight: row.unread ? .semibold : .regular))
                            .foregroundStyle(Theme.Color.ink)
                            .lineLimit(1)
                            .truncationMode(.tail)
                            .nadeLineHeight(Theme.LineHeight.body, size: Metrics.fromSize,
                                            face: row.unread
                                                ? Theme.Font.PostScriptName.bodySemibold
                                                : Theme.Font.PostScriptName.bodyRegular)
                        Spacer(minLength: Theme.Space.s2)
                        Text(time)
                            .font(Theme.Font.body(Metrics.timeSize))
                            .foregroundStyle(Theme.Color.ink62)
                            .tabularNumerals()
                            .layoutPriority(1)
                    }

                    Text(row.subject)
                        .font(Theme.Font.body(Metrics.subjectSize))
                        .foregroundStyle(Theme.Color.ink)
                        .lineLimit(1)
                        .truncationMode(.tail)   // EDGE (E3)
                        .nadeLineHeight(Theme.LineHeight.body, size: Metrics.subjectSize)
                        .padding(.top, Metrics.subjectTop)

                    Text(row.snippet)
                        .font(Theme.Font.body(Metrics.snippetSize))
                        .foregroundStyle(Theme.Color.ink55)
                        .lineLimit(1)
                        .truncationMode(.tail)
                        .nadeLineHeight(Metrics.snippetLineHeight, size: Metrics.snippetSize)
                        .padding(.top, Metrics.snippetTop)

                    if let note = row.agentNote {
                        // The accent rule under the note is `accent-300` while
                        // the text is `accent` — two colours, which is exactly
                        // what `Text.underline` cannot express (the run's
                        // foreground always wins). IOS_DECISIONS records that as
                        // an open gap; for a single line it is closable, and
                        // this closes it: a real `Hairline` in its own colour,
                        // laid under the text by the padding the mockup draws.
                        Text(note)
                            .font(Theme.Font.body(Metrics.noteSize))
                            .foregroundStyle(Theme.Color.accent)
                            .lineLimit(1)
                            .truncationMode(.tail)
                            .nadeLineHeight(Theme.LineHeight.body, size: Metrics.noteSize)
                            .padding(.bottom, Metrics.noteRuleGap)
                            .overlay(alignment: .bottom) { Hairline(color: Theme.Color.accent300) }
                            .padding(.top, Metrics.noteTop)
                    }
                }
                // `min-width: 0` — without this the row's text can push the
                // time column off a narrow screen (EDGE E2).
                .frame(maxWidth: .infinity, alignment: .leading)
            }
            .padding(.top, Metrics.paddingTop)
            .padding(.bottom, Metrics.paddingBottom)
            .padding(.leading, Metrics.paddingLeading)
            .padding(.trailing, Metrics.paddingTrailing)
            .frame(maxWidth: .infinity, alignment: .leading)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        // EDGE (E6): one element with a composed label, not six leaves. The
        // unread dot is a *value*, because a silent circle is invisible to
        // VoiceOver and "unread" is the row's most important fact.
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(spokenLabel)
        .nadeAccessibilityValue(row.unread
            ? String(localized: "Unread", comment: "Spoken value for an unread mail row")
            : nil)
        .accessibilityAddTraits(.isButton)
        .accessibilityIdentifier("mailrow.\(row.id)")
    }

    /// Sender, subject, snippet, the agent's note, then the time — the order
    /// the row is read in, rather than the order the layout happens to nest.
    private var spokenLabel: String {
        var parts = [sender]
        if !row.subject.isEmpty { parts.append(row.subject) }
        if !row.snippet.isEmpty { parts.append(row.snippet) }
        if let note = row.agentNote { parts.append(note) }
        parts.append(time)
        return parts.joined(separator: ", ")
    }
}

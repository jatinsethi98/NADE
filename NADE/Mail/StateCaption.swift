//
//  StateCaption.swift
//  NADE
//
//  The sentence a screen shows when it has no rows, or when something is
//  missing from the ones it has.
//
//  `docs/DESIGN.md` designs no empty state, no loading state and no error
//  state for 1e or 1g. Rather than invent a vocabulary, this borrows the one
//  the design already uses for exactly this register — 1e's smart-rule caption,
//  1f's "Filed in …" footer, 1h's and 1j's captions: **italic 12 pt, muted.**
//
//  There is deliberately **no spinner**. The design has none anywhere, the
//  store is the source of truth, and its first value arrives synchronously
//  (`.immediate`), so there is no moment to fill.
//

import SwiftUI

struct StateCaption: View {
    static let fontSize: CGFloat = 12
    static let topPadding: CGFloat = Theme.Space.s8

    private let text: String
    private let alignment: TextAlignment
    private let color: Color

    init(_ text: String, alignment: TextAlignment = .center, color: Color = Theme.Color.ink55) {
        self.text = text
        self.alignment = alignment
        self.color = color
    }

    var body: some View {
        Text(text)
            .font(Theme.Font.bodyItalic(Self.fontSize))
            .foregroundStyle(color)
            .multilineTextAlignment(alignment)
            // EDGE (E3): a long sentence wraps rather than clipping.
            .fixedSize(horizontal: false, vertical: true)
            .nadeLineHeight(Theme.LineHeight.body, size: Self.fontSize,
                            face: Theme.Font.PostScriptName.bodyItalic)
            .frame(maxWidth: .infinity, alignment: alignment == .center ? .center : .leading)
    }
}

/// The strings, in one place, so the copy audit has somewhere to look.
///
/// Two of them are load-bearing beyond their tone:
///
/// * **"in the last 30 days"** — `docs/API.md` §2 says a mailbox's counts are
///   within the synced window and "the client must not present them as
///   mailbox-wide truth". A bare "No mail" would do exactly that, and would be
///   wrong for anyone whose mail is older than the dev cap.
/// * **"Syncing your mail"** — the server answers `mailboxes: []` for the one
///   to two minutes its first sync takes. Without this the app looks broken at
///   precisely the moment the user has just finished setting it up.
nonisolated enum StateCopy {
    static var emptyMailbox: String {
        String(localized: "No mail here in the last 30 days.",
               comment: "Shown on the mail list when a mailbox has no threads inside the synced window")
    }

    static var noMailboxes: String {
        String(localized: "No mailbox connected yet.",
               comment: "Shown on the mailboxes screen before a Gmail account is linked")
    }

    static var notPaired: String {
        String(localized: "Not paired yet.",
               comment: "Shown on the mailboxes screen when the device has no server credential")
    }

    static var syncPending: String {
        String(localized: "Syncing your mail — this takes a minute.",
               comment: "Shown while the server's first Gmail sync is still running")
    }

    /// The durable write behind an approval failed, so nothing was queued.
    ///
    /// Said out loud because the alternative is silence: the button stays, the
    /// card does not move, and every further tap does the same nothing.
    static var approvalNotQueued: String {
        String(localized: "Couldn't save that on this device — try again.",
               comment: "Shown when an approval could not be written to the local outbox")
    }

    static var storeUnreadable: String {
        String(localized: "Couldn't read your mail from this device.",
               comment: "Shown when the local store cannot be read")
    }

    static var conversationUnreadable: String {
        String(localized: "Couldn't read this conversation from this device.",
               comment: "Shown when a thread cannot be read out of the local store")
    }

    static var threadOpenFailed: String {
        String(localized: "Couldn't open that conversation.",
               comment: "Shown on the thread screen when its detail could not be loaded")
    }

    static var partialThread: String {
        String(localized: "Some of this conversation is missing.",
               comment: "Shown on a thread whose messages could not all be loaded")
    }
}


/// The "‹" that takes a pushed screen back one step.
///
/// 1e's header and 1k's carried byte-identical copies — same glyph, same
/// `Theme.Font.heading(23)`, same accent, same `nadeHitTarget()`, same spoken
/// name. 1f's differs (13 pt, and it names its destination) and keeps its own.
struct BackChevron: View {
    static let size: CGFloat = 23

    let label: String
    let identifier: String
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            Text(verbatim: "‹")
                .font(Theme.Font.heading(Self.size))
                .foregroundStyle(Theme.Color.accent)
                // EDGE (E17): a bare glyph is well under the 44 pt minimum.
                .nadeHitTarget()
        }
        .buttonStyle(.plain)
        .accessibilityLabel(label)
        .accessibilityIdentifier(identifier)
    }
}

/// A section eyebrow with the padding band DESIGN.md puts around it on 1g and
/// 1k — `<top> / 22 / 6`. Both screens had a private copy of the same six lines.
struct SectionBand: View {
    static let bottom: CGFloat = 6

    let title: String
    let top: CGFloat

    var body: some View {
        SectionEyebrow(title)
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.top, top)
            .padding(.horizontal, Theme.Space.screen)
            .padding(.bottom, Self.bottom)
    }
}

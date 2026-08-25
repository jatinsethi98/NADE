//
//  NADELaunch.swift
//  NADEUITests
//
//  The launch vocabulary, in one place.
//
//  `NADEApp.LaunchOptions` defines six arguments; seven test files restated
//  them, three of those inside helpers that could only *append* — so a test
//  needing a different seed rebuilt the whole array a few lines below a helper
//  it could not use, and two of those hand-written arrays disagreed with each
//  other about `-NADENow` and `TZ` for no stated reason.
//
//  Keyed overrides rather than concatenation, so any test can change any option
//  and none can forget `-NADEGallery 0`.
//

import XCTest

enum NADELaunch {
    /// The clock the fixture world is rendered against, matching
    /// `scripts/screenshots.sh`. Pinned because `listTime` is a function of
    /// *now*: without it a row says "Yesterday" today and "17 Aug" next week.
    static let pinnedNow = "2026-08-17T12:00:00Z"

    enum Seed: String { case fixtures, empty, offline }
}

/// The C1/C2 copy screen, in one place per test target.
///
/// v1 takes no outbound action (PLAN C1/C2, `DESIGN.md` §4), so no string this
/// app authors may claim one. That rule was enforced by the same regex typed
/// out in three files in this target and two more in `NADETests`, and it is
/// precisely how the rule failed once already: all five copies matched
/// `\b(sends?|sending)\b` and none matched `sent`, so shipped copy reading
/// "This expired before it could be sent." — under a button labelled
/// "Save draft" — passed every one of them (D78).
///
/// Adding a verb is now one edit here and one in `NADETests/OutboundCopy.swift`,
/// which carries the identical enum. Two homes rather than one because a UI
/// test target is a separate binary that cannot see the unit target's sources;
/// this comment is the pointer between them. The server keeps its own,
/// broader screen (`agents::feed::promises_an_outbound_action`) for the
/// *model's* prose, and `docs/contract/validate.py::OUTBOUND_VERBS` screens
/// server-authored copy — three lists, three different jobs, each stated once.
enum OutboundCopy {

    /// **`delete` is not here, and `Sent` needs an exemption by identifier.**
    /// `DESIGN.md` §4 forbids "no sending, no archiving, no Gmail mutation" —
    /// deleting an *agent* is none of those and is a real v1 capability
    /// (`DELETE /agents/{id}`), and `Sent` is one of the eight Gmail system
    /// labels §2 exposes, a place rather than a promise.
    nonisolated static let pattern =
        #"\b(sends?|sending|sent|forwards?|forwarding|forwarded|reply-all|archiv(e|es|ed|ing))\b"#

    /// For `NSPredicate`-based element queries, which need the whole label to
    /// match. `MATCHES` is anchored; `.*…*` is what makes it a search, and the
    /// `\b`s are what keep "Sender" and "resend" out.
    static let predicate = NSPredicate(format: "label MATCHES[c] %@", ".*\(pattern).*")

    /// Does this string promise something v1 does not do?
    nonisolated static func promises(_ text: String) -> Bool {
        text.range(of: pattern, options: [.regularExpression, .caseInsensitive]) != nil
    }
}

extension XCUIApplication {
    /// A launched app, with everything a UI test needs pinned.
    ///
    /// The time zone matters as much as the clock: "today" is the *device's*
    /// calendar day, so a row's timestamp shifts with the machine's region.
    @discardableResult
    static func nade(
        seed: NADELaunch.Seed? = .fixtures,
        screen: String? = nil,
        mailbox: String? = nil,
        thread: String? = nil,
        now: String? = NADELaunch.pinnedNow,
        gallerySection: String? = nil,
        contentSize: String? = nil,
        query: String? = nil,
        agent: String? = nil
    ) -> XCUIApplication {
        let app = XCUIApplication()
        var options: [String: String] = ["NADEGallery": gallerySection == nil ? "0" : "1"]
        options["NADESeed"] = seed?.rawValue
        options["NADEScreen"] = screen
        options["NADEMailbox"] = mailbox
        options["NADEThread"] = thread
        options["NADENow"] = now
        options["NADEGallerySection"] = gallerySection
        options["UIPreferredContentSizeCategoryName"] = contentSize
        options["NADEQuery"] = query
        options["NADEAgent"] = agent

        app.launchArguments = options
            .sorted { $0.key < $1.key }
            .flatMap { ["-\($0.key)", $0.value] }
        app.launchEnvironment["TZ"] = "UTC"
        app.launch()
        return app
    }
}

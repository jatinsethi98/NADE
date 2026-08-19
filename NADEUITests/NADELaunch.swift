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
        contentSize: String? = nil
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

        app.launchArguments = options
            .sorted { $0.key < $1.key }
            .flatMap { ["-\($0.key)", $0.value] }
        app.launchEnvironment["TZ"] = "UTC"
        app.launch()
        return app
    }
}

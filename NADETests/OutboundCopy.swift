//
//  OutboundCopy.swift
//  NADETests
//
//  The C1/C2 copy screen, for this target.
//
//  v1 takes no outbound action (PLAN C1/C2, `DESIGN.md` §4), so no string this
//  app authors may claim one. That rule was enforced by the same regex typed
//  out in five test files across two targets, and it is precisely how the rule
//  failed once already: all five copies matched `\b(sends?|sending)\b` and none
//  matched `sent`, so shipped copy reading "This expired before it could be
//  sent." — under a button labelled "Save draft" — passed every one of them
//  (D78).
//
//  **This is one of two, and the count is stated rather than hidden.**
//  `NADEUITests/NADELaunch.swift` carries the identical enum for the UI target.
//  A UI test target is a separate binary that cannot `@testable import NADE`,
//  and neither target can see the other's sources, so the honest options were
//  two files that name each other or a third synchronized group in the
//  pbxproj — and two is a number a reader can hold. Adding a verb is both
//  edits; six copies, which is what this replaced, is not.
//
//  The server keeps its own, deliberately broader screen for the *model's*
//  prose (`agents::feed::promises_an_outbound_action`), and
//  `docs/contract/validate.py::OUTBOUND_VERBS` screens server-authored copy.
//  Three lists, three different jobs, each stated once.
//

import Foundation

enum OutboundCopy {

    /// **`delete` is not here.** `DESIGN.md` §4 forbids "no sending, no
    /// archiving, no Gmail mutation" — deleting an *agent* is none of those and
    /// is a real v1 capability (`DELETE /agents/{id}`).
    nonisolated static let pattern =
        #"\b(sends?|sending|sent|forwards?|forwarding|forwarded|reply-all|archiv(e|es|ed|ing))\b"#

    /// Does this string promise something v1 does not do?
    nonisolated static func promises(_ text: String) -> Bool {
        text.range(of: pattern, options: [.regularExpression, .caseInsensitive]) != nil
    }
}

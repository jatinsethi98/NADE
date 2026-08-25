//
//  WireFeedAgentTests.swift
//  NADETests
//
//  P3's wire models against `docs/contract/`.
//
//  Two rules inherited from `WireDecodeTests`, and they are the reason these
//  are worth writing at all:
//
//  1. **Assert values, not "no throw".** A decode that silently produces an
//     empty struct passes a `XCTAssertNoThrow`.
//  2. **Prove the failures too.** A `null` planted in a non-null field, and a
//     missing key, must both fail — otherwise "non-optional" is a comment.
//

import XCTest
@testable import NADE

final class WireFeedAgentTests: XCTestCase {
    // MARK: - Feed

    func testFeedDecodesEveryActionBranch() throws {
        let page = try ContractFixture.decode(WireFeedPage.self, from: "feed")

        XCTAssertEqual(page.items.count, 6)
        XCTAssertNil(page.nextCursor)
        XCTAssertEqual(page.newCount, 3, "the badge counts approvals AND unseen info items")

        // `actions` is exactly the buttons to render, in order.
        let editable = try XCTUnwrap(page.items.first { $0.actions.contains(.edit) })
        XCTAssertEqual(editable.actions, [.approve, .edit, .skip])
        XCTAssertEqual(editable.title, "Reply Drafter")
        guard case .draftReply(let draft) = try XCTUnwrap(editable.data) else {
            return XCTFail("the editable card must be a draft_reply")
        }
        XCTAssertEqual(draft.actionLabel, "Save draft")
        XCTAssertEqual(draft.to, ["kamran@northbound.co"])
        XCTAssertFalse(draft.neverMessaged)

        // A write_note approval has no Edit button: there is no note editor.
        let note = try XCTUnwrap(page.items.first { $0.id.hasSuffix("0001") })
        XCTAssertEqual(note.actions, [.approve, .skip])
        guard case .writeNote(let payload) = try XCTUnwrap(note.data) else {
            return XCTFail("expected write_note")
        }
        XCTAssertEqual(payload.actionLabel, "Save note")
        XCTAssertEqual(payload.noteTitle, "Kettle — next steps")

        // An info item has no buttons and no token.
        let info = try XCTUnwrap(page.items.first { $0.kind == .info })
        XCTAssertEqual(info.actions, [])
        XCTAssertNil(info.approvalToken)
        XCTAssertNil(info.approvalExpiresAt, "expiry is null on every info item")
        guard case .none = try XCTUnwrap(info.data) else {
            return XCTFail("an info item's action is `none`")
        }
    }

    /// The rule the design leans on hardest: a token exists only while an
    /// approval is `new`, and the expiry outlives it so an expired card can say
    /// *when*.
    func testApprovalTokenExistsOnlyWhileNewAndExpiryOutlivesIt() throws {
        let page = try ContractFixture.decode(WireFeedPage.self, from: "feed")

        for item in page.items {
            switch (item.kind, item.status) {
            case (.approval, .new):
                XCTAssertNotNil(item.approvalToken, "a live approval needs its token")
                XCTAssertNil(item.resolvedNote, "a new card has no outcome line")
            case (.approval, _):
                XCTAssertNil(item.approvalToken, "a finished approval must not carry a token")
                XCTAssertNotNil(item.resolvedNote, "a finished card must explain itself")
                XCTAssertNotNil(item.approvalExpiresAt)
            case (.info, _):
                XCTAssertNil(item.approvalToken)
                XCTAssertNil(item.approvalExpiresAt)
            default:
                XCTFail("unexpected kind \(item.kind)")
            }
        }
    }

    /// No `action_label` may name an outbound verb. v1 takes no outbound
    /// action, and the button renders this string verbatim.
    func testNoActionLabelPromisesAnOutboundAction() throws {
        let page = try ContractFixture.decode(WireFeedPage.self, from: "feed")
        for item in page.items {
            guard let label = item.data?.actionLabel else { continue }
            // The same screen every other app-authored string gets. The
            // six-word substring list this used to carry could not see "sent"
            // and matched "Sender" — the two failure modes at once.
            XCTAssertFalse(
                OutboundCopy.promises(label),
                "\(label) promises an action v1 does not take"
            )
        }
    }

    func testSingleFeedItemFixturesDecode() throws {
        for name in ["feed_item", "feed_item_info", "feed_item_editable"] {
            let item = try ContractFixture.decode(WireFeedItem.self, from: name)
            XCTAssertFalse(item.id.isEmpty, name)
            XCTAssertFalse(item.title.isEmpty, name)
        }
    }

    func testEmptyFeedIsAnEmptyArrayNotAFailure() throws {
        let page = try ContractFixture.decode(WireFeedPage.self, from: "feed_empty")
        XCTAssertEqual(page.items, [])
        XCTAssertNil(page.nextCursor)
        XCTAssertEqual(page.newCount, 0)
    }

    func testFeedActionResponsesDecode() throws {
        let approve = try ContractFixture.decode(WireApproveResponse.self, from: "approve")
        XCTAssertEqual(approve.status, "queued")
        XCTAssertFalse(approve.runID.isEmpty)

        let skip = try ContractFixture.decode(WireSkipResponse.self, from: "skip")
        XCTAssertEqual(skip.status, "skipped")

        let seen = try ContractFixture.decode(WireSeenResponse.self, from: "seen")
        XCTAssertEqual(seen.newCount, 2, "marking the info items seen leaves the two approvals")
    }

    /// An action the contract has not defined yet must render a card, not blank
    /// the whole screen. A decode failure fails the *response*, not the row.
    func testAnUnknownDataActionDegradesInsteadOfThrowing() throws {
        var object = try ContractFixture.object("feed_item")
        var data = try XCTUnwrap(object["data"] as? [String: Any])
        data["action"] = "send_carrier_pigeon"
        object["data"] = data

        let payload = try JSONSerialization.data(withJSONObject: object)
        let item = try WireTime.decoder().decode(WireFeedItem.self, from: payload)

        guard case .unrecognised(let action) = try XCTUnwrap(item.data) else {
            return XCTFail("an unknown action must decode to .unrecognised")
        }
        XCTAssertEqual(action, "send_carrier_pigeon")
        XCTAssertNil(item.data?.actionLabel, "an unknown action offers no button label")
    }

    /// ...and the same for the enums.
    func testUnknownEnumValuesSurvive() throws {
        var object = try ContractFixture.object("feed_item")
        object["status"] = "quarantined"
        object["actions"] = ["approve", "teleport"]

        let payload = try JSONSerialization.data(withJSONObject: object)
        let item = try WireTime.decoder().decode(WireFeedItem.self, from: payload)

        XCTAssertEqual(item.status, .unknown("quarantined"))
        XCTAssertEqual(item.actions, [.approve, .unknown("teleport")])
    }

    /// The failure half. `id` has no `|null` in API.md, so a null must throw.
    func testANullInANonNullFeedFieldFails() throws {
        var object = try ContractFixture.object("feed_item")
        object["title"] = NSNull()
        let payload = try JSONSerialization.data(withJSONObject: object)

        XCTAssertThrowsError(try WireTime.decoder().decode(WireFeedItem.self, from: payload))
    }

    func testAMissingFeedKeyFails() throws {
        var object = try ContractFixture.object("feed_item")
        object.removeValue(forKey: "created_at")
        let payload = try JSONSerialization.data(withJSONObject: object)

        XCTAssertThrowsError(try WireTime.decoder().decode(WireFeedItem.self, from: payload))
    }

    // MARK: - Agents

    func testAgentListDecodesOldestFirst() throws {
        let list = try ContractFixture.decode(WireAgentList.self, from: "agents")

        XCTAssertEqual(list.agents.count, 4)
        XCTAssertEqual(list.agents.first?.name, "Job Search Tracker")
        XCTAssertEqual(list.agents.first?.status, .published)
        XCTAssertEqual(list.agents.first?.triggerSummary, "On new mail")

        let scheduled = try XCTUnwrap(list.agents.first { $0.schedule != nil })
        let schedule = try XCTUnwrap(scheduled.schedule)
        XCTAssertEqual(schedule.freq, "week")
        XCTAssertEqual(schedule.interval, 1)
        XCTAssertEqual(schedule.byweekday, ["mon", "tue", "wed", "thu", "fri"])
        XCTAssertNil(schedule.bymonthday, "v1 has no bymonthday control")
        XCTAssertEqual(schedule.at, "08:00")
        XCTAssertEqual(schedule.ends.kind, "never")

        // A never-run agent, so the list can render "never run".
        XCTAssertTrue(list.agents.contains { $0.lastRunAt == nil })
    }

    func testEmptyAgentListDecodes() throws {
        let list = try ContractFixture.decode(WireAgentList.self, from: "agents_empty")
        XCTAssertEqual(list.agents, [])
    }

    /// The spans are what make 1c's sentence composable. They must be present
    /// when `spec` is, and both spans must really be substrings of the
    /// definition, or the composed sentence would not match what the server
    /// compiled.
    func testTheFullAgentCarriesSpansThatComposeItsSentence() throws {
        let agent = try ContractFixture.decode(WireAgent.self, from: "agent")

        XCTAssertNotNil(agent.spec)
        XCTAssertNil(agent.compileError)
        let when = try XCTUnwrap(agent.whenSpan)
        let doing = try XCTUnwrap(agent.doSpan)
        XCTAssertTrue(agent.nlDefinition.contains(when), "when_span is not in the definition")
        XCTAssertTrue(agent.nlDefinition.contains(doing), "do_span is not in the definition")
        XCTAssertEqual(agent.trailing, "Ask me before you save.")

        XCTAssertEqual(agent.allowedTools, [.searchMail, .readThread, .writeNote])
        let spec = try XCTUnwrap(agent.spec)
        XCTAssertEqual(spec.trigger.kind, .mail)
        XCTAssertEqual(spec.trigger.filters.labelIDs, ["INBOX"])
        XCTAssertNil(spec.trigger.filters.hasAttachment)
        XCTAssertEqual(spec.output.kind, "note")

        // spec.tools ⊆ allowed_tools, which the host enforces at dispatch.
        for tool in spec.tools {
            XCTAssertTrue(agent.allowedTools.contains(tool), "\(tool.rawValue) is not allowed")
        }
    }

    /// The compile-failure state: all three spans null, `spec` null, and the
    /// error present. This is the only state in which 1c renders plain text.
    func testAFailedCompileKeepsTheSentenceAndDropsTheSpans() throws {
        let agent = try ContractFixture.decode(WireAgent.self, from: "agent_compile_failed")

        XCTAssertNil(agent.spec)
        XCTAssertNotNil(agent.compileError)
        XCTAssertNil(agent.whenSpan)
        XCTAssertNil(agent.doSpan)
        XCTAssertNil(agent.trailing)
        XCTAssertEqual(agent.status, .draft, "a failed compile is still a draft")
        XCTAssertFalse(agent.nlDefinition.isEmpty, "the user's sentence is never lost")
        XCTAssertEqual(agent.allowedTools, [])
    }

    func testTheDraftAgentFixtureDecodes() throws {
        let agent = try ContractFixture.decode(WireAgent.self, from: "agent_draft")
        XCTAssertEqual(agent.status, .draft)
        XCTAssertTrue(agent.allowedTools.contains(.draftReply))
        XCTAssertEqual(agent.spec?.output.kind, "draft")
    }

    func testTheScheduledAgentFixtureDecodes() throws {
        let agent = try ContractFixture.decode(WireAgent.self, from: "agent_scheduled")
        XCTAssertEqual(agent.spec?.trigger.kind, .schedule)
        XCTAssertNotNil(agent.schedule)
    }

    // MARK: - Round trips

    /// Re-encode and compare, so a nullable field cannot be silently dropped.
    /// This is what forces the explicit `encode(to:)` on every type above.
    func testEveryP3FixtureRoundTripsWithoutLosingAField() throws {
        try assertRoundTrip(WireFeedPage.self, "feed")
        try assertRoundTrip(WireFeedPage.self, "feed_empty")
        try assertRoundTrip(WireFeedItem.self, "feed_item")
        try assertRoundTrip(WireFeedItem.self, "feed_item_info")
        try assertRoundTrip(WireFeedItem.self, "feed_item_editable")
        try assertRoundTrip(WireAgentList.self, "agents")
        try assertRoundTrip(WireAgentList.self, "agents_empty")
        try assertRoundTrip(WireAgent.self, "agent")
        try assertRoundTrip(WireAgent.self, "agent_draft")
        try assertRoundTrip(WireAgent.self, "agent_scheduled")
        try assertRoundTrip(WireAgent.self, "agent_compile_failed")
    }

    private func assertRoundTrip<T: Codable>(
        _ type: T.Type,
        _ name: String,
        file: StaticString = #filePath,
        line: UInt = #line
    ) throws {
        let original = try ContractFixture.object(name)
        let decoded = try ContractFixture.decode(type, from: name)
        let encoded = try WireTime.encoder().encode(decoded)
        let reencoded = try XCTUnwrap(
            JSONSerialization.jsonObject(with: encoded) as? [String: Any]
        )

        XCTAssertEqual(
            NSDictionary(dictionary: reencoded),
            NSDictionary(dictionary: original),
            "\(name) lost or invented a field on the round trip",
            file: file,
            line: line
        )
    }
}

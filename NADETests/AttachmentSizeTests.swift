//
//  AttachmentSizeTests.swift
//  NADETests
//
//  One number, but the mockup prints it and a `.file` count style would print
//  a different one.
//

import XCTest
@testable import NADE

final class AttachmentSizeTests: XCTestCase {

    /// The mockup's own figure for `thread.json`'s role outline:
    /// `<span>240 KB</span>` beside `Kettle — Staff Designer.pdf`, which is
    /// 245 760 bytes. Binary gives 240; decimal gives 246.
    func testTheFixtureAttachmentPrintsWhatTheMockupPrints() throws {
        let thread = try ContractFixture.decode(WireThread.self, from: "thread")
        let pdf = try XCTUnwrap(thread.messages.first?.attachments.first)
        XCTAssertEqual(pdf.size, 245_760)
        XCTAssertEqual(AttachmentSize.string(bytes: pdf.size), "240 KB")
    }

    func testSmallAndLargeAttachmentsStillReadAsSizes() {
        XCTAssertEqual(AttachmentSize.string(bytes: 8_194), "8 KB")
        XCTAssertEqual(AttachmentSize.string(bytes: 10 * 1024 * 1024), "10 MB")
        // Zero bytes is legal and must not render as an empty string beside a
        // tag that then looks broken.
        XCTAssertFalse(AttachmentSize.string(bytes: 0).isEmpty)
    }
}

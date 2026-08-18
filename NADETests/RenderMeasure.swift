//
//  RenderMeasure.swift
//  NADETests
//
//  Measures what SwiftUI actually lays out, rather than what a constant says.
//
//  The previous geometry suite asserted `NToggle.trackWidth == 46` and called
//  that "the design's geometry". It is not: it stays green if `NToggle` stops
//  using the constant, if a `.padding` is added around it, or if a line-height
//  modifier changes the height of every control in the app. Everything here
//  hosts the real view and reads the size UIKit gives it back.
//

import SwiftUI
import UIKit
import XCTest

@MainActor
enum RenderMeasure {

    /// The size SwiftUI resolves for `view`, at a pinned Dynamic Type size so a
    /// simulator with a non-default text size cannot change the numbers.
    /// Stands in for "as much room as it wants". `.greatestFiniteMagnitude`
    /// makes some SwiftUI layouts return non-finite sizes.
    /// `nonisolated` because it is used as a default argument, which Swift
    /// evaluates outside the main actor.
    nonisolated static let unbounded: CGFloat = 10_000

    /// One device pixel of the **running** simulator, in points: ⅓ on an @3x
    /// phone, ½ on the @2x SE. SwiftUI snaps text metrics to this grid, so it
    /// is the floor on how tightly any measured expectation can be stated —
    /// and it is why a tolerance written for iPhone 17 Pro fails on an SE.
    /// Tolerances below are multiples of it rather than a fixed fudge, so they
    /// tighten automatically on the denser screen.
    nonisolated static var snap: CGFloat {
        1 / max(UITraitCollection.current.displayScale, 1)
    }

    static func size<V: View>(
        of view: V,
        proposedWidth: CGFloat = RenderMeasure.unbounded,
        proposedHeight: CGFloat = RenderMeasure.unbounded,
        dynamicTypeSize: DynamicTypeSize = .large
    ) -> CGSize {
        let host = UIHostingController(
            rootView: view
                .dynamicTypeSize(dynamicTypeSize)
                .environment(\.displayScale, 3)
        )
        // Without this the hosting controller folds the *window's* safe-area
        // insets into every measurement — a 1 pt hairline comes back as 55.
        host.safeAreaRegions = []
        host.view.backgroundColor = .clear
        // A window makes the hosting view resolve its environment the way it
        // would on screen; without one some measurements come back zero.
        let window = UIWindow(frame: CGRect(x: 0, y: 0, width: 402, height: 874))
        window.rootViewController = host
        window.isHidden = false
        host.view.setNeedsLayout()
        host.view.layoutIfNeeded()
        return host.sizeThatFits(in: CGSize(width: proposedWidth, height: proposedHeight))
    }

    static func height<V: View>(
        of view: V,
        width: CGFloat = RenderMeasure.unbounded,
        dynamicTypeSize: DynamicTypeSize = .large
    ) -> CGFloat {
        size(of: view, proposedWidth: width, dynamicTypeSize: dynamicTypeSize).height
    }

    static func width<V: View>(
        of view: V,
        proposedWidth: CGFloat = RenderMeasure.unbounded,
        dynamicTypeSize: DynamicTypeSize = .large
    ) -> CGFloat {
        size(of: view, proposedWidth: proposedWidth, dynamicTypeSize: dynamicTypeSize).width
    }

    /// The width Core Text gives `string` in a concrete `UIFont`. Used to prove
    /// a SwiftUI `Font` resolves to the face we think it does: Cormorant, Lora
    /// and SF are different enough that a 15-character sample separates them by
    /// tens of points.
    static func typesetWidth(_ string: String, font: UIFont) -> CGFloat {
        (string as NSString)
            .size(withAttributes: [.font: font])
            .width
    }
}

/// `XCTAssertEqual(_:_:accuracy:)` for CGFloat, with the value in the message
/// so a failure reads as a measurement rather than as a boolean.
func XCTAssertMeasures(
    _ measured: CGFloat,
    _ expected: CGFloat,
    accuracy: CGFloat = 0.5,
    _ what: String,
    file: StaticString = #filePath,
    line: UInt = #line
) {
    XCTAssertEqual(
        measured, expected, accuracy: accuracy,
        "\(what): measured \(measured), design says \(expected)",
        file: file, line: line
    )
}

func XCTAssertAtLeast(
    _ measured: CGFloat,
    _ minimum: CGFloat,
    _ what: String,
    file: StaticString = #filePath,
    line: UInt = #line
) {
    XCTAssertGreaterThanOrEqual(
        measured, minimum,
        "\(what): measured \(measured), needs at least \(minimum)",
        file: file, line: line
    )
}

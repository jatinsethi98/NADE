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
@testable import NADE

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

    // MARK: - Pixels

    /// Device pixels per point in every `Bitmap` below. Fixed here rather than
    /// taken from the simulator, so a bitmap expectation means the same thing on
    /// an @2x SE and an @3x 17 Pro.
    nonisolated static let bitmapScale: CGFloat = 3

    /// What a view actually paints, at `bitmapScale` pixels per point.
    ///
    /// `sizeThatFits` answers "how big is this component". It cannot answer
    /// "where inside its own box does it draw", which is the only way to check
    /// `NToggle.knobDiameter`, `NToggle.knobInset` or an opacity — all of which
    /// were previously asserted only against themselves.
    struct Bitmap {
        let width: Int
        let height: Int
        /// RGBA8, row-major, premultiplied-off (`.noneSkipLast` would drop α).
        private let pixels: [UInt8]

        init(width: Int, height: Int, pixels: [UInt8]) {
            self.width = width
            self.height = height
            self.pixels = pixels
        }

        var pointSize: CGSize {
            CGSize(width: CGFloat(width) / bitmapScale, height: CGFloat(height) / bitmapScale)
        }

        func rgb(x: Int, y: Int) -> (r: Double, g: Double, b: Double) {
            let i = (y * width + x) * 4
            return (Double(pixels[i]) / 255, Double(pixels[i + 1]) / 255, Double(pixels[i + 2]) / 255)
        }

        /// The tight bounding box, **in points**, of every pixel within
        /// `tolerance` of `color`. Antialiasing means the outermost ring of a
        /// curved edge is a blend, so a box measured this way is up to one
        /// device pixel short on each side — which is why callers state their
        /// accuracy as a multiple of `1 / bitmapScale` rather than as a fudge.
        ///
        /// This finds *everything* of a colour. To find a fill inside a border
        /// of the **same** colour, use `longestRun(matching:onRowAt:)` — no
        /// rectangular inset can do it, because a capsule's ring curves inward
        /// at the top and bottom.
        func bounds(
            matching color: (r: Double, g: Double, b: Double),
            tolerance: Double = 0.06
        ) -> CGRect? {
            var minX = width, minY = height, maxX = -1, maxY = -1
            for y in 0..<height {
                for x in 0..<width {
                    let p = rgb(x: x, y: y)
                    let distance = max(abs(p.r - color.r), max(abs(p.g - color.g), abs(p.b - color.b)))
                    guard distance <= tolerance else { continue }
                    minX = min(minX, x); maxX = max(maxX, x)
                    minY = min(minY, y); maxY = max(maxY, y)
                }
            }
            guard maxX >= 0 else { return nil }
            return CGRect(
                x: CGFloat(minX) / bitmapScale,
                y: CGFloat(minY) / bitmapScale,
                width: CGFloat(maxX - minX + 1) / bitmapScale,
                height: CGFloat(maxY - minY + 1) / bitmapScale
            )
        }

        /// The longest unbroken run of pixels matching `color` along the row at
        /// `y` points, as a range in points.
        ///
        /// This is how a fill is separated from a border of the *same* colour:
        /// an "on" toggle's centre row is `[border 1 pt] … [knob 20 pt] …
        /// [border 1 pt]`, and only the knob is 20 pt long. A rectangular inset
        /// cannot do it — a capsule's ring curves inward at the top and bottom,
        /// so any inset deep enough to miss the ring also clips the knob.
        func longestRun(
            matching color: (r: Double, g: Double, b: Double),
            onRowAt y: CGFloat,
            tolerance: Double = 0.06
        ) -> ClosedRange<CGFloat>? {
            let row = min(max(Int((y * bitmapScale).rounded(.down)), 0), height - 1)
            return longestRun(count: width, tolerance: tolerance, color: color) { rgb(x: $0, y: row) }
        }

        /// The vertical twin, down the column at `x` points.
        func longestRun(
            matching color: (r: Double, g: Double, b: Double),
            onColumnAt x: CGFloat,
            tolerance: Double = 0.06
        ) -> ClosedRange<CGFloat>? {
            let column = min(max(Int((x * bitmapScale).rounded(.down)), 0), width - 1)
            return longestRun(count: height, tolerance: tolerance, color: color) { rgb(x: column, y: $0) }
        }

        private func longestRun(
            count: Int,
            tolerance: Double,
            color: (r: Double, g: Double, b: Double),
            sample: (Int) -> (r: Double, g: Double, b: Double)
        ) -> ClosedRange<CGFloat>? {
            var best: ClosedRange<Int>?
            var start: Int?
            func close(at end: Int) {
                guard let s = start else { return }
                let run = s...(end - 1)
                if best == nil || run.count > best!.count { best = run }
                start = nil
            }
            for i in 0..<count {
                let p = sample(i)
                let distance = max(abs(p.r - color.r), max(abs(p.g - color.g), abs(p.b - color.b)))
                if distance <= tolerance {
                    if start == nil { start = i }
                } else {
                    close(at: i)
                }
            }
            close(at: count)
            guard let run = best else { return nil }
            return CGFloat(run.lowerBound) / bitmapScale ... CGFloat(run.upperBound + 1) / bitmapScale
        }

        /// The alpha that composited `foreground` over `background` to give the
        /// pixel at (`x`, `y`) points — solved per channel and averaged.
        ///
        /// This is how an **opacity** is measured rather than recited. It has to
        /// be a single fully covered pixel rather than an aggregate over the
        /// render: text is drawn with a contrast-dependent smoothing gamma, so a
        /// whole-view ink total is not linear in the opacity even though each
        /// individual pixel is (measured: a sum over the button reads 0.486 in
        /// sRGB and 0.588 in linear light for a true 0.45).
        ///
        /// `space` decides whether the blend is solved on the stored sRGB values
        /// or in linear light; callers assert with the one that is exact.
        func alpha(
            atX x: CGFloat,
            y: CGFloat,
            foreground: (r: Double, g: Double, b: Double),
            over background: (r: Double, g: Double, b: Double),
            linearised: Bool
        ) -> Double? {
            let px = min(max(Int((x * bitmapScale).rounded(.down)), 0), width - 1)
            let py = min(max(Int((y * bitmapScale).rounded(.down)), 0), height - 1)
            let out = rgb(x: px, y: py)
            let f = linearised ? (Self.linear(out.r), Self.linear(out.g), Self.linear(out.b)) : (out.r, out.g, out.b)
            let fg = linearised
                ? (Self.linear(foreground.r), Self.linear(foreground.g), Self.linear(foreground.b))
                : (foreground.r, foreground.g, foreground.b)
            let bg = linearised
                ? (Self.linear(background.r), Self.linear(background.g), Self.linear(background.b))
                : (background.r, background.g, background.b)

            var solved: [Double] = []
            for (o, (fgC, bgC)) in zip([f.0, f.1, f.2], zip([fg.0, fg.1, fg.2], [bg.0, bg.1, bg.2])) {
                // A channel where foreground and background agree carries no
                // information about the alpha at all.
                guard abs(fgC - bgC) > 0.05 else { continue }
                solved.append((o - bgC) / (fgC - bgC))
            }
            guard !solved.isEmpty else { return nil }
            return solved.reduce(0, +) / Double(solved.count)
        }

        /// The sRGB electro-optical transfer function (IEC 61966-2-1).
        static func linear(_ encoded: Double) -> Double {
            encoded <= 0.04045 ? encoded / 12.92 : pow((encoded + 0.055) / 1.055, 2.4)
        }
    }

    /// sRGB components of a SwiftUI `Color`, for comparing against a `Bitmap`.
    static func components(of color: SwiftUI.Color) -> (r: Double, g: Double, b: Double) {
        var r: CGFloat = 0, g: CGFloat = 0, b: CGFloat = 0, a: CGFloat = 0
        UIColor(color).getRed(&r, green: &g, blue: &b, alpha: &a)
        return (Double(r), Double(g), Double(b))
    }

    /// Renders `view` over `background` (the app's own ground by default) and
    /// hands back its pixels. `dynamicTypeSize` is pinned for the same reason
    /// `size(of:)` pins it.
    ///
    /// The default is `nil` rather than `Theme.Color.bg` because Swift evaluates
    /// a default argument outside the main actor, and `Theme.Color` is
    /// main-actor isolated.
    static func bitmap(
        of view: some View,
        over background: SwiftUI.Color? = nil,
        dynamicTypeSize: DynamicTypeSize = .large
    ) -> Bitmap? {
        let renderer = ImageRenderer(
            content: view
                .dynamicTypeSize(dynamicTypeSize)
                .background(background ?? Theme.Color.bg)
        )
        renderer.scale = bitmapScale
        guard let cgImage = renderer.cgImage else { return nil }

        let width = cgImage.width
        let height = cgImage.height
        var pixels = [UInt8](repeating: 0, count: width * height * 4)
        guard let context = CGContext(
            data: &pixels,
            width: width,
            height: height,
            bitsPerComponent: 8,
            bytesPerRow: width * 4,
            space: CGColorSpace(name: CGColorSpace.sRGB)!,
            bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue
        ) else { return nil }
        context.draw(cgImage, in: CGRect(x: 0, y: 0, width: width, height: height))
        return Bitmap(width: width, height: height, pixels: pixels)
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

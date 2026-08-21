//
//  FlowLayout.swift
//  NADE
//
//  A wrapping row. `DESIGN.md` §1a's draft-card tag row is "top 14, gap 6,
//  **wrapping**", and SwiftUI has no built-in that wraps: `HStack` compresses
//  its children instead, so a longer tool name or a localisation pushes a tag
//  off the card rather than onto a second line.
//

import SwiftUI

struct FlowLayout: Layout {

    /// One gap, used both across a row and between rows — which is what the
    /// design's single `gap: 6` means.
    var spacing: CGFloat

    private var rowGap: CGFloat { spacing }

    func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) -> CGSize {
        let width = proposal.width ?? .infinity
        let rows = layout(subviews: subviews, in: width)
        let height = rows.reduce(0) { $0 + $1.height } +
            rowGap * CGFloat(max(rows.count - 1, 0))
        // Never wider than what was offered: returning the natural width would
        // make the card grow instead of the row wrapping.
        let widest = rows.map(\.width).max() ?? 0
        return CGSize(width: min(widest, width), height: height)
    }

    func placeSubviews(in bounds: CGRect, proposal: ProposedViewSize,
                       subviews: Subviews, cache: inout ()) {
        let rows = layout(subviews: subviews, in: bounds.width)
        var y = bounds.minY
        for row in rows {
            var x = bounds.minX
            for index in row.indices {
                let size = subviews[index].sizeThatFits(.unspecified)
                subviews[index].place(at: CGPoint(x: x, y: y),
                                      anchor: .topLeading,
                                      proposal: ProposedViewSize(size))
                x += size.width + spacing
            }
            y += row.height + rowGap
        }
    }

    private struct Row {
        var indices: [Int] = []
        var width: CGFloat = 0
        var height: CGFloat = 0
    }

    /// EDGE: a single subview wider than the container still gets its own row
    /// rather than looping forever — the `row.indices.isEmpty` check is what
    /// guarantees progress.
    private func layout(subviews: Subviews, in width: CGFloat) -> [Row] {
        var rows: [Row] = []
        var row = Row()
        for index in subviews.indices {
            let size = subviews[index].sizeThatFits(.unspecified)
            let needed = row.indices.isEmpty ? size.width : row.width + spacing + size.width
            if needed > width, !row.indices.isEmpty {
                rows.append(row)
                row = Row()
                row.indices = [index]
                row.width = size.width
                row.height = size.height
            } else {
                row.indices.append(index)
                row.width = needed
                row.height = max(row.height, size.height)
            }
        }
        if !row.indices.isEmpty { rows.append(row) }
        return rows
    }
}

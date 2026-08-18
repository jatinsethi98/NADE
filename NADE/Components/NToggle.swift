//
//  NToggle.swift
//  NADE
//
//  DESIGN.md §1 Components — Toggle; mockup 1c "Ask before it acts".
//  46 × 26 pill, 1 pt border, 20 pt knob, x = 2 (off) / 22 (on), 0.18 s ease.
//  On = accent-100 track, accent knob, accent border.
//  Off = transparent track, neutral-400 knob, divider border.
//
//  The mockup's `left: 2px / 22px` are measured inside the 1 pt border, so the
//  knob sits 3 pt from the outer edge at both ends — symmetric. That is why the
//  knob is laid out with padding and a leading/trailing alignment rather than a
//  hard `offset(x:)`: it is exact, and it flips correctly in RTL. EDGE (E5).
//

import SwiftUI

struct NToggle: View {
    static let trackWidth: CGFloat = 46
    static let trackHeight: CGFloat = 26
    static let knobDiameter: CGFloat = 20
    /// 1 pt border + the mockup's 2 pt inset.
    static let knobInset: CGFloat = 3

    private let label: String
    @Binding private var isOn: Bool

    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    init(_ label: String, isOn: Binding<Bool>) {
        self.label = label
        self._isOn = isOn
    }

    var body: some View {
        Button {
            isOn.toggle()
        } label: {
            ZStack(alignment: isOn ? .trailing : .leading) {
                Capsule()
                    .fill(isOn ? Theme.Color.accent100 : Color.clear)
                    .overlay {
                        Capsule().strokeBorder(
                            isOn ? Theme.Color.accent : Theme.Color.divider,
                            lineWidth: Theme.Stroke.border
                        )
                    }

                Circle()
                    .fill(isOn ? Theme.Color.accent : Theme.Color.neutral400)
                    .frame(width: Self.knobDiameter, height: Self.knobDiameter)
                    .padding(Self.knobInset)
            }
            .frame(width: Self.trackWidth, height: Self.trackHeight)
            .contentShape(Capsule())
            // EDGE (E17): 46 × 26 — wide enough, 18 pt short vertically. The
            // row it sits in is sized by the label beside it, so the target
            // has to grow without the track doing.
            .nadeHitTarget()
        }
        .buttonStyle(.plain)
        // EDGE (E7): no animation under Reduce Motion — the knob jumps.
        .animation(Theme.Motion.toggle(reduceMotion: reduceMotion), value: isOn)
        // EDGE (E1): a switch is chrome; it keeps its 46 × 26 geometry.
        .dynamicTypeSize(...Theme.Metrics.chromeTypeCeiling)
        // EDGE (E6)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(label)
        .accessibilityValue(isOn ? Text("On", comment: "Toggle on") : Text("Off", comment: "Toggle off"))
        .accessibilityAddTraits(.isToggle)
        .accessibilityAddTraits(isOn ? .isSelected : [])
    }
}

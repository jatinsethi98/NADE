//
//  ScheduleSheet.swift
//  NADE
//
//  1d (`DESIGN.md` §1d). A bottom sheet over 1c, editing a **copy** of the
//  schedule — the footer is Cancel + Save, so nothing may be written until Save.
//

import SwiftUI

struct ScheduleSheet: View {

    private enum M {
        static let horizontal = Theme.Space.screen
        static let top: CGFloat = 16
        static let bottom: CGFloat = 34
        static let grabberW: CGFloat = 38
        static let grabberH: CGFloat = 4
        static let grabberBottom: CGFloat = 16
        static let titleSize: CGFloat = 20
        static let titleBottom: CGFloat = 18

        static let repeatGap: CGFloat = 12
        static let repeatBottom: CGFloat = 16
        static let labelSize: CGFloat = 14
        static let chipPadV: CGFloat = 6
        static let chipPadH: CGFloat = 12
        static let chipGap: CGFloat = 8
        static let chipCaret: CGFloat = 10

        static let blockV: CGFloat = 16
        static let smallLabel: CGFloat = 12
        static let smallLabelBottom: CGFloat = 11
        static let circleGap: CGFloat = 8
        static let circleSize: CGFloat = 12
        static let valueSize: CGFloat = 15

        static let endsBottom: CGFloat = 18
        static let endsRowV: CGFloat = 9
        static let endsGap: CGFloat = 11
        static let endsValueSize: CGFloat = 13

        static let footerTop: CGFloat = 16
        static let footerGap: CGFloat = 10
    }

    let model: AgentBuilderModel
    @State private var isPickingTime = false

    var body: some View {
        VStack(spacing: 0) {
            Capsule()
                .fill(Theme.Color.neutral400)
                .frame(width: M.grabberW, height: M.grabberH)
                .padding(.bottom, M.grabberBottom)
                .accessibilityHidden(true)

            Text("How often")
                .font(Theme.Font.heading(M.titleSize))
                .foregroundStyle(Theme.Color.ink)
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(.bottom, M.titleBottom)
                .accessibilityAddTraits(.isHeader)

            ScrollView {
                VStack(spacing: 0) {
                    repeatEvery
                    Hairline()
                    repeatOn
                    Hairline()
                    atRow
                    Hairline()
                    ends
                }
            }

            Hairline()
            footer
        }
        .padding(.top, M.top)
        .padding(.horizontal, M.horizontal)
        .padding(.bottom, M.bottom)
        .background(Theme.Color.surface)
        .presentationDetents([.medium, .large])
        .presentationDragIndicator(.hidden)
    }

    // MARK: - Repeat every

    private var repeatEvery: some View {
        HStack(spacing: M.repeatGap) {
            Text("Repeat every")
                .font(Theme.Font.body(M.labelSize))
                .foregroundStyle(Theme.Color.ink)

            NStepper("", value: Binding(get: { model.draftSchedule.interval },
                                        set: { model.draftSchedule.interval = $0 }),
                     in: 1...30)
                .accessibilityIdentifier("schedule.interval")

            Spacer(minLength: 0)

            // The chip **cycles** day → week → month. It is not a menu.
            Button {
                model.draftSchedule.unit = model.draftSchedule.unit.next
            } label: {
                HStack(spacing: M.chipGap) {
                    Text(model.draftSchedule.interval > 1
                         ? model.draftSchedule.unit.plural
                         : model.draftSchedule.unit.singular)
                        .font(Theme.Font.body(M.labelSize))
                        .foregroundStyle(Theme.Color.ink)
                    Text(verbatim: "▼")
                        .font(Theme.Font.body(M.chipCaret))
                        .foregroundStyle(Theme.Color.accent)
                }
                .padding(.vertical, M.chipPadV)
                .padding(.horizontal, M.chipPadH)
                .overlay {
                    RoundedRectangle(cornerRadius: Theme.Radius.md)
                        .strokeBorder(Theme.Color.divider, lineWidth: Theme.Stroke.border)
                }
                .nadeHitTarget()   // EDGE (E17): the chip is ~30 pt tall
            }
            .buttonStyle(.plain)
            .accessibilityLabel(Text("Repeat unit", comment: "1d unit chip, spoken name"))
            .accessibilityValue(model.draftSchedule.unit.singular)
            .accessibilityIdentifier("schedule.unit")
        }
        .padding(.bottom, M.repeatBottom)
    }

    // MARK: - Repeat on

    /// **Always visible, at every unit.** The mockup renders this block
    /// unconditionally and applies no disabled styling; only the persisted value
    /// is unit-sensitive (`byweekday` is written only when the unit is week).
    private var repeatOn: some View {
        VStack(alignment: .leading, spacing: 0) {
            Text("Repeat on")
                .font(Theme.Font.body(M.smallLabel))
                .foregroundStyle(Theme.Color.ink55)
                .padding(.bottom, M.smallLabelBottom)

            HStack(spacing: M.circleGap) {
                ForEach(Array(ScheduleDraft.weekdayCodes.enumerated()), id: \.offset) { index, code in
                    let isOn = model.draftSchedule.days.contains(code)
                    Button {
                        if isOn { model.draftSchedule.days.remove(code) }
                        else { model.draftSchedule.days.insert(code) }
                    } label: {
                        Text(ScheduleDraft.weekdayInitials[index])
                            .font(Theme.Font.body(M.circleSize))
                            .foregroundStyle(isOn ? Theme.Color.accent800 : Theme.Color.ink62)
                            .frame(maxWidth: .infinity)
                            .aspectRatio(1, contentMode: .fit)
                            .background(Circle().fill(isOn ? Theme.Color.accent100 : .clear))
                            .overlay {
                                Circle().strokeBorder(
                                    isOn ? Theme.Color.accent : Theme.Color.divider,
                                    lineWidth: Theme.Stroke.border
                                )
                            }
                    }
                    .buttonStyle(.plain)
                    .accessibilityLabel(code.capitalized)
                    .accessibilityAddTraits(isOn ? [.isSelected] : [])
                    .accessibilityIdentifier("schedule.day.\(code)")
                }
            }
        }
        .padding(.vertical, M.blockV)
    }

    // MARK: - At

    /// `+ Added`: the mockup has no time control, yet `schedule.at` exists and
    /// the builder's sentence says "at 8:00" — without this the time is
    /// unsettable.
    private var atRow: some View {
        VStack(alignment: .leading, spacing: 0) {
            Button { isPickingTime.toggle() } label: {
                HStack {
                    Text("At")
                        .font(Theme.Font.body(M.smallLabel))
                        .foregroundStyle(Theme.Color.ink55)
                    Spacer(minLength: 0)
                    Text(model.draftSchedule.at)
                        .font(Theme.Font.body(M.valueSize))
                        .tabularNumerals()
                        .foregroundStyle(Theme.Color.ink)
                }
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .accessibilityIdentifier("schedule.at")

            if isPickingTime {
                DatePicker("", selection: Binding(
                    get: { model.draftSchedule.atDate },
                    set: { model.draftSchedule.setAt($0) }
                ), displayedComponents: .hourAndMinute)
                .datePickerStyle(.wheel)
                .labelsHidden()
                .padding(.top, M.smallLabelBottom)
                .accessibilityIdentifier("schedule.at.picker")
            }
        }
        .padding(.vertical, M.blockV)
    }

    // MARK: - Ends

    private var ends: some View {
        VStack(alignment: .leading, spacing: 0) {
            Text("Ends")
                .font(Theme.Font.body(M.smallLabel))
                .foregroundStyle(Theme.Color.ink55)
                .padding(.bottom, Theme.Space.s2)

            ForEach(ScheduleDraft.EndKind.allCases, id: \.rawValue) { kind in
                Button {
                    model.draftSchedule.endKind = kind
                } label: {
                    HStack(spacing: M.endsGap) {
                        NRadioDot(isSelected: model.draftSchedule.endKind == kind)
                        Text(Self.endLabel(kind))
                            .font(Theme.Font.body(M.labelSize))
                            .foregroundStyle(Theme.Color.ink)
                        Spacer(minLength: 0)
                        Text(model.draftSchedule.endValue(for: kind))
                            .font(Theme.Font.body(M.endsValueSize))
                            .tabularNumerals()
                            // It is the **muted** state that is coloured: ink
                            // when selected, ink62 when not.
                            .foregroundStyle(model.draftSchedule.endKind == kind
                                             ? Theme.Color.ink : Theme.Color.ink62)
                    }
                    .padding(.vertical, M.endsRowV)
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .accessibilityAddTraits(model.draftSchedule.endKind == kind ? [.isSelected] : [])
                .accessibilityIdentifier("schedule.ends.\(kind.rawValue)")
            }

            if model.draftSchedule.endKind == .on {
                DatePicker("", selection: Binding(get: { model.draftSchedule.endDatePicked() },
                                                  set: { model.draftSchedule.setEndDatePicked($0) }),
                           displayedComponents: .date)
                    .datePickerStyle(.compact)
                    .labelsHidden()
                    .accessibilityIdentifier("schedule.ends.date")
            }
            if model.draftSchedule.endKind == .after {
                NStepper("", value: Binding(get: { model.draftSchedule.endCount },
                                            set: { model.draftSchedule.endCount = $0 }),
                         in: 1...999)
                    .accessibilityIdentifier("schedule.ends.count")
            }
        }
        .padding(.top, M.blockV)
        .padding(.bottom, M.endsBottom)
    }

    static func endLabel(_ kind: ScheduleDraft.EndKind) -> String {
        switch kind {
        case .never: String(localized: "Never", comment: "1d Ends option")
        case .on: String(localized: "On", comment: "1d Ends option")
        case .after: String(localized: "After", comment: "1d Ends option")
        }
    }

    // MARK: - Footer

    /// The primary button's label **is the recurrence summary**, and it never
    /// contains a time.
    private var footer: some View {
        HStack(spacing: M.footerGap) {
            NButton(String(localized: "Cancel", comment: "1d footer"),
                    variant: .secondary, width: .flexible) {
                model.isSchedulePresented = false
            }
            .accessibilityIdentifier("schedule.cancel")

            NButton(model.draftSchedule.summary, variant: .primary, width: .flexible) {
                Task { await model.saveSchedule() }
            }
            // 1c's footer guards on `isBusy` and this did not, so repeated taps
            // queued repeated PATCHes onto the write chain.
            .disabled(model.isBusy)
            .accessibilityIdentifier("schedule.save")
        }
        .padding(.top, M.footerTop)
    }
}

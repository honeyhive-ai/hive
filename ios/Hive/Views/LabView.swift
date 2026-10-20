import SwiftUI

struct LabView: View {
    @Environment(AppModel.self) private var model
    @Environment(\.scenePhase) private var scenePhase

    var body: some View {
        NavigationStack {
            ScrollView {
                if let lab = model.lab {
                    VStack(spacing: 16) {
                        LazyVGrid(columns: [GridItem(.flexible(), spacing: 12), GridItem(.flexible(), spacing: 12)],
                                  spacing: 12) {
                            StatTile(title: "Power", value: lab.powerWatts.map { "\(Int($0.rounded())) W" },
                                     systemImage: "bolt.fill", tint: .yellow)
                            StatTile(title: "Cost / hour", value: lab.costPerHour.map { Self.money($0, digits: 3) },
                                     systemImage: "dollarsign.circle", tint: .green)
                            StatTile(title: "Cost / month", value: lab.costPerMonth.map { Self.money($0, digits: 2) },
                                     systemImage: "calendar", tint: .mint, footnote: "30-day projection")
                            StatTile(title: "Rate", value: lab.ratePerKwh.map { Self.money($0, digits: 3) + "/kWh" },
                                     systemImage: "gauge.with.needle", tint: .orange)
                        }

                        section("Workers", systemImage: "server.rack") {
                            ForEach(lab.workers) { w in
                                HStack {
                                    Text(w.name).font(.callout.monospaced())
                                    Spacer()
                                    Label(w.up ? "Up" : "Down", systemImage: "circle.fill")
                                        .labelStyle(StatusDotLabelStyle(color: w.up ? .green : .red))
                                }
                                .accessibilityElement(children: .combine)
                            }
                            if lab.workers.isEmpty { Text("No worker data").foregroundStyle(.secondary) }
                        }

                        section("Temperatures", systemImage: "thermometer.medium") {
                            ForEach(lab.temps) { t in
                                TempRow(temp: t)
                            }
                            if lab.temps.isEmpty { Text("No temperature data").foregroundStyle(.secondary) }
                        }

                        Text("Updated \(lab.updatedAt.formatted(date: .omitted, time: .standard))")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                    .padding(16)
                } else {
                    ProgressView().padding(.top, 120)
                }
            }
            .refreshable { await model.refreshLab() }
            .navigationTitle("Lab")
            .task(id: scenePhase) {
                guard scenePhase == .active else { return }
                while !Task.isCancelled {
                    await model.refreshLab()
                    try? await Task.sleep(for: .seconds(10))
                }
            }
        }
    }

    static func money(_ v: Double, digits: Int) -> String {
        v.formatted(.currency(code: "USD").precision(.fractionLength(digits)))
    }

    private func section<C: View>(_ title: String, systemImage: String, @ViewBuilder content: () -> C) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            Label(title, systemImage: systemImage).font(.headline)
            content()
        }
        .padding(16)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(.fill.quaternary, in: .rect(cornerRadius: 20))
    }
}

struct StatTile: View {
    let title: String
    let value: String?
    let systemImage: String
    let tint: Color
    var footnote: String?

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Label(title, systemImage: systemImage)
                .font(.caption.weight(.semibold))
                .foregroundStyle(tint)
            Text(value ?? "—")
                .font(.title2.weight(.semibold).monospacedDigit())
                .contentTransition(.numericText())
                .lineLimit(1)
                .minimumScaleFactor(0.6)
            if let footnote { Text(footnote).font(.caption2).foregroundStyle(.secondary) }
        }
        .padding(14)
        .frame(maxWidth: .infinity, minHeight: 92, alignment: .topLeading)
        .glassEffect(.regular, in: .rect(cornerRadius: 20))
        .accessibilityElement(children: .combine)
    }
}

struct TempRow: View {
    let temp: LabTemp

    private var color: Color {
        switch temp.celsius {
        case ..<55: .green
        case ..<75: .yellow
        case ..<85: .orange
        default: .red
        }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack {
                Text(temp.name).font(.callout)
                Spacer()
                Text("\(temp.celsius, format: .number.precision(.fractionLength(0)))°C")
                    .font(.callout.monospacedDigit().weight(.medium))
                    .foregroundStyle(color)
            }
            ProgressView(value: min(max(temp.celsius, 0), 100), total: 100).tint(color)
        }
        .accessibilityElement(children: .combine)
    }
}

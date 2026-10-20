import Charts
import SwiftUI

/// The Lab tab's solar pane: how much of the lab's energy the panels covered,
/// what they're doing now, and solar production against the lab's draw.
struct SolarSection: View {
    let solar: LabSolar
    let labWatts: Double?

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            HStack {
                Label("Solar offset", systemImage: "sun.max.fill").font(.headline)
                Spacer()
                if !solar.up {
                    Label("Gateway offline", systemImage: "circle.fill")
                        .labelStyle(StatusDotLabelStyle(color: .red))
                        .font(.caption)
                }
            }

            offset
            now
            if !solar.history.isEmpty { chart }
            totals
        }
        .padding(16)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(.fill.quaternary, in: .rect(cornerRadius: 20))
    }

    // MARK: Offset

    private var offset: some View {
        HStack(spacing: 16) {
            Gauge(value: solar.labOffset24h ?? 0) {
                EmptyView()
            } currentValueLabel: {
                Text(solar.labOffset24h.map { $0.formatted(.percent.precision(.fractionLength(0))) } ?? "—")
                    .font(.system(.title3, design: .rounded).weight(.semibold).monospacedDigit())
            }
            .gaugeStyle(.accessoryCircularCapacity)
            .tint(.yellow)
            .scaleEffect(1.25)
            .frame(width: 76, height: 76)

            VStack(alignment: .leading, spacing: 4) {
                Text("of the lab's energy came from your panels")
                    .font(.callout)
                if let sol = solar.labSolarKwh24h, let lab = solar.labKwh24h {
                    Text("\(Self.kwh(sol)) of \(Self.kwh(lab)) in the last 24 h")
                        .font(.caption.monospacedDigit())
                        .foregroundStyle(.secondary)
                }
                if let value = solar.labSolarValue24h {
                    Text("Worth \(LabView.money(value, digits: 2)) at your rate")
                        .font(.caption.weight(.medium))
                        .foregroundStyle(.green)
                }
            }
        }
        .accessibilityElement(children: .combine)
    }

    // MARK: Now

    private var now: some View {
        VStack(alignment: .leading, spacing: 8) {
            LazyVGrid(columns: [GridItem(.flexible(), spacing: 8), GridItem(.flexible(), spacing: 8),
                                GridItem(.flexible(), spacing: 8)], spacing: 8) {
                mini("Solar", Self.watts(solar.productionWatts), "sun.max", .yellow)
                mini("House", Self.watts(solar.consumptionWatts), "house", .blue)
                mini(gridTitle, Self.watts(solar.gridWatts.map(abs)), gridIcon, gridColor)
            }
            if let covered = labCoveredNow {
                Text("Solar is covering \(covered.formatted(.percent.precision(.fractionLength(0)))) of the lab right now")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
        }
    }

    private var labCoveredNow: Double? {
        guard let sol = solar.labSolarWatts, let lab = labWatts, lab > 0 else { return nil }
        return min(max(sol / lab, 0), 1)
    }

    private var gridTitle: String {
        guard let g = solar.gridWatts else { return "Grid" }
        return g < 0 ? "Exporting" : "Importing"
    }

    private var gridIcon: String {
        (solar.gridWatts ?? 0) < 0 ? "arrow.up.forward.circle" : "arrow.down.backward.circle"
    }

    private var gridColor: Color { (solar.gridWatts ?? 0) < 0 ? .green : .orange }

    private func mini(_ title: String, _ value: String, _ icon: String, _ tint: Color) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            Label(title, systemImage: icon)
                .font(.caption2.weight(.semibold))
                .foregroundStyle(tint)
                .lineLimit(1)
            Text(value)
                .font(.callout.weight(.semibold).monospacedDigit())
                .contentTransition(.numericText())
                .lineLimit(1)
                .minimumScaleFactor(0.7)
        }
        .padding(10)
        .frame(maxWidth: .infinity, alignment: .leading)
        .glassEffect(.regular, in: .rect(cornerRadius: 14))
        .accessibilityElement(children: .combine)
    }

    // MARK: Chart

    private var chart: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text("Last 24 h").font(.caption.weight(.semibold)).foregroundStyle(.secondary)
            Chart {
                ForEach(solar.history) { p in
                    if let w = p.solarWatts {
                        AreaMark(x: .value("Time", p.t), y: .value("Watts", w), series: .value("Series", "Solar"))
                            .foregroundStyle(.linearGradient(colors: [.yellow.opacity(0.55), .yellow.opacity(0.05)],
                                                             startPoint: .top, endPoint: .bottom))
                            .interpolationMethod(.monotone)
                    }
                }
                ForEach(solar.history) { p in
                    if let w = p.labWatts {
                        LineMark(x: .value("Time", p.t), y: .value("Watts", w), series: .value("Series", "Lab"))
                            .foregroundStyle(.orange)
                            .lineStyle(StrokeStyle(lineWidth: 2))
                            .interpolationMethod(.monotone)
                    }
                }
            }
            .chartYAxis {
                AxisMarks(position: .leading) { v in
                    AxisGridLine()
                    AxisValueLabel { if let w = v.as(Double.self) { Text(Self.shortWatts(w)) } }
                }
            }
            .chartXAxis {
                AxisMarks(values: .stride(by: .hour, count: 6)) { _ in
                    AxisGridLine()
                    AxisValueLabel(format: .dateTime.hour())
                }
            }
            .frame(height: 150)
            .accessibilityLabel("Solar production and lab power over the last 24 hours")

            HStack(spacing: 14) {
                legend("Solar", .yellow)
                legend("Lab", .orange)
            }
            .font(.caption2)
            .foregroundStyle(.secondary)
        }
    }

    private func legend(_ name: String, _ color: Color) -> some View {
        HStack(spacing: 4) {
            Circle().fill(color).frame(width: 7, height: 7)
            Text(name)
        }
    }

    // MARK: Totals

    private var totals: some View {
        VStack(spacing: 6) {
            row("Produced", solar.producedKwh24h.map(Self.kwh))
            row("House used", solar.houseKwh24h.map(Self.kwh))
            if let p = solar.producedKwh24h, let h = solar.houseKwh24h, h > 0 {
                row("Solar vs house use", (p / h).formatted(.percent.precision(.fractionLength(0))))
            }
            row("From grid", solar.gridImportKwh24h.map(Self.kwh))
            row("To grid", solar.gridExportKwh24h.map(Self.kwh))
        }
        .font(.callout)
    }

    @ViewBuilder
    private func row(_ title: String, _ value: String?) -> some View {
        if let value {
            HStack {
                Text(title).foregroundStyle(.secondary)
                Spacer()
                Text(value).monospacedDigit()
            }
            .accessibilityElement(children: .combine)
        }
    }

    // MARK: Formatting

    static func watts(_ w: Double?) -> String {
        guard let w else { return "—" }
        return abs(w) >= 1000 ? "\((w / 1000).formatted(.number.precision(.fractionLength(1)))) kW" : "\(Int(w.rounded())) W"
    }

    static func shortWatts(_ w: Double) -> String {
        abs(w) >= 1000 ? "\((w / 1000).formatted(.number.precision(.fractionLength(0...1))))k" : "\(Int(w))"
    }

    static func kwh(_ v: Double) -> String {
        "\(v.formatted(.number.precision(.fractionLength(v < 10 ? 2 : 1)))) kWh"
    }
}

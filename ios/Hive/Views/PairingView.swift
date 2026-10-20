import SwiftUI
import UIKit

struct PairingView: View {
    @Environment(AppModel.self) private var model
    @State private var pasted = ""
    @State private var manualURL = ""
    @State private var manualToken = ""
    @State private var showScanner = false
    @State private var busy = false
    @State private var error: String?
    @State private var showManual = false

    var body: some View {
        ScrollView {
            VStack(spacing: 24) {
                header

                VStack(spacing: 12) {
                    Button {
                        showScanner = true
                    } label: {
                        Label("Scan pairing code", systemImage: "qrcode.viewfinder")
                            .font(.headline)
                            .frame(maxWidth: .infinity)
                            .padding(.vertical, 6)
                    }
                    .buttonStyle(.glassProminent)
                    .controlSize(.large)

                    Button {
                        if let s = UIPasteboard.general.string { pasted = s; Task { await pairFromText() } }
                    } label: {
                        Label("Paste hive://pair link", systemImage: "doc.on.clipboard")
                            .frame(maxWidth: .infinity)
                            .padding(.vertical, 4)
                    }
                    .buttonStyle(.glass)
                    .controlSize(.large)
                }

                card {
                    VStack(alignment: .leading, spacing: 10) {
                        Text("Pairing link").font(.subheadline.weight(.semibold))
                        TextField("hive://pair?url=…&token=…", text: $pasted, axis: .vertical)
                            .textInputAutocapitalization(.never)
                            .autocorrectionDisabled()
                            .font(.callout.monospaced())
                            .lineLimit(1...4)
                        Button("Pair") { Task { await pairFromText() } }
                            .buttonStyle(.bordered)
                            .disabled(pasted.trimmingCharacters(in: .whitespaces).isEmpty || busy)
                    }
                }

                DisclosureGroup("Enter server manually", isExpanded: $showManual) {
                    VStack(alignment: .leading, spacing: 10) {
                        TextField("http://100.x.y.z:8787", text: $manualURL)
                            .keyboardType(.URL)
                            .textInputAutocapitalization(.never)
                            .autocorrectionDisabled()
                        SecureField("Token", text: $manualToken)
                        Button("Connect") { Task { await pairManual() } }
                            .buttonStyle(.bordered)
                            .disabled(manualURL.isEmpty || manualToken.isEmpty || busy)
                    }
                    .textFieldStyle(.roundedBorder)
                    .padding(.top, 8)
                }
                .padding(16)
                .background(.fill.quaternary, in: .rect(cornerRadius: 20))

                if busy { ProgressView("Checking the server…") }
                if let error {
                    Label(error, systemImage: "exclamationmark.triangle")
                        .font(.callout)
                        .foregroundStyle(.red)
                        .frame(maxWidth: .infinity, alignment: .leading)
                }
            }
            .padding(20)
        }
        .scrollDismissesKeyboard(.interactively)
        .navigationTitle("Pair")
        .navigationBarTitleDisplayMode(.inline)
        .fullScreenCover(isPresented: $showScanner) {
            QRScannerSheet { code in
                showScanner = false
                pasted = code
                Task { await pairFromText() }
            }
        }
    }

    private var header: some View {
        VStack(spacing: 12) {
            Image(systemName: "hexagon.fill")
                .font(.system(size: 64))
                .foregroundStyle(Theme.accent.gradient)
                .overlay(Image(systemName: "point.3.connected.trianglepath.dotted")
                    .font(.system(size: 26, weight: .bold)).foregroundStyle(.black.opacity(0.7)))
            Text("Connect to your home host").font(.title2.bold())
            Text("On the server, run **hive serve --pair** and scan the QR code it prints. The phone talks to it over Tailscale.")
                .font(.callout)
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
        }
        .padding(.top, 24)
    }

    private func card<C: View>(@ViewBuilder _ content: () -> C) -> some View {
        content()
            .padding(16)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(.fill.quaternary, in: .rect(cornerRadius: 20))
    }

    private func pairFromText() async {
        guard case let .pair(cfg)? = DeepLink.parse(text: pasted) else {
            error = "That isn't a hive://pair link."
            return
        }
        await pair(cfg)
    }

    private func pairManual() async {
        guard let url = DeepLink.normalizeServerURL(manualURL) else {
            error = APIError.invalidURL.errorDescription
            return
        }
        await pair(ServerConfig(baseURL: url, token: manualToken.trimmingCharacters(in: .whitespacesAndNewlines)))
    }

    private func pair(_ cfg: ServerConfig) async {
        busy = true
        error = nil
        defer { busy = false }
        do {
            try await model.pair(cfg)
        } catch {
            self.error = (error as? APIError)?.errorDescription ?? error.localizedDescription
        }
    }
}

/// Shown when a `hive://pair` link is opened while the app is running.
struct PairConfirmView: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    let candidate: ServerConfig
    @State private var busy = false
    @State private var error: String?

    var body: some View {
        VStack(spacing: 18) {
            Image(systemName: "link.badge.plus").font(.system(size: 44)).foregroundStyle(Theme.accent)
            Text("Pair with this server?").font(.title3.bold())
            Text(candidate.baseURL.absoluteString).font(.callout.monospaced()).foregroundStyle(.secondary)
            if model.isPaired {
                Text("This replaces the current pairing.").font(.footnote).foregroundStyle(.secondary)
            }
            if let error { Text(error).font(.footnote).foregroundStyle(.red).multilineTextAlignment(.center) }
            Button {
                Task {
                    busy = true
                    defer { busy = false }
                    do {
                        try await model.pair(candidate)
                        await model.refreshAll()
                        dismiss()
                    } catch {
                        self.error = (error as? APIError)?.errorDescription ?? error.localizedDescription
                    }
                }
            } label: {
                Group { if busy { ProgressView() } else { Text("Pair") } }.frame(maxWidth: .infinity)
            }
            .buttonStyle(.glassProminent)
            .controlSize(.large)
            .disabled(busy)
            .accessibilityIdentifier("pair.confirm")
        }
        .padding(24)
        .toolbar {
            ToolbarItem(placement: .cancellationAction) {
                Button("Cancel", role: .cancel) { model.pendingPair = nil }
            }
        }
    }
}

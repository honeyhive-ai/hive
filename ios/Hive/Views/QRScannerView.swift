@preconcurrency import AVFoundation
import SwiftUI
import UIKit

/// Full-screen camera with a QR reader; calls `onCode` once with the first
/// `hive://` code it sees.
struct QRScannerSheet: View {
    @Environment(\.dismiss) private var dismiss
    let onCode: (String) -> Void
    @State private var denied = false

    var body: some View {
        ZStack {
            Color.black.ignoresSafeArea()
            if denied {
                ContentUnavailableView {
                    Label("Camera access is off", systemImage: "camera.fill")
                } description: {
                    Text("Allow camera access for Hive in Settings, or paste the pairing link instead.")
                } actions: {
                    Button("Open Settings") {
                        if let url = URL(string: UIApplication.openSettingsURLString) {
                            UIApplication.shared.open(url)
                        }
                    }
                }
            } else {
                QRScannerView(onCode: onCode).ignoresSafeArea()
                RoundedRectangle(cornerRadius: 28)
                    .strokeBorder(.white.opacity(0.85), style: StrokeStyle(lineWidth: 3, dash: [22, 12]))
                    .frame(width: 250, height: 250)
                    .allowsHitTesting(false)
            }
            VStack {
                HStack {
                    Spacer()
                    Button { dismiss() } label: {
                        Image(systemName: "xmark").font(.headline).padding(6)
                    }
                    .buttonStyle(.glass)
                    .buttonBorderShape(.circle)
                    .accessibilityLabel("Close scanner")
                }
                Spacer()
                Text("Point at the QR code from hive serve --pair")
                    .font(.callout)
                    .padding(.horizontal, 16).padding(.vertical, 10)
                    .glassEffect()
            }
            .padding(20)
        }
        .task {
            switch AVCaptureDevice.authorizationStatus(for: .video) {
            case .authorized: denied = false
            case .notDetermined: denied = !(await AVCaptureDevice.requestAccess(for: .video))
            default: denied = true
            }
        }
    }
}

struct QRScannerView: UIViewControllerRepresentable {
    let onCode: (String) -> Void

    func makeUIViewController(context: Context) -> QRScannerController {
        let vc = QRScannerController()
        vc.onCode = onCode
        return vc
    }

    func updateUIViewController(_ vc: QRScannerController, context: Context) {}
}

/// Owns the capture session on a private queue.
private final class CaptureBox: @unchecked Sendable {
    let session = AVCaptureSession()
    let queue = DispatchQueue(label: "hive.qr.session")
}

final class QRScannerController: UIViewController, AVCaptureMetadataOutputObjectsDelegate {
    var onCode: ((String) -> Void)?
    private let box = CaptureBox()
    private var preview: AVCaptureVideoPreviewLayer?
    private var delivered = false

    override func viewDidLoad() {
        super.viewDidLoad()
        view.backgroundColor = .black
        guard let device = AVCaptureDevice.default(for: .video),
              let input = try? AVCaptureDeviceInput(device: device)
        else { return } // simulator / no camera: leave the black view
        let session = box.session
        session.beginConfiguration()
        if session.canAddInput(input) { session.addInput(input) }
        let output = AVCaptureMetadataOutput()
        if session.canAddOutput(output) {
            session.addOutput(output)
            output.setMetadataObjectsDelegate(self, queue: .main)
            output.metadataObjectTypes = [.qr]
        }
        session.commitConfiguration()
        let layer = AVCaptureVideoPreviewLayer(session: session)
        layer.videoGravity = .resizeAspectFill
        view.layer.addSublayer(layer)
        preview = layer
    }

    override func viewDidLayoutSubviews() {
        super.viewDidLayoutSubviews()
        preview?.frame = view.bounds
    }

    override func viewWillAppear(_ animated: Bool) {
        super.viewWillAppear(animated)
        let box = self.box
        box.queue.async { if !box.session.isRunning { box.session.startRunning() } }
    }

    override func viewWillDisappear(_ animated: Bool) {
        super.viewWillDisappear(animated)
        let box = self.box
        box.queue.async { if box.session.isRunning { box.session.stopRunning() } }
    }

    nonisolated func metadataOutput(_ output: AVCaptureMetadataOutput,
                                    didOutput metadataObjects: [AVMetadataObject],
                                    from connection: AVCaptureConnection) {
        let codes = metadataObjects.compactMap { ($0 as? AVMetadataMachineReadableCodeObject)?.stringValue }
        // Delegate queue is .main.
        MainActor.assumeIsolated {
            guard !delivered, let code = codes.first(where: { $0.lowercased().hasPrefix("hive://") }) else { return }
            delivered = true
            UINotificationFeedbackGenerator().notificationOccurred(.success)
            onCode?(code)
        }
    }
}

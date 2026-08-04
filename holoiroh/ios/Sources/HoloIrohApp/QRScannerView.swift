import SwiftUI
import AVFoundation
#if canImport(UIKit)
import UIKit
#endif

/// Scans Quick Response (QR) codes with AVFoundation.
/// The scanner delivers the first decoded text payload through `onCode`.
/// It delivers callbacks on the main thread.
/// It restricts metadata output to QR codes.
/// It starts and stops the capture session on a serial background queue.
///
/// The app must define `NSCameraUsageDescription` in its `Info.plist`.
/// iOS terminates the app if this key is absent when camera access starts.
/// The scanner requests access when authorization is undetermined.
/// It calls `onAuthorizationDenied` for denied, restricted, or unavailable access.
#if canImport(UIKit)
struct QRScannerView: UIViewRepresentable {
    /// Receives the first decoded QR text on the main thread.
    let onCode: (String) -> Void

    /// Reports camera denial, restriction, or unavailable capture hardware.
    /// The scanner calls this closure on the main thread.
    var onAuthorizationDenied: () -> Void = {}

    func makeCoordinator() -> Coordinator {
        Coordinator(onCode: onCode, onAuthorizationDenied: onAuthorizationDenied)
    }

    func makeUIView(context: Context) -> CameraPreviewView {
        let view = CameraPreviewView()
        view.backgroundColor = .black
        context.coordinator.attach(to: view)
        context.coordinator.startWhenAuthorized()
        return view
    }

    func updateUIView(_ uiView: CameraPreviewView, context: Context) {
        // Nothing to reconfigure per update; the session + preview layer are
        // owned by the coordinator and sized by the view's `layoutSubviews`.
    }

    /// Stops capture before SwiftUI removes the preview.
    static func dismantleUIView(_ uiView: CameraPreviewView, coordinator: Coordinator) {
        coordinator.stop()
    }

    /// Owns the capture session and metadata delegate.
    /// The coordinator keeps references that the value-type view cannot store.
    final class Coordinator: NSObject, AVCaptureMetadataOutputObjectsDelegate {
        private let onCode: (String) -> Void
        private let onAuthorizationDenied: () -> Void

        private let session = AVCaptureSession()
        /// Serializes session changes and metadata delivery.
        /// Session operations do not run on the main thread.
        private let sessionQueue = DispatchQueue(label: "com.holoiroh.qrscanner.session")
        private weak var view: CameraPreviewView?

        /// Prevents more than one decoded payload.
        /// Only `sessionQueue` accesses this value.
        private var hasDelivered = false
        private var isConfigured = false

        init(onCode: @escaping (String) -> Void, onAuthorizationDenied: @escaping () -> Void) {
            self.onCode = onCode
            self.onAuthorizationDenied = onAuthorizationDenied
        }

        func attach(to view: CameraPreviewView) {
            self.view = view
            view.previewLayer.session = session
            view.previewLayer.videoGravity = .resizeAspectFill
        }

        /// Resolves camera authorization.
        /// It starts capture only when access is authorized.
        func startWhenAuthorized() {
            switch AVCaptureDevice.authorizationStatus(for: .video) {
            case .authorized:
                configureAndStart()
            case .notDetermined:
                AVCaptureDevice.requestAccess(for: .video) { [weak self] granted in
                    guard let self else { return }
                    if granted {
                        self.configureAndStart()
                    } else {
                        DispatchQueue.main.async { self.onAuthorizationDenied() }
                    }
                }
            case .denied, .restricted:
                DispatchQueue.main.async { [weak self] in self?.onAuthorizationDenied() }
            @unknown default:
                DispatchQueue.main.async { [weak self] in self?.onAuthorizationDenied() }
            }
        }

        /// Configures the capture graph once and starts the session.
        /// Work runs on `sessionQueue`.
        /// Configuration failure calls `onAuthorizationDenied`.
        private func configureAndStart() {
            sessionQueue.async { [weak self] in
                guard let self else { return }
                if !self.isConfigured {
                    guard self.configureSession() else {
                        DispatchQueue.main.async { self.onAuthorizationDenied() }
                        return
                    }
                    self.isConfigured = true
                }
                if !self.session.isRunning {
                    self.session.startRunning()
                }
            }
        }

        /// Adds the camera input and QR metadata output.
        /// It returns `false` when the input or output is unavailable.
        private func configureSession() -> Bool {
            session.beginConfiguration()
            defer { session.commitConfiguration() }

            guard
                let device = AVCaptureDevice.default(for: .video),
                let input = try? AVCaptureDeviceInput(device: device),
                session.canAddInput(input)
            else {
                return false
            }
            session.addInput(input)

            let output = AVCaptureMetadataOutput()
            guard session.canAddOutput(output) else { return false }
            session.addOutput(output)

            // Deliver metadata on the session queue and restrict to QR. The
            // available-types must be set *after* the output is added to the
            // session, or `.qr` is not yet in `availableMetadataObjectTypes`.
            output.setMetadataObjectsDelegate(self, queue: sessionQueue)
            if output.availableMetadataObjectTypes.contains(.qr) {
                output.metadataObjectTypes = [.qr]
            } else {
                // No QR support on this capture output — treat as unusable.
                return false
            }

            return true
        }

        func stop() {
            sessionQueue.async { [weak session] in
                if session?.isRunning == true {
                    session?.stopRunning()
                }
            }
        }

        // MARK: - AVCaptureMetadataOutputObjectsDelegate

        func metadataOutput(
            _ output: AVCaptureMetadataOutput,
            didOutput metadataObjects: [AVMetadataObject],
            from connection: AVCaptureConnection
        ) {
            // Runs on `sessionQueue`. Deliver the first readable QR string
            // exactly once, then stop the session so we don't fire repeatedly
            // for the same code sitting in front of the camera.
            guard !hasDelivered else { return }
            guard
                let object = metadataObjects.first as? AVMetadataMachineReadableCodeObject,
                object.type == .qr,
                let value = object.stringValue
            else {
                return
            }

            hasDelivered = true
            if session.isRunning {
                session.stopRunning()
            }
            DispatchQueue.main.async { [weak self] in
                self?.onCode(value)
            }
        }
    }
}

/// Uses an `AVCaptureVideoPreviewLayer` as its backing layer.
/// The `layerClass` override keeps the preview layer sized with the view.
final class CameraPreviewView: UIView {
    override class var layerClass: AnyClass { AVCaptureVideoPreviewLayer.self }

    var previewLayer: AVCaptureVideoPreviewLayer {
        // Safe by construction: `layerClass` guarantees the backing layer's
        // type. A failure here would mean UIKit ignored `layerClass`, which
        // does not happen — so a hard trap is the correct fail-fast.
        guard let layer = layer as? AVCaptureVideoPreviewLayer else {
            fatalError("CameraPreviewView.layer was not an AVCaptureVideoPreviewLayer")
        }
        return layer
    }
}
#else
struct QRScannerView: View {
    let onCode: (String) -> Void
    var onAuthorizationDenied: () -> Void = {}

    var body: some View {
        Color.black
    }
}
#endif

import SwiftUI
import AVFoundation

/// Live QR-code scanner: a SwiftUI wrapper around a UIKit view whose
/// backing layer is an `AVCaptureVideoPreviewLayer`, driven by an
/// `AVCaptureSession` with an `AVCaptureMetadataOutput` restricted to
/// `.qr`. When a QR is decoded, its string payload is delivered once via
/// `onCode`.
///
/// This is the *scan* half of pairing (Project Aro PRD P0-2): it turns the
/// QR the Mac daemon prints (`mac-daemon/src/main.rs print_ticket_qr`, which
/// encodes the raw `iroh-live:…` ticket string) into that same string on the
/// phone, so `PairingView` can auto-fill the ticket field instead of the
/// user retyping a 100+-character token. It knows nothing about iroh or the
/// ticket format — it only decodes QR text and hands it up; `PairingTicket`
/// does the extraction and `PairingPhrase` the verification.
///
/// ## Structure mirrors `VideoRenderView`
/// Same `UIViewRepresentable` + `Coordinator` shape as the video render
/// surface: a dedicated `UIView` subclass whose `layerClass` is the capture
/// preview layer, a `Coordinator` holding the strong references the value-
/// type `View` cannot, `dismantleUIView` for deterministic teardown, and a
/// serial queue so the `AVCaptureSession` is never started/stopped on the
/// main thread (Apple documents `startRunning()` as blocking).
///
/// ## Camera permission (`NSCameraUsageDescription` required)
/// Starting the session triggers the camera-permission prompt the first
/// time. iOS **terminates the process** if `NSCameraUsageDescription` is
/// missing from the app's `Info.plist` when that happens — this cannot be
/// fixed in library code (a bare SwiftPM package has no `Info.plist`), so
/// the requirement is documented in
/// `Sources/HoloIrohApp/REQUIRED_INFO_PLIST_KEYS.md` for whoever wraps this
/// package in an Xcode app target. This view handles the *authorization
/// state* correctly-by-construction: it requests access when undetermined,
/// reports denied/restricted up via `onAuthorizationDenied` (so the caller
/// can show guidance instead of a black frame), and only starts the session
/// once authorized.
///
/// ## Headless-build honesty
/// The camera capture path cannot be exercised in a headless/simulator
/// build (there is no camera, and permission prompts need a real app host),
/// so this view's *runtime* behavior is not witnessed by the build. What
/// the build proves is that it compiles against the real iOS 17 SDK
/// (AVFoundation types, `sampleBufferRenderer`-style availability gating,
/// the delegate conformance). The decode → ticket-extraction → phrase
/// pipeline that *can* be witnessed lives in `PairingTicket` /
/// `PairingPhrase`, which are pure and are verified directly.
struct QRScannerView: UIViewRepresentable {
    /// Called (once) with the decoded QR string payload on the main thread.
    let onCode: (String) -> Void

    /// Called on the main thread if the camera is denied or restricted, so
    /// the caller can show guidance rather than a permanently black preview.
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

    /// Stop the session and drop references so nothing keeps capturing into
    /// a torn-down view.
    static func dismantleUIView(_ uiView: CameraPreviewView, coordinator: Coordinator) {
        coordinator.stop()
    }

    /// Owns the `AVCaptureSession` and the metadata delegate. Holds the
    /// strong references the value-type `View` cannot.
    final class Coordinator: NSObject, AVCaptureMetadataOutputObjectsDelegate {
        private let onCode: (String) -> Void
        private let onAuthorizationDenied: () -> Void

        private let session = AVCaptureSession()
        /// Serial queue for session start/stop and metadata delivery — never
        /// the main thread (Apple: `startRunning()` blocks).
        private let sessionQueue = DispatchQueue(label: "com.holoiroh.qrscanner.session")
        private weak var view: CameraPreviewView?

        /// Guards against delivering more than one decode. Only touched on
        /// `sessionQueue` (metadata delegate is dispatched there), so a plain
        /// `Bool` is safe without extra locking.
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

        /// Resolve camera authorization, then start the session if allowed.
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

        /// Build the capture graph (once) and start running, off the main
        /// thread. Any missing hardware/permission failure degrades to a
        /// black preview + denied callback rather than a crash.
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

        /// Wire camera input + a `.qr` metadata output. Returns `false` if
        /// the device/input/output can't be built (e.g. no camera in the
        /// simulator), so the caller can surface that instead of showing a
        /// dead preview.
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

/// A `UIView` whose backing layer *is* an `AVCaptureVideoPreviewLayer`,
/// via the `layerClass` override (the standard way to back a view with a
/// specific `CALayer` subclass so the layer is auto-sized to the view).
/// Same pattern `SampleBufferView` uses for the video render surface.
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

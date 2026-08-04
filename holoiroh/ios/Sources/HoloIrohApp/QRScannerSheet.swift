import SwiftUI

/// Presents a full-screen Quick Response (QR) scanner during pairing.
/// The sheet includes a camera preview, reticle, Cancel action, and permission guidance.
/// After decoding, the sheet calls `onScanned` with the raw string and dismisses.
/// `PairingView` extracts and fills the ticket.
struct QRScannerSheet: View {
    /// The scanner calls this closure on the main thread with the decoded QR string.
    let onScanned: (String) -> Void

    @Environment(\.dismiss) private var dismiss
    @State private var permissionDenied = false

    var body: some View {
        NavigationStack {
            ZStack {
                if permissionDenied {
                    deniedView
                } else {
                    QRScannerView(
                        onCode: { code in
                            onScanned(code)
                            dismiss()
                        },
                        onAuthorizationDenied: {
                            permissionDenied = true
                        }
                    )
                    .ignoresSafeArea()

                    reticle
                }
            }
            .navigationTitle("Scan the Mac's QR")
            #if os(iOS)
            .navigationBarTitleDisplayMode(.inline)
            #endif
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { dismiss() }
                }
            }
        }
    }

    /// Shows a square aiming guide over the camera preview.
    private var reticle: some View {
        RoundedRectangle(cornerRadius: 16)
            .strokeBorder(Color.white.opacity(0.9), lineWidth: 3)
            .frame(width: 240, height: 240)
            .shadow(radius: 8)
            .accessibilityHidden(true)
    }

    /// Shows camera-permission guidance.
    /// The user can cancel and paste the ticket instead.
    private var deniedView: some View {
        VStack(spacing: 16) {
            Image(systemName: "video.slash")
                .font(.system(size: 44))
                .foregroundStyle(.secondary)
            Text("Camera access is off")
                .font(.headline)
            Text("To scan the QR code, allow camera access for Aro in Settings. You can also cancel and paste the ticket text instead.")
                .font(.footnote)
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
                .padding(.horizontal, 32)
        }
        .padding()
    }
}

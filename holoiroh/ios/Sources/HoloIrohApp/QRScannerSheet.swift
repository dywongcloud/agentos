import SwiftUI

/// The full-screen sheet that hosts `QRScannerView` during pairing: a live
/// camera preview with a framing reticle, a Cancel button, and a
/// permission-denied fallback so the user is never left staring at a black
/// rectangle with no explanation.
///
/// On a successful decode it hands the raw QR string back via `onScanned`
/// and dismisses; the caller (`PairingView`) runs `PairingTicket.extract`
/// on it and auto-fills the ticket field.
struct QRScannerSheet: View {
    /// Called on the main thread with the decoded QR string.
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
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { dismiss() }
                }
            }
        }
    }

    /// A simple square framing guide so the user knows where to aim.
    private var reticle: some View {
        RoundedRectangle(cornerRadius: 16)
            .strokeBorder(Color.white.opacity(0.9), lineWidth: 3)
            .frame(width: 240, height: 240)
            .shadow(radius: 8)
            .accessibilityHidden(true)
    }

    /// Shown when camera access is denied/restricted — actionable guidance
    /// plus the always-available paste fallback (the user can Cancel back to
    /// the paste field).
    private var deniedView: some View {
        VStack(spacing: 16) {
            Image(systemName: "video.slash")
                .font(.system(size: 44))
                .foregroundStyle(.secondary)
            Text("Camera access is off")
                .font(.headline)
            Text("To scan the QR code, allow camera access for HoloIroh in Settings. You can also cancel and paste the ticket text instead.")
                .font(.footnote)
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
                .padding(.horizontal, 32)
        }
        .padding()
    }
}

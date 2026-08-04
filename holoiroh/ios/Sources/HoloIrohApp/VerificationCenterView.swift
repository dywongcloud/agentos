import SwiftUI

struct VerificationCenterView: View {
    let verification: TinfoilVerification?

    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationStack {
            Form {
                if let verification {
                    verifiedSection(verification)
                    fingerprintSection(verification.groundTruth)
                    measurementSection(verification.groundTruth)
                } else {
                    unavailableSection
                }

                Section("Vendor reference") {
                    Link(
                        "Open Tinfoil Verification Center",
                        destination: URL(string: "https://verification-center.tinfoil.sh")!
                    )
                    Text("The hosted page describes Tinfoil's enclave fleet. The proof above comes from this Mac daemon's attested, origin-bound client and is the proof relevant to Holoiroh traffic.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }
            .navigationTitle("Verification Center")
            #if os(iOS)
            .navigationBarTitleDisplayMode(.inline)
            #endif
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button("Done") { dismiss() }
                }
            }
        }
    }

    private func verifiedSection(_ verification: TinfoilVerification) -> some View {
        Section("Verified transport") {
            Label("Attestation verified", systemImage: "checkmark.shield.fill")
                .foregroundStyle(.green)
            LabeledContent("Enclave", value: verification.host)
            LabeledContent("Release digest") {
                proofText(verification.groundTruth.digest)
            }
            Text("The daemon verified code provenance, enclave measurements, and the TLS public key before enabling Tinfoil requests. Requests are restricted to this verified HTTPS origin.")
                .font(.caption)
                .foregroundStyle(.secondary)
        }
    }

    private func fingerprintSection(_ truth: TinfoilGroundTruth) -> some View {
        Section("Cryptographic bindings") {
            proofRow("Code fingerprint", truth.codeFingerprint)
            proofRow("Enclave fingerprint", truth.enclaveFingerprint)
            proofRow("TLS public key", truth.tlsPublicKey ?? "Unavailable")
            proofRow("HPKE public key", truth.hpkePublicKey ?? "Unavailable")
        }
    }

    private func measurementSection(_ truth: TinfoilGroundTruth) -> some View {
        Section("Measurements") {
            LabeledContent("Code type", value: truth.codeMeasurement.type)
            LabeledContent("Code registers", value: "\(truth.codeMeasurement.registers.count)")
            LabeledContent("Enclave type", value: truth.enclaveMeasurement.type)
            LabeledContent("Enclave registers", value: "\(truth.enclaveMeasurement.registers.count)")
        }
    }

    private var unavailableSection: some View {
        Section("Verified transport") {
            Label("No attestation received", systemImage: "exclamationmark.shield")
                .foregroundStyle(.orange)
            Text("This connection has not supplied Tinfoil verification ground truth. Tinfoil features may be disabled, the daemon may still be connecting, or attestation may have failed. Do not treat the hosted vendor page as proof for this connection.")
                .font(.caption)
                .foregroundStyle(.secondary)
        }
    }

    private func proofRow(_ label: String, _ value: String) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(label)
                .font(.caption)
                .foregroundStyle(.secondary)
            proofText(value)
        }
    }

    private func proofText(_ value: String) -> some View {
        Text(value)
            .font(.caption.monospaced())
            .textSelection(.enabled)
    }
}

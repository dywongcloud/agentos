import SwiftUI
import WebKit

/// Embeds Tinfoil's hosted Verification Center (`verification-center.tinfoil.sh`), which shows
/// the cryptographic proof (Sigstore code-integrity + enclave attestation + HPKE key match) that
/// Tinfoil-routed inference from this app runs inside a verified secure enclave.
///
/// ## Why a bare iframe embed, not the full `postMessage` handshake
///
/// docs.tinfoil.sh/guides/verification-center describes an integration where the HOST APP
/// fetches a "verification document" (via the `tinfoil` npm SDK's `getVerificationDocument()`)
/// and posts it INTO the iframe, scoping the shown proof to a specific request this app made.
/// Two things were resolved by direct inspection of the real `tinfoil` npm package (not
/// guessed) before building this:
///
/// 1. **No API key is needed anywhere in this flow.** `atc.js`'s `fetchAttestationBundle`
///    fetches `<ATC_BASE_URL>/attestation` with only a `Content-Type` header -- no
///    `Authorization`, no key. This matters because this codebase has a hard invariant (see
///    `tinfoil_proxy.rs`/every `tinfoil_*.rs` module doc) that the daemon's Tinfoil bearer key
///    never leaves the daemon process -- confirming the key is never needed client-side means
///    this view can be built at all without violating that invariant or needing a server-side
///    proxy through the daemon.
/// 2. **The actual cryptographic verification (Sigstore transparency-log checks, enclave
///    attestation signature verification) is NOT implemented in the JS glue this app could
///    inspect** -- it re-exports `@tinfoilsh/verifier`, a separate (likely Go-compiled-to-WASM)
///    package whose real verification logic was not reachable via the same source inspection.
///    Hand-replicating real attestation cryptography from documentation alone, without that
///    package's actual source, would risk a verification UI that LOOKS real but checks nothing
///    -- worse than not building it. Tinfoil's own hosted iframe already runs that real
///    `@tinfoilsh/verifier` logic itself (it's Tinfoil's own page, on Tinfoil's own domain), so
///    embedding it directly gets the genuine verification without re-implementing it.
///
/// What this means concretely: this view shows Tinfoil's own default verification UI (every
/// enclave Tinfoil operates), not scoped to a specific request this app made. Scoping to a
/// specific request via the `postMessage(TINFOIL_VERIFICATION_DOCUMENT)` handshake remains a
/// real, reachable enhancement -- tracked, not abandoned -- once `@tinfoilsh/verifier`'s actual
/// document shape can be inspected directly (its source, not just its re-export surface).
struct VerificationCenterView: View {
    @Environment(\.dismiss) private var dismiss
    @Environment(\.colorScheme) private var colorScheme

    var body: some View {
        NavigationStack {
            VerificationCenterWebView(darkMode: colorScheme == .dark)
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
}

/// `WKWebView` wrapper loading the Tinfoil-hosted verification-center iframe URL directly (as
/// a full page, not literally inside an `<iframe>`, since this app has no surrounding page of
/// its own to host one in -- the URL params `darkMode`/`showHeader` are the same ones the docs
/// describe for iframe embedding, and apply identically to a direct top-level load).
private struct VerificationCenterWebView: UIViewRepresentable {
    let darkMode: Bool

    func makeUIView(context: Context) -> WKWebView {
        let webView = WKWebView()
        load(into: webView)
        return webView
    }

    func updateUIView(_ webView: WKWebView, context: Context) {
        // Re-load on a live dark-mode change so the embedded page's own theme stays in sync
        // with the app's, rather than freezing at whatever theme was active on first present.
        load(into: webView)
    }

    private func load(into webView: WKWebView) {
        var components = URLComponents(string: "https://verification-center.tinfoil.sh")!
        components.queryItems = [
            URLQueryItem(name: "darkMode", value: darkMode ? "true" : "false"),
            URLQueryItem(name: "showHeader", value: "true"),
        ]
        guard let url = components.url else { return }
        // Avoid a redundant reload if already showing this exact URL (updateUIView fires on
        // every SwiftUI re-render, not only on a real dark-mode change).
        if webView.url != url {
            webView.load(URLRequest(url: url))
        }
    }
}

import SwiftUI
import PhotosUI
import UniformTypeIdentifiers

/// Selects the confidential-inference attachment flow.
enum TinfoilAttachMode {
    case document
    case image
}

/// Selects and sends a document or image for confidential inference.
/// Image requests can include a question.
/// The sheet displays the matching result or failure.
struct TinfoilAttachSheet: View {
    let mode: TinfoilAttachMode
    /// Sends the request to the daemon.
    let onSend: (ClientMessage) -> Void

    @Environment(\.dismiss) private var dismiss

    @State private var pickedPhoto: PhotosPickerItem?
    @State private var pickedImageData: Data?
    @State private var pickedDocumentURL: URL?
    @State private var pickedDocumentName: String?
    @State private var prompt: String = "What is in this image?"
    @State private var showFileImporter = false
    @State private var isSending = false

    /// Contains the daemon result for this `requestId`.
    /// The caller sets either the success or failure binding.
    @Binding var resultText: String?
    @Binding var resultError: String?
    let requestId: String

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    switch mode {
                    case .document:
                        documentPicker
                    case .image:
                        imagePicker
                    }
                }

                if mode == .image {
                    Section("Question") {
                        TextField("What is in this image?", text: $prompt, axis: .vertical)
                    }
                }

                if let resultText {
                    Section("Result") {
                        Text(resultText)
                            .textSelection(.enabled)
                        if mode == .image {
                            Button {
                                onSend(.requestSpeech(requestId: requestId, text: resultText, voice: "serena"))
                            } label: {
                                Label("Speak result", systemImage: "speaker.wave.2")
                            }
                        }
                    }
                }
                if let resultError {
                    Section("Error") {
                        Text(resultError)
                            .foregroundStyle(.red)
                    }
                }

                Section {
                    Text("This is sent to Tinfoil's confidential-computing cloud for processing. Images are redacted for detected PII on-device before upload. Inspect this connection's cryptographic proof in Diagnostics → Verification Center.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }
            .navigationTitle(mode == .document ? "Attach Document" : "Attach Image")
            #if os(iOS)
            .navigationBarTitleDisplayMode(.inline)
            #endif
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") {
                        isSending = false
                        dismiss()
                    }
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button(isSending ? "Sending…" : "Send") {
                        send()
                    }
                    .disabled(isSending || !canSend)
                }
            }
            .fileImporter(
                isPresented: $showFileImporter,
                allowedContentTypes: [.pdf, .plainText, .commaSeparatedText, .html, .data],
                allowsMultipleSelection: false
            ) { result in
                if case .success(let urls) = result, let url = urls.first {
                    pickedDocumentURL = url
                    pickedDocumentName = url.lastPathComponent
                }
            }
            .task(id: pickedPhoto) {
                guard let pickedPhoto else { return }
                pickedImageData = try? await pickedPhoto.loadTransferable(type: Data.self)
            }
            .onChange(of: resultText) { _, result in
                if result != nil { isSending = false }
            }
            .onChange(of: resultError) { _, error in
                if error != nil { isSending = false }
            }
            .onDisappear { isSending = false }
        }
    }

    private var documentPicker: some View {
        Group {
            Button {
                showFileImporter = true
            } label: {
                Label(pickedDocumentName ?? "Choose a file", systemImage: "doc")
            }
        }
    }

    private var imagePicker: some View {
        Group {
            PhotosPicker(selection: $pickedPhoto, matching: .images) {
                Label(pickedImageData == nil ? "Choose a photo" : "Photo selected", systemImage: "photo")
            }
            if let pickedImageData, let uiImage = PlatformImage(data: pickedImageData) {
                uiImage.swiftUIImage
                    .resizable()
                    .scaledToFit()
                    .frame(maxHeight: 160)
            }
        }
    }

    private var canSend: Bool {
        switch mode {
        case .document: return pickedDocumentURL != nil
        case .image: return pickedImageData != nil
        }
    }

    private func send() {
        isSending = true
        switch mode {
        case .document:
            guard let url = pickedDocumentURL else {
                isSending = false
                return
            }
            let didAccess = url.startAccessingSecurityScopedResource()
            defer { if didAccess { url.stopAccessingSecurityScopedResource() } }
            guard let data = try? Data(contentsOf: url) else {
                resultError = "Could not read the selected file."
                isSending = false
                return
            }
            onSend(.processDocument(
                requestId: requestId,
                filename: url.lastPathComponent,
                dataBase64: data.base64EncodedString(),
                mode: "text"
            ))
        case .image:
            guard let pickedImageData else {
                isSending = false
                return
            }
            onSend(.analyzeImage(
                requestId: requestId,
                imageDataBase64: pickedImageData.base64EncodedString(),
                prompt: prompt
            ))
        }
    }
}

/// Wraps platform image decoding for iOS and macOS package builds.
private struct PlatformImage {
    let swiftUIImage: Image

    init?(data: Data) {
        #if os(iOS)
        guard let uiImage = UIImage(data: data) else { return nil }
        self.swiftUIImage = Image(uiImage: uiImage)
        #else
        guard let nsImage = NSImage(data: data) else { return nil }
        self.swiftUIImage = Image(nsImage: nsImage)
        #endif
    }
}

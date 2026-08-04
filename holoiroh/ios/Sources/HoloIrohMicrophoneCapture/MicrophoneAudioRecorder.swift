import AVFoundation
import Combine
import Foundation

/// A WAV recording that can only come from this module's microphone input tap.
///
/// The type has no public initializer. The app can forward a capture to the
/// control channel, but it cannot wrap arbitrary bytes, system audio, or
/// speaker output as microphone audio.
public struct CapturedMicrophoneAudio: Equatable, Sendable {
    private let wavData: Data

    public var audioDataBase64: String {
        wavData.base64EncodedString()
    }

    public let format = "wav"

    fileprivate init(wavData: Data) {
        self.wavData = wavData
    }
}

private final class InputTapWriter: @unchecked Sendable {
    private let lock = NSLock()
    private var file: AVAudioFile?
    private var failure: Error?

    init(file: AVAudioFile) {
        self.file = file
    }

    func write(_ buffer: AVAudioPCMBuffer) {
        lock.lock()
        defer { lock.unlock() }
        guard failure == nil else { return }
        do {
            try file?.write(from: buffer)
        } catch {
            failure = error
        }
    }

    func close() -> Error? {
        lock.lock()
        defer { lock.unlock() }
        file = nil
        return failure
    }
}

#if DEBUG
private final class SendablePCMBuffer: @unchecked Sendable {
    let value: AVAudioPCMBuffer

    init(_ value: AVAudioPCMBuffer) {
        self.value = value
    }
}
#endif

/// Records only the device microphone through `AVAudioEngine.inputNode`.
///
/// This target intentionally has no API for display, system, call, or speaker
/// audio. The opaque result is the only audio value accepted by the app's
/// Tinfoil transcription message.
@MainActor
public final class MicrophoneAudioRecorder: NSObject, ObservableObject {
    public static let maximumRecordingDuration: TimeInterval = 120
    static let maximumWavBytes = 32 * 1024 * 1024

    @Published public private(set) var isRecording = false
    @Published public private(set) var lastError: String?

    private let audioEngine = AVAudioEngine()
    private var outputURL: URL?
    private var tapWriter: InputTapWriter?
    private var durationTask: Task<Void, Never>?

    public override init() {
        super.init()
    }

    public func start() async {
        guard !isRecording else { return }
        lastError = nil

        guard await Self.requestPermission() else {
            lastError = "Microphone access was not authorized."
            return
        }
        guard !Task.isCancelled else { return }

        #if os(iOS)
        do {
            let session = AVAudioSession.sharedInstance()
            try session.setCategory(.record, mode: .measurement, options: .duckOthers)
            try session.setActive(true, options: .notifyOthersOnDeactivation)
        } catch {
            lastError = "Audio session setup failed: \(error.localizedDescription)"
            return
        }
        #endif

        let input = audioEngine.inputNode
        let format = input.outputFormat(forBus: 0)
        guard format.sampleRate > 0, format.channelCount > 0 else {
            lastError = "The microphone input format is unavailable."
            deactivateAudioSession()
            return
        }
        let url = FileManager.default.temporaryDirectory
            .appendingPathComponent("holoiroh-microphone-\(UUID().uuidString).wav")

        do {
            let file = try AVAudioFile(forWriting: url, settings: format.settings)
            let writer = InputTapWriter(file: file)
            input.removeTap(onBus: 0)
            input.installTap(onBus: 0, bufferSize: 1024, format: format) { buffer, _ in
                writer.write(buffer)
            }
            audioEngine.prepare()
            try audioEngine.start()
            outputURL = url
            tapWriter = writer
            isRecording = true
            scheduleDurationLimit()
        } catch {
            input.removeTap(onBus: 0)
            try? FileManager.default.removeItem(at: url)
            lastError = "Failed to start recording: \(error.localizedDescription)"
            deactivateAudioSession()
        }
    }

    public func stopAndCapture() -> CapturedMicrophoneAudio? {
        guard isRecording, let url = outputURL, let writer = tapWriter else {
            return nil
        }

        durationTask?.cancel()
        durationTask = nil
        audioEngine.stop()
        audioEngine.inputNode.removeTap(onBus: 0)
        isRecording = false
        outputURL = nil
        tapWriter = nil
        deactivateAudioSession()

        defer { try? FileManager.default.removeItem(at: url) }
        if let error = writer.close() {
            lastError = "Recording encode failed: \(error.localizedDescription)"
            return nil
        }

        do {
            let size = try url.resourceValues(forKeys: [.fileSizeKey]).fileSize ?? 0
            guard size <= Self.maximumWavBytes else {
                lastError = "The microphone recording exceeded the 32 MiB limit."
                return nil
            }
            return try Self.captureFromInputTapWav(Data(contentsOf: url))
        } catch {
            lastError = error.localizedDescription
            return nil
        }
    }

    public func discard() {
        guard isRecording else { return }
        durationTask?.cancel()
        durationTask = nil
        let url = outputURL
        audioEngine.stop()
        audioEngine.inputNode.removeTap(onBus: 0)
        _ = tapWriter?.close()
        isRecording = false
        outputURL = nil
        tapWriter = nil
        deactivateAudioSession()
        if let url {
            try? FileManager.default.removeItem(at: url)
        }
    }

    private func scheduleDurationLimit(
        nanoseconds: UInt64? = nil,
        onExpire: (() -> Void)? = nil
    ) {
        let waitNanoseconds = nanoseconds
            ?? UInt64(Self.maximumRecordingDuration * 1_000_000_000)
        durationTask?.cancel()
        durationTask = Task { [weak self] in
            try? await Task.sleep(nanoseconds: waitNanoseconds)
            guard !Task.isCancelled else { return }
            if let onExpire {
                onExpire()
            } else {
                self?.expireRecording()
            }
        }
    }

    private func expireRecording() {
        guard isRecording else { return }
        discard()
        lastError = "The microphone recording stopped at the 120-second limit."
    }

    private static func requestPermission() async -> Bool {
        #if os(iOS)
        if #available(iOS 17.0, *) {
            return await AVAudioApplication.requestRecordPermission()
        }
        return await withCheckedContinuation { continuation in
            AVAudioSession.sharedInstance().requestRecordPermission { granted in
                continuation.resume(returning: granted)
            }
        }
        #else
        return true
        #endif
    }

    private static func captureFromInputTapWav(_ data: Data) throws -> CapturedMicrophoneAudio {
        guard data.count <= maximumWavBytes else {
            throw CocoaError(.fileReadTooLarge)
        }
        guard data.count >= 12,
              data.prefix(4) == Data("RIFF".utf8),
              data.subdata(in: 8..<12) == Data("WAVE".utf8) else {
            throw CocoaError(.fileReadCorruptFile)
        }
        return CapturedMicrophoneAudio(wavData: data)
    }

    private func deactivateAudioSession() {
        #if os(iOS)
        try? AVAudioSession.sharedInstance().setActive(
            false,
            options: .notifyOthersOnDeactivation
        )
        #endif
    }

    #if DEBUG
    static func exerciseDurationTaskForProbe() async throws {
        let recorder = MicrophoneAudioRecorder()
        var expirations = 0

        recorder.scheduleDurationLimit(nanoseconds: 5_000_000) {
            expirations += 1
        }
        recorder.scheduleDurationLimit(nanoseconds: 20_000_000) {
            expirations += 100
        }
        try await Task.sleep(nanoseconds: 2_000_000)
        guard expirations == 0 else {
            throw NSError(domain: "HoloIrohMicrophoneCapture.Probe", code: 1)
        }

        recorder.durationTask?.cancel()
        try await Task.sleep(nanoseconds: 25_000_000)
        guard expirations == 0 else {
            throw NSError(domain: "HoloIrohMicrophoneCapture.Probe", code: 1)
        }

        recorder.scheduleDurationLimit(nanoseconds: 1_000_000) {
            expirations += 1
        }
        try await Task.sleep(nanoseconds: 5_000_000)
        guard expirations == 1 else {
            throw NSError(domain: "HoloIrohMicrophoneCapture.Probe", code: 1)
        }
        recorder.durationTask?.cancel()
    }

    /// Races real PCM writes against close to witness the input callback's lock.
    static func exerciseConcurrentTapWriterForProbe() throws {
        guard let format = AVAudioFormat(
            commonFormat: .pcmFormatFloat32,
            sampleRate: 16_000,
            channels: 1,
            interleaved: false
        ), let buffer = AVAudioPCMBuffer(pcmFormat: format, frameCapacity: 64) else {
            throw CocoaError(.fileWriteUnknown)
        }
        buffer.frameLength = 64
        let sendableBuffer = SendablePCMBuffer(buffer)

        for _ in 0..<64 {
            let url = FileManager.default.temporaryDirectory
                .appendingPathComponent("holoiroh-writer-probe-\(UUID().uuidString).wav")
            defer { try? FileManager.default.removeItem(at: url) }
            let file = try AVAudioFile(forWriting: url, settings: format.settings)
            let writer = InputTapWriter(file: file)
            let group = DispatchGroup()
            let queue = DispatchQueue.global(qos: .userInitiated)

            for _ in 0..<16 {
                group.enter()
                queue.async {
                    writer.write(sendableBuffer.value)
                    group.leave()
                }
            }
            group.enter()
            queue.async {
                _ = writer.close()
                group.leave()
            }
            group.wait()

            if let error = writer.close() {
                throw error
            }
            guard try Data(contentsOf: url).count >= 12 else {
                throw CocoaError(.fileWriteUnknown)
            }
        }
    }

    /// Drives the post-input-tap validation path in executable probes.
    static func captureFromInputTapForProbe(_ data: Data) throws -> CapturedMicrophoneAudio {
        try captureFromInputTapWav(data)
    }
    #endif
}

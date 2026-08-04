import Foundation
import Speech
import AVFoundation

/// Represents one partial or final speech-recognition result.
/// `isFinal` matches `SFSpeechRecognitionResult.isFinal`.
struct VoiceTranscript: Equatable {
    let text: String
    let isFinal: Bool
}

/// Defines startup failures from `VoiceTranscriber.start()`.
enum VoiceTranscriberError: Error, LocalizedError {
    /// Indicates missing, denied, or restricted speech-recognition or microphone authorization.
    case notAuthorized
    /// Indicates that the requested speech recognizer is unavailable.
    /// This includes unsupported locales, unavailable network recognition, and missing on-device models.
    case recognizerUnavailable
    /// Indicates that `AVAudioEngine` could not start. The associated error contains the cause.
    case audioEngineFailed(Error)
    /// Indicates that `start()` was called while a session was already running.
    case alreadyRunning

    var errorDescription: String? {
        switch self {
        case .notAuthorized:
            return "Speech recognition or microphone access was not authorized."
        case .recognizerUnavailable:
            return "The speech recognizer is unavailable for the current locale/network state."
        case .audioEngineFailed(let underlying):
            return "Audio engine failed to start: \(underlying.localizedDescription)"
        case .alreadyRunning:
            return "VoiceTranscriber is already running."
        }
    }
}

/// Captures microphone audio and streams transcription through Apple Speech.
///
/// - `AVAudioEngine` sends Pulse-Code Modulation (PCM) buffers to the recognition request.
/// - The recognizer emits partial results and one final result.
/// - Supported configurations use on-device recognition.
/// - Other configurations use the framework default recognition path.
/// - `updates` and `onUpdate` receive the same results.
///
/// Drive `start()` and `stop()` from one thread.
final class VoiceTranscriber: NSObject {

    /// Receives each partial and final transcript update. Assign before `start()`.
    var onUpdate: ((VoiceTranscript) -> Void)?

    /// Receives asynchronous recognition failures after startup.
    /// Synchronous startup failures are thrown from `start()`.
    var onError: ((Error) -> Void)?

    /// Indicates whether a recognition session is active.
    private(set) var isRunning = false

    private let audioEngine = AVAudioEngine()
    private let speechRecognizer: SFSpeechRecognizer?

    private var recognitionRequest: SFSpeechAudioBufferRecognitionRequest?
    private var recognitionTask: SFSpeechRecognitionTask?

    private var updatesContinuation: AsyncStream<VoiceTranscript>.Continuation?

    /// Provides partial and final transcript updates.
    /// Each transcriber creates one stream.
    /// Use one concurrent consumer for that stream.
    let updates: AsyncStream<VoiceTranscript>

    /// - Parameter locale: The speech-recognition locale. The default is the current user locale.
    ///
    /// If recognizer creation fails, each `start()` call throws `.recognizerUnavailable`.
    init(locale: Locale = Locale.current) {
        self.speechRecognizer = SFSpeechRecognizer(locale: locale)

        var continuation: AsyncStream<VoiceTranscript>.Continuation!
        self.updates = AsyncStream { cont in
            continuation = cont
        }
        self.updatesContinuation = continuation

        super.init()
        self.speechRecognizer?.delegate = self
    }

    deinit {
        updatesContinuation?.finish()
    }

    // MARK: - Public API

    /// Requests required authorization and starts live transcription.
    ///
    /// The method throws when authorization fails, the recognizer is unavailable, or the audio engine cannot start.
    /// After startup, `onError` receives asynchronous failures and the session stops.
    func start() async throws {
        guard !isRunning else { throw VoiceTranscriberError.alreadyRunning }

        guard let speechRecognizer, speechRecognizer.isAvailable else {
            throw VoiceTranscriberError.recognizerUnavailable
        }

        let authorized = await Self.requestAuthorization()
        guard authorized else {
            throw VoiceTranscriberError.notAuthorized
        }

        // Tear down any stale request/task state from a prior run before
        // wiring up fresh ones.
        cleanupRecognition()

        let request = SFSpeechAudioBufferRecognitionRequest()
        request.shouldReportPartialResults = true

        // Prefer on-device recognition for privacy (audio never leaves the
        // device) and latency (no network round-trip), but only where the
        // recognizer actually supports it -- setting this unconditionally
        // on an unsupported locale/device silently degrades to no
        // recognition on some OS versions rather than falling back.
        if speechRecognizer.supportsOnDeviceRecognition {
            request.requiresOnDeviceRecognition = true
        }

        self.recognitionRequest = request

        #if os(iOS)
        let audioSession = AVAudioSession.sharedInstance()
        do {
            try audioSession.setCategory(.record, mode: .measurement, options: .duckOthers)
            try audioSession.setActive(true, options: .notifyOthersOnDeactivation)
        } catch {
            cleanupRecognition()
            throw VoiceTranscriberError.audioEngineFailed(error)
        }
        #endif

        let inputNode = audioEngine.inputNode
        let recordingFormat = inputNode.outputFormat(forBus: 0)
        inputNode.removeTap(onBus: 0)
        inputNode.installTap(onBus: 0, bufferSize: 1024, format: recordingFormat) { [weak self] buffer, _ in
            self?.recognitionRequest?.append(buffer)
        }

        audioEngine.prepare()
        do {
            try audioEngine.start()
        } catch {
            inputNode.removeTap(onBus: 0)
            cleanupRecognition()
            throw VoiceTranscriberError.audioEngineFailed(error)
        }

        isRunning = true

        recognitionTask = speechRecognizer.recognitionTask(with: request) { [weak self] result, error in
            guard let self else { return }

            if let result {
                let update = VoiceTranscript(
                    text: result.bestTranscription.formattedString,
                    isFinal: result.isFinal
                )
                self.onUpdate?(update)
                self.updatesContinuation?.yield(update)
            }

            if let error {
                self.onError?(error)
                self.stop()
                return
            }

            if result?.isFinal == true {
                self.stop()
            }
        }
    }

    /// Stops recognition and removes the audio tap.
    /// Resets state for a later `start()` call.
    /// Repeated calls are safe.
    func stop() {
        guard isRunning else { return }
        isRunning = false

        if audioEngine.isRunning {
            audioEngine.stop()
        }
        audioEngine.inputNode.removeTap(onBus: 0)

        recognitionRequest?.endAudio()
        cleanupRecognition()

        #if os(iOS)
        try? AVAudioSession.sharedInstance().setActive(false, options: .notifyOthersOnDeactivation)
        #endif
    }

    // MARK: - Private helpers

    private func cleanupRecognition() {
        recognitionTask?.cancel()
        recognitionTask = nil
        recognitionRequest = nil
    }

    /// Requests speech-recognition and microphone authorization. Returns `true` only when both requests succeed.
    private static func requestAuthorization() async -> Bool {
        let speechStatus = await withCheckedContinuation { (continuation: CheckedContinuation<SFSpeechRecognizerAuthorizationStatus, Never>) in
            SFSpeechRecognizer.requestAuthorization { status in
                continuation.resume(returning: status)
            }
        }
        guard speechStatus == .authorized else { return false }

        #if os(iOS)
        if #available(iOS 17.0, *) {
            let micStatus = await AVAudioApplication.requestRecordPermission()
            return micStatus
        } else {
            let micGranted = await withCheckedContinuation { (continuation: CheckedContinuation<Bool, Never>) in
                AVAudioSession.sharedInstance().requestRecordPermission { granted in
                    continuation.resume(returning: granted)
                }
            }
            return micGranted
        }
        #else
        return true
        #endif
    }
}

// MARK: - SFSpeechRecognizerDelegate

extension VoiceTranscriber: SFSpeechRecognizerDelegate {
    func speechRecognizer(_ speechRecognizer: SFSpeechRecognizer, availabilityDidChange available: Bool) {
        if !available {
            onError?(VoiceTranscriberError.recognizerUnavailable)
            stop()
        }
    }
}

// MARK: - ObservableObject wrapper for SwiftUI

/// Adapts `VoiceTranscriber` for SwiftUI.
/// Publishes transcript text, recording state, and errors.
@MainActor
final class VoiceTranscriberModel: ObservableObject {
    @Published private(set) var liveText: String = ""
    @Published private(set) var isRecording: Bool = false
    @Published var lastError: String?

    private let transcriber: VoiceTranscriber

    init(locale: Locale = Locale.current) {
        self.transcriber = VoiceTranscriber(locale: locale)
        self.transcriber.onUpdate = { [weak self] update in
            guard let self else { return }
            Task { @MainActor in
                self.liveText = update.text
            }
        }
        self.transcriber.onError = { [weak self] error in
            guard let self else { return }
            Task { @MainActor in
                self.lastError = error.localizedDescription
                self.isRecording = false
            }
        }
    }

    /// Starts a recognition session and clears the previous transcript.
    func start() async {
        guard !isRecording else { return }
        lastError = nil
        liveText = ""
        do {
            try await transcriber.start()
            isRecording = true
        } catch {
            lastError = error.localizedDescription
            isRecording = false
        }
    }

    /// Stops recognition and retains the latest transcript.
    func stop() {
        transcriber.stop()
        isRecording = false
    }

    /// Starts or stops recognition for a single microphone control.
    func toggle() async {
        if isRecording {
            stop()
        } else {
            await start()
        }
    }
}

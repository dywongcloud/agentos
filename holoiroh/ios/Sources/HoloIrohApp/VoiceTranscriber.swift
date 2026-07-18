import Foundation
import Speech
import AVFoundation

/// One transcript update emitted while a `VoiceTranscriber` session is running.
///
/// `isFinal` mirrors `SFSpeechRecognitionResult.isFinal`: `false` for every
/// intermediate (partial) hypothesis as the user keeps speaking, `true` for
/// the single terminal result that closes out the recognition task.
struct VoiceTranscript: Equatable {
    let text: String
    let isFinal: Bool
}

/// Errors surfaced by `VoiceTranscriber.start()`.
enum VoiceTranscriberError: Error, LocalizedError {
    /// The user has not granted (or has explicitly denied/restricted) speech
    /// recognition and/or microphone authorization.
    case notAuthorized
    /// `SFSpeechRecognizer` could not be constructed for the requested
    /// locale (the failable `SFSpeechRecognizer(locale:)` initializer
    /// returned `nil`), or the resulting recognizer reports `isAvailable ==
    /// false` (e.g. no network for a server-side recognizer, or the
    /// on-device model for this locale isn't installed).
    case recognizerUnavailable
    /// `AVAudioEngine` failed to start (e.g. audio session configuration
    /// rejected, hardware busy).
    case audioEngineFailed(Error)
    /// Attempted to `start()` a session that is already running.
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

/// Captures microphone audio and streams live speech-to-text transcription
/// using Apple's Speech framework.
///
/// Design:
/// - `AVAudioEngine` taps the device microphone and feeds PCM buffers into an
///   `SFSpeechAudioBufferRecognitionRequest`.
/// - `SFSpeechRecognizer` drives an `SFSpeechRecognitionTask` against that
///   request, delivering partial results as the user speaks and one final
///   result when recognition completes.
/// - `requiresOnDeviceRecognition` is set to `true` whenever the recognizer
///   reports `supportsOnDeviceRecognition == true`, for privacy (audio never
///   leaves the device) and latency (no network round-trip). When on-device
///   recognition isn't supported for the active locale/device, the request
///   falls back to the framework's default (server-assisted) recognition
///   rather than failing outright.
/// - Callers observe results either via the `updates` `AsyncStream` or the
///   `onUpdate` closure -- both fire for every partial and the terminal
///   final result. `onUpdate` is convenient for simple call sites (e.g.
///   driving a SwiftUI `@State` from a synchronous callback); `updates` suits
///   callers already inside a `Task`/`async` context.
///
/// Not thread-safe for concurrent `start()`/`stop()` calls from multiple
/// threads; intended to be driven from the main actor (as SwiftUI view code
/// naturally does).
final class VoiceTranscriber: NSObject {

    /// Fires for every partial and final transcript update. Assign before
    /// calling `start()` if you want callback-style delivery instead of (or
    /// alongside) consuming `updates`.
    var onUpdate: ((VoiceTranscript) -> Void)?

    /// Fires once if `start()` produces an error asynchronously after
    /// having already begun (e.g. the recognition task itself errors mid-
    /// session). Synchronous startup failures are thrown directly from
    /// `start()` instead.
    var onError: ((Error) -> Void)?

    /// `true` while a recognition session is active.
    private(set) var isRunning = false

    private let audioEngine = AVAudioEngine()
    private let speechRecognizer: SFSpeechRecognizer?

    private var recognitionRequest: SFSpeechAudioBufferRecognitionRequest?
    private var recognitionTask: SFSpeechRecognitionTask?

    private var updatesContinuation: AsyncStream<VoiceTranscript>.Continuation?

    /// Live stream of partial/final transcript updates. A fresh
    /// `AsyncStream` (and continuation) is created per `VoiceTranscriber`
    /// instance; multiple concurrent consumers of the same stream are not
    /// supported (standard `AsyncStream` single-consumer semantics).
    let updates: AsyncStream<VoiceTranscript>

    /// - Parameter locale: Locale to recognize speech in. Defaults to the
    ///   user's current locale. `SFSpeechRecognizer(locale:)` is failable;
    ///   if construction fails for the given locale, `speechRecognizer` is
    ///   `nil` and every `start()` call throws `.recognizerUnavailable`.
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

    /// Requests (if needed) speech-recognition + microphone authorization,
    /// then starts capturing microphone audio and streaming live
    /// transcription results to `updates`/`onUpdate`.
    ///
    /// Throws synchronously if authorization is denied, the recognizer is
    /// unavailable, or the audio engine fails to start. Once running,
    /// asynchronous failures (e.g. the recognition task itself erroring)
    /// are reported via `onError` and also stop the session (mirroring
    /// `stop()`).
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

    /// Stops the current recognition session, tears down the audio tap, and
    /// resets internal state so `start()` can be called again. Safe to call
    /// multiple times and safe to call when not running.
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

    /// Requests both speech-recognition and microphone authorization,
    /// resolving `true` only if both are granted.
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

/// Thin `ObservableObject` adapter around `VoiceTranscriber` for SwiftUI call
/// sites: publishes the live transcript text and recording/error state as
/// `@Published` properties so a view can bind to them directly, while the
/// underlying `VoiceTranscriber` still exposes its `AsyncStream`/closure API
/// for non-SwiftUI consumers.
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

    /// Starts a fresh recognition session, clearing any previous live text.
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

    /// Stops the current recognition session; `liveText` retains the last
    /// transcript received (final, if the session completed naturally).
    func stop() {
        transcriber.stop()
        isRecording = false
    }

    /// Toggles between `start()` and `stop()` -- convenient for a single mic
    /// button's tap handler.
    func toggle() async {
        if isRecording {
            stop()
        } else {
            await start()
        }
    }
}

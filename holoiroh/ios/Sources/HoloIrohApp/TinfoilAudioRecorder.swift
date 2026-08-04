import HoloIrohMicrophoneCapture

/// Provides the microphone module's opt-in recorder under the app's compatibility name.
/// The module does not expose initializers for audio from arbitrary bytes.
/// It also excludes system audio and speaker output.
typealias TinfoilAudioRecorder = MicrophoneAudioRecorder

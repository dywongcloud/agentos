# Required `Info.plist` keys for the app target

`ios/Package.swift` defines the `HoloIrohApp` Swift Package Manager library.
A library package cannot contain an `Info.plist` or produce an installable `.app` bundle.

`ios/App` provides the Xcode app target that wraps this package.
`ios/App/project.yml` generates `ios/App/Info.plist` and configures the required usage descriptions.
Regenerate the Xcode project after changing these values.

## Permission use

`VoiceTranscriber.swift` requests speech-recognition and microphone authorization at runtime.
It calls `SFSpeechRecognizer.requestAuthorization`.
It also calls `AVAudioApplication.requestRecordPermission` or `AVAudioSession.requestRecordPermission`.

`HoloIrohMicrophoneCapture` installs a separate `AVAudioEngine.inputNode` tap.
It installs this tap only when the user enables optional Tinfoil transcription.
Its opaque `CapturedMicrophoneAudio` value has no public initializer.
The app cannot construct `TranscribeAudio` from arbitrary bytes.
`HoloIrohMicrophoneCapture` requests no digital system-audio or speaker-output mix.
The microphone can still capture nearby speakers acoustically.
`VoiceTranscriber` requests on-device recognition when the platform supports it.
Otherwise, Speech can use its default recognition path.

`QRScannerView.swift` requests camera authorization with `AVCaptureDevice.requestAccess(for: .video)`.
`PairingView` reaches this request through its **Scan QR code** button.
The app presents a live `QRScannerView` capture session after authorization.

## Required keys

`ios/App/project.yml` configures these keys in the app target:

| Key | Needed by | Configured value |
|---|---|---|
| `NSCameraUsageDescription` | `QRScannerView` for QR pairing | "Aro uses the camera to scan the pairing QR code your Mac displays." |
| `NSMicrophoneUsageDescription` | `VoiceTranscriber` and optional Tinfoil transcription | "Aro uses the microphone to let you speak prompts instead of typing them, and (only if you enable Tinfoil audio transcription) to send your own voice to Tinfoil's confidential-computing cloud." |
| `NSSpeechRecognitionUsageDescription` | `VoiceTranscriber` | "Aro transcribes your speech on-device (when supported) to turn it into a text prompt." |

Keep each key in the generated app's `Info.plist` before using its related feature.
If a key is missing, iOS terminates the app when code requests that permission.
The app cannot replace a missing usage description at runtime.

These calls require the keys:

- `AVCaptureDevice.requestAccess` requires `NSCameraUsageDescription`.
- The microphone-permission API requires `NSMicrophoneUsageDescription`.
- `SFSpeechRecognizer.requestAuthorization` requires `NSSpeechRecognitionUsageDescription`.

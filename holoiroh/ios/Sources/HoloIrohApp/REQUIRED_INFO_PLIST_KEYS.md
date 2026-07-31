# Required Info.plist keys for the eventual Xcode app target

`ios/` is a bare Swift Package Manager **library** package (see
`Package.swift` -- `products: [.library(...)]`). It cannot itself carry an
`Info.plist` or produce an installable `.app` bundle. Per `README.md`'s
"Setup" section, a thin Xcode app target that depends on this package must
wrap `ios/`.

`VoiceTranscriber.swift` requests Speech-recognition and microphone
authorization at runtime (`SFSpeechRecognizer.requestAuthorization`,
`AVAudioApplication.requestRecordPermission` / `AVAudioSession
.requestRecordPermission`). `QRScannerView.swift` requests **camera**
authorization at runtime (`AVCaptureDevice.requestAccess(for: .video)`,
reached from `PairingView`'s "Scan QR" button). If the corresponding
usage-description key is missing from the *app's* `Info.plist`, iOS
hard-crashes the process the moment any of these authorization prompts
triggers. Library code cannot work around this crash. This SPM package has
nowhere to put the key today.

README's "Setup (once implemented)" section describes how to create the
wrapping Xcode app target. Whoever creates that target must add all of the
following keys to its `Info.plist`. Add the keys before the corresponding
feature can run on a real device or simulator:

| Key                                    | Needed by | Suggested value                                                    |
|-----------------------------------------|-----------|---------------------------------------------------------------------|
| `NSCameraUsageDescription`              | `QRScannerView` (Scan QR pairing) | "HoloIroh uses the camera to scan the pairing QR code your Mac displays." |
| `NSMicrophoneUsageDescription`          | `VoiceTranscriber` (mic button)   | "HoloIroh uses the microphone to let you speak prompts instead of typing them." |
| `NSSpeechRecognitionUsageDescription`   | `VoiceTranscriber` (mic button)   | "HoloIroh transcribes your speech on-device (when supported) to turn it into a text prompt." |

Without these keys, tapping the corresponding control will not show a
permission dialog. Instead, the process hard-crashes immediately when the
code calls into the authorization API:

- `AVCaptureDevice.requestAccess` for the camera
- `SFSpeechRecognizer.requestAuthorization` or the mic-permission API for voice

The camera key is the newest addition. A user needs it as soon as they tap
**Scan QR** in `PairingView`, since that action presents the live
`QRScannerView` capture session.

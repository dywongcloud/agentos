# Required Info.plist keys for the eventual Xcode app target

`ios/` is a bare Swift Package Manager **library** package (see
`Package.swift` -- `products: [.library(...)]`), which cannot itself carry
an `Info.plist` or produce an installable `.app` bundle (per `README.md`'s
"Setup" section: it must be wrapped in a thin Xcode app target that depends
on this package).

`VoiceTranscriber.swift` requests Speech-recognition and microphone
authorization at runtime (`SFSpeechRecognizer.requestAuthorization`,
`AVAudioApplication.requestRecordPermission` / `AVAudioSession
.requestRecordPermission`). `QRScannerView.swift` requests **camera**
authorization at runtime (`AVCaptureDevice.requestAccess(for: .video)`,
reached from `PairingView`'s "Scan QR" button). iOS hard-crashes the
process the moment any of these authorization prompts is triggered if the
corresponding usage-description key is missing from the *app's*
`Info.plist` -- this cannot be worked around in library code, and there is
nowhere in this SPM package to put it today.

Whoever creates the wrapping Xcode app target (see README's "Setup (once
implemented)") **must** add all of the following keys to that target's
`Info.plist` before the corresponding feature can be exercised on a real
device or simulator:

| Key                                    | Needed by | Suggested value                                                    |
|-----------------------------------------|-----------|---------------------------------------------------------------------|
| `NSCameraUsageDescription`              | `QRScannerView` (Scan QR pairing) | "HoloIroh uses the camera to scan the pairing QR code your Mac displays." |
| `NSMicrophoneUsageDescription`          | `VoiceTranscriber` (mic button)   | "HoloIroh uses the microphone to let you speak prompts instead of typing them." |
| `NSSpeechRecognitionUsageDescription`   | `VoiceTranscriber` (mic button)   | "HoloIroh transcribes your speech on-device (when supported) to turn it into a text prompt." |

Without these, tapping the corresponding control will not show a permission
dialog -- the app will terminate immediately when the code calls into the
authorization API (`AVCaptureDevice.requestAccess` for the camera,
`SFSpeechRecognizer.requestAuthorization` / the mic-permission API for
voice). The camera key is the newest addition: it is required as soon as a
user taps **Scan QR** in `PairingView`, since that presents the live
`QRScannerView` capture session.

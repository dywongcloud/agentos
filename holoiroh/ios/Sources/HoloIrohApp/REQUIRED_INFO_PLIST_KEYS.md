# Required Info.plist keys for the eventual Xcode app target

`ios/` is a bare Swift Package Manager **library** package (see
`Package.swift` -- `products: [.library(...)]`), which cannot itself carry
an `Info.plist` or produce an installable `.app` bundle (per `README.md`'s
"Setup" section: it must be wrapped in a thin Xcode app target that depends
on this package).

`VoiceTranscriber.swift` requests Speech-recognition and microphone
authorization at runtime (`SFSpeechRecognizer.requestAuthorization`,
`AVAudioApplication.requestRecordPermission` / `AVAudioSession
.requestRecordPermission`). iOS hard-crashes the process the moment either
authorization prompt is triggered if the corresponding usage-description
key is missing from the *app's* `Info.plist` -- this cannot be worked around
in library code, and there is nowhere in this SPM package to put it today.

Whoever creates the wrapping Xcode app target (see README's "Setup (once
implemented)") **must** add both of the following keys to that target's
`Info.plist` before the mic button can be exercised on a real device or
simulator:

| Key                                    | Suggested value                                                    |
|-----------------------------------------|---------------------------------------------------------------------|
| `NSMicrophoneUsageDescription`          | "HoloIroh uses the microphone to let you speak prompts instead of typing them." |
| `NSSpeechRecognitionUsageDescription`   | "HoloIroh transcribes your speech on-device (when supported) to turn it into a text prompt." |

Without these, tapping the mic button will not show a permission dialog --
the app will terminate immediately when `start()` calls into
`SFSpeechRecognizer.requestAuthorization`/the mic-permission API.

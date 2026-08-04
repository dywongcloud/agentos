# iPad Simulator remote-input witness

This witness uses public XCTest UI automation APIs.
It does not add code to the app target.
It does not use private input APIs.

## Safety

The remote-input test can click and type on the Mac.
Run the daemon in dry-run mode.
Dry-run mode receives input but does not post macOS Core Graphics events.

Do not use a production ticket.
Do not expose a credential field on the Mac.
Do not save an `.xcresult` bundle that contains sensitive screen content.

## Prerequisites

- Use an arm64 Mac.
- Install XcodeGen.
- Start the daemon with `HOLOIROH_INPUT_DRY_RUN=1`.
- Keep the current ticket and PIN in shell variables.
- Use an available iPad Simulator with iOS 17 or later.

## Generate and build

Run these commands from `ios/App`:

```sh
xcodegen generate

xcodebuild \
  -project HoloIroh.xcodeproj \
  -scheme HoloIroh \
  -destination 'platform=iOS Simulator,name=iPad Pro 11-inch (M5),OS=26.4' \
  -derivedDataPath /tmp/HoloIrohWitnessDerivedData \
  ARCHS=arm64 ONLY_ACTIVE_ARCH=YES CODE_SIGNING_ALLOWED=NO \
  build-for-testing
```

Change the destination name if that Simulator is unavailable.
Use `xcrun simctl list devices available` to list valid names.

## Run

The gesture test needs a live daemon connection.
It releases remote control before local pan and zoom gestures.
Use the ticket and PIN variables below for a deterministic DEBUG auto-pair.
Set `HOLOIROH_WITNESS_USE_SAVED_PROFILE=1` instead to use the app's saved auto-connect profile.
Do not set the auto-pair ticket or PIN when you use the saved profile.

The remote-input test also needs the dry-run confirmation variable.
The confirmation prevents accidental input on the Mac.

```sh
umask 077
printf '%s\n' \
  "HOLOIROH_WITNESS_TICKET = $TICKET" \
  "HOLOIROH_WITNESS_PIN = $PIN" \
  'HOLOIROH_WITNESS_INPUT_DRY_RUN_CONFIRMED = 1' \
  > /tmp/HoloIrohWitness.xcconfig

xcodebuild \
  -project HoloIroh.xcodeproj \
  -scheme HoloIroh \
  -destination 'platform=iOS Simulator,name=iPad Pro 11-inch (M5),OS=26.4' \
  -derivedDataPath /tmp/HoloIrohWitnessDerivedData \
  -xcconfig /tmp/HoloIrohWitness.xcconfig \
  ARCHS=arm64 ONLY_ACTIVE_ARCH=YES CODE_SIGNING_ALLOWED=NO \
  test \
  -only-testing:HoloIrohUITests/HoloIrohRemoteInputWitnessTests

rm -f /tmp/HoloIrohWitness.xcconfig
```

The temporary configuration keeps the ticket and PIN out of the Xcode build command.
The scheme expands these settings into the UI test runner environment.
Use `test`, not `test-without-building`, when you change witness settings.

For a saved profile, omit the temporary configuration.
Add `HOLOIROH_WITNESS_USE_SAVED_PROFILE=1` as an `xcodebuild` setting.

If connection configuration is absent, XCTest skips both connected methods.
Set both the ticket and PIN, or enable the saved-profile path.
If dry-run confirmation is absent, XCTest skips only the remote-input method.
The tests do not edit saved profiles or the daemon allowlist.

## What the witness proves

The public-pointer method needs no daemon connection.
It executes `hover()` and `rightClick()` on the iPad Simulator window.
This result proves that the public APIs compile and run on this Simulator.
It does not prove that the remote surface recognizers observed those events.

The gesture method performs a real XCTest pinch.
It asserts the existing `Zoom` accessibility label.
This result proves that the SwiftUI magnification recognizer committed the zoom.

The gesture method then performs a real coordinate press-and-drag.
It records a screenshot after the pan.
The app does not expose pan offset through accessibility.
The screenshot is evidence of execution, not a pan-state assertion.

The remote-input method uses these public APIs:

- `tap()` for a primary touch click.
- `press(forDuration:thenDragTo:)` for a primary touch drag.
- `click()` for a primary pointer click.
- `click(forDuration:thenDragTo:)` for a primary pointer drag.
- `hover()` for pointer movement.
- `rightClick()` for a secondary pointer click.
- `typeText(_:)` for hardware-style keyboard input.

The current Xcode Simulator SDK marks `hover()` and `rightClick()` as available on iOS 15 and later.
The app target is iOS 17.
Baguette and private Input/Output Kit Human Interface Device APIs are not required for event submission.

The test waits for the app's in-control banner before remote input.
It also asserts the app's `took control` diagnostic in the controls sheet.
This diagnostic proves the app invoked its take-control send path.
It is not a daemon acknowledgment.
`RemoteControlInputView` becomes first responder when it enters the window.
The test calls `typeText(_:)` on the live remote surface.
It uses a unique ASCII sentinel.

## Evidence gap

The app has no accessible per-event diagnostic.
The daemon sends no correlated input acknowledgment to the app.
A successful XCTest call therefore proves event submission to the Simulator.
It does not prove that each event crossed the control channel.
It also does not prove that macOS accepted a Core Graphics event.

Two follow-up hooks would close this gap:

1. Expose `zoom`, `panX`, and `panY` as a DEBUG accessibility value on `PanZoomVideoSurface`.
2. Add a DEBUG witness identifier to each remote-input event.
   Echo that identifier from the daemon after input dispatch.
   Expose the last acknowledged identifier and action as an accessibility value.

The second hook must update after daemon dispatch, not before `sendControlMessage`.
That order proves the recognizer, wire, and daemon dispatch path.
Dry-run mode can then prove the full path without affecting the Mac.

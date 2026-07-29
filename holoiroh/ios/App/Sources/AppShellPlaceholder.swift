// This target is a thin Xcode app-target wrapper around the `HoloIrohApp` SwiftUI package
// (see `ios/Sources/HoloIrohApp/REQUIRED_INFO_PLIST_KEYS.md`) -- it exists only to carry the
// Info.plist keys and code-signing identity a bare SPM library package cannot provide. The
// real `@main App` entry point lives in the linked `HoloIrohApp` package itself.
import HoloIrohApp

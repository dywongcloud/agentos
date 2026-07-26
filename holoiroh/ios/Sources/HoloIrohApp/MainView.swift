import SwiftUI
import UIKit

/// Main screen shown once the app has paired (see `ContentView`'s navigation
/// wiring). On appear it opens the real connection: one shared
/// `holoiroh-ios-bridge` handle (`HoloConnection`) carrying both the
/// `iroh-live` video subscription and the control-ALPN channel.
///
/// This screen is the **task dashboard** and drives the full Project Aro PRD
/// section 6.1 user-visible state machine: it holds a single `SessionState`
/// (`SessionState.swift`) and hosts a `SessionView` that renders exactly the
/// content + controls PRD 6.1 specifies for whichever of the eight states is
/// current. The eight states and their controls are:
///
/// - **Idle** — push-to-talk + paired-Mac availability; *Start*
/// - **Reviewing** — transcript / destination / dictated-text; *Edit / Send / Discard*
/// - **Connecting** — *Cancel*
/// - **Working** — Remote View + app/status/last-action/next-action; *Pause / Cancel / TakeControl*
/// - **Input needed** (P0-14) — what's needed / why / current-frame / response-options;
///   *TakeControl / ResolveLocally / Choose / Cancel*
/// - **Draft ready** — live target + verification; *Review / RequestSend / Cancel*
/// - **Awaiting approval** — destination / text / frame / commitment; *Approve / Reject*
/// - **Failed** — actionable cause + recovery; *Retry / TakeControl / Dismiss*
///
/// The persistent elements (the four README-called-out pieces) are preserved
/// and placed where they belong:
/// 1. The live **Remote View** — a real `VideoRenderView`
///    (`AVSampleBufferDisplayLayer`-backed) bound to a `VideoFrameSource`. It
///    is shown only in the states that need it (`SessionState.showsRemoteView`:
///    Working and DraftReady), and the synthetic on-device source stands in
///    for the not-yet-wired `iroh-live` network source.
/// 2. The **prompt bar** (text field + mic + send), always available so a new
///    request can be started from any state.
/// 3. The **status/log panel**, now inside the hidden controls sheet.
/// 4. The per-state **SessionView** panel, also inside the controls sheet
///    (toggled by the command bar's sparkle button; hidden by default).
///
/// ## Transport
///
/// When the app target links `HoloirohIosBridge.xcframework`, everything here
/// is driven by the real transport: `HoloConnection` connects the ticket and
/// PIN on appear, `frameSource` becomes the shared-bridge
/// `IrohLiveFrameSource`, outbound messages go through
/// `FFIControlChannelSender`, and inbound daemon `ServerMessage`s
/// (`handleServerMessage`) drive `session` and the log panel.
///
/// In bridge-less builds (simulator/CI, `#if canImport` stub) the transport
/// is unavailable: sends fall back to the `LoggingControlChannelSender`
/// stand-in, the synthetic frame source keeps the render path live, and the
/// "Demo" menu (now in the controls sheet) still jumps directly to any of
/// the eight states so every state's controls remain reachable.
struct MainView: View {
    /// The ticket this session paired with, shown in the header so the
    /// user can confirm which daemon they connected to.
    let ticket: String

    /// The pairing PIN captured on the pairing screen, used for the control
    /// channel's pre-session PIN handshake (`control_connect`). Empty when
    /// the daemon needs none (`--no-pin-auth` / already-allowlisted device).
    let pin: String

    /// Returns to `PairingView`.
    let onDisconnect: () -> Void

    /// App-wide profile store, so a `current_ticket` from the daemon can refresh
    /// the saved "Dev Mac" default when the daemon's identity has rotated.
    @EnvironmentObject private var profileStore: ConnectionProfileStore

    // MARK: - Dashboard identity fields (PRD 6.1 dashboard row)

    /// The paired Mac's name, shown in Idle availability + Connecting/Working.
    private let macName = "Studio Mac"
    /// Availability of the paired Mac (drives the Idle Start control's enabled
    /// state). Constant here -- a real presence signal arrives on the control
    /// channel later.
    private let macAvailable = true
    /// The active inference mode badge (PRD P0-11 "Aro Private" local mode).
    /// This build's daemon runs an on-device `llama-server` local model, so
    /// the honest value is "Aro Private (local)".
    private let inferenceMode = "Aro Private (local)"

    // MARK: - State

    /// The single source of truth for which PRD 6.1 state the dashboard shows.
    @State private var session: SessionState = .idle

    /// The shared-bridge connection owner: ONE `holoiroh-ios-bridge` handle
    /// carrying both the video subscription and the control channel. Created
    /// per screen; `onAppear` connects it with this session's ticket + PIN.
    @StateObject private var connection = HoloConnection()

    /// Orb reaction driver (`OrbEffects.swift`): every real send kicks the
    /// blob's thinking pulse, and prompts that mention known apps put their
    /// badges in orbit around it.
    @StateObject private var orbEffects = OrbEffectsState()

    /// The last task-committing message sent (`advanceFromReviewToWorking`),
    /// kept so the Failed panel's Retry re-sends the same request.
    @State private var lastSentTask: ClientMessage?

    /// Whether the user has paused the active task (wire `.pause` sent, no
    /// `.resume` yet). Drives the task-control pill's Pause/Resume toggle and
    /// keeps a pause-cancel `task_done(canceled)` from being mistaken for a
    /// real end-of-task (see `applyTaskDone`).
    @State private var isTaskPaused = false

    /// The `request_id` of the daemon's outstanding `input_request` (today:
    /// the sensitive-app consent ask), so the Choose control can echo it back
    /// as `.inputResponse`. `nil` when nothing is outstanding.
    @State private var activeInputRequestID: String?

    @State private var promptText: String = ""

    // MARK: - Clarify (ad-hoc clarifying questions)

    /// Ask the daemon to generate clarifying questions before running a
    /// possibly-ambiguous prompt. Opt-out via Diagnostics; default on.
    @AppStorage("clarifyEnabled") private var clarifyEnabled = true
    /// The prompt awaiting clarification -- set when a ClarifyRequest is sent,
    /// cleared when the questions are answered, dismissed, come back empty, or
    /// time out (fallback to a direct send).
    @State private var pendingClarifyPrompt: String?
    /// The daemon's clarifying questions; non-empty shows the ClarifyPanel above
    /// the command bar.
    @State private var clarifyQuestions: [ClarifyingQuestion] = []
    /// True while the clarify inference is in flight (drives the thinking state).
    @State private var isClarifying = false

    /// Guards `sendAutoPairPromptIfNeeded()` to at most once per process
    /// launch -- `.connected` can re-fire after a reconnect, and this must
    /// never re-send on that path.
    @State private var didSendAutoPairPrompt = false

    /// Whether the command bar's prompt field currently has keyboard focus.
    /// Binding this (rather than leaving focus implicit) is what makes the
    /// keyboard dismissible at all: SwiftUI has no built-in "tap outside to
    /// dismiss" or "Done button closes the keyboard" behavior for a bare
    /// `TextField` -- both are driven by setting this to `false`. Also
    /// cleared on every send (`sendLivePrompt`) so hitting the paperplane
    /// button or the keyboard's own Send key closes the keyboard instead of
    /// leaving it open with an now-empty field beneath it.
    @FocusState private var isPromptFocused: Bool

    /// Live keyboard height, tracked via `UIResponder.keyboardWill{Show,Hide}Notification`
    /// rather than derived from `isPromptFocused`/safe-area insets: this view's root backdrop
    /// uses `ignoresSafeArea()`, so SwiftUI's automatic keyboard-avoidance (which relies on the
    /// view responding to safe-area changes) never kicks in here, and `@FocusState` alone carries
    /// no height information anyway. Read directly from the notification's own
    /// `UIResponder.keyboardFrameEndUserInfoKey` so this always reflects the REAL animating
    /// keyboard height for the device/orientation/keyboard-type in use, not a guessed constant.
    /// Drives the command bar's full-height rise and the live-share box's
    /// clear-the-bar shift in `body` (see the `barShift`/`boxShift` derivation there).
    @State private var keyboardHeight: CGFloat = 0

    /// Real, live-measured height of the risen bottom stack (task pill + recent-
    /// prompts strip + clarify thinking-row/panel + the command bar itself),
    /// read via `ViewHeightKey` -- replaces a hardcoded `barClearance` constant
    /// that silently went stale every time new content (the clarify panel, the
    /// recent-prompts strip) was added above the command bar, since a fixed
    /// number can never reflect a VStack whose content varies. Falls back to a
    /// single command-bar-row's rough height (96) before the first real
    /// measurement lands, so `boxShift` is never computed against zero.
    @State private var measuredBarStackHeight: CGFloat = 96

    /// Real, live-measured height of `remoteTypeBar`, same `ViewHeightKey`
    /// discipline as `measuredBarStackHeight` above -- used to compute exactly
    /// how far the bar itself must rise to clear the keyboard, independent of
    /// the (now-stationary-during-remote-typing) video box's own position. See
    /// `remoteTypeBarShift(boxBottomY:screenHeight:)`.
    @State private var measuredRemoteTypeBarHeight: CGFloat = 56

    /// The real system keyboard-animation duration for the CURRENT show/hide,
    /// captured from `keyboardWillShowNotification`'s own userInfo
    /// (`UIResponder.keyboardAnimationDurationUserInfoKey`) instead of an
    /// approximate spring -- eliminates the visible lag-during-rise class of
    /// "the keyboard still overlaps the bar for a moment" bugs, since the bar's
    /// motion is now driven by the SAME duration the keyboard itself animates
    /// on. Falls back to a sane default (0.25s, iOS's typical keyboard
    /// duration) if the notification ever omits the key.
    @State private var keyboardAnimationDuration: Double = 0.25
    @StateObject private var voice = VoiceTranscriberModel()
    @State private var logEntries: [LogEntry] = [
        LogEntry(message: .status(text: "paired -- control channel not yet connected"))
    ]

    /// Whether the live-share surface is expanded to fullscreen (tap the
    /// floating panel to expand; the close control collapses it back).
    /// Layout-only state: the underlying `VideoRenderView` keeps ONE view
    /// identity across the transition, so the frame source is never
    /// stopped/restarted by going fullscreen.
    @State private var isVideoFullscreen = false

    /// True once the user has manually collapsed fullscreen WHILE still
    /// physically landscape -- suppresses the auto-landscape-fullscreen
    /// re-trigger for the rest of that landscape session (never fight the
    /// user, same pattern as the launch auto-connect suppression), and
    /// resets the moment the device rotates back to portrait so the NEXT
    /// landscape entry auto-expands again.
    @State private var manuallyCollapsedWhileLandscape = false

    /// True while the user has escalated to hands-on control: a touch surface
    /// over the live view injects their taps/drags/scrolls as remote input, and
    /// the daemon has paused any active agent turn. Entering sends `takeControl`
    /// (and resets zoom so touches map 1:1 to the video); exiting sends
    /// `releaseControl`. See `RemoteControl.swift`.
    @State private var isControllingRemotely = false

    /// True while the Mac's focused field is secure (password-class) -- the login window's
    /// authentication UI, a screen-lock password prompt, or a `sudo`/Keychain dialog has
    /// focus. Driven by `ServerMessage.secureInputState`, itself sourced from the daemon's
    /// `secure_input_watchdog` polling macOS's own `IsSecureEventInputEnabled()` signal. Used
    /// to explain the video's black region (ScreenCaptureKit excludes secure UI from every
    /// captured frame -- an OS security boundary, not a holoiroh bug) instead of leaving a
    /// user staring at an unexplained black rectangle.
    @State private var isMacAtLockScreen = false

    /// Whether the lightweight floating typing overlay (task 4) is shown over
    /// the live-share surface. Only ever visible while `isControllingRemotely`
    /// -- cleared automatically on releaseControl (see `toggleRemoteControl`)
    /// so it never persists floating over a non-controlled video.
    @State private var showRemoteTypeOverlay = false
    /// Focus for the floating typing overlay's text field, so opening it
    /// auto-raises the keyboard without the user needing a second tap.
    @FocusState private var isRemoteTypeFocused: Bool

    /// Whether the hidden controls sheet (the per-state SessionView panel,
    /// the status/log panel, and Disconnect) is presented. Toggled by the
    /// command bar's sparkle button; hidden by default.
    @State private var showControls = false

    /// App foreground/background state, driving the foreground video
    /// recovery in `body` (see the `.onChange(of: scenePhase)` there).
    @Environment(\.scenePhase) private var scenePhase

    // MARK: Live-share zoom (pinch to zoom, drag to pan, double-tap resets)

    /// Committed zoom factor for the live-share surface (1 = fit), passed
    /// into `PanZoomVideoSurface` as a `Binding` -- the in-flight pinch
    /// (`pinchScale`) and its clamping now live entirely inside that struct
    /// (see its doc for why: keeping the live gesture state off `MainView`
    /// itself is the actual performance fix).
    @State private var zoomScale: CGFloat = 1
    /// Committed pan offset of the zoomed content (points, post-scale),
    /// passed into `PanZoomVideoSurface` as a `Binding`.
    @State private var panOffset: CGSize = .zero

    /// Holds the running frame-timing witness (see `FrameTimingProbe.swift`)
    /// so it isn't deallocated mid-measurement -- debug-only, see
    /// `runFrameTimingProbeIfNeeded`.
    #if DEBUG
    @State private var frameTimingProbe: FrameTimingProbe?
    @State private var frameTimingDriveTimer: Timer?
    #endif

    // MARK: Auto-reconnect (app-switch/backgrounding recovery)

    /// Consecutive automatic reconnect attempts since the last successful
    /// `.connected`. Bounded (`Self.maxReconnectAttempts`) so a genuinely
    /// dead daemon surfaces a real failure instead of silent retry forever.
    @State private var reconnectAttempts = 0
    /// True while a reconnect is scheduled/in flight, so overlapping
    /// triggers (failure event + foreground return) collapse into one.
    @State private var isReconnecting = false
    /// Identity of the foreground liveness check in flight, so a stale
    /// check (superseded by a newer app switch) is a no-op.
    @State private var livenessCheckID = UUID()
    private static let maxReconnectAttempts = 5

    /// The frame producer bound to the video surface. Held with `@State`
    /// so it keeps a stable identity across view updates (a fresh source
    /// on every re-render would restart the display link each time).
    /// Starts as the synthetic pre-connect placeholder and is swapped for
    /// the real shared-bridge `IrohLiveFrameSource` the moment
    /// `HoloConnection` reaches `.connected` (see `handleConnectionPhase`);
    /// bridge-less builds keep the synthetic source for the whole session.
    @State private var frameSource: VideoFrameSource = SyntheticVideoFrameSource()

    /// The control-channel send seam (`ControlChannelSender.swift`). This is
    /// the **single injection site** for the outbound message path -- most
    /// importantly the remote kill-switch `ClientMessage.stop` the Cancel
    /// controls send (see `sendControlMessage` / `sessionActions.cancel`).
    /// Once `HoloConnection` completes the control-ALPN PIN handshake this is
    /// the real `FFIControlChannelSender` (every message goes over the wire
    /// via `holoiroh_ios_bridge_control_send`); before that -- and in
    /// bridge-less simulator/CI builds -- it falls back to the
    /// `LoggingControlChannelSender` stand-in, which encodes the same wire
    /// bytes and surfaces them in the log panel. Computed rather than stored
    /// so the fallback's `report` closure can append to `logEntries` without
    /// a stored-property initialization-order problem.
    private var controlChannel: ControlChannelSending {
        if let real = connection.controlSender {
            return real
        }
        return LoggingControlChannelSender { message, wire in
            // Surface the sent message in the same status/log panel every other
            // event flows through. The trailing newline is trimmed for display
            // (it is real on the wire, noise in the UI).
            let trimmed = wire.trimmingCharacters(in: .whitespacesAndNewlines)
            // Routed through `log` (not a direct append) so it is subject to the
            // same ring cap; a disconnected remote-control drag would otherwise
            // append at touch frequency with nothing trimming it.
            log(.status(text: "→ sent (not connected) \(message.wireKindLabel): \(trimmed)"))
        }
    }

    var body: some View {
        GeometryReader { geo in
            // Shared layout math: the live-share box is ~85% of the width at
            // a 16:10 aspect, with its top edge moved down to ~60% of the
            // height (was ~40% -- a ~20%-of-screen-height drop) so the box
            // clears most of the Spline orb scene above it. The orb's own
            // square canvas (SplineOrbBackground) can run up to 630pt tall
            // pinned near the top -- on an iPhone 14-class screen (390x844)
            // that reaches y~521, well past the old 40% box top (y~338),
            // which is why the box read as overlapping the orb. The box's
            // bottom edge at 60% + boxHeight still comfortably clears the
            // command bar below it. The SAME numbers drive both the box
            // chrome and the persistent video surface so the two always
            // coincide exactly.
            let boxWidth = geo.size.width * 0.85
            let boxHeight = boxWidth * 10.0 / 16.0
            // Box top at 56% of screen height (was 60% -- live-witnessed on
            // device: the box's bottom edge physically covered the
            // pause/stop task pill), PLUS a dynamic clearance for the pill's
            // own footprint whenever it is showing, so the mid-task controls
            // are never obscured by the live share.
            let pillClearance: CGFloat = (isTaskActive || isTaskPaused) ? 58 : 0
            let boxCenterY = geo.size.height * 0.56 + boxHeight / 2 - pillClearance
            let isConnected = connection.phase == .connected
            // Fullscreen only ever presents while the video surface exists;
            // if the connection drops mid-fullscreen the normal layout comes
            // straight back instead of leaving a black screen.
            let isFullscreenActive = isVideoFullscreen && isConnected
            // Auto-landscape-fullscreen (task 3): the SAME GeometryReader that
            // already drives every other layout number in this view is the
            // simplest, most robust orientation signal -- no separate
            // UIDevice.orientation notification wiring needed, and it
            // naturally re-fires on every real rotation since `body` itself
            // re-renders when `geo.size` changes.
            let isLandscape = geo.size.width > geo.size.height

            // Keyboard avoidance, DECOUPLED (live-witnessed bug in the joint
            // version: shifting bar and box together by `keyboardHeight -
            // spaceBelowBox` left the command bar UNDER the keyboard -- i.e.
            // invisible while typing -- whenever the box sat high enough that
            // `spaceBelowBox` swallowed most of the keyboard height):
            //
            // - The COMMAND BAR must always clear the keyboard completely, so
            //   it rises by the FULL keyboard height (`barShift`) and sits
            //   just above it -- the whole point of typing is seeing the bar.
            // - The BOX only moves by however much it needs to stay clear of
            //   the risen bar (`boxShift`): the bar's top lands at
            //   `H - keyboardHeight - barClearance`, and the box's bottom
            //   must stay above that. On small screens this can push the box
            //   toward the orb; the bar's visibility wins while the keyboard
            //   is up (it drops back the moment the keyboard closes).
            let boxBottomY = boxCenterY + boxHeight / 2
            let spaceBelowBox = geo.size.height - boxBottomY
            let barShift = keyboardHeight
            // The REAL measured height of the risen stack (see
            // `measuredBarStackHeight`'s doc) plus a small visual gap, instead
            // of a hardcoded guess -- correct regardless of whether the stack
            // is just the command bar or also carries the clarify panel/
            // recent-prompts strip/thinking row above it.
            let barClearance: CGFloat = measuredBarStackHeight + 8
            // Gated on `isPromptFocused` (the MAIN command bar), NOT on
            // `keyboardHeight` alone: `keyboardHeight` is a single global signal
            // that rises identically whether the command bar's field OR the
            // remote-type overlay's field is what's focused, and `remoteTypeBar`
            // is an overlay riding along with the box's own position -- so
            // without this gate, focusing the remote-type field ALSO shoved the
            // whole live-share video out of view (live-reported: "it moves the
            // screenshare up so i cant see it"), even though watching the video
            // update live is the entire point of remote-typing. The command
            // bar's own shelf behavior (this IS its rise) is unaffected.
            let boxShift = (keyboardHeight > 0 && isPromptFocused)
                ? max(0, keyboardHeight + barClearance - spaceBelowBox)
                : 0

            ZStack {
                // Layer 0: full-black backdrop + the blue blob orb Spline
                // scene (see SplineOrbBackground's doc for the web-runtime
                // rationale + offline gradient fallback). The orb renders
                // its own top-area layout; nothing is overlaid on the top
                // ~40% of the screen so it stays clear. The backdrop also
                // doubles as the tap-outside-to-dismiss-keyboard target --
                // safe because SplineOrbBackground is `.allowsHitTesting(false)`
                // and this tap only ever clears text-field focus, never
                // interferes with the live-share box's own tap-to-fullscreen
                // gesture (a distinct view, hit-tested first since it's later
                // in the ZStack).
                Color.black.ignoresSafeArea()
                    .contentShape(Rectangle())
                    .onTapGesture { isPromptFocused = false }
                SplineOrbBackground()
                // The orb's reaction layer (pulse rings, breathing glow,
                // orbiting app badges) -- positioned by the same canvas math
                // as the orb itself, never hit-testable.
                OrbReactionOverlay(state: orbEffects)

                if !isFullscreenActive {
                    // CENTER: the live-screen-share box (chrome + placeholder
                    // only -- the video itself is the persistent
                    // `videoOverlay` surface below, framed to this box).
                    liveShareBox(width: boxWidth, height: boxHeight)
                        .position(x: geo.size.width / 2, y: boxCenterY - boxShift)

                    // BOTTOM: the task-control pill (only while a task is
                    // active/paused) over the single command bar. Rises by the
                    // FULL keyboard height so the bar is always visible above
                    // the keyboard; the box shifts independently (see the
                    // barShift/boxShift derivation above).
                    VStack(spacing: 8) {
                        Spacer()
                        // The height probe is attached to THIS inner stack, not the
                        // outer one. The outer VStack contains a `Spacer()`, so it
                        // always fills the whole screen -- measuring it reported
                        // ~844pt (the full height) as `measuredBarStackHeight`
                        // instead of the ~100pt the bars actually occupy. That value
                        // feeds `barClearance`, which feeds `boxShift`, so focusing
                        // the prompt field shifted the live video up by roughly the
                        // height of the screen and pushed it out of view entirely --
                        // on a product where watching the screen while you type is
                        // the point.
                        VStack(spacing: 8) {
                            if isTaskActive || isTaskPaused {
                                taskControlBar
                                    .padding(.horizontal, 16)
                                    .transition(.move(edge: .bottom).combined(with: .opacity))
                            }
                            commandBar
                                .padding(.horizontal, 16)
                                .padding(.bottom, 8)
                        }
                        .background(
                            GeometryReader { stackGeo in
                                Color.clear.preference(key: ViewHeightKey.self, value: stackGeo.size.height)
                            }
                        )
                    }
                    .onPreferenceChange(ViewHeightKey.self) { measuredBarStackHeight = $0 }
                    .offset(y: -barShift)
                    .animation(.spring(response: 0.3, dampingFraction: 0.9), value: isTaskActive || isTaskPaused)
                }

                // Topmost: the persistent live-share video surface -- ONE
                // VideoRenderView across both the boxed and fullscreen
                // layouts (same view identity; the fullscreen transition is
                // a pure frame/layout change, never an unmount/remount).
                videoOverlay(
                    in: geo.size,
                    boxWidth: boxWidth,
                    boxHeight: boxHeight,
                    boxCenterY: boxCenterY - boxShift
                )
            }
            // Smooth, organic upward motion as the keyboard rises/falls,
            // rather than the box/command bar snapping to their new position
            // the instant `keyboardHeight` changes. A spring (not a fixed-
            // duration easing curve) so the motion still feels responsive if
            // the keyboard's own show/hide animation timing varies slightly
            // by device/iOS version, matching this app's existing spring-first
            // animation style elsewhere (e.g. the zoom/pan gestures).
            // Keyed on `keyboardHeight` -- both barShift and boxShift derive
            // from it, so one animation covers both surfaces. Duration matches
            // the REAL system keyboard animation (captured per-notification
            // into `keyboardAnimationDuration`), not an approximate spring --
            // a mismatched duration is what makes the bar visibly lag behind
            // the keyboard's own rise, the most likely real-world shape of
            // "the keyboard overlaps the command bar" during the transition
            // itself (the resting position was already correct).
            .animation(.easeInOut(duration: keyboardAnimationDuration), value: keyboardHeight)
            // The box glides (not snaps) when the task pill appears/vanishes
            // and shifts its position via pillClearance.
            .animation(.spring(response: 0.3, dampingFraction: 0.9), value: isTaskActive || isTaskPaused)
            // Manual keyboard avoidance is the ONLY authority here: without
            // this, SwiftUI's automatic safe-area keyboard avoidance ALSO
            // raises the layout on some iOS versions and STACKS with the
            // manual `barShift` offset -- live-witnessed as the command bar
            // flying to the very top of the screen when the keyboard opened.
            .ignoresSafeArea(.keyboard)
            // Auto-landscape-fullscreen: entering landscape expands to
            // fullscreen (unless the user already manually collapsed it this
            // landscape session); returning to portrait always restores the
            // boxed layout and clears that suppression for next time. The
            // manual tap-to-fullscreen toggle (in `videoOverlay`) still works
            // independently in portrait.
            .onChange(of: isLandscape) { _, nowLandscape in
                withAnimation(.spring(response: 0.35, dampingFraction: 0.85)) {
                    if nowLandscape {
                        if !manuallyCollapsedWhileLandscape {
                            isVideoFullscreen = true
                        }
                    } else {
                        isVideoFullscreen = false
                        manuallyCollapsedWhileLandscape = false
                    }
                }
            }
        }
        .onReceive(NotificationCenter.default.publisher(for: UIResponder.keyboardWillShowNotification)) { note in
            captureKeyboardAnimationDuration(from: note)
            guard let frame = note.userInfo?[UIResponder.keyboardFrameEndUserInfoKey] as? CGRect else { return }
            keyboardHeight = frame.height
        }
        .onReceive(NotificationCenter.default.publisher(for: UIResponder.keyboardWillChangeFrameNotification)) { note in
            captureKeyboardAnimationDuration(from: note)
            guard let frame = note.userInfo?[UIResponder.keyboardFrameEndUserInfoKey] as? CGRect else { return }
            // A visible-but-resized keyboard (e.g. QuickType bar toggling,
            // switching to a different keyboard layout) fires this instead
            // of a fresh will-show -- keep the tracked height in sync so the
            // shift doesn't freeze at a stale value.
            keyboardHeight = max(0, UIScreen.main.bounds.height - frame.origin.y)
        }
        .onReceive(NotificationCenter.default.publisher(for: UIResponder.keyboardWillHideNotification)) { note in
            captureKeyboardAnimationDuration(from: note)
            keyboardHeight = 0
        }
        .toolbar(.hidden, for: .navigationBar)
        // Hidden-by-default controls: the SessionView state panel, the
        // status/log panel, and Disconnect live in this sheet, toggled by
        // the command bar's sparkle button.
        .sheet(isPresented: $showControls) {
            controlsSheet
        }
        // Live partial + final transcript updates populate the prompt field
        // as they arrive while a recognition session is running.
        .onChange(of: voice.liveText) { _, newText in
            guard voice.isRecording, !newText.isEmpty else { return }
            promptText = newText
        }
        .onChange(of: voice.lastError) { _, error in
            guard let error else { return }
            log(.error(text: error))
        }
        // Real transport lifecycle: connect the shared bridge on appear,
        // reflect its phase changes, and tear it down when the screen goes.
        .onAppear(perform: configureConnectionIfNeeded)
        .onAppear(perform: autoFocusPromptIfNeeded)
        .onDisappear { connection.shutdown() }
        .onChange(of: connection.phase) { _, newPhase in
            handleConnectionPhase(newPhase)
        }
        // Boxed <-> fullscreen changes the zoom viewport; the committed pan
        // was clamped for the old one, so start the new layout at fit.
        .onChange(of: isVideoFullscreen) { _, _ in
            resetZoom()
        }
        // Foreground recovery, two tiers (live-witnessed bug: "screen goes
        // black and errors out if I open a different app"):
        //
        // Tier 1 (cheap, quick app switches): iOS invalidates VideoToolbox
        // decode sessions on backgrounding, so restarting the frame source
        // gets a FRESH track whose first frame is a keyframe, healing the
        // decoder. Works only while the underlying QUIC session survived.
        //
        // Tier 2 (longer absences): iOS suspends the process, the QUIC
        // session idles out daemon-side, and a frame-source restart then
        // re-subscribes on a DEAD bridge -- black screen, plus the control
        // pump's poll failure flips the phase to `.failed` ("errors out").
        // Detection is a liveness check (no frame within 4s of the
        // restart) or the phase already being `.failed` on return; recovery
        // is a FULL reconnect (fresh bridge + ticket + PIN) via
        // `attemptReconnect`, which the daemon supports (it re-accepts the
        // same ticket + allowlisted device without a new PIN scan).
        .onChange(of: scenePhase) { _, newPhase in
            guard newPhase == .active else { return }
            switch connection.phase {
            case .connected:
                frameSource.stop()
                frameSource.start()
                armForegroundLivenessCheck()
            case .failed:
                attemptReconnect(reason: "connection lost while backgrounded")
            case .idle, .connecting:
                break
            }
        }
    }

    // MARK: - Keyboard shelf helpers

    /// How far `remoteTypeBar` must rise (independent of the video box, which
    /// stays put -- see `boxShift`'s doc) to clear the keyboard, when the
    /// remote-typing field is what's focused. Pure function of already-known
    /// geometry so it's directly probe-testable without a live keyboard.
    ///
    /// `boxBottomY` is the box's bottom edge in screen coordinates (its natural
    /// resting position, since the box no longer shifts for this case);
    /// `remoteTypeBar` sits `14`pt below that (its own `.padding(.bottom, 14)`)
    /// plus its own real measured height. If that natural bottom edge is
    /// already above the keyboard's top edge, no rise is needed.
    private func remoteTypeBarShift(boxBottomY: CGFloat, screenHeight: CGFloat) -> CGFloat {
        guard isRemoteTypeFocused, keyboardHeight > 0 else { return 0 }
        let naturalBottomY = boxBottomY + 14 + measuredRemoteTypeBarHeight
        let keyboardTopY = screenHeight - keyboardHeight
        return max(0, naturalBottomY - keyboardTopY + 8)
    }

    /// Reads the REAL keyboard-animation duration out of a keyboard
    /// show/hide/change-frame notification's userInfo, so the command-bar's
    /// rise/fall animation (see `keyboardAnimationDuration`) matches the
    /// system's own timing instead of an approximate spring. Falls back to
    /// the existing value (never resets to a guess) if a notification is ever
    /// missing the key -- degrade gracefully, never regress to a worse guess.
    private func captureKeyboardAnimationDuration(from note: Notification) {
        guard let duration = note.userInfo?[UIResponder.keyboardAnimationDurationUserInfoKey] as? Double,
              duration > 0
        else { return }
        keyboardAnimationDuration = duration
    }

    // MARK: - Live-share zoom helpers

    private func resetZoom() {
        zoomScale = 1
        panOffset = .zero
    }

    // MARK: - Auto-reconnect

    /// After a foreground frame-source restart, verify frames actually
    /// resume; if none arrive within the window, the transport is dead and
    /// only a full reconnect helps. Keyed by `livenessCheckID` so only the
    /// LATEST app switch's check can fire.
    private func armForegroundLivenessCheck() {
        let checkID = UUID()
        livenessCheckID = checkID
        let restartedAt = Date()
        DispatchQueue.main.asyncAfter(deadline: .now() + 4) {
            guard livenessCheckID == checkID,
                  scenePhase == .active,
                  connection.phase == .connected else { return }
            let last = frameSource.lastFrameAt
            if last == nil || last! < restartedAt {
                attemptReconnect(reason: "no frames after returning to foreground")
            }
        }
    }

    /// Bounded, backoff-spaced full reconnect: tear the dead session down
    /// (`HoloConnection.reset`) and redo the whole connect (bridge + ticket
    /// + PIN) with the credentials this screen was opened with. The peer
    /// sees a status line, not an error, unless every attempt is exhausted.
    private func attemptReconnect(reason: String) {
        guard !isReconnecting else { return }
        guard reconnectAttempts < Self.maxReconnectAttempts else {
            log(.error(text: "reconnect failed after \(Self.maxReconnectAttempts) attempts -- use Disconnect and pair again"))
            return
        }
        isReconnecting = true
        reconnectAttempts += 1
        let attempt = reconnectAttempts
        log(.status(text: "connection lost (\(reason)) -- reconnecting (attempt \(attempt)/\(Self.maxReconnectAttempts))"))
        // First attempt immediately; later ones back off (2s, 4s, 8s, 8s)
        // so a daemon mid-restart isn't hammered.
        let delay: TimeInterval = attempt == 1 ? 0 : min(pow(2, Double(attempt - 1)), 8)
        DispatchQueue.main.asyncAfter(deadline: .now() + delay) {
            isReconnecting = false
            guard connection.phase != .connected else { return }
            connection.reset()
            connection.connect(ticket: ticket, pin: pin)
        }
    }

    // MARK: - Live-share box (chrome + placeholder)

    /// The centered rounded live-screen-share box: black fill, thin light
    /// border, corner radius 28. Shows the placeholder text until the
    /// connection is up; once connected the persistent `videoOverlay`
    /// surface renders the live video framed to exactly this box. Tapping
    /// toggles fullscreen (only meaningful once connected).
    private func liveShareBox(width: CGFloat, height: CGFloat) -> some View {
        let isConnected = connection.phase == .connected
        return RoundedRectangle(cornerRadius: 28)
            .fill(Color.black)
            .overlay(
                RoundedRectangle(cornerRadius: 28)
                    .stroke(
                        LinearGradient(
                            colors: [.white.opacity(0.40), .white.opacity(0.10)],
                            startPoint: .top,
                            endPoint: .bottom
                        ),
                        lineWidth: 1
                    )
            )
            .overlay {
                if !isConnected {
                    VStack(spacing: 10) {
                        Image(systemName: "rectangle.on.rectangle")
                            .font(.system(size: 30, weight: .light))
                            .foregroundStyle(.white.opacity(0.45))
                        Text("Live screen share")
                            .font(.headline)
                            .foregroundStyle(.white.opacity(0.85))
                        if connection.phase == .connecting {
                            HStack(spacing: 6) {
                                ProgressView()
                                    .tint(Self.orbAccent)
                                    .scaleEffect(0.7)
                                Text("Connecting…")
                                    .font(.caption)
                                    .foregroundStyle(.white.opacity(0.6))
                            }
                        } else {
                            Text("Appears when your Mac connects")
                                .font(.caption)
                                .foregroundStyle(.white.opacity(0.45))
                        }
                    }
                    .padding(.horizontal, 16)
                }
            }
            .frame(width: width, height: height)
            .contentShape(RoundedRectangle(cornerRadius: 28))
            .onTapGesture {
                guard isConnected else { return }
                withAnimation(.spring(response: 0.35, dampingFraction: 0.85)) {
                    isVideoFullscreen.toggle()
                }
            }
            .shadow(color: .black.opacity(0.6), radius: 24, y: 10)
            .shadow(color: Self.orbAccent.opacity(0.12), radius: 32)
            .accessibilityLabel("Live screen share")
    }

    // MARK: - Controls sheet (hidden by default)

    /// The sheet the sparkle button toggles: the demo state-jump menu, the
    /// per-state `SessionView` panel, the status/log panel, and Disconnect.
    /// None of this is visible by default -- the main screen stays minimal.
    private var controlsSheet: some View {
        ScrollView {
            VStack(spacing: 16) {
                demoMenu
                    .frame(maxWidth: .infinity, alignment: .leading)

                SessionView(
                    state: session,
                    macName: macName,
                    macAvailable: macAvailable,
                    inferenceMode: inferenceMode,
                    actions: sessionActions
                )

                logPanel
                    .clipShape(RoundedRectangle(cornerRadius: 12))

                Button(role: .destructive) {
                    connection.shutdown()
                    onDisconnect()
                } label: {
                    VStack(spacing: 2) {
                        Text("Disconnect")
                            .font(.headline)
                        // This button is also the only way back to the pairing screen's saved
                        // profiles list -- without this line the profile-switching path was
                        // undiscoverable (it looked like a one-way "end the session" action).
                        Text("Switch Mac or manage saved connections")
                            .font(.caption)
                            .foregroundStyle(.white.opacity(0.85))
                    }
                    .frame(maxWidth: .infinity)
                }
                .buttonStyle(.borderedProminent)
                .tint(.red)
            }
            .padding()
        }
        .presentationDetents([.medium, .large])
    }

    // MARK: - Demo control (local transition trigger stand-in)

    /// A menu (in the controls sheet) that jumps to any of the eight PRD 6.1 states
    /// with a representative payload. This is the "direct" half of making the
    /// state machine demonstrably live without a wired control channel -- see
    /// this type's doc comment. Each jump also drops a synthesized log entry
    /// so the log panel reflects the jump.
    private var demoMenu: some View {
        Menu {
            Button("Idle") { jump(to: .idle) }
            Button("Reviewing") { jump(to: .reviewing(Self.demoReview)) }
            Button("Connecting") { jump(to: .connecting) }
            Button("Working") { jump(to: .working(Self.demoWorking)) }
            Button("Input needed") { jump(to: .inputNeeded(Self.demoInputNeeded)) }
            Button("Draft ready") { jump(to: .draftReady(Self.demoDraft)) }
            Button("Awaiting approval") { jump(to: .awaitingApproval(Self.demoApproval)) }
            Button("Failed") { jump(to: .failed(Self.demoFailure)) }
        } label: {
            Label("Demo", systemImage: "wand.and.stars")
        }
    }

    private func jump(to newState: SessionState) {
        session = newState
        log(.status(text: "demo → \(newState.displayName)"))
    }

    // MARK: - Session action wiring

    /// The control callbacks handed to `SessionView`. The task-committing
    /// legs are real: `send` commits the reviewed request over the control
    /// channel (`advanceFromReviewToWorking`), `cancel` sends the kill-switch
    /// `ClientMessage.stop`, and `retry` re-sends the last request; the
    /// daemon's `ServerMessage` responses then drive `session`
    /// (`handleServerMessage`). Actions with no wire message yet
    /// (approve/choose/resolve-locally/take-control) remain local transitions
    /// until the protocol grows their `ClientMessage` shapes.
    private var sessionActions: SessionActions {
        SessionActions(
            start: { beginReview(from: currentTranscriptOrDemo()) },
            edit: {
                // Return to the prompt bar to amend the request.
                if case .reviewing(let payload) = session {
                    promptText = payload.transcript
                }
                log(.status(text: "editing request"))
                session = .idle
            },
            send: { advanceFromReviewToWorking() },
            discard: {
                log(.status(text: "request discarded"))
                session = .idle
            },
            cancel: {
                // Remote kill-switch: the Cancel control (shown in the Working,
                // Connecting, Input-needed, and Draft-ready panels) is the same
                // Stop the task pill exposes on the main screen.
                stopActiveTask()
            },
            pause: { togglePause() },
            takeControl: { log(.status(text: "take control -- manual input handed to user")) },
            resolveLocally: {
                log(.status(text: "resolving input request locally on device"))
                // Resolving the input request resumes the working turn.
                session = .working(Self.demoWorking)
            },
            choose: { option in
                log(.status(text: "chose: \(option)"))
                if let requestID = activeInputRequestID {
                    // A REAL daemon input request (sensitive-app consent):
                    // echo the selection back verbatim. The daemon resumes
                    // (allow) or stops (deny) the task and its own frames
                    // settle the session state; connecting is the honest
                    // in-between.
                    sendControlMessage(.inputResponse(requestId: requestID, selectedOption: option))
                    activeInputRequestID = nil
                    session = option.lowercased().contains("stop") ? .idle : .connecting
                } else {
                    session = .working(Self.demoWorking)
                }
            },
            review: { log(.status(text: "reviewing draft in Remote View")) },
            requestSend: {
                log(.status(text: "send requested -- awaiting your approval"))
                session = .awaitingApproval(Self.demoApproval)
            },
            approve: {
                log(.taskProgress(text: "approved -- committing action"))
                log(.status(text: "message sent"))
                session = .idle
            },
            reject: {
                log(.status(text: "commitment rejected -- nothing sent"))
                session = .idle
            },
            retry: {
                if let lastSentTask {
                    // A retry is a user-message send too -- the orb reacts.
                    if case .prompt(let text) = lastSentTask {
                        orbEffects.react(to: text)
                    } else if case .voiceTranscript(let text) = lastSentTask {
                        orbEffects.react(to: text)
                    }
                    sendControlMessage(lastSentTask)
                    log(.status(text: "retrying task"))
                    session = .connecting
                } else {
                    log(.status(text: "nothing to retry"))
                    session = .idle
                }
            },
            dismiss: {
                log(.status(text: "failure dismissed"))
                session = .idle
            }
        )
    }

    /// Pause/Resume over the REAL wire (the old version only toggled a local
    /// flag -- the agent on the Mac kept acting). Pause sends `.pause` (the
    /// daemon parks the turn); Resume sends `.resume` (the daemon re-dispatches
    /// it on the same backend session). Local state mirrors optimistically;
    /// the daemon's own status/`task_done` frames are the source of truth in
    /// the log.
    private func togglePause() {
        if isTaskPaused {
            sendControlMessage(.resume)
            isTaskPaused = false
            log(.status(text: "→ resume"))
            if case .working(var payload) = session {
                payload.isPaused = false
                session = .working(payload)
            } else {
                session = .connecting
            }
        } else {
            guard isTaskActive else {
                log(.status(text: "no active task to pause"))
                return
            }
            sendControlMessage(.pause)
            isTaskPaused = true
            log(.status(text: "→ pause"))
            if case .working(var payload) = session {
                payload.isPaused = true
                session = .working(payload)
            }
        }
    }

    // MARK: - Real connection wiring

    /// Wires the connection's decoded-event stream into this view and starts
    /// the real connect (bridge create → ticket connect → control-ALPN PIN
    /// handshake). Safe to call again on re-appear: `HoloConnection.connect`
    /// is idempotent once past `.idle`.
    private func configureConnectionIfNeeded() {
        connection.onServerMessage = { message in
            handleServerMessage(message)
        }
        connection.connect(ticket: ticket, pin: pin)
    }

    /// Reacts to connection lifecycle changes: on `.connected`, swap the
    /// synthetic placeholder for the shared-bridge live frame source; on
    /// `.failed`, surface the reason (bridge-less builds land here too, and
    /// keep the synthetic source + logging-sender fallbacks).
    private func handleConnectionPhase(_ phase: HoloConnection.Phase) {
        switch phase {
        case .idle, .connecting:
            break
        case .connected:
            reconnectAttempts = 0
            isReconnecting = false
            resetZoom()
            if let live = connection.liveFrameSource {
                frameSource.stop()
                frameSource = live
            }
            Haptics.fire(.connect)
            ConnectionDiagnostics.shared.recordConnected(ticket: ticket)
            sendAutoPairPromptIfNeeded()
        case .failed(let reason):
            ConnectionDiagnostics.shared.recordFailure(reason, ticket: ticket)
            // While the app is frontmost, a mid-session failure (daemon
            // restarted, network blipped, QUIC idle-out racing the
            // background transition) heals itself via the same bounded
            // reconnect as the app-switch path. Backgrounded failures wait:
            // sockets are suspended anyway, and the `.active` transition
            // retries the moment the user returns.
            log(.error(text: "connection unavailable: \(reason)"))
            if scenePhase == .active {
                attemptReconnect(reason: reason)
            }
        }
    }

    // MARK: - Real control-channel event handling

    /// Projects one daemon `ServerMessage` onto the log panel and, where it
    /// implies a lifecycle change, onto `session` -- the coarse PRD 6.1
    /// projection (`SessionState`'s mapping table). The wire protocol's four
    /// message kinds are coarse, so the projection is too: `task_progress`
    /// advances/updates the active task, `error` fails it, `ack`/`status`
    /// are log-only.
    private func handleServerMessage(_ message: ServerMessage) {
        log(message)
        switch message {
        case .ack, .status:
            break
        case .taskProgress(let text):
            applyTaskProgress(text)
        case .error(let text):
            failActiveTask(cause: text)
        case .taskDone(let status, let text):
            applyTaskDone(status: status, text: text)
        case .taskActive(let paused, _):
            restoreTaskControls(paused: paused)
        case .authRejected(let text):
            failActiveTask(cause: text ?? "The daemon rejected this device's authentication.")
        case .currentTicket(let ticket):
            applyCurrentTicket(ticket)
        case .clarifyQuestions(let questions):
            applyClarifyQuestions(questions)
        case .inputRequest(let requestId, let kind, let context, let responseOptions, _):
            presentInputRequest(
                requestId: requestId,
                kind: kind,
                context: context,
                responseOptions: responseOptions
            )
        case .secureInputState(let active):
            isMacAtLockScreen = active
        }
    }

    /// `task_done` is the daemon's real end-of-task signal (completed /
    /// failed / canceled). One subtlety: pausing a task IS a cancel on the
    /// wire (the daemon parks the turn by canceling it -- see the daemon's
    /// pause semantics), so a `canceled` arriving while `isTaskPaused` keeps
    /// the paused pill up instead of dismissing the task.
    private func applyTaskDone(status: String, text: String?) {
        switch status {
        case "failed":
            isTaskPaused = false
            failActiveTask(cause: text)
        case "canceled":
            if !isTaskPaused, isTaskActive {
                session = .idle
            }
        default: // "completed"
            isTaskPaused = false
            if isTaskActive {
                session = .idle
            }
        }
    }

    /// The daemon reported its current ticket over the authenticated channel.
    /// If it differs from the saved default (the daemon's identity rotated),
    /// refresh the "Dev Mac" default so future launches reach the rotated daemon
    /// without a QR re-scan. Validated + no-op-on-same in the store, so a stale
    /// or identical ticket never downgrades a working default.
    private func applyCurrentTicket(_ ticket: String) {
        let previous = profileStore.defaultProfile?.ticket ?? ""
        guard profileStore.refreshDefaultTicket(ticket) else { return }
        ConnectionDiagnostics.shared.recordTicketRefresh(from: previous, to: ticket)
        log(.status(text: "saved Dev Mac ticket refreshed — daemon identity rotated"))
    }

    /// Projects a daemon `input_request` (today: the sensitive-app consent
    /// gate) onto the PRD 6.1 Input-needed state with its REAL payload, and
    /// opens the controls sheet so the Choose buttons are actually on screen.
    private func presentInputRequest(
        requestId: String,
        kind: String,
        context: String,
        responseOptions: [String]
    ) {
        let uiKind: InputRequestKind
        switch kind {
        case "credential": uiKind = .credentialNeeded
        case "mfa": uiKind = .mfaNeeded
        case "ambiguous_choice": uiKind = .ambiguousChoice
        case "missing_info": uiKind = .missingInfo
        default: uiKind = .sensitiveAccess
        }
        activeInputRequestID = requestId
        isTaskPaused = false
        session = .inputNeeded(InputRequestPayload(
            kind: uiKind,
            whatIsNeeded: context,
            why: "The task is paused until you decide.",
            currentFrame: "See the live view above.",
            responseOptions: responseOptions
        ))
        showControls = true

        #if DEBUG
        // Unattended consent witness (same env-hook family as
        // HOLOIROH_AUTOPAIR_*): devicectl cannot tap the Choose buttons, so
        // HOLOIROH_AUTOCONSENT="Allow once" answers the request through the
        // exact same wire path a real tap takes, 1s after it appears.
        if let auto = ProcessInfo.processInfo.environment["HOLOIROH_AUTOCONSENT"],
           responseOptions.contains(auto) {
            DispatchQueue.main.asyncAfter(deadline: .now() + 1) {
                guard activeInputRequestID == requestId else { return }
                log(.status(text: "auto-consent (debug): \(auto)"))
                sendControlMessage(.inputResponse(requestId: requestId, selectedOption: auto))
                activeInputRequestID = nil
                session = auto.lowercased().contains("stop") ? .idle : .connecting
            }
        }
        #endif
    }

    /// `task_progress` drives Connecting → Working (the daemon accepted the
    /// task and is acting) and thereafter updates the Working dashboard
    /// fields in place.
    private func applyTaskProgress(_ text: String?) {
        let line = text ?? "working"
        switch session {
        case .connecting:
            session = .working(WorkingPayload(
                app: macName,
                status: line,
                lastAction: "task accepted",
                nextAction: "in progress"
            ))
        case .working(var payload):
            payload.lastAction = payload.status
            payload.status = line
            session = .working(payload)
        default:
            break
        }
    }

    /// Reconnect restoration (issue-2): the daemon's `task_active` told us a task
    /// from before the connection drop is still live, so bring the Pause/Stop
    /// task-control pill back. A running task moves `session` to `.working`
    /// (making `isTaskActive` true -> pill shows "Task running" + Pause/Stop); a
    /// paused task sets `isTaskPaused` (pill shows "Paused" + Resume/Stop). Never
    /// downgrades a fresher local state: the running branch only fires when we
    /// aren't already showing an active task, so a live turn's own progress
    /// isn't clobbered by a late reconnect notice.
    private func restoreTaskControls(paused: Bool) {
        if paused {
            // Reconnected to a paused task, OR auto-yield stepped the agent aside
            // because the user is active: show the pill in its Paused state.
            isTaskPaused = true
        } else {
            // Running: reconnect to a live task, or auto-yield resuming now the
            // user is idle. Clear the paused flag so the pill flips Paused ->
            // running (unconditionally, since during a live task isTaskActive is
            // already true), and make the task active if we weren't showing it
            // (the reconnect-into-idle case).
            isTaskPaused = false
            if !isTaskActive {
                session = .working(WorkingPayload(
                    app: macName,
                    status: "running",
                    lastAction: "resumed",
                    nextAction: "in progress"
                ))
            }
        }
    }

    /// A daemon-reported `error` fails whatever task is active; outside a
    /// task it is log-only (already appended by `handleServerMessage`).
    private func failActiveTask(cause: String?) {
        switch session {
        case .connecting, .working, .inputNeeded, .draftReady, .awaitingApproval:
            session = .failed(FailurePayload(
                cause: cause ?? "The daemon reported an error.",
                recovery: "Retry the task, or take control on the Mac."
            ))
        case .idle, .reviewing, .failed:
            break
        }
    }

    // MARK: - Organic transitions (prompt-send walk)

    /// The current transcript to review: the live prompt text if present, else
    /// a representative demo transcript so Start from an empty field still
    /// demonstrates the flow.
    private func currentTranscriptOrDemo() -> String {
        let trimmed = promptText.trimmingCharacters(in: .whitespacesAndNewlines)
        return trimmed.isEmpty ? Self.demoReview.transcript : trimmed
    }

    /// Idle → Reviewing: a captured request is shown back for confirmation.
    private func beginReview(from transcript: String) {
        let payload = ReviewPayload(
            transcript: transcript,
            destination: Self.demoReview.destination,
            dictatedText: Self.demoReview.dictatedText
        )
        session = .reviewing(payload)
        log(.status(text: "reviewing: \"\(transcript)\""))
        promptText = ""
    }

    /// Reviewing → Connecting, the "Send" leg: commits the reviewed request
    /// to the daemon over the control channel. There is no local timer any
    /// more -- the transition out of `.connecting` is driven by the daemon's
    /// real responses (`handleServerMessage`): a `task_progress` advances to
    /// `.working`, an `error` fails the task.
    private func advanceFromReviewToWorking() {
        guard case .reviewing(let payload) = session else { return }
        // Voice-originated transcripts keep their `voice_transcript` wire tag
        // (PROTOCOL.md); typed prompts go out as `prompt`.
        let message: ClientMessage = payload.transcript == voice.liveText
            ? .voiceTranscript(text: payload.transcript)
            : .prompt(text: payload.transcript)
        lastSentTask = message
        // Staged sends are user-message sends too -- the orb reacts.
        orbEffects.react(to: payload.transcript)
        sendControlMessage(message)
        log(.status(text: "sent -- waiting for the daemon"))
        session = .connecting
    }

    // MARK: - Live-share surface (boxed <-> fullscreen)

    /// The persistent live-share surface. Mounted the moment the connection
    /// is up and NEVER unmounted by session-state changes or the fullscreen
    /// toggle (the old `showsRemoteView`-gated slot tore the shared frame
    /// source down on every state transition -- a live-witnessed
    /// permanent-black-screen bug class). Boxed: framed to exactly the
    /// center live-share box (chrome drawn by `liveShareBox`). Fullscreen
    /// (tap to expand): fills the screen over a black backdrop with the live
    /// chat feed + command bar overlaid, Twitch-style, so prompts can be
    /// sent to the agent without leaving the mirror. ONE VideoRenderView
    /// instance across both layouts: `.id` keys it to the SOURCE's identity
    /// only (connect-time swap), never the fullscreen flag, so the
    /// transition is a pure frame/layout change.
    @ViewBuilder
    private func videoOverlay(
        in size: CGSize,
        boxWidth: CGFloat,
        boxHeight: CGFloat,
        boxCenterY: CGFloat
    ) -> some View {
        if connection.phase == .connected {
            // The visible viewport the (possibly zoomed) mirror is clipped
            // to. Zoom scales the render view INSIDE this fixed viewport;
            // pan slides the scaled content, clamped so no black bars can
            // be dragged into view.
            let viewport = CGSize(
                width: isVideoFullscreen ? size.width : boxWidth,
                height: isVideoFullscreen ? size.height : boxHeight
            )

            ZStack {
                if isVideoFullscreen {
                    Color.black
                        .ignoresSafeArea()
                        .transition(.opacity)
                }

                // The gesture-transform-affected content (video + pinch/pan +
                // zoom badge) lives in its own View struct so a live pinch/pan
                // only re-renders THAT small subtree, not this whole body --
                // see `PanZoomVideoSurface`'s doc for the full performance
                // rationale (the "panning and scrolling is choppy" fix).
                PanZoomVideoSurface(
                    frameSource: frameSource,
                    viewport: viewport,
                    isVideoFullscreen: isVideoFullscreen,
                    isControllingRemotely: isControllingRemotely,
                    zoomScale: $zoomScale,
                    panOffset: $panOffset
                )
                    .overlay(alignment: .topTrailing) {
                        // Expand / collapse control.
                        Button {
                            withAnimation(.spring(response: 0.35, dampingFraction: 0.85)) {
                                // Collapsing WHILE physically landscape means the user is
                                // explicitly overriding the auto-landscape-fullscreen trigger --
                                // suppress it until the next rotation-to-portrait-and-back
                                // (see `manuallyCollapsedWhileLandscape`'s doc).
                                if isVideoFullscreen, size.width > size.height {
                                    manuallyCollapsedWhileLandscape = true
                                }
                                isVideoFullscreen.toggle()
                            }
                        } label: {
                            Image(systemName: isVideoFullscreen
                                  ? "arrow.down.right.and.arrow.up.left"
                                  : "arrow.up.left.and.arrow.down.right")
                                .font(.footnote.weight(.bold))
                                .contentTransition(.symbolEffect(.replace))
                                .padding(10)
                                .background(.ultraThinMaterial, in: Circle())
                        }
                        .padding(10)
                        .sensoryFeedback(.impact(weight: .medium), trigger: isVideoFullscreen)
                        .accessibilityLabel(isVideoFullscreen ? "Exit fullscreen" : "Fullscreen live view")
                    }
                    // Hands-on control: a touch surface over the video that injects
                    // the user's taps/drags/scrolls as remote input, plus a toggle
                    // and a banner. The pan/tap gestures below are explicitly
                    // GestureMask.none'd while controlling (a plain `.overlay`
                    // does NOT, by itself, stop a `simultaneousGesture` elsewhere in
                    // the hierarchy from also recognizing the same touch -- that was
                    // this section's own prior, incorrect assumption, and the
                    // resulting double-interpretation was the live-reported "pan/zoom
                    // accidentally moves the cursor" bug). Pinch stays active
                    // unconditionally -- see its own doc below.
                    .overlay {
                        if isControllingRemotely {
                            RemoteControlSurface(frameSize: frameSource.lastFrameSize) { ev in
                                sendControlMessage(.remoteControl(ev))
                            }
                            .frame(width: viewport.width, height: viewport.height)
                            .clipShape(RoundedRectangle(cornerRadius: isVideoFullscreen ? 0 : 28))
                        }
                    }
                    .overlay(alignment: .topLeading) { remoteControlToggle }
                    .overlay(alignment: .topTrailing) { remoteTypeToggle }
                    .overlay(alignment: .top) {
                        // Stacked, not competing for the same slot: the lock banner explains
                        // the black region whenever it's up, independent of whether the user
                        // also happens to be in hands-on control at the same moment.
                        VStack(spacing: 6) {
                            if isMacAtLockScreen {
                                Label(
                                    isControllingRemotely
                                        ? "Mac is locked \u{2014} tap the keyboard to type your password"
                                        : "Mac is locked \u{2014} take control to sign in",
                                    systemImage: "lock.fill"
                                )
                                    .font(.caption2.weight(.semibold))
                                    .foregroundStyle(.white)
                                    .padding(.horizontal, 10)
                                    .padding(.vertical, 5)
                                    .background(Color.blue.opacity(0.92), in: Capsule())
                            }
                            if isControllingRemotely {
                                Text("You're in control \u{2014} the agent is paused")
                                    .font(.caption2.weight(.semibold))
                                    .foregroundStyle(.white)
                                    .padding(.horizontal, 10)
                                    .padding(.vertical, 5)
                                    .background(Color.orange.opacity(0.92), in: Capsule())
                            }
                        }
                        .padding(.top, 10)
                    }
                    .overlay(alignment: .bottom) {
                        if isControllingRemotely, showRemoteTypeOverlay {
                            remoteTypeBar
                                .background(
                                    GeometryReader { barGeo in
                                        Color.clear.preference(key: ViewHeightKey.self, value: barGeo.size.height)
                                    }
                                )
                                .onPreferenceChange(ViewHeightKey.self) { measuredRemoteTypeBarHeight = $0 }
                                .padding(.bottom, 14)
                                // Independent of `boxShift` (which stays 0 while
                                // remote-typing -- see its own doc) so the video
                                // never moves: only this bar rises, by exactly
                                // enough to clear the keyboard from ITS OWN
                                // natural (unshifted) position, using the same
                                // real-measured-height discipline as the command
                                // bar's `measuredBarStackHeight` rather than a
                                // hardcoded guess.
                                .offset(y: -remoteTypeBarShift(boxBottomY: boxCenterY + boxHeight / 2, screenHeight: size.height))
                                .transition(.move(edge: .bottom).combined(with: .opacity))
                        }
                    }
                    .animation(.spring(response: 0.3, dampingFraction: 0.9), value: isControllingRemotely)
                    .animation(.spring(response: 0.3, dampingFraction: 0.9), value: isMacAtLockScreen)
                    .animation(.easeInOut(duration: keyboardAnimationDuration), value: isRemoteTypeFocused)
                    .animation(.spring(response: 0.3, dampingFraction: 0.9), value: showRemoteTypeOverlay)
                    .contentShape(Rectangle())
                    // Pinch-to-zoom and pan-to-drag are attached inside
                    // `PanZoomVideoSurface` itself now (see its doc) -- only
                    // the double-tap/single-tap gestures, which don't touch
                    // the live gesture state, stay attached out here.
                    // Double-tap: reset zoom (attached BEFORE the single-tap so
                    // SwiftUI sequences them; the single-tap then only fires when a
                    // second tap doesn't follow). Also gated off during control -- a
                    // tap on the video while controlling is a remote click
                    // (RemoteControlSurface's own tap), not a local zoom-reset.
                    .gesture(
                        TapGesture(count: 2).onEnded {
                            withAnimation(.spring(response: 0.3, dampingFraction: 0.85)) {
                                resetZoom()
                            }
                        },
                        including: isControllingRemotely ? .none : .all
                    )
                    .gesture(
                        TapGesture().onEnded {
                            if !isVideoFullscreen {
                                withAnimation(.spring(response: 0.35, dampingFraction: 0.85)) {
                                    isVideoFullscreen = true
                                }
                            }
                        },
                        including: isControllingRemotely ? .none : .all
                    )
                    .accessibilityLabel("Live remote view of the Mac")
                    .position(
                        x: size.width / 2,
                        y: isVideoFullscreen ? size.height / 2 : boxCenterY
                    )

                if isVideoFullscreen {
                    VStack(spacing: 0) {
                        Spacer()
                        fullscreenChatOverlay
                    }
                    .transition(.move(edge: .bottom).combined(with: .opacity))
                }
            }
            .allowsHitTesting(true)
        }
    }

    /// Fullscreen bottom overlay: the task-control pill (when relevant) and
    /// the command bar, for sending prompts to the agent without leaving the
    /// mirror. Fullscreen sends go DIRECTLY to the daemon (no Reviewing
    /// detour): fullscreen is the live-driving mode.
    ///
    /// Deliberately does NOT show a log feed here: an earlier "Twitch-style"
    /// stack of the last 5 log entries covered a large fraction of the
    /// screen once a burst of status messages arrived (reconnects, ticket
    /// refreshes, lock-state changes) -- live-reported as the live share
    /// becoming "unviewable" behind piled-up log entries. Full history is
    /// still reachable via the sparkle button's controls sheet
    /// (`logPanel`); fullscreen stays reserved for the video itself.
    private var fullscreenChatOverlay: some View {
        VStack(spacing: 8) {
            if isTaskActive || isTaskPaused {
                taskControlBar
            }
            commandBar(fullscreen: true)
        }
        .padding(.horizontal, 12)
        // Landscape safe-area awareness: in landscape the notch/Dynamic
        // Island (and rounded corners) sit on the LEADING or TRAILING edge
        // instead of the top -- `.safeAreaPadding` (iOS 17+) adds exactly
        // enough extra padding to clear whichever edge actually needs it in
        // the CURRENT orientation, so this overlay never sits under the
        // sensor housing regardless of portrait vs. either landscape.
        .safeAreaPadding(.horizontal)
        .padding(.bottom, 8)
        .background(
            LinearGradient(colors: [.clear, .black.opacity(0.55)], startPoint: .top, endPoint: .bottom)
                .ignoresSafeArea(edges: .bottom)
                .allowsHitTesting(false)
        )
    }

    // MARK: - Status / log panel

    private var logPanel: some View {
        VStack(alignment: .leading, spacing: 4) {
            Text("Status")
                .font(.caption)
                .foregroundStyle(.secondary)
                .padding(.horizontal)

            if logEntries.isEmpty {
                Text("No activity yet")
                    .font(.footnote)
                    .foregroundStyle(.tertiary)
                    .frame(maxWidth: .infinity, alignment: .center)
                    .padding()
            } else {
                ScrollViewReader { proxy in
                    ScrollView {
                        LazyVStack(alignment: .leading, spacing: 6) {
                            ForEach(logEntries) { entry in
                                logRow(entry)
                                    .id(entry.id)
                            }
                        }
                        .padding(.horizontal)
                    }
                    .onChange(of: logEntries.count) {
                        if let lastID = logEntries.last?.id {
                            withAnimation {
                                proxy.scrollTo(lastID, anchor: .bottom)
                            }
                        }
                    }
                }
            }
        }
        .frame(height: 140)
        .background(Color(.secondarySystemBackground))
    }

    private func logRow(_ entry: LogEntry) -> some View {
        HStack(alignment: .top, spacing: 8) {
            Text(entry.formattedTime)
                .font(.system(.caption2, design: .monospaced))
                .foregroundStyle(.tertiary)

            Text(entry.message.kindLabel)
                .font(.system(.caption2, design: .monospaced))
                .fontWeight(.semibold)
                .foregroundStyle(logColor(for: entry.message))
                .frame(width: 64, alignment: .leading)

            Text(entry.message.displayText)
                .font(.caption)
                .foregroundStyle(.primary)
                .lineLimit(3)
                .fixedSize(horizontal: false, vertical: true)

            Spacer(minLength: 0)
        }
        .padding(.vertical, 2)
    }

    private func logColor(for message: ServerMessage) -> Color {
        switch message {
        case .ack: return .secondary
        case .status: return .blue
        case .taskProgress, .taskActive: return .orange
        case .error, .authRejected: return .red
        case .taskDone(let status, _):
            return status == "failed" ? .red : .green
        case .inputRequest: return .yellow
        case .currentTicket: return .blue
        case .clarifyQuestions: return Self.orbAccent
        case .secureInputState: return .blue
        }
    }

    // MARK: - Task controls (stop / pause / redirect surface)

    /// Whether a task is currently live from this screen's point of view --
    /// the states in which the task-control pill (Pause/Stop) is shown and a
    /// sent prompt becomes a REDIRECT of the running task rather than a new
    /// one.
    private var isTaskActive: Bool {
        switch session {
        case .connecting, .working, .inputNeeded: return true
        case .idle, .reviewing, .draftReady, .awaitingApproval, .failed: return false
        }
    }

    /// The always-visible-while-active task control pill: live status label +
    /// Pause/Resume toggle + Stop. This is the main-screen control surface
    /// the PRD 6.1 Working panel's Pause/Cancel buttons used to be the only
    /// home for -- but that panel lives inside the hidden controls sheet, so
    /// mid-task control had no visible affordance at all.
    private var taskControlBar: some View {
        HStack(spacing: 10) {
            HStack(spacing: 6) {
                Circle()
                    .fill(isTaskPaused ? Color.yellow : Color.green)
                    .frame(width: 7, height: 7)
                Text(isTaskPaused ? "Paused" : "Task running")
                    .font(.caption.weight(.medium))
                    .foregroundStyle(.white.opacity(0.85))
            }

            Spacer()

            Button {
                togglePause()
            } label: {
                Label(isTaskPaused ? "Resume" : "Pause",
                      systemImage: isTaskPaused ? "play.fill" : "pause.fill")
                    .font(.caption.weight(.semibold))
                    .padding(.horizontal, 12)
                    .padding(.vertical, 7)
                    .background(Color.white.opacity(0.10), in: Capsule())
            }
            .buttonStyle(.plain)
            .foregroundStyle(.white)
            .accessibilityLabel(isTaskPaused ? "Resume task" : "Pause task")

            Button {
                stopActiveTask()
            } label: {
                Label("Stop", systemImage: "stop.fill")
                    .font(.caption.weight(.semibold))
                    .padding(.horizontal, 12)
                    .padding(.vertical, 7)
                    .background(Color.red.opacity(0.28), in: Capsule())
            }
            .buttonStyle(.plain)
            .foregroundStyle(Color.red.opacity(0.95))
            .accessibilityLabel("Stop task")
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 8)
        .background(Color(white: 0.09).opacity(0.92), in: Capsule())
        .overlay(Capsule().stroke(.white.opacity(0.10), lineWidth: 1))
        .shadow(color: .black.opacity(0.4), radius: 12, y: 4)
    }

    /// The Stop control: remote kill-switch plus local state reset, shared by
    /// the pill and the SessionView Cancel actions.
    private func stopActiveTask() {
        sendStop()
        isTaskPaused = false
        activeInputRequestID = nil
        log(.status(text: "task cancelled"))
        session = .idle
    }

    // MARK: - Command bar

    /// The default (main-screen) command bar. Same input path as before --
    /// prompts are STAGED into the Reviewing panel, not sent directly.
    private var commandBar: some View {
        commandBar(fullscreen: false)
    }

    /// The single bottom command bar in one large dark rounded container:
    /// sparkle (controls-sheet toggle) + prompt field + mic + send.
    /// `fullscreen: true` (the live-mirror overlay) sends prompts DIRECTLY
    /// to the daemon -- fullscreen is the live-driving mode, no Reviewing
    /// detour -- while `false` keeps the existing stage-then-confirm flow.
    /// Accent blue echoing the Spline orb's own coloring, used across the command bar / pairing
    /// flow / saved-profile rows for a cohesive "glowing orb over deep space" look.
    private static let orbAccent = Color(red: 0.30, green: 0.56, blue: 1.0)

    private func commandBar(fullscreen: Bool) -> some View {
        let hasPrompt = !promptText.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
        return VStack(spacing: 8) {
            if !clarifyQuestions.isEmpty {
                ClarifyPanel(
                    questions: clarifyQuestions,
                    onCancel: { cancelClarification() },
                    onContinue: { answers in submitClarification(answers) }
                )
                .transition(.opacity.combined(with: .move(edge: .bottom)))
            } else if isClarifying {
                clarifyThinkingRow
            }
            if RecentPromptStore.container != nil, !isControllingRemotely, clarifyQuestions.isEmpty, !isClarifying {
                RecentPromptsStrip { text in
                    promptText = text
                    isPromptFocused = true
                }
            }
            HStack(spacing: 10) {
            Button {
                showControls.toggle()
            } label: {
                Image(systemName: "sparkles")
                    .font(.system(size: 17, weight: .semibold))
                    .foregroundStyle(
                        LinearGradient(
                            colors: [.white, Color(red: 0.45, green: 0.70, blue: 1.0)],
                            startPoint: .topLeading,
                            endPoint: .bottomTrailing
                        )
                    )
                    .symbolEffect(.bounce, value: showControls)
                    .frame(width: 38, height: 38)
                    .background(
                        showControls ? Self.orbAccent.opacity(0.22) : Color(white: 0.15),
                        in: RoundedRectangle(cornerRadius: 12)
                    )
                    .animation(.easeOut(duration: 0.15), value: showControls)
            }
            .buttonStyle(.plain)
            .accessibilityLabel("Toggle session controls")

            TextField(
                "",
                text: $promptText,
                prompt: Text("What do you want to do?").foregroundStyle(.white.opacity(0.35)),
                axis: .vertical
            )
                .textFieldStyle(.plain)
                .lineLimit(1...4)
                .foregroundStyle(.white)
                .tint(.white)
                .submitLabel(.send)
                .focused($isPromptFocused)
                .padding(.horizontal, 14)
                .padding(.vertical, 10)
                .background(Color(white: 0.13), in: RoundedRectangle(cornerRadius: 14))
                .overlay(
                    RoundedRectangle(cornerRadius: 14)
                        .stroke(.white.opacity(0.08), lineWidth: 1)
                )
                // Escape hatch for the keyboard's own "return"/multi-line
                // behavior swallowing a plain tap on the keyboard's chrome:
                // the shared Done bar above the keyboard, the standard iOS
                // affordance for dismissing a keyboard with no visible
                // return-to-send key (this field uses `axis: .vertical` +
                // multi-line, so "return" inserts a newline, not submit).
                .keyboardDoneToolbar { isPromptFocused = false }
                // ALWAYS direct-send, both modes: the minimal UI hides the
                // Reviewing panel inside the controls sheet, so the old
                // stage-then-confirm flow silently swallowed every prompt
                // (live-witnessed: user typed a Slack task, daemon log shows
                // zero inbound prompts -- it was staged into a panel that is
                // no longer visible). Staged review remains reachable via
                // the sheet's SessionView for flows that enter it there.
                .onSubmit { sendLivePrompt() }

            Button {
                toggleMicrophone()
            } label: {
                Image(systemName: voice.isRecording ? "mic.fill" : "mic")
                    .font(.system(size: 17))
                    .foregroundStyle(voice.isRecording ? Color.red : Color(white: 0.75))
                    .symbolEffect(.pulse, isActive: voice.isRecording)
                    .frame(width: 30, height: 38)
                    .overlay(
                        Circle()
                            .stroke(Color.red.opacity(voice.isRecording ? 0.5 : 0), lineWidth: 1.5)
                    )
            }
            .buttonStyle(.plain)
            .sensoryFeedback(.impact(weight: .light), trigger: voice.isRecording)
            .accessibilityLabel(voice.isRecording ? "Stop recording" : "Start voice prompt")

            Button {
                // Direct send in both modes -- see .onSubmit above for the
                // staged-review-swallowed-prompts bug this closes.
                sendLivePrompt()
            } label: {
                Image(systemName: "paperplane.fill")
                    .font(.system(size: 17))
                    .foregroundStyle(hasPrompt ? Self.orbAccent : Color(white: 0.35))
                    .frame(width: 30, height: 38)
                    .background(Color.white.opacity(0.06), in: Circle())
                    .animation(.easeOut(duration: 0.15), value: hasPrompt)
            }
            .buttonStyle(.plain)
            .disabled(!hasPrompt)
            .accessibilityLabel("Send prompt")
        }
        .padding(12)
        .background(Color(white: 0.09), in: RoundedRectangle(cornerRadius: 24))
        .overlay(
            RoundedRectangle(cornerRadius: 24)
                .stroke(
                    LinearGradient(
                        colors: [.white.opacity(0.16), .white.opacity(0.04)],
                        startPoint: .top,
                        endPoint: .bottom
                    ),
                    lineWidth: 1
                )
        )
        .shadow(color: .black.opacity(0.5), radius: 20, y: 8)
        .shadow(color: Self.orbAccent.opacity(0.18), radius: 24, y: -2)
        }
        .animation(.easeInOut(duration: 0.22), value: isClarifying)
        .animation(.easeInOut(duration: 0.22), value: clarifyQuestions.count)
    }

    /// Compact "generating clarifying questions" indicator shown above the
    /// command bar while the clarify inference is in flight.
    private var clarifyThinkingRow: some View {
        HStack(spacing: 10) {
            ProgressView()
                .controlSize(.small)
                .tint(Color.aroAccentBright)
            Text("Thinking of a few quick questions…")
                .font(.caption)
                .foregroundStyle(.white.opacity(0.7))
            Spacer(minLength: 0)
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 10)
        .background(.ultraThinMaterial, in: Capsule())
        .overlay(Capsule().strokeBorder(Color.white.opacity(0.10), lineWidth: 1))
        .transition(.opacity)
    }

    /// Debug-only unattended witness: fires exactly once per process
    /// launch, the moment the control channel reaches `.connected`, sending
    /// a real prompt through the exact same `sendLivePrompt()` path a tap on
    /// the send button uses. Exists for `holoiroh-redeploy-iphone-fix`'s
    /// device-witness step (a real `devicectl` launch has no way to tap the
    /// on-screen UI) and any future device-only regression check -- reads
    /// `HOLOIROH_AUTOPAIR_PROMPT` (paired with `ContentView`'s
    /// `HOLOIROH_AUTOPAIR_TICKET`/`_PIN`, same DEBUG-only gate and rationale).
    private func sendAutoPairPromptIfNeeded() {
        #if DEBUG
        guard !didSendAutoPairPrompt,
              let prompt = ProcessInfo.processInfo.environment["HOLOIROH_AUTOPAIR_PROMPT"],
              !prompt.isEmpty
        else { return }
        didSendAutoPairPrompt = true
        promptText = prompt
        sendLivePrompt()
        #endif
    }

    /// Debug-only unattended witness for keyboard-avoidance UI changes (the
    /// live-share box/command bar shifting up as the keyboard opens):
    /// `devicectl`/`simctl` have no real UI-input capability to simulate a
    /// tap on the prompt field, so `HOLOIROH_AUTOFOCUS_PROMPT=1` focuses it
    /// programmatically on appear -- the exact same `isPromptFocused = true`
    /// a real tap would trigger, driving the real keyboard notifications and
    /// therefore the real keyboard-avoidance animation in `body`. Independent of
    /// pairing/connection state (unlike `sendAutoPairPromptIfNeeded`, which
    /// needs `.connected`) since this only exercises the layout, not a send.
    private func autoFocusPromptIfNeeded() {
        #if DEBUG
        // Deterministic orb-reaction witness: trigger the full reaction
        // (pulse + glow + orbiting badges parsed from the given text) with
        // no connection needed, on a long window so screenshot bursts can
        // catch the badges at multiple orbital angles.
        if let reactText = ProcessInfo.processInfo.environment["HOLOIROH_DEBUG_ORB_REACT"],
           !reactText.isEmpty {
            DispatchQueue.main.asyncAfter(deadline: .now() + 1.0) {
                orbEffects.react(to: reactText, duration: 20)
            }
        }
        if let heightStr = ProcessInfo.processInfo.environment["HOLOIROH_DEBUG_KEYBOARD_HEIGHT"],
           let height = Double(heightStr) {
            // Isolated layout-only witness: sets keyboardHeight directly,
            // bypassing @FocusState/UIResponder notifications entirely, to
            // verify the barShift/boxShift math/animation independent of whether
            // the simulator's software keyboard or focus plumbing behaves.
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.5) {
                keyboardHeight = CGFloat(height)
            }
            return
        }
        if ProcessInfo.processInfo.environment["HOLOIROH_FRAME_TIMING_PROBE"] == "1" {
            DispatchQueue.main.asyncAfter(deadline: .now() + 1.0) {
                runFrameTimingProbe()
            }
        }
        guard ProcessInfo.processInfo.environment["HOLOIROH_AUTOFOCUS_PROMPT"] == "1" else { return }
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.3) {
            isPromptFocused = true
        }
        #endif
    }

    #if DEBUG
    /// Empirical witness for the video pan/zoom performance investigation --
    /// see `FrameTimingProbe.swift`'s doc for the full caveat: this drives
    /// `zoomScale`/`panOffset` (the `MainView`-owned `@State` `PanZoomVideoSurface`
    /// receives as a `Binding`), NOT `pinchScale`/`panDrag` (the `@GestureState`
    /// that actually moved into that child and is what a real pinch/pan ticks),
    /// so it does not isolate that specific optimization -- read its numbers
    /// as raw data about a different code path, not as before/after proof.
    /// Runs for `HOLOIROH_FRAME_TIMING_PROBE_SECONDS` seconds (default 4)
    /// while a `CADisplayLink` measures actual per-frame cost. Zoom/pan are
    /// restored to identity afterward so the probe never leaves the view
    /// visibly altered. Triggered once per launch by
    /// `HOLOIROH_FRAME_TIMING_PROBE=1`.
    private func runFrameTimingProbe() {
        let seconds = Double(ProcessInfo.processInfo.environment["HOLOIROH_FRAME_TIMING_PROBE_SECONDS"] ?? "") ?? 4.0
        let probe = FrameTimingProbe(label: "video-pan-zoom") { _ in }
        frameTimingProbe = probe
        probe.start()

        let start = Date()
        // ~120Hz drive interval -- matches ProMotion touch-tracking rate, the
        // upper end of what a real pinch/pan's @GestureState updates at.
        let timer = Timer.scheduledTimer(withTimeInterval: 1.0 / 120.0, repeats: true) { t in
            let elapsed = Date().timeIntervalSince(start)
            guard elapsed < seconds else {
                t.invalidate()
                frameTimingDriveTimer = nil
                probe.stop()
                frameTimingProbe = nil
                withAnimation(nil) {
                    zoomScale = 1
                    panOffset = .zero
                }
                return
            }
            // A small continuous oscillation in both zoom and pan -- exercises
            // the identical clampZoom/clampedPan + liveScale/liveOffset path a
            // real pinch-then-drag gesture would, at a steady synthetic rate.
            let t2 = elapsed * 2 * .pi
            zoomScale = 1.5 + 0.3 * CGFloat(sin(t2 * 0.5))
            panOffset = CGSize(
                width: 40 * CGFloat(sin(t2)),
                height: 40 * CGFloat(cos(t2))
            )
        }
        frameTimingDriveTimer = timer
    }
    #endif

    /// Live send: straight to the daemon, straight into the feed -- no
    /// Reviewing stage. While a task is ACTIVE, a sent prompt REDIRECTS it
    /// (the daemon cancels the running turn, keeps its session history, and
    /// runs the new instruction) rather than queueing a second task behind
    /// it; from idle it starts a fresh task. Either way the session enters
    /// `.connecting` so the task controls appear and the daemon's own
    /// `task_progress`/`task_done` frames drive it from there.
    /// The Take-control / Release-control toggle shown on the live view.
    private var remoteControlToggle: some View {
        Button {
            toggleRemoteControl()
        } label: {
            Image(systemName: isControllingRemotely ? "hand.raised.fill" : "hand.point.up.left")
                .font(.footnote.weight(.bold))
                .foregroundStyle(isControllingRemotely ? Color.orange : .white)
                .padding(10)
                .background(.ultraThinMaterial, in: Circle())
        }
        .padding(10)
        // Landscape safe-area awareness: this toggle is pinned `.topLeading`
        // on the video (see `videoOverlay`) -- in landscape the LEADING edge
        // may be the notch/Dynamic Island side depending on which way the
        // phone is rotated, so it needs the same safe-area-aware padding as
        // `fullscreenChatOverlay`.
        .safeAreaPadding(.leading)
        // Take-control haptic, gated by the diagnostics "Haptics" toggle.
        .sensoryFeedback(trigger: isControllingRemotely) { _, _ in
            Haptics.isEnabled ? .impact(weight: .medium) : nil
        }
        .accessibilityLabel(isControllingRemotely ? "Release control" : "Take control of the Mac")
    }

    /// Floating toggle (task 4) for the lightweight typing overlay -- only
    /// ever shown while `isControllingRemotely`, paired top-trailing against
    /// `remoteControlToggle`'s top-leading position.
    @ViewBuilder
    private var remoteTypeToggle: some View {
        if isControllingRemotely {
            Button {
                withAnimation(.spring(response: 0.3, dampingFraction: 0.9)) {
                    showRemoteTypeOverlay.toggle()
                }
                if showRemoteTypeOverlay {
                    isRemoteTypeFocused = true
                }
            } label: {
                Image(systemName: showRemoteTypeOverlay ? "keyboard.chevron.compact.down" : "keyboard")
                    .font(.footnote.weight(.bold))
                    .foregroundStyle(showRemoteTypeOverlay ? Color.aroAccentBright : .white)
                    .padding(10)
                    .background(.ultraThinMaterial, in: Circle())
            }
            .padding(10)
            .safeAreaPadding(.trailing)
            // Shifted below the fullscreen toggle (also `.topTrailing`, always visible) so the
            // two buttons stack instead of overlapping while `isControllingRemotely` is true --
            // live-reported: the keyboard button sat directly on top of the fullscreen button.
            .offset(y: 54)
            .accessibilityLabel(showRemoteTypeOverlay ? "Hide typing keyboard" : "Type on the remote screen")
        }
    }

    /// The lightweight, translucent, non-blocking floating text-entry bar
    /// (task 4): shown while controlling remotely AND `showRemoteTypeOverlay`
    /// is on. Bound to the SAME `promptText`/`sendLivePrompt()` path the
    /// regular command bar uses -- `sendLivePrompt`'s existing
    /// `isControllingRemotely` branch already routes typed text as
    /// `.remoteControl(.text(...))`, so this overlay is purely an
    /// alternative, more-discoverable UI surface over that one mechanism,
    /// never a duplicate send path. Bounded height (a single-line field, not
    /// the multi-line command-bar editor) so it only ever covers a small
    /// strip of the live view.
    ///
    /// Deliberately styled in the SAME orange this app already uses for every
    /// other "you are actively remote-controlling" signal (the toggle icon,
    /// the "You're in control" banner) -- a leading cursor glyph plus an
    /// orange-tinted border, instead of the command bar's neutral white/gray
    /// chrome -- so a glance distinguishes "typing into the remote screen"
    /// from "composing a new agent task" even before reading the placeholder
    /// text. Its keyboard-rise is ALSO independently animated from the
    /// command bar's (see `remoteTypeBarShift`'s doc) -- the two typing
    /// interactions look and move differently on purpose.
    private var remoteTypeBar: some View {
        HStack(spacing: 10) {
            Image(systemName: "cursorarrow.rays")
                .font(.system(size: 14, weight: .semibold))
                .foregroundStyle(Color.orange)

            TextField(
                "",
                text: $promptText,
                prompt: Text("Type into the remote screen…").foregroundStyle(.white.opacity(0.4))
            )
            .textFieldStyle(.plain)
            .foregroundStyle(.white)
            .tint(Color.aroAccentBright)
            .focused($isRemoteTypeFocused)
            .submitLabel(.send)
            .onSubmit { sendLivePrompt() }

            Button {
                sendLivePrompt()
            } label: {
                Image(systemName: "arrow.up.circle.fill")
                    .font(.system(size: 22))
                    .foregroundStyle(
                        promptText.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
                            ? .white.opacity(0.3) : Color.aroAccentBright
                    )
            }
            .disabled(promptText.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)

            Button {
                isRemoteTypeFocused = false
                withAnimation(.spring(response: 0.3, dampingFraction: 0.9)) {
                    showRemoteTypeOverlay = false
                }
            } label: {
                Image(systemName: "xmark.circle.fill")
                    .font(.system(size: 18))
                    .foregroundStyle(.white.opacity(0.5))
            }
            .accessibilityLabel("Dismiss typing keyboard")
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 10)
        .background(.ultraThinMaterial, in: RoundedRectangle(cornerRadius: 16, style: .continuous))
        .overlay(
            RoundedRectangle(cornerRadius: 16, style: .continuous)
                .strokeBorder(Color.orange.opacity(0.55), lineWidth: 1.5)
        )
        .padding(.horizontal, 24)
        .safeAreaPadding(.horizontal)
        .shadow(color: .black.opacity(0.4), radius: 14, y: 4)
    }

    /// Enter/exit hands-on control: send `takeControl`/`releaseControl`, and on
    /// entry reset zoom/pan so touch coordinates map 1:1 to the video (the
    /// daemon pauses any active agent turn on `takeControl`).
    private func toggleRemoteControl() {
        isControllingRemotely.toggle()
        if isControllingRemotely {
            withAnimation(.easeOut(duration: 0.2)) {
                zoomScale = 1
                panOffset = .zero
            }
            sendControlMessage(.remoteControl(.takeControl))
            log(.status(text: "\u{2192} took control of the Mac"))
        } else {
            sendControlMessage(.remoteControl(.releaseControl))
            log(.status(text: "\u{2192} released control"))
            // The typing overlay only ever makes sense while controlling --
            // never leave it floating over a video the user is no longer
            // driving directly.
            isRemoteTypeFocused = false
            showRemoteTypeOverlay = false
        }
    }

    private func sendLivePrompt() {
        let trimmed = promptText.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return }
        // While in hands-on control the command bar types INTO the Mac (as remote
        // keyboard input) rather than prompting the agent; keep focus so the user
        // can keep typing.
        if isControllingRemotely {
            promptText = ""
            sendControlMessage(.remoteControl(.text(trimmed)))
            log(.status(text: "\u{2192} typed to Mac: \(trimmed)"))
            return
        }
        promptText = ""
        isPromptFocused = false
        // Clarify intercept: for a fresh task prompt, ask the daemon for
        // clarifying questions first (opt-out). Redirects/active tasks, an
        // in-flight clarify, or a disabled toggle fall straight through to a
        // direct send -- the exact prior behavior.
        if clarifyEnabled, !isTaskActive, !isTaskPaused, pendingClarifyPrompt == nil {
            beginClarify(trimmed)
            return
        }
        dispatchPrompt(trimmed)
    }

    /// Sends `trimmed` as the real prompt/redirect (the original send path).
    private func dispatchPrompt(_ trimmed: String) {
        lastSentTask = .prompt(text: trimmed)
        // Best-effort recent-prompts history (SwiftData); never blocks the send.
        RecentPromptsRepository().record(trimmed)
        // The orb visibly reacts to every send; apps named in the prompt
        // orbit it while it "thinks" (OrbEffects.swift).
        orbEffects.react(to: trimmed)
        if isTaskActive || isTaskPaused {
            sendControlMessage(.redirect(text: trimmed))
            log(.status(text: "→ redirect: \(trimmed)"))
        } else {
            sendControlMessage(.prompt(text: trimmed))
            log(.status(text: "→ live prompt: \(trimmed)"))
        }
        isTaskPaused = false
        activeInputRequestID = nil
        session = .connecting
    }

    /// Kicks off clarification for `prompt`: sends a ClarifyRequest, shows the
    /// thinking state, and starts a timeout that falls back to a direct send so
    /// a prompt is never lost if the daemon never answers.
    private func beginClarify(_ prompt: String) {
        pendingClarifyPrompt = prompt
        clarifyQuestions = []
        isClarifying = true
        orbEffects.react(to: prompt)
        sendControlMessage(.clarifyRequest(prompt: prompt))
        log(.status(text: "→ clarifying: \(prompt)"))
        DispatchQueue.main.asyncAfter(deadline: .now() + 25) {
            guard isClarifying, pendingClarifyPrompt == prompt, clarifyQuestions.isEmpty else { return }
            log(.status(text: "clarify timed out — sending directly"))
            isClarifying = false
            pendingClarifyPrompt = nil
            dispatchPrompt(prompt)
        }
    }

    /// The daemon answered a ClarifyRequest. Empty -> the prompt was clear, send
    /// it directly. Non-empty -> present the panel for the user to answer.
    private func applyClarifyQuestions(_ questions: [ClarifyingQuestion]) {
        guard let prompt = pendingClarifyPrompt else { return }
        isClarifying = false
        if questions.isEmpty {
            pendingClarifyPrompt = nil
            dispatchPrompt(prompt)
        } else {
            clarifyQuestions = questions
        }
    }

    /// Continue tapped in the panel: compose the clarified prompt + send it.
    private func submitClarification(_ answers: [(question: String, answer: String)]) {
        guard let prompt = pendingClarifyPrompt else { return }
        let clarified = ClarifyComposer.compose(original: prompt, answers: answers)
        pendingClarifyPrompt = nil
        clarifyQuestions = []
        dispatchPrompt(clarified)
    }

    /// Dismiss the panel without running anything (the original prompt is
    /// dropped; the user can retype).
    private func cancelClarification() {
        pendingClarifyPrompt = nil
        clarifyQuestions = []
        isClarifying = false
    }

    private func toggleMicrophone() {
        // Real on-device transcription via `VoiceTranscriberModel` (Speech
        // framework). `ClientMessage.voiceTranscript` control-channel send
        // is still a later task -- today the final transcript just lands in
        // `promptText`, same as if the user had typed it.
        let wasRecording = voice.isRecording
        Task {
            await voice.toggle()
        }
        log(.status(text: wasRecording ? "stopped listening" : "listening…"))
    }

    // MARK: - Control-channel send helpers

    /// Sends one `ClientMessage` to the daemon through the control-channel seam
    /// (`controlChannel`). The seam encodes it to its `PROTOCOL.md` NDJSON wire
    /// form and (today) surfaces it in the log panel; the real `iroh` transport
    /// drops in behind this same call later. This is the single place the UI
    /// hands a message to the transport, so every outbound message -- the
    /// kill-switch `.stop`, prompts, transcripts -- goes through one path.
    private func sendControlMessage(_ message: ClientMessage) {
        controlChannel.send(message)
    }

    /// Sends the remote kill-switch `ClientMessage.stop`. Wired to every Cancel
    /// control (Working/Connecting/Input-needed/Draft-ready) -- see
    /// `sessionActions.cancel`. On the daemon this maps to
    /// `ControlMessage::Stop { context_id: None }` and engages the global
    /// `holo stop` kill switch (`mac-daemon`'s `HoloControlBridge::handle_stop`).
    private func sendStop() {
        sendControlMessage(.stop)
    }

    // MARK: - Log helper

    /// Newest-N ring cap for the status log.
    ///
    /// `logEntries` had five append sites and no removals, so it grew for the
    /// entire life of a session — which on this product is the steady state, not
    /// an edge case: people leave the mirror open. Every append also re-evaluates
    /// this view's body, so an ever-longer array made that steadily more
    /// expensive. 200 is far more history than the UI ever shows (the controls
    /// sheet scrolls, the removed fullscreen feed showed 5) while making the cost
    /// constant instead of unbounded.
    private static let maxLogEntries = 200

    private func log(_ message: ServerMessage) {
        logEntries.append(LogEntry(message: message))
        if logEntries.count > Self.maxLogEntries {
            logEntries.removeFirst(logEntries.count - Self.maxLogEntries)
        }
    }
}

// MARK: - Representative demo payloads

private extension MainView {
    static let demoReview = ReviewPayload(
        transcript: "Send the design team a note that the launch review moved to Thursday.",
        destination: "Slack › #design",
        dictatedText: "Heads up — the launch review moved to Thursday at 2pm. Same room."
    )

    static let demoWorking = WorkingPayload(
        app: "Slack",
        status: "navigating to #design",
        lastAction: "clicked the channel switcher",
        nextAction: "open the #design channel and focus the composer"
    )

    static let demoInputNeeded = InputRequestPayload(
        kind: .credentialNeeded,
        whatIsNeeded: "Your Slack sign-in on this Mac has expired.",
        why: "Slack is showing a login wall, so the message can't be drafted until you're signed back in.",
        currentFrame: "Slack login wall (email + password fields)",
        responseOptions: []
    )

    static let demoDraft = DraftPayload(
        target: "Slack › #design › message composer",
        draftSummary: "Heads up — the launch review moved to Thursday at 2pm. Same room.",
        verification: "Matches your request: mentions the launch review and the new Thursday time. Nothing has been sent."
    )

    static let demoApproval = ApprovalPayload(
        destination: "Slack › #design",
        text: "Heads up — the launch review moved to Thursday at 2pm. Same room.",
        frame: "Composer focused, message typed, Send button visible",
        commitmentDescription: "Send this message to the #design channel."
    )

    static let demoFailure = FailurePayload(
        cause: "Couldn't find the #design channel — it may have been renamed or archived.",
        recovery: "Retry to search again, or take control to pick the channel manually."
    )
}

#Preview("Main - idle") {
    NavigationStack {
        MainView(ticket: "iroh-live:example-ticket", pin: "123456", onDisconnect: {})
    }
    .environmentObject(ConnectionProfileStore())
}

import SwiftUI
#if canImport(UIKit)
import UIKit
#endif
#if canImport(AppKit)
import AppKit
#endif

/// Displays the task dashboard after pairing and opens the shared bridge connection.
///
/// One `SessionState` controls the Product Requirements Document (PRD) 6.1 dashboard panel:
///
/// - Idle provides task entry and Mac availability.
/// - Reviewing confirms transcript, destination, and dictated text.
/// - Connecting supports cancellation.
/// - Working shows the media stream and task status.
/// - Input needed collects required user input.
/// - Draft ready supports review and send requests.
/// - Awaiting approval confirms the final action.
/// - Failed provides recovery controls.
///
/// The prompt bar remains available in all states.
/// The controls sheet contains `SessionView`, status history, demo controls, and disconnection.
///
/// When the bridge is linked, `HoloConnection` provides the media stream and control channel.
/// Bridge-less builds use the synthetic frame source and logging sender.
struct MainView: View {
    /// Provides the ticket for this paired session.
    let ticket: String

    /// Provides the pairing personal identification number (PIN) for the control-channel handshake.
    /// An empty value supports a daemon that does not require a PIN.
    let pin: String

    /// Returns the app to `PairingView`.
    let onDisconnect: () -> Void

    /// Provides the profile store that saves a rotated ticket received from the daemon.
    @EnvironmentObject private var profileStore: ConnectionProfileStore
    @EnvironmentObject private var tinfoilVerificationStore: TinfoilVerificationStore

    // MARK: - Dashboard identity fields (PRD 6.1 dashboard row)

    /// Defines the paired Mac name shown in dashboard states.
    private let macName = "Studio Mac"
    /// Indicates paired Mac availability. This value is currently constant.
    private let macAvailable = true
    /// Defines the active inference mode shown by the dashboard.
    private let inferenceMode = "Aro Private (local)"

    // MARK: - State

    /// Stores the current PRD 6.1 dashboard state.
    @State private var session: SessionState = .idle

    /// Owns one bridge connection for the media stream and control channel.
    /// `onAppear` connects it with this session ticket and PIN.
    @StateObject private var connection = HoloConnection()

    /// Drives the orb pulse and app badges after each real send.
    @StateObject private var orbEffects = OrbEffectsState()

    /// Stores the last task message so the Failed panel can retry it.
    @State private var lastSentTask: ClientMessage?

    /// Indicates whether the active task is paused.
    /// Prevents a pause-related canceled status from ending the displayed task.
    @State private var isTaskPaused = false

    /// Stores the daemon request identifier for the active input request.
    @State private var activeInputRequestID: String?
    @State private var pendingApproval: ApprovalRequest?
    @State private var presentedApprovalForCancellation: ApprovalRequest?

    @State private var promptText: String = ""
    @State private var autonomousExecutionPermitted = false
    @State private var executionMode = "restricted"

    // MARK: - Clarify (ad-hoc clarifying questions)

    /// Controls whether the app requests clarification before a new ambiguous prompt. The default is `true`.
    @AppStorage("clarifyEnabled") private var clarifyEnabled = true
    /// Stores the prompt awaiting clarification. A response, cancellation, empty result, or timeout clears it.
    @State private var pendingClarifyPrompt: String?
    /// Stores daemon clarification questions. A nonempty array displays `ClarifyPanel`.
    @State private var clarifyQuestions: [ClarifyingQuestion] = []
    /// Indicates that clarification inference is in progress.
    @State private var isClarifying = false

    // MARK: - Tinfoil-backed features (document/image/audio/planner)

    @AppStorage(AppSettings.TinfoilAudio.storageKey)
    private var tinfoilAudioEnabled = AppSettings.TinfoilAudio.enabledByDefault
    @StateObject private var tinfoilAudioRecorder = TinfoilAudioRecorder()
    @StateObject private var speechPlayback = SpeechPlaybackController()

    @State private var showDocumentAttachSheet = false
    @State private var showImageAttachSheet = false
    @State private var showPlanSheet = false
    @State private var showTinfoilRecordSheet = false

    @State private var tinfoilRequestID = UUID().uuidString
    @State private var tinfoilVerificationSessionID = UUID()
    @State private var documentResult: String?
    @State private var documentError: String?
    @State private var imageResult: String?
    @State private var imageError: String?
    @State private var transcriptionResult: String?
    @State private var transcriptionError: String?
    @State private var planSteps: [String]?
    @State private var planError: String?

    /// Prevents the debug automatic-pairing prompt from sending more than once per process launch.
    @State private var didSendAutoPairPrompt = false

    /// Tracks focus for the main prompt field.
    /// Clearing this value dismisses the keyboard.
    /// Sends also clear it.
    @FocusState private var isPromptFocused: Bool

    /// Tracks the keyboard height from UIKit keyboard notifications.
    /// This value reflects the current keyboard frame for the device and orientation.
    @State private var keyboardHeight: CGFloat = 0

    /// Stores the measured height of the bottom command stack.
    /// The initial 96-point value covers the interval before measurement.
    @State private var measuredBarStackHeight: CGFloat = 96

    /// Stores the measured height of `remoteTypeBar`.
    /// The initial value is 56 points.
    @State private var measuredRemoteTypeBarHeight: CGFloat = 56

    /// Stores the current UIKit keyboard animation duration.
    /// The default is 0.25 seconds when a notification omits the value.
    @State private var keyboardAnimationDuration: Double = 0.25
    @StateObject private var voice = VoiceTranscriberModel()
    @State private var logEntries: [LogEntry] = [
        LogEntry(message: .status(text: "paired -- control channel not yet connected"))
    ]

    /// Indicates whether the media stream is fullscreen.
    /// Layout changes preserve one `VideoRenderView` identity.
    @State private var isVideoFullscreen = false

    /// Prevents automatic fullscreen from overriding a manual collapse during the current landscape interval.
    /// Portrait orientation clears this value.
    @State private var manuallyCollapsedWhileLandscape = false

    /// Indicates that the user controls the Mac through the media stream.
    /// Entering this mode pauses the active daemon task.
    @State private var isControllingRemotely = false

    /// Indicates that macOS secure input is active.
    /// The app explains the resulting protected region in the media stream.
    @State private var isMacAtLockScreen = false

    /// Controls the floating typing overlay during hands-on remote control.
    @State private var showRemoteTypeOverlay = false
    /// Tracks focus for the floating remote-typing field.
    @FocusState private var isRemoteTypeFocused: Bool

    /// Controls the sheet that contains session controls, status history, and disconnection.
    @State private var showControls = false

    /// Provides app lifecycle state for media-stream recovery.
    @Environment(\.scenePhase) private var scenePhase

    // MARK: Live-share zoom (pinch to zoom, drag to pan, double-tap resets)

    /// Stores the committed media-stream zoom factor.
    /// `PanZoomVideoSurface` owns the in-progress pinch state.
    @State private var zoomScale: CGFloat = 1
    /// Stores the committed pan offset in post-scale points.
    @State private var panOffset: CGSize = .zero

    /// Retains the debug frame-timing probe until measurement completes.
    #if DEBUG && canImport(UIKit)
    @State private var frameTimingProbe: FrameTimingProbe?
    @State private var frameTimingDriveTimer: Timer?
    #endif
    #if DEBUG
    @State private var didRunTakeControlWitness = false
    #endif

    // MARK: Auto-reconnect (app-switch/backgrounding recovery)

    /// Counts consecutive automatic reconnect attempts since the last connection.
    /// `Self.maxReconnectAttempts` bounds retries.
    @State private var reconnectAttempts = 0
    /// Stores when the app last left the foreground.
    @State private var lastBackgroundedAt: Date?
    /// Requires 30 seconds outside the foreground before restoring the reconnect budget.
    private static let reconnectBudgetRefreshAfter: TimeInterval = 30
    /// Prevents overlapping failure and foreground triggers from starting multiple reconnects.
    @State private var isReconnecting = false
    /// Identifies the current foreground liveness check. Superseded checks do nothing.
    @State private var livenessCheckID = UUID()
    private static let maxReconnectAttempts = 5

    /// Provides frames to the media-stream view with stable source identity.
    /// The synthetic source remains active until a bridge connection supplies the live source.
    @State private var frameSource: VideoFrameSource = SyntheticVideoFrameSource()

    /// Provides the single outbound control-channel path.
    /// Connected sessions use `FFIControlChannelSender`.
    /// Other sessions use `LoggingControlChannelSender`.
    private var controlChannel: ControlChannelSending {
        if let real = connection.controlSender {
            return real
        }
        return LoggingControlChannelSender { message, wire in
            // Surface bounded metadata in the same status/log panel every other
            // event flows through. Routed through `log` (not a direct append) so
            // it is subject to the same ring cap; a disconnected remote-control
            // drag would otherwise append at touch frequency with nothing trimming it.
            log(.status(text: "→ sent (not connected) \(message.wireKindLabel): \(wire.utf8.count) bytes"))
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
        #if os(iOS)
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
        #endif
        #if os(iOS)
        .toolbar(.hidden, for: .navigationBar)
        #endif
        // Hidden-by-default controls: the SessionView state panel, the
        // status/log panel, and Disconnect live in this sheet, toggled by
        // the command bar's sparkle button.
        .sheet(
            item: $pendingApproval,
            onDismiss: { cancelPendingApprovalIfNeeded() }
        ) { request in
            ApprovalSheet(request: request) { decision in
                respondToApproval(request, decision: decision)
            }
        }
        .sheet(isPresented: $showControls) {
            controlsSheet
        }
        .sheet(
            isPresented: $showDocumentAttachSheet,
            onDismiss: { invalidateTinfoilRequest() }
        ) {
            TinfoilAttachSheet(
                mode: .document,
                onSend: { sendTinfoilMessage($0) },
                resultText: $documentResult,
                resultError: $documentError,
                requestId: tinfoilRequestID
            )
        }
        .sheet(
            isPresented: $showImageAttachSheet,
            onDismiss: { invalidateTinfoilRequest() }
        ) {
            TinfoilAttachSheet(
                mode: .image,
                onSend: { sendTinfoilMessage($0) },
                resultText: $imageResult,
                resultError: $imageError,
                requestId: tinfoilRequestID
            )
        }
        .sheet(
            isPresented: $showPlanSheet,
            onDismiss: { invalidateTinfoilRequest() }
        ) {
            PlanStepsSheet(
                onSend: { sendTinfoilMessage($0) },
                onRunStep: { dispatchPrompt($0) },
                autonomousExecutionPermitted: autonomousExecutionPermitted,
                steps: $planSteps,
                planError: $planError,
                requestId: tinfoilRequestID
            )
        }
        .sheet(
            isPresented: $showTinfoilRecordSheet,
            onDismiss: { invalidateTinfoilRequest() }
        ) {
            TinfoilRecordSheet(
                recorder: tinfoilAudioRecorder,
                onSend: { capture in
                    transcriptionResult = nil
                    transcriptionError = nil
                    sendTinfoilMessage(.transcribeAudio(
                        requestId: tinfoilRequestID,
                        capture: capture
                    ))
                },
                resultText: $transcriptionResult,
                resultError: $transcriptionError
            )
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
        .onDisappear {
            cancelPendingApprovalIfNeeded()
            tinfoilVerificationStore.reset()
            invalidateTinfoilRequest()
            connection.shutdown()
        }
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
            if newPhase != .active {
                // Stamp the first non-active transition only, so a quick
                // Control Center peek does not look like a long absence.
                if lastBackgroundedAt == nil { lastBackgroundedAt = Date() }
                return
            }
            // Replenish the reconnect budget, but only after a real absence.
            //
            // The budget was otherwise reset ONLY by a successful connect, so a
            // single window where the Mac was unreachable (asleep, off Wi-Fi)
            // burned every attempt and left the session permanently unable to
            // reconnect for the rest of its life — force-quitting the app being
            // the only escape.
            //
            // Deliberately NOT reset on every activation: `.active` also fires
            // when a notification banner or Control Center is dismissed, and
            // resetting there would erase the bound the retry budget exists to
            // provide, letting the app hammer a Mac that is genuinely gone. A
            // minimum gap means only an actual return-to-the-app grants a fresh
            // budget. Explicit user intent (the Try again button) bypasses this.
            let now = Date()
            if let left = lastBackgroundedAt, now.timeIntervalSince(left) >= Self.reconnectBudgetRefreshAfter {
                reconnectAttempts = 0
            }
            lastBackgroundedAt = nil
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

    /// Calculates how far the remote typing bar must rise above the keyboard.
    /// The media-stream box remains stationary.
    /// The result uses measured bar height and an 8-point gap.
    private func remoteTypeBarShift(boxBottomY: CGFloat, screenHeight: CGFloat) -> CGFloat {
        guard isRemoteTypeFocused, keyboardHeight > 0 else { return 0 }
        let naturalBottomY = boxBottomY + 14 + measuredRemoteTypeBarHeight
        let keyboardTopY = screenHeight - keyboardHeight
        return max(0, naturalBottomY - keyboardTopY + 8)
    }

    /// Reads a positive animation duration from a UIKit keyboard notification. Missing values preserve the current duration.
    #if os(iOS)
    private func captureKeyboardAnimationDuration(from note: Notification) {
        guard let duration = note.userInfo?[UIResponder.keyboardAnimationDurationUserInfoKey] as? Double,
              duration > 0
        else { return }
        keyboardAnimationDuration = duration
    }
    #endif

    // MARK: - Live-share zoom helpers

    private func resetZoom() {
        zoomScale = 1
        panOffset = .zero
    }

    // MARK: - Auto-reconnect

    /// Checks for a frame within 4 seconds after foreground restart.
    /// If no frame arrives, the app starts a full reconnect.
    /// Only the latest check can act.
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

    /// Resets the failed session and reconnects with the current ticket and PIN.
    /// Retries are bounded and use increasing delays.
    private func attemptReconnect(reason: String) {
        guard !isReconnecting else { return }
        guard reconnectAttempts < Self.maxReconnectAttempts else {
            log(.error(text: "reconnect failed after \(Self.maxReconnectAttempts) attempts -- use Disconnect and pair again"))
            return
        }
        beginTinfoilVerificationSession()
        invalidateTinfoilRequest()
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

    /// Displays the rounded media-stream container with a 28-point corner radius.
    /// Before connection, it shows status and retry controls.
    /// After connection, `videoOverlay` supplies the media stream.
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
                        } else if case .failed = connection.phase {
                            // A failed connection previously rendered the same
                            // passive "Appears when your Mac connects" as idle,
                            // so the one state the user needs to act on looked
                            // identical to the one that needs no action, with no
                            // visible way to retry. This is the only surface that
                            // reports it — there is no banner or pill elsewhere.
                            Text("Can't reach your Mac")
                                .font(.caption.weight(.semibold))
                                .foregroundStyle(.white.opacity(0.75))
                            Text("Make sure it's awake and on the same network.")
                                .font(.caption2)
                                .foregroundStyle(.white.opacity(0.5))
                                .multilineTextAlignment(.center)
                            Button {
                                // An explicit tap is a fresh intent, so it also
                                // clears the bounded-retry budget.
                                reconnectAttempts = 0
                                attemptReconnect(reason: "manual retry")
                            } label: {
                                Text("Try again")
                                    .font(.caption.weight(.semibold))
                                    .padding(.horizontal, 14)
                                    .padding(.vertical, 7)
                                    .background(Self.orbAccent.opacity(0.9), in: Capsule())
                                    .foregroundStyle(.white)
                            }
                            .padding(.top, 2)
                            .accessibilityLabel("Try connecting to your Mac again")
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

    /// Displays demo controls, `SessionView`, status history, and the Disconnect action. The sheet is hidden by default.
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
                    tinfoilVerificationStore.reset()
                    invalidateTinfoilRequest()
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

    /// Provides representative payloads for direct navigation to each PRD 6.1 state.
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

    // MARK: - Tinfoil attach menu

    /// Provides Tinfoil document, image, audio, and planner actions.
    /// Each opening creates a new request identifier.
    private var tinfoilAttachMenu: some View {
        Menu {
            Button {
                tinfoilRequestID = UUID().uuidString
                documentResult = nil
                documentError = nil
                showDocumentAttachSheet = true
            } label: {
                Label("Attach Document", systemImage: "doc")
            }
            Button {
                tinfoilRequestID = UUID().uuidString
                imageResult = nil
                imageError = nil
                showImageAttachSheet = true
            } label: {
                Label("Attach Image", systemImage: "photo")
            }
            Button {
                tinfoilRequestID = UUID().uuidString
                planSteps = nil
                planError = nil
                showPlanSheet = true
            } label: {
                Label("Plan a Task", systemImage: "list.bullet.rectangle")
            }
            if tinfoilAudioEnabled {
                Button {
                    tinfoilRequestID = UUID().uuidString
                    showTinfoilRecordSheet = true
                } label: {
                    Label("Record & Transcribe", systemImage: "waveform")
                }
            }
        } label: {
            Image(systemName: "plus.circle")
                .font(.system(size: 17))
                .foregroundStyle(Color(white: 0.75))
                .frame(width: 30, height: 38)
        }
        .accessibilityLabel("Attach or plan with Tinfoil")
    }

    /// Sends a Tinfoil-backed message through the control channel and records its message kind.
    private func sendTinfoilMessage(_ message: ClientMessage) {
        sendControlMessage(message)
        log(.status(text: "→ \(message.wireKindLabel)"))
    }

    private func invalidateTinfoilRequest() {
        tinfoilRequestID = UUID().uuidString
    }

    // MARK: - Session action wiring

    /// Provides `SessionView` control callbacks.
    ///
    /// - Send, cancel, retry, and input responses use control-channel messages.
    /// - Other callbacks update local presentation state.
    /// - Daemon messages remain the source for task lifecycle changes.
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

    /// Sends pause or resume through the control channel.
    /// Local state updates immediately.
    /// Daemon status remains the task-state source of truth.
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

    private func beginTinfoilVerificationSession() {
        let sessionID = UUID()
        tinfoilVerificationSessionID = sessionID
        tinfoilVerificationStore.beginSession(
            id: sessionID,
            profileIdentity: ticket
        )
    }

    /// Assigns the daemon-message handler and starts the bridge connection.
    /// Repeated calls do nothing after the connection leaves `.idle`.
    private func configureConnectionIfNeeded() {
        beginTinfoilVerificationSession()
        invalidateTinfoilRequest()
        connection.onServerMessage = { message in
            handleServerMessage(message)
        }
        connection.connect(ticket: ticket, pin: pin)
    }

    /// Applies connection phase changes to the media source and recovery state.
    /// Bridge-less failures keep the synthetic source and logging sender.
    private func handleConnectionPhase(_ phase: HoloConnection.Phase) {
        switch phase {
        case .idle, .connecting:
            autonomousExecutionPermitted = false
        case .connected:
            reconnectAttempts = 0
            isReconnecting = false
            resetZoom()
            if let live = connection.liveFrameSource {
                frameSource.stop()
                frameSource = live
            }
            #if DEBUG
            runTakeControlWitnessIfNeeded()
            #endif
            Haptics.fire(.connect)
            ConnectionDiagnostics.shared.recordConnected(ticket: ticket)
            sendAutoPairPromptIfNeeded()
        case .failed(let reason):
            cancelPendingApprovalIfNeeded()
            autonomousExecutionPermitted = false
            tinfoilVerificationStore.reset()
            invalidateTinfoilRequest()
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

    /// Applies one daemon message to status history and dashboard state.
    /// Task progress, completion, input, and errors can change the dashboard.
    /// Other message types update their dedicated state.
    private func handleServerMessage(_ message: ServerMessage) {
        guard isCurrentTinfoilResponse(message) else { return }
        log(message)
        switch message {
        case .ack:
            break
        case .status(_, let reportedExecutionMode, let capabilities):
            if reportedExecutionMode != nil || capabilities != nil {
                executionMode = reportedExecutionMode ?? "restricted"
                autonomousExecutionPermitted = executionMode == "legacy_holo"
                    && (capabilities ?? []).contains("autonomous_holo")
            }
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
        case .tinfoilVerification(let verification):
            tinfoilVerificationStore.update(
                verification,
                sessionID: tinfoilVerificationSessionID,
                profileIdentity: ticket
            )
        case .clarifyQuestions(let questions):
            applyClarifyQuestions(questions)
        case .inputRequest(let requestId, let kind, let context, let responseOptions, _):
            presentInputRequest(
                requestId: requestId,
                kind: kind,
                context: context,
                responseOptions: responseOptions
            )
        case .approvalRequest(let request):
            presentApproval(request)
        case .secureInputState(let active):
            isMacAtLockScreen = active
        case .documentProcessed(let requestId, let markdown):
            guard requestId == tinfoilRequestID else { return }
            documentError = nil
            documentResult = markdown
        case .documentProcessFailed(let requestId, let error):
            guard requestId == tinfoilRequestID else { return }
            documentResult = nil
            documentError = error
        case .imageAnalyzed(let requestId, let text):
            guard requestId == tinfoilRequestID else { return }
            imageError = nil
            imageResult = text
        case .imageAnalysisFailed(let requestId, let error):
            guard requestId == tinfoilRequestID else { return }
            imageResult = nil
            imageError = error
        case .audioTranscribed(let requestId, let text):
            guard requestId == tinfoilRequestID else { return }
            transcriptionError = nil
            transcriptionResult = text
        case .audioTranscriptionFailed(let requestId, let error):
            guard requestId == tinfoilRequestID else { return }
            transcriptionResult = nil
            transcriptionError = error
        case .speechReady(let requestId, let audioDataBase64):
            guard requestId == tinfoilRequestID else { return }
            speechPlayback.play(base64WavData: audioDataBase64)
        case .speechFailed(let requestId, let error):
            guard requestId == tinfoilRequestID else { return }
            log(.error(text: "speech synthesis failed: \(error)"))
        case .typedPlanReady(let requestId, let plan):
            guard requestId == tinfoilRequestID else { return }
            planError = nil
            planSteps = plan.steps.map { step in
                switch step {
                case .action(let proposal): return "Typed action \(proposal.actionId)"
                case .complete: return "Complete"
                }
            }
        case .plannerStatus:
            break
        case .plannerReceipt:
            break
        case .planReady(let requestId, let steps):
            guard requestId == tinfoilRequestID else { return }
            planError = nil
            planSteps = steps
        case .planFailed(let requestId, let error):
            guard requestId == tinfoilRequestID else { return }
            planSteps = nil
            planError = error
        }
    }

    private func isCurrentTinfoilResponse(_ message: ServerMessage) -> Bool {
        message.tinfoilRequestId.map { $0 == tinfoilRequestID } ?? true
    }

    /// Applies the daemon task-completion status.
    /// A canceled status does not end a task while the app marks it paused.
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

    /// Saves a changed current ticket from the authenticated control channel.
    /// Validation and unchanged-ticket handling occur in the profile store.
    private func applyCurrentTicket(_ ticket: String) {
        let previous = profileStore.defaultProfile?.ticket ?? ""
        guard profileStore.refreshDefaultTicket(ticket) else { return }
        ConnectionDiagnostics.shared.recordTicketRefresh(from: previous, to: ticket)
        log(.status(text: "saved Dev Mac ticket refreshed — daemon identity rotated"))
    }

    private func presentApproval(_ request: ApprovalRequest) {
        let now = UInt64(Date().timeIntervalSince1970 * 1_000)
        guard request.expiresAt > now else {
            log(.error(text: "approval request expired before presentation"))
            return
        }
        cancelPendingApprovalIfNeeded()
        pendingApproval = request
        presentedApprovalForCancellation = request
    }

    private func cancelPendingApprovalIfNeeded() {
        guard let request = presentedApprovalForCancellation else { return }
        respondToApproval(request, decision: .cancel)
    }

    private func respondToApproval(_ request: ApprovalRequest, decision: ApprovalDecision) {
        guard presentedApprovalForCancellation?.approvalId == request.approvalId else { return }
        let resolvedDecision: ApprovalDecision
        if decision == .approve {
            let now = UInt64(Date().timeIntervalSince1970 * 1_000)
            resolvedDecision = request.expiresAt > now ? .approve : .cancel
        } else {
            resolvedDecision = decision
        }
        pendingApproval = nil
        presentedApprovalForCancellation = nil
        sendControlMessage(.approvalResponse(
            approvalId: request.approvalId,
            actionId: request.actionId,
            proposalDigest: request.proposalDigest,
            decision: resolvedDecision
        ))
        switch resolvedDecision {
        case .approve: log(.status(text: "approval sent"))
        case .deny: log(.status(text: "denial sent"))
        case .cancel: log(.status(text: "approval canceled"))
        }
    }

    /// Displays one daemon input request in the Input-needed state.
    /// Also opens the controls sheet so response options are visible.
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

    /// Moves Connecting to Working and applies later task-progress updates to the Working payload.
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

    /// Restores task controls from the daemon `task_active` message after reconnection.
    /// Running and paused tasks restore different pill states.
    /// Existing active state is not replaced.
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

    /// Moves an active task to Failed after a daemon error. Errors outside active tasks remain in status history.
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

    /// Returns trimmed prompt text or the representative demo transcript when the prompt is empty.
    private func currentTranscriptOrDemo() -> String {
        let trimmed = promptText.trimmingCharacters(in: .whitespacesAndNewlines)
        return trimmed.isEmpty ? Self.demoReview.transcript : trimmed
    }

    /// Moves Idle to Reviewing with a captured request.
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

    /// Sends the reviewed request and moves Reviewing to Connecting.
    /// Daemon progress or error messages determine the next state.
    private func advanceFromReviewToWorking() {
        guard case .reviewing(let payload) = session else { return }
        if executionMode != "legacy_holo" {
            dispatchPrompt(payload.transcript)
            return
        }
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

    /// Displays one persistent media-stream view after connection.
    /// Session-state and fullscreen changes do not recreate the frame source.
    /// Fullscreen mode overlays task controls and the command bar.
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
                        #if canImport(UIKit)
                        if isControllingRemotely {
                            RemoteControlSurface(
                                frameSize: frameSource.lastFrameSize,
                                zoom: zoomScale,
                                pan: panOffset
                            ) { ev in
                                sendControlMessage(.remoteControl(ev))
                            }
                            .frame(width: viewport.width, height: viewport.height)
                            .clipShape(RoundedRectangle(cornerRadius: isVideoFullscreen ? 0 : 28))
                        }
                        #else
                        EmptyView()
                        #endif
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
                    .accessibilityValue(isControllingRemotely ? "Remote control active" : "View only")
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

    /// Displays task controls and the command bar over the fullscreen media stream.
    /// Status history remains in the controls sheet.
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

    private static var logBackground: Color {
        #if os(iOS)
        Color(uiColor: .secondarySystemBackground)
        #else
        Color(nsColor: .controlBackgroundColor)
        #endif
    }

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
        .background(Self.logBackground)
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
        case .inputRequest, .approvalRequest: return .yellow
        case .currentTicket, .tinfoilVerification: return .blue
        case .clarifyQuestions: return Self.orbAccent
        case .secureInputState: return .blue
        case .documentProcessed, .imageAnalyzed, .audioTranscribed, .planReady, .typedPlanReady, .plannerReceipt: return .green
        case .plannerStatus: return .blue
        case .documentProcessFailed, .imageAnalysisFailed, .audioTranscriptionFailed, .planFailed:
            return .red
        case .speechReady: return .green
        case .speechFailed: return .red
        }
    }

    // MARK: - Task controls (stop / pause / redirect surface)

    /// Indicates whether the current dashboard state represents an active task.
    private var isTaskActive: Bool {
        switch session {
        case .connecting, .working, .inputNeeded: return true
        case .idle, .reviewing, .draftReady, .awaitingApproval, .failed: return false
        }
    }

    /// Displays active-task status with Pause or Resume and Stop controls.
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

    /// Stops the daemon task and resets the local task state.
    private func stopActiveTask() {
        cancelPendingApprovalIfNeeded()
        sendStop()
        isTaskPaused = false
        activeInputRequestID = nil
        log(.status(text: "task cancelled"))
        session = .idle
    }

    // MARK: - Command bar

    /// Provides the default main-screen command bar. Prompts send directly through `sendLivePrompt()`.
    private var commandBar: some View {
        commandBar(fullscreen: false)
    }

    /// Defines the shared orb-blue accent used by the command bar and related flows.
    private static let orbAccent = Color(red: 0.30, green: 0.56, blue: 1.0)

    private func commandBar(fullscreen: Bool) -> some View {
        let hasPrompt = !promptText.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
        let promptInputPermitted = true
        return VStack(spacing: 8) {
            if !autonomousExecutionPermitted, !isControllingRemotely {
                Label("Safe typed mode — goals use reviewed typed plans", systemImage: "checkmark.shield.fill")
                    .font(.caption.weight(.semibold))
                    .foregroundStyle(.secondary)
            }
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
                .onSubmit {
                    guard promptInputPermitted else { return }
                    sendLivePrompt()
                }
                .disabled(!promptInputPermitted)

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
            .disabled(!promptInputPermitted)
            .sensoryFeedback(.impact(weight: .light), trigger: voice.isRecording)
            .accessibilityLabel(voice.isRecording ? "Stop recording" : "Start voice prompt")

            tinfoilAttachMenu

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
            .disabled(!hasPrompt || !promptInputPermitted)
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

    /// Displays progress while the daemon generates clarification questions.
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

    /// Sends one debug prompt after the control channel connects.
    /// `HOLOIROH_AUTOPAIR_PROMPT` provides the prompt.
    /// This path uses `sendLivePrompt()`.
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

    /// Enables debug witnesses for keyboard layout, orb reactions, frame timing, and disconnection.
    /// `HOLOIROH_AUTOFOCUS_PROMPT=1` focuses the prompt field after appearance.
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
        #if canImport(UIKit)
        if ProcessInfo.processInfo.environment["HOLOIROH_FRAME_TIMING_PROBE"] == "1" {
            DispatchQueue.main.asyncAfter(deadline: .now() + 1.0) {
                runFrameTimingProbe()
            }
        }
        #endif
        if ProcessInfo.processInfo.environment["HOLOIROH_WITNESS_DISCONNECT"] == "1" {
            DispatchQueue.main.asyncAfter(deadline: .now() + 6.0) {
                NSLog("MainView: witness invoking disconnect")
                onDisconnect()
            }
        }
        guard ProcessInfo.processInfo.environment["HOLOIROH_AUTOFOCUS_PROMPT"] == "1" else { return }
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.3) {
            isPromptFocused = true
        }
        #endif
    }

    #if DEBUG
    private func runTakeControlWitnessIfNeeded() {
        guard !didRunTakeControlWitness,
              ProcessInfo.processInfo.environment["HOLOIROH_WITNESS_TAKE_CONTROL"] == "1",
              connection.phase == .connected,
              connection.liveFrameSource != nil
        else { return }
        didRunTakeControlWitness = true
        DispatchQueue.main.async {
            guard connection.phase == .connected,
                  connection.liveFrameSource != nil,
                  !isControllingRemotely
            else { return }
            toggleRemoteControl()
            assert(isControllingRemotely, "take-control witness must enter remote-control state")
        }
    }

    #if canImport(UIKit)
    /// Measures frame timing while changing committed zoom and pan state.
    /// This probe does not measure in-progress gesture state.
    /// The duration defaults to 4 seconds.
    /// `HOLOIROH_FRAME_TIMING_PROBE_SECONDS` can override the duration.
    /// Zoom and pan return to identity after measurement.
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
    #endif

    /// Displays the control-mode toggle over the media stream.
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

    /// Displays the remote-typing toggle while hands-on control is active.
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

    /// Displays a single-line typing bar over the media stream during hands-on control.
    /// Text uses the existing remote-control send path.
    /// Orange styling distinguishes remote typing from agent prompts.
    /// Only the bar moves above the keyboard.
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

    /// Enters or leaves hands-on remote control.
    /// Entry resets zoom and pan before it sends `takeControl`.
    /// Exit sends `releaseControl` and closes the typing overlay.
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
            // Length only, never the text. While controlling remotely the
            // command bar types straight into the Mac -- and the lock-screen
            // banner explicitly tells the user to type their Mac PASSWORD here.
            // Logging it verbatim put that password in plaintext into the
            // in-app status log.
            log(.status(text: "\u{2192} typed \(trimmed.count) characters to Mac"))
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

    /// Sends a new prompt or redirects the active task.
    private func dispatchPrompt(_ trimmed: String) {
        RecentPromptsRepository().record(trimmed)
        orbEffects.react(to: trimmed)
        if executionMode == "legacy_holo" {
            let legacyMessage: ClientMessage = (isTaskActive || isTaskPaused)
                ? .redirect(text: trimmed)
                : .prompt(text: trimmed)
            lastSentTask = legacyMessage
            sendControlMessage(legacyMessage)
            log(.status(text: isTaskActive || isTaskPaused
                ? "→ legacy Holo redirect: \(trimmed)"
                : "→ legacy Holo prompt: \(trimmed)"))
            isTaskPaused = false
            activeInputRequestID = nil
            session = .connecting
            return
        }

        let typed = ClientMessage.typedPrompt(TypedPrompt(
            goalId: "goal-\(UUID().uuidString.lowercased())",
            instruction: trimmed
        ))
        lastSentTask = typed
        sendControlMessage(typed)
        log(.status(text: "→ safe typed goal: \(trimmed)"))
        isTaskPaused = false
        activeInputRequestID = nil
        session = .connecting
    }

    /// Requests clarification and displays progress.
    /// After 25 seconds without questions, sends the original prompt directly.
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

    /// Applies clarification questions from the daemon.
    /// An empty array sends the original prompt directly.
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

    /// Combines clarification answers with the original prompt and sends the result.
    private func submitClarification(_ answers: [(question: String, answer: String)]) {
        guard let prompt = pendingClarifyPrompt else { return }
        let clarified = ClarifyComposer.compose(original: prompt, answers: answers)
        pendingClarifyPrompt = nil
        clarifyQuestions = []
        dispatchPrompt(clarified)
    }

    /// Cancels clarification and discards the pending prompt.
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

    /// Sends one message through the selected control-channel sender.
    /// Connected sessions use the bridge.
    /// Bridge-less or disconnected sessions record bounded metadata locally.
    private func sendControlMessage(_ message: ClientMessage) {
        guard !message.requiresAutonomousHolo || autonomousExecutionPermitted else {
            log(.status(text: "restricted mode: autonomous execution unavailable"))
            return
        }
        controlChannel.send(message)
    }

    /// Sends the global daemon stop message with no context identifier.
    /// All task Cancel controls use this method.
    private func sendStop() {
        sendControlMessage(.stop(contextId: nil))
    }

    // MARK: - Log helper

    /// Limits status history to the newest 200 entries.
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

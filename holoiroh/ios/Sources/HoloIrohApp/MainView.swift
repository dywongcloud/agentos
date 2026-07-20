import SwiftUI

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

    /// The last task-committing message sent (`advanceFromReviewToWorking`),
    /// kept so the Failed panel's Retry re-sends the same request.
    @State private var lastSentTask: ClientMessage?

    @State private var promptText: String = ""
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

    /// Whether the hidden controls sheet (the per-state SessionView panel,
    /// the status/log panel, and Disconnect) is presented. Toggled by the
    /// command bar's sparkle button; hidden by default.
    @State private var showControls = false

    /// App foreground/background state, driving the foreground video
    /// recovery in `body` (see the `.onChange(of: scenePhase)` there).
    @Environment(\.scenePhase) private var scenePhase

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
            logEntries.append(LogEntry(message: .status(text: "→ sent (not connected) \(message.wireKindLabel): \(trimmed)")))
        }
    }

    var body: some View {
        GeometryReader { geo in
            // Shared layout math: the live-share box is ~85% of the width at
            // a 16:10 aspect, with its top edge at ~40% of the height so the
            // orb scene keeps the top of the screen entirely to itself. The
            // SAME numbers drive both the box chrome and the persistent
            // video surface so the two always coincide exactly.
            let boxWidth = geo.size.width * 0.85
            let boxHeight = boxWidth * 10.0 / 16.0
            let boxCenterY = geo.size.height * 0.40 + boxHeight / 2
            let isConnected = connection.phase == .connected
            // Fullscreen only ever presents while the video surface exists;
            // if the connection drops mid-fullscreen the normal layout comes
            // straight back instead of leaving a black screen.
            let isFullscreenActive = isVideoFullscreen && isConnected

            ZStack {
                // Layer 0: full-black backdrop + the blue blob orb Spline
                // scene (see SplineOrbBackground's doc for the web-runtime
                // rationale + offline gradient fallback). The orb renders
                // its own top-area layout; nothing is overlaid on the top
                // ~40% of the screen so it stays clear.
                Color.black.ignoresSafeArea()
                SplineOrbBackground()

                if !isFullscreenActive {
                    // CENTER: the live-screen-share box (chrome + placeholder
                    // only -- the video itself is the persistent
                    // `videoOverlay` surface below, framed to this box).
                    liveShareBox(width: boxWidth, height: boxHeight)
                        .position(x: geo.size.width / 2, y: boxCenterY)

                    // BOTTOM: the single command bar.
                    VStack(spacing: 0) {
                        Spacer()
                        commandBar
                            .padding(.horizontal, 16)
                            .padding(.bottom, 8)
                    }
                }

                // Topmost: the persistent live-share video surface -- ONE
                // VideoRenderView across both the boxed and fullscreen
                // layouts (same view identity; the fullscreen transition is
                // a pure frame/layout change, never an unmount/remount).
                videoOverlay(
                    in: geo.size,
                    boxWidth: boxWidth,
                    boxHeight: boxHeight,
                    boxCenterY: boxCenterY
                )
            }
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
            logEntries.append(LogEntry(message: .error(text: error)))
        }
        // Real transport lifecycle: connect the shared bridge on appear,
        // reflect its phase changes, and tear it down when the screen goes.
        .onAppear(perform: configureConnectionIfNeeded)
        .onDisappear { connection.shutdown() }
        .onChange(of: connection.phase) { _, newPhase in
            handleConnectionPhase(newPhase)
        }
        // Foreground recovery: iOS invalidates VideoToolbox decode sessions
        // when the app backgrounds, so on return the old subscription's
        // decoder can only error until a fresh keyframe arrives -- and if
        // the track went stale, never. Restarting the frame source gets a
        // FRESH track from the daemon, whose first frame is always a
        // keyframe (live-witnessed in every frame-pull probe), healing the
        // decoder deterministically. stop()/start() serialize on the
        // source's own queue, and the shared bridge pointer survives stop()
        // (see IrohLiveFrameSource.teardownHandles) -- live-witnessed as
        // "works, then black screen after switching apps" without this.
        .onChange(of: scenePhase) { _, newPhase in
            guard newPhase == .active, connection.phase == .connected else { return }
            frameSource.stop()
            frameSource.start()
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
                    .stroke(Color.white.opacity(0.35), lineWidth: 1)
            )
            .overlay {
                if !isConnected {
                    Text("Live Screen Share here")
                        .font(.system(size: 22))
                        .foregroundStyle(.white)
                        .multilineTextAlignment(.center)
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
                    Text("Disconnect")
                        .font(.headline)
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
        logEntries.append(LogEntry(message: .status(text: "demo → \(newState.displayName)")))
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
                // Connecting, Input-needed, and Draft-ready panels) is the
                // "Stop" the user hits to halt whatever the agent is doing on the
                // Mac. Send the actual `ClientMessage.stop` over the control
                // channel *before* the local transition, so the daemon's
                // `handle_stop` -> `holo stop` kill-switch path fires (once the
                // transport is real). The local `.idle` transition + log entry is
                // the stand-in for the daemon's `Done { Canceled }` response until
                // the real channel streams it back.
                sendStop()
                log(.status(text: "task cancelled"))
                session = .idle
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
                session = .working(Self.demoWorking)
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

    /// Toggle the Working panel's pause flag in place.
    private func togglePause() {
        guard case .working(var payload) = session else { return }
        payload.isPaused.toggle()
        session = .working(payload)
        log(.status(text: payload.isPaused ? "paused" : "resumed"))
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
            if let live = connection.liveFrameSource {
                frameSource.stop()
                frameSource = live
            }
        case .failed(let reason):
            log(.error(text: "connection unavailable: \(reason)"))
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
        }
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
            ZStack {
                if isVideoFullscreen {
                    Color.black
                        .ignoresSafeArea()
                        .transition(.opacity)
                }

                VideoRenderView(source: frameSource)
                    .id(ObjectIdentifier(frameSource as AnyObject))
                    .background(Color.black)
                    .frame(
                        width: isVideoFullscreen ? size.width : boxWidth,
                        height: isVideoFullscreen ? size.height : boxHeight
                    )
                    .clipShape(RoundedRectangle(cornerRadius: isVideoFullscreen ? 0 : 28))
                    .overlay(
                        RoundedRectangle(cornerRadius: isVideoFullscreen ? 0 : 28)
                            .stroke(Color.white.opacity(isVideoFullscreen ? 0 : 0.35), lineWidth: 1)
                    )
                    .overlay(alignment: .topTrailing) {
                        // Expand / collapse control.
                        Button {
                            withAnimation(.spring(response: 0.35, dampingFraction: 0.85)) {
                                isVideoFullscreen.toggle()
                            }
                        } label: {
                            Image(systemName: isVideoFullscreen
                                  ? "arrow.down.right.and.arrow.up.left"
                                  : "arrow.up.left.and.arrow.down.right")
                                .font(.footnote.weight(.bold))
                                .padding(8)
                                .background(.ultraThinMaterial, in: Circle())
                        }
                        .padding(10)
                        .accessibilityLabel(isVideoFullscreen ? "Exit fullscreen" : "Fullscreen live view")
                    }
                    .contentShape(Rectangle())
                    .onTapGesture {
                        if !isVideoFullscreen {
                            withAnimation(.spring(response: 0.35, dampingFraction: 0.85)) {
                                isVideoFullscreen = true
                            }
                        }
                    }
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

    /// Twitch-style live overlay for fullscreen: the last few log entries as
    /// a translucent feed, with the command bar below for sending prompts to
    /// the agent without leaving the mirror. Fullscreen sends go DIRECTLY to
    /// the daemon (no Reviewing detour): fullscreen is the live-driving mode.
    private var fullscreenChatOverlay: some View {
        VStack(spacing: 8) {
            VStack(alignment: .leading, spacing: 4) {
                ForEach(logEntries.suffix(5)) { entry in
                    HStack(alignment: .top, spacing: 6) {
                        Text(entry.message.kindLabel)
                            .font(.system(.caption2, design: .monospaced).weight(.bold))
                            .foregroundStyle(logColor(for: entry.message))
                        Text(entry.message.displayText)
                            .font(.caption)
                            .foregroundStyle(.white.opacity(0.92))
                            .lineLimit(2)
                    }
                    .padding(.horizontal, 10)
                    .padding(.vertical, 4)
                    .background(.black.opacity(0.45), in: RoundedRectangle(cornerRadius: 8))
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)

            commandBar(fullscreen: true)
        }
        .padding(.horizontal, 12)
        .padding(.bottom, 8)
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
        case .taskProgress: return .orange
        case .error: return .red
        }
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
    private func commandBar(fullscreen: Bool) -> some View {
        HStack(spacing: 10) {
            Button {
                showControls.toggle()
            } label: {
                Image(systemName: "sparkles")
                    .font(.system(size: 17, weight: .semibold))
                    .foregroundStyle(.white)
                    .frame(width: 38, height: 38)
                    .background(Color(white: 0.15), in: RoundedRectangle(cornerRadius: 12))
            }
            .buttonStyle(.plain)
            .accessibilityLabel("Toggle session controls")

            TextField("What do you want to do?", text: $promptText, axis: .vertical)
                .textFieldStyle(.plain)
                .lineLimit(1...4)
                .foregroundStyle(.white)
                .tint(.white)
                .padding(.horizontal, 14)
                .padding(.vertical, 10)
                .background(Color(white: 0.13), in: RoundedRectangle(cornerRadius: 14))
                .onSubmit { fullscreen ? sendLivePrompt() : sendPrompt() }

            Button {
                toggleMicrophone()
            } label: {
                Image(systemName: voice.isRecording ? "mic.fill" : "mic")
                    .font(.system(size: 17))
                    .foregroundStyle(voice.isRecording ? Color.red : Color(white: 0.75))
                    .frame(width: 30, height: 38)
            }
            .buttonStyle(.plain)
            .accessibilityLabel(voice.isRecording ? "Stop recording" : "Start voice prompt")

            Button {
                fullscreen ? sendLivePrompt() : sendPrompt()
            } label: {
                Image(systemName: "paperplane")
                    .font(.system(size: 17))
                    .foregroundStyle(Color(white: 0.75))
                    .frame(width: 30, height: 38)
            }
            .buttonStyle(.plain)
            .disabled(promptText.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
            .accessibilityLabel("Send prompt")
        }
        .padding(12)
        .background(Color(white: 0.09), in: RoundedRectangle(cornerRadius: 24))
    }

    /// Fullscreen live-mode send: straight to the daemon as a prompt, and
    /// straight into the feed -- no Reviewing stage.
    private func sendLivePrompt() {
        let trimmed = promptText.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return }
        promptText = ""
        lastSentTask = .prompt(text: trimmed)
        sendControlMessage(.prompt(text: trimmed))
        log(.status(text: "→ live prompt: \(trimmed)"))
    }

    private func sendPrompt() {
        let trimmed = promptText.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return }

        // Capturing a prompt only *stages* it: nothing reaches the daemon
        // until the user confirms with the Reviewing panel's Send control
        // (`advanceFromReviewToWorking`), which performs the real
        // control-channel send. Acks now come from the daemon, not a local
        // synthesized entry.
        beginReview(from: trimmed)
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
        logEntries.append(
            LogEntry(message: .status(text: wasRecording ? "stopped listening" : "listening…"))
        )
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

    private func log(_ message: ServerMessage) {
        logEntries.append(LogEntry(message: message))
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
}

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
/// 3. The **status/log panel**, always visible below the state panel.
/// 4. The per-state **SessionView** panel.
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
/// "Demo" toolbar menu still jumps directly to any of the eight states so
/// every state's controls remain reachable and inspectable.
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
        VStack(spacing: 0) {
            ScrollView {
                VStack(spacing: 12) {
                    // Remote View is shown only in states that need it
                    // (Working / DraftReady). It keeps a stable identity so
                    // the single video surface is not rebuilt per state.
                    if session.showsRemoteView {
                        videoPreview
                    }

                    SessionView(
                        state: session,
                        macName: macName,
                        macAvailable: macAvailable,
                        inferenceMode: inferenceMode,
                        actions: sessionActions
                    )
                }
                .padding(.horizontal)
                .padding(.top, 12)
            }

            logPanel

            promptBar
                .padding()
        }
        .navigationTitle("HoloIroh")
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .topBarLeading) {
                demoMenu
            }
            ToolbarItem(placement: .topBarTrailing) {
                Button("Disconnect", role: .destructive) {
                    connection.shutdown()
                    onDisconnect()
                }
            }
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
    }

    // MARK: - Demo control (local transition trigger stand-in)

    /// A toolbar menu that jumps directly to any of the eight PRD 6.1 states
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

    // MARK: - Video preview

    private var videoPreview: some View {
        // Real render surface: a `VideoRenderView` (AVSampleBufferDisplayLayer)
        // bound to `frameSource` -- the shared-bridge `IrohLiveFrameSource`
        // once connected, the synthetic placeholder before then. `.id` keys
        // the representable to the source's identity so the connect-time swap
        // tears down the old binding (`dismantleUIView` stops the old source)
        // and `makeUIView` starts the new one.
        VideoRenderView(source: frameSource)
            .id(ObjectIdentifier(frameSource as AnyObject))
            .background(Color.black)
            .aspectRatio(16.0 / 9.0, contentMode: .fit)
            .clipShape(RoundedRectangle(cornerRadius: 12))
            .accessibilityLabel("Live remote view")
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

    // MARK: - Prompt bar

    private var promptBar: some View {
        HStack(spacing: 8) {
            TextField("Type a prompt…", text: $promptText, axis: .vertical)
                .textFieldStyle(.roundedBorder)
                .lineLimit(1...4)
                .onSubmit(sendPrompt)

            Button {
                toggleMicrophone()
            } label: {
                Image(systemName: voice.isRecording ? "mic.fill" : "mic")
                    .foregroundStyle(voice.isRecording ? Color.red : Color.accentColor)
            }
            .buttonStyle(.bordered)
            .accessibilityLabel(voice.isRecording ? "Stop recording" : "Start voice prompt")

            Button {
                sendPrompt()
            } label: {
                Image(systemName: "arrow.up.circle.fill")
                    .font(.title2)
            }
            .buttonStyle(.plain)
            .disabled(promptText.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
            .accessibilityLabel("Send prompt")
        }
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

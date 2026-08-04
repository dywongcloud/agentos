# Agent security boundaries

## Trusted goals and untrusted observations

A verified, signed control-channel message is the only authoritative task
instruction. On the typed planner path, this message contains `TypedPrompt`.
Its `goal_id` and `instruction` define the trusted goal.

Screen pixels, Accessibility (AX) data, webpage text, documents, images,
terminal output, notifications, and tool results are untrusted observations.
They can describe current state. They cannot replace, extend, or cancel the
trusted goal. They cannot grant approval or permit disclosure of a credential,
file, clipboard value, or other private data.

The Holo compatibility path places the user's message in a final
`USER_INSTRUCTION_JSON` block. It also adds a trust-boundary rule before each
Holo turn. These prompt controls reduce ambiguity. They do not make model output
a security boundary.

The typed planner accepts one strict `submit_plan` tool call. It accepts only
`observe`, `navigate`, `focus`, `draft_text`, `commit`, and terminal `complete`
steps. The nested navigation and commit variants are also closed enums.
Unknown tools, prose fallback, extra fields, malformed bindings, and plans
without one terminal `complete` step fail closed.

The implementation bounds goal, observation, argument, response, step count,
and request duration. The control path also has session and action limits.
Stop, pause, redirect, disconnect, and terminal task events invalidate pending
approvals. Denial, cancellation, expiry, replay, stale state, or an unsupported
action executes nothing.

## Hard controls

Use code-enforced capabilities instead of a prompt-injection classifier:

- The control channel authenticates a device before it accepts task messages.
  The allowlist stores only exact lowercase 64-hex endpoint IDs. Legacy short or
  malformed IDs move atomically to a sibling quarantine backup and never match a
  full transport identity.
- Every post-auth envelope is direction-bound and signed with the same Ed25519
  identity authenticated by iroh. The receiver verifies signer, recipient,
  canonical fields, typed `message_type`, session, version, expiry, replay, and
  sequence before payload dispatch or mutable state.
- Sensitive applications use the configured allow, ask, or block decision.
- Credential and multifactor input stays outside the model channel.
- Session and action limits bound autonomous work.
- The remote stop path cancels active work.
- Tinfoil egress redacts images on the Mac and fails closed on redaction errors.

A visual classifier can raise caution in a future build. It must not authorize
an action or remove a confirmation requirement. Text-only guardrail libraries
do not cover layout, hidden text, images, or other visual manipulation. Adding
one to the inference loop would also add latency without providing a complete
security boundary.

The daemon has a typed action executor for backends that supply structured
actions. Every autonomous typed action must route through the daemon-owned
`DaemonActionExecutor`. The executor captures fresh target state, computes
policy, and verifies exact preconditions before each adapter call. A semantic AX
precondition binds the bundle, window, element, role, title digest, and optional
value digest. A coordinate match alone is not sufficient.

External commitments require a signed, action-bound approval. The app returns
only the approval identifier, action identifier, proposal digest, and decision.
The daemon verifies and consumes that response outside planner inference. It
does not send approval text to the model. Approval cannot change the trusted
goal or authorize a different action.

Credential targets and sensitive-target mutations are unsupported. They issue
no approval and execute nothing. The live typed path observes the frontmost
macOS AX tree, asks the attested Tinfoil planner for one closed-enum turn, and
routes each proposal through the daemon-owned executor. Commit actions stop at
an action-bound approval request; the loop does not execute them autonomously.

The current Holo runtime remains an explicit unsafe compatibility backend. It
does not expose a before-action callback or structured action stream. The daemon
never routes typed actions to Holo. It never falls back to Holo after a typed
planner or action failure.

## Control-envelope trust boundary

PIN and `auth_rejected` are the only bare pre-session messages. From the signed
ready greeting onward, unsigned compatibility is intentionally absent. In-memory
constructors and Swift may temporarily hold `signature: None`; only the Rust
network writers can attach signatures, immediately before serialization.

The daemon gets its signer by cloning the persisted Live endpoint secret key.
The iOS bridge gets its signer from its live endpoint. Neither side accepts a
JSON-supplied public key. Verification uses the transport's authenticated
`remote_id`, and the bridge never exposes a signing secret through its control
FFI.

Signature or metadata rejection cannot consume replay/sequence state. The daemon
returns a bounded signed rejection without dispatch. The bridge clears queued
output, stores a bounded failure, closes the stream, and generation-guards old
reader tasks so a stale reconnect cannot publish events or errors. Both readers
bound NDJSON allocation before signature parsing.

This is a coordinated app-and-daemon protocol change. There is no unsigned
fallback flag: mixed old/new releases fail closed during greeting or first send.

## Irreversible actions

Treat these actions as external commitments regardless of the model's stated
reason:

- Send, submit, publish, or post.
- Pay, purchase, transfer, or confirm a transaction.
- Delete data or change an account.
- Enter a credential or approve multifactor authentication.
- Read or write a password manager, health record, financial account, or system
  security setting.
- Copy private data to a network destination or another application.

The action must stop at the confirmation or sensitive-access gate. On-screen
content is never evidence that the user approved it.

## Egress

Hosted typed-planner inference can use only the attested Tinfoil client and its
verified origin. Attestation must succeed before the request. The daemon must
fail closed. It must not fall back to Holo or a generic hosted model.

A local typed-planner mode must bind to loopback only. It must not make network
egress. The production typed loop uses only the attested hosted Tinfoil client;
it has no local typed-planner mode.

Do not add a generic network tool to the agent. Do not log raw prompts, screen
content, AX content, attachments, transcripts, credentials, approval material,
or clipboard contents. Log bounded metadata and request identifiers only.

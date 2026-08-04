# Holoiroh contributor instructions

## Technical writing standard

Use ASD-STE100 principles in Rust documentation, Swift documentation, Markdown,
and user-visible operational text.

- Use short, direct sentences. Target 20 words or fewer.
- Put one instruction or claim in each sentence.
- Use active voice when the actor is known.
- Use the same term for the same component or action.
- Do not use a synonym only to add variety.
- Define abbreviations at first use.
- Put conditions before the action when order matters.
- Use numbered steps for ordered procedures.
- Use bullets for unordered facts.
- State limits with exact units and boundary behavior.
- State failures and recovery actions explicitly.
- Do not describe planned behavior as implemented behavior.
- Do not claim performance, compatibility, or security without a real witness.

## Code documentation structure

For a public module, type, or function, document only information that the code
does not state clearly.

Use this order when each item applies:

1. Purpose.
2. Inputs and preconditions.
3. Observable behavior.
4. Output or state change.
5. Failure behavior.
6. Safety or concurrency invariant.
7. Real witness command or source reference.

Do not restate a function name or repeat its signature in prose. Explain why a
constraint exists when removing it would cause a defect. Keep historical detail
in an architecture document when it is not required to use the code safely.

## Project terms

Use these terms consistently:

- **daemon**: the macOS `holoiroh-daemon` process.
- **app**: the iOS SwiftUI application.
- **bridge**: `holoiroh-ios-bridge`, unless a specific Holo bridge is named.
- **control channel**: the `holoiroh/control/1` iroh protocol.
- **media stream**: the `iroh-live` screen broadcast.
- **ticket**: the iroh connection ticket.
- **verification proof**: the Tinfoil attestation evidence shown by the app.
- **local inference**: `llama-server` on loopback.
- **confidential inference**: the attested Tinfoil enclave path.

## Validation

Use executable examples and live probes instead of standing test files. Record
the exact command and observed result. Keep credentials in ignored environment
files. Never place a credential, raw attachment, prompt, transcript, or screen
content in logs or committed fixtures.

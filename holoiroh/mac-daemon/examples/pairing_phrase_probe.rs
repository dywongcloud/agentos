//! Manual, run-by-hand probe: witnesses that the daemon's `pairing_phrase` derivation matches
//! the iOS side (`PairingPhrase.swift`) byte-for-byte, using the exact known-answer vectors from
//! `holoiroh/ios/PAIRING_PHRASE.md`. If these pass, the Mac and the phone display the SAME SAS
//! phrase for a given ticket, which is the whole point of the short-phrase mutual verification
//! (a MITM-substituted ticket yields a different phrase on one side). Not `#[cfg(test)]`, run via
//! `cargo run --example pairing_phrase_probe`, per this repo's no-unit-tests rule.

use holoiroh_daemon::pairing_phrase::pairing_phrase;

/// (ticket, expected 4-word phrase) -- copied verbatim from PAIRING_PHRASE.md's known-answer
/// table, which is the iOS side's own CryptoKit-SHA256 output. Matching these proves the Rust
/// `sha2` + shared WORDLIST reproduces the Swift result.
const VECTORS: &[(&str, &str)] = &[
    (
        "iroh-live:TleiXllmGyIDcEOXtF-AIExJQnPFPlZuzkXmR6OVWNwDAQDAqAFM09EDAQDAqEAB09EDAQDAqP8K09ED/holoiroh",
        "grove cover rival quilt",
    ),
    (
        "iroh-live:QTkI9b7mK9JTO8u1DjKCF-5HKeA_8trhtNSq3lo29IYDAQDAqAFMsY8DAQDAqEABsY8DAQDAqP8KsY8D/holoiroh",
        "blend patio eagle cliff",
    ),
];

fn main() {
    println!("=== daemon pairing_phrase vs iOS known-answer vectors (PAIRING_PHRASE.md) ===");
    let mut all_ok = true;
    for (ticket, expected) in VECTORS {
        let got = pairing_phrase(ticket);
        let ok = got == *expected;
        all_ok &= ok;
        println!(
            "  ticket ...{}  -> {:?}  (expected {:?})  {}",
            &ticket[ticket.len().saturating_sub(24)..],
            got,
            expected,
            if ok { "MATCH" } else { "MISMATCH" }
        );
        assert!(
            ok,
            "daemon phrase {got:?} != iOS-derived {expected:?} for ticket {ticket} -- the Rust and Swift derivations have diverged; pairing verification would fail on real devices"
        );
    }

    // Determinism + distinct-ticket-distinct-phrase sanity (a substituted ticket MUST change
    // the phrase, or the SAS gives no MITM protection).
    let a = pairing_phrase(VECTORS[0].0);
    let a2 = pairing_phrase(VECTORS[0].0);
    let b = pairing_phrase(VECTORS[1].0);
    assert_eq!(a, a2, "same ticket must always give the same phrase");
    assert_ne!(a, b, "different tickets must give different phrases (MITM protection)");

    assert!(all_ok);
    println!(
        "\npairing_phrase_probe: OK -- daemon SHA-256+wordlist phrase derivation matches the iOS side byte-for-byte on both known-answer vectors, is deterministic, and distinct tickets yield distinct phrases. The Mac's printed 'verification phrase' will equal what the iOS app shows for the same scanned ticket."
    );
}

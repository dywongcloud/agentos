//! Manual, run-by-hand probe for the `--rotate-every` ticket-rotation feature (see `main.rs`'s
//! `Cli::rotate_every` + the rotation ticker, and PAIRING.md's "Ticket rotation" section). Two
//! things are witnessed by real execution, no `#[cfg(test)]`, run via
//! `cargo run --example ticket_rotation_probe`:
//!
//! 1. The `duration::parse_rotate_duration` parser: valid forms (`30m`, `2h`, `90s`, `1h30m`,
//!    `1d`) parse to the right `Duration`, and every malformed form (`""`, `abc`, `10x`, `5`
//!    unitless, `0s`) is a clean `Err`, never a panic.
//! 2. The rotation *effect*: distinct tickets (which is what a real rotation produces -- a new
//!    iroh identity yields a new ticket) yield distinct verification phrases via the SAME
//!    `pairing_phrase::pairing_phrase` the daemon re-prints each tick, and re-deriving the phrase
//!    for one ticket is stable. This witnesses that the re-print on each rotation tick reflects
//!    the current ticket correctly and that a rotated ticket is visibly different to the operator.
//!
//! What this probe does NOT and cannot witness (honest boundary): the actual timer firing inside
//! a running daemon, or a full fresh-iroh-keypair identity rotation -- both need the daemon to run
//! past the macOS TCC preflight (see the `holoiroh-user-action-grant-tcc-and-run-daemon` PRD row).
//! Here the current pass re-prints the *current* ticket on each tick; this probe covers the parser
//! and the phrase-re-derivation that the re-print depends on.

use holoiroh_daemon::duration::parse_rotate_duration;
use holoiroh_daemon::pairing_phrase::pairing_phrase;

fn main() {
    println!("=== (1) parse_rotate_duration: valid forms ===");
    let valid: &[(&str, u64)] = &[
        ("30m", 1800),
        ("2h", 7200),
        ("90s", 90),
        ("1h30m", 5400),
        ("1d", 86_400),
        ("  45m  ", 2700), // trimmed
    ];
    for (input, want_secs) in valid {
        let got = parse_rotate_duration(input)
            .unwrap_or_else(|e| panic!("{input:?} should parse, got Err({e})"));
        assert_eq!(
            got.as_secs(),
            *want_secs,
            "{input:?} -> {}s, expected {want_secs}s",
            got.as_secs()
        );
        println!("  {input:?} -> {}s  OK", got.as_secs());
    }

    println!("\n=== (1) parse_rotate_duration: malformed forms are clean Err, never panic ===");
    let malformed: &[&str] = &["", "   ", "abc", "10x", "5", "0s", "m30", "12"];
    for input in malformed {
        match parse_rotate_duration(input) {
            Ok(d) => panic!("{input:?} should be an error, parsed to {d:?}"),
            Err(e) => println!("  {input:?} -> Err({e})  OK"),
        }
    }

    println!("\n=== (2) rotation effect: distinct tickets -> distinct phrases, stable per-ticket ===");
    // Two different ticket strings, as a real rotation (new iroh identity) would produce.
    let ticket_a = "iroh-live:AAAA_rotation_probe_ticket_one_DAQDAqAFM/holoiroh";
    let ticket_b = "iroh-live:BBBB_rotation_probe_ticket_two_DAQDAqAFM/holoiroh";

    let phrase_a1 = pairing_phrase(ticket_a);
    let phrase_a2 = pairing_phrase(ticket_a);
    let phrase_b = pairing_phrase(ticket_b);
    println!("  ticket A phrase: {phrase_a1:?}");
    println!("  ticket B phrase: {phrase_b:?}");
    assert_eq!(
        phrase_a1, phrase_a2,
        "re-deriving the phrase for the same ticket must be stable (the re-print on a tick shows the same phrase for an unchanged ticket)"
    );
    assert_ne!(
        phrase_a1, phrase_b,
        "a rotated (distinct) ticket must yield a DIFFERENT phrase, so the operator visibly sees the rotation"
    );

    println!(
        "\nticket_rotation_probe: OK -- parse_rotate_duration accepts 30m/2h/90s/1h30m/1d (+ trims) and cleanly rejects empty/garbage/unitless/zero without panic; a rotated (distinct) ticket produces a distinct verification phrase via the same pairing_phrase the daemon re-prints each --rotate-every tick, and per-ticket phrase derivation is stable. The live timer firing + full keypair rotation need the TCC-gated daemon run (see PAIRING.md / holoiroh-user-action-grant-tcc-and-run-daemon)."
    );
}

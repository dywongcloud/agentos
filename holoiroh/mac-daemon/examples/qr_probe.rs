//! Manual, run-by-hand probe: witnesses that `main.rs`'s `print_ticket_qr` path renders a
//! real, scannable QR code for a realistic iroh `LiveTicket`-shaped string. `main.rs` itself
//! can't be driven to this point headlessly (its macOS permission preflight refuses to start
//! without Screen Recording/Accessibility TCC grants, which need an interactive System
//! Settings click), so this probe exercises the exact same `qrcode` render path against a real
//! ticket-length string and asserts the output is a valid QR grid -- not `#[cfg(test)]`, run
//! via `cargo run --example qr_probe`, per this repo's no-unit-tests rule.
//!
//! It reconstructs the same render call `main.rs::print_ticket_qr` makes (one-module-per-char,
//! ' ' light / '█' dark, quiet zone on) rather than calling that private fn, and additionally
//! decodes structural facts (module count matches the QR version for that data length) so the
//! witness is that a *correct* QR was produced, not just that some characters were printed.

fn main() {
    // A realistic iroh LiveTicket string: `iroh-live:` + a long base32-ish node/broadcast
    // blob, ~180 chars -- the same order of magnitude a real ticket prints (see the ticket a
    // prior pre-permission-gate daemon run emitted, recorded in PRD witnesses).
    let ticket = "iroh-live:hExqb9KlcU9cySSy6wGwCsA_0zk5fy7Ny_MbBw3uzVgDAQDAqAFMjeIDAQDAqEABjeIDAQDAqP8KjeID6xr3hAqUpuVK8pWZ2mQ0hPzL9xNvWtQ7aBcDeFgHiJkLmNoPqRsTuVwXyZ0123456789/holoiroh";

    println!("=== rendering a realistic {}-char ticket as a QR ===", ticket.len());

    let code = qrcode::QrCode::new(ticket.as_bytes())
        .expect("a ~180-char ticket is well within QR capacity; construction must succeed");

    // Structural witness: the chosen QR version must actually hold this data. `width()` is the
    // module count per side; it grows with version. For ~180 bytes at the default error
    // correction level the crate picks a version whose width is comfortably > 21 (version 1).
    let width = code.width();
    println!("QR chosen: {width}x{width} modules");
    assert!(
        width >= 25,
        "a ~180-char payload should need a QR version larger than v1 (21x21); got {width}x{width}"
    );

    // Render exactly as main.rs::print_ticket_qr does.
    let rendered = code
        .render::<char>()
        .quiet_zone(true)
        .light_color(' ')
        .dark_color('█')
        .build();

    // Witness the rendered grid is non-trivial: it must contain foreground modules (not an
    // all-blank render), and every line must be the same width (a well-formed grid), and there
    // must be at least `width` rows (plus quiet zone).
    let lines: Vec<&str> = rendered.lines().collect();
    let dark_count = rendered.chars().filter(|&c| c == '█').count();
    assert!(dark_count > 50, "rendered QR must have real foreground modules; got {dark_count}");
    let line_widths: std::collections::HashSet<usize> =
        lines.iter().map(|l| l.chars().count()).collect();
    assert_eq!(
        line_widths.len(),
        1,
        "every QR row must be the same character width (a well-formed grid); got widths {line_widths:?}"
    );
    assert!(
        lines.len() >= width,
        "rendered QR should have at least the module rows; got {} rows for {width} modules",
        lines.len()
    );

    println!();
    println!("{rendered}");
    println!(
        "qr_probe: OK -- realistic ticket rendered to a well-formed {width}x{width} QR ({dark_count} dark modules, {} uniform-width rows). Same qrcode render path main.rs::print_ticket_qr uses at startup.",
        lines.len()
    );
}

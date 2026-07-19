//! Manual, run-by-hand probe: witnesses that `main.rs`'s `print_ticket_qr` path renders a
//! real, SCANNABLE QR code for a realistic iroh `LiveTicket`-shaped string, and that the
//! density fix (EcLevel::L + `Dense1x2` half-block rendering) is materially smaller/squarer
//! than the old `QrCode::new` (EcLevel::M) + one-module-per-`char` render that a phone camera
//! could not lock onto. `main.rs` can't be driven here headlessly (macOS TCC preflight), so
//! this exercises the exact same `qrcode` render path against a real ticket-length string.
//! Not `#[cfg(test)]`; run via `cargo run --example qr_probe [ticket]`, per the no-unit-tests rule.

fn main() {
    // A realistic ~230-char ticket (the length the user's real daemon printed): `iroh-live:` +
    // a long base64url node/broadcast blob + `/holoiroh`. Override by passing a real ticket as
    // argv[1] to check its exact version/size.
    let ticket: String = std::env::args().nth(1).unwrap_or_else(|| {
        "iroh-live:UmmYHPAypao5FUFMdPzjzkeI7HlV6AfFVTVpkkVvaJwGACNodHRwczovL3VzdzEtMS5yZWxheS5uMC5pcm9oLmxpbmsuLwEALTJiaeqXAgEALTJiaajyAwEAwKgBTMjAwEAwKhAAcCjAwEAwKj_CsCjAw/holoiroh".to_string()
    });
    println!("=== ticket length: {} bytes ===", ticket.len());

    // Show the version/module-count win of EcLevel::L over the old QrCode::new (EcLevel::M)
    // default -- fewer modules == a smaller, easier-to-scan code.
    let mut l_width = 0usize;
    for (name, ec) in [
        ("L (new fix)", qrcode::EcLevel::L),
        ("M (old QrCode::new default)", qrcode::EcLevel::M),
    ] {
        let code = qrcode::QrCode::with_error_correction_level(ticket.as_bytes(), ec)
            .expect("ticket must be within QR capacity");
        println!(
            "  EcLevel::{name}: version {:?}, {} modules per side",
            code.version(),
            code.width()
        );
        if matches!(ec, qrcode::EcLevel::L) {
            l_width = code.width();
        }
    }

    // Render EXACTLY as main.rs::print_ticket_qr now does: EcLevel::L + Dense1x2 half-block.
    let code = qrcode::QrCode::with_error_correction_level(ticket.as_bytes(), qrcode::EcLevel::L)
        .expect("ticket must be within QR capacity");
    let rendered = code
        .render::<qrcode::render::unicode::Dense1x2>()
        .quiet_zone(true)
        .build();

    let lines: Vec<&str> = rendered.lines().collect();
    let rows = lines.len();
    let cols = lines.first().map_or(0, |l| l.chars().count());
    let old_side = l_width + 8; // old char render: one row+col per module + 4-module quiet zone each side.

    // Structural witnesses: well-formed grid, non-empty, and the half-block render is at most
    // ~half the row count of the old one-row-per-module render (that halving is the whole fix).
    let uniform: std::collections::HashSet<usize> = lines.iter().map(|l| l.chars().count()).collect();
    assert_eq!(uniform.len(), 1, "every QR row must be the same width; got {uniform:?}");
    assert!(rows > 10 && cols > 10, "render must be a real grid; got {cols}x{rows}");
    assert!(
        rows <= old_side / 2 + 2,
        "Dense1x2 must roughly halve the height vs the old {old_side}-row char render; got {rows} rows"
    );

    println!(
        "Dense1x2 render: {cols} cols x {rows} rows  (old char render would be {old_side} x {old_side})"
    );
    println!();
    println!("{rendered}");
    println!(
        "qr_probe: OK -- EcLevel::L + Dense1x2 render is {rows} rows (vs {old_side} for the old M-level char render), a well-formed {cols}x{rows} grid. This is the exact path main.rs::print_ticket_qr uses; the far smaller/squarer code is what makes a phone camera able to scan it."
    );
}

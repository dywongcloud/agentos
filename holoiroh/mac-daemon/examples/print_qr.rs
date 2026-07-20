//! Throwaway utility: renders a given ticket string as a terminal QR code,
//! reusing the exact rendering approach `main.rs::print_ticket_qr` uses, so
//! an already-running daemon's ticket can be displayed in a separate visible
//! terminal without restarting the daemon (which would tear down its
//! existing session_id/allowlist state).
fn main() {
    let ticket = std::env::args().nth(1).expect("usage: print_qr <ticket>");
    match qrcode::QrCode::with_error_correction_level(ticket.as_bytes(), qrcode::EcLevel::L) {
        Ok(code) => {
            let rendered = code
                .render::<qrcode::render::unicode::Dense1x2>()
                .quiet_zone(true)
                .build();
            println!("{rendered}");
        }
        Err(err) => eprintln!("could not render QR: {err}"),
    }
    println!("{ticket}");
}

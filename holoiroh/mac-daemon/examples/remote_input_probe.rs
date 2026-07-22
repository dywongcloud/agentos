//! Live witness for remote-control input injection (crate::remote_input): asserts
//! the normalized->display coordinate mapping, then INJECTS a real cursor move to
//! the display center and reads the cursor back to confirm it landed there. Moves
//! the real cursor briefly, and posting CGEvents needs Accessibility, so this is
//! NOT a headless CI probe (it SKIPs the injection half, still asserting the pure
//! mapping, when Accessibility isn't granted to the process running it).
//!
//! Run with `cargo run --example remote_input_probe -p holoiroh-daemon`.

use holoiroh_daemon::remote_input as ri;

fn main() {
    // --- Pure coordinate mapping (always witnessable) ---
    let tl = ri::map_normalized(0.0, 0.0);
    let br = ri::map_normalized(1.0, 1.0);
    let c = ri::map_normalized(0.5, 0.5);
    println!(
        "map: (0,0)->({:.0},{:.0}) (1,1)->({:.0},{:.0}) (0.5,0.5)->({:.0},{:.0})",
        tl.x, tl.y, br.x, br.y, c.x, c.y
    );
    assert!(br.x > tl.x && br.y > tl.y, "display must have positive extent");
    assert!(
        (c.x - (tl.x + br.x) / 2.0).abs() < 0.01 && (c.y - (tl.y + br.y) / 2.0).abs() < 0.01,
        "(0.5,0.5) must map to the display midpoint"
    );
    // Clamping: out-of-range normalized coords stay on the display.
    let over = ri::map_normalized(2.0, -1.0);
    assert!((over.x - br.x).abs() < 0.01 && (over.y - tl.y).abs() < 0.01, "coords must clamp to 0..1");

    if !ri::is_permitted() {
        println!(
            "remote_input_probe: OK (mapping) -- injection SKIPPED (grant Accessibility to witness \
             the cursor move)."
        );
        return;
    }

    // --- Live injection: move to center, read the cursor back ---
    ri::move_cursor(0.5, 0.5);
    std::thread::sleep(std::time::Duration::from_millis(120));
    let cur = ri::cursor_location().expect("read cursor");
    println!("after move(0.5,0.5): cursor=({:.0},{:.0}) expected~({:.0},{:.0})", cur.x, cur.y, c.x, c.y);
    let dx = (cur.x - c.x).abs();
    let dy = (cur.y - c.y).abs();
    assert!(dx < 3.0 && dy < 3.0, "cursor did not land at the mapped center (dx={dx}, dy={dy})");

    // A click and a scroll must inject without panicking.
    ri::click(0.5, 0.5, false, 1);
    ri::scroll(0.5, 0.5, 0.0, -1.0);

    println!("remote_input_probe: OK -- injection moved the real cursor to the mapped display point.");
}

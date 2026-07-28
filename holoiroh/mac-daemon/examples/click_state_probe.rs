//! Witnesses that consecutive remote taps become a real double-click.
//!
//! The phone sends `count: 1` for every tap and cannot do otherwise without waiting out the
//! double-click window before each one -- half a second of dead time on the most common
//! interaction in a feature whose whole point is feeling immediate. So the daemon derives the
//! click state the way the window server does for real hardware, from how soon and how near the
//! previous click was. Without it, two fast taps are two single clicks and nothing in macOS ever
//! opens.
//!
//! Runs under `HOLOIROH_INPUT_DRY_RUN=1`, which records the click states instead of posting
//! CGEvents, so this cannot click anything on the machine running it.

use holoiroh_daemon::remote_input;

/// Mirrors `remote_input`'s own slop, used here only to decide whether this machine has enough
/// display geometry for the distance rule to be testable at all.
const DOUBLE_CLICK_SLOP_PX: f64 = 6.0;

/// Waits past the double-click window so a scenario starts from a clean sequence. The tracking
/// state is deliberately global and persistent -- four taps in a row really should read 1,2,3,3
/// -- so scenarios have to separate themselves the same way a user would.
fn begin_fresh_sequence() {
    std::thread::sleep(std::time::Duration::from_millis(650));
}

fn click_states(taps: &[(f64, f64)]) -> Vec<i64> {
    begin_fresh_sequence();
    let _ = remote_input::take_applied_click_states();
    for (x, y) in taps {
        remote_input::click(*x, *y, false, 1);
    }
    remote_input::take_applied_click_states()
}

fn main() {
    assert_eq!(
        std::env::var("HOLOIROH_INPUT_DRY_RUN").as_deref(),
        Ok("1"),
        "this probe must run with HOLOIROH_INPUT_DRY_RUN=1 so it records click states instead of \
         clicking on the machine running it"
    );

    let spot = (0.5, 0.5);

    // The distance rule below compares MAPPED points, so it needs a display with real geometry.
    // A headless machine (CI, a Mac with no attached display) has CGMainDisplayID() return 0 and
    // CGDisplayBounds collapse to a zero rect, which maps every normalized point to the same
    // place -- two taps at opposite corners would then look like a double-click and this would
    // fail for a reason that has nothing to do with the code under test.
    let far_apart = (
        remote_input::map_normalized(0.2, 0.2),
        remote_input::map_normalized(0.8, 0.8),
    );
    let geometry_is_real = (far_apart.0.x - far_apart.1.x).abs() > DOUBLE_CLICK_SLOP_PX
        || (far_apart.0.y - far_apart.1.y).abs() > DOUBLE_CLICK_SLOP_PX;

    let two = click_states(&[spot, spot]);
    assert_eq!(
        two,
        vec![1, 2],
        "two fast taps in the same place must be a double-click; got {two:?}"
    );
    println!("two fast taps in one place -> {two:?}");

    let four = click_states(&[spot, spot, spot, spot]);
    assert_eq!(
        four,
        vec![1, 2, 3, 3],
        "click state must climb to triple-click and then stay there rather than growing without \
         bound; got {four:?}"
    );
    println!("four fast taps in one place -> {four:?}");

    if geometry_is_real {
        let apart = click_states(&[(0.2, 0.2), (0.8, 0.8)]);
        assert_eq!(
            apart,
            vec![1, 1],
            "two taps in different places are two separate clicks, however fast; got {apart:?}"
        );
        println!("two fast taps far apart -> {apart:?}");
    } else {
        println!(
            "two fast taps far apart -> skipped: this machine reports no display geometry, so \
             every normalized point maps to the same place and the distance rule cannot be \
             exercised. The timing rules below still run."
        );
    }

    begin_fresh_sequence();
    let _ = remote_input::take_applied_click_states();
    remote_input::click(spot.0, spot.1, false, 1);
    std::thread::sleep(std::time::Duration::from_millis(650));
    remote_input::click(spot.0, spot.1, false, 1);
    let slow = remote_input::take_applied_click_states();
    assert_eq!(
        slow,
        vec![1, 1],
        "taps further apart than the double-click window are separate clicks; got {slow:?}"
    );
    println!("two taps 650ms apart -> {slow:?}");

    begin_fresh_sequence();
    let _ = remote_input::take_applied_click_states();
    remote_input::click(spot.0, spot.1, false, 1);
    remote_input::click(spot.0, spot.1, true, 1);
    let mixed = remote_input::take_applied_click_states();
    assert_eq!(
        mixed,
        vec![1, 1],
        "a right click never continues a left click's sequence; got {mixed:?}"
    );
    println!("left tap then right tap -> {mixed:?}");

    begin_fresh_sequence();
    let _ = remote_input::take_applied_click_states();
    remote_input::click(spot.0, spot.1, false, 2);
    let explicit = remote_input::take_applied_click_states();
    assert_eq!(
        explicit,
        vec![1, 2],
        "an explicit count still forces a double-click, so the wire keeps working unchanged; got \
         {explicit:?}"
    );
    println!("explicit count: 2 -> {explicit:?}");

    println!(
        "\nVERDICT: OK -- a fast double-tap opens things, taps that are far apart or slow stay \
         separate, and the phone never has to stall a tap to find out which it was"
    );
}

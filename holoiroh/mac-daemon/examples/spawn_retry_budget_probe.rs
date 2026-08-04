use holoiroh_daemon::holo_bridge::HoloA2aListener;
use holoiroh_daemon::holo_bridge::listener::{
    CHILD_LISTEN_FD, HOLO_A2A_LISTEN_FD_ENV, LISTEN_BACKLOG,
};

fn main() -> anyhow::Result<()> {
    let listener = HoloA2aListener::bind(0)?;
    let address = listener.address();
    assert!(address.ip().is_loopback());
    assert_eq!(address, listener.address());
    assert_eq!(CHILD_LISTEN_FD, 198);
    assert_eq!(HOLO_A2A_LISTEN_FD_ENV, "HOLO_A2A_LISTEN_FD");
    assert!(LISTEN_BACKLOG >= 128);
    println!(
        "spawn retry budget is obsolete: parent listener {address} stays bound, child fd={CHILD_LISTEN_FD}, backlog={LISTEN_BACKLOG}"
    );
    Ok(())
}

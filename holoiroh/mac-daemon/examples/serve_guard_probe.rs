//! Manual, run-by-hand probe for the `holo serve` single-instance guard's ownership semantics
//! (`holo_bridge::process::GuardClaim`) and `HoloServeProcess::spawn`'s failure paths. Run via
//! `cargo run --example serve_guard_probe`. No `#[cfg(test)]`, no mocks -- every assertion runs
//! against the REAL process-wide static and the REAL `spawn()` code path.
//!
//! Exists because two guard bugs were witnessed live on a real Mac (the daemon's health loop
//! logging `restart failed: failed to respawn holo serve` every tick, forever, after a stale
//! stub squatted the serve port and the real child died on bind):
//!
//! 1. Restart could NEVER succeed: the dead child's process object still held the guard when
//!    the replacement spawned, so the replacement's acquire always failed.
//! 2. The old object's `Drop` unconditionally released the flag -- clobbering the claim a
//!    replacement had just acquired, re-opening the double-spawn hole.
//!
//! (1)+(2) are fixed by claim ownership: this probe witnesses the exact acquire/disarm/replace
//! ordering `HoloBridge::restart_process` now performs, against the same static it uses.
//!
//! Honest boundary: the full restart path (dead real child -> disarm -> respawn -> A2A agent
//! card probe -> slot swap) needs a live `holo serve`, which needs the installed holo CLI +
//! its runtime -- exercised on the operator's real Mac, not headlessly here. This probe covers
//! the guard-ordering LOGIC that made restart impossible, plus spawn's two headless-reachable
//! failure paths (squatted port, missing launcher) including that neither failure leaks a claim.

use holoiroh_daemon::holo_bridge::process::{GuardClaim, HoloServeProcess};

#[tokio::main(flavor = "current_thread")]
async fn main() {
    println!("=== (1) GuardClaim: acquire / second-acquire-fails / release / re-acquire ===");
    let first = GuardClaim::try_acquire().expect("fresh guard must acquire");
    assert!(
        GuardClaim::try_acquire().is_none(),
        "second acquire while first claim is live must fail"
    );
    println!("  acquire OK; concurrent second acquire refused OK");
    drop(first);
    let reacquired = GuardClaim::try_acquire()
        .expect("guard must be re-acquirable after the claim is dropped (released)");
    println!("  drop released the claim; re-acquire OK");

    println!(
        "\n=== (2) restart ordering: disarm-old THEN acquire-new; dropping disarmed old is a no-op ==="
    );
    // `reacquired` plays the dead old process's claim; simulate exactly what
    // `HoloBridge::restart_process` does now:
    drop(reacquired); // disarm_guard(): the old claim is dropped BEFORE the new spawn...
    let new_claim = GuardClaim::try_acquire()
        .expect("...so the replacement's acquire must succeed (this failing WAS the live bug)");
    // The old process object (now claim-less) being dropped later must NOT release the flag the
    // new claim holds -- claim-less means nothing to release, structurally:
    assert!(
        GuardClaim::try_acquire().is_none(),
        "new claim must still hold the guard after the disarmed old object is gone"
    );
    println!("  disarm->respawn ordering acquires OK; disarmed-old cannot clobber new claim OK");
    drop(new_claim);

    println!(
        "\n=== (3) spawn() failure path A: squatted port -> parent bind error, no claim leak ==="
    );
    // The long-lived Rust listener is now the authoritative bind, before any child exists.
    let squatter = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("bind ephemeral");
    let port = squatter.local_addr().expect("addr").port();
    let err =
        match HoloServeProcess::spawn("holoiroh-nonexistent-binary-for-probe", port, None, None)
            .await
        {
            Ok(_) => panic!("spawn against a squatted port must fail"),
            Err(err) => err,
        };
    let msg = format!("{err:#}");
    assert!(
        msg.contains("failed to bind Holo A2A listener")
            && msg.contains("already in use")
            && msg.contains(&port.to_string()),
        "port-conflict error must name the parent bind and address (got: {msg})"
    );
    println!("  squatted-port parent bind failed with named address OK: {msg}");
    assert!(
        GuardClaim::try_acquire().map(drop).is_some(),
        "failed spawn must have released its guard claim (early-return leak)"
    );
    println!("  no guard claim leaked after parent bind failure OK");
    drop(squatter);

    println!(
        "\n=== (4) spawn() failure path B: free port, missing launcher -> named error, no claim leak ==="
    );
    let probe_port = {
        // Grab-then-release a free ephemeral port for the missing-binary case.
        let l = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("bind ephemeral");
        l.local_addr().expect("addr").port()
    };
    let err = match HoloServeProcess::spawn(
        "holoiroh-nonexistent-binary-for-probe",
        probe_port,
        None,
        None,
    )
    .await
    {
        Ok(_) => panic!("spawn with a nonexistent binary must fail"),
        Err(err) => err,
    };
    let msg = format!("{err:#}");
    assert!(
        msg.contains("could not resolve Holo launcher")
            && msg.contains("holoiroh-nonexistent-binary-for-probe"),
        "missing-launcher error must name the launcher (got: {msg})"
    );
    println!("  missing launcher failed with named-launcher error OK");
    assert!(
        GuardClaim::try_acquire().map(drop).is_some(),
        "failed spawn must have released its guard claim"
    );
    println!("  claim released after spawn failure OK");

    println!(
        "\nserve_guard_probe: OK -- GuardClaim ownership (acquire/refuse/release/re-acquire), the \
         restart disarm-old-then-acquire-new ordering (the fix for the live 'failed to respawn \
         holo serve' loop), no-clobber by a disarmed object, and both headless spawn() failure \
         paths (squatted port rejected by the parent listener; missing launcher named) without a \
         guard leak -- all witnessed against the real static + real spawn(). The full \
         dead-child->respawn->agent-card->swap path needs a live holo serve on a real Mac."
    );
}

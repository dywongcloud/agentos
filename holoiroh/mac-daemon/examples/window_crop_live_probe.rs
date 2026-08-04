use anyhow::Result;
use holoiroh_daemon::window_crop::{
    SystemWindowSnapshotSource, WindowSnapshotSource, resolve_crop,
};

fn main() -> Result<()> {
    let snapshot = SystemWindowSnapshotSource
        .snapshot()
        .map_err(|reason| anyhow::anyhow!("live extraction failed: {}", reason.code()))?;
    println!(
        "frontmost owner_pid={} window=({:.0},{:.0},{:.0},{:.0}) display_count={}",
        snapshot.owner_pid,
        snapshot.window_bounds.x,
        snapshot.window_bounds.y,
        snapshot.window_bounds.width,
        snapshot.window_bounds.height,
        snapshot.displays.len()
    );
    for display in &snapshot.displays {
        println!(
            "display id={} bounds=({:.0},{:.0},{:.0},{:.0})",
            display.id,
            display.bounds.x,
            display.bounds.y,
            display.bounds.width,
            display.bounds.height
        );
    }
    if snapshot.displays.len() == 1 {
        let display = snapshot.displays[0];
        let width = display.bounds.width.round() as u32;
        let height = display.bounds.height.round() as u32;
        match resolve_crop(&snapshot, width, height) {
            Ok(crop) => println!(
                "live_mapping image={}x{} crop=({},{},{},{})",
                width, height, crop.x, crop.y, crop.width, crop.height
            ),
            Err(reason) => println!("live_mapping=no_crop reason={}", reason.code()),
        }
    } else {
        println!("live_mapping=no_crop reason=display_count_not_one");
    }
    println!("WINDOW CROP LIVE EXTRACTION PROBE PASSED");
    Ok(())
}

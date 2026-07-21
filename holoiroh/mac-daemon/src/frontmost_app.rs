//! Frontmost-application lookup: which macOS app currently owns the screen.
//!
//! This is the missing live input `crate::sensitive_categories` documents in
//! its "What this module is not" section -- `SensitiveCategories::classify`
//! is a bundle-ID membership check, and until now nothing in the daemon
//! could *supply* a bundle ID to check. The sensitive-app watchdog
//! (`crate::holo_bridge::control`) polls this while a turn is running: the
//! Holo agent drives whatever app is frontmost, so the frontmost bundle ID
//! is the closest real proxy this daemon has for "the surface the agent is
//! about to act on". (A finer per-window/per-URL classifier is explicitly
//! out of scope per `sensitive_categories`' own module doc -- browser tabs
//! and in-app screens are invisible at this granularity.)
//!
//! ## Why `lsappinfo`, not an objc2/NSWorkspace binding
//!
//! `lsappinfo` is a macOS-shipped LaunchServices CLI (present on every
//! supported macOS version, no install step) whose `front` subcommand prints
//! the frontmost application's ASN and whose `info` subcommand prints that
//! app's `CFBundleIdentifier` -- exactly the two facts needed, for zero new
//! crate dependencies. An `NSWorkspace.frontmostApplication` binding via the
//! `objc2-app-kit` stack would need a new dependency tree plus main-thread
//! discipline (`NSWorkspace` is main-thread-affine) inside an async daemon;
//! two short subprocess calls a second, on the watchdog's own interval, cost
//! effectively nothing by comparison. Failure of either call (unexpected
//! output shape, sandboxing, future OS change) degrades to `None`, which the
//! watchdog treats as "no classification possible this tick" -- never a turn
//! failure.

use tokio::process::Command;

/// Returns the frontmost application's bundle identifier (e.g.
/// `"com.apple.Safari"`), or `None` if it cannot be determined this tick.
pub async fn frontmost_bundle_id() -> Option<String> {
    // `lsappinfo front` prints a single line like:
    //   ASN:0x0-0x1e01e0:
    let front = Command::new("lsappinfo").arg("front").output().await.ok()?;
    if !front.status.success() {
        return None;
    }
    let asn = String::from_utf8_lossy(&front.stdout).trim().to_string();
    if asn.is_empty() {
        return None;
    }

    // `lsappinfo info -only bundleid <asn>` prints a single line like:
    //   "CFBundleIdentifier"="com.apple.Safari"
    // (older macOS prints the key unquoted; parse both shapes).
    let info = Command::new("lsappinfo")
        .args(["info", "-only", "bundleid", &asn])
        .output()
        .await
        .ok()?;
    if !info.status.success() {
        return None;
    }
    let line = String::from_utf8_lossy(&info.stdout).trim().to_string();
    let value = line.split('=').nth(1)?.trim();
    let bundle_id = value.trim_matches('"').trim();
    if bundle_id.is_empty() || bundle_id == "NULL" {
        return None;
    }
    Some(bundle_id.to_string())
}

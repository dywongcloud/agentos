//! Frontmost-application lookup: which macOS app currently owns the screen.
//!
//! This module supplies the missing live input that `crate::sensitive_categories` documents in
//! its "What this module is not" section. `SensitiveCategories::classify` is a bundle-ID
//! membership check. Until this module existed, nothing in the daemon could supply a bundle ID
//! to check. The sensitive-app watchdog (`crate::holo_bridge::control`) polls this module while
//! a turn is running. The Holo agent drives whatever app is frontmost, so the frontmost bundle
//! ID is the closest real proxy this daemon has for "the surface the agent is about to act on".
//! A finer per-window or per-URL classifier is explicitly out of scope, per
//! `sensitive_categories`' own module doc: browser tabs and in-app screens stay invisible at
//! this granularity.
//!
//! ## Why `lsappinfo`, not an objc2/NSWorkspace binding
//!
//! `lsappinfo` is a macOS-shipped LaunchServices CLI, present on every supported macOS version
//! with no install step. Its `front` subcommand prints the frontmost application's ASN. Its
//! `info` subcommand prints that app's `CFBundleIdentifier`. These are exactly the two facts
//! this module needs, and neither call needs a new crate dependency. An
//! `NSWorkspace.frontmostApplication` binding through the `objc2-app-kit` stack would instead
//! need a new dependency tree, plus main-thread discipline inside an async daemon (`NSWorkspace`
//! is main-thread-affine). Two short subprocess calls a second, on the watchdog's own interval,
//! cost effectively nothing by comparison. A failure of either call -- an unexpected output
//! shape, sandboxing, a future OS change -- degrades to `None`. The watchdog treats `None` as
//! "no classification possible this tick", never as a turn failure.

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

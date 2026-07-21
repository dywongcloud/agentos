//! Manual, run-by-hand probe: witnesses `frontmost_app::frontmost_bundle_id` against this
//! Mac's REAL frontmost application, then runs that live bundle id through
//! `SensitiveCategories::classify` -- i.e. the exact two-step pipeline the sensitive-app
//! watchdog (`holo_bridge::control`) performs each tick, executed for real.
//!
//! Run with `cargo run --example frontmost_app_probe` (needs a GUI session -- headless CI has
//! no frontmost app, in which case the lookup legitimately returns `None` and the probe says
//! so instead of asserting).

use holoiroh_daemon::frontmost_app;
use holoiroh_daemon::sensitive_categories::SensitiveCategories;

#[tokio::main]
async fn main() {
    let bundle_id = frontmost_app::frontmost_bundle_id().await;
    println!("frontmost bundle id: {bundle_id:?}");

    let Some(bundle_id) = bundle_id else {
        println!(
            "frontmost_app_probe: no frontmost app resolvable (headless session?) -- the \
             watchdog treats this exact outcome as 'skip this tick', never a failure."
        );
        return;
    };

    let cats = SensitiveCategories::load_or_init_default()
        .unwrap_or_else(|_| SensitiveCategories::default_categories());
    match cats.classify(&bundle_id) {
        Some(cat) => println!(
            "classified: {} -> category '{}' ({}), setting {:?} -- the watchdog would gate on this",
            bundle_id, cat.id, cat.display_name, cat.setting
        ),
        None => println!(
            "classified: {bundle_id} -> no sensitive category -- the watchdog would let the turn proceed"
        ),
    }
    println!("frontmost_app_probe: OK -- live lookup + classification executed for real.");
}
